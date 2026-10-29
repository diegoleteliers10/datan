use datan_model::Model;

use crate::data_doc::{DataDoc, QueryDoc, QueryStep};
use datan_storage::Engine;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QueryCompileError {
    #[error("el nombre de la tabla está vacío")]
    EmptyTable,
    #[error("la columna `{0}` no existe en la consulta")]
    UnknownColumn(String),
    #[error("el nombre de columna está vacío")]
    EmptyColumnName,
    #[error("ya existe la columna `{0}`")]
    DuplicateColumn(String),
    #[error("no se puede quitar la última columna")]
    LastColumn,
    #[error("la consulta debe ser una sola sentencia SELECT")]
    UnsafeSourceQuery,
}

pub fn compile_view(
    table: &str,
    columns: &[String],
    query: Option<&QueryDoc>,
) -> Result<String, QueryCompileError> {
    compile_view_for(table, columns, query, QueryPurpose::Model).map(|(sql, _)| sql)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryPurpose {
    Model,
    Preview,
}

fn compile_view_for(
    table: &str,
    columns: &[String],
    query: Option<&QueryDoc>,
    purpose: QueryPurpose,
) -> Result<(String, Vec<String>), QueryCompileError> {
    if table.trim().is_empty() {
        return Err(QueryCompileError::EmptyTable);
    }
    let source_columns = query
        .map(|query| query.source_columns.as_slice())
        .filter(|columns| !columns.is_empty())
        .unwrap_or(columns);
    let mut schema = source_columns.to_vec();
    let mut sql = match query.and_then(|query| query.source_sql.as_deref()) {
        Some(source_sql) if is_read_only_select(source_sql) => {
            source_sql.trim().trim_end_matches(';').to_string()
        }
        Some(_) => return Err(QueryCompileError::UnsafeSourceQuery),
        None => format!("SELECT * FROM pg.{}", quote(table)),
    };
    let mut error_prefix = "__datan_conversion_error_".to_string();
    while schema.iter().any(|name| name.to_ascii_lowercase().starts_with(&error_prefix))
        || query.into_iter().flat_map(|query| &query.steps).any(|step| {
            matches!(step, QueryStep::RenameColumn { new_name, .. } if new_name.to_ascii_lowercase().starts_with(&error_prefix))
        })
    {
        error_prefix.push('_');
    }
    let mut error_columns: Vec<String> = if purpose == QueryPurpose::Preview {
        (0..schema.len())
            .map(|index| format!("{error_prefix}{index}"))
            .collect()
    } else {
        Vec::new()
    };
    if purpose == QueryPurpose::Preview {
        let projection = schema
            .iter()
            .map(|name| quote(name))
            .chain(
                error_columns
                    .iter()
                    .map(|name| format!("FALSE AS {}", quote(name))),
            )
            .collect::<Vec<_>>()
            .join(", ");
        sql = format!("SELECT {projection} FROM ({sql}) AS preview_source");
    }
    if let Some(query) = query {
        for (index, step) in query.steps.iter().enumerate() {
            match step {
                QueryStep::RemoveEmptyRows { column } => {
                    if !schema
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(column))
                    {
                        return Err(QueryCompileError::UnknownColumn(column.clone()));
                    }
                    sql = format!(
                        "SELECT * FROM ({sql}) AS step_{index} WHERE \
                         NULLIF(TRIM(CAST({} AS VARCHAR)), '') IS NOT NULL",
                        quote(column)
                    );
                }
                QueryStep::SortRows { column, direction } => {
                    if !schema
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(column))
                    {
                        return Err(QueryCompileError::UnknownColumn(column.clone()));
                    }
                    let direction = match direction {
                        crate::data_doc::SortDirection::Ascending => "ASC",
                        crate::data_doc::SortDirection::Descending => "DESC",
                    };
                    sql = format!(
                        "SELECT * FROM ({sql}) AS step_{index} ORDER BY {} {direction} NULLS LAST",
                        quote(column)
                    );
                }
                QueryStep::RemoveDuplicates => {
                    if purpose == QueryPurpose::Preview {
                        let values = schema
                            .iter()
                            .map(|name| quote(name))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let projection =
                            schema
                                .iter()
                                .map(|name| quote(name))
                                .chain(error_columns.iter().map(|name| {
                                    format!("BOOL_OR({}) AS {}", quote(name), quote(name))
                                }))
                                .collect::<Vec<_>>()
                                .join(", ");
                        sql = format!(
                            "SELECT {projection} FROM ({sql}) AS step_{index} GROUP BY {values}"
                        );
                    } else {
                        sql = format!("SELECT DISTINCT * FROM ({sql}) AS step_{index}");
                    }
                }
                QueryStep::ChangeType {
                    column,
                    column_type,
                } => {
                    if !schema
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(column))
                    {
                        return Err(QueryCompileError::UnknownColumn(column.clone()));
                    }
                    let projection = schema
                        .iter()
                        .map(|known| {
                            if known.eq_ignore_ascii_case(column) {
                                format!(
                                    "TRY_CAST({} AS {}) AS {}",
                                    quote(known),
                                    column_type.sql_type(),
                                    quote(known)
                                )
                            } else {
                                quote(known)
                            }
                        })
                        .chain(error_columns.iter().enumerate().map(|(index, error)| {
                            if schema[index].eq_ignore_ascii_case(column) {
                                format!(
                                    "({} OR ({} IS NOT NULL AND TRY_CAST({} AS {}) IS NULL)) AS {}",
                                    quote(error),
                                    quote(column),
                                    quote(column),
                                    column_type.sql_type(),
                                    quote(error)
                                )
                            } else {
                                quote(error)
                            }
                        }))
                        .collect::<Vec<_>>()
                        .join(", ");
                    sql = format!("SELECT {projection} FROM ({sql}) AS step_{index}");
                }
                QueryStep::RenameColumn { column, new_name } => {
                    let Some(column_index) = schema
                        .iter()
                        .position(|known| known.eq_ignore_ascii_case(column))
                    else {
                        return Err(QueryCompileError::UnknownColumn(column.clone()));
                    };
                    if new_name.trim().is_empty() {
                        return Err(QueryCompileError::EmptyColumnName);
                    }
                    if schema.iter().enumerate().any(|(index, known)| {
                        index != column_index && known.eq_ignore_ascii_case(new_name.trim())
                    }) {
                        return Err(QueryCompileError::DuplicateColumn(
                            new_name.trim().to_string(),
                        ));
                    }
                    let projection = schema
                        .iter()
                        .enumerate()
                        .map(|(index, known)| {
                            if index == column_index {
                                format!("{} AS {}", quote(known), quote(new_name.trim()))
                            } else {
                                quote(known)
                            }
                        })
                        .chain(error_columns.iter().map(|name| quote(name)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    sql = format!("SELECT {projection} FROM ({sql}) AS step_{index}");
                    schema[column_index] = new_name.trim().to_string();
                }
                QueryStep::RemoveColumn { column } => {
                    let Some(column_index) = schema
                        .iter()
                        .position(|known| known.eq_ignore_ascii_case(column))
                    else {
                        return Err(QueryCompileError::UnknownColumn(column.clone()));
                    };
                    if schema.len() <= 1 {
                        return Err(QueryCompileError::LastColumn);
                    }
                    let projection = schema
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| *index != column_index)
                        .map(|(_, known)| quote(known))
                        .chain(
                            error_columns
                                .iter()
                                .enumerate()
                                .filter(|(index, _)| *index != column_index)
                                .map(|(_, name)| quote(name)),
                        )
                        .collect::<Vec<_>>()
                        .join(", ");
                    sql = format!("SELECT {projection} FROM ({sql}) AS step_{index}");
                    schema.remove(column_index);
                    if purpose == QueryPurpose::Preview {
                        error_columns.remove(column_index);
                    }
                }
            }
        }
    }
    Ok((sql, error_columns))
}

pub fn is_read_only_select(sql: &str) -> bool {
    let Some(tokens) = sql_tokens(sql) else {
        return false;
    };
    let Some(SqlToken::Word(first, 0)) = tokens.first() else {
        return false;
    };
    let first = first.as_str();
    if !first.eq_ignore_ascii_case("select") && !first.eq_ignore_ascii_case("with") {
        return false;
    }

    if tokens
        .iter()
        .any(|token| matches!(token, SqlToken::Semicolon))
    {
        let Some(last) = tokens.last() else {
            return false;
        };
        if !matches!(last, SqlToken::Semicolon) {
            return false;
        }
    }

    let mut depth = 0usize;
    let mut root_select = first.eq_ignore_ascii_case("select");
    let mut previous_open = false;
    for token in &tokens {
        match token {
            SqlToken::Open => {
                depth += 1;
                previous_open = true;
            }
            SqlToken::Close => {
                let Some(next_depth) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next_depth;
                previous_open = false;
            }
            SqlToken::Semicolon => previous_open = false,
            SqlToken::Word(word, token_depth) => {
                if *token_depth == 0 && word.eq_ignore_ascii_case("select") {
                    root_select = true;
                }
                if previous_open && is_mutating_statement(word) {
                    return false;
                }
                previous_open = false;
            }
            SqlToken::Other => previous_open = false,
        }
    }
    depth == 0 && root_select
}

#[derive(Debug, PartialEq, Eq)]
enum SqlToken {
    Word(String, usize),
    Open,
    Close,
    Semicolon,
    Other,
}

fn is_mutating_statement(keyword: &str) -> bool {
    [
        "insert", "update", "delete", "merge", "create", "alter", "drop", "truncate", "copy",
        "call", "execute", "attach", "detach", "pragma", "vacuum", "export", "import", "grant",
        "revoke", "begin", "commit", "rollback",
    ]
    .iter()
    .any(|candidate| keyword.eq_ignore_ascii_case(candidate))
}

fn sql_tokens(sql: &str) -> Option<Vec<SqlToken>> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut depth = 0usize;
    let mut ended = false;

    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
        } else if bytes[index..].starts_with(b"--") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes[index..].starts_with(b"/*") {
            index += 2;
            let mut comment_depth = 1usize;
            while index < bytes.len() && comment_depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    comment_depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    comment_depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            if comment_depth != 0 {
                return None;
            }
        } else if bytes[index] == b'\'' || bytes[index] == b'"' {
            let quote = bytes[index];
            index += 1;
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == b'\\' && index + 1 < bytes.len() {
                    index += 2;
                } else if bytes[index] == quote {
                    if bytes.get(index + 1) == Some(&quote) {
                        index += 2;
                    } else {
                        index += 1;
                        closed = true;
                        break;
                    }
                } else {
                    index += 1;
                }
            }
            if !closed {
                return None;
            }
            if ended {
                return None;
            }
            tokens.push(SqlToken::Other);
        } else if bytes[index] == b'$' {
            let tag_end = bytes[index + 1..]
                .iter()
                .position(|byte| *byte == b'$')
                .map(|offset| index + 1 + offset);
            if let Some(tag_end) = tag_end {
                let tag = &bytes[index..=tag_end];
                let content_start = tag_end + 1;
                let closing = bytes[content_start..]
                    .windows(tag.len())
                    .position(|window| window == tag)?
                    + content_start;
                index = closing + tag.len();
                if ended {
                    return None;
                }
                tokens.push(SqlToken::Other);
            } else {
                if ended {
                    return None;
                }
                tokens.push(SqlToken::Word("$".to_string(), depth));
                index += 1;
            }
        } else if bytes[index] == b';' {
            if ended {
                return None;
            }
            ended = true;
            tokens.push(SqlToken::Semicolon);
            index += 1;
        } else if bytes[index] == b'(' {
            if ended {
                return None;
            }
            tokens.push(SqlToken::Open);
            depth += 1;
            index += 1;
        } else if bytes[index] == b')' {
            if ended {
                return None;
            }
            depth = depth.checked_sub(1)?;
            tokens.push(SqlToken::Close);
            index += 1;
        } else if bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'$') {
            if ended {
                return None;
            }
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'$'))
            {
                index += 1;
            }
            tokens.push(SqlToken::Word(
                std::str::from_utf8(&bytes[start..index]).ok()?.to_string(),
                depth,
            ));
        } else {
            if ended {
                return None;
            }
            tokens.push(SqlToken::Other);
            index += 1;
        }
    }
    (depth == 0 && !tokens.is_empty()).then_some(tokens)
}

pub fn compile_preview_query(
    table: &str,
    columns: &[String],
    query: Option<&QueryDoc>,
) -> Result<String, QueryCompileError> {
    compile_preview_from(
        table,
        columns,
        query,
        &format!("datan_model.{}", quote(table)),
        None,
    )
}

pub fn compile_preview_with_errors(
    table: &str,
    columns: &[String],
    query: Option<&QueryDoc>,
) -> Result<String, QueryCompileError> {
    let (body, errors) = compile_view_for(table, columns, query, QueryPurpose::Preview)?;
    let projection = output_columns(columns, query)?
        .iter()
        .chain(errors.iter())
        .map(|name| {
            format!(
                "CAST(datan_preview.{} AS VARCHAR) AS {}",
                quote(name),
                quote(name)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    compile_preview_from(
        table,
        columns,
        query,
        &format!("({body}) AS datan_preview"),
        Some(&projection),
    )
}

fn compile_preview_from(
    table: &str,
    columns: &[String],
    query: Option<&QueryDoc>,
    source: &str,
    projection_override: Option<&str>,
) -> Result<String, QueryCompileError> {
    if table.trim().is_empty() {
        return Err(QueryCompileError::EmptyTable);
    }
    let output_columns = output_columns(columns, query)?;
    let projection = output_columns
        .iter()
        .map(|column| quote(column))
        .collect::<Vec<_>>()
        .join(", ");
    let projection = projection_override.unwrap_or(&projection);
    let mut sql = format!("SELECT {projection} FROM {source}");
    let sort = query.and_then(|query| {
        let mut schema = if query.source_columns.is_empty() {
            columns.to_vec()
        } else {
            query.source_columns.clone()
        };
        let mut sort = None;
        for step in &query.steps {
            match step {
                QueryStep::SortRows { column, direction } => {
                    sort = Some((column.clone(), *direction));
                }
                QueryStep::RenameColumn { column, new_name } => {
                    if let Some(known) = schema
                        .iter_mut()
                        .find(|known| known.eq_ignore_ascii_case(column))
                    {
                        if sort
                            .as_ref()
                            .is_some_and(|(sorted, _)| sorted.eq_ignore_ascii_case(column))
                        {
                            if let Some((sorted, _)) = sort.as_mut() {
                                *sorted = new_name.clone();
                            }
                        }
                        *known = new_name.clone();
                    }
                }
                QueryStep::RemoveColumn { column } => {
                    schema.retain(|known| !known.eq_ignore_ascii_case(column));
                    if sort
                        .as_ref()
                        .is_some_and(|(sorted, _)| sorted.eq_ignore_ascii_case(column))
                    {
                        sort = None;
                    }
                }
                _ => {}
            }
        }
        sort
    });
    if let Some((column, direction)) = sort {
        if !output_columns
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&column))
        {
            return Err(QueryCompileError::UnknownColumn(column));
        }
        let direction = match direction {
            crate::data_doc::SortDirection::Ascending => "ASC",
            crate::data_doc::SortDirection::Descending => "DESC",
        };
        sql.push_str(&format!(
            " ORDER BY {} {direction} NULLS LAST",
            if projection_override.is_some() {
                format!("datan_preview.{}", quote(&column))
            } else {
                quote(&column)
            }
        ));
    }
    Ok(sql)
}

pub fn output_columns(
    columns: &[String],
    query: Option<&QueryDoc>,
) -> Result<Vec<String>, QueryCompileError> {
    let mut schema = query
        .map(|query| query.source_columns.as_slice())
        .filter(|columns| !columns.is_empty())
        .unwrap_or(columns)
        .to_vec();
    for step in query.into_iter().flat_map(|query| &query.steps) {
        match step {
            QueryStep::RenameColumn { column, new_name } => {
                let Some(index) = schema
                    .iter()
                    .position(|known| known.eq_ignore_ascii_case(column))
                else {
                    return Err(QueryCompileError::UnknownColumn(column.clone()));
                };
                if new_name.trim().is_empty() {
                    return Err(QueryCompileError::EmptyColumnName);
                }
                if schema.iter().enumerate().any(|(other, known)| {
                    other != index && known.eq_ignore_ascii_case(new_name.trim())
                }) {
                    return Err(QueryCompileError::DuplicateColumn(
                        new_name.trim().to_string(),
                    ));
                }
                schema[index] = new_name.trim().to_string();
            }
            QueryStep::RemoveColumn { column } => {
                let Some(index) = schema
                    .iter()
                    .position(|known| known.eq_ignore_ascii_case(column))
                else {
                    return Err(QueryCompileError::UnknownColumn(column.clone()));
                };
                if schema.len() <= 1 {
                    return Err(QueryCompileError::LastColumn);
                }
                schema.remove(index);
            }
            QueryStep::RemoveEmptyRows { column }
            | QueryStep::SortRows { column, .. }
            | QueryStep::ChangeType { column, .. } => {
                if !schema
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(column))
                {
                    return Err(QueryCompileError::UnknownColumn(column.clone()));
                }
            }
            QueryStep::RemoveDuplicates => {}
        }
    }
    Ok(schema)
}

pub fn install_query_views(
    engine: &Engine,
    model: &Model,
    queries: &DataDoc,
) -> Result<(), String> {
    engine
        .execute("CREATE SCHEMA IF NOT EXISTS datan_model")
        .map_err(|error| format!("No se pudo crear el esquema de consultas: {error}"))?;
    for table in &model.tables {
        let columns: Vec<String> = table
            .columns
            .iter()
            .filter(|column| !column.is_calculated())
            .map(|column| column.name.clone())
            .collect();
        let query = queries.query(&table.name);
        let source_columns = query
            .map(|query| query.source_columns.as_slice())
            .filter(|columns| !columns.is_empty())
            .unwrap_or(&columns);
        install_query_view(engine, &table.name, source_columns, query)?;
    }
    Ok(())
}

pub fn install_query_view(
    engine: &Engine,
    table: &str,
    columns: &[String],
    query: Option<&QueryDoc>,
) -> Result<(), String> {
    let sql = compile_view(table, columns, query)
        .map_err(|error| format!("No se pudo compilar la consulta de `{table}`: {error}"))?;
    engine
        .execute("CREATE SCHEMA IF NOT EXISTS datan_model")
        .map_err(|error| format!("No se pudo crear el esquema de consultas: {error}"))?;
    let view = format!(
        "CREATE OR REPLACE VIEW datan_model.{} AS {sql}",
        quote(table)
    );
    engine
        .execute(&view)
        .map_err(|error| format!("No se pudo aplicar los pasos de `{table}`: {error}"))
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_doc::QueryStep;

    #[test]
    fn acepta_select_y_cte_de_solo_lectura() {
        assert!(is_read_only_select("SELECT * FROM ventas"));
        assert!(is_read_only_select(
            "WITH resumen AS (SELECT region, SUM(importe) AS total FROM ventas GROUP BY region) SELECT * FROM resumen"
        ));
        assert!(is_read_only_select(
            "WITH RECURSIVE niveles(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM niveles WHERE n < 3) SELECT n FROM niveles;"
        ));
    }

    #[test]
    fn rechaza_escrituras_en_cte_y_sentencias_adicionales() {
        assert!(!is_read_only_select(
            "WITH borradas AS (DELETE FROM ventas RETURNING *) SELECT * FROM borradas"
        ));
        assert!(!is_read_only_select(
            "WITH actualizadas AS (UPDATE ventas SET importe = 0 RETURNING *) SELECT * FROM actualizadas"
        ));
        assert!(!is_read_only_select("SELECT 1; DELETE FROM ventas"));
        assert!(!is_read_only_select(
            "WITH datos AS (SELECT 1) DELETE FROM ventas"
        ));
    }

    #[test]
    fn ignora_palabras_y_punto_y_coma_dentro_de_literales_y_comentarios() {
        assert!(is_read_only_select(
            "-- delete from ventas;\nWITH datos AS (SELECT 'delete; from' AS texto) SELECT texto FROM datos; -- fin"
        ));
        assert!(!is_read_only_select("SELECT 1 /* comentario abierto"));
    }

    fn preview_rows(source_sql: &str, columns: &[&str], steps: Vec<QueryStep>) -> Vec<Vec<String>> {
        let engine = Engine::open(":memory:").expect("open DuckDB");
        let columns = columns
            .iter()
            .map(|column| (*column).to_string())
            .collect::<Vec<_>>();
        let query = QueryDoc {
            table: "ventas".to_string(),
            source_columns: columns.clone(),
            source_sql: Some(source_sql.to_string()),
            steps,
        };
        let sql =
            compile_preview_with_errors("ventas", &columns, Some(&query)).expect("compile preview");
        engine.render_rows(&sql).expect("read preview")
    }

    #[test]
    fn preview_distinguishes_conversion_errors_from_source_nulls() {
        let rows = preview_rows(
            "SELECT * FROM (VALUES (1, '12.5'), (2, 'invalid'), (3, NULL), (4, '')) AS source(id, amount)",
            &["id", "amount"],
            vec![
                QueryStep::ChangeType {
                    column: "amount".to_string(),
                    column_type: crate::data_doc::ColumnType::Number,
                },
                QueryStep::SortRows {
                    column: "id".to_string(),
                    direction: crate::data_doc::SortDirection::Ascending,
                },
            ],
        );

        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0][1].parse::<f64>(), Ok(12.5));
        assert_eq!(&rows[0][2..], &["false", "false"]);
        assert_eq!(
            &rows[1][1..],
            &[datan_storage::CELDA_EN_BLANCO, "false", "true"]
        );
        assert_eq!(
            &rows[2][1..],
            &[datan_storage::CELDA_EN_BLANCO, "false", "false"]
        );
        assert_eq!(
            &rows[3][1..],
            &[datan_storage::CELDA_EN_BLANCO, "false", "true"]
        );
    }

    #[test]
    fn preview_keeps_error_flags_aligned_after_rename_and_remove() {
        let rows = preview_rows(
            "SELECT 'invalid' AS discarded, 'invalid' AS amount, 'note' AS label",
            &["discarded", "amount", "label"],
            vec![
                QueryStep::ChangeType {
                    column: "discarded".to_string(),
                    column_type: crate::data_doc::ColumnType::Number,
                },
                QueryStep::ChangeType {
                    column: "amount".to_string(),
                    column_type: crate::data_doc::ColumnType::Number,
                },
                QueryStep::RenameColumn {
                    column: "amount".to_string(),
                    new_name: "total".to_string(),
                },
                QueryStep::RemoveColumn {
                    column: "discarded".to_string(),
                },
            ],
        );

        assert_eq!(
            rows,
            vec![vec![
                datan_storage::CELDA_EN_BLANCO,
                "note",
                "true",
                "false"
            ]]
        );
    }

    #[test]
    fn preview_merges_duplicate_error_flags_with_bool_or() {
        let rows = preview_rows(
            "SELECT * FROM (VALUES ('invalid'), (NULL), ('invalid')) AS source(amount)",
            &["amount"],
            vec![
                QueryStep::ChangeType {
                    column: "amount".to_string(),
                    column_type: crate::data_doc::ColumnType::Number,
                },
                QueryStep::RemoveDuplicates,
            ],
        );

        assert_eq!(rows, vec![vec![datan_storage::CELDA_EN_BLANCO, "true"]]);
    }

    #[test]
    fn preview_sorts_numeric_values_before_text_conversion() {
        let rows = preview_rows(
            "SELECT * FROM (VALUES (10), (2), (NULL)) AS source(amount)",
            &["amount"],
            vec![QueryStep::SortRows {
                column: "amount".to_string(),
                direction: crate::data_doc::SortDirection::Ascending,
            }],
        );

        assert_eq!(
            rows,
            vec![
                vec!["2", "false"],
                vec!["10", "false"],
                vec![datan_storage::CELDA_EN_BLANCO, "false"]
            ]
        );
    }

    #[test]
    fn preview_error_aliases_do_not_replace_source_or_renamed_columns() {
        let rows = preview_rows(
            "SELECT 'kept' AS \"__DATAN_CONVERSION_ERROR_0\", 'invalid' AS amount",
            &["__DATAN_CONVERSION_ERROR_0", "amount"],
            vec![
                QueryStep::ChangeType {
                    column: "amount".to_string(),
                    column_type: crate::data_doc::ColumnType::Number,
                },
                QueryStep::RenameColumn {
                    column: "amount".to_string(),
                    new_name: "__datan_conversion_error__1".to_string(),
                },
            ],
        );

        assert_eq!(
            rows,
            vec![vec![
                "kept",
                datan_storage::CELDA_EN_BLANCO,
                "false",
                "true"
            ]]
        );
    }

    #[test]
    fn consulta_sin_pasos_conserva_tabla_origen() {
        assert_eq!(
            compile_view("Ventas", &["Fecha".to_string()], None),
            Ok("SELECT * FROM pg.\"Ventas\"".to_string())
        );
    }

    #[test]
    fn quitar_vacios_quotea_nombres_y_filtra_null_y_texto_vacio() {
        let query = QueryDoc {
            table: "Ventas".to_string(),
            source_columns: Vec::new(),
            source_sql: None,
            steps: vec![QueryStep::RemoveEmptyRows {
                column: "Región \"Norte\"".to_string(),
            }],
        };

        assert_eq!(
            compile_view(
                "Ventas",
                &["Región \"Norte\"".to_string()],
                Some(&query)
            ),
            Ok("SELECT * FROM (SELECT * FROM pg.\"Ventas\") AS step_0 WHERE NULLIF(TRIM(CAST(\"Región \"\"Norte\"\"\" AS VARCHAR)), '') IS NOT NULL".to_string())
        );
    }

    #[test]
    fn rechaza_una_columna_que_no_existe() {
        let query = QueryDoc {
            table: "Ventas".to_string(),
            source_columns: Vec::new(),
            source_sql: None,
            steps: vec![QueryStep::RemoveEmptyRows {
                column: "No existe".to_string(),
            }],
        };

        assert_eq!(
            compile_view("Ventas", &["Fecha".to_string()], Some(&query)),
            Err(QueryCompileError::UnknownColumn("No existe".to_string()))
        );
    }

    #[test]
    fn ordena_por_columna_con_direccion_y_nulos_al_final() {
        let query = QueryDoc {
            table: "Ventas".to_string(),
            source_columns: Vec::new(),
            source_sql: None,
            steps: vec![QueryStep::SortRows {
                column: "Fecha".to_string(),
                direction: crate::data_doc::SortDirection::Descending,
            }],
        };

        assert_eq!(
            compile_view("Ventas", &["Fecha".to_string()], Some(&query)),
            Ok("SELECT * FROM (SELECT * FROM pg.\"Ventas\") AS step_0 ORDER BY \"Fecha\" DESC NULLS LAST".to_string())
        );
    }

    #[test]
    fn consulta_de_vista_previa_aplica_el_orden_en_el_select_final() {
        let query = QueryDoc {
            table: "Ventas".to_string(),
            source_columns: Vec::new(),
            source_sql: None,
            steps: vec![QueryStep::SortRows {
                column: "Fecha".to_string(),
                direction: crate::data_doc::SortDirection::Descending,
            }],
        };

        assert_eq!(
            compile_preview_query("Ventas", &["Fecha".to_string()], Some(&query)),
            Ok(
                "SELECT \"Fecha\" FROM datan_model.\"Ventas\" ORDER BY \"Fecha\" DESC NULLS LAST"
                    .to_string()
            )
        );
    }

    #[test]
    fn cambiar_tipo_quotea_y_convierte_la_columna_seleccionada() {
        let query = QueryDoc {
            table: "Ventas".to_string(),
            source_columns: Vec::new(),
            source_sql: None,
            steps: vec![QueryStep::ChangeType {
                column: "Importe".to_string(),
                column_type: crate::data_doc::ColumnType::Number,
            }],
        };

        assert_eq!(
            compile_view("Ventas", &["Importe".to_string()], Some(&query)),
            Ok("SELECT TRY_CAST(\"Importe\" AS DOUBLE) AS \"Importe\" FROM (SELECT * FROM pg.\"Ventas\") AS step_0".to_string())
        );
    }

    #[test]
    fn renombra_quita_y_cambia_tipo_segun_el_esquema_de_cada_paso() {
        let columns = vec![
            "importe bruto".to_string(),
            "region".to_string(),
            "nota".to_string(),
        ];
        let query = QueryDoc {
            table: "ventas".to_string(),
            source_columns: columns.clone(),
            source_sql: None,
            steps: vec![
                QueryStep::RenameColumn {
                    column: "importe bruto".to_string(),
                    new_name: "importe".to_string(),
                },
                QueryStep::ChangeType {
                    column: "importe".to_string(),
                    column_type: crate::data_doc::ColumnType::Number,
                },
                QueryStep::RemoveColumn {
                    column: "region".to_string(),
                },
            ],
        };

        assert_eq!(
            compile_view("ventas", &columns, Some(&query)),
            Ok("SELECT \"importe\", \"nota\" FROM (SELECT TRY_CAST(\"importe\" AS DOUBLE) AS \"importe\", \"region\", \"nota\" FROM (SELECT \"importe bruto\" AS \"importe\", \"region\", \"nota\" FROM (SELECT * FROM pg.\"ventas\") AS step_0) AS step_1) AS step_2".to_string())
        );
        assert_eq!(
            output_columns(&columns, Some(&query)),
            Ok(vec!["importe".to_string(), "nota".to_string()])
        );
        assert_eq!(
            compile_preview_query("ventas", &columns, Some(&query)),
            Ok("SELECT \"importe\", \"nota\" FROM datan_model.\"ventas\"".to_string())
        );
    }

    #[test]
    fn vista_aplicada_expone_nombres_finales_y_columnas_restantes() {
        let engine = Engine::open(":memory:").expect("abre DuckDB");
        engine
            .execute(
                "CREATE SCHEMA pg; CREATE TABLE pg.ventas (importe_bruto VARCHAR, region VARCHAR); \
             INSERT INTO pg.ventas VALUES ('12.5', 'Norte')",
            )
            .expect("crea fuente");
        let columns = vec!["importe_bruto".to_string(), "region".to_string()];
        let query = QueryDoc {
            table: "ventas".to_string(),
            source_columns: columns.clone(),
            source_sql: None,
            steps: vec![
                QueryStep::RenameColumn {
                    column: "importe_bruto".to_string(),
                    new_name: "importe".to_string(),
                },
                QueryStep::ChangeType {
                    column: "importe".to_string(),
                    column_type: crate::data_doc::ColumnType::Number,
                },
                QueryStep::RemoveColumn {
                    column: "region".to_string(),
                },
            ],
        };
        install_query_view(&engine, "ventas", &columns, Some(&query)).expect("crea vista");
        let rows = engine
            .render_rows("SELECT importe FROM datan_model.ventas")
            .expect("lee vista");
        assert_eq!(rows, vec![vec!["12.500000"]]);
    }

    #[test]
    fn vista_duckdb_aplica_el_paso_a_las_filas() {
        let engine = Engine::open(":memory:").expect("abre motor DuckDB");
        engine
            .execute(
                "CREATE SCHEMA pg; \
                 CREATE TABLE pg.ventas (region VARCHAR); \
                 INSERT INTO pg.ventas VALUES ('Norte'), (''), (NULL), ('Sur')",
            )
            .expect("crea tabla de origen");
        let columns = vec!["region".to_string()];
        let query = QueryDoc {
            table: "ventas".to_string(),
            source_columns: columns.clone(),
            source_sql: None,
            steps: vec![QueryStep::RemoveEmptyRows {
                column: "region".to_string(),
            }],
        };

        install_query_view(&engine, "ventas", &columns, Some(&query))
            .expect("crea vista transformada");
        let rows = engine
            .render_rows("SELECT region FROM datan_model.ventas ORDER BY region")
            .expect("lee vista");

        assert_eq!(rows, vec![vec!["Norte"], vec!["Sur"]]);
    }

    #[test]
    fn vista_duckdb_quita_filas_duplicadas_completas() {
        let engine = Engine::open(":memory:").expect("abre motor DuckDB");
        engine
            .execute(
                "CREATE SCHEMA pg; CREATE TABLE pg.ventas (region VARCHAR, cantidad INTEGER); \
                 INSERT INTO pg.ventas VALUES ('Norte', 2), ('Norte', 2), ('Sur', 1)",
            )
            .expect("crea tabla de origen");
        let query = QueryDoc {
            table: "ventas".to_string(),
            source_columns: vec!["region".to_string(), "cantidad".to_string()],
            source_sql: None,
            steps: vec![QueryStep::RemoveDuplicates],
        };

        install_query_view(
            &engine,
            "ventas",
            &["region".to_string(), "cantidad".to_string()],
            Some(&query),
        )
        .expect("crea vista sin duplicados");
        let rows = engine
            .render_rows("SELECT region, cantidad FROM datan_model.ventas ORDER BY region")
            .expect("lee vista");

        assert_eq!(rows, vec![vec!["Norte", "2"], vec!["Sur", "1"]]);
    }

    #[test]
    fn consulta_de_vista_previa_devuelve_filas_en_el_orden_elegido() {
        let engine = Engine::open(":memory:").expect("abre motor DuckDB");
        engine
            .execute(
                "CREATE SCHEMA pg; CREATE TABLE pg.ventas (fecha INTEGER); \
                 INSERT INTO pg.ventas VALUES (2), (1), (3)",
            )
            .expect("crea tabla de origen");
        let columns = vec!["fecha".to_string()];
        let query = QueryDoc {
            table: "ventas".to_string(),
            source_columns: columns.clone(),
            source_sql: None,
            steps: vec![QueryStep::SortRows {
                column: "fecha".to_string(),
                direction: crate::data_doc::SortDirection::Descending,
            }],
        };
        install_query_view(&engine, "ventas", &columns, Some(&query))
            .expect("instala la vista transformada");
        let sql = compile_preview_query("ventas", &columns, Some(&query))
            .expect("compila la consulta de vista previa");
        let rows = engine.render_rows(&sql).expect("lee filas ordenadas");

        assert_eq!(rows, vec![vec!["3"], vec!["2"], vec!["1"]]);
    }

    #[test]
    fn vista_duckdb_convierte_tipo_y_deja_nulos_los_valores_invalidos() {
        let engine = Engine::open(":memory:").expect("abre motor DuckDB");
        engine
            .execute(
                "CREATE SCHEMA pg; CREATE TABLE pg.ventas (importe VARCHAR); \
                 INSERT INTO pg.ventas VALUES ('12.5'), ('incorrecto')",
            )
            .expect("crea tabla de origen");
        let columns = vec!["importe".to_string()];
        let query = QueryDoc {
            table: "ventas".to_string(),
            source_columns: columns.clone(),
            source_sql: None,
            steps: vec![QueryStep::ChangeType {
                column: "importe".to_string(),
                column_type: crate::data_doc::ColumnType::Number,
            }],
        };
        install_query_view(&engine, "ventas", &columns, Some(&query))
            .expect("instala la vista convertida");
        let rows = engine
            .render_rows("SELECT importe FROM datan_model.ventas ORDER BY importe NULLS LAST")
            .expect("lee importes convertidos");

        assert_eq!(
            rows,
            vec![vec!["12.500000"], vec![datan_storage::CELDA_EN_BLANCO]]
        );
    }
}
