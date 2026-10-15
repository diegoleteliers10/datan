//! Contexto de filtro: el conjunto de predicados activos sobre columnas.
//!
//! Puro. Sin duckdb, sin gpui, sin IO.
//!
//! # Que es un contexto de filtro
//!
//! En DAX, un filtro es un predicado sobre una columna, no un conjunto de filas. `Region =
//! "Norte"` es un filtro sobre `Dim Product[Region]`. Los datos seeleon al evaluarse el
//! predicado, no al construirse el contexto.
//!
//! Eso hace que un contexto de filtro sea pequeno y composable: son unas pocas columnas con
//! valores. La propagacion por el grafo de relaciones es lo unico que lo hace crecer.
//!
//! Es inmutable. `with_*` devuelve un contexto nuevo. Eso permite cachear planes por
//! combinacion de filtro sin mutar nada.

use datan_model::{Column, FilterDirection, Model, ObjectId, Relationship, RelationshipRole};
use std::collections::{BTreeMap, BTreeSet};

/// Operador de un filtro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOperator {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    In,
}

/// Un valor de filtro. Los tipos coinciden con los de `datan_model::DataType`.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    Text(String),
    Integer(i64),
    Decimal(f64),
    Date(i32),
    Boolean(bool),
    /// `BLANK` de DAX. No es `NULL`: `BLANK` compara igual a 0 y a la cadena vacia.
    Blank,
}

impl FilterValue {
    /// `BLANK` es especial en DAX: `BLANK() = 0` es verdadero y `BLANK() = ""` tambien.
    /// Un filtro `= BLANK` no es un `IS NULL` de SQL.
    pub fn equals(&self, other: &FilterValue) -> bool {
        match (self, other) {
            (FilterValue::Blank, FilterValue::Blank) => true,
            (FilterValue::Integer(a), FilterValue::Integer(b)) => a == b,
            (FilterValue::Decimal(a), FilterValue::Decimal(b)) => a == b,
            (FilterValue::Integer(a), FilterValue::Decimal(b)) => (*a as f64) == *b,
            (FilterValue::Decimal(a), FilterValue::Integer(b)) => *a == (*b as f64),
            (FilterValue::Text(a), FilterValue::Text(b)) => a == b,
            (FilterValue::Date(a), FilterValue::Date(b)) => a == b,
            (FilterValue::Boolean(a), FilterValue::Boolean(b)) => a == b,
            (FilterValue::Text(a), FilterValue::Integer(b)) => a == &b.to_string(),
            (FilterValue::Text(a), FilterValue::Decimal(b)) => a == &b.to_string(),
            _ => false,
        }
    }
}

/// De donde sale un filtro.
///
/// Es la distincion mas importante del crate. Un filtro **concreto** trae sus valores:
/// `Dim Product[Region] = "Norte"`. Un filtro **propagado** no trae valores: trae la
/// columna de la que viene y los filtros que la produjeron, y significa "las filas cuya
/// clave esta en el conjunto que sale de aplicar esos filtros ahi".
///
/// Por que importa: en un star schema el filtro `Dim Product[Region] = "Norte"` no se
/// convierte en `Fact Sales[Region] = "Norte"`. Llega a los hechos como
/// `Fact Sales[ProductId] IN (SELECT ProductId FROM Dim Product WHERE Region = 'Norte')`.
///
/// Si se copiaran los valores, cross-filtering daria respuestas equivocadas en cuanto la
/// dimension cambiara entre consultas, y la cache de planes no podria invalidar bien.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterSource {
    /// Valores concretos, 필요하다 cuando el autor escribio el filtro.
    Concrete(Vec<FilterValue>),
    /// El filtro viaja desde otra columna por el grafo de relaciones.
    Propagated {
        /// La columna de la que viene, en el linaje de Power BI.
        origin_column: ObjectId,
        /// Los filtros concretos que hay que aplicar en el origen para obtener el conjunto.
        origin_filters: Vec<Filter>,
    },
}

/// Un filtro sobre una columna.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub column: ObjectId,
    pub operator: FilterOperator,
    pub source: FilterSource,
}

impl Filter {
    pub fn equal(column: ObjectId, value: FilterValue) -> Self {
        Self {
            column,
            operator: FilterOperator::Equal,
            source: FilterSource::Concrete(vec![value]),
        }
    }

    pub fn in_values(column: ObjectId, values: Vec<FilterValue>) -> Self {
        Self {
            column,
            operator: FilterOperator::In,
            source: FilterSource::Concrete(values),
        }
    }

    /// Los valores, si el filtro es concreto. Un filtro propagado no los tiene.
    pub fn values(&self) -> &[FilterValue] {
        match &self.source {
            FilterSource::Concrete(values) => values,
            FilterSource::Propagated { .. } => &[],
        }
    }

    pub fn is_propagated(&self) -> bool {
        matches!(self.source, FilterSource::Propagated { .. })
    }
}

/// Un contexto de filtro.
///
/// Inmutable y ordenado. El orden viene del `BTreeMap`, no de la insercion: dos contextos
/// con los mismos filtros tienen que ser iguales para que el cache de planes acierte.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterContext {
    filters: BTreeMap<ObjectId, Vec<Filter>>,
}

impl FilterContext {
    /// Contexto vacio: sin filtros. Es lo que hay al inicio de cada celda de grupo.
    pub fn empty() -> Self {
        Self {
            filters: BTreeMap::new(),
        }
    }

    /// Aplica un filtro. Si la columna ya tiene filtros, se combinan con `AND`.
    pub fn with(&self, filter: Filter) -> Self {
        let mut next = self.clone();
        next.filters.entry(filter.column).or_default().push(filter);
        next
    }

    pub fn with_equal(&self, column: ObjectId, value: FilterValue) -> Self {
        self.with(Filter::equal(column, value))
    }

    /// Quita todos los filtros de una columna.
    pub fn without_column(&self, column: ObjectId) -> Self {
        let mut next = self.clone();
        next.filters.remove(&column);
        next
    }

    /// Quita todos los filtros de una tabla.
    pub fn without_table(&self, model: &Model, table: ObjectId) -> Self {
        let Some(owner) = model.table(table) else {
            return self.clone();
        };
        let mut next = self.clone();
        for column in &owner.columns {
            next.filters.remove(&column.id);
        }
        next
    }

    /// Quita todos los filtros de todas las tablas. Es lo que hace `ALL()` sin argumentos.
    pub fn without_everything(&self) -> Self {
        Self::empty()
    }

    /// Los filtros de una columna.
    pub fn filters_on(&self, column: ObjectId) -> &[Filter] {
        self.filters.get(&column).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    pub fn column_count(&self) -> usize {
        self.filters.len()
    }

    /// Todas las columnas con filtro, en orden.
    pub fn filtered_columns(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.filters.keys().copied()
    }

    /// Intersecta dos contextos. Es lo que hace `KEEPFILTERS`.
    ///
    /// La semantica de DAX es que un filtro sobre la misma columna se combina con `AND`, y
    /// un filtro sobre una columna distinta se anade. Dos contextos disjuntos dan un
    /// resultado vacio, que aqui se representa como un contexto vacio con marca de
    /// contradiccion.
    pub fn intersect(&self, other: &FilterContext) -> FilterContext {
        let mut result = self.clone();
        for (column, incoming) in &other.filters {
            match result.filters.get_mut(column) {
                Some(existing) => {
                    // El filtro mas restrictivo gana: si alguno es vacio, el conjunto lo es.
                    let empty = existing
                        .iter()
                        .chain(incoming.iter())
                        .any(|f| f.values().is_empty());
                    existing.extend(incoming.iter().cloned());
                    if empty {
                        existing.push(Filter {
                            column: *column,
                            operator: FilterOperator::In,
                            source: FilterSource::Concrete(Vec::new()),
                        });
                    }
                }
                None => {
                    result.filters.insert(*column, incoming.clone());
                }
            }
        }
        result
    }

    /// El contexto ya propagado por el grafo de relaciones.
    ///
    /// Un filtro sobre `Dim Product[Region]` tiene que llegar a `Fact Sales`. Eso se hace
    /// leyendo la relationship y copiando el filtro a la columna del otro lado. La
    /// propagacion es transitiva: un filtro sobre Dim Date tiene que llegar a los hechos a
    /// traves de la cadena.
    ///
    /// El ciclo es detectible: si una relationship bidireccional forma un ciclo, propagar
    /// puede no terminar. Por eso hay un limite de rondas.
    pub fn propagate(&self, model: &Model) -> FilterContext {
        let mut current = self.clone();
        // El numero maximo de columnas del modelo es una cota dura y suficiente: cada ronda
        //_utils anade al menos una columna, asi que nunca hay mas rondas que columnas.
        let max_rounds = model.tables.iter().map(|t| t.columns.len()).sum::<usize>() + 1;

        for _ in 0..max_rounds {
            let mut changed = false;
            for relationship in model.active_relationships() {
                let before = current.column_count();
                current = current.propagate_one(model, relationship);
                if current.column_count() != before {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        current
    }

    /// Propaga a lo largo de una relationship.
///
/// La propagacion es a nivel de TABLA, no de columna. Un filtro sobre cualquier columna de
/// `Dim Product` filtra filas de `Dim Product`, y eso filtra `Fact Sales` por la clave de la
/// relacion. Por eso se comprueba si la tabla del lado de la dimension tiene algun filtro,
/// no si la columna concreta lo tiene.
///
/// El filtro viaja como `Propagated`, no con valores copiados.
fn propagate_one(&self, model: &Model, relationship: &Relationship) -> FilterContext {
    let mut result = self.clone();

    if relationship.role == RelationshipRole::Inactive {
        return result;
    }

    let from_table = model.column(relationship.from_column).map(|(t, _)| t.id);
    let to_table = model.column(relationship.to_column).map(|(t, _)| t.id);

    // Filtros que viven en la tabla de la dimension.
    let origin_filters: Vec<Filter> = match from_table {
        Some(table_id) => self.filters_in_table(model, table_id).to_vec(),
        None => Vec::new(),
    };

    if !origin_filters.is_empty() {
        // El conjunto que sale de aplicar esos filtros en la tabla de origen.
        let origin_context = FilterContext::from_filters(origin_filters);
        result = result.with(Filter {
            column: relationship.to_column,
            operator: FilterOperator::In,
            source: FilterSource::Propagated {
                origin_column: relationship.from_column,
                origin_filters: origin_context.filters.into_values().flatten().collect(),
            },
        });
    }

    // Bidireccional: tambien al reves. Un ciclo aca es lo que hace que el backend SQL no
    // pueda expresar la consulta, porque SQL exige arbol de joins aciclico.
    if relationship.direction == FilterDirection::Both {
        let reverse_filters: Vec<Filter> = match to_table {
            Some(table_id) => self.filters_in_table(model, table_id).to_vec(),
            None => Vec::new(),
        };
        if !reverse_filters.is_empty() {
            let origin_context = FilterContext::from_filters(reverse_filters);
            result = result.with(Filter {
                column: relationship.from_column,
                operator: FilterOperator::In,
                source: FilterSource::Propagated {
                    origin_column: relationship.to_column,
                    origin_filters: origin_context.filters.into_values().flatten().collect(),
                },
            });
        }
    }

    let _ = to_table;
    result
}

/// Los filtros que viven en una tabla, en cualquier columna.
fn filters_in_table(&self, model: &Model, table: ObjectId) -> Vec<Filter> {
    let Some(owner) = model.table(table) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for column in &owner.columns {
        found.extend(self.filters_on(column.id).iter().cloned());
    }
    found
}

    /// Todas las tablas que el contexto toca, calculadas propagando. Sirve para saber que
    /// joins necesita la consulta SQL.
    pub fn tables_touched(&self, model: &Model) -> BTreeSet<ObjectId> {
        let propagated = self.propagate(model);
        let mut tables = BTreeSet::new();
        for column in propagated.filtered_columns() {
            if let Some((table, _)) = model.column(column) {
                tables.insert(table.id);
            }
        }
        tables
    }

    /// Las columnas que hay que proyectar para evaluar este contexto.
    pub fn required_columns(&self, model: &Model) -> BTreeSet<ObjectId> {
        self.propagate(model).filters.keys().copied().collect()
    }

    /// Todas las columnas del modelo que participan en alguna relationship. Son las que el
    /// extractor de atomos tiene que mirar.
    pub fn relationship_columns(model: &Model) -> BTreeSet<ObjectId> {
        model
            .relationships
            .iter()
            .flat_map(|r| [r.from_column, r.to_column])
            .collect()
    }

    /// Los filtros agrupados por columna, en orden.
    pub fn entries(&self) -> impl Iterator<Item = (&ObjectId, &Vec<Filter>)> {
        self.filters.iter()
    }

    /// Construye un contexto a partir de una lista plana de filtros.
    pub fn from_filters(filters: Vec<Filter>) -> Self {
        let mut context = Self::empty();
        for filter in filters {
            context = context.with(filter);
        }
        context
    }

    /// Todos los filtros, en orden por columna.
    pub fn into_values(self) -> Vec<Filter> {
        self.filters.into_values().flatten().collect()
    }

    /// Los filtros de una columna, o un slice vacio.
    pub fn get(&self, column: ObjectId) -> &[Filter] {
        self.filters_on(column)
    }
}

/// Una columna del modelo, resumida. Lo que el motor necesita saber sin cargar el modelo
/// entero en cada evaluacion.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub id: ObjectId,
    pub table: ObjectId,
    pub table_name: String,
    pub name: String,
}

impl ColumnInfo {
    pub fn from(model: &Model, column: &Column) -> Option<Self> {
        let (table, _) = model.column(column.id)?;
        Some(Self {
            id: column.id,
            table: table.id,
            table_name: table.name.clone(),
            name: column.name.clone(),
        })
    }
}