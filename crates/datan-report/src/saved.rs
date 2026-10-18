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
    #[serde(default)]
    filters: Vec<SavedFilter>,
    /// El visual del lienzo al guardar. Un informe guardado en "Anillo" y reabierto en
    /// "Tabla" es otro informe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    visual: Option<String>,
    /// El periodo activo en meses hacia atras, si lo habia.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    period_months: Option<u8>,
}

/// Un filtro guardado.
///
/// `values` es un conjunto porque un slicer elige varios. Los informes guardados antes de
/// esto tienen `value = "..."` con un solo valor, y siguen abriendo: el alias lo lee como un
/// conjunto de uno.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedFilter {
    column: String,
    #[serde(alias = "value", default, deserialize_with = "uno_o_varios")]
    values: Vec<String>,
}

/// Lee un valor suelto o una lista, para que un informe viejo siga abriendo.
fn uno_o_varios<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum UnoOVarios {
        Uno(String),
        Varios(Vec<String>),
    }

    Ok(match UnoOVarios::deserialize(deserializer)? {
        UnoOVarios::Uno(value) => vec![value],
        UnoOVarios::Varios(values) => values,
    })
}

/// Lo que el informe guarda de la PANTALLA y no esta en la definicion de la consulta.
///
/// Va aparte de [`Report`] a proposito: el visual y el periodo no cambian un numero, cambian
/// como se mira. Mezclarlos con los filtros haria que reabrir un informe en otra vista
/// cambiara los datos.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedView {
    pub visual: Option<String>,
    pub period_months: Option<u8>,
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
    save_view(report, name, &SavedView::default())
}

/// Guarda un informe con lo que la pantalla estaba mostrando.
pub fn save_view(report: &Report, name: &str, view: &SavedView) -> Result<(), SavedError> {
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
        .filter_map(|(column, filters)| {
            // Todos los valores de un filtro, no el primero: un slicer con tres elegidos
            // guardaba uno y el informe volvia con otro recorte.
            let values: Vec<String> = filters
                .iter()
                .flat_map(|filter| filter.values().iter().map(|value| format!("{value:?}")))
                .collect();
            (!values.is_empty()).then(|| SavedFilter {
                column: report
                    .model
                    .column(*column)
                    .map(|(_, c)| c.name.clone())
                    .unwrap_or_default(),
                values,
            })
        })
        .collect();

    let file = File {
        name: name.to_string(),
        database: report.qualifier.clone(),
        dimensions,
        measures,
        filters,
        visual: view.visual.clone(),
        period_months: view.period_months,
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

/// Un informe guardado, como lo necesita la pantalla de abrir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedEntry {
    pub name: String,
    /// La base contra la que se guardo. Un informe de otra base no se puede abrir con el
    /// modelo de esta: los nombres resolverian a columnas distintas o a ninguna.
    pub database: String,
}

/// Los informes guardados con su base.
///
/// Un archivo ilegible se SALTA: una pantalla de abrir que no abre porque uno de los
/// informes esta roto no sirve para lo unico que hace.
pub fn index() -> Vec<SavedEntry> {
    list()
        .into_iter()
        .filter_map(|name| {
            let path = folder().join(format!("{name}.toml"));
            let text = std::fs::read_to_string(&path).ok()?;
            let file: File = toml::from_str(&text).ok()?;
            Some(SavedEntry {
                name,
                database: file.database,
            })
        })
        .collect()
}

/// Abre un informe contra un modelo.
///
/// Los nombres se resuelven contra el modelo de la conexion: los ids son internos y cambian
/// entre ejecuciones. Guardar ids seria guardar numeros que no significan nada mañana.
pub fn open(name: &str, model: &datan_model::Model) -> Result<Report, SavedError> {
    Ok(open_view(name, model)?.0)
}

/// Abre un informe y devuelve ademas lo que la pantalla mostraba al guardarlo.
pub fn open_view(
    name: &str,
    model: &datan_model::Model,
) -> Result<(Report, SavedView), SavedError> {
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
        let values = filter
            .values
            .iter()
            .map(|value| parse_value(value, data_type))
            .collect();
        report = report.filtered_to(column, values);
    }

    let view = SavedView {
        visual: file.visual,
        period_months: file.period_months,
    };

    Ok((report, view))
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

    #[test]
    fn un_informe_viejo_con_un_valor_sigue_abriendo() {
        // El formato de antes tenia `value = "..."` con un solo valor. Un archivo guardado
        // con la version anterior tiene que abrir igual: perder un filtro es abrir otro
        // informe sin avisar.
        let viejo = r#"
name = "ventas"
database = "pg"
dimensions = ["region"]
measures = ["Qty"]
[[filters]]
column = "region"
value = "Text(\"Norte\")"
"#;
        let file: File = toml::from_str(viejo).expect("el formato viejo parsea");

        assert_eq!(file.filters.len(), 1);
        assert_eq!(file.filters[0].values, vec!["Text(\"Norte\")"]);
        assert_eq!(file.visual, None);
        assert_eq!(file.period_months, None);
    }

    #[test]
    fn un_filtro_con_varios_valores_da_la_vuelta() {
        let filter = SavedFilter {
            column: "region".to_string(),
            values: vec!["Text(\"Norte\")".to_string(), "Text(\"Sur\")".to_string()],
        };
        let text = toml::to_string_pretty(&filter).expect("serializa");

        assert!(text.contains("Norte") && text.contains("Sur"), "{text}");

        let back: SavedFilter = toml::from_str(&text).expect("parsea");
        assert_eq!(back, filter);
    }

    #[test]
    fn el_visual_y_el_periodo_solo_se_escriben_si_estan() {
        let file = File {
            name: "ventas".to_string(),
            database: "pg".to_string(),
            dimensions: vec![],
            measures: vec![],
            filters: vec![],
            visual: None,
            period_months: None,
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(!text.contains("visual"), "{text}");
        assert!(!text.contains("period"), "{text}");

        let con_vista = File {
            visual: Some("Anillo".to_string()),
            period_months: Some(6),
            ..file
        };
        let text = toml::to_string_pretty(&con_vista).expect("serializa");
        assert!(text.contains("visual = \"Anillo\""), "{text}");
        assert!(text.contains("period_months = 6"), "{text}");

        let back: File = toml::from_str(&text).expect("parsea");
        assert_eq!(back, con_vista);
    }
}