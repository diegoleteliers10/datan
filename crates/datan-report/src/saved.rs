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

use std::collections::BTreeMap;
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
    #[error("`{name}` no sirve como nombre de informe: sin barra, sin `..` y sin punto al principio")]
    InvalidName { name: String },
    #[error("ya existe un informe llamado `{name}`")]
    Occupied { name: String },
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
    /// El Top N de la vista, si lo habia.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    top_n: Option<SavedTopN>,
    /// El formato de cada visual, por su clave ("Tarjetas", "Barras", "Tabla"...).
    ///
    /// Un mapa y no un solo formato: cambiar de Barras a Tabla y volver tiene que encontrar
    /// las barras como estaban. Un archivo sin esta tabla abre con todos los ajustes
    /// encendidos, que es como se dibujaba antes de que existieran.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    formato: BTreeMap<String, SavedFormato>,
}

/// El formato del visual del lienzo.
///
/// Booleanos y no un enum por tipo de visual: el mismo ajuste sirve a varios tipos, y un
/// enum obligaria a repetir cada campo en cada variante. El panel de propiedades dibuja solo
/// los ajustes que el visual activo usa, asi que ninguno queda encendido sin efecto.
///
/// Los valores por defecto son `true`: un informe guardado antes de que existiera el formato
/// se abre con el mismo dibujo que tenia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedFormato {
    /// El titulo de la tarjeta. En una tarjeta KPI es el nombre de la medida.
    #[serde(skip_serializing_if = "es_cierto")]
    pub titulo: bool,
    /// El fondo y el borde de la tarjeta.
    #[serde(skip_serializing_if = "es_cierto")]
    pub fondo: bool,
    /// El valor al lado de cada barra.
    #[serde(skip_serializing_if = "es_cierto")]
    pub etiquetas: bool,
    /// El eje de valores y su rejilla en los graficos del sistema.
    #[serde(skip_serializing_if = "es_cierto")]
    pub eje: bool,
    /// La variacion contra el periodo anterior y su linea.
    #[serde(skip_serializing_if = "es_cierto")]
    pub comparacion: bool,
    /// Las lineas horizontales de la tabla.
    #[serde(skip_serializing_if = "es_cierto")]
    pub cuadricula: bool,
}

impl Default for SavedFormato {
    fn default() -> Self {
        Self {
            titulo: true,
            fondo: true,
            etiquetas: true,
            eje: true,
            comparacion: true,
            cuadricula: true,
        }
    }
}

/// Un ajuste encendido es el default y no se escribe: el archivo dice que se apago.
fn es_cierto(value: &bool) -> bool {
    *value
}

impl SavedFormato {
    /// `true` si esta todo como al abrir la app. Un archivo con el formato por defecto no
    /// escribe la tabla: el archivo sigue diciendo lo mismo con menos lineas.
    pub fn es_el_default(&self) -> bool {
        *self == Self::default()
    }
}

/// El Top N de la vista, con la medida por NOMBRE.
///
/// Los ids de las medidas son internos y cambian entre ejecuciones: guardar el id seria
/// guardar un numero que manana no significa nada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTopN {
    pub measure: String,
    pub count: usize,
    /// `true` toma los MENORES.
    #[serde(default)]
    pub abajo: bool,
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
    pub top_n: Option<SavedTopN>,
    /// El formato de cada visual, por su clave. Los visuales que no estan usan el default.
    pub formato: BTreeMap<String, SavedFormato>,
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
        return Err(SavedError::InvalidName {
            name: name.to_string(),
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
        top_n: view.top_n.clone(),
        // Un visual que quedo con todos los ajustes encendidos no se escribe: el mapa dice
        // donde el usuario cambio algo.
        formato: view
            .formato
            .iter()
            .filter(|(_, formato)| !formato.es_el_default())
            .map(|(visual, formato)| (visual.clone(), *formato))
            .collect(),
    };

    let text = toml::to_string_pretty(&file)
        .map_err(|_| SavedError::UnknownDimension {
            name: name.to_string(),
            database: String::new(),
        })?;

    std::fs::write(&path, text).map_err(|source| SavedError::Write { path, source })
}

/// Renombra un informe guardado.
///
/// Mueve el archivo y actualiza el nombre de adentro. El nombre del informe ES el nombre del
/// archivo: cambiar solo uno de los dos deja un informe que dice llamarse de una forma y vive
/// en otra, y el proximo guardado escribe un segundo archivo con el nombre nuevo.
///
/// No pisa un informe que ya existe. Perder uno para renombrar otro es la clase de perdida
/// que no se nota hasta que se busca.
pub fn rename(from: &str, to: &str) -> Result<(), SavedError> {
    rename_in(&folder(), from, to)
}

/// La mitad testeable de `rename`.
fn rename_in(folder: &std::path::Path, from: &str, to: &str) -> Result<(), SavedError> {
    let from_path = folder.join(file_name(from)?);
    let to_path = folder.join(file_name(to)?);
    if from_path == to_path {
        return Ok(());
    }

    let text = std::fs::read_to_string(&from_path).map_err(|source| SavedError::Read {
        path: from_path.clone(),
        source,
    })?;
    let mut file: File = toml::from_str(&text).map_err(|source| SavedError::Parse {
        path: from_path.clone(),
        source,
    })?;

    if to_path.exists() {
        return Err(SavedError::Occupied {
            name: to.to_string(),
        });
    }

    file.name = to.to_string();
    let text = toml::to_string_pretty(&file).map_err(|_| SavedError::InvalidName {
        name: to.to_string(),
    })?;
    std::fs::write(&to_path, text).map_err(|source| SavedError::Write {
        path: to_path.clone(),
        source,
    })?;
    // El viejo se borra DESPUES de escribir el nuevo: si el borrado falla queda una copia de
    // mas, que se ve y se puede borrar. Al reves quedaria un informe sin archivo.
    std::fs::remove_file(&from_path).map_err(|source| SavedError::Write {
        path: from_path,
        source,
    })?;
    Ok(())
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
        top_n: file.top_n,
        formato: file.formato,
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

    fn carpeta_de_prueba(nombre: &str) -> std::path::PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "datan-informes-{}-{nombre}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("crea la carpeta");
        folder
    }

    fn informe_de_prueba(nombre: &str) -> File {
        File {
            name: nombre.to_string(),
            database: "pg".to_string(),
            dimensions: vec!["region".to_string()],
            measures: vec!["Qty".to_string()],
            filters: vec![],
            visual: None,
            period_months: None,
            top_n: None,
            formato: BTreeMap::new(),
        }
    }

    #[test]
    fn renombrar_mueve_el_archivo_y_el_nombre_de_adentro() {
        let folder = carpeta_de_prueba("mueve");
        let viejo = folder.join("region.toml");
        let texto = toml::to_string_pretty(&informe_de_prueba("region")).expect("serializa");
        std::fs::write(&viejo, texto).expect("escribe");

        rename_in(&folder, "region", "region norte").expect("renombra");

        assert!(!viejo.exists(), "el archivo viejo ya no esta");
        let nuevo = std::fs::read_to_string(folder.join("region norte.toml")).expect("lee");
        let file: File = toml::from_str(&nuevo).expect("parsea");
        assert_eq!(file.name, "region norte");
        // Lo que define el informe viaja: el renombre no toca la definicion.
        assert_eq!(file.dimensions, vec!["region".to_string()]);
        assert_eq!(file.database, "pg");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn renombrar_no_pisa_otro_informe() {
        let folder = carpeta_de_prueba("ocupado");
        for nombre in ["region", "categoria"] {
            let texto = toml::to_string_pretty(&informe_de_prueba(nombre)).expect("serializa");
            std::fs::write(folder.join(format!("{nombre}.toml")), texto).expect("escribe");
        }

        let error = rename_in(&folder, "region", "categoria").expect_err("no pisa");
        assert!(matches!(error, SavedError::Occupied { .. }), "{error:?}");
        // Los dos quedan como estaban.
        assert!(folder.join("region.toml").exists());
        assert!(folder.join("categoria.toml").exists());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn renombrar_a_un_nombre_invalido_no_toca_nada() {
        let folder = carpeta_de_prueba("invalido");
        let texto = toml::to_string_pretty(&informe_de_prueba("region")).expect("serializa");
        std::fs::write(folder.join("region.toml"), texto).expect("escribe");

        let error = rename_in(&folder, "region", "../fuera").expect_err("rechaza");
        assert!(matches!(error, SavedError::InvalidName { .. }), "{error:?}");
        assert!(folder.join("region.toml").exists());
        let _ = std::fs::remove_dir_all(&folder);
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
        assert_eq!(file.top_n, None);
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
            top_n: None,
            formato: BTreeMap::new(),
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(!text.contains("visual"), "{text}");
        assert!(!text.contains("period"), "{text}");
        assert!(!text.contains("top_n"), "{text}");

        let con_vista = File {
            visual: Some("Anillo".to_string()),
            period_months: Some(6),
            top_n: Some(SavedTopN {
                measure: "Qty".to_string(),
                count: 5,
                abajo: false,
            }),
            ..file
        };
        let text = toml::to_string_pretty(&con_vista).expect("serializa");
        assert!(text.contains("visual = \"Anillo\""), "{text}");
        assert!(text.contains("period_months = 6"), "{text}");
        assert!(text.contains("measure = \"Qty\""), "{text}");

        let back: File = toml::from_str(&text).expect("parsea");
        assert_eq!(back, con_vista);
        assert_eq!(back.top_n.expect("el top n").count, 5);
    }

    #[test]
    fn un_informe_sin_formato_abre_con_todos_los_ajustes() {
        // Los informes guardados antes de que existiera el formato no tienen la tabla, y
        // tienen que abrirse con el dibujo de siempre.
        let viejo = r#"
name = "ventas"
database = "pg"
dimensions = ["region"]
measures = ["Qty"]
"#;
        let file: File = toml::from_str(viejo).expect("parsea");
        assert!(file.formato.is_empty());
    }

    #[test]
    fn un_formato_a_medias_llena_el_resto_con_el_default() {
        // Un `[formato]` con un solo ajuste apaga ese y deja los demas como estaban: la
        // tabla se escribe solo con lo que el usuario toco.
        let texto = r#"
name = "ventas"
database = "pg"
dimensions = []
measures = []

[formato.Barras]
titulo = false
"#;
        let file: File = toml::from_str(texto).expect("parsea");
        let formato = file.formato.get("Barras").expect("el visual");
        assert!(!formato.titulo);
        assert!(formato.fondo);
        assert!(formato.etiquetas);
        assert!(!formato.es_el_default());
    }

    #[test]
    fn el_formato_viaja_en_el_archivo() {
        let formato = SavedFormato {
            titulo: false,
            cuadricula: false,
            ..SavedFormato::default()
        };
        let file = File {
            name: "ventas".to_string(),
            database: "pg".to_string(),
            dimensions: vec![],
            measures: vec![],
            filters: vec![],
            visual: Some("Tabla".to_string()),
            period_months: None,
            top_n: None,
            formato: BTreeMap::from([("Tabla".to_string(), formato)]),
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(text.contains("[formato.Tabla]"), "{text}");
        assert!(text.contains("titulo = false"), "{text}");
        // Lo que esta encendido no se escribe: el archivo dice lo que se apago.
        assert!(!text.contains("fondo = true"), "{text}");

        let back: File = toml::from_str(&text).expect("vuelve a parsear");
        assert_eq!(back.formato.get("Tabla"), Some(&formato));
    }

    #[test]
    fn el_formato_por_defecto_no_se_escribe() {
        let file = File {
            name: "ventas".to_string(),
            database: "pg".to_string(),
            dimensions: vec![],
            measures: vec![],
            filters: vec![],
            visual: None,
            period_months: None,
            top_n: None,
            formato: BTreeMap::new(),
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(!text.contains("[formato]"), "{text}");
    }
}