use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct File {
    query: Vec<QueryDoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    loaded_query: Vec<LoadedQueryDoc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataDoc {
    pub queries: Vec<QueryDoc>,
    pub loaded_queries: Vec<LoadedQueryDoc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadedQueryDoc {
    pub table: String,
    pub source_sql: String,
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueryDoc {
    pub table: String,
    pub steps: Vec<QueryStep>,
    /// Esquema físico antes de los pasos. Permite reconstruir la vista tras renombrar o quitar
    /// campos del modelo lógico.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum QueryStep {
    RemoveEmptyRows {
        column: String,
    },
    SortRows {
        column: String,
        direction: SortDirection,
    },
    RemoveDuplicates,
    ChangeType {
        column: String,
        column_type: ColumnType,
    },
    RenameColumn {
        column: String,
        new_name: String,
    },
    RemoveColumn {
        column: String,
    },
}

pub fn move_query_step(steps: &mut [QueryStep], from: usize, to: usize) -> bool {
    if from >= steps.len() || to >= steps.len() || from == to {
        return false;
    }
    steps.swap(from, to);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Number,
    Text,
    Date,
}

impl ColumnType {
    pub fn model_type(self) -> datan_model::DataType {
        match self {
            Self::Number => datan_model::DataType::Decimal,
            Self::Text => datan_model::DataType::Text,
            Self::Date => datan_model::DataType::Date,
        }
    }

    pub fn sql_type(self) -> &'static str {
        match self {
            Self::Number => "DOUBLE",
            Self::Text => "VARCHAR",
            Self::Date => "DATE",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DataDocError {
    #[error("no se pudo leer {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no se pudo escribir {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} no es un TOML válido: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl DataDoc {
    pub fn source_column_types(
        model: &datan_model::Model,
    ) -> std::collections::BTreeMap<datan_model::ObjectId, datan_model::DataType> {
        model
            .tables
            .iter()
            .flat_map(|table| &table.columns)
            .filter(|column| !column.is_calculated())
            .map(|column| (column.id, column.data_type))
            .collect()
    }

    pub fn path(database: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let filename = database
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace('\\', "%5C");
        PathBuf::from(home)
            .join(".datan")
            .join("queries")
            .join(format!("{filename}.toml"))
    }

    pub fn load(database: &str) -> Result<Self, DataDocError> {
        Self::load_from(&Self::path(database))
    }

    pub fn load_from(path: &Path) -> Result<Self, DataDocError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(DataDocError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let file: File = toml::from_str(&text).map_err(|source| DataDocError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            queries: file.query,
            loaded_queries: file.loaded_query,
        })
    }

    pub fn save(&self, database: &str) -> Result<(), DataDocError> {
        let path = Self::path(database);
        if let Some(parent) = path.parent()
            && let Err(source) = std::fs::create_dir_all(parent)
        {
            return Err(DataDocError::Write {
                path: parent.to_path_buf(),
                source,
            });
        }
        self.save_to(&path)
    }

    pub fn query(&self, table: &str) -> Option<&QueryDoc> {
        self.queries
            .iter()
            .find(|query| query.table.eq_ignore_ascii_case(table))
    }

    pub fn set_query(&mut self, query: QueryDoc) {
        if let Some(existing) = self
            .queries
            .iter_mut()
            .find(|existing| existing.table.eq_ignore_ascii_case(&query.table))
        {
            *existing = query;
        } else {
            self.queries.push(query);
        }
    }

    pub fn set_loaded_query(&mut self, query: LoadedQueryDoc) {
        if let Some(existing) = self
            .loaded_queries
            .iter_mut()
            .find(|existing| existing.table.eq_ignore_ascii_case(&query.table))
        {
            *existing = query;
        } else {
            self.loaded_queries.push(query);
        }
    }

    pub fn validate_source_schema(&self, model: &datan_model::Model) -> Result<(), String> {
        for query in &self.queries {
            if query.source_columns.is_empty() {
                continue;
            }
            let Some(table) = model.table_by_name(&query.table) else {
                return Err(format!(
                    "La tabla `{}` ya no existe en la fuente.",
                    query.table
                ));
            };
            let source_columns: Vec<&str> = table
                .columns
                .iter()
                .filter(|column| !column.is_calculated())
                .map(|column| column.name.as_str())
                .collect();
            let saved_columns: Vec<&str> =
                query.source_columns.iter().map(String::as_str).collect();
            if !same_names(&source_columns, &saved_columns) {
                return Err(format!(
                    "Cambió el esquema de `{}`. Actualiza o quita sus pasos antes de usar esta consulta.",
                    query.table
                ));
            }
        }
        Ok(())
    }

    pub fn apply_column_types(&self, model: &mut datan_model::Model) {
        for query in &self.queries {
            let Some(table) = model.table_by_name(&query.table) else {
                continue;
            };
            let table_id = table.id;
            for step in &query.steps {
                let QueryStep::ChangeType {
                    column,
                    column_type,
                } = step
                else {
                    continue;
                };
                if let Some(table) = model.tables.iter_mut().find(|table| table.id == table_id)
                    && let Some(column) = table
                        .columns
                        .iter_mut()
                        .find(|known| known.name.eq_ignore_ascii_case(column))
                    && !column.is_calculated()
                {
                    column.data_type = column_type.model_type();
                }
            }
        }
    }

    pub fn apply_column_types_from_source(
        &self,
        model: &mut datan_model::Model,
        source_types: &std::collections::BTreeMap<datan_model::ObjectId, datan_model::DataType>,
    ) -> Result<(), String> {
        for table in &mut model.tables {
            for column in &mut table.columns {
                if let Some(data_type) = source_types.get(&column.id)
                    && !column.is_calculated()
                {
                    column.data_type = *data_type;
                }
            }
        }
        self.apply_steps_to_model(model)
    }

    pub fn apply_steps_to_model(&self, model: &mut datan_model::Model) -> Result<(), String> {
        for query in &self.queries {
            let Some(table_id) = model.table_by_name(&query.table).map(|table| table.id) else {
                continue;
            };
            for step in &query.steps {
                let Some(table) = model.table(table_id) else {
                    continue;
                };
                match step {
                    QueryStep::ChangeType {
                        column,
                        column_type,
                    } => {
                        let Some(id) = table.column_by_name(column).map(|column| column.id) else {
                            return Err(format!(
                                "La columna `{column}` no existe en `{}`.",
                                table.name
                            ));
                        };
                        if let Some(column) = model
                            .tables
                            .iter_mut()
                            .find(|table| table.id == table_id)
                            .and_then(|table| {
                                table.columns.iter_mut().find(|column| column.id == id)
                            })
                        {
                            column.data_type = column_type.model_type();
                        }
                    }
                    QueryStep::RenameColumn { column, new_name } => {
                        let Some(target) = table.column_by_name(column) else {
                            return Err(format!(
                                "La columna `{column}` no existe en `{}`.",
                                table.name
                            ));
                        };
                        let id = target.id;
                        if new_name.trim().is_empty() {
                            return Err("El nombre de columna no puede estar vacío.".to_string());
                        }
                        if table.columns.iter().any(|known| {
                            known.id != id
                                && (known.name.eq_ignore_ascii_case(new_name)
                                    || known
                                        .aliases
                                        .iter()
                                        .any(|alias| alias.eq_ignore_ascii_case(new_name)))
                        }) {
                            return Err(format!(
                                "Ya existe la columna `{new_name}` en `{}`.",
                                table.name
                            ));
                        }
                        let table = model
                            .tables
                            .iter_mut()
                            .find(|table| table.id == table_id)
                            .expect("tabla encontrada");
                        let target = table
                            .columns
                            .iter_mut()
                            .find(|known| known.id == id)
                            .expect("columna encontrada");
                        if !target
                            .aliases
                            .iter()
                            .any(|alias| alias.eq_ignore_ascii_case(&target.name))
                        {
                            target.aliases.push(target.name.clone());
                        }
                        target.name = new_name.trim().to_string();
                    }
                    QueryStep::RemoveColumn { column } => {
                        let Some(target) = table.column_by_name(column) else {
                            return Err(format!(
                                "La columna `{column}` no existe en `{}`.",
                                table.name
                            ));
                        };
                        let id = target.id;
                        if table.columns.len() <= 1 {
                            return Err(
                                "No se puede quitar la última columna de una tabla.".to_string()
                            );
                        }
                        if table.key_columns.contains(&id)
                            || table.columns.iter().any(|known| known.sort_by == Some(id))
                            || model.relationships.iter().any(|relationship| {
                                relationship.from_column == id || relationship.to_column == id
                            })
                        {
                            return Err(format!(
                                "No se puede quitar `{column}` porque define una clave, orden o relación."
                            ));
                        }
                        if table
                            .columns
                            .iter()
                            .filter(|known| known.id != id)
                            .filter_map(|known| known.expression())
                            .any(|expression| {
                                datan_dax_check::check_row(expression, model, table_id).is_ok_and(
                                    |checked| checked.expr.referenced_columns().contains(&id),
                                )
                            })
                        {
                            return Err(format!(
                                "No se puede quitar `{column}` porque otra columna calculada la usa."
                            ));
                        }
                        if model.measures.iter().any(|measure| {
                            datan_dax_check::check(&measure.expression, model).is_ok_and(
                                |checked| checked.expr.referenced_columns().contains(&id),
                            )
                        }) {
                            return Err(format!(
                                "No se puede quitar `{column}` porque una medida la usa."
                            ));
                        }
                        model
                            .tables
                            .iter_mut()
                            .find(|table| table.id == table_id)
                            .expect("tabla encontrada")
                            .columns
                            .retain(|known| known.id != id);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

fn same_names(left: &[&str], right: &[&str]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mover_paso_cambia_el_orden_y_rechaza_indices_invalidos() {
        let mut steps = vec![
            QueryStep::RemoveDuplicates,
            QueryStep::RemoveEmptyRows {
                column: "Importe".to_string(),
            },
        ];

        assert!(move_query_step(&mut steps, 1, 0));
        assert_eq!(
            steps[0],
            QueryStep::RemoveEmptyRows {
                column: "Importe".to_string(),
            }
        );
        assert!(!move_query_step(&mut steps, 0, 2));
        assert!(!move_query_step(&mut steps, 1, 1));
    }

    #[test]
    fn nombre_de_base_no_puede_salir_del_directorio_queries() {
        let path = DataDoc::path("../../outside\\file");
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let expected_parent = PathBuf::from(home).join(".datan").join("queries");

        assert_eq!(path.parent(), Some(expected_parent.as_path()));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("..%2F..%2Foutside%5Cfile.toml")
        );
    }

    #[test]
    fn aplica_el_ultimo_tipo_guardado_a_la_columna_del_modelo() {
        let mut model = datan_model::Model {
            tables: vec![datan_model::Table {
                id: datan_model::ObjectId(1),
                name: "Ventas".to_string(),
                kind: datan_model::TableKind::Facts,
                columns: vec![datan_model::Column::new(
                    datan_model::ObjectId(2),
                    "Importe",
                    datan_model::DataType::Text,
                )],
                key_columns: Vec::new(),
                is_date_table: false,
            }],
            ..datan_model::Model::default()
        };
        let document = DataDoc {
            queries: vec![QueryDoc {
                table: "ventas".to_string(),
                source_columns: Vec::new(),
                source_sql: None,
                steps: vec![
                    QueryStep::ChangeType {
                        column: "importe".to_string(),
                        column_type: ColumnType::Number,
                    },
                    QueryStep::ChangeType {
                        column: "IMPORTE".to_string(),
                        column_type: ColumnType::Date,
                    },
                ],
            }],
            ..DataDoc::default()
        };

        document.apply_column_types(&mut model);

        assert_eq!(
            model.tables[0].columns[0].data_type,
            datan_model::DataType::Date
        );
    }

    #[test]
    fn al_quitar_el_paso_de_tipo_restablece_el_tipo_de_origen() {
        let mut model = datan_model::Model {
            tables: vec![datan_model::Table {
                id: datan_model::ObjectId(1),
                name: "Ventas".to_string(),
                kind: datan_model::TableKind::Facts,
                columns: vec![datan_model::Column::new(
                    datan_model::ObjectId(2),
                    "Importe",
                    datan_model::DataType::Text,
                )],
                key_columns: Vec::new(),
                is_date_table: false,
            }],
            ..datan_model::Model::default()
        };
        let source_types = DataDoc::source_column_types(&model);
        DataDoc {
            queries: vec![QueryDoc {
                table: "Ventas".to_string(),
                source_columns: Vec::new(),
                source_sql: None,
                steps: vec![QueryStep::ChangeType {
                    column: "Importe".to_string(),
                    column_type: ColumnType::Number,
                }],
            }],
            ..DataDoc::default()
        }
        .apply_column_types_from_source(&mut model, &source_types)
        .expect("aplica tipo guardado");
        DataDoc::default()
            .apply_column_types_from_source(&mut model, &source_types)
            .expect("restaura el tipo de origen");

        assert_eq!(
            model.tables[0].columns[0].data_type,
            datan_model::DataType::Text
        );
    }

    #[test]
    fn renombrar_conserva_id_y_alias_y_aplica_el_tipo_en_el_nombre_nuevo() {
        let mut model = datan_model::Model {
            tables: vec![datan_model::Table {
                id: datan_model::ObjectId(1),
                name: "Ventas".to_string(),
                kind: datan_model::TableKind::Facts,
                columns: vec![
                    datan_model::Column::new(
                        datan_model::ObjectId(2),
                        "Importe bruto",
                        datan_model::DataType::Text,
                    ),
                    datan_model::Column::new(
                        datan_model::ObjectId(3),
                        "Region",
                        datan_model::DataType::Text,
                    ),
                ],
                key_columns: Vec::new(),
                is_date_table: false,
            }],
            ..datan_model::Model::default()
        };
        let source_types = DataDoc::source_column_types(&model);
        let document = DataDoc {
            queries: vec![QueryDoc {
                table: "Ventas".to_string(),
                source_columns: vec!["Importe bruto".to_string(), "Region".to_string()],
                source_sql: None,
                steps: vec![
                    QueryStep::RenameColumn {
                        column: "Importe bruto".to_string(),
                        new_name: "Importe".to_string(),
                    },
                    QueryStep::ChangeType {
                        column: "Importe".to_string(),
                        column_type: ColumnType::Number,
                    },
                    QueryStep::RemoveColumn {
                        column: "Region".to_string(),
                    },
                ],
            }],
            ..DataDoc::default()
        };
        document
            .apply_column_types_from_source(&mut model, &source_types)
            .expect("aplica pasos de modelo");
        let table = model.table_by_name("Ventas").unwrap();
        let importe = table.column_by_name("Importe").unwrap();
        assert_eq!(importe.id, datan_model::ObjectId(2));
        assert_eq!(importe.data_type, datan_model::DataType::Decimal);
        assert_eq!(
            table.column_by_name("Importe bruto").unwrap().id,
            importe.id
        );
        assert!(table.column_by_name("Region").is_none());
    }

    #[test]
    fn no_quita_una_columna_usada_por_una_medida() {
        let mut model = datan_model::Model {
            tables: vec![datan_model::Table {
                id: datan_model::ObjectId(1),
                name: "Ventas".to_string(),
                kind: datan_model::TableKind::Facts,
                columns: vec![
                    datan_model::Column::new(
                        datan_model::ObjectId(2),
                        "Importe",
                        datan_model::DataType::Decimal,
                    ),
                    datan_model::Column::new(
                        datan_model::ObjectId(3),
                        "Region",
                        datan_model::DataType::Text,
                    ),
                ],
                key_columns: Vec::new(),
                is_date_table: false,
            }],
            measures: vec![datan_model::Measure::new(
                datan_model::ObjectId(4),
                "Ingresos",
                "SUM('Ventas'[Importe])",
            )],
            ..datan_model::Model::default()
        };
        let document = DataDoc {
            queries: vec![QueryDoc {
                table: "Ventas".to_string(),
                source_columns: vec!["Importe".to_string(), "Region".to_string()],
                source_sql: None,
                steps: vec![QueryStep::RemoveColumn {
                    column: "Importe".to_string(),
                }],
            }],
            ..DataDoc::default()
        };

        assert!(document.apply_steps_to_model(&mut model).is_err());
        assert!(model.column_by_name("Ventas", "Importe").is_some());
    }

    #[test]
    fn consulta_y_pasos_hacen_round_trip_toml() {
        let path = ruta_temporal("round-trip");
        let doc = DataDoc {
            queries: vec![QueryDoc {
                table: "Ventas".to_string(),
                source_columns: Vec::new(),
                source_sql: None,
                steps: vec![QueryStep::RemoveEmptyRows {
                    column: "Región".to_string(),
                }],
            }],
            ..DataDoc::default()
        };

        doc.save_to(&path).expect("guarda TOML");
        assert_eq!(DataDoc::load_from(&path).expect("carga TOML"), doc);
        std::fs::remove_file(path).expect("limpia archivo temporal");
    }

    #[test]
    fn consulta_antigua_o_vacia_carga_sin_migracion() {
        let path = ruta_temporal("empty");
        std::fs::write(&path, "").expect("escribe TOML vacío");

        assert_eq!(
            DataDoc::load_from(&path).expect("carga TOML vacío"),
            DataDoc::default()
        );
        std::fs::remove_file(path).expect("limpia archivo temporal");
    }

    fn ruta_temporal(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "datan-data-doc-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }
}

impl DataDoc {
    fn save_to(&self, path: &Path) -> Result<(), DataDocError> {
        let text = toml::to_string_pretty(&File {
            query: self.queries.clone(),
            loaded_query: self.loaded_queries.clone(),
        })
        .map_err(|error| DataDocError::Write {
            path: path.to_path_buf(),
            source: std::io::Error::other(error.to_string()),
        })?;
        std::fs::write(path, text).map_err(|source| DataDocError::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}
