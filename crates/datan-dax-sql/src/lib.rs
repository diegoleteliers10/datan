//! Bajada del Measure IR a SQL de DuckDB.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. El SQL se produce como texto y se testea sin base de
//! datos. `datan-storage` lo ejecuta; este crate solo lo escribe.
//!
//! Esa separacion es deliberada: permite diffear el SQL generado, guardarlo en el modelo,
//! y compararlo entre versiones. Es una de las razones por las que el Measure IR es un IR y
//! no "DAX compila a SQL".
//!
//! # La forma que genera
//!
//! Un `AtomSet` produce **un** `SELECT` con todos los atomos en la misma pasada:
//!
//! ```sql
//! SELECT
//!   "Region",
//!   SUM("Qty")            AS "atom_0",
//!   SUM("Qty" * "Price")  AS "atom_1"
//! FROM "Fact Sales"
//! WHERE "ProductId" IN (
//!   SELECT "ProductId" FROM "Dim Product" WHERE "Region" = 'Norte'
//! )
//! GROUP BY "Region"
//! ```
//!
//! Un filtro propagado se vuelve un `IN` sobre la tabla de origen. No un JOIN. El motivo es
//! concreto: un JOIN con la dimension trae filas repetidas y rompe `COUNT(DISTINCT ...)` y
//! `SUM` sobre columnas de la dimension. Un `IN` es semijoin, no multiplica.
//!
//! Y todas las celdas de grupo salen en la misma pasada. Ese es el criterio del dia 2 del
//! spike: un `HASH_GROUP_BY`, ninguna subconsulta por celda.

mod quote;
mod source;

pub use quote::{escape_string, quote_identifier};
pub use source::{SourceMap, SourceMapError};

use datan_dax_eval::{Atom, AtomKind, AtomSet, FilterContext, FilterSource, Granularity};
use datan_model::{Model, ObjectId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LowerError {
    #[error("no se puede bajar a SQL: {0}")]
    Unsupported(String),
    #[error("falta el mapeo fisico de {0}")]
    MissingMapping(String),
    #[error("un filtro necesita un valor concreto o una columna de origen valida")]
    BadFilter,
    #[error("la consulta necesita al menos una columna de agrupamiento")]
    NoGrouping,
    #[error(transparent)]
    Source(#[from] SourceMapError),
}

/// Una columna de salida del SQL generado.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputColumn {
    /// Nombre en el resultado. Los atomos se llaman `atom_0`, `atom_1`, por indice, para que
    /// el orden sea estable y comparable entre consultas.
    pub name: String,
    /// La columna del modelo que produce esta salida, si es una agrupacion.
    pub source: Option<ObjectId>,
    /// La etiqueta del atomo, si es un atomo.
    pub atom: Option<String>,
}

/// El plan generado, mas el SQL.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlPlan {
    pub sql: String,
    pub columns: Vec<OutputColumn>,
}

impl SqlPlan {
    /// El nombre de la columna de salida de un atomo, dado su indice.
    pub fn atom_column(index: usize) -> String {
        format!("atom_{index}")
    }
}

/// Baja un `AtomSet` a una unica query.
///
/// Falla si algun atomo necesita el interprete. Quien llama debe comprobar
/// `AtomSet::needs_interpreter()` antes, o usar `try_lower` y dejar que lo diga.
pub fn lower(
    model: &Model,
    sources: &SourceMap,
    atoms: &AtomSet,
    filters: &FilterContext,
) -> Result<SqlPlan, LowerError> {
    if atoms.needs_interpreter() {
        let culprit = atoms
            .atoms
            .iter()
            .find(|atom| atom.needs_interpreter())
            .map(|atom| atom.label.clone())
            .unwrap_or_default();
        return Err(LowerError::Unsupported(format!(
            "el atomo `{culprit}` necesita el interprete"
        )));
    }

    let grouping = &atoms.grouping;
    if grouping.is_empty() {
        return Err(LowerError::NoGrouping);
    }

    let mut columns = Vec::with_capacity(grouping.len() + atoms.atoms.len());
    let mut select = Vec::with_capacity(grouping.len() + atoms.atoms.len());

    // La tabla principal es la que contiene la columna agregada de los atomos. Si los
    // atomos viven en tablas distintas, es un modelo mal formado.
    let fact_table = first_fact_table(model, atoms).ok_or_else(|| {
        LowerError::Unsupported("no se encontro la tabla de hechos".to_string())
    })?;

    // Las columnas de agrupamiento pueden vivir en tablas que no son la de hechos. Por eso
    // hay que traerlas con un JOIN por la clave de la relacion.
    //
    // El filtro propagado, en cambio, SI es un `IN`: ahi solo hace falta saber si la clave
    // existe, y un JOIN multiplicaria filas y romperia `COUNT(DISTINCT)`.
    //
    // Los alias `t0`, `t1` evitan colisiones: `fact_sales` y `dim_product` tienen las dos
    // una columna `product_id`.
    let mut needed: Vec<ObjectId> = vec![fact_table];
    for column in grouping {
        let owner = owner_of(model, *column);
        if !needed.contains(&owner) {
            needed.push(owner);
        }
    }

    let aliases = Aliases::new(&needed);

    // Las columnas de agrupamiento NO se aliasean. Su nombre fisico ya es el nombre de
    // salida, y DuckDB rechaza un alias dentro del `GROUP BY` incluso cuando el alias es
    // identico al nombre de la columna: "Alias with name X exists, but aliases cannot be
    // used as part of an expression in the GROUP BY". Se prefija con el alias de TABLA,
    // que si es necesario para desambiguar.
    for column in grouping {
        let (physical, _) = sources.column(*column)?;
        columns.push(OutputColumn {
            name: physical.to_string(),
            source: Some(*column),
            atom: None,
        });
        select.push(format!(
            "{}.{}",
            aliases.of(owner_of(model, *column)),
            quote_identifier(physical)
        ));
    }

    let fact_physical = sources.table(fact_table)?.0.to_string();
    let mut from_clause = format!("{} {}", quote_identifier(&fact_physical), aliases.of(fact_table));

    // Un JOIN por cada tabla extra, por el camino mas corto desde la tabla de hechos.
    for table in needed.iter().skip(1) {
        let path = shortest_join_path(model, fact_table, *table)
            .ok_or_else(|| {
                LowerError::Unsupported(format!(
                    "no hay relationship entre la tabla de hechos y la tabla {table} que agrupa"
                ))
            })?;
        for edge in &path {
            let from_table = owner_of(model, edge.from_column);
            let to_table = owner_of(model, edge.to_column);
            let (from_physical, _) = sources.column(edge.from_column)?;
            let (to_physical, _) = sources.column(edge.to_column)?;
            let _ = from_table;
            let (to_table_physical, _) = sources.table(to_table)?;
            from_clause.push_str(&format!(
                "\n  JOIN {} {} ON {}.{} = {}.{}",
                quote_identifier(to_table_physical),
                aliases.of(to_table),
                aliases.of(from_table),
                quote_identifier(from_physical),
                aliases.of(to_table),
                quote_identifier(to_physical),
            ));
        }
    }

    for (index, atom) in atoms.atoms.iter().enumerate() {
        let name = SqlPlan::atom_column(index);
        let expression = lower_atom(model, sources, &aliases, atom)?;
        select.push(format!("{expression} AS {}", quote_identifier(&name)));
        columns.push(OutputColumn {
            name,
            source: atom.column,
            atom: Some(atom.label.clone()),
        });
    }

    let where_clause = lower_filters(model, sources, &aliases, &atoms.grouping, filters)?;

    // El `GROUP BY` repite el nombre de la columna, NO el alias. DuckDB rechaza un alias
    // dentro del `GROUP BY`: "aliases cannot be used as part of an expression in the GROUP
    // BY". Por eso aqui se usa el nombre entrecomillado, no el del `SELECT`.
    let group_by = grouping
        .iter()
        .map(|column| {
            sources
                .column(*column)
                .map(|(physical, _)| {
                    format!(
                        "{}.{}",
                        aliases.of(owner_of(model, *column)),
                        quote_identifier(physical)
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");

    let sql = format!(
        "SELECT\n  {}\nFROM {}\n{}\nGROUP BY {}",
        select.join(",\n  "),
        from_clause,
        where_clause,
        group_by
    );

    Ok(SqlPlan { sql, columns })
}

/// Intenta bajar a SQL, y devuelve `None` si hace falta el interprete.
pub fn try_lower(
    model: &Model,
    sources: &SourceMap,
    atoms: &AtomSet,
    filters: &FilterContext,
) -> Option<SqlPlan> {
    lower(model, sources, atoms, filters).ok()
}

/// Los alias de tabla del `FROM`. Necesarios porque dos tablas del star schema comparten
/// nombres de columna: `fact_sales[product_id]` y `dim_product[product_id]`.
pub struct Aliases {
    by_table: std::collections::HashMap<ObjectId, String>,
}

impl Aliases {
    fn new(tables: &[ObjectId]) -> Self {
        Self {
            by_table: tables
                .iter()
                .enumerate()
                .map(|(index, table)| (*table, format!("t{index}")))
                .collect(),
        }
    }

    fn of(&self, table: ObjectId) -> String {
        self.by_table
            .get(&table)
            .cloned()
            .unwrap_or_else(|| "t0".to_string())
    }
}

fn owner_of(model: &Model, column: ObjectId) -> ObjectId {
    model
        .column(column)
        .map(|(table, _)| table.id)
        .unwrap_or(ObjectId(0))
}

fn first_fact_table(model: &Model, atoms: &AtomSet) -> Option<ObjectId> {
    atoms
        .atoms
        .iter()
        .filter_map(|atom| atom.column)
        .find_map(|column| model.column(column).map(|(table, _)| table.id))
}

fn lower_atom(
    model: &Model,
    sources: &SourceMap,
    aliases: &Aliases,
    atom: &Atom,
) -> Result<String, LowerError> {
    let column_id = atom.column.ok_or_else(|| {
        LowerError::Unsupported(format!(
            "el atomo `{}` agrega sobre una expresion, no sobre una columna",
            atom.label
        ))
    })?;
    let (column_name, column_table) = sources.column(column_id)?;
    let column_name = format!(
        "{}.{}",
        aliases.of(column_table),
        quote_identifier(column_name)
    );

    let function = match atom.kind {
        AtomKind::Sum => "SUM",
        AtomKind::Count => "COUNT",
        AtomKind::CountRows => "COUNT(*)",
        AtomKind::Min => "MIN",
        AtomKind::Max => "MAX",
        AtomKind::Average => "AVG",
        AtomKind::DistinctCount => return Ok(format!("COUNT(DISTINCT {column_name})")),
        AtomKind::Expression => {
            return Err(LowerError::Unsupported(format!(
                "el atomo `{}` es una agregacion sobre expresion",
                atom.label
            )));
        }
    };

    // `COUNTROWS(tabla)` no lleva columna. Los demas si.
    let _ = model;
    Ok(format!("{function}({column_name})"))
}

/// Convierte el contexto de filtro en un `WHERE`.
///
/// Un filtro concreto se escribe como predicado. Uno propagado se convierte en un `IN` sobre
/// la tabla de origen, con los filtros del origen dentro. No es un JOIN: ver la cabecera del
/// modulo.
fn lower_filters(
    model: &Model,
    sources: &SourceMap,
    aliases: &Aliases,
    grouping: &[ObjectId],
    filters: &FilterContext,
) -> Result<String, LowerError> {
    let propagated = filters.propagate(model);
    let mut predicates = Vec::new();
    // Un filtro propagado produce un semijoin. Si varias columnas de la misma tabla tienen
    // uno, se combinan en un solo `IN` con la clausula interna unida. Repetir el semijoin
    // por columna es correcto pero lento, y el plan generado se vuelve ilegible.
    let mut seen_propagated: Vec<ObjectId> = Vec::new();

    for (column, column_filters) in propagated.entries() {
        // Una columna de agrupamiento que aparece en el filtro es una redundancia: el
        // `GROUP BY` ya la agrupa. Meterla en el `WHERE` ademas es una contradiccion si el
        // valor cambia por celda.
        if grouping.contains(column) {
            continue;
        }
        let (target, target_table) = sources.column(*column)?;
        let qualified = format!("{}.{}", aliases.of(target_table), quote_identifier(target));

        // Una columna que YA tiene un semijoin no necesita su predicado directo tampoco.
        // `propagate` corre en rondas y el mismo semijoin puede aparecer dos veces en la
        // columna: generar `"ProductId" IN (...) AND "ProductId" IN (...) AND "Region" =
        // 'X'` es correcto pero ilegible, y hace el planner repetir el semijoin.
        if seen_propagated.contains(&target_table) {
            continue;
        }

        let mut wrote_propagated = false;
        for filter in column_filters {
            if matches!(filter.source, FilterSource::Propagated { .. }) {
                if wrote_propagated {
                    continue;
                }
                wrote_propagated = true;
                seen_propagated.push(target_table);
            }
            predicates.push(lower_filter(sources, &qualified, filter)?);
        }
    }

    if predicates.is_empty() {
        return Ok(String::new());
    }
    Ok(format!("WHERE {}", predicates.join(" AND ")))
}

fn lower_filter(
    sources: &SourceMap,
    target: &str,
    filter: &datan_dax_eval::Filter,
) -> Result<String, LowerError> {
    let target = target.to_string();
    match &filter.source {
        FilterSource::Concrete(values) => {
            let operator = match filter.operator {
                datan_dax_eval::FilterOperator::Equal => "=",
                datan_dax_eval::FilterOperator::NotEqual => "<>",
                datan_dax_eval::FilterOperator::Less => "<",
                datan_dax_eval::FilterOperator::LessOrEqual => "<=",
                datan_dax_eval::FilterOperator::Greater => ">",
                datan_dax_eval::FilterOperator::GreaterOrEqual => ">=",
                datan_dax_eval::FilterOperator::In => "IN",
            };
            let rendered: Vec<String> = values.iter().map(render_value).collect();
            if rendered.is_empty() {
                // Un `IN` vacio no es `FALSE` en DAX: un filtro vacio deja pasar todo. Se
                // escribe el caso explicito para que el SQL signifique lo mismo.
                return Ok("1 = 1".to_string());
            }
            if matches!(filter.operator, datan_dax_eval::FilterOperator::In)
                && rendered.len() > 1
            {
                Ok(format!("{target} IN ({})", rendered.join(", ")))
            } else {
                Ok(format!("{target} {operator} {}", rendered[0]))
            }
        }

        FilterSource::Propagated {
            origin_column,
            origin_filters,
        } => {
            let (origin_physical, origin_table) = sources.column(*origin_column)?;
            let (table_physical, _) = sources.table(origin_table)?;

            // Los filtros del origen se aniden un nivel. Es un semijoin, no un join: no
            // duplica filas de la tabla de hechos.
            let inner: Vec<String> = origin_filters
                .iter()
                .map(|inner_filter| {
                    let (inner_column, _) = sources.column(inner_filter.column)?;
                    // Dentro del subselect no hay alias: la columna es de una sola tabla.
                    lower_filter(sources, &quote_identifier(inner_column), inner_filter)
                })
                .collect::<Result<Vec<_>, _>>()?;

            let inner_where = if inner.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", inner.join(" AND "))
            };

            Ok(format!(
                "{target} IN (SELECT {} FROM {}{})",
                quote_identifier(&origin_physical),
                quote_identifier(&table_physical),
                inner_where
            ))
        }
    }
}

fn render_value(value: &datan_dax_eval::FilterValue) -> String {
    use datan_dax_eval::FilterValue as V;
    match value {
        V::Text(text) => escape_string(text),
        V::Integer(n) => n.to_string(),
        V::Decimal(n) => n.to_string(),
        V::Date(days) => format!("DATE '{days}'"),
        V::Boolean(b) => b.to_string(),
        V::Blank => "NULL".to_string(),
    }
}

/// El SQL que se genera cuando no hay filtros. Se usa en los tests como referencia.
pub fn empty_filter_marker() -> &'static str {
    ""
}

/// Un arista del camino de join: la relationship que conecta dos columnas.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinEdge {
    pub from_column: ObjectId,
    pub to_column: ObjectId,
}

/// El camino mas corto entre dos tablas por el grafo de relaciones.
///
/// Se busca con BFS sobre las relaciones activas. Devuelve los aristas en orden, desde la
/// tabla de hechos hacia la tabla destino. Si no hay camino, el modelo esta desconectado y
/// la consulta no se puede escribir.
fn shortest_join_path(model: &Model, from: ObjectId, to: ObjectId) -> Option<Vec<JoinEdge>> {
    if from == to {
        return Some(Vec::new());
    }
    let mut queue: std::collections::VecDeque<(ObjectId, Vec<JoinEdge>)> =
        std::collections::VecDeque::new();
    let mut visited = vec![from];

    queue.push_back((from, Vec::new()));
    while let Some((table, path)) = queue.pop_front() {
        for relationship in &model.relationships {
            if relationship.role != datan_model::RelationshipRole::Active {
                continue;
            }
            let from_table = model.column(relationship.from_column).map(|(t, _)| t.id);
            let to_table = model.column(relationship.to_column).map(|(t, _)| t.id);

            // La relationship se puede recorrer en los dos sentidos: el `FROM` de un JOIN no
            // distingue dimension de hecho.
            let edges: [(Option<ObjectId>, JoinEdge); 2] = [
                (
                    from_table,
                    JoinEdge {
                        from_column: relationship.from_column,
                        to_column: relationship.to_column,
                    },
                ),
                (
                    to_table,
                    JoinEdge {
                        from_column: relationship.to_column,
                        to_column: relationship.from_column,
                    },
                ),
            ];

            for (source, edge) in edges {
                // `edge.from_column` es la columna del lado de `source`, y
                // `edge.to_column` la del lado del destino. Eso ya codifica la direccion:
                // no hace falta volver a mirar la relationship.
                let Some(source) = source else { continue };
                if source != table {
                    continue;
                }
                let destination = model
                    .column(edge.to_column)
                    .map(|(t, _)| t.id)?;

                if destination == to {
                    let mut full = path.clone();
                    full.push(edge);
                    return Some(full);
                }
                if !visited.contains(&destination) {
                    visited.push(destination);
                    let mut extended = path.clone();
                    extended.push(edge);
                    queue.push_back((destination, extended));
                }
            }
        }
    }
    None
}

const _: Option<Granularity> = None;