//! Un informe: dimensiones, medidas y filtros. Y la consulta que lo llena.
//!
//! Un informe es una matriz. Las filas son combinaciones de dimensiones, las columnas son
//! medidas, y las celdas son numeros.
//!
//! # Por que el arnes diferencial no basta
//!
//! El arnes prueba que UNA medida da el mismo numero por los dos caminos. Un informe prueba
//! algo distinto y mas fuerte: que N medidas en M celdas se leen con las consultas
//! MINIMAS, y que pinchar una celda cambia el filtro de todas a la vez.
//!
//! # Una consulta por conjunto de atomos, y como se vuelven a pegar
//!
//! `Revenue`, `Qty` y `Margin` comparten hojas. Si cada medida fuera su propia consulta,
//! una matriz de 6 columnas leeria la tabla de hechos 6 veces.
//!
//! Las medidas que producen el MISMO `AtomSet` comparten la lectura. Pero dos conjuntos
//! distintos dan dos consultas con columnas de medida DIFERENTES, y hay que volver a unirlas
//! en una sola matriz.
//!
//! Se pegan por las columnas de dimension, no por posicion a ciegas. Es seguro porque todas
//! las medidas se evaluan bajo el MISMO filtro y el MISMO agrupamiento, y el `ORDER BY` del
//! lowering ordena por las columnas de agrupamiento. Las filas salen en el mismo orden y con
//! las mismas claves. Sin el `ORDER BY` seria suposicion: DuckDB devuelve los grupos en el
//! orden de su hash, y dos consultas distintas pueden darryptos en orden distinto.

pub mod export;
pub mod saved;

use std::collections::BTreeMap;

use datan_dax_eval::{AtomSet, FilterContext, FilterValue, Interpreter, Scalar};
use datan_dax_sql::{SourceMap, lower, lower_scalar};
use datan_model::{DataType, Measure, Model, ObjectId};
use datan_storage::Engine;

/// Un error de ejecucion de un informe.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("la columna {0} no existe en el modelo")]
    UnknownColumn(ObjectId),
    #[error("la medida {0} no existe en el modelo")]
    UnknownMeasure(ObjectId),
    #[error("un informe sin medidas no muestra nada: se necesita al menos una")]
    NoMeasures,
    #[error("el modelo no tiene una tabla de hechos. Un informe agrupa una tabla de hechos, \\
             no una lista de tablas sueltas")]
    NoFactTable,
    #[error("`{measure}` necesita el interprete y no hay origen de datos en el informe")]
    NoSource { measure: String },
    #[error("fallo la consulta del informe: {0}")]
    Query(String),
    #[error("el interprete fallo: {0}")]
    Interp(String),
    #[error("el motor devolvio {got} columnas y el informe esperaba {expected}")]
    ColumnCount { expected: usize, got: usize },
}

/// Tope de celdas de la matriz.
///
/// El producto cartesiano de las dimensiones es el limite real del informe. El corte se dice
/// con el numero en vez de truncar en silencio: un informe que sale con menos filas de las
/// que tiene parece uno que funciona.
pub const MAX_CELLS: usize = 500_000;

/// Un informe.
#[derive(Debug, Clone)]
pub struct Report {
    pub model: Model,
    /// Las columnas de la izquierda, en orden.
    pub dimensions: Vec<ObjectId>,
    /// Las medidas, en orden de columna.
    pub measures: Vec<ObjectId>,
    /// El filtro actual. Lo cambia el cross-filter.
    pub filters: FilterContext,
    /// El alias de la base adjunta a DuckDB, si la hay.
    ///
    /// Una base de Postgres adjunta vive bajo el alias del `ATTACH`, y sus tablas se llaman
    /// `pg.product_boleta`. Sin esto, el SQL se genera bien y DuckDB responde "table does not
    /// exist" con una pista que es justamente el nombre con el prefijo que falta.
    pub qualifier: String,
}

impl Report {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            dimensions: Vec::new(),
            measures: Vec::new(),
            filters: FilterContext::empty(),
            qualifier: String::new(),
        }
    }

    /// El informe corre contra la base adjunta con este alias.
    pub fn attached_to(mut self, alias: &str) -> Self {
        self.qualifier = alias.to_string();
        self
    }

    pub fn with_dimensions(mut self, dimensions: Vec<ObjectId>) -> Self {
        self.dimensions = dimensions;
        self
    }

    pub fn with_measures(mut self, measures: Vec<ObjectId>) -> Self {
        self.measures = measures;
        self
    }

    pub fn measure_definitions(&self) -> Result<Vec<&Measure>, ReportError> {
        self.measures
            .iter()
            .map(|id| {
                self.model
                    .measures
                    .iter()
                    .find(|measure| measure.id == *id)
                    .ok_or(ReportError::UnknownMeasure(*id))
            })
            .collect()
    }

    /// El nombre de una medida, si existe.
    pub fn measure_name(&self, measure: ObjectId) -> Option<&str> {
        self.model
            .measures
            .iter()
            .find(|m| m.id == measure)
            .map(|m| m.name.as_str())
    }

    /// La tabla de hechos: la que contiene los atoms.
    pub fn fact_table(&self) -> Result<ObjectId, ReportError> {
        self.model
            .tables
            .iter()
            .find(|table| matches!(table.kind, datan_model::TableKind::Facts))
            .map(|table| table.id)
            .ok_or(ReportError::NoFactTable)
    }

    /// Anade un filtro de igualdad, como el que produce pinchar una celda.
    pub fn filtered_by(&self, column: ObjectId, value: FilterValue) -> Self {
        let mut next = self.clone();
        next.filters = next.filters.with_equal(column, value);
        next
    }

    /// Acota la columna de fecha a un rango, como los botones 3M / 6M / 12M.
    ///
    /// Dos filtros, `>=` y `<=`, y no un `BETWEEN`: los dos operadores ya viajan por el
    /// grafo de relaciones sin un caso especial.
    pub fn filtered_between(&self, column: ObjectId, from: i32, to: i32) -> Self {
        let mut next = self.clone();
        next.filters = next
            .filters
            .with_greater_equal(column, FilterValue::Date(from))
            .with_less_equal(column, FilterValue::Date(to));
        next
    }

    /// Suelta un filtro de una columna, como el que produce pinchar la cabecera.
    pub fn unfiltered(&self, column: ObjectId) -> Self {
        let mut next = self.clone();
        next.filters = next.filters.without_column(column);
        next
    }

    /// El total de cada medida, para las tarjetas.
    ///
    /// Sin agrupamiento. Sumar las filas de la matriz solo vale para medidas aditivas: el
    /// promedio de promedios no es el promedio, y el conteo distinto de conteos tampoco.
    ///
    /// Devuelve en el MISMO orden que `self.measures`, para que la tarjeta i es la medida i.
    pub fn totals(
        &self,
        engine: &Engine,
        source: Option<&dyn datan_dax_eval::DataSource>,
    ) -> Result<Vec<String>, ReportError> {
        let sources = SourceMap::from_model(&self.model).with_qualifier(&self.qualifier);
        let definitions = self.measure_definitions()?;

        // Los mismos grupos de lectura que `execute`: medidas con las mismas hojas
        // comparten consulta.
        let mut groups: Vec<(AtomSet, Vec<usize>)> = Vec::new();
        for (index, measure) in definitions.iter().enumerate() {
            let atoms = datan_dax_eval::extract_atoms(
                &measure.expression,
                &self.model,
                &self.filters,
                &self.dimensions,
            )
            .map_err(|error| ReportError::Interp(error.to_string()))?;
            match groups
                .iter_mut()
                .find(|(set, _)| set.signature() == atoms.signature())
            {
                Some((_, members)) => members.push(index),
                None => groups.push((atoms, vec![index])),
            }
        }

        let mut out = vec![String::new(); self.measures.len()];
        for (atoms, members) in &groups {
            let values: Vec<String> = if atoms.needs_interpreter() {
                let src = source.ok_or_else(|| ReportError::NoSource {
                    measure: members
                        .first()
                        .and_then(|i| self.measures.get(*i))
                        .and_then(|id| self.measure_name(*id))
                        .unwrap_or("?")
                        .to_string(),
                })?;
                let interpreter = Interpreter::new(&self.model, src);
                interpreter
                    .evaluate_totals(&atoms.atoms, &atoms.outputs, &self.filters)
                    .map_err(|e| ReportError::Interp(e.to_string()))?
                    .iter()
                    .map(scalar_to_text)
                    .collect()
            } else {
                let plan = lower_scalar(&self.model, &sources, atoms, &self.filters)
                    .map_err(|e| ReportError::Interp(e.to_string()))?;
                let rows = engine
                    .render_rows(&plan.sql)
                    .map_err(|e| ReportError::Query(e.to_string()))?;
                rows.first().cloned().unwrap_or_default()
            };

            for (offset, member) in members.iter().enumerate() {
                out[*member] = values.get(offset).cloned().unwrap_or_default();
            }
        }

        Ok(out)
    }

    /// Las cabeceras: primero las dimensiones, luego las medidas.
    pub fn headers(&self) -> Result<Vec<Header>, ReportError> {
        let mut out = Vec::with_capacity(self.dimensions.len() + self.measures.len());
        for column in &self.dimensions {
            let (table, definition) = self
                .model
                .column(*column)
                .ok_or(ReportError::UnknownColumn(*column))?;
            out.push(Header {
                label: format!("{}[{}]", table.name, definition.name),
                column: *column,
                measure: None,
                data_type: definition.data_type,
            });
        }
        for measure in self.measure_definitions()? {
            out.push(Header {
                label: measure.name.clone(),
                column: ObjectId(0),
                measure: Some(measure.id),
                data_type: DataType::Decimal,
            });
        }
        Ok(out)
    }

    /// Cuantas consultas hacen falta para este informe.
    ///
    /// Es el numero que importa para la latencia: no las medidas, sino las CONSULTAS. Dos
    /// medidas que comparten hojas cuestan una.
    pub fn query_count(&self) -> Result<usize, ReportError> {
        let mut signatures: BTreeMap<Vec<(String, Vec<ObjectId>)>, usize> = BTreeMap::new();
        for measure in self.measure_definitions()? {
            let atoms = datan_dax_eval::extract_atoms(
                &measure.expression,
                &self.model,
                &self.filters,
                &self.dimensions,
            )
            .map_err(|error| ReportError::Interp(error.to_string()))?;
            *signatures.entry(atoms.signature()).or_default() += 1;
        }
        Ok(signatures.len())
    }

    /// Corre el informe.
    ///
    /// `source` solo se usa para las medidas que caen al interprete. Sin el, un informe con
    /// un `CALCULATE` falla en vez de dar un numero: es preferible a calcularlo con un origen
    /// vacio, que sale BLANK y parece una celda sin datos.
    pub fn execute(
        &self,
        engine: &Engine,
        source: Option<&dyn datan_dax_eval::DataSource>,
    ) -> Result<Matrix, ReportError> {
        if self.measures.is_empty() {
            return Err(ReportError::NoMeasures);
        }
        let sources = SourceMap::from_model(&self.model).with_qualifier(&self.qualifier);
        let headers = self.headers()?;

        // Un plan por conjunto de atomos. Cada plan lleva la lista de medidas que lee, en
        // el orden en que deben salir en la matriz.
        let mut plans: Vec<(AtomSet, Vec<usize>)> = Vec::new();
        for (index, measure) in self.measure_definitions()?.into_iter().enumerate() {
            let atoms = datan_dax_eval::extract_atoms(
                &measure.expression,
                &self.model,
                &self.filters,
                &self.dimensions,
            )
            .map_err(|error| ReportError::Interp(error.to_string()))?;
            match plans
                .iter_mut()
                .find(|(set, _)| set.signature() == atoms.signature())
            {
                Some((_, members)) => members.push(index),
                None => plans.push((atoms, vec![index])),
            }
        }

        let width = headers.len();
        let mut rows: Vec<Vec<String>> = Vec::new();

        for (atoms, members) in &plans {
            // Cada plan trae las dimensiones primero y despues SUS PROPIAS medidas. El
            // numero de columnas de medida es el de `members`, que puede ser uno o varios.
            let measure_columns = if atoms.outputs.is_empty() {
                atoms.atoms.len()
            } else {
                atoms.outputs.len()
            };
            let produced = self.dimensions.len() + measure_columns;

            let cells = if atoms.needs_interpreter() {
                let source = source.ok_or_else(|| ReportError::NoSource {
                    measure: members
                        .first()
                        .and_then(|index| self.measures.get(*index))
                        .map(|id| self.measure_name(*id).unwrap_or("?").to_string())
                        .unwrap_or_else(|| "?".to_string()),
                })?;
                let interpreter = Interpreter::new(&self.model, source);
                let grouped = interpreter
                    .evaluate_grouped(
                        &atoms.atoms,
                        &atoms.outputs,
                        &self.filters,
                        &self.dimensions,
                        MAX_CELLS,
                    )
                    .map_err(|error| ReportError::Interp(error.to_string()))?;
                grouped
                    .rows
                    .iter()
                    .map(|row| row.iter().map(scalar_to_text).collect())
                    .collect::<Vec<Vec<String>>>()
            } else {
                let plan = lower(&self.model, &sources, atoms, &self.filters)
                    .map_err(|error| ReportError::Interp(error.to_string()))?;
                engine
                    .render_rows(&plan.sql)
                    .map_err(|error| ReportError::Query(error.to_string()))?
            };

            for row in &cells {
                if row.len() != produced {
                    return Err(ReportError::ColumnCount {
                        expected: produced,
                        got: row.len(),
                    });
                }
            }

            if rows.is_empty() {
                rows = cells
                    .iter()
                    .map(|row| {
                        let mut full = row.clone();
                        full.resize(width, String::new());
                        full
                    })
                    .collect();
            } else if cells.len() != rows.len() {
                // Dos consultas con el mismo filtro y agrupamiento tienen las mismas filas.
                // Si no, el informe esta mal formado y pegar por posicion daria una matriz
                // con las medidas de una columna mezcladas con las de otra.
                return Err(ReportError::Query(format!(
                    "dos medidas del informe produjeron {} y {} filas. Con el mismo filtro y \\
                     agrupamiento tienen que ser las mismas",
                    rows.len(),
                    cells.len()
                )));
            } else {
                for (row, cell) in rows.iter_mut().zip(&cells) {
                    for (offset, member) in members.iter().enumerate() {
                        let target = self.dimensions.len() + member;
                        if target < row.len() {
                            row[target] = cell[self.dimensions.len() + offset].clone();
                        }
                    }
                }
            }
        }

        Ok(Matrix {
            headers,
            rows,
            truncated: false,
        })
    }
}

/// Una cabecera de columna.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub label: String,
    /// La columna del modelo. Para una medida es `ObjectId(0)`: una medida no es una columna.
    pub column: ObjectId,
    /// `Some` si la columna es una medida, `None` si es una dimension.
    pub measure: Option<ObjectId>,
    pub data_type: DataType,
}

impl Header {
    pub fn is_dimension(&self) -> bool {
        self.measure.is_none()
    }
}

/// La matriz del informe.
#[derive(Debug, Clone)]
pub struct Matrix {
    pub headers: Vec<Header>,
    /// Cada fila: primero los valores de dimension, luego los de medida.
    pub rows: Vec<Vec<String>>,
    /// `true` si el informe tiene mas celdas de las que se muestran.
    pub truncated: bool,
}

impl Matrix {
    pub fn width(&self) -> usize {
        self.headers.len()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// El valor de una celda.
    pub fn cell(&self, row: usize, column: usize) -> Option<&str> {
        self.rows.get(row)?.get(column).map(String::as_str)
    }

    /// Los primeros `n` valores de una columna de dimension, ordenados por una de medidas.
    ///
    /// Es lo que alimenta un grafico de barras. Las celdas que no parsean a numero se
    /// descartan: una dimension agrupada junto a una medida no numerica no es un grafico, es
    /// un error de seleccion que la UI debio evitar.
    pub fn top(&self, dimension: usize, measure: usize, n: usize) -> Vec<(String, f64)> {
        let mut values: Vec<(String, f64)> = self
            .rows
            .iter()
            .filter_map(|row| {
                let label = row.get(dimension)?.clone();
                let value = row.get(measure)?.parse::<f64>().ok()?;
                Some((label, value))
            })
            .collect();
        values.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        values.truncate(n);
        values
    }

    /// La cabecera de una columna.
    pub fn header(&self, column: usize) -> Option<&Header> {
        self.headers.get(column)
    }

    /// El indice de la dimension de una columna, si es una dimension.
    ///
    /// Es lo que convierte un clic en un filtro: la celda `(fila, columna)` es un valor de
    /// la dimension `dimensions[columna]`.
    pub fn dimension_index(&self, column: usize) -> Option<usize> {
        let header = self.headers.get(column)?;
        if !header.is_dimension() {
            return None;
        }
        Some(
            self.headers
                .iter()
                .take(column)
                .filter(|h| h.is_dimension())
                .count(),
        )
    }

    /// La matriz como tabla de texto.
    ///
    /// Imprime las primeras filas, no todas: un informe con 5000 celdas llenaria la terminal
    /// y el numero que interesa se ve en las primeras.
    pub fn print(&self, limit: usize) {
        let width = self.width();
        let pad = |value: String| -> String {
            let mut out: String = value.chars().take(44).collect();
            while out.chars().count() < width {
                out.push(' ');
            }
            out
        };

        let header: Vec<String> = self
            .headers
            .iter()
            .map(|h| pad(h.label.clone()))
            .collect();
        println!("  {}", header.join(" | "));

        for row in self.rows.iter().take(limit) {
            let cells: Vec<String> = (0..width)
                .map(|column| pad(row.get(column).cloned().unwrap_or_default()))
                .collect();
            println!("  {}", cells.join(" | "));
        }
        if self.rows.len() > limit {
            println!("  ... y {} filas mas", self.rows.len() - limit);
        }
    }

    /// El filtro que produce pinchar `(fila, columna)`, o `None` si la celda no es de una
    /// dimension.
    pub fn drill(&self, report: &Report, row: usize, column: usize) -> Option<Report> {
        let dimension = self.dimension_index(column)?;
        let column_id = *report.dimensions.get(dimension)?;
        let value = self.cell(row, column)?.to_string();
        let filter = match self.headers[column].data_type {
            DataType::Text => FilterValue::Text(value),
            DataType::Date => FilterValue::Date(value.parse().ok()?),
            _ => FilterValue::Integer(value.parse().ok()?),
        };
        Some(report.filtered_by(column_id, filter))
    }
}

/// Un escalar del interprete como texto, con la misma convencion que el SQL.
///
/// La comparacion entre caminos es de cadenas. Si los dos usan redondeos distintos, todos
/// los promedios divergen y el arnes se queja de un motor que esta bien.
pub fn scalar_to_text(value: &Scalar) -> String {
    match value {
        Scalar::Blank => String::new(),
        Scalar::Boolean(b) => b.to_string(),
        Scalar::Integer(n) => n.to_string(),
        Scalar::Decimal(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n:.6}")
            }
        }
        Scalar::Text(t) => t.clone(),
        Scalar::Date(day) => day.to_string(),
    }
}