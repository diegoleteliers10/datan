//! Guardar y abrir informes.
//!
//! # Que es un informe guardado
//!
//! La definicion, no el resultado. Dimensiones, medidas, filtros y conexion. Al abrir se
//! vuelve a correr contra la base: los numeros son los de AHORA.
//!
//! Guardar el resultado en vez de la definicion produceria un archivo que se ve bien y
//! miente: el informe de ayer con los numeros de ayer, presentados como si fueran de hoy.
//! Es exactamente el fallo que un producto de datos no puede tener.
//!
//! # Formato
//!
//! TOML en `~/.datan/reports/`. Un archivo por informe, con el nombre del informe como
//! nombre de archivo: se pueden compartir copiando un archivo, y se pueden borrar a mano.
//!
//! Los ids de columnas y medidas son internos del modelo y cambian entre ejecuciones. Lo que
//! se guarda son los NOMBRES, y al abrir se resuelven contra el modelo de esa conexion.

use std::path::PathBuf;

use datan_dax_eval::FilterValue;
use datan_model::{DataType, ObjectId};
use serde::{Deserialize, Serialize};

use crate::Report;

/// Un error de informe guardado.
#[derive(Debug, thiserror::Error)]
pub enum SavedError {
    #[error("no se pudo escribir {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no se pudo leer {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} no es un informe valido: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("la dimension `{name}` no existe en la base `{database}`")]
    UnknownDimension { name: String, database: String },
    #[error("la medida `{name}` no existe en la base `{database}`")]
    UnknownMeasure { name: String, database: String },
}

/// El archivo de un informe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct File {
    name: String,
    database: String,
    dimensions: Vec<String>,
    measures: Vec<String>,
    filters: Vec<SavedFilter>,
}

/// Un filtro guardado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedFilter {
    column: String,
    value: String,
}

/// El nombre de archivo de un informe.
///
/// Se usa el nombre del informe tal cual. Un nombre con `/` o con `..` no se puede crear:
/// se rechaza al guardar en vez de crear un archivo fuera de la carpeta.
fn file_name(name: &str) -> Result<String, SavedError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        return Err(SavedError::UnknownDimension {
            name: name.to_string(),
            database: String::new(),
        });
    }
    Ok(format!("{name}.toml"))
}

/// La carpeta donde viven los informes.
pub fn folder() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".datan").join("reports")
}

/// Guarda un informe.
pub fn save(report: &Report, name: &str) -> Result<(), SavedError> {
    let path = folder().join(file_name(name)?);

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return Err(SavedError::Write {
            path: parent.to_path_buf(),
            source: error,
        });
    }

    let dimensions = report
        .dimensions
        .iter()
        .filter_map(|id| report.model.column(*id).map(|(_, c)| c.name.clone()))
        .collect();
    let measures = report
        .measures
        .iter()
        .filter_map(|id| report.measure_name(*id).map(str::to_string))
        .collect();
    let filters = report
        .filters
        .entries()
        .flat_map(|(column, filters)| {
            filters.iter().filter_map(move |filter| {
                filter.values().first().map(|value| SavedFilter {
                    column: report
                        .model
                        .column(*column)
                        .map(|(_, c)| c.name.clone())
                        .unwrap_or_default(),
                    value: format!("{value:?}"),
                })
            })
        })
        .collect();

    let file = File {
        name: name.to_string(),
        database: report.qualifier.clone(),
        dimensions,
        measures,
        filters,
    };

    let text = toml::to_string_pretty(&file)
        .map_err(|_| SavedError::UnknownDimension {
            name: name.to_string(),
            database: String::new(),
        })?;

    std::fs::write(&path, text).map_err(|source| SavedError::Write { path, source })
}

/// Los nombres de los informes guardados, ordenados.
pub fn list() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.path().extension()?.to_str()?.to_string().eq("toml").then(|| {
            entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        }))
        .flatten()
        .collect();
    names.sort();
    names
}

/// Abre un informe contra un modelo.
///
/// Los nombres se resuelven contra el modelo de la conexion: los ids son internos y cambian
/// entre ejecuciones. Guardar ids seria guardar numeros que no significan nada mañana.
pub fn open(name: &str, model: &datan_model::Model) -> Result<Report, SavedError> {
    let path = folder().join(file_name(name)?);
    let text = std::fs::read_to_string(&path).map_err(|source| SavedError::Read {
        path,
        source,
    })?;
    let file: File = toml::from_str(&text).map_err(|source| SavedError::Parse {
        path: folder().join(format!("{name}.toml")),
        source,
    })?;

    let database = file.database.clone();
    let dimensions = file
        .dimensions
        .iter()
        .map(|dimension| {
            find_column(model, dimension).ok_or_else(|| SavedError::UnknownDimension {
                name: dimension.clone(),
                database: database.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let measures = file
        .measures
        .iter()
        .map(|measure| {
            model
                .measures
                .iter()
                .find(|m| m.name == *measure)
                .map(|m| m.id)
                .ok_or_else(|| SavedError::UnknownMeasure {
                    name: measure.clone(),
                    database: database.clone(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut report = Report::new(model.clone())
        .attached_to(&database)
        .with_dimensions(dimensions)
        .with_measures(measures);

    for filter in &file.filters {
        let Some(column) = find_column(&model, &filter.column) else {
            continue;
        };
        let data_type = model.column(column).map(|(_, c)| c.data_type);
        let value = parse_value(&filter.value, data_type);
        report = report.filtered_by(column, value);
    }

    Ok(report)
}

/// La columna por nombre, buscando en todas las tablas.
fn find_column(model: &datan_model::Model, name: &str) -> Option<ObjectId> {
    model
        .tables
        .iter()
        .find_map(|table| table.column_by_name(name).map(|c| c.id))
}

/// El texto de un filtro al valor que era.
///
/// Se guarda con `{value:?}` por rapidez, y se lee con la regla del tipo de la columna. Un
/// numero guardado como texto se parsea; un texto se queda igual. Guardar el tipo al lado
/// seria mas limpio y mas codigo para un caso que hoy no existe.
fn parse_value(text: &str, data_type: Option<DataType>) -> FilterValue {
    match data_type {
        Some(DataType::Integer | DataType::Moneda) => {
            FilterValue::Integer(text.parse().unwrap_or(0))
        }
        Some(DataType::Decimal) => FilterValue::Decimal(text.parse().unwrap_or(0.0)),
        Some(DataType::Boolean) => FilterValue::Boolean(text == "true"),
        _ => {
            let trimmed = text
                .strip_prefix("Text(\"")
                .and_then(|rest| rest.strip_suffix("\")"))
                .unwrap_or(text);
            FilterValue::Text(trimmed.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_nombre_con_barra_o_puntos_se_rechaza() {
        // Es la unica barrera entre un nombre de informe y una ruta fuera de la carpeta.
        // `../secrets` no debe poder escribirse.
        assert!(file_name("ventas 2026").is_ok());
        assert!(file_name("ventas/2026").is_err());
        assert!(file_name("ventas\\2026").is_err());
        assert!(file_name("..").is_err());
        assert!(file_name(".oculto").is_err());
        assert!(file_name("").is_err());
    }

    #[test]
    fn el_nombre_de_archivo_lleva_la_extension() {
        assert_eq!(file_name("ventas").unwrap(), "ventas.toml");
    }

    #[test]
    fn el_texto_de_un_filtro_vuelve_a_ser_texto() {
        // `FilterValue::Text("Norte")` se guarda como `Text("Norte")` por el Debug. Sin
        // quitar el envoltorio, el filtro buscaria la region `Text("Norte")` y la matriz
        // quedaria vacia: el informe abre sin error y sin datos.
        let value = parse_value("Text(\"Norte\")", Some(DataType::Text));
        assert_eq!(value, FilterValue::Text("Norte".to_string()));
    }

    #[test]
    fn un_texto_sin_envoltorio_tambien_se_lee() {
        let value = parse_value("Norte", Some(DataType::Text));
        assert_eq!(value, FilterValue::Text("Norte".to_string()));
    }

    #[test]
    fn un_numero_se_parsea_al_tipo_de_su_columna() {
        assert_eq!(
            parse_value("60", Some(DataType::Integer)),
            FilterValue::Integer(60)
        );
        assert_eq!(
            parse_value("60.5", Some(DataType::Decimal)),
            FilterValue::Decimal(60.5)
        );
    }

    #[test]
    fn un_booleano_se_lee_como_booleano() {
        assert_eq!(
            parse_value("true", Some(DataType::Boolean)),
            FilterValue::Boolean(true)
        );
        assert_eq!(
            parse_value("false", Some(DataType::Boolean)),
            FilterValue::Boolean(false)
        );
    }
}