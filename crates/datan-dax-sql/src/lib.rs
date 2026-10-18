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

pub use quote::{escape_string, quote_identifier, quote_qualified};
pub use source::{SourceMap, SourceMapError};

use datan_dax_eval::{
    Atom, AtomKind, AtomSet, BinaryOp, FilterContext, FilterSource, Granularity, Shape,
};
use datan_model::{JoinEdge, Model, ObjectId};
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
    /// La medida que produce esta salida, si viene de una forma.
    ///
    /// El `name` es un slot (`atom_0`) porque el orden tiene que ser estable entre consultas.
    /// La medida es lo que el grid pinta en la cabecera. Sin este campo, una columna de
    /// salida no sabe que medida representa y la cabecera tendria que adivinarla.
    pub measure: Option<String>,
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
    //
    // La tabla de cada COLUMNA de una agregacion tambien cuenta. `SUM(qty * costo)` necesita
    // la dimension en el FROM aunque no agrupe por ella: sin el JOIN, el `costo` queda sin
    // calificar y DuckDB falla al ejecutar.
    let mut needed: Vec<ObjectId> = vec![fact_table];
    for table in required_tables(model, atoms) {
        if !needed.contains(&table) {
            needed.push(table);
        }
    }
    for column in grouping {
        let owner = owner_of(model, *column);
        if !needed.contains(&owner) {
            needed.push(owner);
        }
        // La columna de orden de un `sort_by` tiene que estar en el FROM, aunque no se
        // agrupe ni se agrega por ella: solo aparece dentro del `MIN` del ORDER BY.
        if let Some(sort) = model.column(*column).and_then(|(_, d)| d.sort_by) {
            let owner = owner_of(model, sort);
            if !needed.contains(&owner) {
                needed.push(owner);
            }
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
            measure: None,
        });
        select.push(format!(
            "{}.{}",
            aliases.of(owner_of(model, *column)),
            quote_identifier(physical)
        ));
    }

    // Calificado: una base adjunta vive bajo el alias del `ATTACH`.
    let (fact_physical, _) = sources.qualified(fact_table)?;
    let mut from_clause = aliases.of(fact_table);

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
            let (to_table_physical, _) = sources.qualified(to_table)?;
            from_clause.push_str(&format!(
                "\n  JOIN {} {} ON {}.{} = {}.{}",
                quote_qualified(&to_table_physical),
                aliases.of(to_table),
                aliases.of(from_table),
                quote_identifier(from_physical),
                aliases.of(to_table),
                quote_identifier(to_physical),
            ));
        }
    }

    // Dos caminos de salida, y la distincion importa.
    //
    // Con `outputs`, cada salida es una FORMA: una medida que es aritmetica entre
    // agregaciones emite una columna construida con la division, y las hojas que repite no
    // son copias sino la misma expresion escrita dos veces.
    //
    // Sin `outputs`, cada atomo es su propia columna. Es el caso de siempre: una medida de
    // una sola hoja, o una que el interprete tiene que resolver.
    if !atoms.outputs.is_empty() {
        for (index, output) in atoms.outputs.iter().enumerate() {
            let name = SqlPlan::atom_column(index);
            let expression = render_shape(&output.shape, atoms, model, sources, &aliases)?;
            select.push(format!("{expression} AS {}", quote_identifier(&name)));
            columns.push(OutputColumn {
                name,
                source: atoms.outputs_atom_column(&output.shape),
                atom: Some(output.label.clone()),
                measure: Some(output.label.clone()),
            });
        }
    } else {
        for (index, atom) in atoms.atoms.iter().enumerate() {
            let name = SqlPlan::atom_column(index);
            let expression = lower_atom(model, sources, &aliases, atom)?;
            select.push(format!("{expression} AS {}", quote_identifier(&name)));
            columns.push(OutputColumn {
                name,
                source: atom.column,
                atom: Some(atom.label.clone()),
                measure: None,
            });
        }
    }

    // El `WHERE` necesita un salto detras: pegado al `JOIN` produce `t1."id"WHERE`, que
    // DuckDB acepta como un identificador raro y falla con un error que no senala el sitio.
    let where_clause = match lower_filters(model, sources, &aliases, &atoms.grouping, filters)? {
        empty if empty.is_empty() => String::new(),
        clause => format!("\n{clause}"),
    };

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

    // El `ORDER BY` es lo que hace que el grid tenga filas estables.
    //
    // Sin el, DuckDB devuelve los grupos en el orden del hash de la tabla hash, que es
    // estable DENTRO de una consulta y distinto entre consultas segun el plan. El usuario ve
    // las filas reordenarse al cambiar un filtro de otra dimension, y el arnes diferencial
    // diverge en todas las medidas no aditivas solo por el orden.
    //
    // Ordena por las columnas de agrupamiento, que son la clave de la fila del informe. Las
    // medidas NO se ordenan: su orden cambiaria entre celdas sin motivo.
    //
    // Una columna con `sort_by` se ordena por SU columna de orden. En el SQL entra como
    // `MIN` porque no esta en el `GROUP BY`: como es funcionalmente dependiente de la que
    // agrupa, el minimo es EL valor, y el orden sale igual que en el interprete.
    let order_by = grouping
        .iter()
        .map(|column| {
            if let Some((sort_physical, sort_owner)) = model
                .column(*column)
                .and_then(|(_, definition)| definition.sort_by)
                .filter(|sort| *sort != *column)
                .and_then(|sort| sources.column(sort).ok())
            {
                return Ok(format!(
                    "MIN({}.{})",
                    aliases.of(sort_owner),
                    quote_identifier(sort_physical)
                ));
            }
            let (physical, _) = sources.column(*column)?;
            Ok(format!(
                "{}.{}",
                aliases.of(owner_of(model, *column)),
                quote_identifier(physical)
            ))
        })
        .collect::<Result<Vec<String>, LowerError>>()?;
    let order_clause = if order_by.is_empty() {
        String::new()
    } else {
        format!("\nORDER BY {}", order_by.join(", "))
    };

    let sql = format!(
        "SELECT\n  {}\nFROM {} {}{}\n\nGROUP BY {}{order_clause}",
        select.join(",\n  "),
        quote_qualified(&fact_physical),
        from_clause,
        where_clause,
        group_by
    );

    Ok(SqlPlan { sql, columns })
}

/// Baja un `AtomSet` a una consulta SIN agrupamiento: el total de la medida.
///
/// Es lo que alimenta una tarjeta KPI. `lower` exige agrupamiento porque una matriz sin
/// filas no es una matriz; un total no tiene filas por definicion, y forzar un `GROUP BY`
/// para calcularlo obligaria a sumar los grupos despues, que solo vale para medidas
/// aditivas. `AVG` de promedios no es el promedio, y `DISTINCTCOUNT` de conteos tampoco.
///
/// La forma es la misma que `lower` menos el `GROUP BY`, el `ORDER BY` y las columnas de
/// agrupamiento.
pub fn lower_scalar(
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

    let fact_table = first_fact_table(model, atoms).ok_or_else(|| {
        LowerError::Unsupported("no se encontro la tabla de hechos".to_string())
    })?;

    let mut needed: Vec<ObjectId> = vec![fact_table];
    for table in required_tables(model, atoms) {
        if !needed.contains(&table) {
            needed.push(table);
        }
    }

    let aliases = Aliases::new(&needed);
    let mut select = Vec::with_capacity(atoms.atoms.len());
    let mut columns = Vec::with_capacity(atoms.atoms.len());

    if !atoms.outputs.is_empty() {
        for (index, output) in atoms.outputs.iter().enumerate() {
            let name = SqlPlan::atom_column(index);
            let expression = render_shape(&output.shape, atoms, model, sources, &aliases)?;
            select.push(format!("{expression} AS {}", quote_identifier(&name)));
            columns.push(OutputColumn {
                name,
                source: atoms.outputs_atom_column(&output.shape),
                atom: Some(output.label.clone()),
                measure: Some(output.label.clone()),
            });
        }
    } else {
        for (index, atom) in atoms.atoms.iter().enumerate() {
            let name = SqlPlan::atom_column(index);
            let expression = lower_atom(model, sources, &aliases, atom)?;
            select.push(format!("{expression} AS {}", quote_identifier(&name)));
            columns.push(OutputColumn {
                name,
                source: atom.column,
                atom: Some(atom.label.clone()),
                measure: None,
            });
        }
    }

    let (fact_physical, _) = sources.qualified(fact_table)?;
    let mut from_clause = aliases.of(fact_table);
    for table in needed.iter().skip(1) {
        let path = model.join_path(fact_table, *table).ok_or_else(|| {
            LowerError::Unsupported(format!(
                "no hay relationship entre la tabla de hechos y la tabla {table}"
            ))
        })?;
        for edge in &path {
            let from_table = owner_of(model, edge.from_column);
            let to_table = owner_of(model, edge.to_column);
            let (from_physical, _) = sources.column(edge.from_column)?;
            let (to_physical, _) = sources.column(edge.to_column)?;
            let (to_table_physical, _) = sources.qualified(to_table)?;
            from_clause.push_str(&format!(
                "\n  JOIN {} {} ON {}.{} = {}.{}",
                quote_qualified(&to_table_physical),
                aliases.of(to_table),
                aliases.of(from_table),
                quote_identifier(from_physical),
                aliases.of(to_table),
                quote_identifier(to_physical),
            ));
        }
    }

    let where_clause = match lower_filters(model, sources, &aliases, &atoms.grouping, filters)? {
        empty if empty.is_empty() => String::new(),
        clause => format!("\n{clause}"),
    };

    let sql = format!(
        "SELECT\n  {}\nFROM {} {}{}\n",
        select.join(",\n  "),
        quote_qualified(&fact_physical),
        from_clause,
        where_clause
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

/// La tabla principal de la consulta.
///
/// `COUNTROWS` y las agregaciones sobre tabla no tienen columna: su tabla viene en el atomo.
/// Sin esta rama, `COUNTROWS('Fact Sales')` no baja a SQL y el caso queda sin fast path.
fn first_fact_table(model: &Model, atoms: &AtomSet) -> Option<ObjectId> {
    atoms
        .atoms
        .iter()
        .find_map(|atom| atom.anchor_table(model))
}

/// Todas las tablas que el conjunto de atomos necesita en el FROM.
///
/// Una agregacion sobre expresion puede tocar columnas de dos tablas: `SUM(qty * costo)`
/// necesita el hecho Y la dimension. Antes el lowering solo unia la tabla que Seahia la
/// columna agregada, asi que el `costo` de la dimension quedaba sin calificar y el SQL
/// fallaba al ejecutarse.
fn required_tables(model: &Model, atoms: &AtomSet) -> std::collections::BTreeSet<ObjectId> {
    let mut tables = std::collections::BTreeSet::new();
    for atom in &atoms.atoms {
        for column in atom.referenced_columns() {
            if let Some((table, _)) = model.column(column) {
                tables.insert(table.id);
            }
        }
        if let Some(table) = atom.table {
            tables.insert(table);
        }
    }
    for grouping in &atoms.grouping {
        if let Some((table, _)) = model.column(*grouping) {
            tables.insert(table.id);
        }
    }
    tables
}

/// Renderiza un `CheckedExpr` como SQL, calificando cada columna con el alias de su tabla.
///
/// Devuelve `None` si la expresion trae un nodo que no es aritmetica. Un `CALCULATE` o una
/// referencia a medida dentro del argumento de una agregacion no se renderiza: eso cae al
/// interprete, que lo resuelve por celda de grupo.
fn render_expression(
    expr: &datan_dax_eval::CheckedExpr,
    sources: &SourceMap,
    aliases: &Aliases,
) -> Result<String, LowerError> {
    use datan_dax_eval::CheckedExpr as E;

    Ok(match expr {
        E::Column { column, .. } => {
            let (name, table) = sources.column(*column)?;
            format!("{}.{}", aliases.of(table), quote_identifier(name))
        }
        E::Number { value, .. } => value.to_string(),
        E::Text { value, .. } => escape_string(value),
        E::Binary { left, right, op, .. } => {
            let left = render_expression(left, sources, aliases)?;
            let right = render_expression(right, sources, aliases)?;
            let operator = match op {
                datan_dax_eval::BinaryOp::Add => "+",
                datan_dax_eval::BinaryOp::Subtract => "-",
                datan_dax_eval::BinaryOp::Multiply => "*",
                datan_dax_eval::BinaryOp::Divide => "/",
                other => {
                    return Err(LowerError::Unsupported(format!(
                        "el operador `{other:?}` no se baja a SQL dentro de una agregacion"
                    )));
                }
            };
            // Los parentesis importan: `SUM(a + b * c)` no es `SUM(a) + SUM(b) * SUM(c)`.
            // Por eso cada lado va entre parentesis sin的条件, y la precedencia queda
            // determinada por el arbol, no por el texto.
            format!("({left} {operator} {right})")
        }
        E::Unary { op, operand, .. } => {
            let inner = render_expression(operand, sources, aliases)?;
            match op {
                datan_dax_eval::UnaryOp::Negate => format!("(-{inner})"),
                other => {
                    return Err(LowerError::Unsupported(format!(
                        "`{other:?}` no se baja a SQL dentro de una agregacion"
                    )));
                }
            }
        }
        other => {
            return Err(LowerError::Unsupported(format!(
                "`{other:?}` no se baja a SQL dentro de una agregacion"
            )));
        }
    })
}

/// Renderiza una forma sobre atomos.
///
/// Los parentesis van en cada nivel, no solo alrededor de los operandos binarios. Es lo que
/// hace que `SUM(a + b * c)` no se lea como `SUM(a) + SUM(b) * SUM(c)`: la precedencia la
/// decide el arbol, no el texto.
///
/// La division lleva `NULLIF` por el denominador. DuckDB devuelve `inf` para `x / 0` y
/// `NaN` para `0 / 0`; DAX devuelve BLANK, y el interprete tambien. Sin el `NULLIF` la misma
/// medida da un numero visiblemente falso en el fast path y un blanco en el interprete.
fn render_shape(
    shape: &Shape,
    atoms: &AtomSet,
    model: &Model,
    sources: &SourceMap,
    aliases: &Aliases,
) -> Result<String, LowerError> {
    Ok(match shape {
        Shape::Number(value) => value.to_string(),
        Shape::Atom(index) => {
            let atom = atoms.atoms.get(*index).ok_or_else(|| {
                LowerError::Unsupported(format!("la forma apunta al atomo {index}, que no existe"))
            })?;
            lower_atom(model, sources, aliases, atom)?
        }
        Shape::Binary { op, left, right } => {
            let left = render_shape(left, atoms, model, sources, aliases)?;
            let right = render_shape(right, atoms, model, sources, aliases)?;
            match op {
                BinaryOp::Add => format!("({left} + {right})"),
                BinaryOp::Subtract => format!("({left} - {right})"),
                BinaryOp::Multiply => format!("({left} * {right})"),
                BinaryOp::Divide => format!("({left} / NULLIF({right}, 0))"),
                // Las comparaciones y los conectores se bajan a un predicado. `IF` necesita
                // que su condicion sepa bajar, y una comparacion suelta en el nivel superior
                // de una medida tambien es un valor: antes esto era un error.
                BinaryOp::Equal => format!("({left} = {right})"),
                BinaryOp::NotEqual => format!("({left} <> {right})"),
                BinaryOp::Less => format!("({left} < {right})"),
                BinaryOp::LessEqual => format!("({left} <= {right})"),
                BinaryOp::Greater => format!("({left} > {right})"),
                BinaryOp::GreaterEqual => format!("({left} >= {right})"),
                BinaryOp::And => format!("({left} AND {right})"),
                BinaryOp::Or => format!("({left} OR {right})"),
                other => {
                    return Err(LowerError::Unsupported(format!(
                        "el operador `{}` no se baja a SQL dentro de una forma de medida",
                        other.as_str()
                    )));
                }
            }
        }
        // Una funcion escalar sobre formas: `DIVIDE`, `IF`, `COALESCE`, `SWITCH`. Los
        // argumentos se renderizan primero, para que la precedencia la decida el arbol.
        Shape::Call { name, args } => {
            let args = args
                .iter()
                .map(|arg| render_shape(arg, atoms, model, sources, aliases))
                .collect::<Result<Vec<_>, _>>()?;
            render_call(name, &args)?
        }
    })
}

/// Una funcion escalar sobre formas ya renderizadas.
///
/// La division lleva el mismo `NULLIF` que la binaria, por la misma razon: DuckDB devuelve
/// `inf` para `x / 0` y DAX devuelve BLANK. El tercer argumento de `DIVIDE` es el valor
/// alterno, y va en un `COALESCE` porque el denominador cero y un numerador BLANK dan los dos
/// el mismo nulo.
///
/// Una funcion que no este aqui devuelve un error, no un texto a medias: la forma solo se
/// construye para las funciones que esta funcion sabe bajar.
fn render_call(name: &str, args: &[String]) -> Result<String, LowerError> {
    let unsupported = || {
        LowerError::Unsupported(format!(
            "`{name}` con {} argumentos no se baja a SQL dentro de una forma de medida",
            args.len()
        ))
    };
    Ok(match (name, args) {
        ("DIVIDE", [a, b]) => format!("(({a}) / NULLIF(({b}), 0))"),
        ("DIVIDE", [a, b, alternate]) => {
            format!("COALESCE((({a}) / NULLIF(({b}), 0)), {alternate})")
        }
        ("IF", [condition, yes, no]) => {
            format!("(CASE WHEN {condition} THEN {yes} ELSE {no} END)")
        }
        ("IF", [condition, yes]) => format!("(CASE WHEN {condition} THEN {yes} END)"),
        ("COALESCE", args) if !args.is_empty() => format!("COALESCE({})", args.join(", ")),
        ("SWITCH", [expression, rest @ ..]) => {
            let mut sql = format!("(CASE {expression}");
            let mut index = 0;
            while index + 1 < rest.len() {
                sql.push_str(&format!(" WHEN {} THEN {}", rest[index], rest[index + 1]));
                index += 2;
            }
            // Un argumento que sobra al final es el valor por defecto, no una pareja sin
            // resultado.
            if index < rest.len() {
                sql.push_str(&format!(" ELSE {}", rest[index]));
            }
            sql.push_str(" END)");
            sql
        }
        _ => return Err(unsupported()),
    })
}

fn lower_atom(
    model: &Model,
    sources: &SourceMap,
    aliases: &Aliases,
    atom: &Atom,
) -> Result<String, LowerError> {
    // `COUNTROWS(tabla)` no tiene columna: es `COUNT(*)` sobre el FROM.
    if atom.kind == AtomKind::CountRows {
        return Ok("COUNT(*)".to_string());
    }

    // `SUM([Qty] * [Price])` no tiene columna: el argumento es una expresion. Se renderiza
    // entera, con sus tablas calificadas. Sin esto, las medidas de negocio mas simples del
    // plan, `Revenue` y `COGS`, no bajan a SQL.
    let column_name = match atom.column {
        Some(column_id) => {
            let (name, table) = sources.column(column_id)?;
            format!("{}.{}", aliases.of(table), quote_identifier(name))
        }
        None => match &atom.expression {
            Some(expression) => render_expression(expression, sources, aliases)?,
            None => {
                return Err(LowerError::Unsupported(format!(
                    "el atomo `{}` no tiene ni columna ni expresion",
                    atom.label
                )));
            }
        },
    };

    let function = match atom.kind {
        AtomKind::Sum => "SUM",
        AtomKind::Count => "COUNT",
        // `CountRows` se resuelve antes: no lleva columna.
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
            if matches!(filter.operator, datan_dax_eval::FilterOperator::In) {
                // `IN` con uno o varios valores, siempre entre parentesis. Con UN valor
                // salia `col IN 'x'`, que no es SQL: el parentesis no es opcional y la
                // consulta no parsea.
                return Ok(format!("{target} IN ({})", rendered.join(", ")));
            }
            // Un operador de comparacion con varios valores es el AND de cada uno: `col = a`
            // y `col = b` no los cumple ninguna fila, y `col <> a` y `col <> b` los cumple
            // casi todas. Tomar el primero daria un filtro que no es el que se pidio.
            Ok(rendered
                .iter()
                .map(|value| format!("{target} {operator} {value}"))
                .collect::<Vec<_>>()
                .join(" AND "))
        }

        FilterSource::Propagated {
            origin_column,
            origin_filters,
        } => {
            let (origin_physical, origin_table) = sources.column(*origin_column)?;
            let (table_physical, _) = sources.qualified(origin_table)?;

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
                quote_qualified(&table_physical),
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
        // DuckDB no acepta `DATE '19723'`: espera el literal de texto. Bajar el numero
        // crudo daba un error de conversion en tiempo de ejecucion, no al compilar.
        V::Date(days) => format!("DATE '{}'", datan_dax_eval::format_date(*days)),
        V::Boolean(b) => b.to_string(),
        V::Blank => "NULL".to_string(),
    }
}

/// El SQL que se genera cuando no hay filtros. Se usa en los tests como referencia.
pub fn empty_filter_marker() -> &'static str {
    ""
}

/// El camino mas corto entre dos tablas por el grafo de relaciones.
///
/// Vive en el modelo, no aqui. El interprete necesita la misma pregunta para resolver una
/// agregacion que cruza tablas, y dos implementaciones de BFS divergen en cuanto una
/// relationship se filtra y la otra no.
fn shortest_join_path(model: &Model, from: ObjectId, to: ObjectId) -> Option<Vec<JoinEdge>> {
    model.join_path(from, to)
}

const _: Option<Granularity> = None;