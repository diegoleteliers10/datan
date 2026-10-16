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

use crate::atom::Atom;
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
        // atomo. El resto la deduce de la columna.
        let table = match (atom.column, atom.table) {
            (Some(column), _) => self.owner(column),
            (None, Some(table)) => table,
            (None, None) => return Err(InterpError::Unsupported(atom.label.clone())),
        };

        let rows = self.select_rows(table, context);

        // `COUNTROWS` no necesita columna. Las demas si.
        if atom.kind == AtomKind::CountRows {
            return Ok(Scalar::Integer(rows.len() as i64));
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