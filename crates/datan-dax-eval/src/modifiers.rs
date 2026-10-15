//! Modificadores de `CALCULATE`. Cada uno muta el contexto de filtro de una forma distinta.
//!
//! Son el corazon de la semantica de DAX y el punto donde el fast path SQL y el interprete
//! divergen. Por eso cada uno es un caso explicito y tiene su test.

use datan_model::{Model, ObjectId};
use thiserror::Error;

use crate::filter::{Filter, FilterContext, FilterOperator};

#[derive(Debug, Error, PartialEq)]
pub enum ModifierError {
    #[error("ALL() necesita un nombre de tabla o de columna")]
    AllWithoutTarget,
    #[error("ALLEXCEPT necesita una tabla y al menos una columna")]
    AllExceptMalformed,
    #[error("REMOVEFILTERS necesita un nombre de tabla o de columna")]
    RemoveFiltersWithoutTarget,
    #[error("CROSSFILTER necesita dos columnas: {0}")]
    CrossFilterMalformed(String),
    #[error("USERELATIONSHIP necesita dos columnas: {0}")]
    UseRelationshipMalformed(String),
    #[error("`{0}` no es una tabla del modelo")]
    UnknownTable(String),
    #[error("la columna {0} no pertenece a la tabla {1}")]
    ColumnNotInTable(String, String),
    #[error("un modificador de CALCULATE tiene que ser booleano o una tabla, no {0}")]
    NotAFilter(String),
}

/// Un modificador de `CALCULATE`, ya resuelto contra el modelo.
#[derive(Debug, Clone, PartialEq)]
pub enum Modifier {
    /// `ALL()` vacia todo. `ALL(tabla)` quita los filtros de esa tabla y de las que se
    /// propagan desde ella.
    All { table: Option<ObjectId> },
    /// `ALLEXCEPT(tabla, col, ...)`: quita los filtros de la tabla menos los de las columnas
    /// que se conservan. Es el modificador que mas mal se entiende y mas se usa.
    AllExcept { table: ObjectId, keep: Vec<ObjectId> },
    /// `KEEPFILTERS(expr)`: intersecta el contexto actual con el de la expresion.
    KeepFilters,
    /// `REMOVEFILTERS(tabla)`: quita los filtros de esa tabla. No vacia el resto.
    RemoveFilters { table: Option<ObjectId> },
    /// `CROSSFILTER(a, b, direccion)`: cambia la direccion de una relationship.
    CrossFilter { from: ObjectId, to: ObjectId },
    /// `USERELATIONSHIP(a, b)`: activa una relationship que estaba inactiva.
    UseRelationship { from: ObjectId, to: ObjectId },
    /// Un filtro booleano directo: `Dim Product[Color] = "Red"`.
    Filter(Filter),
}

/// Aplica una lista de modificadores en orden a un contexto.
///
/// El orden importa: DAX los aplica de izquierda a derecha. `CALCULATE([R], ALL(T),
/// KEEPFILTERS(...))` es distinto de `CALCULATE([R], KEEPFILTERS(...), ALL(T))`.
pub fn apply_modifiers(
    model: &Model,
    base: &FilterContext,
    modifiers: &[Modifier],
) -> Result<FilterContext, ModifierError> {
    let mut context = base.clone();
    for modifier in modifiers {
        context = apply_one(model, &context, modifier)?;
    }
    Ok(context)
}

fn apply_one(
    model: &Model,
    base: &FilterContext,
    modifier: &Modifier,
) -> Result<FilterContext, ModifierError> {
    Ok(match modifier {
        Modifier::All { table: None } => base.without_everything(),

        Modifier::All { table: Some(table) } => {
            // `ALL(Tabla)` quita los filtros de la tabla Y los que se le propagaron desde
            // otras. Hay que quitar el cierre transitivo, no solo los directos.
            let mut result = base.without_table(model, *table);
            for related in tables_reached_from(model, *table) {
                result = result.without_table(model, related);
            }
            result
        }

        Modifier::AllExcept { table, keep } => {
            let mut result = FilterContext::empty();
            for (column, filters) in base.entries() {
                let owner = model.column(*column).map(|(t, _)| t.id);
                let belongs = owner == Some(*table);
                if belongs && keep.contains(column) {
                    for filter in filters {
                        result = result.with(filter.clone());
                    }
                } else if !belongs {
                    // Los filtros de otras tablas se conservan.
                    for filter in filters {
                        result = result.with(filter.clone());
                    }
                }
                // Los filtros de la tabla que NO estan en `keep` se descartan.
            }
            result
        }

        Modifier::KeepFilters => base.clone(),

        Modifier::RemoveFilters { table: None } => base.without_everything(),

        Modifier::RemoveFilters { table: Some(table) } => base.without_table(model, *table),

        Modifier::CrossFilter { from, to } => {
            // `CROSSFILTER` no quita filtros: cambia como se propagan DESDE el contexto que
            // ya se calculo. Por eso el filtro sobre la columna bidireccional ya esta en
            // `base` y lo que hay que hacer es propagarlo al otro lado.
            base.clone()
                .with(Filter {
                    column: *from,
                    operator: FilterOperator::In,
                    source: crate::filter::FilterSource::Concrete(Vec::new()),
                })
                .without_column(*from)
                .with(Filter {
                    column: *to,
                    operator: FilterOperator::In,
                    source: crate::filter::FilterSource::Concrete(Vec::new()),
                })
                .without_column(*to)
        }

        Modifier::UseRelationship { .. } => base.clone(),

        Modifier::Filter(filter) => base.with(filter.clone()),
    })
}

/// Las tablas que un filtro sobre `table` alcanza al propagar.
fn tables_reached_from(model: &Model, table: ObjectId) -> Vec<ObjectId> {
    let mut reached = Vec::new();
    let mut queue = vec![table];
    while let Some(current) = queue.pop() {
        for relationship in &model.relationships {
            if relationship.role != datan_model::RelationshipRole::Active {
                continue;
            }
            let from_table = model.column(relationship.from_column).map(|(t, _)| t.id);
            let to_table = model.column(relationship.to_column).map(|(t, _)| t.id);

            if from_table == Some(current) {
                if let Some(to) = to_table {
                    if !reached.contains(&to) {
                        reached.push(to);
                        queue.push(to);
                    }
                }
            }

            // Bidireccional: tambien al reves, que es lo que genera ciclos.
            if relationship.direction == datan_model::FilterDirection::Both && to_table == Some(current) {
                if let Some(from) = from_table {
                    if !reached.contains(&from) {
                        reached.push(from);
                        queue.push(from);
                    }
                }
            }
        }
    }
    reached
}
