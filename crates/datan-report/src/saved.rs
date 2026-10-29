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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use datan_dax_eval::{Filter, FilterOperator, FilterValue};
use datan_model::DataType;
use serde::{Deserialize, Serialize};

use crate::Report;

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

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
    #[error(
        "`{name}` no sirve como nombre de informe: sin barra, sin `..` y sin punto al principio"
    )]
    InvalidName { name: String },
    #[error("ya existe un informe llamado `{name}`")]
    Occupied { name: String },
}

/// El archivo de un informe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct File {
    name: String,
    database: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    loaded_queries: Vec<SavedSourceTable>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_positions: BTreeMap<String, SavedModelPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_at: Option<i64>,
    dimensions: Vec<String>,
    measures: Vec<String>,
    #[serde(default)]
    filters: Vec<SavedFilter>,
    /// Filtros que se aplican a todas las paginas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    report_filters: Vec<SavedFilter>,
    #[serde(default, skip_serializing_if = "SavedPanels::is_default")]
    panels: SavedPanels,
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
    /// Paginas del informe. Si faltan, `open_view` devuelve una sola vista como antes.
    #[serde(
        default,
        rename = "pagina",
        alias = "pages",
        skip_serializing_if = "Vec::is_empty"
    )]
    pages: Vec<SavedPage>,
    /// La pagina seleccionada al guardar.
    #[serde(default, skip_serializing_if = "es_cero")]
    active_page: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct RecentFile {
    reports: Vec<RecentReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecentReport {
    name: String,
    opened_at: i64,
    #[serde(default)]
    path: Option<PathBuf>,
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
    /// El eje Y o el eje de valores del grafico.
    #[serde(skip_serializing_if = "es_cierto")]
    pub eje: bool,
    /// El eje X o el eje de categorias del grafico.
    #[serde(skip_serializing_if = "es_cierto")]
    pub eje_x: bool,
    /// Rejilla de los graficos. None conserva el valor del campo legacy `eje`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejilla_graficos: Option<bool>,
    /// La variacion contra el periodo anterior y su linea.
    #[serde(skip_serializing_if = "es_cierto")]
    pub comparacion: bool,
    /// Las lineas horizontales de la tabla.
    #[serde(skip_serializing_if = "es_cierto")]
    pub cuadricula: bool,
    /// Densidad de filas de la tabla.
    #[serde(skip_serializing_if = "SavedTableDensity::is_normal")]
    pub densidad_tabla: SavedTableDensity,
    /// Alterna el fondo de las filas pares de la tabla.
    #[serde(skip_serializing_if = "es_falso")]
    pub filas_alternas: bool,
    /// El renglon de total al final de la tabla. Con dos dimensiones o mas, los subtotales.
    #[serde(skip_serializing_if = "es_cierto")]
    pub total_general: bool,
    /// El indice de color para la serie principal, de 0 a 4.
    #[serde(skip_serializing_if = "es_color_default")]
    pub color_serie: u8,
    /// Color RGB personalizado para la serie principal. Cero usa la paleta.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_serie_rgb: u32,
    /// Muestra el nombre de la medida como leyenda.
    #[serde(skip_serializing_if = "es_cierto")]
    pub leyenda: bool,
    /// Radio interior del anillo, en px antes del zoom.
    #[serde(skip_serializing_if = "es_sesenta_y_cuatro")]
    pub radio_interior_anillo: u8,
    /// Los valores menores tienen un mejor resultado.
    #[serde(skip_serializing_if = "es_falso")]
    pub kpi_menor_es_mejor: bool,
    /// Muestra el promedio como linea de referencia en Lineas y Area.
    #[serde(skip_serializing_if = "es_falso")]
    pub linea_referencia: bool,
    /// Muestra una referencia para el valor minimo del visual.
    #[serde(skip_serializing_if = "es_falso")]
    pub linea_minimo: bool,
    /// Muestra una referencia para el valor maximo del visual.
    #[serde(skip_serializing_if = "es_falso")]
    pub linea_maximo: bool,
    /// Muestra una referencia numerica fija.
    #[serde(skip_serializing_if = "es_falso")]
    pub linea_constante: bool,
    /// Valor fijo en milésimas para conservar Eq y Copy en el formato.
    #[serde(skip_serializing_if = "es_cero_i64")]
    pub valor_linea_constante_milesimas: i64,
    /// Muestra el ajuste lineal de la serie.
    #[serde(skip_serializing_if = "es_falso")]
    pub linea_tendencia: bool,
    /// Extiende el ajuste lineal por tres periodos.
    #[serde(skip_serializing_if = "es_falso")]
    pub pronostico: bool,
    /// Muestra una escala de color en las celdas numericas de Matriz.
    #[serde(skip_serializing_if = "es_falso")]
    pub formato_condicional: bool,
    /// Muestra un total calculado por categoria del eje de columnas en Matriz.
    #[serde(skip_serializing_if = "es_falso")]
    pub subtotales_columna: bool,
    /// Escala de texto del visual como porcentaje. El valor por defecto es 100.
    #[serde(skip_serializing_if = "es_cien")]
    pub escala_texto: u8,
    /// Escala del titulo, como porcentaje de su tamano normal.
    #[serde(skip_serializing_if = "es_cien")]
    pub escala_titulo: u8,
    /// Escala del subtitulo, como porcentaje de su tamano normal.
    #[serde(skip_serializing_if = "es_cien")]
    pub escala_subtitulo: u8,
    /// Escala del valor principal, como porcentaje de su tamano normal.
    #[serde(skip_serializing_if = "es_cien")]
    pub escala_valor: u8,
    /// Color RGB de texto. Cero usa el color del tema.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_texto: u32,
    /// Color RGB del titulo. Cero usa el color de texto.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_titulo: u32,
    /// Color RGB del subtitulo. Cero usa el color de texto.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_subtitulo: u32,
    /// Color RGB del valor principal. Cero usa el color de texto.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_valor: u32,
    /// Color RGB de fondo. Cero usa el color del tema.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_fondo: u32,
    /// Color RGB de borde. Cero usa el color del tema.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_borde: u32,
    /// Grosor del borde, de 0 a 3 px. El valor por defecto es 1.
    #[serde(skip_serializing_if = "es_uno")]
    pub ancho_borde: u8,
    /// Radio de esquina en px. El valor por defecto es 12.
    #[serde(skip_serializing_if = "es_doce")]
    pub radio_borde: u8,
    /// Usa una fuente monoespaciada en el visual.
    #[serde(skip_serializing_if = "es_falso")]
    pub fuente_mono: bool,
    /// Familia tipografica del visual. `System` conserva la fuente del tema.
    #[serde(skip_serializing_if = "SavedFontFamily::is_system")]
    pub familia_fuente: SavedFontFamily,
    /// Colorea el valor KPI con el estado frente a su medida objetivo.
    #[serde(skip_serializing_if = "es_falso")]
    pub color_por_objetivo: bool,
    /// Color RGB del resultado favorable. Cero usa verde.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_estado_favorable: u32,
    /// Color RGB del resultado desfavorable. Cero usa rojo.
    #[serde(skip_serializing_if = "es_cero_rgb")]
    pub color_estado_desfavorable: u32,
}

impl Default for SavedFormato {
    fn default() -> Self {
        Self {
            titulo: true,
            fondo: true,
            etiquetas: true,
            eje: true,
            eje_x: true,
            rejilla_graficos: None,
            comparacion: true,
            cuadricula: true,
            densidad_tabla: SavedTableDensity::default(),
            filas_alternas: false,
            total_general: true,
            color_serie: 0,
            color_serie_rgb: 0,
            leyenda: true,
            radio_interior_anillo: 64,
            kpi_menor_es_mejor: false,
            linea_referencia: false,
            linea_minimo: false,
            linea_maximo: false,
            linea_constante: false,
            valor_linea_constante_milesimas: 0,
            linea_tendencia: false,
            pronostico: false,
            formato_condicional: false,
            subtotales_columna: false,
            escala_texto: 100,
            escala_titulo: 100,
            escala_subtitulo: 100,
            escala_valor: 100,
            color_texto: 0,
            color_titulo: 0,
            color_subtitulo: 0,
            color_valor: 0,
            color_fondo: 0,
            color_borde: 0,
            ancho_borde: 1,
            radio_borde: 12,
            fuente_mono: false,
            familia_fuente: SavedFontFamily::System,
            color_por_objetivo: false,
            color_estado_favorable: 0,
            color_estado_desfavorable: 0,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedTableDensity {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

impl SavedTableDensity {
    fn is_normal(density: &Self) -> bool {
        *density == Self::Normal
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedFontFamily {
    #[default]
    System,
    Sans,
    Serif,
    Monospace,
}

impl SavedFontFamily {
    fn is_system(family: &Self) -> bool {
        *family == Self::System
    }
}

/// Un ajuste encendido es el default y no se escribe: el archivo dice que se apago.
fn es_cierto(value: &bool) -> bool {
    *value
}

fn es_falso(value: &bool) -> bool {
    !*value
}

fn es_cero(value: &usize) -> bool {
    *value == 0
}

fn es_cero_i64(value: &i64) -> bool {
    *value == 0
}

fn es_cero_rgb(value: &u32) -> bool {
    *value == 0
}

fn es_cien(value: &u8) -> bool {
    *value == 100
}

fn es_uno(value: &u8) -> bool {
    *value == 1
}

fn es_doce(value: &u8) -> bool {
    *value == 12
}

fn es_sesenta_y_cuatro(value: &u8) -> bool {
    *value == 64
}

fn es_color_default(value: &u8) -> bool {
    *value == 0
}

impl SavedFormato {
    pub fn muestra_rejilla_graficos(&self) -> bool {
        self.rejilla_graficos.unwrap_or(self.eje)
    }

    /// Cambia el eje de valores y conserva el estado visible de la rejilla.
    pub fn set_eje_valores(&mut self, visible: bool) {
        self.rejilla_graficos
            .get_or_insert_with(|| self.eje);
        self.eje = visible;
    }

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFilter {
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(alias = "value", default, deserialize_with = "uno_o_varios")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<SavedFilterOperator>,
}

/// Operador persistido de un filtro. `None` conserva el formato legacy `IN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedFilterOperator {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    /// Rango inclusivo con dos valores: limite inferior y limite superior.
    Between,
}

impl From<SavedFilterOperator> for FilterOperator {
    fn from(operator: SavedFilterOperator) -> Self {
        match operator {
            SavedFilterOperator::Equal => Self::Equal,
            SavedFilterOperator::NotEqual => Self::NotEqual,
            SavedFilterOperator::Less => Self::Less,
            SavedFilterOperator::LessOrEqual => Self::LessOrEqual,
            SavedFilterOperator::Greater => Self::Greater,
            SavedFilterOperator::GreaterOrEqual => Self::GreaterOrEqual,
            SavedFilterOperator::Between => unreachable!("Between expands to two predicates"),
        }
    }
}

impl From<FilterOperator> for SavedFilterOperator {
    fn from(operator: FilterOperator) -> Self {
        match operator {
            FilterOperator::Equal => Self::Equal,
            FilterOperator::NotEqual => Self::NotEqual,
            FilterOperator::Less => Self::Less,
            FilterOperator::LessOrEqual => Self::LessOrEqual,
            FilterOperator::Greater => Self::Greater,
            FilterOperator::GreaterOrEqual => Self::GreaterOrEqual,
            FilterOperator::In => unreachable!("IN uses the legacy values field"),
        }
    }
}

/// Referencia a una columna guardada. La variante legacy mantiene compatibilidad con informes viejos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SavedColumnRef {
    Legacy(String),
    Qualified { table: String, column: String },
}

impl SavedColumnRef {
    /// Resuelve una referencia calificada sin confundir columnas con el mismo nombre.
    pub fn resolve(&self, model: &datan_model::Model) -> Option<datan_model::ObjectId> {
        match self {
            Self::Legacy(name) => model
                .tables
                .iter()
                .find_map(|table| table.column_by_name(name).map(|column| column.id)),
            Self::Qualified { table, column } => model
                .table_by_name(table)
                .and_then(|table| table.column_by_name(column))
                .map(|column| column.id),
        }
    }
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedView {
    pub visual: Option<String>,
    pub period_months: Option<u8>,
    pub top_n: Option<SavedTopN>,
    /// El formato de cada visual, por su clave. Los visuales que no estan usan el default.
    pub formato: BTreeMap<String, SavedFormato>,
    /// Paginas guardadas. La lista vacia mantiene el informe de una sola pagina.
    pub pages: Vec<SavedPage>,
    /// Indice de la pagina seleccionada. Los archivos viejos abren la primera.
    pub active_page: usize,
    /// Filtros comunes a todas las paginas.
    pub report_filters: Vec<SavedFilter>,
    pub panels: SavedPanels,
    /// Hora Unix del ultimo guardado correcto.
    pub saved_at: Option<i64>,
    /// Clave que separa las consultas cargadas de cada informe.
    pub data_scope: Option<String>,
    /// Clave estable de la conexión, sin credenciales.
    pub source_key: Option<String>,
    /// Esquema de las tablas cargadas por el informe.
    pub loaded_queries: Vec<SavedSourceTable>,
    /// Posiciones de las tablas en el lienzo Modelo, por nombre estable.
    pub model_positions: BTreeMap<String, SavedModelPosition>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSourceTable {
    pub table: String,
    pub source_sql: String,
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
}

/// Visibilidad de los paneles laterales del informe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPanels {
    #[serde(skip_serializing_if = "es_cierto")]
    pub datos: bool,
    #[serde(skip_serializing_if = "es_cierto")]
    pub filtros: bool,
    #[serde(skip_serializing_if = "es_cierto")]
    pub visualizaciones: bool,
}

impl SavedPanels {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for SavedPanels {
    fn default() -> Self {
        Self {
            datos: true,
            filtros: true,
            visualizaciones: true,
        }
    }
}

/// Una pagina guardada por nombre de campo, para que los ids internos puedan cambiar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPage {
    pub name: String,
    /// Tamano del lienzo de esta pagina, en pixeles del lienzo.
    #[serde(default, skip_serializing_if = "CanvasSize::is_default")]
    pub canvas_size: CanvasSize,
    /// Identificador del visual seleccionado en el lienzo. Las paginas viejas usan cadena vacia.
    pub active_visual: String,
    pub dimensions: Vec<SavedColumnRef>,
    pub measures: Vec<String>,
    pub filters: Vec<SavedFilter>,
    pub visual: Option<String>,
    pub period_months: Option<u8>,
    pub top_n: Option<SavedTopN>,
    pub formato: BTreeMap<String, SavedFormato>,
    pub sort: Option<SavedSort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom_percent: Option<u16>,
    /// Visuales del lienzo. Los campos anteriores siguen como entrada de migracion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visuals: Vec<SavedVisual>,
    /// Vistas guardadas del lienzo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<SavedBookmark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedImplicitMeasure {
    pub column: SavedColumnRef,
    pub aggregation: SavedAggregation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "field")]
pub enum SavedValueOrder {
    Measure(String),
    Implicit(SavedColumnRef),
    ImplicitMeasure(SavedImplicitMeasure),
}

/// Roles de campo que el visual puede guardar aunque todavia no cambien la consulta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedVisualRole {
    SecondaryY,
    SmallMultiples,
    Size,
    Tooltips,
}

/// Un campo que ocupa un rol de un visual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "field")]
pub enum SavedVisualField {
    Column(SavedColumnRef),
    Measure(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum SavedColorMetric {
    Measure(String),
    Implicit(SavedImplicitMeasure),
}

/// Una condicion numerica para colorear un punto o un KPI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operator", content = "values")]
pub enum SavedColorRuleCondition {
    LessThan(String),
    BetweenInclusive { minimum: String, maximum: String },
    AtLeast(String),
}

impl SavedColorRuleCondition {
    fn parse_limit(value: &str) -> Option<f64> {
        value
            .trim()
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .filter(|limit| limit.is_finite())
    }

    pub fn matches(&self, value: f64) -> bool {
        if !value.is_finite() {
            return false;
        }
        match self {
            Self::LessThan(limit) => Self::parse_limit(limit).is_some_and(|limit| value < limit),
            Self::BetweenInclusive { minimum, maximum } => {
                let minimum = Self::parse_limit(minimum);
                let maximum = Self::parse_limit(maximum);
                matches!((minimum, maximum), (Some(minimum), Some(maximum)) if minimum <= maximum && value >= minimum && value <= maximum)
            }
            Self::AtLeast(limit) => Self::parse_limit(limit).is_some_and(|limit| value >= limit),
        }
    }
}

/// Una regla de color. La primera condicion que coincide gana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedConditionalColorRule {
    pub condition: SavedColorRuleCondition,
    pub color: u32,
}

impl Default for SavedConditionalColorRule {
    fn default() -> Self {
        Self {
            condition: SavedColorRuleCondition::AtLeast("0".to_string()),
            color: 0x2457d6,
        }
    }
}

/// Reglas ordenadas de color para un visual.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedConditionalColorRules {
    pub rules: Vec<SavedConditionalColorRule>,
}

impl SavedConditionalColorRules {
    pub fn insert_before_fallback(&mut self, rule: SavedConditionalColorRule) {
        let fallback_index = self.rules.len().checked_sub(1).filter(|index| {
            matches!(
                &self.rules[*index].condition,
                SavedColorRuleCondition::AtLeast(_)
            )
        });
        if let Some(index) = fallback_index {
            self.rules.insert(index, rule);
        } else {
            self.rules.push(rule);
        }
    }

    pub fn color_for(&self, value: f64) -> Option<u32> {
        self.rules
            .iter()
            .find(|rule| rule.condition.matches(value))
            .map(|rule| rule.color & 0x00ff_ffff)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "field")]
pub enum SavedGaugeField {
    Measure(String),
    Implicit(SavedColumnRef),
    ImplicitMeasure(SavedImplicitMeasure),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedGaugeFields {
    pub value: Option<SavedGaugeField>,
    pub minimum: Option<SavedGaugeField>,
    pub maximum: Option<SavedGaugeField>,
    pub target: Option<SavedGaugeField>,
}

fn gauge_fields_are_empty(fields: &SavedGaugeFields) -> bool {
    fields.value.is_none()
        && fields.minimum.is_none()
        && fields.maximum.is_none()
        && fields.target.is_none()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedAggregation {
    Sum,
    Average,
    Count,
    CountRows,
    DistinctCount,
    Min,
    Max,
    First,
    Last,
    Earliest,
    Latest,
    Median,
    Variance,
    StandardDeviation,
}

/// Tamano del lienzo de una pagina.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasSize {
    pub width: u32,
    pub height: u32,
}

impl CanvasSize {
    fn is_default(size: &Self) -> bool {
        *size == Self::default()
    }

    /// Keeps dimensions loaded from a project inside the canvas limits.
    pub fn normalized(self) -> Self {
        Self {
            width: self.width.clamp(280, 10_000),
            height: self.height.clamp(160, 10_000),
        }
    }
}

impl Default for CanvasSize {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
        }
    }
}

/// Posicion de un visual en la pagina, en unidades del lienzo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPosition {
    pub x: u32,
    pub y: u32,
}

/// Posicion de una tabla en el lienzo Modelo, en unidades del lienzo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedModelPosition {
    pub x: u32,
    pub y: u32,
}

/// Tamano de un visual en la pagina, en unidades del lienzo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSize {
    pub width: u32,
    pub height: u32,
}

/// Un visual del lienzo. Los nombres de columnas y medidas no dependen de ids de ejecucion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedVisual {
    /// Identificador estable dentro del informe.
    pub id: String,
    /// Titulo visible y editable en el lienzo.
    #[serde(default)]
    pub title: String,
    /// Texto opcional que aparece debajo del titulo.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    pub position: SavedPosition,
    pub size: SavedSize,
    pub visible: bool,
    /// Tipo de visual, por ejemplo `Tabla`, `Barras` o `Anillo`.
    pub kind: String,
    /// Medida del visual que define el color condicional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_color_measure: Option<String>,
    /// Segunda medida para comparar fila a fila con la medida de color condicional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_color_compare_measure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_color_source: Option<SavedColorMetric>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_color_comparison: Option<SavedColorMetric>,
    /// Reglas numericas. `None` conserva el color condicional de los informes anteriores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_color_rules: Option<SavedConditionalColorRules>,
    /// Vista del segmentador. Los informes anteriores usan lista.
    #[serde(default, skip_serializing_if = "slicer_mode_is_list")]
    pub slicer_mode: SavedSlicerMode,
    /// Intervalo inclusivo del segmentador de fechas, en dias desde 1970-01-01.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_range: Option<SavedDateRange>,
    pub dimensions: Vec<SavedColumnRef>,
    /// Columna que separa una dimensión en series. `None` conserva informes antiguos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legend: Option<SavedColumnRef>,
    /// Colores por valor de leyenda. La etiqueta se usa como clave estable entre sesiones.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub series_colors: BTreeMap<String, u32>,
    /// Numero de dimensiones de fila en una matriz. `None` conserva el diseño anterior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix_row_count: Option<usize>,
    pub measures: Vec<String>,
    /// Agregaciones implicitas de columnas, locales al visual.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implicit_measures: Vec<SavedImplicitMeasure>,
    /// Orden de campos de valores. Vacio en informes antiguos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_order: Vec<SavedValueOrder>,
    /// Roles de campo que el renderer aun no usa. Se guardan al mover los campos entre visuales.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<SavedVisualRole, Vec<SavedVisualField>>,
    /// Asignaciones de los campos del Medidor. `None` restaura informes anteriores.
    #[serde(default, skip_serializing_if = "gauge_fields_are_empty")]
    pub gauge_fields: SavedGaugeFields,
    pub filters: Vec<SavedFilter>,
    /// Seleccion propia del segmentador. `None` identifica informes antiguos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slicer_selection: Option<Vec<SavedFilter>>,
    pub top_n: Option<SavedTopN>,
    pub sort: Option<SavedSort>,
    pub period_months: Option<u8>,
    /// Formato de cada tipo de visual usado por este elemento.
    /// `formato` conserva el formato activo para lectores de informes anteriores.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub formatos: BTreeMap<String, SavedFormato>,
    pub formato: SavedFormato,
    /// Interaccion de este visual con cada destino, por id.
    pub interactions: BTreeMap<String, SavedInteraction>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedSlicerMode {
    #[default]
    List,
    Dropdown,
    DateRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDateRange {
    pub start: i32,
    pub end: i32,
}

fn slicer_mode_is_list(mode: &SavedSlicerMode) -> bool {
    *mode == SavedSlicerMode::List
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedInteraction {
    #[default]
    Filter,
    Highlight,
    Disabled,
}

/// Una instantanea con nombre de los filtros y visuales de una pagina.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedBookmark {
    pub name: String,
    pub active_visual: String,
    pub filters: Vec<SavedFilter>,
    pub visuals: Vec<SavedVisual>,
}

impl Default for SavedVisual {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            subtitle: String::new(),
            position: SavedPosition::default(),
            size: SavedSize::default(),
            visible: true,
            kind: String::new(),
            conditional_color_measure: None,
            conditional_color_compare_measure: None,
            conditional_color_source: None,
            conditional_color_comparison: None,
            conditional_color_rules: None,
            slicer_mode: SavedSlicerMode::default(),
            date_range: None,
            dimensions: Vec::new(),
            legend: None,
            series_colors: BTreeMap::new(),
            matrix_row_count: None,
            measures: Vec::new(),
            implicit_measures: Vec::new(),
            value_order: Vec::new(),
            roles: BTreeMap::new(),
            gauge_fields: SavedGaugeFields::default(),
            filters: Vec::new(),
            slicer_selection: None,
            top_n: None,
            sort: None,
            period_months: None,
            formatos: BTreeMap::new(),
            formato: SavedFormato::default(),
            interactions: BTreeMap::new(),
        }
    }
}

/// El orden guardado por indice de columna de la matriz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSort {
    pub column_index: usize,
    #[serde(default)]
    pub descending: bool,
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

fn recent_file_path() -> PathBuf {
    folder()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("recent-reports.toml")
}

fn recent_times(path: &Path) -> std::collections::HashMap<String, i64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<RecentFile>(&text).ok())
        .map(|file| {
            file.reports
                .into_iter()
                .filter(|report| report.path.is_none())
                .map(|report| (report.name, report.opened_at))
                .collect()
        })
        .unwrap_or_default()
}

fn recent_path_times(path: &Path) -> std::collections::HashMap<PathBuf, i64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<RecentFile>(&text).ok())
        .map(|file| {
            file.reports
                .into_iter()
                .filter_map(|report| report.path.map(|path| (path, report.opened_at)))
                .collect()
        })
        .unwrap_or_default()
}

fn record_opened_at(path: &Path, name: &str, file_path: Option<&Path>, opened_at: i64) {
    let mut file = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<RecentFile>(&text).ok())
        .unwrap_or_default();
    file.reports.retain(|report| match file_path {
        Some(file_path) => report.path.as_deref() != Some(file_path),
        None => report.path.is_some() || report.name != name,
    });
    file.reports.insert(
        0,
        RecentReport {
            name: name.to_string(),
            opened_at,
            path: file_path.map(Path::to_path_buf),
        },
    );
    file.reports.truncate(50);
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(text) = toml::to_string_pretty(&file) else {
        return;
    };
    let _ = std::fs::write(path, text);
}

/// Registra una apertura correcta para ordenar la lista de informes recientes.
pub fn mark_opened(name: &str) {
    let opened_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64);
    if let Some(opened_at) = opened_at {
        record_opened_at(&recent_file_path(), name, None, opened_at);
    }
}

/// Registra una apertura o un guardado por ruta, incluso fuera de la carpeta local de informes.
pub fn mark_opened_path(name: &str, path: &Path) {
    let opened_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64);
    if let Some(opened_at) = opened_at {
        record_opened_at(&recent_file_path(), name, Some(path), opened_at);
    }
}

/// Guarda un informe.
pub fn save(report: &Report, name: &str) -> Result<(), SavedError> {
    save_view(report, name, &SavedView::default())
}

/// Guarda un informe con lo que la pantalla estaba mostrando.
pub fn save_view(report: &Report, name: &str, view: &SavedView) -> Result<(), SavedError> {
    let path = folder().join(file_name(name)?);
    save_view_to(report, name, &path, view)
}

/// Guarda el informe en la ruta que el usuario eligió.
pub fn save_view_to(
    report: &Report,
    name: &str,
    path: &Path,
    view: &SavedView,
) -> Result<(), SavedError> {
    let path = path.to_path_buf();

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
            let (table, definition) = report.model.column(*column).expect("columna del modelo");
            let mut saved = Vec::new();
            let lower = filters.iter().position(|filter| {
                filter.operator == FilterOperator::GreaterOrEqual && filter.values().len() == 1
            });
            let upper = filters.iter().position(|filter| {
                filter.operator == FilterOperator::LessOrEqual && filter.values().len() == 1
            });
            let range_indices = match (lower, upper) {
                (Some(lower), Some(upper)) if lower != upper => {
                    let values = [&filters[lower].values()[0], &filters[upper].values()[0]]
                        .into_iter()
                        .map(|value| format!("{value:?}"))
                        .collect();
                    saved.push(SavedFilter {
                        column: definition.name.clone(),
                        table: Some(table.name.clone()),
                        values,
                        operator: Some(SavedFilterOperator::Between),
                    });
                    Some([lower, upper])
                }
                _ => None,
            };

            for (index, filter) in filters.iter().enumerate() {
                if range_indices.is_some_and(|indices| indices.contains(&index)) {
                    continue;
                }
                let values = filter
                    .values()
                    .iter()
                    .map(|value| format!("{value:?}"))
                    .collect::<Vec<_>>();
                let operator = match filter.operator {
                    FilterOperator::In => None,
                    operator => Some(SavedFilterOperator::from(operator)),
                };
                if !values.is_empty() {
                    saved.push(SavedFilter {
                        column: definition.name.clone(),
                        table: Some(table.name.clone()),
                        values,
                        operator,
                    });
                }
            }
            saved
        })
        .collect();

    let file = File {
        name: name.to_string(),
        database: report.qualifier.clone(),
        data_scope: view.data_scope.clone(),
        source_key: view.source_key.clone(),
        loaded_queries: view.loaded_queries.clone(),
        model_positions: view.model_positions.clone(),
        saved_at: view.saved_at,
        dimensions,
        measures,
        filters,
        report_filters: view.report_filters.clone(),
        panels: view.panels,
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
        pages: view.pages.clone(),
        active_page: view.active_page,
    };

    let text = toml::to_string_pretty(&file).map_err(|_| SavedError::UnknownDimension {
        name: name.to_string(),
        database: String::new(),
    })?;

    write_atomically(&path, &text).map_err(|source| SavedError::Write { path, source })
}

fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    write_atomically_with(path, contents, |from, to| std::fs::rename(from, to))
}

fn write_atomically_with(
    path: &Path,
    contents: &str,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    if path.file_name().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "la ruta no tiene nombre",
        ));
    }
    let temporary = loop {
        let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".datan-save-{}-{id}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(mut file) => {
                let write_result = file
                    .write_all(contents.as_bytes())
                    .and_then(|()| file.sync_all());
                drop(file);
                if let Err(error) = write_result {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error);
                }
                break temporary;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };

    let result = replace(&temporary, path);
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
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
    file.saved_at = Some(epoch_now());
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

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Los nombres de los informes guardados, ordenados.
pub fn list() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .path()
                .extension()?
                .to_str()?
                .to_string()
                .eq("toml")
                .then(|| {
                    entry
                        .path()
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(str::to_string)
                })
        })
        .flatten()
        .collect();
    names.sort();
    names
}

/// Un informe guardado, como lo necesita la pantalla de abrir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: SavedKind,
    /// La base contra la que se guardo. Un informe de otra base no se puede abrir con el
    /// modelo de esta: los nombres resolverian a columnas distintas o a ninguna.
    pub database: String,
    /// Hora de la ultima apertura o guardado.
    pub modified_at: Option<i64>,
    /// Origen de datos que se necesita para abrir el informe.
    pub source_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedKind {
    Report,
    Project,
}

/// Los informes guardados con su base.
///
/// Un archivo ilegible se SALTA: una pantalla de abrir que no abre porque uno de los
/// informes esta roto no sirve para lo unico que hace.
pub fn index() -> Vec<SavedEntry> {
    let recent_path_times = recent_path_times(&recent_file_path());
    let legacy_recent_times = recent_times(&recent_file_path());
    let mut entries: Vec<SavedEntry> = list()
        .into_iter()
        .filter_map(|name| {
            let path = folder().join(format!("{name}.toml"));
            let text = std::fs::read_to_string(&path).ok()?;
            let file: File = toml::from_str(&text).ok()?;
            let recent_at = [
                recent_path_times.get(&path).copied(),
                legacy_recent_times.get(&name).copied(),
                file.saved_at,
                modified_at(&path),
            ]
            .into_iter()
            .flatten()
            .max();
            Some(SavedEntry {
                name,
                path,
                kind: SavedKind::Report,
                database: file.database,
                modified_at: recent_at,
                source_key: file.source_key,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
}

/// Los informes recientes, con su ruta completa para abrir el archivo correcto.
pub fn recent_index() -> Vec<SavedEntry> {
    let recent_path = recent_file_path();
    let file = std::fs::read_to_string(&recent_path)
        .ok()
        .and_then(|text| toml::from_str::<RecentFile>(&text).ok())
        .unwrap_or_default();
    merge_recent_entries(index(), file)
}

fn merge_recent_entries(mut entries: Vec<SavedEntry>, file: RecentFile) -> Vec<SavedEntry> {
    for recent in file.reports {
        let Some(path) = recent.path else {
            continue;
        };
        let Some(mut entry) = entry_from_path(&path, Some(recent.opened_at)) else {
            continue;
        };
        if let Some(existing) = entries.iter_mut().find(|existing| existing.path == path) {
            existing.modified_at = entry.modified_at;
        } else {
            entry.modified_at = Some(recent.opened_at.max(entry.modified_at.unwrap_or_default()));
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    entries
}

fn entry_from_path(path: &Path, opened_at: Option<i64>) -> Option<SavedEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let document: toml::Value = toml::from_str(&text).ok()?;
    let fields = document.as_table()?;
    let (kind, name, database) =
        if let Some(database) = fields.get("database").and_then(|v| v.as_str()) {
            (
                SavedKind::Report,
                fields
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| path.file_stem()?.to_str().map(str::to_string))?,
                database.to_string(),
            )
        } else if fields
            .get("format_version")
            .and_then(|v| v.as_integer())
            .is_some()
            && fields.get("title").and_then(|v| v.as_str()).is_some()
            && fields.get("pages").and_then(|v| v.as_array()).is_some()
        {
            (
                SavedKind::Project,
                fields
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| path.file_stem()?.to_str().map(str::to_string))?,
                String::new(),
            )
        } else {
            return None;
        };
    let modified_at = [
        opened_at,
        fields.get("saved_at").and_then(|value| value.as_integer()),
        modified_at(path),
    ]
    .into_iter()
    .flatten()
    .max();
    Some(SavedEntry {
        name,
        path: path.to_path_buf(),
        kind,
        database,
        modified_at,
        source_key: fields
            .get("source_key")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
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
    open_view_from_path(&path, model)
}

/// Abre un informe desde la ruta que el usuario eligió.
pub fn open_view_from_path(
    path: &Path,
    model: &datan_model::Model,
) -> Result<(Report, SavedView), SavedError> {
    let path = path.to_path_buf();
    let text = std::fs::read_to_string(&path).map_err(|source| SavedError::Read {
        path: path.clone(),
        source,
    })?;
    let file: File = toml::from_str(&text).map_err(|source| SavedError::Parse {
        path: path.clone(),
        source,
    })?;

    let database = file.database.clone();
    let dimensions = file
        .dimensions
        .iter()
        .map(|dimension| {
            SavedColumnRef::Legacy(dimension.clone())
                .resolve(model)
                .ok_or_else(|| SavedError::UnknownDimension {
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

    let mut saved_predicates = Vec::new();
    for filter in file
        .filters
        .iter()
        .filter(|filter| filter.operator.is_none())
    {
        let column_ref = filter.table.clone().map_or_else(
            || SavedColumnRef::Legacy(filter.column.clone()),
            |table| SavedColumnRef::Qualified {
                table,
                column: filter.column.clone(),
            },
        );
        let Some(column) = column_ref.resolve(&model) else {
            continue;
        };
        let data_type = model.column(column).map(|(_, c)| c.data_type);
        let values = filter
            .values
            .iter()
            .map(|value| parse_filter_value(value, data_type))
            .collect();
        // Legacy value selections remain a single IN filter per column.
        report = report.filtered_to(column, values);
    }

    for filter in file
        .filters
        .iter()
        .filter(|filter| filter.operator.is_some())
    {
        let column_ref = filter.table.clone().map_or_else(
            || SavedColumnRef::Legacy(filter.column.clone()),
            |table| SavedColumnRef::Qualified {
                table,
                column: filter.column.clone(),
            },
        );
        let Some(column) = column_ref.resolve(&model) else {
            continue;
        };
        let data_type = model.column(column).map(|(_, c)| c.data_type);
        let Some(operator) = filter.operator else {
            continue;
        };
        let values = filter
            .values
            .iter()
            .map(|value| parse_filter_value(value, data_type))
            .collect::<Vec<_>>();

        match (operator, values.as_slice()) {
            (SavedFilterOperator::Between, [lower, upper]) => {
                saved_predicates.push(Filter::new(
                    column,
                    FilterOperator::GreaterOrEqual,
                    lower.clone(),
                ));
                saved_predicates.push(Filter::new(
                    column,
                    FilterOperator::LessOrEqual,
                    upper.clone(),
                ));
            }
            (SavedFilterOperator::Between, _) => {}
            (operator, [value]) => saved_predicates.push(Filter::new(
                column,
                FilterOperator::from(operator),
                value.clone(),
            )),
            (_, _) => {}
        }
    }
    report.filters = saved_predicates
        .into_iter()
        .fold(report.filters.clone(), |context, filter| {
            context.with(filter)
        });

    let view = SavedView {
        visual: file.visual,
        period_months: file.period_months,
        top_n: file.top_n,
        formato: file.formato,
        pages: file.pages,
        active_page: file.active_page,
        report_filters: file.report_filters,
        panels: file.panels,
        saved_at: file.saved_at.or_else(|| modified_at(&path)),
        data_scope: file.data_scope,
        source_key: file.source_key,
        loaded_queries: file.loaded_queries,
        model_positions: file.model_positions,
    };

    Ok((report, view))
}

/// Lee las claves de origen antes de resolver las columnas guardadas del informe.
pub fn report_data_metadata(
    name: &str,
) -> Result<(Option<String>, Option<String>, Vec<SavedSourceTable>), SavedError> {
    let path = folder().join(file_name(name)?);
    report_data_metadata_from_path(&path)
}

/// Lee las claves del origen desde un archivo de informe.
pub fn report_data_metadata_from_path(
    path: &Path,
) -> Result<(Option<String>, Option<String>, Vec<SavedSourceTable>), SavedError> {
    let path = path.to_path_buf();
    let text = std::fs::read_to_string(&path).map_err(|source| SavedError::Read {
        path: path.clone(),
        source,
    })?;
    let file: File = toml::from_str(&text).map_err(|source| SavedError::Parse {
        path: path.clone(),
        source,
    })?;
    Ok((file.data_scope, file.source_key, file.loaded_queries))
}

fn modified_at(path: &std::path::Path) -> Option<i64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

/// El texto de un filtro al valor que era.
///
/// Se guarda con `{value:?}` por rapidez, y se lee con la regla del tipo de la columna. Un
/// numero guardado como texto se parsea; un texto se queda igual. Guardar el tipo al lado
/// seria mas limpio y mas codigo para un caso que hoy no existe.
pub fn parse_filter_value(text: &str, data_type: Option<DataType>) -> FilterValue {
    // El `BLANK` se guarda como `Blank` y no depende del tipo: un grupo sin valor en una
    // columna de texto tambien es BLANK, y leerlo como el texto "Blank" creaba un filtro que
    // no coincidia con ninguna fila.
    if text == "Blank" {
        return FilterValue::Blank;
    }
    let typed_value = |kind: &str| {
        text.strip_prefix(&format!("{kind}("))
            .and_then(|rest| rest.strip_suffix(')'))
    };
    match data_type {
        Some(DataType::Date | DataType::DateTime) => {
            let day = typed_value("Date").unwrap_or(text);
            FilterValue::Date(datan_dax_eval::parse_day(day).unwrap_or(0))
        }
        Some(DataType::Integer | DataType::Moneda) => {
            FilterValue::Integer(typed_value("Integer").unwrap_or(text).parse().unwrap_or(0))
        }
        Some(DataType::Decimal) => FilterValue::Decimal(
            typed_value("Decimal")
                .unwrap_or(text)
                .parse()
                .unwrap_or(0.0),
        ),
        Some(DataType::Boolean) => {
            FilterValue::Boolean(typed_value("Boolean").unwrap_or(text) == "true")
        }
        _ => {
            let trimmed = typed_value("Text")
                .unwrap_or(text)
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(text);
            FilterValue::Text(trimmed.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reglas_de_color_usan_primer_rango_coincidente_y_ignoran_limites_invalidos() {
        let rules = SavedConditionalColorRules {
            rules: vec![
                SavedConditionalColorRule {
                    condition: SavedColorRuleCondition::LessThan("0".into()),
                    color: 0x12dc2626,
                },
                SavedConditionalColorRule {
                    condition: SavedColorRuleCondition::BetweenInclusive {
                        minimum: "0".into(),
                        maximum: "10".into(),
                    },
                    color: 0x16a34a,
                },
            ],
        };

        assert_eq!(rules.color_for(-1.0), Some(0xdc2626));
        assert_eq!(rules.color_for(0.0), Some(0x16a34a));
        assert_eq!(rules.color_for(10.0), Some(0x16a34a));
        assert_eq!(rules.color_for(11.0), None);
        assert_eq!(rules.color_for(f64::NAN), None);
        assert!(
            !SavedColorRuleCondition::BetweenInclusive {
                minimum: "10".into(),
                maximum: "0".into(),
            }
            .matches(5.0)
        );
        assert!(!SavedColorRuleCondition::AtLeast("NaN".into()).matches(1.0));
    }

    #[test]
    fn reglas_condicionales_se_guardan_y_abren_en_el_visual() {
        let visual = SavedVisual {
            conditional_color_source: Some(SavedColorMetric::Measure("Ingresos".into())),
            conditional_color_rules: Some(SavedConditionalColorRules {
                rules: vec![SavedConditionalColorRule {
                    condition: SavedColorRuleCondition::AtLeast("100".into()),
                    color: 0x2457d6,
                }],
            }),
            ..SavedVisual::default()
        };

        let encoded = toml::Value::try_from(visual.clone()).expect("serializa");
        let restored: SavedVisual = encoded.try_into().expect("abre");
        assert_eq!(
            restored.conditional_color_rules,
            visual.conditional_color_rules
        );

        let legacy: SavedVisual = toml::from_str(r#"kind = "Columnas""#).expect("lee legacy");
        assert_eq!(legacy.conditional_color_rules, None);
    }

    #[test]
    fn regla_nueva_se_inserta_antes_del_fallback_y_acepta_decimales_con_coma() {
        let mut rules = SavedConditionalColorRules {
            rules: vec![
                SavedConditionalColorRule {
                    condition: SavedColorRuleCondition::LessThan("0".into()),
                    color: 0xdc2626,
                },
                SavedConditionalColorRule {
                    condition: SavedColorRuleCondition::AtLeast("0".into()),
                    color: 0x16a34a,
                },
            ],
        };
        rules.insert_before_fallback(SavedConditionalColorRule {
            condition: SavedColorRuleCondition::BetweenInclusive {
                minimum: "1,5".into(),
                maximum: "2,5".into(),
            },
            color: 0x2457d6,
        });

        assert_eq!(rules.color_for(2.0), Some(0x2457d6));
        assert_eq!(rules.color_for(3.0), Some(0x16a34a));
    }

    #[test]
    fn un_visual_legacy_se_carga_sin_leyenda() {
        let mut value = toml::Value::try_from(SavedVisual::default()).unwrap();
        value.as_table_mut().unwrap().remove("legend");
        let legacy: SavedVisual = value.try_into().unwrap();
        assert_eq!(legacy.legend, None);
    }

    #[test]
    fn colores_de_series_se_guardan_por_etiqueta_y_el_legacy_usa_paleta() {
        let mut visual = SavedVisual::default();
        visual.series_colors.insert("Norte".into(), 0x123456);
        let encoded = toml::Value::try_from(visual).unwrap();
        let restored: SavedVisual = encoded.try_into().unwrap();
        assert_eq!(restored.series_colors.get("Norte"), Some(&0x123456));

        let mut legacy = toml::Value::try_from(SavedVisual::default()).unwrap();
        legacy.as_table_mut().unwrap().remove("series_colors");
        let restored: SavedVisual = legacy.try_into().unwrap();
        assert!(restored.series_colors.is_empty());
    }

    #[test]
    fn roles_secundarios_del_visual_se_guardan_y_el_legacy_usa_vacios() {
        let mut visual = SavedVisual::default();
        visual.roles.insert(
            SavedVisualRole::SecondaryY,
            vec![SavedVisualField::Measure("Margen".into())],
        );
        let encoded = toml::Value::try_from(visual.clone()).expect("serializa");
        let restored: SavedVisual = encoded.try_into().expect("abre");
        assert_eq!(restored.roles, visual.roles);

        let mut legacy = toml::Value::try_from(SavedVisual::default()).unwrap();
        legacy.as_table_mut().unwrap().remove("roles");
        let restored: SavedVisual = legacy.try_into().unwrap();
        assert!(restored.roles.is_empty());
    }


    #[test]
    fn lineas_analiticas_se_guardan_y_los_informes_anteriores_usan_valores_por_defecto() {
        let format = SavedFormato {
            linea_minimo: true,
            linea_maximo: true,
            linea_constante: true,
            valor_linea_constante_milesimas: 123_400,
            ..SavedFormato::default()
        };
        let encoded = toml::Value::try_from(format).expect("serializa");
        let restored: SavedFormato = encoded.try_into().expect("abre");
        assert!(restored.linea_minimo);
        assert!(restored.linea_maximo);
        assert!(restored.linea_constante);
        assert_eq!(restored.valor_linea_constante_milesimas, 123_400);

        let legacy: SavedFormato = toml::from_str("titulo = true").expect("lee legacy");
        assert!(!legacy.linea_minimo);
        assert!(!legacy.linea_maximo);
        assert!(!legacy.linea_constante);
        assert_eq!(legacy.valor_linea_constante_milesimas, 0);
    }

    #[test]
    fn formato_de_tabla_se_guarda_y_conserva_valores_anteriores() {
        let format = SavedFormato {
            densidad_tabla: SavedTableDensity::Comfortable,
            filas_alternas: true,
            ..SavedFormato::default()
        };
        let encoded = toml::Value::try_from(format).expect("serializa");
        let restored: SavedFormato = encoded.try_into().expect("abre");
        assert_eq!(restored.densidad_tabla, SavedTableDensity::Comfortable);
        assert!(restored.filas_alternas);

        let legacy: SavedFormato = toml::from_str("titulo = true").expect("lee legacy");
        assert_eq!(legacy.densidad_tabla, SavedTableDensity::Normal);
        assert!(!legacy.filas_alternas);
    }

    #[test]
    fn ejes_y_rejilla_migran_el_eje_legacy_y_guardan_estados_independientes() {
        let legacy: SavedFormato = toml::from_str("eje = false").expect("lee legacy");
        assert!(legacy.eje_x);
        assert!(!legacy.eje);
        assert!(!legacy.muestra_rejilla_graficos());

        let format = SavedFormato {
            eje: false,
            eje_x: false,
            rejilla_graficos: Some(true),
            ..SavedFormato::default()
        };
        let encoded = toml::Value::try_from(format).expect("serializa");
        let restored: SavedFormato = encoded.clone().try_into().expect("abre");
        assert!(!restored.eje);
        assert!(!restored.eje_x);
        assert!(restored.muestra_rejilla_graficos());
        assert_eq!(encoded["rejilla_graficos"].as_bool(), Some(true));
    }

    #[test]
    fn cambiar_eje_de_valores_no_cambia_la_rejilla() {
        let mut format = SavedFormato::default();
        format.set_eje_valores(false);
        assert!(!format.eje);
        assert!(format.muestra_rejilla_graficos());

        let mut legacy: SavedFormato = toml::from_str("eje = false").expect("lee legacy");
        legacy.set_eje_valores(true);
        assert!(legacy.eje);
        assert!(!legacy.muestra_rejilla_graficos());
    }

    #[test]
    fn radio_interior_del_anillo_se_guarda_y_legacy_usa_el_default() {
        let formato = SavedFormato {
            radio_interior_anillo: 88,
            ..SavedFormato::default()
        };
        let texto = toml::to_string(&formato).expect("serializa formato");
        let guardado: SavedFormato = toml::from_str(&texto).expect("lee formato");
        assert_eq!(guardado.radio_interior_anillo, 88);

        let anterior: SavedFormato = toml::from_str("titulo = true").expect("lee legacy");
        assert_eq!(anterior.radio_interior_anillo, 64);
    }

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
        let folder =
            std::env::temp_dir().join(format!("datan-informes-{}-{nombre}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("crea la carpeta");
        folder
    }

    #[test]
    fn el_guardado_atomico_reemplaza_el_archivo_completo() {
        let folder = carpeta_de_prueba("guardado-atomico");
        let path = folder.join("ventas.toml");
        std::fs::write(&path, "version anterior").expect("escribe el informe anterior");

        write_atomically(&path, "version nueva").expect("reemplaza el informe");

        assert_eq!(
            std::fs::read_to_string(&path).expect("lee el informe"),
            "version nueva"
        );
        assert_eq!(
            std::fs::read_dir(&folder).expect("lee la carpeta").count(),
            1
        );
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn un_fallo_al_reemplazar_conserva_el_informe_anterior_y_limpia_el_temporal() {
        let folder = carpeta_de_prueba("fallo-guardado-atomico");
        let path = folder.join("ventas.toml");
        std::fs::write(&path, "version anterior").expect("escribe el informe anterior");

        let result = write_atomically_with(&path, "version nueva", |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fallo de prueba",
            ))
        });

        assert_eq!(
            result.expect_err("el reemplazo debe fallar").kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("lee el informe"),
            "version anterior"
        );
        assert_eq!(
            std::fs::read_dir(&folder).expect("lee la carpeta").count(),
            1
        );
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn abrir_un_informe_lo_sube_en_la_lista_reciente() {
        let path = carpeta_de_prueba("recientes").join("recent-reports.toml");
        record_opened_at(&path, "ventas", None, 10);
        record_opened_at(&path, "clientes", None, 20);
        record_opened_at(&path, "ventas", None, 30);

        let times = recent_times(&path);

        assert_eq!(times.get("ventas"), Some(&30));
        assert_eq!(times.get("clientes"), Some(&20));
        let _ = std::fs::remove_dir_all(path.parent().expect("carpeta de prueba"));
    }

    #[test]
    fn recientes_distingue_archivos_con_el_mismo_nombre_y_detecta_sus_formatos() {
        let folder = carpeta_de_prueba("rutas-recientes");
        let project_path = folder.join("proyecto").join("Informe.dtx");
        let report_path = folder.join("reporte").join("Informe.dtx");
        std::fs::create_dir_all(project_path.parent().expect("carpeta proyecto"))
            .expect("crea carpeta proyecto");
        std::fs::create_dir_all(report_path.parent().expect("carpeta informe"))
            .expect("crea carpeta informe");
        std::fs::write(
            &project_path,
            "format_version = 1\ntitle = 'Informe'\npages = []\nactive_page = 0\n",
        )
        .expect("escribe proyecto");
        std::fs::write(
            &report_path,
            "name = 'Informe'\ndatabase = 'pg'\ndimensions = []\nmeasures = []\n",
        )
        .expect("escribe informe");
        let recent_path = folder.join("recent-reports.toml");
        record_opened_at(&recent_path, "Informe", Some(&project_path), 10);
        record_opened_at(&recent_path, "Informe", Some(&report_path), 20);
        let file = std::fs::read_to_string(&recent_path)
            .ok()
            .and_then(|text| toml::from_str::<RecentFile>(&text).ok())
            .expect("lee rutas recientes");

        let entries = merge_recent_entries(Vec::new(), file);

        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|entry| { entry.path == project_path && entry.kind == SavedKind::Project })
        );
        assert!(
            entries
                .iter()
                .any(|entry| { entry.path == report_path && entry.kind == SavedKind::Report })
        );
        let _ = std::fs::remove_dir_all(folder);
    }

    fn informe_de_prueba(nombre: &str) -> File {
        File {
            name: nombre.to_string(),
            database: "pg".to_string(),
            data_scope: None,
            source_key: None,
            loaded_queries: Vec::new(),
            model_positions: BTreeMap::new(),
            saved_at: None,
            dimensions: vec!["region".to_string()],
            measures: vec!["Qty".to_string()],
            filters: vec![],
            report_filters: vec![],
            panels: SavedPanels::default(),
            visual: None,
            period_months: None,
            top_n: None,
            formato: BTreeMap::new(),
            pages: Vec::new(),
            active_page: 0,
        }
    }

    #[test]
    fn los_metadatos_del_origen_y_la_fecha_se_conservan_al_guardar_y_abrir() {
        let folder = carpeta_de_prueba("metadatos-origen");
        let path = folder.join("ventas.toml");
        let model = datan_testkit::model();
        let report = Report::new(model.clone())
            .attached_to("pg")
            .with_dimensions(vec![datan_testkit::PRODUCT_REGION]);
        let view = SavedView {
            data_scope: Some("informe-ventas".to_string()),
            source_key: Some("postgres-ventas".to_string()),
            loaded_queries: vec![SavedSourceTable {
                table: "dim_product".to_string(),
                source_sql: "SELECT region FROM dim_product".to_string(),
                columns: vec!["region".to_string()],
                column_types: vec!["Text".to_string()],
            }],
            model_positions: BTreeMap::from([(
                "dim_product".to_string(),
                SavedModelPosition { x: 120, y: 240 },
            )]),
            saved_at: Some(1_700_000_000),
            ..SavedView::default()
        };

        save_view_to(&report, "ventas", &path, &view).expect("guarda el informe");

        let file: File = toml::from_str(&std::fs::read_to_string(&path).expect("lee el archivo"))
            .expect("lee el informe");
        assert_eq!(file.data_scope, view.data_scope);
        assert_eq!(file.source_key, view.source_key);
        assert_eq!(file.loaded_queries, view.loaded_queries);
        assert_eq!(file.model_positions, view.model_positions);
        assert_eq!(file.saved_at, view.saved_at);
        let (reopened, reopened_view) =
            open_view_from_path(&path, &model).expect("abre el informe");
        assert_eq!(reopened.dimensions, report.dimensions);
        assert_eq!(reopened_view, view);
        assert_eq!(
            report_data_metadata_from_path(&path).expect("lee los metadatos"),
            (view.data_scope, view.source_key, view.loaded_queries)
        );

        let _ = std::fs::remove_dir_all(&folder);
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
        let value = parse_filter_value("Text(\"Norte\")", Some(DataType::Text));
        assert_eq!(value, FilterValue::Text("Norte".to_string()));
    }

    #[test]
    fn un_texto_sin_envoltorio_tambien_se_lee() {
        let value = parse_filter_value("Norte", Some(DataType::Text));
        assert_eq!(value, FilterValue::Text("Norte".to_string()));
    }

    #[test]
    fn un_numero_se_parsea_al_tipo_de_su_columna() {
        assert_eq!(
            parse_filter_value("60", Some(DataType::Integer)),
            FilterValue::Integer(60)
        );
        assert_eq!(
            parse_filter_value("60.5", Some(DataType::Decimal)),
            FilterValue::Decimal(60.5)
        );
        assert_eq!(
            parse_filter_value("Integer(1000)", Some(DataType::Moneda)),
            FilterValue::Integer(1000)
        );
        assert_eq!(
            parse_filter_value("Decimal(60.5)", Some(DataType::Decimal)),
            FilterValue::Decimal(60.5)
        );
    }

    #[test]
    fn un_booleano_se_lee_como_booleano() {
        assert_eq!(
            parse_filter_value("true", Some(DataType::Boolean)),
            FilterValue::Boolean(true)
        );
        assert_eq!(
            parse_filter_value("false", Some(DataType::Boolean)),
            FilterValue::Boolean(false)
        );
        assert_eq!(
            parse_filter_value("Boolean(true)", Some(DataType::Boolean)),
            FilterValue::Boolean(true)
        );
    }

    #[test]
    fn una_fecha_y_fecha_hora_se_leen_como_dia() {
        assert_eq!(
            parse_filter_value("Date(100)", Some(DataType::DateTime)),
            FilterValue::Date(100)
        );
        assert_eq!(
            parse_filter_value("2026-10-25", Some(DataType::DateTime)),
            FilterValue::Date(datan_dax_eval::parse_day("2026-10-25").expect("fecha"))
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
        assert!(file.pages.is_empty());
        assert_eq!(file.active_page, 0);
        assert!(file.report_filters.is_empty());
    }

    #[test]
    fn las_paginas_se_serializan_con_sus_selecciones_y_vistas() {
        let page = SavedPage {
            name: "Detalle".to_string(),
            canvas_size: CanvasSize::default(),
            active_visual: "visual-ventas".to_string(),
            dimensions: vec![SavedColumnRef::Qualified {
                table: "dim_product".to_string(),
                column: "region".to_string(),
            }],
            measures: vec!["Qty".to_string()],
            filters: vec![SavedFilter {
                column: "region".to_string(),
                table: Some("dim_product".to_string()),
                values: vec!["Text(\"Norte\")".to_string(), "Text(\"Sur\")".to_string()],
                operator: None,
            }],
            visual: Some("Barras".to_string()),
            period_months: Some(6),
            top_n: Some(SavedTopN {
                measure: "Qty".to_string(),
                count: 5,
                abajo: true,
            }),
            formato: BTreeMap::from([(
                "Barras".to_string(),
                SavedFormato {
                    titulo: false,
                    ..SavedFormato::default()
                },
            )]),
            sort: Some(SavedSort {
                column_index: 2,
                descending: true,
            }),
            zoom_percent: Some(125),
            visuals: vec![
                SavedVisual {
                    id: "visual-ventas".to_string(),
                    title: "Pedidos".to_string(),
                    position: SavedPosition { x: 12, y: 24 },
                    size: SavedSize {
                        width: 480,
                        height: 320,
                    },
                    kind: "Barras".to_string(),
                    dimensions: vec![SavedColumnRef::Qualified {
                        table: "dim_product".to_string(),
                        column: "region".to_string(),
                    }],
                    measures: vec!["Qty".to_string()],
                    filters: vec![SavedFilter {
                        column: "region".to_string(),
                        table: Some("dim_product".to_string()),
                        values: vec!["Text(\"Norte\")".to_string()],
                        operator: None,
                    }],
                    top_n: Some(SavedTopN {
                        measure: "Qty".to_string(),
                        count: 3,
                        abajo: false,
                    }),
                    sort: Some(SavedSort {
                        column_index: 1,
                        descending: true,
                    }),
                    period_months: Some(12),
                    formato: SavedFormato {
                        etiquetas: false,
                        ..SavedFormato::default()
                    },
                    ..SavedVisual::default()
                },
                SavedVisual {
                    id: "visual-total".to_string(),
                    position: SavedPosition { x: 520, y: 24 },
                    size: SavedSize {
                        width: 240,
                        height: 160,
                    },
                    visible: false,
                    kind: "Tarjetas".to_string(),
                    measures: vec!["Amount".to_string()],
                    ..SavedVisual::default()
                },
            ],
            bookmarks: vec![SavedBookmark {
                name: "Ventas Norte".to_string(),
                active_visual: "visual-total".to_string(),
                filters: vec![SavedFilter {
                    column: "region".to_string(),
                    table: Some("dim_product".to_string()),
                    values: vec!["Text(\"Norte\")".to_string()],
                    operator: None,
                }],
                visuals: vec![SavedVisual {
                    id: "visual-total".to_string(),
                    kind: "Tarjetas".to_string(),
                    ..SavedVisual::default()
                }],
            }],
        };
        let summary = SavedPage {
            name: "Resumen".to_string(),
            dimensions: vec![SavedColumnRef::Qualified {
                table: "dim_date".to_string(),
                column: "day".to_string(),
            }],
            filters: vec![SavedFilter {
                column: "day".to_string(),
                table: Some("dim_date".to_string()),
                values: vec!["Date(19723)".to_string()],
                operator: None,
            }],
            ..SavedPage::default()
        };
        let mut file = informe_de_prueba("ventas");
        file.pages = vec![page.clone(), summary.clone()];
        file.active_page = 1;
        file.report_filters = vec![SavedFilter {
            column: "pais".to_string(),
            table: None,
            values: vec!["Text(\"Chile\")".to_string()],
            operator: None,
        }];

        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(text.contains("[[pagina]]"), "{text}");
        assert!(text.contains("active_visual = \"visual-ventas\""), "{text}");
        let back: File = toml::from_str(&text).expect("parsea");

        assert_eq!(back.pages, vec![page, summary]);
        assert_eq!(back.pages[0].bookmarks.len(), 1);
        assert_eq!(back.pages[0].visuals[0].title, "Pedidos");
        assert_eq!(back.pages[0].bookmarks[0].active_visual, "visual-total");
        assert_eq!(
            back.pages[0].bookmarks[0].filters[0].values,
            vec!["Text(\"Norte\")"]
        );
        assert_eq!(back.active_page, 1);
        assert_eq!(back.report_filters, file.report_filters);
    }

    #[test]
    fn la_vista_vieja_usa_la_primera_pagina_y_sin_filtros_de_informe() {
        let text = r#"
visual = "Tabla"
"#;
        let view: SavedView = toml::from_str(text).expect("lee la vista vieja");

        assert_eq!(view.active_page, 0);
        assert!(view.report_filters.is_empty());
        assert_eq!(view.panels, SavedPanels::default());
    }

    #[test]
    fn la_visibilidad_de_los_paneles_se_guarda_y_el_default_no_se_escribe() {
        let mut file = informe_de_prueba("ventas");
        let default_text = toml::to_string_pretty(&file).expect("serializa");
        assert!(!default_text.contains("[panels]"), "{default_text}");

        file.panels = SavedPanels {
            datos: false,
            filtros: true,
            visualizaciones: true,
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(text.contains("[panels]"), "{text}");
        assert!(text.contains("datos = false"), "{text}");
        assert!(!text.contains("filtros = true"), "{text}");
        assert!(!text.contains("visualizaciones = true"), "{text}");
        let reopened: File = toml::from_str(&text).expect("abre");
        assert_eq!(reopened.panels, file.panels);
    }

    #[test]
    fn all_panel_visibility_combinations_roundtrip() {
        for datos in [false, true] {
            for filtros in [false, true] {
                for visualizaciones in [false, true] {
                    let mut file = informe_de_prueba("ventas");
                    file.panels = SavedPanels {
                        datos,
                        filtros,
                        visualizaciones,
                    };
                    let text = toml::to_string_pretty(&file).expect("serialize report");
                    let reopened: File = toml::from_str(&text).expect("read report");
                    assert_eq!(reopened.panels, file.panels);
                }
            }
        }
    }

    #[test]
    fn old_panel_visibility_keeps_filters_visible() {
        let panels: SavedPanels =
            toml::from_str("datos = false\nvisualizaciones = false").expect("read old panels");
        assert_eq!(
            panels,
            SavedPanels {
                datos: false,
                filtros: true,
                visualizaciones: false,
            }
        );
    }

    #[test]
    fn modo_desplegable_se_guarda_y_el_informe_anterior_usa_lista() {
        let previous: SavedVisual =
            toml::from_str(r#"kind = "Segmentador""#).expect("lee el visual anterior");
        assert_eq!(previous.slicer_mode, SavedSlicerMode::List);
        assert_eq!(previous.date_range, None);
        let list_text = toml::to_string(&previous).expect("serializa la lista");
        assert!(!list_text.contains("slicer_mode"), "{list_text}");

        let visual = SavedVisual {
            kind: "Segmentador".to_string(),
            slicer_mode: SavedSlicerMode::Dropdown,
            ..SavedVisual::default()
        };
        let text = toml::to_string(&visual).expect("serializa");
        assert!(text.contains("slicer_mode = \"dropdown\""), "{text}");
        let reopened: SavedVisual = toml::from_str(&text).expect("abre");
        assert_eq!(reopened.slicer_mode, SavedSlicerMode::Dropdown);

        let ranged = SavedVisual {
            kind: "Segmentador".to_string(),
            slicer_mode: SavedSlicerMode::DateRange,
            date_range: Some(SavedDateRange {
                start: 19_723,
                end: 19_753,
            }),
            ..SavedVisual::default()
        };
        let text = toml::to_string(&ranged).expect("serializa el rango");
        let reopened: SavedVisual = toml::from_str(&text).expect("abre el rango");
        assert_eq!(reopened.slicer_mode, SavedSlicerMode::DateRange);
        assert_eq!(reopened.date_range, ranged.date_range);
    }

    #[test]
    fn division_de_matriz_se_guarda_y_el_archivo_anterior_conserva_su_diseno() {
        let previous: SavedVisual =
            toml::from_str("kind = \"Matriz\"").expect("lee el visual anterior");
        assert_eq!(previous.matrix_row_count, None);

        let visual = SavedVisual {
            kind: "Matriz".to_string(),
            dimensions: vec![
                SavedColumnRef::Legacy("region".to_string()),
                SavedColumnRef::Legacy("year".to_string()),
                SavedColumnRef::Legacy("month".to_string()),
            ],
            matrix_row_count: Some(2),
            ..SavedVisual::default()
        };
        let text = toml::to_string(&visual).expect("serializa");
        assert!(text.contains("matrix_row_count = 2"), "{text}");
        let reopened: SavedVisual = toml::from_str(&text).expect("lee");
        assert_eq!(reopened.matrix_row_count, Some(2));
    }

    #[test]
    fn seleccion_del_segmentador_se_guarda_y_distingue_archivos_anteriores() {
        let previous: SavedVisual =
            toml::from_str("kind = \"Segmentador\"").expect("lee el visual anterior");
        assert_eq!(previous.slicer_selection, None);

        let visual = SavedVisual {
            kind: "Segmentador".to_string(),
            slicer_selection: Some(vec![SavedFilter {
                column: "region".to_string(),
                table: Some("dim_product".to_string()),
                values: vec!["Text(\"Norte\")".to_string()],
                operator: None,
            }]),
            ..SavedVisual::default()
        };
        let text = toml::to_string(&visual).expect("serializa");
        let reopened: SavedVisual = toml::from_str(&text).expect("lee");
        assert_eq!(reopened.slicer_selection, visual.slicer_selection);
    }

    #[test]
    fn agregaciones_implicitas_se_guardan_dentro_del_visual() {
        let visual = SavedVisual {
            kind: "KPI".to_string(),
            implicit_measures: vec![SavedImplicitMeasure {
                column: SavedColumnRef::Qualified {
                    table: "Ventas".to_string(),
                    column: "Cliente".to_string(),
                },
                aggregation: SavedAggregation::CountRows,
                table: Some("Ventas".to_string()),
            }],
            value_order: vec![
                SavedValueOrder::Implicit(SavedColumnRef::Qualified {
                    table: "Ventas".to_string(),
                    column: "Cliente".to_string(),
                }),
                SavedValueOrder::Measure("Ingresos existentes".to_string()),
            ],
            ..SavedVisual::default()
        };

        let text = toml::to_string(&visual).expect("serializa el visual");
        let reopened: SavedVisual = toml::from_str(&text).expect("lee el visual");

        assert_eq!(reopened.implicit_measures, visual.implicit_measures);
        assert_eq!(reopened.value_order, visual.value_order);
    }

    #[test]
    fn agregaciones_de_categorias_y_fechas_se_guardan_y_se_recuperan() {
        let column = SavedColumnRef::Qualified {
            table: "Ventas".to_string(),
            column: "Fecha".to_string(),
        };
        let visual = SavedVisual {
            kind: "KPI".to_string(),
            implicit_measures: [
                SavedAggregation::First,
                SavedAggregation::Last,
                SavedAggregation::Earliest,
                SavedAggregation::Latest,
            ]
            .into_iter()
            .map(|aggregation| SavedImplicitMeasure {
                column: column.clone(),
                aggregation,
                table: None,
            })
            .collect(),
            ..SavedVisual::default()
        };

        let text = toml::to_string(&visual).expect("serializa visual");
        let reopened: SavedVisual = toml::from_str(&text).expect("abre visual");

        assert_eq!(reopened.implicit_measures, visual.implicit_measures);
    }

    #[test]
    fn una_pagina_parcial_usa_los_valores_por_defecto() {
        let text = r#"
name = "ventas"
database = "pg"
dimensions = []
measures = []

[[pagina]]
name = "Resumen"
dimensions = ["day"]
"#;
        let file: File = toml::from_str(text).expect("parsea");

        assert_eq!(file.pages.len(), 1);
        assert_eq!(file.pages[0].name, "Resumen");
        assert_eq!(
            file.pages[0].dimensions,
            vec![SavedColumnRef::Legacy("day".to_string())]
        );
        assert!(file.pages[0].filters.is_empty());
        assert_eq!(file.pages[0].visual, None);
        assert!(file.pages[0].active_visual.is_empty());
        assert!(file.pages[0].visuals.is_empty());
        assert_eq!(file.pages[0].zoom_percent, None);
    }

    #[test]
    fn las_paginas_guardadas_con_el_nombre_provisional_siguen_abriendo() {
        let text = r#"
name = "ventas"
database = "pg"
dimensions = []
measures = []

[[pages]]
name = "Resumen"
"#;
        let file: File = toml::from_str(text).expect("lee pages como alias viejo");

        assert_eq!(file.pages.len(), 1);
        assert_eq!(file.pages[0].name, "Resumen");
    }

    #[test]
    fn la_referencia_calificada_resuelve_columnas_con_nombre_repetido() {
        let model = datan_testkit::model();
        let qualified = SavedColumnRef::Qualified {
            table: "dim_date".to_string(),
            column: "day".to_string(),
        };

        assert_eq!(qualified.resolve(&model), Some(datan_testkit::DATE_DAY));
        assert_eq!(
            SavedColumnRef::Legacy("day".to_string()).resolve(&model),
            Some(datan_testkit::FACT_DAY)
        );
    }

    #[test]
    fn un_filtro_con_varios_valores_da_la_vuelta() {
        let filter = SavedFilter {
            column: "region".to_string(),
            table: None,
            values: vec!["Text(\"Norte\")".to_string(), "Text(\"Sur\")".to_string()],
            operator: None,
        };
        let text = toml::to_string_pretty(&filter).expect("serializa");

        assert!(text.contains("Norte") && text.contains("Sur"), "{text}");

        let back: SavedFilter = toml::from_str(&text).expect("parsea");
        assert_eq!(back, filter);
    }

    #[test]
    fn los_filtros_legacy_con_value_o_values_siguen_abriendo() {
        let singular: SavedFilter = toml::from_str(
            r#"
column = "region"
value = 'Text("Norte")'
"#,
        )
        .expect("lee value legacy");
        let plural: SavedFilter = toml::from_str(
            r#"
column = "region"
values = ['Text("Norte")', 'Text("Sur")']
"#,
        )
        .expect("lee values legacy");

        assert_eq!(singular.values, vec![r#"Text("Norte")"#]);
        assert_eq!(singular.operator, None);
        assert_eq!(plural.values.len(), 2);
        assert_eq!(plural.operator, None);
    }

    #[test]
    fn los_operadores_de_comparacion_y_rango_se_serializan() {
        for filter in [
            SavedFilter {
                column: "year".to_string(),
                table: Some("calendar".to_string()),
                values: vec!["Integer(2020)".to_string()],
                operator: Some(SavedFilterOperator::GreaterOrEqual),
            },
            SavedFilter {
                column: "day".to_string(),
                table: Some("calendar".to_string()),
                values: vec!["Date(100)".to_string(), "Date(120)".to_string()],
                operator: Some(SavedFilterOperator::Between),
            },
        ] {
            let text = toml::to_string_pretty(&filter).expect("serializa");
            let back: SavedFilter = toml::from_str(&text).expect("lee el filtro");
            assert_eq!(back, filter);
        }
    }

    #[test]
    fn guardar_y_abrir_conserva_los_dos_limites_del_rango() {
        let folder = carpeta_de_prueba("rango-filtro");
        let path = folder.join("ventas.toml");
        let mut model = datan_testkit::model();
        model
            .tables
            .iter_mut()
            .find(|table| table.id == datan_testkit::DATE_TABLE)
            .and_then(|table| {
                table
                    .columns
                    .iter_mut()
                    .find(|column| column.id == datan_testkit::DATE_DAY)
            })
            .expect("columna de fecha")
            .data_type = DataType::DateTime;
        let mut report = Report::new(model.clone()).attached_to("pg");
        report.filters = report
            .filters
            .with_greater_equal(datan_testkit::DATE_DAY, FilterValue::Date(100))
            .with_less_equal(datan_testkit::DATE_DAY, FilterValue::Date(120))
            .with(Filter::new(
                datan_testkit::FACT_AMOUNT,
                FilterOperator::Greater,
                FilterValue::Integer(1000),
            ));

        save_view_to(&report, "ventas", &path, &SavedView::default()).expect("guarda");
        let file: File = toml::from_str(&std::fs::read_to_string(&path).expect("lee archivo"))
            .expect("lee filtro guardado");
        assert_eq!(file.filters.len(), 2);
        let date_range = file
            .filters
            .iter()
            .find(|filter| filter.operator == Some(SavedFilterOperator::Between))
            .expect("guarda rango inclusivo");
        assert_eq!(date_range.values, vec!["Date(100)", "Date(120)"]);
        assert!(file.filters.iter().any(|filter| {
            filter.column == "amount"
                && filter.operator == Some(SavedFilterOperator::Greater)
                && filter.values == ["Integer(1000)"]
        }));

        let (reopened, _) = open_view_from_path(&path, &model).expect("abre el informe");
        let predicates = reopened
            .filters
            .entries()
            .flat_map(|(_, filters)| filters.iter())
            .collect::<Vec<_>>();
        assert_eq!(predicates.len(), 3);
        assert!(predicates.iter().any(|filter| {
            filter.operator == FilterOperator::GreaterOrEqual
                && filter.values() == [FilterValue::Date(100)]
        }));
        assert!(predicates.iter().any(|filter| {
            filter.operator == FilterOperator::LessOrEqual
                && filter.values() == [FilterValue::Date(120)]
        }));
        assert!(predicates.iter().any(|filter| {
            filter.column == datan_testkit::FACT_AMOUNT
                && filter.operator == FilterOperator::Greater
                && filter.values() == [FilterValue::Integer(1000)]
        }));

        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn el_visual_y_el_periodo_solo_se_escriben_si_estan() {
        let file = File {
            name: "ventas".to_string(),
            database: "pg".to_string(),
            data_scope: None,
            source_key: None,
            loaded_queries: Vec::new(),
            model_positions: BTreeMap::new(),
            saved_at: None,
            dimensions: vec![],
            measures: vec![],
            filters: vec![],
            report_filters: vec![],
            panels: SavedPanels::default(),
            visual: None,
            period_months: None,
            top_n: None,
            formato: BTreeMap::new(),
            pages: Vec::new(),
            active_page: 0,
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
    fn los_formatos_anteriores_conservan_el_color_y_la_leyenda_por_default() {
        let texto = r#"
name = "ventas"
database = "pg"
dimensions = []
measures = []

[formato.Lineas]
titulo = false
"#;
        let file: File = toml::from_str(texto).expect("lee formato anterior");
        let formato = file.formato.get("Lineas").expect("visual");
        assert_eq!(formato.color_serie, 0);
        assert!(formato.leyenda);
        assert!(!formato.kpi_menor_es_mejor);
    }

    #[test]
    fn el_color_de_serie_se_guarda_y_se_recupera() {
        let mut formato = SavedFormato::default();
        formato.color_serie = 3;
        formato.color_serie_rgb = 0x0100_0000;
        let texto = toml::to_string(&formato).expect("serializa formato");
        let guardado: SavedFormato = toml::from_str(&texto).expect("lee formato");
        assert_eq!(guardado.color_serie, 3);
        assert_eq!(guardado.color_serie_rgb, 0x0100_0000);
    }

    #[test]
    fn conserva_el_estilo_y_subtitulo_del_visual() {
        let visual = SavedVisual {
            title: "Ingresos".to_string(),
            subtitle: "Por región".to_string(),
            formato: SavedFormato {
                escala_texto: 120,
                escala_titulo: 110,
                escala_subtitulo: 90,
                escala_valor: 140,
                color_texto: 0x1f2937,
                color_titulo: 0x2563eb,
                color_subtitulo: 0x64748b,
                color_valor: 0x059669,
                color_fondo: 0xf8fafc,
                color_borde: 0x2563eb,
                ancho_borde: 2,
                radio_borde: 8,
                fuente_mono: true,
                color_por_objetivo: true,
                color_estado_favorable: 0x16a34a,
                color_estado_desfavorable: 0xdc2626,
                ..SavedFormato::default()
            },
            ..SavedVisual::default()
        };
        let text = toml::to_string(&visual).expect("serializa el visual");
        let reopened: SavedVisual = toml::from_str(&text).expect("abre el visual");
        assert_eq!(reopened.subtitle, "Por región");
        assert_eq!(reopened.formato, visual.formato);
    }

    #[test]
    fn conserva_formatos_distintos_por_tipo_de_visual() {
        let visual = SavedVisual {
            kind: "Barras".to_string(),
            formatos: BTreeMap::from([
                (
                    "Barras".to_string(),
                    SavedFormato {
                        color_serie_rgb: 0x2457d6,
                        ..SavedFormato::default()
                    },
                ),
                (
                    "KPI".to_string(),
                    SavedFormato {
                        color_fondo: 0xf1f5f9,
                        escala_valor: 140,
                        ..SavedFormato::default()
                    },
                ),
            ]),
            ..SavedVisual::default()
        };
        let text = toml::to_string(&visual).expect("serializa estilos");
        let reopened: SavedVisual = toml::from_str(&text).expect("abre estilos");

        assert_eq!(reopened.formatos, visual.formatos);
    }

    #[test]
    fn conserva_agregaciones_implicitas_por_rol_del_visual() {
        let column = SavedColumnRef::Qualified {
            table: "Ventas".into(),
            column: "Importe".into(),
        };
        let sum = SavedImplicitMeasure {
            column: column.clone(),
            aggregation: SavedAggregation::Sum,
            table: None,
        };
        let average = SavedImplicitMeasure {
            column,
            aggregation: SavedAggregation::Average,
            table: None,
        };
        let visual = SavedVisual {
            value_order: vec![
                SavedValueOrder::ImplicitMeasure(sum.clone()),
                SavedValueOrder::ImplicitMeasure(average.clone()),
            ],
            gauge_fields: SavedGaugeFields {
                value: Some(SavedGaugeField::ImplicitMeasure(sum)),
                target: Some(SavedGaugeField::ImplicitMeasure(average)),
                ..SavedGaugeFields::default()
            },
            ..SavedVisual::default()
        };

        let serialized = toml::to_string(&visual).expect("serializa visual");
        let reopened: SavedVisual = toml::from_str(&serialized).expect("abre visual");

        assert_eq!(reopened.value_order, visual.value_order);
        assert_eq!(reopened.gauge_fields, visual.gauge_fields);
    }

    #[test]
    fn visual_anterior_sin_mapa_de_formatos_usa_el_formato_singular() {
        let visual: SavedVisual = toml::from_str(
            r#"
kind = "KPI"

[formato]
color_fondo = 15790320
"#,
        )
        .expect("abre formato anterior");

        assert!(visual.formatos.is_empty());
        assert_eq!(visual.formato.color_fondo, 0xf0f0f0);
    }

    #[test]
    fn medida_de_color_condicional_se_guarda_y_archivos_anteriores_usan_none() {
        let previous: SavedVisual =
            toml::from_str(r#"kind = "Columnas""#).expect("lee un visual anterior");
        assert_eq!(previous.conditional_color_measure, None);
        assert_eq!(previous.conditional_color_compare_measure, None);

        let visual = SavedVisual {
            kind: "Columnas".to_string(),
            measures: vec!["Ingresos".to_string()],
            conditional_color_measure: Some("Ingresos".to_string()),
            conditional_color_compare_measure: Some("Meta".to_string()),
            conditional_color_source: Some(SavedColorMetric::Measure("Ingresos".to_string())),
            conditional_color_comparison: Some(SavedColorMetric::Measure("Meta".to_string())),
            ..SavedVisual::default()
        };
        let text = toml::to_string(&visual).expect("serializa el visual");
        assert!(
            text.contains("conditional_color_measure = \"Ingresos\""),
            "{text}"
        );
        let reopened: SavedVisual = toml::from_str(&text).expect("abre el visual");
        assert_eq!(
            reopened.conditional_color_measure,
            Some("Ingresos".to_string())
        );
        assert_eq!(
            reopened.conditional_color_compare_measure,
            Some("Meta".to_string())
        );
        assert_eq!(
            reopened.conditional_color_source,
            visual.conditional_color_source
        );
        assert_eq!(
            reopened.conditional_color_comparison,
            visual.conditional_color_comparison
        );
    }

    #[test]
    fn una_agregacion_implicita_de_color_se_guarda_con_columna_y_tipo() {
        let visual = SavedVisual {
            kind: "Columnas".to_string(),
            conditional_color_source: Some(SavedColorMetric::Implicit(SavedImplicitMeasure {
                column: SavedColumnRef::Qualified {
                    table: "ventas".to_string(),
                    column: "cliente".to_string(),
                },
                aggregation: SavedAggregation::DistinctCount,
                table: None,
            })),
            conditional_color_comparison: Some(SavedColorMetric::Measure("Objetivo".to_string())),
            ..SavedVisual::default()
        };
        let text = toml::to_string(&visual).expect("serializa la regla");
        let reopened: SavedVisual = toml::from_str(&text).expect("abre la regla");
        assert_eq!(
            reopened.conditional_color_source,
            visual.conditional_color_source
        );
        assert_eq!(
            reopened.conditional_color_comparison,
            visual.conditional_color_comparison
        );
    }

    #[test]
    fn los_formatos_viejos_usan_estilo_visual_por_defecto() {
        let formato: SavedFormato = toml::from_str("titulo = false").expect("formato antiguo");
        assert_eq!(
            formato,
            SavedFormato {
                titulo: false,
                ..SavedFormato::default()
            }
        );
    }

    #[test]
    fn el_sentido_del_objetivo_kpi_se_guarda_y_se_recupera() {
        let mut formato = SavedFormato::default();
        formato.kpi_menor_es_mejor = true;
        let texto = toml::to_string(&formato).expect("serializa formato");
        let guardado: SavedFormato = toml::from_str(&texto).expect("lee formato");
        assert!(guardado.kpi_menor_es_mejor);
    }

    #[test]
    fn la_familia_tipografica_se_guarda_y_archivos_viejos_usan_el_tema() {
        let previous: SavedFormato =
            toml::from_str("fuente_mono = true").expect("formato anterior");
        assert_eq!(previous.familia_fuente, SavedFontFamily::System);
        assert!(previous.fuente_mono);

        let formato = SavedFormato {
            familia_fuente: SavedFontFamily::Serif,
            ..SavedFormato::default()
        };
        let texto = toml::to_string(&formato).expect("serializa fuente");
        let reopened: SavedFormato = toml::from_str(&texto).expect("recupera fuente");
        assert_eq!(reopened.familia_fuente, SavedFontFamily::Serif);
    }

    #[test]
    fn los_ajustes_analiticos_se_guardan_y_el_default_es_apagado() {
        let mut formato = SavedFormato::default();
        assert!(!formato.linea_referencia);
        assert!(!formato.linea_tendencia);
        assert!(!formato.pronostico);
        formato.linea_referencia = true;
        formato.linea_tendencia = true;
        formato.pronostico = true;
        let texto = toml::to_string(&formato).expect("serializa formato");
        let guardado: SavedFormato = toml::from_str(&texto).expect("lee formato");
        assert!(guardado.linea_referencia);
        assert!(guardado.linea_tendencia);
        assert!(guardado.pronostico);
    }

    #[test]
    fn totales_de_columna_se_guardan_y_los_informes_anteriores_los_apagan() {
        let previous: SavedFormato = toml::from_str("titulo = false").expect("formato anterior");
        assert!(!previous.subtotales_columna);

        let formato = SavedFormato {
            subtotales_columna: true,
            ..SavedFormato::default()
        };
        let text = toml::to_string(&formato).expect("serializa formato");
        let reopened: SavedFormato = toml::from_str(&text).expect("lee formato");
        assert!(reopened.subtotales_columna);
    }

    #[test]
    fn las_interacciones_entre_visuales_se_guardan_y_los_archivos_viejos_usan_filtro() {
        let texto = r#"
id = "a"
kind = "Barras"
interactions = { b = "highlight" }
"#;
        let mut visual: SavedVisual = toml::from_str(texto).expect("lee interaccion guardada");
        assert_eq!(
            visual.interactions.get("b"),
            Some(&SavedInteraction::Highlight)
        );

        visual.interactions.clear();
        let texto = toml::to_string(&visual).expect("serializa visual");
        let guardado: SavedVisual = toml::from_str(&texto).expect("recupera visual");
        assert_eq!(guardado.interactions, BTreeMap::new());
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
            data_scope: None,
            source_key: None,
            loaded_queries: Vec::new(),
            model_positions: BTreeMap::new(),
            saved_at: None,
            dimensions: vec![],
            measures: vec![],
            filters: vec![],
            report_filters: vec![],
            panels: SavedPanels::default(),
            visual: Some("Tabla".to_string()),
            period_months: None,
            top_n: None,
            formato: BTreeMap::from([("Tabla".to_string(), formato)]),
            pages: Vec::new(),
            active_page: 0,
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
            data_scope: None,
            source_key: None,
            loaded_queries: Vec::new(),
            model_positions: BTreeMap::new(),
            saved_at: None,
            dimensions: vec![],
            measures: vec![],
            filters: vec![],
            report_filters: vec![],
            panels: SavedPanels::default(),
            visual: None,
            period_months: None,
            top_n: None,
            formato: BTreeMap::new(),
            pages: Vec::new(),
            active_page: 0,
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(!text.contains("[formato]"), "{text}");
    }
}
