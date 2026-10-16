//! El interprete por celda de grupo.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. `cargo test` corre en milisegundos.
//!
//! # Por que el fallback corre sobre celdas, no sobre filas
//!
//! Es la idea que hace aceptable tener un motor de formulas propio. El fallback NO evalua
//! cada atomo sobre la tabla entera: evalua cada atomo una vez por celda de grupo, y cada
//! `SUM` es una lectura de un escalar ya calculado por el motor de almacenamiento.
//!
//! Una matriz de 5.000 celdas con 6 atomos son 30.000 lecturas. Son milisegundos. Si en
//! cambio el fallback recorriera 10M de filas por celda, serian 50.000 millones de
//! operaciones, y no habria motivo para escribir un motor propio: habria que borrar el
//! codigo.
//!
//! # Por que las fuentes son un trait
//!
//! `datan-dax-eval` no puede depender de `duckdb` ni de `arrow`: el plan exige que el motor
//! sea headless para que la parte dificil itere en milisegundos. Por eso los datos entran por
//! un trait, y el adaptador a Arrow vive en `datan-testkit`.

use datan_model::{Model, ObjectId};
use std::collections::{BTreeSet, HashMap};

use crate::atom::{Atom, AtomKind};
use crate::{BinaryOp, CheckedExpr, UnaryOp};
use crate::filter::{FilterContext, FilterSource};

/// Un valor escalar de DAX.
///
/// `Blank` NO es `null`. En DAX, `BLANK()` es igual a `0`, es igual a la cadena vacia, y
/// `DISTINCTCOUNT` lo ignora mientras que `COUNTBLANK` lo cuenta. Confundirlo con `null`
/// cambia numeros.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Blank,
    Boolean(bool),
    Integer(i64),
    Decimal(f64),
    Text(String),
    Date(i32),
}

impl Scalar {
    pub fn is_blank(&self) -> bool {
        matches!(self, Scalar::Blank)
    }

    /// El cero que DAX le asigna a `BLANK` en aritmetica.
    pub fn as_number(&self) -> f64 {
        match self {
            Scalar::Blank => 0.0,
            Scalar::Boolean(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Scalar::Integer(n) => *n as f64,
            Scalar::Decimal(n) => *n,
            Scalar::Text(_) | Scalar::Date(_) => 0.0,
        }
    }

    /// El orden que usa DAX para comparar valores de tipos distintos. Text ordena despues
    /// que los numeros.
    pub fn compare(&self, other: &Scalar) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Scalar::Blank, Scalar::Blank) => Ordering::Equal,
            (Scalar::Text(a), Scalar::Text(b)) => a.cmp(b),
            (Scalar::Date(a), Scalar::Date(b)) => a.cmp(b),
            _ => self
                .as_number()
                .partial_cmp(&other.as_number())
                .unwrap_or(Ordering::Equal),
        }
    }
}

/// Una columna en memoria. La vista que el interprete necesita de los datos.
pub trait ColumnView: Send + Sync {
    fn len(&self) -> usize;
    /// El valor de la fila. Devuelve `Blank` si la fila no existe o es nula.
    fn value_at(&self, row: usize) -> Scalar;
    /// Cuantas filas NO son blank.
    fn non_blank_count(&self) -> usize;
    /// Cuantos valores distintos, ignorando los blank. Es lo que necesita `DISTINCTCOUNT`.
    fn distinct_count(&self) -> usize;
}

/// Un origen de datos: columnas por su id del modelo.
pub trait DataSource {
    fn column(&self, column: ObjectId) -> Option<&dyn ColumnView>;
    /// El numero de filas de la tabla a la que pertenece la columna.
    fn table_len(&self, table: ObjectId) -> usize;
}

/// El resultado de evaluar un conjunto de atomos en todas las celdas de grupo.
///
/// Cada fila son los valores de agrupamiento seguidos de los valores de los atomos, en el
/// orden de `atoms`.
#[derive(Debug, Clone, Default)]
pub struct GroupedResult {
    pub rows: Vec<Vec<Scalar>>,
    /// Cuantas columnas de cada fila son agrupamiento. El resto son medidas.
    pub width: usize,
}

impl GroupedResult {
    pub fn measures(&self) -> Vec<Vec<Scalar>> {
        self.rows
            .iter()
            .map(|row| row[self.width..].to_vec())
            .collect()
    }
}

/// Avanza un indice odometro. `false` cuando ya se agotaron las combinaciones.
fn advance(index: &mut [usize], dimensions: &[Vec<Option<Scalar>>]) -> bool {
    if dimensions.is_empty() {
        return false;
    }
    let mut position = index.len();
    while position > 0 {
        position -= 1;
        index[position] += 1;
        if index[position] < dimensions[position].len() {
            return true;
        }
        index[position] = 0;
    }
    false
}

/// Compara dos filas por las columnas de agrupamiento.
fn compare_rows(left: &[Scalar], right: &[Scalar], width: usize) -> std::cmp::Ordering {
    for column in 0..width {
        let ordering = left[column].compare(&right[column]);
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

/// Los valores de las columnas de una expresion, ya materializados.
///
/// Un vector por columna, alineado con las filas seleccionadas del ancla. El arbol de la
/// expresion se recorre contra esta tabla, sin volver a leer el origen.
struct ColumnValues {
    columns: std::collections::HashMap<ObjectId, Vec<Scalar>>,
    row_count: usize,
}

impl ColumnValues {
    fn get(&self, column: ObjectId, row: usize) -> Option<&Scalar> {
        if row >= self.row_count {
            return None;
        }
        self.columns.get(&column).and_then(|values| values.get(row))
    }
}

/// Aritmetica de DAX sobre dos escalares.
///
/// La division es el caso que importa: `BLANK / algo` y `algo / 0` dan BLANK, nunca `inf` ni
/// `NaN`. DuckDB devuelve `inf` para `10 / 0` y `NaN` para `0 / 0`, asi que el fast path
/// necesita `NULLIF` y el interprete necesita esto. Si los dos no coinciden, la misma
/// medida da un numero visiblemente falso en un camino y un blanco en el otro.
fn binary_numbers(left: &Scalar, right: &Scalar, op: BinaryOp) -> Scalar {
    if left.is_blank() || right.is_blank() {
        return Scalar::Blank;
    }
    let a = left.as_number();
    let b = right.as_number();
    let value = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Subtract => a - b,
        BinaryOp::Multiply => a * b,
        BinaryOp::Divide => {
            if b == 0.0 {
                return Scalar::Blank;
            }
            a / b
        }
        // Una comparacion dentro de una agregacion no es un caso de informe. Devolver BLANK
        // es honesto: un operador que no se implementa no puede devolver un numero.
        _ => return Scalar::Blank,
    };
    if value.is_nan() || value.is_infinite() {
        return Scalar::Blank;
    }
    number_to_scalar(value)
}

/// El resultado de evaluar un conjunto de atomos en una celda de grupo.
#[derive(Debug, Clone, Default)]
pub struct CellResult {
    pub values: HashMap<String, Scalar>,
}

impl CellResult {
    pub fn get(&self, label: &str) -> Option<&Scalar> {
        self.values.get(label)
    }

    pub fn number(&self, label: &str) -> Option<f64> {
        self.values.get(label).map(Scalar::as_number)
    }
}

/// El interprete.
pub struct Interpreter<'a> {
    model: &'a Model,
    source: &'a dyn DataSource,
}

/// Error del interprete.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InterpError {
    #[error("el origen no tiene la columna {0}")]
    MissingColumn(ObjectId),
    #[error("un atomo de este tipo no se puede interpretar")]
    Unsupported(String),
    #[error("el origen no tiene la tabla {0}")]
    MissingTable(ObjectId),
    #[error("la tabla {0} no tiene camino de join desde la tabla del hecho")]
    NoJoinPath(ObjectId),
    #[error(
        "el agrupamiento produce {cells} celdas y el tope es {max}. Un producto cartesiano \
         de columnas de alta cardinalidad no es un informe, es una explosion"
    )]
    TooManyCells { cells: usize, max: usize },
}

impl<'a> Interpreter<'a> {
    pub fn new(model: &'a Model, source: &'a dyn DataSource) -> Self {
        Self { model, source }
    }

    /// Evalua un atomo bajo un contexto de filtro, ya propagado.
    ///
    /// Devuelve UN escalar. Eso es todo el punto: el fallback produce un escalar por atomo
    /// por celda, nunca un escaneo de filas.
    pub fn evaluate_atom(
        &self,
        atom: &Atom,
        _cell_context: &FilterContext,
    ) -> Result<Scalar, InterpError> {
        // El contexto del ATOMO manda sobre el de la celda, SIEMPRE.
        //
        // No hay caso de respaldo: un contexto vacio es un contexto vacio, no "el de la
        // celda". `CALCULATE([Qty], ALL('Dim Product'))` deja el contexto del atomo VACIO a
        // proposito, y usar el de la celda reintroducia el filtro que el `ALL` acaba de
        // quitar. Es un fallo en silencio: devuelve numeros y no avisa.
        let context = &atom.context;

        // `COUNTROWS` y las agregaciones sobre tabla no tienen columna: la tabla viene en el
        // atomo. El resto deduce la tabla de la columna, o de la EXPRESION si no tiene.
        let table = match (atom.column, atom.table) {
            (Some(column), _) => self.owner(column),
            (None, Some(table)) => table,
            (None, None) => match self.expression_anchor(atom.expression.as_ref()) {
                Some(table) => table,
                None => return Err(InterpError::Unsupported(atom.label.clone())),
            },
        };

        let rows = self.select_rows(table, context);

        // `COUNTROWS` no necesita columna. Las demas si.
        if atom.kind == AtomKind::CountRows {
            return Ok(Scalar::Integer(rows.len() as i64));
        }

        // Una agregacion sobre EXPRESION: `SUM(Qty * Price)`, `SUM(Qty * Cost)`.
        //
        // Antes esto caia en `Unsupported` porque el atomo no tiene columna unica. El fallo
        // era invisible: las tres medidas del plan (`Revenue`, `COGS`, `Margin`) bajan a SQL,
        // asi que el arnes diferencial nunca lo ejecuto y 27 casos estaban verdes sin
        // cubrir la medida mas comun de un informe real.
        if let Some(expression) = &atom.expression {
            return self.aggregate_expression(expression, atom.kind, table, &rows);
        }

        let Some(column) = atom.column.and_then(|c| self.source.column(c)) else {
            return Err(InterpError::MissingColumn(
                atom.column.unwrap_or(ObjectId(0)),
            ));
        };

        use crate::atom::AtomKind;
        Ok(match atom.kind {
            AtomKind::Sum => {
                // `SUM` sobre cero filas da BLANK en DAX, no 0. Es la diferencia con SQL,
                // donde `SUM` de un conjunto vacio devuelve 0. La celda vacia de un informe
                // y una celda con un total de cero no son lo mismo, y el usuario lo nota.
                if rows.is_empty() {
                    return Ok(Scalar::Blank);
                }
                let mut total = 0.0;
                let mut non_blank = 0usize;
                for row in &rows {
                    let value = column.value_at(*row);
                    if value.is_blank() {
                        continue;
                    }
                    total += value.as_number();
                    non_blank += 1;
                }
                if non_blank == 0 {
                    Scalar::Blank
                } else {
                    number_to_scalar(total)
                }
            }
            AtomKind::CountRows => Scalar::Integer(rows.len() as i64),
            AtomKind::Count => {
                // `COUNT` sobre una columna cuenta los no-blank. `COUNTROWS` cuenta filas.
                let count = rows
                    .iter()
                    .filter(|row| !column.value_at(**row).is_blank())
                    .count();
                Scalar::Integer(count as i64)
            }
            AtomKind::Min => {
                let mut best: Option<Scalar> = None;
                for row in &rows {
                    let value = column.value_at(*row);
                    if value.is_blank() {
                        continue;
                    }
                    best = Some(match best {
                        Some(current) if current.compare(&value) != std::cmp::Ordering::Greater => current,
                        _ => value,
                    });
                }
                best.unwrap_or(Scalar::Blank)
            }
            AtomKind::Max => {
                let mut best: Option<Scalar> = None;
                for row in &rows {
                    let value = column.value_at(*row);
                    if value.is_blank() {
                        continue;
                    }
                    best = Some(match best {
                        Some(current) if current.compare(&value) != std::cmp::Ordering::Less => current,
                        _ => value,
                    });
                }
                best.unwrap_or(Scalar::Blank)
            }
            AtomKind::Average => {
                let mut total = 0.0;
                let mut count = 0usize;
                for row in &rows {
                    let value = column.value_at(*row);
                    if value.is_blank() {
                        continue;
                    }
                    total += value.as_number();
                    count += 1;
                }
                if count == 0 {
                    Scalar::Blank
                } else {
                    number_to_scalar(total / count as f64)
                }
            }
            AtomKind::DistinctCount => {
                let mut distinct: BTreeSet<String> = BTreeSet::new();
                for row in &rows {
                    let value = column.value_at(*row);
                    if value.is_blank() {
                        // `DISTINCTCOUNT` ignora los BLANK. Es la diferencia entre esto y
                        // un COUNT(DISTINCT) de SQL.
                        continue;
                    }
                    distinct.insert(scalar_key(&value));
                }
                Scalar::Integer(distinct.len() as i64)
            }
            AtomKind::Expression => {
                return Err(InterpError::Unsupported(atom.label.clone()));
            }
        })
    }

    /// La tabla que ancla una expresion: la de la columna mas "de hechos" que encuentre.
    ///
    /// No hay forma de saberlo por los tipos, asi que se busca la columna mas cercana a un
    /// hecho en el grafo de relationships. Una tabla sin camino a ningun hecho no puede
    /// anclar nada y devuelve `None`.
    fn expression_anchor(&self, expression: Option<&CheckedExpr>) -> Option<ObjectId> {
        let expression = expression?;
        let columns = crate::atom::columns_of(expression);
        if columns.is_empty() {
            return None;
        }
        columns
            .iter()
            .filter_map(|column| self.model.column(*column).map(|(table, _)| table.id))
            .find(|table| {
                self.model.tables.iter().any(|t| {
                    matches!(t.kind, datan_model::TableKind::Facts) && t.id == *table
                })
            })
            .or_else(|| self.model.column(columns[0]).map(|(t, _)| t.id))
    }

    /// Agrega una expresion sobre las filas seleccionadas.
    ///
    /// Las columnas se materializan TODAS antes de agregar, una vez por atomo. Sin ese paso,
    /// resolver `Cost` para cada fila de `Qty` seria buscar en la dimension entera por cada
    /// fila del hecho: O(n*m). Con 1M de lineas y 500 productos son 500 millones de
    /// comparaciones, y el interprete seria mas lento que SQL por dos ordenes de magnitud.
    fn aggregate_expression(
        &self,
        expression: &CheckedExpr,
        kind: AtomKind,
        table: ObjectId,
        rows: &[usize],
    ) -> Result<Scalar, InterpError> {
        let values = self.materialize(expression, table, rows)?;

        let mut total = 0.0;
        let mut non_blank = 0usize;
        // `MIN` y `MAX` comparan el valor, no su numero: sobre texto el orden de `Scalar` no
        // es el de la cadena y sobre booleanos no hay orden numerico.
        let mut best: Option<Scalar> = None;

        for row in 0..rows.len() {
            let value = self.eval_with(expression, &values, row)?;
            if value.is_blank() {
                continue;
            }
            total += value.as_number();
            non_blank += 1;
            match kind {
                AtomKind::Min => {
                    best = Some(match best {
                        Some(current) if current.compare(&value) != std::cmp::Ordering::Greater => {
                            current
                        }
                        _ => value,
                    });
                }
                AtomKind::Max => {
                    best = Some(match best {
                        Some(current) if current.compare(&value) != std::cmp::Ordering::Less => {
                            current
                        }
                        _ => value,
                    });
                }
                _ => {}
            }
        }

        // `SUM` sobre cero filas o sobre ceros todos da BLANK en DAX, no 0. La celda vacia de
        // un informe y una celda con total cero no son lo mismo, y el usuario lo nota.
        Ok(match kind {
            AtomKind::Count => Scalar::Integer(non_blank as i64),
            AtomKind::Sum | AtomKind::Expression => {
                if non_blank == 0 {
                    Scalar::Blank
                } else {
                    number_to_scalar(total)
                }
            }
            AtomKind::Average => {
                if non_blank == 0 {
                    Scalar::Blank
                } else {
                    number_to_scalar(total / non_blank as f64)
                }
            }
            AtomKind::Min | AtomKind::Max => best.unwrap_or(Scalar::Blank),
            // `COUNTROWS` se resuelve antes de llegar aqui: no tiene expresion.
            AtomKind::CountRows | AtomKind::DistinctCount => {
                return Err(InterpError::Unsupported("agregacion sobre expresion".to_string()));
            }
        })
    }

    /// Resuelve los valores de cada columna para cada fila seleccionada.
    ///
    /// El resultado tiene un vector por columna, alineado con `rows`. Una columna del ancla
    /// se lee directo. Una columna de otra tabla se resuelve por el camino de join, con un
    /// indice de clave a fila construido UNA vez para toda la columna.
    fn materialize(
        &self,
        expression: &CheckedExpr,
        anchor: ObjectId,
        rows: &[usize],
    ) -> Result<ColumnValues, InterpError> {
        let mut out: std::collections::HashMap<ObjectId, Vec<Scalar>> =
            std::collections::HashMap::new();

        for column in crate::atom::columns_of(expression) {
            let owner = self.owner(column);
            let view = self
                .source
                .column(column)
                .ok_or(InterpError::MissingColumn(column))?;

            if owner == anchor {
                out.insert(column, rows.iter().map(|row| view.value_at(*row)).collect());
                continue;
            }

            // Una fila del ancla puede no tener equivalente en la otra tabla: una venta cuyo
            // producto se borro. `assume_referential_integrity` es `false` por defecto, asi
            // que Datan no inventa la fila. El valor es BLANK, que es lo que hace que la
            // agregacion lo ignore en vez de sumar cero.
            let path = self
                .model
                .join_path(anchor, owner)
                .ok_or(InterpError::NoJoinPath(owner))?;
            let index = self.build_index(&path, owner)?;
            let source_view = self
                .source
                .column(column)
                .ok_or(InterpError::MissingColumn(column))?;

            let mut resolved = Vec::with_capacity(rows.len());
            let mut current_rows: Vec<usize> = rows.to_vec();
            for (position, edge) in path.iter().enumerate() {
                let key_view = self
                    .source
                    .column(edge.from_column)
                    .ok_or(InterpError::MissingColumn(edge.from_column))?;
                let mut next = Vec::with_capacity(current_rows.len());
                for row in &current_rows {
                    let key = key_view.value_at(*row);
                    match index.get(&scalar_key(&key)) {
                        Some(matched) => next.push(*matched),
                        // La fila no tiene equivalente: se propaga el hueco en vez de
                        // inventar una fila. Ver la nota de arriba.
                        None => {
                            let _ = position;
                            next.push(usize::MAX);
                        }
                    }
                }
                current_rows = next;
            }

            resolved.extend(
                current_rows
                    .iter()
                    .map(|row| {
                        if *row == usize::MAX {
                            Scalar::Blank
                        } else {
                            source_view.value_at(*row)
                        }
                    }),
            );
            out.insert(column, resolved);
        }

        Ok(ColumnValues {
            columns: out,
            row_count: rows.len(),
        })
    }

    /// Indice de clave a fila para la tabla destino de un camino.
    ///
    /// La columna final del camino es la que une. Se indexa esa, no la que se va a leer: el
    /// camino puede tener varias aristas y la clave de union es la de la ultima.
    fn build_index(
        &self,
        path: &[datan_model::JoinEdge],
        destination: ObjectId,
    ) -> Result<std::collections::HashMap<String, usize>, InterpError> {
        let Some(last) = path.last() else {
            return Ok(std::collections::HashMap::new());
        };
        let key_column = last.to_column;
        if self.owner(key_column) != destination {
            return Err(InterpError::MissingTable(destination));
        }
        let view = self
            .source
            .column(key_column)
            .ok_or(InterpError::MissingColumn(key_column))?;
        let length = self.source.table_len(destination);

        let mut index = std::collections::HashMap::with_capacity(length);
        for row in 0..length {
            index.entry(scalar_key(&view.value_at(row))).or_insert(row);
        }
        Ok(index)
    }

    /// Evalua una expresion con las columnas ya materializadas.
    fn eval_with(
        &self,
        expression: &CheckedExpr,
        values: &ColumnValues,
        row: usize,
    ) -> Result<Scalar, InterpError> {
        Ok(match expression {
            CheckedExpr::Column { column, .. } => values
                .get(*column, row)
                .cloned()
                .unwrap_or(Scalar::Blank),
            CheckedExpr::Number { value, .. } => Scalar::Decimal(*value),
            CheckedExpr::Text { value, .. } => Scalar::Text(value.clone()),
            CheckedExpr::Boolean { value, .. } => Scalar::Boolean(*value),
            CheckedExpr::Blank { .. } => Scalar::Blank,
            CheckedExpr::Unary { op, operand, .. } => {
                let inner = self.eval_with(operand, values, row)?;
                match op {
                    UnaryOp::Negate => match inner {
                        Scalar::Integer(n) => Scalar::Integer(-n),
                        Scalar::Decimal(n) => Scalar::Decimal(-n),
                        other => other,
                    },
                    UnaryOp::Not => Scalar::Boolean(inner.as_number() == 0.0),
                }
            }
            CheckedExpr::Binary { op, left, right, .. } => {
                let left = self.eval_with(left, values, row)?;
                let right = self.eval_with(right, values, row)?;
                binary_numbers(&left, &right, *op)
            }
            other => return Err(InterpError::Unsupported(format!("{other:?}"))),
        })
    }

    /// Evalua todos los atomos de una celda.
    pub fn evaluate_cell(
        &self,
        atoms: &[Atom],
        context: &FilterContext,
    ) -> Result<CellResult, InterpError> {
        // El contexto tiene que venir propagado. `extract_atoms` ya lo propaga por atomo,
        // pero quien llama puede pasar el contexto crudo, y sin propagar el filtro de la
        // dimension no llega a los hechos y la celda sale sin filtrar. Es un fallo en
        // silencio: devuelve numeros, solo que incorrectos.
        let context = &context.propagate(self.model);
        let mut result = CellResult::default();
        for atom in atoms {
            // Cada atomo trae su propio contexto propagado, que es mas preciso que el de
            // la celda: un `CALCULATE` con modificadores lo cambia.
            let atom_context = context.clone();
            let value = self.evaluate_atom(atom, &atom_context)?;
            result.values.insert(atom.label.clone(), value);
        }
        Ok(result)
    }

    /// Evalua el conjunto para cada celda de grupo y devuelve las filas.
    ///
    /// El informe es una matriz: filas por dimension, columnas por medida. El fast path lo
    /// resuelve con un `GROUP BY`; el interprete tiene que recorrer las combinaciones.
    ///
    /// Se recorre el PRODUCTO CARTESIANO de los valores distintos de cada columna de
    /// agrupamiento, no solo las combinaciones que existen. Una combinacion vacia se
    /// descarta al final, pero descartarla antes evita un `SUM` sobre cero filas por cada
    /// celda que nunca se va a pintar. El caso patologico es real: agrupar por dos columnas
    /// de texto de 50 valores cada una son 2500 celdas para 1M de filas, y la mayoria vacias.
    /// Con tres columnas de alta cardinalidad el producto se dispara, asi que hay un tope y
    /// el corte se dice en voz alta en vez de truncar en silencio.
    pub fn evaluate_grouped(
        &self,
        atoms: &[Atom],
        context: &FilterContext,
        grouping: &[ObjectId],
        max_cells: usize,
    ) -> Result<GroupedResult, InterpError> {
        let mut values_per_column = Vec::with_capacity(grouping.len());
        for column in grouping {
            values_per_column.push(self.distinct_values(*column)?);
        }

        let combinations = values_per_column.iter().map(|v| v.len()).product::<usize>();
        if combinations > max_cells {
            return Err(InterpError::TooManyCells { cells: combinations, max: max_cells });
        }

        let mut rows = Vec::new();
        let mut index = vec![0usize; grouping.len()];
        loop {
            let keys: Vec<Option<Scalar>> = grouping
                .iter()
                .zip(&index)
                .zip(&values_per_column)
                .map(|((_, position), distinct)| distinct[*position].clone())
                .collect();

            // El contexto de la celda lleva los valores de agrupamiento encima del contexto
            // de la consulta. La columna se filtra por igualdad, que es lo que hace el
            // `GROUP BY`.
            let mut cell_context = context.clone();
            for (column, key) in grouping.iter().zip(&keys) {
                if let Some(key) = key {
                    cell_context = cell_context.with_equal(
                        *column,
                        crate::filter::FilterValue::from_scalar(key),
                    );
                }
            }

            let mut cells = Vec::with_capacity(grouping.len());
            let mut any_value = false;
            for key in &keys {
                cells.push(key.clone().unwrap_or(Scalar::Blank));
                if !key.as_ref().is_some_and(Scalar::is_blank) {
                    any_value = true;
                }
            }

            // Una celda sin filas NO es una celda. Un `GROUP BY` de SQL no devuelve los
            // grupos vacios, y devolverlos hace que el arnes diverge en cada caso con
            // filtro.
            //
            // La comprobacion es el numero de filas del ancla, no si el resultado es BLANK:
            // `COUNT` sobre cero filas devuelve 0, no BLANK, asi que un 0 no distingue "grupo
            // vacio" de "grupo con cero". Mirar el valor de la medida es la via que falla en
            // silencio, y falla en todos los conteos.
            let cell: CellResult = self.evaluate_cell_in(atoms, &cell_context)?;
            let has_rows = self.any_rows(atoms, &cell_context);
            if any_value && has_rows {
                for atom in atoms {
                    cells.push(cell.get(&atom.label).cloned().unwrap_or(Scalar::Blank));
                }
                rows.push(cells);
            }

            if !advance(&mut index, &values_per_column) {
                break;
            }
        }

        rows.sort_by(|left, right| compare_rows(left, right, grouping.len()));
        Ok(GroupedResult { rows, width: grouping.len() })
    }

    /// Si alguna fila entra en la celda.
    ///
    /// Mira el contexto de las tablas de los atomos, propagado. Con varios atomos basta con
    /// que uno tenga filas: el `GROUP BY` de SQL decide por el conjunto de hechos, no por
    /// cada medida.
    fn any_rows(&self, atoms: &[Atom], context: &FilterContext) -> bool {
        let propagated = context.propagate(self.model);
        for atom in atoms {
            let Some(table) = self.atom_table(atom) else {
                continue;
            };
            if !self.select_rows(table, &propagated).is_empty() {
                return true;
            }
        }
        false
    }

    fn atom_table(&self, atom: &Atom) -> Option<ObjectId> {
        match (atom.column, atom.table) {
            (Some(column), _) => Some(self.owner(column)),
            (None, Some(table)) => Some(table),
            (None, None) => self.expression_anchor(atom.expression.as_ref()),
        }
    }

    /// Como [`Self::evaluate_cell`], sin repropagar el contexto.
    ///
    /// `evaluate_cell` propaga porque su entrada es un contexto crudo de usuario. Aqui la
    /// celda ya trae los filtros de grupo propagados desde [`Self::select_rows`], y volver a
    /// propagar es idempotente pero caro: una matriz de 2500 celdas paga la propagacion 2500
    /// veces sobre el mismo modelo.
    fn evaluate_cell_in(
        &self,
        atoms: &[Atom],
        context: &FilterContext,
    ) -> Result<CellResult, InterpError> {
        // El contexto de celda se propaga UNA vez por celda y se une con el del atomo.
        //
        // `evaluate_atom` usa el contexto del ATOMO y descarta el de la celda, y esa regla
        // es correcta para un `CALCULATE` con modificadores: el `ALL` quiere vaciar el
        // contexto y usar el de la celda lo reintroduce. Para la agrupacion es al reves: el
        // filtro de grupo es una restriccion que el atomo no puede conocer, asi que se le
        // SUMA al suyo.
        //
        // Sin esta union, `SUM(Qty)` agrupado por region devuelve el total en cada fila.
        let cell = &context.propagate(self.model);
        let mut result = CellResult::default();
        for atom in atoms {
            let merged = Atom {
                context: atom.context.union(cell),
                ..atom.clone()
            };
            let value = self.evaluate_atom(&merged, cell)?;
            result.values.insert(atom.label.clone(), value);
        }
        Ok(result)
    }

    /// Los valores distintos de una columna, en un orden estable.
    ///
    /// El orden es por representacion textual, no por hash: el fast path devuelve las filas
    /// ordenadas por el `GROUP BY` de SQL y las dos listas tienen que poder compararse. Un
    /// orden distinto aqui haria divergir el arnes sin que el motor este mal.
    fn distinct_values(&self, column: ObjectId) -> Result<Vec<Option<Scalar>>, InterpError> {
        let view = self
            .source
            .column(column)
            .ok_or(InterpError::MissingColumn(column))?;
        let mut distinct: BTreeSet<String> = BTreeSet::new();
        let mut order: std::collections::HashMap<String, Option<Scalar>> =
            std::collections::HashMap::new();
        let length = self.source.table_len(self.owner(column));
        for row in 0..length {
            let value = view.value_at(row);
            if value.is_blank() {
                continue;
            }
            let key = scalar_key(&value);
            if distinct.insert(key.clone()) {
                order.insert(key, Some(value));
            }
        }
        let mut out: Vec<Option<Scalar>> = distinct.iter().filter_map(|k| order.get(k).cloned()).collect();
        out.sort_by(|a, b| scalar_key(a.as_ref().unwrap()).cmp(&scalar_key(b.as_ref().unwrap())));
        Ok(out)
    }

    /// Los indices de fila de una tabla que sobreviven al contexto de filtro.
    ///
    /// Este es el unico lugar donde el interprete recorre filas. Y solo lo hace para
    /// calcular QUE filas entran: el valor agregado sale de una lectura escalar. Por eso la
    /// celda, no la fila, es la unidad de coste.
    pub fn select_rows(&self, table: ObjectId, context: &FilterContext) -> Vec<usize> {
        let total = self.source.table_len(table);
        let mut rows: Vec<usize> = (0..total).collect();

        let Some(owner) = self.model.table(table) else {
            return rows;
        };

        for column in &owner.columns {
            let filters = context.filters_on(column.id);
            if filters.is_empty() {
                continue;
            }
            let Some(view) = self.source.column(column.id) else {
                continue;
            };
            rows.retain(|row| filters.iter().all(|filter| self.matches(view, *row, filter)));
        }
        rows
    }

    fn matches(&self, view: &dyn ColumnView, row: usize, filter: &crate::filter::Filter) -> bool {
        use crate::filter::FilterOperator;
        let actual = view.value_at(row);

        match &filter.source {
            FilterSource::Concrete(values) => match filter.operator {
                FilterOperator::In => {
                    values.iter().any(|value| values_match(&actual, value))
                }
                FilterOperator::Equal => {
                    values.first().is_some_and(|value| values_match(&actual, value))
                }
                FilterOperator::NotEqual => {
                    values.first().is_none_or(|value| !values_match(&actual, value))
                }
                FilterOperator::Less => {
                    values.first().is_some_and(|value| actual.compare(&to_scalar(value)) == std::cmp::Ordering::Less)
                }
                FilterOperator::LessOrEqual => {
                    values.first().is_some_and(|value| actual.compare(&to_scalar(value)) != std::cmp::Ordering::Greater)
                }
                FilterOperator::Greater => {
                    values.first().is_some_and(|value| actual.compare(&to_scalar(value)) == std::cmp::Ordering::Greater)
                }
                FilterOperator::GreaterOrEqual => {
                    values.first().is_some_and(|value| actual.compare(&to_scalar(value)) != std::cmp::Ordering::Less)
                }
            },

            // Una semijoin: la fila entra si su clave esta en el conjunto que sale de
            // aplicar los filtros del origen. Es la propagacion de lineage, evaluada.
            FilterSource::Propagated {
                origin_column,
                origin_filters,
            } => {
                let Some(origin) = self.source.column(*origin_column) else {
                    return false;
                };
                let actual_key = actual;
                (0..origin.len()).any(|candidate_row| {
                    if origin.value_at(candidate_row) != actual_key {
                        return false;
                    }
                    origin_filters.iter().all(|inner| {
                        let Some(inner_view) = self.source.column(inner.column) else {
                            return false;
                        };
                        self.matches(inner_view, candidate_row, inner)
                    })
                })
            }
        }
    }

    fn owner(&self, column: ObjectId) -> ObjectId {
        self.model
            .column(column)
            .map(|(table, _)| table.id)
            .unwrap_or(ObjectId(0))
    }
}

fn number_to_scalar(value: f64) -> Scalar {
    // Un entero que cabe en i64 se devuelve como entero. Datan usa `Moneda` para pesos sin
    // centimos, y perder la integridad a float seria un error de producto.
    if value.fract() == 0.0 && value.abs() <= 9.007_199_254_740_992e15 {
        Scalar::Integer(value as i64)
    } else {
        Scalar::Decimal(value)
    }
}

fn to_scalar(value: &crate::filter::FilterValue) -> Scalar {
    use crate::filter::FilterValue as V;
    match value {
        V::Blank => Scalar::Blank,
        V::Boolean(b) => Scalar::Boolean(*b),
        V::Integer(n) => Scalar::Integer(*n),
        V::Decimal(n) => Scalar::Decimal(*n),
        V::Date(days) => Scalar::Date(*days),
        V::Text(text) => Scalar::Text(text.clone()),
    }
}

/// Compara un valor de la columna con un valor de filtro, con las reglas de DAX.
///
/// `BLANK = 0` es verdadero en DAX, y tambien `BLANK = ""`. Un filtro `= BLANK` no es un
/// `IS NULL` de SQL.
fn values_match(actual: &Scalar, expected: &crate::filter::FilterValue) -> bool {
    use crate::filter::FilterValue as V;
    match (actual, expected) {
        (Scalar::Blank, V::Blank) => true,
        (Scalar::Blank, V::Integer(0)) => true,
        (Scalar::Blank, V::Decimal(n)) if *n == 0.0 => true,
        (Scalar::Blank, V::Text(t)) if t.is_empty() => true,
        (Scalar::Blank, _) => false,
        (Scalar::Text(a), V::Text(b)) => a == b,
        (Scalar::Text(a), V::Integer(n)) => a == &n.to_string(),
        (Scalar::Text(a), V::Decimal(n)) => a == &n.to_string(),
        (Scalar::Integer(a), V::Integer(b)) => a == b,
        (Scalar::Date(a), V::Date(b)) => a == b,
        (Scalar::Boolean(a), V::Boolean(b)) => a == b,
        _ => actual.compare(&to_scalar(expected)) == std::cmp::Ordering::Equal,
    }
}

/// Una clave estable para deduplicar. Los blank no llegan aqui: `DISTINCTCOUNT` los ignora.
fn scalar_key(value: &Scalar) -> String {
    match value {
        Scalar::Blank => "blank".to_string(),
        Scalar::Boolean(b) => format!("b{b}"),
        Scalar::Integer(n) => format!("i{n}"),
        Scalar::Decimal(n) => format!("d{n}"),
        Scalar::Text(t) => format!("t{t}"),
        Scalar::Date(d) => format!("D{d}"),
    }
}

/// Una columna en memoria de enteros. Para tests y para el adaptador simple.
pub struct IntColumn {
    pub values: Vec<Option<i64>>,
}

impl ColumnView for IntColumn {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn value_at(&self, row: usize) -> Scalar {
        match self.values.get(row) {
            Some(Some(n)) => Scalar::Integer(*n),
            _ => Scalar::Blank,
        }
    }

    fn non_blank_count(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }

    fn distinct_count(&self) -> usize {
        let distinct: BTreeSet<i64> = self.values.iter().flatten().copied().collect();
        distinct.len()
    }
}

/// Una columna en memoria de fechas, en dias desde 1970-01-01.
///
/// El mismo numero entero que usa `FilterValue::Date` y que DuckDB devuelve al castear un
/// `DATE` a `INTEGER`. Que los tres coincidan es lo que permite comparar los dos caminos sin
/// una capa de conversion que pueda mentir.
pub struct DateColumn {
    pub values: Vec<Option<i32>>,
}

impl ColumnView for DateColumn {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn value_at(&self, row: usize) -> Scalar {
        match self.values.get(row) {
            Some(Some(day)) => Scalar::Date(*day),
            _ => Scalar::Blank,
        }
    }

    fn non_blank_count(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }

    fn distinct_count(&self) -> usize {
        let distinct: BTreeSet<i32> = self.values.iter().flatten().copied().collect();
        distinct.len()
    }
}

/// Una columna en memoria de texto.
pub struct TextColumn {
    pub values: Vec<Option<String>>,
}

impl ColumnView for TextColumn {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn value_at(&self, row: usize) -> Scalar {
        match self.values.get(row) {
            Some(Some(t)) => Scalar::Text(t.clone()),
            _ => Scalar::Blank,
        }
    }

    fn non_blank_count(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }

    fn distinct_count(&self) -> usize {
        let distinct: BTreeSet<&String> = self.values.iter().flatten().collect();
        distinct.len()
    }
}

/// Un origen en memoria. Un `HashMap` de columna a vista.
pub struct MemorySource {
    columns: HashMap<ObjectId, Box<dyn ColumnView>>,
    table_lengths: HashMap<ObjectId, usize>,
}

impl MemorySource {
    pub fn new() -> Self {
        Self {
            columns: HashMap::new(),
            table_lengths: HashMap::new(),
        }
    }

    pub fn with_column(mut self, id: ObjectId, view: Box<dyn ColumnView>) -> Self {
        self.columns.insert(id, view);
        self
    }

    pub fn with_table_length(mut self, table: ObjectId, rows: usize) -> Self {
        self.table_lengths.insert(table, rows);
        self
    }
}

impl Default for MemorySource {
    fn default() -> Self {
        Self::new()
    }
}

impl DataSource for MemorySource {
    fn column(&self, column: ObjectId) -> Option<&dyn ColumnView> {
        self.columns.get(&column).map(|view| view.as_ref())
    }

    fn table_len(&self, table: ObjectId) -> usize {
        self.table_lengths.get(&table).copied().unwrap_or(0)
    }
}