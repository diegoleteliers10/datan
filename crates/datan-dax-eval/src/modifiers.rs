//! Modificadores de `CALCULATE`. Cada uno muta el contexto de filtro de una forma distinta.
//!
//! Son el corazon de la semantica de DAX y el punto donde el fast path SQL y el interprete
//! divergen. Por eso cada uno es un caso explicito y tiene su test.

use datan_dax_check::CheckedExpr;
use datan_model::{Model, ObjectId};
use thiserror::Error;

use crate::filter::{Filter, FilterContext, FilterOperator, FilterValue};

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
    #[error("no se pudo interpretar el modificador: {0}")]
    Unresolved(String),
}

/// Traduce una expresion de modificador, ya resuelta, al `Modifier` del modelo.
///
/// `datan-dax-check` deja los modificadores como `CheckedExpr` porque su semantica
/// depende del contexto. Aqui es donde se vuelven filtros concretos.
pub fn resolve_modifier(expr: &CheckedExpr) -> Result<Modifier, ModifierError> {
    match expr {
        CheckedExpr::Call { name, args, .. } => match name.as_str() {
            "ALL" => Ok(Modifier::All {
                table: table_argument(args.first()),
            }),
            "REMOVEFILTERS" => Ok(Modifier::RemoveFilters {
                table: table_argument(args.first()),
            }),
            "ALLEXCEPT" => {
                let Some(table) = table_argument(args.first()) else {
                    return Err(ModifierError::AllExceptMalformed);
                };
                let keep: Vec<ObjectId> = args
                    .iter()
                    .skip(1)
                    .filter_map(|arg| column_argument(arg))
                    .collect();
                if keep.is_empty() {
                    return Err(ModifierError::AllExceptMalformed);
                }
                Ok(Modifier::AllExcept { table, keep })
            }
            "KEEPFILTERS" => {
                // `KEEPFILTERS` necesita la expresion para poder intersectar. El
                // filtro concreto lo resuelve `evaluate_atom`, que tiene los datos.
                Ok(Modifier::KeepFilters {
                    inner: args.first().cloned(),
                })
            }
            "CROSSFILTER" | "USERELATIONSHIP" => {
                let (Some(from), Some(to)) = (
                    args.first().and_then(|arg| column_argument(arg)),
                    args.get(1).and_then(|arg| column_argument(arg)),
                ) else {
                    return Err(ModifierError::CrossFilterMalformed(format!("{name}()")));
                };
                if name == "CROSSFILTER" {
                    Ok(Modifier::CrossFilter { from, to })
                } else {
                    Ok(Modifier::UseRelationship { from, to })
                }
            }
            other => Err(ModifierError::Unresolved(other.to_string())),
        },

        // Un booleano es un filtro: `Dim Product[Region] = "Norte"`.
        CheckedExpr::Binary {
            op, left, right, ..
        } if op.is_comparison() => {
            let (column, value) = match (left.as_ref(), right.as_ref()) {
                (CheckedExpr::Column { column, .. }, other) => (*column, other),
                (other, CheckedExpr::Column { column, .. }) => (*column, other),
                _ => return Err(ModifierError::NotAFilter(format!("{expr:?}"))),
            };
            let (operator, value) = match (op, literal_of(value)) {
                (datan_dax_syntax::BinaryOp::Equal, Some(v)) => (FilterOperator::Equal, v),
                (datan_dax_syntax::BinaryOp::NotEqual, Some(v)) => (FilterOperator::NotEqual, v),
                (datan_dax_syntax::BinaryOp::Less, Some(v)) => (FilterOperator::Less, v),
                (datan_dax_syntax::BinaryOp::LessEqual, Some(v)) => {
                    (FilterOperator::LessOrEqual, v)
                }
                (datan_dax_syntax::BinaryOp::Greater, Some(v)) => (FilterOperator::Greater, v),
                (datan_dax_syntax::BinaryOp::GreaterEqual, Some(v)) => {
                    (FilterOperator::GreaterOrEqual, v)
                }
                _ => return Err(ModifierError::NotAFilter(format!("{expr:?}"))),
            };
            Ok(Modifier::Filter(Filter {
                column,
                operator,
                source: crate::filter::FilterSource::Concrete(vec![value]),
            }))
        }

        other => Err(ModifierError::NotAFilter(format!("{other:?}"))),
    }
}

/// El nombre de una tabla, desde una referencia o desde una columna.
fn table_argument(expr: Option<&CheckedExpr>) -> Option<ObjectId> {
    match expr? {
        CheckedExpr::Table { table, .. } => Some(*table),
        // Una referencia sin calificar ya llego resuelta como `Column` o `Table`. No
        // existe `CheckedExpr::Reference`: el chequeo lo resolvio contra el modelo.
        _ => None,
    }
}

/// El id de una columna, desde una referencia calificada o sin calificar.
fn column_argument(expr: &CheckedExpr) -> Option<ObjectId> {
    match expr {
        CheckedExpr::Column { column, .. } => Some(*column),
        _ => None,
    }
}

fn literal_of(expr: &CheckedExpr) -> Option<FilterValue> {
    match expr {
        CheckedExpr::Text { value, .. } => Some(FilterValue::Text(value.clone())),
        CheckedExpr::Number { value, .. } => Some(if value.fract() == 0.0 {
            FilterValue::Integer(*value as i64)
        } else {
            FilterValue::Decimal(*value)
        }),
        CheckedExpr::Boolean { value, .. } => Some(FilterValue::Boolean(*value)),
        CheckedExpr::Blank { .. } => Some(FilterValue::Blank),
        _ => None,
    }
}

/// Un modificador de `CALCULATE`, ya resuelto contra el modelo.
#[derive(Debug, Clone, PartialEq)]
pub enum Modifier {
    /// `ALL()` vacia todo. `ALL(tabla)` quita los filtros de esa tabla y de las que se
    /// propagan desde ella.
    All { table: Option<ObjectId> },
    /// `ALLEXCEPT(tabla, col, ...)`: quita los filtros de la tabla menos los de las columnas
    /// que se conservan. Es el modificador que mas mal se entiende y mas se usa.
    AllExcept {
        table: ObjectId,
        keep: Vec<ObjectId>,
    },
    /// `KEEPFILTERS(expr)`: intersecta el contexto actual con el de la expresion.
    ///
    /// Guarda la expresion sin evaluar. Resolverla requiere los datos, y el contexto de
    /// filtro no los tiene: solo tiene predicados. Quien la resuelve es el interprete.
    KeepFilters { inner: Option<CheckedExpr> },
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
            // `ALLEXCEPT(T, c1, c2)` quita los filtros de `T` EXCEPTO los de las columnas
            // marcadas.
            //
            // Lo que hay que conservar no son solo los filtros de las columnas `keep`: hay
            // que conservar los filtros de la TABLA que se derivan de ellas. Un filtro sobre
            // `T[c1]` viaja a los hechos por propagacion, y quitarlo de `T` sin tocar el
            // semijoin daria un resultado incoherente: la dimension sin filtro y los hechos
            // con el filtro puesto.
            //
            // Por eso se conservan los filtros de `keep` Y los que salen de propagar `keep`
            // al resto del modelo. Lo que se quita es el resto de la tabla.
            let mut result = FilterContext::empty();
            for (column, filters) in base.entries() {
                let owner = model.column(*column).map(|(t, _)| t.id);
                let belongs = owner == Some(*table);

                if !belongs {
                    // Filtros de otras tablas: se conservan.
                    for filter in filters {
                        result = result.with(filter.clone());
                    }
                } else if keep.contains(column) {
                    for filter in filters {
                        result = result.with(filter.clone());
                    }
                }
                // Las columnas de la tabla que NO estan en `keep` se descartan.
            }

            // Los semijoin que vienen de las columnas `keep` hacia otras tablas tambien se
            // conservan: son la propagacion de los filtros que no estamos quitando.
            let kept_context = result.propagate(model);
            for (column, filters) in kept_context.entries() {
                let owner = model.column(*column).map(|(t, _)| t.id);
                if owner != Some(*table) {
                    for filter in filters {
                        if result.filters_on(*column).is_empty() {
                            result = result.with(filter.clone());
                        }
                    }
                }
            }
            result
        }

        // `KEEPFILTERS` con expresion la resuelve el interprete cuando tiene los datos.
        // Aqui no se puede: el contexto solo tiene predicados, no valores.
        Modifier::KeepFilters { .. } => base.clone(),

        Modifier::RemoveFilters { table: None } => base.without_everything(),

        Modifier::RemoveFilters { table: Some(table) } => base.without_table(model, *table),

        // `CROSSFILTER` y `USERELATIONSHIP` no quitan filtros: cambian COMO se propagan
        // los que ya estan. El contexto de filtro no lleva esa informacion, asi que el
        // efecto vive en la propagacion y aqui no se toca nada.
        Modifier::CrossFilter { .. } => base.clone(),
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
            if relationship.direction == datan_model::FilterDirection::Both
                && to_table == Some(current)
            {
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
