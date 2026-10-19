//! La app. Conectar a Postgres, elegir dimensiones y medidas, y ver el informe.
//!
//! # El reparto entre hilos
//!
//! Todo el trabajo pesado ocurre en `background_spawn`: conectar, leer el catalogo, construir
//! el modelo y correr las consultas. Al hilo de UI solo llega la matriz ya calculada, como
//! cadenas.
//!
//! `Engine` es `Send` pero no `Sync`, y `Model` es dato plano. La conexion vive en la tarea de
//! fondo y no se comparte: cada ejecucion abre la suya contra la base adjunta en memoria.
//!
//! # Por que `uniform_list` y no el componente `Table` de gpui-kit
//!
//! `uniform_list` es lo que la Fase 0 midio a 23 microsegundos por frame con un millón de
//! filas. El componente `Table` trae su propia virtualizacion, que no se ha medido aqui, y
//! meterlo seria cambiar la pieza validada por otra sin numero que la compare. gpui-kit se
//! usa para el tema y los controles, que es donde aporta.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use datan_connect::{PostgresCatalog, Source, introspect};
use datan_dax_eval::{AtomKind, BinaryOp, Shape};
use datan_model::{DataType, Model, ObjectId, RelationshipRole, TableKind};
mod components;
mod config;
mod formato;
mod model_doc;
mod tokens;

use components::Peso;
use config::Connections;
use datan_report::{Header, Matrix, Report, RowKind, source::DuckSource};
use datan_storage::Engine;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::chart::{AreaChart, BarChart, LineChart, PieChart};
use gpui_kit::*;
// `Div::on_click` vive en este trait y `gpui-kit` reexporta `gpui::*`, asi que entra aqui.
use gpui_kit::component::Theme;
// `gpui-kit` es una fachada sobre gpui: reexporta sus tipos pero no sus traits de
// interactividad, que son los que traen `on_click`, `when` y los estilos de hover.
// El crate se llama `gpui-pre` pero su libreria se llama `gpui`. Por eso el alias.
use ::gpui::InteractiveElement;
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::scroll::ScrollableElement;

/// Que se eligio del modelo.
#[derive(Clone)]
struct Selection {
    dimensions: Vec<ObjectId>,
    measures: Vec<ObjectId>,
}

/// Un filtro activo, para poder quitarlo con un clic.
#[derive(Clone)]
struct ActiveFilter {
    column: ObjectId,
    label: SharedString,
    value: SharedString,
}

/// Lo que se puede deshacer: el estado de la VISTA, no el resultado.
///
/// La matriz no entra. Se vuelve a correr con el mismo modelo y la misma base, y muestra los
/// numeros de ahora. Guardar la matriz haria que deshacer mostrara un numero viejo como si
/// fuera actual, que es justo lo que un informe no puede hacer.
#[derive(Clone)]
struct Vista {
    selection: Selection,
    filters: Vec<ActiveFilter>,
    visual: String,
    sort: Option<(usize, bool)>,
    top_n: Option<datan_report::TopN>,
    date_months: Option<u8>,
    formatos: std::collections::BTreeMap<String, datan_report::saved::SavedFormato>,
}

/// Cuantos pasos guarda el historial.
///
/// Un tope y no historia infinita: cada paso es una seleccion y una lista de filtros, pero el
/// tope tambien dice donde termina lo que se puede deshacer. Cincuenta alcanza para el trabajo
/// de una sesion.
const HISTORIAL_MAX: usize = 50;

/// Las dos pilas de deshacer y rehacer.
///
/// Es una pila y no parte de `Datan`: asi el orden de los pasos se prueba sin una ventana, que
/// es lo unico que no se puede montar en un test. La app solo le pasa la vista actual.
#[derive(Default)]
struct Historial {
    atras: Vec<Vista>,
    adelante: Vec<Vista>,
}

impl Historial {
    /// Anota la vista que se va a abandonar. Un cambio nuevo borra el futuro: despues de
    /// deshacer y tocar otra cosa, rehacer ya no lleva a ninguna parte.
    fn recordar(&mut self, vista: Vista) {
        self.adelante.clear();
        if self.atras.len() == HISTORIAL_MAX {
            self.atras.remove(0);
        }
        self.atras.push(vista);
    }

    /// Devuelve la vista anterior y guarda la actual para rehacer. `None` si no hay nada.
    fn deshacer(&mut self, actual: Vista) -> Option<Vista> {
        let vista = self.atras.pop()?;
        self.adelante.push(actual);
        Some(vista)
    }

    fn rehacer(&mut self, actual: Vista) -> Option<Vista> {
        let vista = self.adelante.pop()?;
        self.atras.push(actual);
        Some(vista)
    }

    /// Olvida todo. Se llama cuando cambia el MODELO o el informe: los ids del historial son
    /// de otra cosa y restaurarlos apuntaria a columnas que ya no estan.
    fn olvidar(&mut self) {
        self.atras.clear();
        self.adelante.clear();
    }
}

/// La fila lista para pintar. `SharedString` es un string compartido por referencia: una
/// columna con 500 valores repetidos cuesta 500 asignaciones, no 500 por celda.
#[derive(Clone)]
struct Column {
    header: SharedString,
    values: Arc<[SharedString]>,
    numeric: bool,
}

#[derive(Clone)]
struct Grid {
    columns: Arc<[Column]>,
    /// Que renglon es cada fila, en el orden de la matriz: dato, subtotal o gran total.
    /// Los subtotales se pintan distintos y no aceptan clic.
    kinds: Arc<[datan_report::RowKind]>,
    row_count: usize,
}

enum State {
    Connecting,
    Choosing,
    /// La lista de informes guardados. Se llega desde "Abrir", con el modelo ya conectado:
    /// abrir un informe resuelve nombres contra un modelo, y sin modelo no hay nada que
    /// resolver.
    Opening,
    Ready,
    /// La conexion fallo: no hay modelo.
    Failed(String),
    /// La conexion funciono y el INFORME fallo. El mismo pantallazo con otro titulo: decir
    /// "no se pudo conectar" cuando la conexion funciona manda a buscar el error al lugar
    /// equivocado.
    Broken(String),
}

/// El modo "conectar dos columnas" de la barra lateral.
///
/// Un dialogo de relaciones pide dos desplegables y un boton. Dos clics sobre las columnas
/// que ya estan en el panel dicen lo mismo y no abren nada. El estado es explicito para que
/// el primer clic no tenga que guardar una columna "a medias" en otro campo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Conexion {
    #[default]
    Apagada,
    /// Esperando la primera columna.
    PrimerLado,
    /// Ya hay una columna. El siguiente clic elige la otra.
    SegundoLado(ObjectId),
}

/// La vista.
struct Datan {
    state: State,
    selection: Selection,
    /// El informe actual, con sus filtros. Es la fuente de verdad del cross-filter.
    report: Option<Report>,
    filters: Vec<ActiveFilter>,
    /// La matriz, para el cross-filter.
    ///
    /// El grid de pintarla es una COPIA con `SharedString`. La matriz se queda porque pinchar
    /// una celda necesita saber que valor hay en esa posicion, y releerla del grid
    /// obligaria a desreferenciar los `Arc` en el manejador del clic.
    formula_error: Option<String>,
    /// El visual activo del lienzo: Todo, Tarjetas, Barras o Tabla.
    visual: String,
    /// El nombre del informe que se esta mirando, si se guardo o se abrio.
    ///
    /// `None` es un informe sin guardar: no tiene archivo, asi que no tiene nombre. Se
    /// muestra el que tendria, que es el que "Guardar" le va a poner.
    report_name: Option<String>,
    /// El indicador de la barra superior tras guardar.
    /// Cuando se guardo el informe que se esta mirando, en segundos desde la epoca. `None`
    /// es un informe con cambios sin guardar, o sin guardar todavia.
    last_saved: Option<i64>,
    /// Modo Lectura: el informe se mira y no se toca.
    ///
    /// Un boton que no hace nada no se muestra; este modo SI hace algo: apaga el filtro por
    /// clic, que es el gesto que cambia el informe.
    lectura: bool,
    /// Orden de la matriz: (indice de columna, descendente). Tercer clic lo quita.
    sort: Option<(usize, bool)>,
    matrix: Option<Matrix>,
    grid: Option<Grid>,
    /// El total de cada medida seleccionada, para las tarjetas.
    /// (nombre, total, delta contra el mismo periodo del ano anterior).
    totals: Vec<Tarjeta>,
    /// La barra de formula DAX.
    ///
    /// Se crea en el primer render porque `InputState::new` pide una `Window`, y `new` de la
    /// vista no la tiene. Es el patron habitual en gpui.
    formula: Option<Entity<InputState>>,
    /// La busqueda del panel Datos. Se crea en el primer render, como la de formula.
    search: Option<Entity<InputState>>,
    /// El rango de fechas activo en meses hacia atras. `None` es todo el historico.
    date_months: Option<u8>,
    /// Las tablas abiertas en el panel Datos, por id.
    expanded: Vec<ObjectId>,
    /// Filas por tabla, para el contador del panel. `COUNT(*)` por tabla al conectar.
    row_counts: std::collections::BTreeMap<ObjectId, i64>,
    /// Cuando se consultaron los conteos, para el "actualizado hace X min".
    counted_at: Option<i64>,
    /// Cuantas consultas costo el ultimo informe, y cuantos ms.
    stats: Option<String>,
    /// Lo que el modelo del usuario no pudo aplicar al conectar.
    ///
    /// Se muestra en la barra de estado porque una medida que existia y ya no se puede
    /// aplicar no puede desaparecer en silencio: el informe la pide por nombre y falla.
    model_warnings: Vec<String>,
    /// Las jerarquias del usuario, ya resueltas a ids: (nombre, niveles de fuera a dentro).
    /// Es lo que alimenta el drill.
    jerarquias: Vec<(String, Vec<ObjectId>)>,
    /// El Top N de la matriz, si hay uno. Es de la PANTALLA, como el periodo: se vuelve a
    /// aplicar en cada corrida y se guarda con la vista.
    top_n: Option<datan_report::TopN>,
    database: Option<String>,
    scroll: UniformListScrollHandle,
    /// El modo de conectar dos columnas, si esta encendido.
    conexion: Conexion,
    /// El formato de cada visual, por su clave. Cambiar de Barras a Tabla y volver tiene que
    /// encontrar las barras como estaban: un solo formato para la pagina no lo hace.
    formatos: std::collections::BTreeMap<String, datan_report::saved::SavedFormato>,
    /// Los pasos de deshacer y rehacer.
    historial: Historial,
    /// El numero de la ultima corrida pedida. Una corrida vieja que termina despues de una
    /// nueva NO pinta: dos clics seguidos (o dos deshaceres) dejan dos consultas en vuelo y
    /// la que llega tarde pisaria la matriz de la que llego primero.
    refresh_seq: u64,
}

impl Datan {
    fn new(_cx: &mut Context<Self>) -> Self {
        // Arranca en la pantalla de conexion. No se intenta conectar solo: una app que
        // abre una base sin preguntar es una app que escribe en la base equivocada.
        Self {
            state: State::Choosing,
            selection: Selection {
                dimensions: Vec::new(),
                measures: Vec::new(),
            },
            report: None,
            filters: Vec::new(),
            matrix: None,
            grid: None,
            totals: Vec::new(),
            formula_error: None,
            visual: "Todo".to_string(),
            report_name: None,
            last_saved: None,
            lectura: false,
            sort: None,
            formula: None,
            search: None,
            date_months: None,
            expanded: Vec::new(),
            row_counts: Default::default(),
            counted_at: None,
            stats: None,
            model_warnings: Vec::new(),
            jerarquias: Vec::new(),
            top_n: None,
            database: None,
            scroll: UniformListScrollHandle::default(),
            conexion: Conexion::Apagada,
            formatos: Default::default(),
            historial: Historial::default(),
            refresh_seq: 0,
        }
    }

    /// La lista de conexiones guardadas.
    fn render_connections(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let connections = Connections::load().unwrap_or_default();
        let mut list = div().flex_col().gap_2();

        for connection in &connections.list {
            let database = connection.database.clone();
            list = list.child(
                div()
                    .id(ElementId::Name(SharedString::from(format!(
                        "conn-{}",
                        connection.database
                    ))))
                    .w(px(360.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.list_hover))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(connection.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::LETRA_ETIQUETA))
                            .text_color(theme.muted_foreground)
                            .child(connection.database.clone()),
                    )
                    .on_click(cx.listener({
                        move |view, _event, _window, cx| {
                            view.connect(&database, cx)
                        }
                    })),
            );
        }

        list.into_any_element()
    }

    /// La lista de informes guardados.
    ///
    /// Todos los de la base conectada. Un informe de OTRA base se pinta apagado, con su base al
    /// lado y sin clic: abrirlo resolveria nombres contra un modelo que no es el suyo, y el error
    /// que saldria ("la dimension X no existe") manda a buscar el problema al lugar equivocado.
    fn render_reports(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let entries = datan_report::saved::index();

        let mut list = div().flex_col().gap_2();
        if entries.is_empty() {
            list = list.child(
                div()
                    .w(px(360.0))
                    .text_size(px(tokens::LETRA_CUERPO))
                    .text_color(theme.muted_foreground)
                    .child("No hay informes guardados con esta base."),
            );
        }

        for entry in entries {
            // El archivo guarda el QUALIFIER del informe (`pg`), no el nombre de la base: es
            // la etiqueta con la que se califica cada tabla en el SQL. Compararlo con el
            // nombre de la base ponia todos los informes como ajenos.
            let propia = self
                .report
                .as_ref()
                .is_some_and(|report| report.qualifier == entry.database);
            let name = entry.name.clone();
            let row = div()
                .w(px(360.0))
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_3()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(entry.name.clone()),
                );

            if propia {
                list = list.child(
                    row.id(ElementId::Name(SharedString::from(format!("report-{name}"))))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.list_hover))
                        .on_click(cx.listener(move |view, _event, _window, cx| {
                            view.open_report(&name, cx)
                        })),
                );
            } else {
                // Sin clic: un informe de otra base no se puede abrir con este modelo.
                list = list.child(row.bg(theme.muted));
            }
        }

        div()
            .flex()
            .flex_1()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .font_semibold()
                    .text_size(px(tokens::LETRA_H1))
                    .child("Abrir informe"),
            )
            .child(
                div()
                    .text_size(px(tokens::LETRA_CUERPO))
                    .text_color(theme.muted_foreground)
                    .child("Los informes guardados de esta base"),
            )
            .child(list)
            .child(clickable(
                &theme,
                "volver".into(),
                "Volver".to_string(),
                cx.listener(|view, _event, _window, cx| {
                    view.state = State::Ready;
                    cx.notify();
                }),
            ))
            .into_any_element()
    }

    /// Crea una medida desde la barra de formula.
    ///
    /// El formato es `Nombre = expresion`, partido en el PRIMER `=`. Una expresion DAX puede
    /// tener `=` dentro (los filtros de CALCULATE), asi que partir en todos partiria el
    /// nombre de la medida en el lugar equivocado.
    fn add_measure(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.formula.clone() else {
            return;
        };
        let text = state.read(cx).value().to_string();
        let Some((name, expression)) = text.split_once("=") else {
            self.formula_error =
                Some("formato: Nombre = expresion. Falta el `=`.".to_string());
            cx.notify();
            return;
        };
        let name = name.trim().to_string();
        let expression = expression.trim().to_string();
        if name.is_empty() || expression.is_empty() {
            self.formula_error =
                Some("formato: Nombre = expresion. Falta el nombre o la expresion.".to_string());
            cx.notify();
            return;
        }

        // Se valida contra el modelo ANTES de tocar nada. Una medida que no chequea no se
        // agrega: el usuario ve el error del chequeo, que es el mismo que veria Power BI.
        let Some(report) = self.report.clone() else {
            return;
        };
        let mut model = report.model.clone();
        if let Err(error) = datan_dax_check::check(&expression, &model) {
            self.formula_error = Some(format!("{error}"));
            cx.notify();
            return;
        }
        // Un nombre repetido no crea una segunda medida: la referencia por nombre seria
        // ambigua, y el modelo resuelve por la primera. Se avisa y no se toca nada.
        if model.measure_by_name(&name).is_some() {
            self.formula_error = Some(format!("ya existe una medida `{name}`"));
            cx.notify();
            return;
        }
        // El documento se escribe antes de tocar la vista: si el archivo no se puede guardar,
        // la medida no se agrega y el usuario ve por que. Una medida que vive solo en pantalla
        // se pierde al cerrar y deja un informe guardado que no abre.
        let Some(database) = self.database.clone() else {
            self.formula_error = Some("sin base conectada: la medida no se puede guardar".to_string());
            cx.notify();
            return;
        };
        let mut doc = match model_doc::ModelDoc::load(&database) {
            Ok(doc) => doc,
            Err(error) => {
                self.formula_error = Some(format!("{error}"));
                cx.notify();
                return;
            }
        };
        doc.upsert_measure(model_doc::MeasureDoc::new(name.clone(), expression.clone()));
        if let Err(error) = doc.save(&database) {
            self.formula_error = Some(format!("{error}"));
            cx.notify();
            return;
        }

        // El modelo en memoria se arma con el MISMO codigo que al conectar. El id sale de
        // ahi y no de una cuenta escrita aparte, que se desincronizaria al primer cambio.
        self.model_warnings = doc.apply(&mut model);
        let Some(id) = model.measure_by_name(&name).map(|m| m.id) else {
            return;
        };

        self.formula_error = None;
        self.report = Some(
            Report::new(model)
                .attached_to(&report.qualifier)
                .with_dimensions(self.selection.dimensions.clone())
                .with_measures(self.selection.measures.clone()),
        );
        self.toggle(id, true, cx);
    }

    /// Enciende o apaga el modo de conectar dos columnas.
    fn comenzar_conexion(&mut self, cx: &mut Context<Self>) {
        if self.lectura {
            self.stats = Some("modo Lectura: el modelo no se toca".to_string());
            cx.notify();
            return;
        }
        self.conexion = match self.conexion {
            Conexion::Apagada => {
                self.stats = Some("elige la primera columna de la relacion".to_string());
                Conexion::PrimerLado
            }
            Conexion::PrimerLado | Conexion::SegundoLado(_) => {
                self.stats = None;
                Conexion::Apagada
            }
        };
        cx.notify();
    }

    /// Crea la relacion entre dos columnas y la guarda en el documento del modelo.
    fn conectar(&mut self, primera: ObjectId, segunda: ObjectId, cx: &mut Context<Self>) {
        let Some(report) = self.report.clone() else {
            return;
        };
        let (Some((tabla_a, columna_a)), Some((tabla_b, columna_b))) =
            (report.model.column(primera), report.model.column(segunda))
        else {
            self.conexion = Conexion::Apagada;
            cx.notify();
            return;
        };
        if primera == segunda {
            self.stats = Some("una relacion une dos columnas distintas".to_string());
            cx.notify();
            return;
        }
        // El lado UNO es el que tiene la columna como clave. Cuando ninguna lo es, el par no
        // tiene direccion clara y se avisa en vez de inventarla: una relacion al reves manda
        // los filtros por el camino equivocado sin dar ningun error.
        let es_clave =
            |table: &datan_model::Table, column: &datan_model::Column| {
                table.key_columns.contains(&column.id)
            };
        let (from, to) = if es_clave(tabla_a, columna_a) {
            ((tabla_a, columna_a), (tabla_b, columna_b))
        } else if es_clave(tabla_b, columna_b) {
            ((tabla_b, columna_b), (tabla_a, columna_a))
        } else {
            self.stats = Some(format!(
                "ni {}[{}] ni {}[{}] son clave de su tabla: la relacion no tiene lado UNO",
                tabla_a.name, columna_a.name, tabla_b.name, columna_b.name
            ));
            self.conexion = Conexion::Apagada;
            cx.notify();
            return;
        };
        let Some(database) = self.database.clone() else {
            return;
        };
        let mut doc = match model_doc::ModelDoc::load(&database) {
            Ok(doc) => doc,
            Err(error) => {
                self.stats = Some(format!("{error}"));
                self.conexion = Conexion::Apagada;
                cx.notify();
                return;
            }
        };
        doc.upsert_relationship(model_doc::RelationshipDoc::new(
            from.0.name.clone(),
            from.1.name.clone(),
            to.0.name.clone(),
            to.1.name.clone(),
        ));
        if let Err(error) = doc.save(&database) {
            self.stats = Some(format!("{error}"));
            self.conexion = Conexion::Apagada;
            cx.notify();
            return;
        }

        let mut model = report.model.clone();
        self.model_warnings = doc.apply(&mut model);
        self.report = Some(
            Report::new(model)
                .attached_to(&report.qualifier)
                .with_dimensions(self.selection.dimensions.clone())
                .with_measures(self.selection.measures.clone()),
        );
        self.olvidar_historial();
        self.stats = Some(format!(
            "relacion {}[{}] a {}[{}]",
            from.0.name, from.1.name, to.0.name, to.1.name
        ));
        self.conexion = Conexion::Apagada;
        self.refresh(cx);
    }

    /// Cambia el rol de una relationship y lo guarda en el documento del modelo.
    ///
    /// La introspeccion solo lee foreign keys. Una relacion que la base no declara no existe
    /// para el motor, y hasta ahora no habia forma de decirla desde la app. El interruptor
    /// apaga la que el catalogo trae mal, que es el otro lado del mismo problema.
    fn toggle_relationship(&mut self, from: ObjectId, to: ObjectId, cx: &mut Context<Self>) {
        if self.lectura {
            self.stats = Some("modo Lectura: el modelo no se toca".to_string());
            cx.notify();
            return;
        }
        let Some(report) = self.report.clone() else {
            return;
        };
        let (Some((from_table, from_column)), Some((to_table, to_column))) = (
            report
                .model
                .column(from)
                .map(|(table, column)| (table.name.clone(), column.name.clone())),
            report
                .model
                .column(to)
                .map(|(table, column)| (table.name.clone(), column.name.clone())),
        ) else {
            return;
        };
        let Some(current) = report
            .model
            .relationships
            .iter()
            .find(|relating| relating.from_column == from && relating.to_column == to)
        else {
            return;
        };
        let next = match current.role {
            RelationshipRole::Active => "inactiva",
            RelationshipRole::Inactive => "activa",
        };
        let Some(database) = self.database.clone() else {
            return;
        };
        // El documento se escribe antes de tocar la vista, igual que al crear una medida: una
        // relacion que vive solo en pantalla se pierde al cerrar, y el informe guardado vuelve
        // a leer un modelo sin ella.
        let mut doc = match model_doc::ModelDoc::load(&database) {
            Ok(doc) => doc,
            Err(error) => {
                self.stats = Some(format!("{error}"));
                cx.notify();
                return;
            }
        };
        doc.upsert_relationship(
            model_doc::RelationshipDoc::new(from_table, from_column, to_table, to_column)
                .con_rol(next),
        );
        if let Err(error) = doc.save(&database) {
            self.stats = Some(format!("{error}"));
            cx.notify();
            return;
        }

        let mut model = report.model.clone();
        self.model_warnings = doc.apply(&mut model);
        self.report = Some(
            Report::new(model)
                .attached_to(&report.qualifier)
                .with_dimensions(self.selection.dimensions.clone())
                .with_measures(self.selection.measures.clone()),
        );
        self.olvidar_historial();
        self.stats = Some(format!("relacion {next}"));
        self.refresh(cx);
    }

    /// Filtra por el valor de texto de una dimension, como pinchar una barra.
    fn filter_by_label(&mut self, column: ObjectId, label: &str, cx: &mut Context<Self>) {
        if self.lectura {
            self.stats = Some("modo Lectura: el clic no filtra".to_string());
            cx.notify();
            return;
        }
        // Pinchar dos veces la misma barra CAMBIA el valor, no acumula dos filtros que se
        // intersectan y dejan la matriz vacia.
        self.recordar();
        self.filters.retain(|filter| filter.column != column);
        self.filters.push(ActiveFilter {
            column,
            label: SharedString::from(
                self.model_column_name(column),
            ),
            value: SharedString::from(label.to_string()),
        });
        self.refresh(cx);
    }

    fn model_column_name(&self, column: ObjectId) -> String {
        self.report
            .as_ref()
            .and_then(|r| r.model.column(column))
            .map(|(_, c)| c.name.clone())
            .unwrap_or_default()
    }

    fn model_measure_name(&self, measure: ObjectId) -> String {
        self.report
            .as_ref()
            .and_then(|r| r.model.measures.iter().find(|m| m.id == measure))
            .map(|m| m.name.clone())
            .unwrap_or_default()
    }

    /// El estado de la vista, para el historial.
    fn vista(&self) -> Vista {
        Vista {
            selection: self.selection.clone(),
            filters: self.filters.clone(),
            visual: self.visual.clone(),
            sort: self.sort,
            top_n: self.top_n,
            date_months: self.date_months,
            formatos: self.formatos.clone(),
        }
    }

    /// Anota el estado ANTES de cambiarlo. Cada accion que cambia la vista llama a esto.
    fn recordar(&mut self) {
        let vista = self.vista();
        self.historial.recordar(vista);
    }

    /// Vuelve al estado anterior. El informe se vuelve a correr: los numeros son los de la
    /// base de ahora, sin el cambio.
    fn deshacer(&mut self, cx: &mut Context<Self>) {
        let actual = self.vista();
        let Some(vista) = self.historial.deshacer(actual) else {
            return;
        };
        self.restaurar(vista);
        self.refresh(cx);
    }

    fn rehacer(&mut self, cx: &mut Context<Self>) {
        let actual = self.vista();
        let Some(vista) = self.historial.rehacer(actual) else {
            return;
        };
        self.restaurar(vista);
        self.refresh(cx);
    }

    fn restaurar(&mut self, vista: Vista) {
        self.selection = vista.selection;
        self.filters = vista.filters;
        self.visual = vista.visual;
        self.sort = vista.sort;
        self.top_n = vista.top_n;
        self.date_months = vista.date_months;
        self.formatos = vista.formatos;
    }

    fn olvidar_historial(&mut self) {
        self.historial.olvidar();
    }

    /// El formato del visual que se esta mirando. El que no se toco usa el default.
    fn formato_actual(&self) -> datan_report::saved::SavedFormato {
        self.formatos
            .get(&self.visual)
            .copied()
            .unwrap_or_default()
    }

    /// Cambia un ajuste del visual activo.
    fn ajustar_formato(
        &mut self,
        cambio: impl FnOnce(&mut datan_report::saved::SavedFormato),
        cx: &mut Context<Self>,
    ) {
        if self.lectura {
            self.stats = Some("modo Lectura: el formato no se toca".to_string());
            cx.notify();
            return;
        }
        self.recordar();
        cambio(self.formatos.entry(self.visual.clone()).or_default());
        cx.notify();
    }

    /// La linea de estado de la barra superior: visuales, consultas y milisegundos, y al
    /// final lo que el modelo del usuario no pudo aplicar.
    ///
    /// Lo que no se pudo aplicar se agrega y no reemplaza al resto: el informe puede estar
    /// bien y faltar una medida que el usuario no esta mirando.
    fn linea_estado(&self, visuales: usize) -> String {
        let mut line = match &self.stats {
            Some(stats) => format!(
                "{}  \u{00b7}  {stats}",
                plural(visuales, "visualizacion", "visualizaciones")
            ),
            None => plural(visuales, "visualizacion", "visualizaciones"),
        };
        if let Some(first) = self.model_warnings.first() {
            line.push_str("  \u{00b7}  ");
            line.push_str(first);
            let rest = self.model_warnings.len() - 1;
            if rest > 0 {
                line.push_str(&format!(" y {rest} mas"));
            }
        }
        line
    }

    /// El nombre que tendria el informe sin guardar: la primera dimension.
    ///
    /// Es la columna que distingue un informe de otro sobre la misma base. Un dialogo de
    /// nombre no se puede verificar sin pantalla, y el nombre se puede cambiar renombrando el
    /// archivo.
    fn suggested_name(&self) -> String {
        // Las dimensiones viven en la SELECCION, no en el informe base: el base es el que
        // acumula filtros, y `refresh` le pone las dimensiones al correr.
        let Some(report) = self.report.as_ref() else {
            return "informe".to_string();
        };
        self.selection
            .dimensions
            .first()
            .and_then(|id| report.model.column(*id).map(|(_, c)| c.name.clone()))
            .unwrap_or_else(|| "informe".to_string())
    }

    /// El nombre del informe que se esta mirando: el guardado, o el que tendria.
    fn report_title(&self) -> String {
        self.report_name
            .clone()
            .unwrap_or_else(|| self.suggested_name())
    }

    /// Guarda el informe actual. Sin nombre de archivo elegido a mano: el informe se llama
    /// por la primera dimension, que es lo que lo distingue. Un dialogo de nombre no se puede
    /// verificar sin pantalla.
    ///
    /// Se guarda DOS veces bajo el mismo nombre: la segunda vez sobrescribe. Un informe
    /// guardado que se guarda otra vez con otro nombre deja dos archivos que se parecen.
    fn save_report(&mut self, cx: &mut Context<Self>) {
        let Some(report) = self.report.clone() else {
            return;
        };
        let Some(matrix) = self.matrix.clone() else {
            return;
        };

        let name = self.report_title();
        let label = format!("{name} ({} filas)", matrix.row_count());

        // El visual y el periodo son de la PANTALLA: no cambian un numero, cambian como se
        // mira. Se guardan al lado para que reabrir un informe en "Anillo" no abra "Tabla".
        let view = datan_report::saved::SavedView {
            visual: Some(self.visual.clone()),
            period_months: self.date_months,
            top_n: self.top_n.map(|top| datan_report::saved::SavedTopN {
                measure: self.model_measure_name(top.measure),
                count: top.count,
                abajo: top.abajo,
            }),
            formato: self.formatos.clone(),
        };

        match datan_report::saved::save_view(&report, &name, &view) {
            Ok(()) => {
                self.report_name = Some(name);
                self.stats = Some(format!("guardado como `{label}`"));
                self.last_saved = Some(epoch_now());
                cx.notify();
            }
            Err(error) => {
                self.stats = Some(format!("no se pudo guardar: {error}"));
                cx.notify();
            }
        }
    }

    /// Abre un informe guardado contra el modelo conectado.
    ///
    /// La seleccion, el visual y el periodo se recuperan del archivo. Los filtros se vuelven
    /// a poner en la barra: el informe abre con el recorte que tenia, y se puede soltar.
    fn open_report(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(model) = self.report.as_ref().map(|report| report.model.clone()) else {
            return;
        };

        match datan_report::saved::open_view(name, &model) {
            Ok((report, view)) => {
                self.selection = Selection {
                    dimensions: report.dimensions.clone(),
                    measures: report.measures.clone(),
                };
                self.report_name = Some(name.to_string());
                // El visual guardado puede ser uno que ya no existe: se queda el actual en
                // vez de pintar una pantalla que no esta.
                if let Some(visual) = view.visual {
                    self.visual = visual;
                }
                self.formatos = view.formato;
                self.date_months = view.period_months;
                // La medida del Top N se resuelve por nombre, como todo lo guardado. Una
                // medida que ya no existe deja la vista sin Top N en vez de romperla.
                self.top_n = view.top_n.and_then(|top| {
                    report
                        .model
                        .measures
                        .iter()
                        .find(|measure| measure.name == top.measure)
                        .map(|measure| datan_report::TopN {
                            measure: measure.id,
                            count: top.count,
                            abajo: top.abajo,
                        })
                });
                self.filters = report
                    .filters
                    .entries()
                    .flat_map(|(column, filters)| {
                        let label = report
                            .model
                            .column(*column)
                            .map(|(_, c)| c.name.clone())
                            .unwrap_or_default();
                        filters
                            .iter()
                            .flat_map(|filter| filter.values().to_vec())
                            .map(move |value| ActiveFilter {
                                column: *column,
                                label: SharedString::from(label.clone()),
                                value: SharedString::from(valor_legible(&value)),
                            })
                    })
                    .collect();
                self.report = Some(report);
                // El historial era de otro informe. Deshacer despues de abrir llevaria a
                // una vista que no es la de este archivo.
                self.olvidar_historial();
                self.state = State::Ready;
                self.stats = Some(format!("abierto `{name}`"));
                self.refresh(cx);
            }
            Err(error) => {
                self.stats = Some(format!("no se pudo abrir `{name}`: {error}"));
                self.state = State::Ready;
                cx.notify();
            }
        }
    }

    /// Exporta la matriz a CSV junto a la carpeta del usuario.
    /// Enciende, apaga o cambia el Top N.
    ///
    /// La medida que ordena es la PRIMERA de la seleccion: la matriz ordena por la columna
    /// que se ve mas a la izquierda de las medidas, que es la que el usuario eligio primero.
    fn set_top_n(&mut self, top: Option<(usize, bool)>, cx: &mut Context<Self>) {
        let next = top.and_then(|(count, abajo)| {
            self.selection
                .measures
                .first()
                .map(|measure| datan_report::TopN {
                    measure: *measure,
                    count,
                    abajo,
                })
        });
        // El boton de un Top N ya activo lo apaga. Si el resultado es el mismo que ya estaba,
        // no hay paso de historial: deshacer no tiene que deshacer un clic que no cambio nada.
        if next == self.top_n {
            return;
        }
        self.recordar();
        self.top_n = next;
        self.refresh(cx);
    }

    /// Reemplaza la dimension actual por el nivel de jerarquia que llega.
    ///
    /// Los filtros activos SE QUEDAN: filtrar por region y bajar a producto tiene que ver
    /// solo los productos de esa region. El orden por medida se suelta, porque los indices
    /// de columna apuntan a la matriz anterior.
    fn drill_nivel(&mut self, nivel: ObjectId, cx: &mut Context<Self>) {
        // Bajar al nivel donde ya se esta no cambia nada: no deja un paso de historial que
        // no hace nada al deshacer.
        if self.selection.dimensions.as_slice() == [nivel] {
            return;
        }
        self.recordar();
        self.selection.dimensions = vec![nivel];
        self.sort = None;
        self.refresh(cx);
    }

    fn export_csv(&mut self, cx: &mut Context<Self>) {
        self.exportar(Archivo::Csv, cx);
    }

    fn export_excel(&mut self, cx: &mut Context<Self>) {
        self.exportar(Archivo::Excel, cx);
    }

    fn export_pdf(&mut self, cx: &mut Context<Self>) {
        self.exportar(Archivo::Pdf, cx);
    }

    /// Escribe la matriz a `~/Desktop/<titulo>-<dia>.<ext>`.
    ///
    /// El nombre sale del titulo del informe y del dia: dos exportaciones del mismo informe
    /// el mismo dia se pisan, y "informe (1).csv" es lo que el navegador hace peor. La barra
    /// de estado dice donde quedo, porque un archivo exportado sin saber donde no sirve.
    fn exportar(&mut self, archivo: Archivo, cx: &mut Context<Self>) {
        let Some(matrix) = self.matrix.clone() else {
            return;
        };
        let titulo = self.report_title();
        // El dia viene del reloj, convertido con el calendario del motor: no hay ninguna
        // dependencia de tiempo en el arbol y no la va a haber por un nombre de archivo.
        let dia = datan_dax_eval::format_date((epoch_now() / 86_400) as i32);
        let (bytes, extension) = match archivo {
            Archivo::Csv => (datan_report::export::csv(&matrix).into_bytes(), "csv"),
            Archivo::Excel => match datan_report::export::xlsx(&matrix, &titulo) {
                Ok(bytes) => (bytes, "xlsx"),
                Err(error) => {
                    self.stats = Some(format!("no se pudo exportar: {error}"));
                    cx.notify();
                    return;
                }
            },
            Archivo::Pdf => (datan_report::export::pdf(&matrix, &titulo), "pdf"),
        };

        let base = std::env::var("HOME")
            .map(|home| std::path::PathBuf::from(home).join("Desktop"))
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        let ruta = datan_report::export::ruta_de_export(&base, &titulo, &dia, extension);

        match std::fs::write(&ruta, bytes) {
            Ok(()) => {
                self.stats = Some(format!("exportado a {}", ruta.display()));
                cx.notify();
            }
            Err(error) => {
                self.stats = Some(format!("no se pudo exportar: {error}"));
                cx.notify();
            }
        }
    }

    /// Conecta y monta el modelo. Todo en el hilo de fondo.
    fn connect(&mut self, database: &str, cx: &mut Context<Self>) {
        self.state = State::Connecting;
        let database = database.to_string();
        let db_for_build = database.clone();

        cx.spawn(async move |this, cx| {
            let built = cx
                .background_spawn(async move { build_model(&db_for_build) })
                .await;

            _ = this.update(cx, |view, cx| {
                match built {
                    Ok((model, report, avisos, jerarquias)) => {
                        view.selection = suggested(&model);
                        // El modo de conectar dos columnas es del modelo anterior: los ids
                        // que tenia no existen en este. Los filtros, el Top N y el orden
                        // tampoco: sus ids e indices apuntan a columnas de la base anterior.
                        // Los filtros se saltaban al correr, pero la pastilla se seguia
                        // dibujando como si gobernara.
                        view.conexion = Conexion::Apagada;
                        view.filters.clear();
                        view.top_n = None;
                        view.sort = None;
                        view.olvidar_historial();
                        view.report = Some(report);
                        view.model_warnings = avisos;
                        view.jerarquias = jerarquias;
                        // Sin esto la cabecera dice "sin base" y el refresh cae por defecto a
                        // `datan_bench`: la app aparentaba estar conectada a otra base.
                        view.database = Some(database.clone());
                        // Las filas por tabla se cuentan al conectar, no en cada refresh:
                        // el numero cambia muy despacio y el panel se dibuja en cada frame.
                        // Las tablas empiezan cerradas: el panel muestra QUE hay, y abrir una
                        // es una decision del usuario. Con busqueda, la que coincide se abre
                        // sola, que es cuando hay una razon para abrirla.
                        view.expanded.clear();
                        view.counted_at = None;
                        view.count_rows(cx);

                        // La conexion que funciono se recuerda. Un usuario que conecta
                        // siempre a la misma base no deberia buscarla cada vez: la lista
                        // ordena por uso y la de arriba es la de ayer.
                        let mut saved = Connections::load().unwrap_or_default();
                        saved.upsert(config::Connection {
                            name: database.clone(),
                            database: database.clone(),
                        });
                        if let Err(error) = saved.save() {
                            eprintln!("[datan] no se pudo guardar la conexion: {error}");
                        }

                        view.state = State::Connecting;
                        view.refresh(cx);
                    }
                    Err(error) => view.state = State::Failed(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Cuenta las filas de cada tabla para el panel Datos.
    fn count_rows(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.report.as_ref().map(|r| r.model.clone()) else {
            return;
        };
        let database = self
            .database
            .clone()
            .unwrap_or_else(|| "datan_bench".to_string());
        let tables: Vec<(ObjectId, String)> = model
            .tables
            .iter()
            .map(|t| (t.id, t.name.clone()))
            .collect();

        cx.spawn(async move |this, cx| {
            let counted = cx
                .background_spawn(async move { count_rows(&tables, &database) })
                .await;
            _ = this.update(cx, |view, _cx| {
                if let Ok(counts) = counted {
                    view.row_counts = counts;
                    view.counted_at = Some(epoch_now());
                }
            });
        })
        .detach();
    }

    /// Cambia una dimension o medida de la seleccion y vuelve a correr el informe.
    fn toggle(&mut self, column: ObjectId, is_measure: bool, cx: &mut Context<Self>) {
        // Conectar dos columnas manda sobre la seleccion: el usuario esta armando el modelo,
        // no el informe, y un clic que ademas cambiara la matriz seria dos acciones a la vez.
        if self.conexion != Conexion::Apagada {
            if is_measure {
                self.stats =
                    Some("una relacion une columnas, no medidas".to_string());
                cx.notify();
                return;
            }
            match self.conexion {
                Conexion::Apagada => {}
                Conexion::PrimerLado => {
                    self.conexion = Conexion::SegundoLado(column);
                    self.stats = Some("elige la segunda columna".to_string());
                    cx.notify();
                }
                Conexion::SegundoLado(primera) => self.conectar(primera, column, cx),
            }
            return;
        }

        self.recordar();
        if is_measure {
            // El Top N ordena por UNA medida. Cuando esa medida sale de la seleccion, el
            // recorte no tiene por que ordenar y se suelta: dejarlo puesto seria un ajuste
            // encendido que no hace nada.
            if let Some(top) = self.top_n
                && top.measure == column
                && self.selection.measures.contains(&column)
            {
                self.top_n = None;
            }
            if let Some(index) = self.selection.measures.iter().position(|id| *id == column)
            {
                self.selection.measures.remove(index);
            } else {
                self.selection.measures.push(column);
            }
        } else if let Some(index) =
            self.selection.dimensions.iter().position(|id| *id == column)
        {
            self.selection.dimensions.remove(index);
        } else {
            self.selection.dimensions.push(column);
        }
        self.refresh(cx);
    }

    /// Recorre el informe con la seleccion actual.
    /// La columna de fecha que gobierna el rango. La de la tabla de hechos si la hay, si no
    /// la primera fecha de una tabla conectada.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(base) = self.report.clone() else {
            return;
        };
        self.refresh_seq += 1;
        let seq = self.refresh_seq;
        let mut report = base
            .with_dimensions(self.selection.dimensions.clone())
            .with_measures(self.selection.measures.clone())
            // Con dos dimensiones o mas la matriz es una jerarquia y lleva subtotales. Con
            // una, no hay niveles intermedios y encenderlos cambiaria el numero de filas
            // que ven el grid, el drill y los exports sin motivo.
            .with_subtotals(self.selection.dimensions.len() >= 2);
        // El Top N es de la primera dimension: con dos o mas la matriz es una jerarquia y el
        // recorte no tiene una lectura unica, asi que no se aplica. El ajuste SE QUEDA: al
        // volver a una dimension vuelve a regir.
        if let Some(top) = self.top_n
            && self.selection.dimensions.len() == 1
            && self.selection.measures.contains(&top.measure)
        {
            report = report.with_top_n(top);
        }
        // Los filtros activos se vuelven a aplicar por COLUMNA, como un conjunto: dos valores
        // de la misma columna son una union (`IN`), y aplicarlos como dos filtros que se
        // intersectan dejaria la matriz vacia. El informe base es el que tiene el filtro de
        // la ULTIMA pulsacion, y reconstruir desde cero con la lista de filtros hace que
        // quitar uno no deshaga los de despues.
        let mut por_columna: Vec<(ObjectId, Vec<datan_dax_eval::FilterValue>)> = Vec::new();
        for filter in &self.filters {
            let Some(definition) = report.model.column(filter.column) else {
                continue;
            };
            let value = match definition.1.data_type {
                DataType::Integer | DataType::Moneda | DataType::Decimal => {
                    datan_dax_eval::FilterValue::Integer(filter.value.parse().unwrap_or(0))
                }
                // La pastilla de una fecha guarda el DIA desde 1970, que es lo que escribio
                // `valor_legible`. Leerla como texto daba un filtro que nunca coincide.
                // La pastilla de una fecha guarda lo que se pincho, y eso llega como dia
                // desde 1970 o como `YYYY-MM-DD`. `parse_day` lee las dos.
                DataType::Date => {
                    datan_dax_eval::FilterValue::Date(datan_dax_eval::parse_day(&filter.value).unwrap_or(0))
                }
                _ => datan_dax_eval::FilterValue::Text(filter.value.to_string()),
            };
            match por_columna
                .iter_mut()
                .find(|(column, _)| *column == filter.column)
            {
                Some((_, values)) => values.push(value),
                None => por_columna.push((filter.column, vec![value])),
            }
        }
        for (column, values) in por_columna {
            report = report.filtered_to(column, values);
        }
        // El rango de fechas va DESPUES de la igualdad: es otro filtro sobre otra columna y
        // el orden no altera el AND, pero aplicarlo aqui garantiza que el rango se imponga
        // aunque el cross-filter haya tocado la fecha.
        // La ventana anterior se arma sobre una COPIA del informe con los mismos filtros de
        // igualdad, antes de imponer el rango. Si se partiera del informe ya acotado, el
        // rango actual y el anterior se intersectarian y el delta compararia una ventana de
        // doce meses contra un solo dia.
        let previous = match (self.date_months, date_column(&report)) {
            (Some(months), Some(date)) => {
                let today = today();
                let ancla = datan_dax_eval::shift(
                    datan_dax_eval::TimeFunction::DateAdd {
                        units: -1,
                        unit: datan_dax_eval::CalendarUnit::Year,
                    },
                    datan_dax_eval::DateRange::new(today, today),
                    today,
                )
                .start;
                let antes = window_of(ancla, months);
                Some(report.filtered_between(date, antes.start, antes.end))
            }
            // Sin rango activo no hay periodo anterior con el que comparar: el historico
            // completo contra el historico completo un ano antes no es un delta.
            _ => None,
        };
        if let (Some(months), Some(date)) = (self.date_months, date_column(&report))
        {
            let range = window_of(today(), months);
            report = report.filtered_between(date, range.start, range.end);
        }

        let database = self
            .database
            .clone()
            .unwrap_or_else(|| "datan_bench".to_string());
        cx.spawn(async move |this, cx| {
            let built = cx
                .background_spawn(async move { run_report(report, previous, database) })
                .await;
            _ = this.update(cx, |view, cx| {
                // Llego una corrida vieja: hay otra pedida despues y esa manda.
                if view.refresh_seq != seq {
                    return;
                }
                match built {
                    Ok((matrix, stats, cards)) => {
                        // Volver arriba tras filtrar. Sin esto, el usuario pincha una fila
                        // del medio, el resultado baja a tres filas, y se queda mirando la
                        // posicion 400 de una lista de tres: el grid parece vacio.
                        view.scroll.scroll_to_item(0, ScrollStrategy::Top);
                        let formatos = view
                            .report
                            .as_ref()
                            .map(|report| formatos_del_modelo(&report.model))
                            .unwrap_or_default();
                        view.grid = Some(build_grid(&matrix, &formatos));
                        view.matrix = Some(matrix);                        view.stats = Some(stats);
                        view.totals = cards;
                        view.formula_error = None;
                        view.last_saved = None;
                        view.state = State::Ready;
                    }
                    Err(error) => view.state = State::Broken(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Cross-filter: pinchar una celda de una dimension.
    fn drill(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        if self.lectura {
            // El clic no filtra a proposito, y se dice. Un clic que no responde parece una
            // app rota, y este es el gesto que mas se usa.
            self.stats = Some("modo Lectura: el clic no filtra".to_string());
            cx.notify();
            return;
        }
        let Some(report) = self.report.clone() else {
            return;
        };
        let Some(matrix) = self.matrix() else {
            return;
        };
        let Some(header) = matrix.header(column).cloned() else {
            return;
        };
        if !header.is_dimension() {
            // Una medida no se puede filtrar. Se avisa en vez de no hacer nada: un clic que
            // no hace nada parece una app rota.
            self.stats = Some("una medida no se puede usar como filtro".to_string());
            cx.notify();
            return;
        }
        let Some(value) = matrix.cell(row, column).map(str::to_string) else {
            return;
        };

        // Quitar el filtro anterior de esa columna antes de poner el nuevo. Pinchar dos
        // veces la misma columna debe CAMBIAR el valor, no acumular dos filtros que se
        // intersectan y dan una celda vacia.
        self.recordar();
        self.filters.retain(|filter| filter.column != header.column);
        self.filters.push(ActiveFilter {
            column: header.column,
            label: SharedString::from(report.model.column(header.column).map(|(_, c)| c.name.clone()).unwrap_or_default()),
            value: SharedString::from(value),
        });
        self.refresh(cx);
    }

    /// Quita un filtro activo.
    fn clear_filter(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.filters.len() {
            self.filters.remove(index);
            self.refresh(cx);
        }
    }

    fn matrix(&self) -> Option<&Matrix> {
        self.matrix.as_ref()
    }
}

// ---------------------------------------------------------------------------------------------
// Trabajo de fondo
// ---------------------------------------------------------------------------------------------

fn extension_root() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    Path::new(&home).join(".duckdb").join("extensions")
}

/// Abre el motor y lee el modelo.
///
/// La extension de Postgres se precarga desde disco, sin red: si aqui falla, la app tampoco
/// arranca en la maquina de un usuario.
fn open_engine() -> Result<Engine, String> {
    let engine = Engine::open_with_bundled_extensions(":memory:", &extension_root())
        .map_err(|e| format!("no se pudo abrir el motor: {e}"))?;
    engine
        .load_bundled_extension("postgres")
        .map_err(|e| format!("no se pudo cargar la extension de Postgres: {e}"))?;
    Ok(engine)
}

fn attach(engine: &Engine, database: &str) -> Result<(), String> {
    engine
        .execute(&format!(
            "ATTACH 'host=localhost dbname={database}' AS pg (TYPE POSTGRES, READ_ONLY)"
        ))
        .map_err(|e| {
            format!(
                "no se pudo conectar a la base `{database}`: {e}\\n\\n\
                 Comprueba que el servidor este arriba y que la base exista."
            )
        })
}

#[allow(dead_code)]
/// Lo que deja construir el modelo: el modelo, el informe base, lo que no se aplico y las
/// jerarquias resueltas a ids.
type ModeloListo = (Model, Report, Vec<String>, Vec<(String, Vec<ObjectId>)>);

fn build_model(database: &str) -> Result<ModeloListo, String> {
    let engine = open_engine()?;
    attach(&engine, database)?;

    let mut catalog = PostgresCatalog::connect(&format!("host=localhost dbname={database}"))
        .map_err(|e| format!("no se pudo leer el catalogo de Postgres: {e}"))?;
    // `None`: la tabla de hechos la elige la heuristica. Pasar aqui el nombre de la BASE
    // era un bug: buscaba una tabla que se llamara como la base, y fallaba con cualquier
    // conexion. La eleccion de hechos es un paso de UI que todavia no existe.
    let introspection = introspect(
        &engine,
        &Source::new("pg"),
        Some(&mut catalog),
        None,
    )
    .map_err(|e| e.to_string())?;
    let mut model = introspection.model;

    // El modelo del usuario se aplica ENCIMA del introspeccionado. La introspeccion no puede
    // devolver una medida que el usuario escribio, y sin esto un informe guardado con una
    // medida de usuario no la puede resolver al abrir. Un documento corrupto para la conexion
    // a proposito: ahi esta el trabajo que no se puede volver a generar.
    let doc = model_doc::ModelDoc::load(database).map_err(|e| e.to_string())?;
    let mut avisos = doc.apply(&mut model);

    // Las jerarquias no tocan el modelo: son de la PANTALLA. Los niveles se resuelven a ids
    // aca, con el modelo ya aplicado, y lo que no resuelve se avisa como el resto.
    let mut jerarquias = Vec::new();
    for jerarquia in &doc.jerarquias {
        match jerarquia.resolver(&model) {
            Ok(niveles) => jerarquias.push((jerarquia.name.clone(), niveles)),
            Err(aviso) => avisos.push(aviso),
        }
    }

    let report = Report::new(model.clone()).attached_to("pg");
    Ok((model, report, avisos, jerarquias))
}

fn run_report(
    report: Report,
    previous: Option<Report>,
    database: String,
) -> Result<(Matrix, String, Vec<Tarjeta>), String> {
    let started = Instant::now();

    // El motor se abre aqui y se cae al final: `Engine` es `Send` pero no `Sync`, asi que
    // compartirlo entre tareas exigiria un canal. Abrirlo cuesta unos milisegundos y hace el
    // hilo de trabajo simple.
    let engine = open_engine()?;
    attach(&engine, &database)?;

    // El interprete lee de DuckDB las medidas que no se bajan a SQL. El mapa fisico sale del
    // modelo con el mismo calificador que el lowering, asi que los dos caminos leen las
    // mismas tablas.
    let source = DuckSource::new(&engine, &report.model, &report.qualifier);
    let matrix = report.execute(&engine, Some(&source)).map_err(|e| e.to_string())?;
    let totals = report.totals(&engine, Some(&source)).map_err(|e| e.to_string())?;
    // Una pasada extra de totales, sobre la ventana anterior. Las medidas comparten sus
    // grupos de lectura, asi que no son N consultas: son las mismas de `totals` con otro
    // filtro de fecha. Es el mismo modelo, y el mapa fisico tambien.
    let before = match &previous {
        Some(report) => Some(report.totals(&engine, Some(&source)).map_err(|e| e.to_string())?),
        None => None,
    };

    // La linea de la tarjeta. Es UNA consulta extra y no doce: se agrupa por la columna de
    // fecha y los dias se suman en su mes. Sin columna de fecha no hay serie que dibujar.
    let date = date_column(&report);
    let series = match date {
        Some(date) => monthly_series(&report, &engine, date)?,
        None => Series {
            por_medida: vec![None; report.measures.len()],
            consulto: false,
        },
    };
    let queries = report.query_count().unwrap_or(0) + usize::from(series.consulto);

    // Los milisegundos enteros: `241.941792ms` es la duracion de un cronometro, no un dato
    // que alguien lea en la barra de abajo. Y un informe que tarda menos de un milisegundo
    // tardo algo: no se redondea a cero.
    let ms = started.elapsed().as_millis().max(1);
    let stats = format!(
        "{}, {}, {}, {ms} ms",
        plural(queries, "consulta", "consultas"),
        plural(matrix.row_count(), "fila", "filas"),
        plural(matrix.width(), "columna", "columnas"),
    );
    // Los nombres de las medidas, en el MISMO orden que los totales: la tarjeta i es la
    // medida i.
    let formatos = formatos_del_modelo(&report.model);
    let cards = report
        .measures
        .iter()
        .zip(&totals)
        .enumerate()
        .map(|(index, (id, total))| {
            let nombre = report.measure_name(*id).unwrap_or("?").to_string();
            // Un total anterior de cero no da porcentaje: el denominador es cero y el
            // resultado seria infinito. Se dice "nuevo" y no "+inf%".
            let delta = before.as_ref().and_then(|totals| {
                let antes = totals.get(index)?.parse::<f64>().ok()?;
                let ahora = total.parse::<f64>().ok()?;
                if antes.abs() < f64::EPSILON {
                    return None;
                }
                Some((ahora - antes) / antes.abs())
            });
            Tarjeta {
                nombre,
                total: total.clone(),
                delta,
                serie: series.por_medida.get(index).cloned().flatten(),
                formato: formatos.get(id).cloned(),
            }
        })
        .collect();

    Ok((matrix, stats, cards))
}

/// El texto de un valor de filtro, como se lee en la pastilla de la barra.
///
/// La pastilla guarda TEXTO, y de ahi sale el filtro al reconstruir. Un valor de fecha sale
/// como el dia desde 1970, que es lo que la pastilla muestra hoy: la barra de periodo no crea
/// pastillas, crea el rango.
fn valor_legible(value: &datan_dax_eval::FilterValue) -> String {
    use datan_dax_eval::FilterValue;
    match value {
        FilterValue::Text(text) => text.clone(),
        FilterValue::Integer(number) => number.to_string(),
        FilterValue::Decimal(number) => number.to_string(),
        FilterValue::Date(day) => day.to_string(),
        FilterValue::Boolean(flag) => flag.to_string(),
        FilterValue::Blank => "(en blanco)".to_string(),
    }
}


/// La columna de fecha del informe: la del hecho primero, y si no, la primera conectada.
///
/// Va suelta y no como metodo de la vista porque `run_report` corre en el hilo de fondo.
/// El titulo de un visual: la medida y la dimension que lo arman.
///
/// Los nombres son los del modelo y no se traducen. Un titulo que dice "Ventas" cuando la
/// medida se llama `product_boleta quantity` manda al usuario a buscar un campo que no existe.
fn titulo_de(medida: &str, dimension: &str) -> String {
    match (medida.is_empty(), dimension.is_empty()) {
        (false, false) => format!("{medida} por {dimension}"),
        (false, true) => medida.to_string(),
        _ => "sin datos".to_string(),
    }
}

fn date_column(report: &Report) -> Option<ObjectId> {
    let model = &report.model;
    let fact = report.fact_table().ok();
    let connected = |id: ObjectId| match fact {
        Some(fact_id) => fact_id == id || model.join_path(fact_id, id).is_some(),
        None => false,
    };
    let mut dates: Vec<(ObjectId, ObjectId, bool)> = model
        .tables
        .iter()
        .filter(|t| connected(t.id))
        .flat_map(|t| t.columns.iter().map(move |c| (t.id, c)))
        .filter(|(_, c)| matches!(c.data_type, DataType::Date | DataType::DateTime))
        // El tercer valor marca `Date` puro. `created_at` es un timestamp de insercion que
        // vale como fecha para el motor, pero cuando la misma tabla trae la fecha de
        // NEGOCIO (`boleta.fecha`), esa gana: un timestamp de auditoria concentra todo en
        // el dia en que se cargo la tabla.
        .map(|(table, column)| (table, column.id, matches!(column.data_type, DataType::Date)))
        .collect();
    // La de la tabla de hechos primero, y dentro de cada tabla la `Date` pura antes que el
    // timestamp. La introspeccion entrega las columnas por orden de declaracion, y `fecha`
    // venia despues de `created_at`: sin este orden, el periodo caia siempre en el
    // timestamp y toda la ventana de doce meses daba un solo dia.
    dates.sort_by_key(|(table, _, es_date)| (Some(*table) != fact, !*es_date));
    dates.first().map(|(_, column, _)| *column)
}

/// El nivel al que se llega al taladrar desde la dimension actual.
///
/// La dimension tiene que ser UN nivel de UNA jerarquia: el drill reemplaza el nivel, no lo
/// apila. `abajo` pide el siguiente; `arriba`, el anterior, y desde el primero no hay. Una
/// dimension que no es nivel de ninguna jerarquia tampoco taladra.
fn drill_objetivo(
    jerarquias: &[(String, Vec<ObjectId>)],
    dimension: ObjectId,
    abajo: bool,
) -> Option<ObjectId> {
    for (_, niveles) in jerarquias {
        let Some(index) = niveles.iter().position(|nivel| *nivel == dimension) else {
            continue;
        };
        let destino = if abajo { index + 1 } else { index.checked_sub(1)? };
        return niveles.get(destino).copied();
    }
    None
}

/// Si la forma de salida de una medida suma.
///
/// Las sumas y las restas de hojas aditivas suman: los meses de `SUM(a) - SUM(b)` se pueden
/// sumar y dan el total del anio. Una division no: doce cocientes sumados no son el cociente
/// del anio.
fn forma_aditiva(shape: &Shape, kinds: &[AtomKind]) -> bool {
    match shape {
        Shape::Atom(index) => matches!(
            kinds.get(*index),
            Some(AtomKind::Sum | AtomKind::Count | AtomKind::CountRows)
        ),
        // Un numero suelto no cambia con los meses.
        Shape::Number(_) => true,
        Shape::Binary { op, left, right } => {
            matches!(op, BinaryOp::Add | BinaryOp::Subtract)
                && forma_aditiva(left, kinds)
                && forma_aditiva(right, kinds)
        }
        // Una llamada no es aditiva sin razonarlo funcion por funcion, y una racha de meses
        // equivocada en una tarjeta es una mentira con forma de dato. `DIVIDE` es un cociente,
        // y `IF` y `COALESCE` eligen: ninguna es la suma de sus meses.
        Shape::Call { .. } => false,
    }
}

/// Si sumar los meses da el total de la medida.
///
/// La medida guarda su texto DAX y no su forma. La forma la conoce el extractor de atomos que
/// arma el plan de consulta, asi que se le pregunta a el. Adivinar por el texto daria por
/// aditiva a `SUM(a) / SUM(b)`, y esa linea en la tarjeta seria una mentira con forma de dato.
fn es_aditiva(
    expression: &str,
    model: &Model,
    context: &datan_dax_eval::FilterContext,
    grouping: &[ObjectId],
) -> bool {
    let Ok(atomos) = datan_dax_eval::extract_atoms(expression, model, context, grouping) else {
        return false;
    };
    // Sin `outputs` la medida no es una combinacion de agregaciones: `SUMX` con contexto de
    // fila, por ejemplo. No se puede saber si sumar los meses da el total, y una linea que no
    // se sabe si es cierta no se dibuja.
    if atomos.outputs.is_empty() {
        return false;
    }
    let kinds: Vec<AtomKind> = atomos.atoms.iter().map(|atom| atom.kind).collect();
    atomos
        .outputs
        .iter()
        .all(|output| forma_aditiva(&output.shape, &kinds))
}

/// La linea mensual de cada medida y si la consulta que la arma se hizo.
///
/// Una ranura por medida y en el orden de `report.measures`: la tarjeta i lee la ranura i. Sin
/// columna de fecha no hay consulta y todas las ranuras quedan vacias.
struct Series {
    por_medida: Vec<Option<Vec<f64>>>,
    consulto: bool,
}

/// La serie mensual de cada medida aditiva, para la linea de su tarjeta.
///
/// Es UNA consulta extra, agrupada por la columna de fecha: el motor agrupa por dia y los
/// dias se suman en el mes que les toca. Una consulta por mes serian doce.
fn monthly_series(report: &Report, engine: &Engine, date: ObjectId) -> Result<Series, String> {
    let mut series: Vec<Option<Vec<f64>>> = vec![None; report.measures.len()];
    let indices: Vec<usize> = report
        .measure_definitions()
        .map_err(|error| error.to_string())?
        .iter()
        .enumerate()
        .filter(|(_, measure)| {
            es_aditiva(
                &measure.expression,
                &report.model,
                &report.filters,
                &report.dimensions,
            )
        })
        .map(|(index, _)| index)
        .collect();
    if indices.is_empty() {
        return Ok(Series {
            por_medida: series,
            consulto: false,
        });
    }

    let measures: Vec<ObjectId> = indices.iter().map(|index| report.measures[*index]).collect();
    let serie = report
        .clone()
        .with_dimensions(vec![date])
        .with_measures(measures);
    let source = DuckSource::new(engine, &report.model, &report.qualifier);
    let matrix = serie.execute(engine, Some(&source)).map_err(|error| error.to_string())?;

    // La clave es "YYYY-MM" y `BTreeMap` ordena por clave, asi que el orden alfabetico de
    // esas claves ES el orden cronologico. No hace falta convertir el mes a numero.
    let mut meses: std::collections::BTreeMap<String, Vec<f64>> = std::collections::BTreeMap::new();
    for fila in 0..matrix.row_count() {
        let Some(celda) = matrix.cell(fila, 0) else {
            continue;
        };
        // La fecha llega como dia desde 1970 cuando la resuelve el interprete, y como
        // `YYYY-MM-DD` cuando la resuelve el SQL. Las dos formas se leen igual.
        let clave = match celda.parse::<i32>() {
            Ok(dia) => datan_dax_eval::format_date(dia),
            Err(_) => celda.to_string(),
        };
        let mes = clave.get(..7).unwrap_or(&clave).to_string();
        let bucket = meses
            .entry(mes)
            .or_insert_with(|| vec![0.0; indices.len()]);
        for (hueco, columna) in bucket.iter_mut().zip(1usize..) {
            if let Some(valor) = matrix.cell(fila, columna).and_then(|v| v.parse::<f64>().ok()) {
                *hueco += valor;
            }
        }
    }

    let meses: Vec<Vec<f64>> = meses.into_values().collect();
    for (desplazamiento, index) in indices.iter().enumerate() {
        series[*index] = Some(
            meses
                .iter()
                .map(|mes| mes.get(desplazamiento).copied().unwrap_or(0.0))
                .collect(),
        );
    }
    Ok(Series {
        por_medida: series,
        consulto: true,
    })
}

/// La seleccion por defecto: la primera dimension de texto y las primeras medidas.
///
/// Es una seleccion, no una opinion. El usuario la cambia desde la barra lateral.
fn suggested(model: &Model) -> Selection {
    let dimensions = model
        .tables
        .iter()
        .filter(|t| matches!(t.kind, TableKind::Dimension))
        .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
        .filter(|(t, c)| matches!(c.data_type, DataType::Text) && !t.key_columns.contains(&c.id))
        .map(|(_, c)| c.id)
        .take(1)
        .collect();

    let measures = model
        .measures
        .iter()
        .filter(|m| m.expression.starts_with("SUM"))
        .map(|m| m.id)
        .take(2)
        .collect();

    Selection {
        dimensions,
        measures,
    }
}

fn build_grid(
    matrix: &Matrix,
    formatos: &std::collections::HashMap<ObjectId, formato::Formato>,
) -> Grid {
    let width = matrix.width();
    let columns: Vec<Column> = (0..width)
        .map(|column| {
            let header = matrix
                .header(column)
                .map(|h| SharedString::from(h.label.clone()))
                .unwrap_or_default();
            let numeric = matrix
                .header(column)
                .map(|h: &Header| !h.is_dimension())
                .unwrap_or(false);
            // La celda de una medida con `format_string` sale formateada para LEERSE. El
            // texto crudo sigue siendo el que viaja: el clic que filtra lee `view.matrix`.
            let formato = matrix
                .header(column)
                .and_then(|h| h.measure)
                .and_then(|id| formatos.get(&id));
            let values: Vec<SharedString> = (0..matrix.row_count())
                .map(|row| {
                    let texto = matrix.cell(row, column).unwrap_or("");
                    let mostrado = match (formato, texto.parse::<f64>()) {
                        (Some(formato), Ok(value)) => formato.aplicar(value),
                        _ => texto.to_string(),
                    };
                    SharedString::from(mostrado)
                })
                .collect();
            Column {
                header,
                values: Arc::from(values.into_boxed_slice()),
                numeric,
            }
        })
        .collect();

    Grid {
        columns: Arc::from(columns.into_boxed_slice()),
        kinds: Arc::from(
            (0..matrix.row_count())
                .map(|row| matrix.row_kind(row))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        row_count: matrix.row_count(),
    }
}

/// El formato entendible de cada medida, por su id. Las medidas sin `format_string`, o con
/// uno que no se reconoce, no entran.
fn formatos_del_modelo(model: &Model) -> std::collections::HashMap<ObjectId, formato::Formato> {
    model
        .measures
        .iter()
        .filter_map(|measure| {
            let texto = measure.format_string.as_deref()?;
            let formato = formato::Formato::parse(texto)?;
            Some((measure.id, formato))
        })
        .collect()
}



/// Un boton con clic.
///
/// El id no es opcional. La via imperativa, `element.interactivity().on_click(...)`,
/// registra el listener pero gpui no despacha el clic: el elemento se pinta, el cursor
/// cambia, y el clic no hace nada. Es exactamente lo que paso en pantalla: las conexiones se
/// veian bien y no se podian elegir.
///
/// Los clics se despachan por la via STATEFUL: `div().id(...)` convierte el `Div` en
/// `Stateful<Div>`, y el `on_click` de `StatefulInteractiveElement` si dispara. Cada id
/// tiene que ser unico y estable dentro de la ventana.
fn clickable(
    theme: &Theme,
    id: SharedString,
    label: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    clickable_peso(theme, Peso::Secundario, id, label, on_click)
}

/// Un boton con el peso que se le pida.
fn clickable_peso(
    theme: &Theme,
    peso: Peso,
    id: SharedString,
    label: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    components::boton(theme, peso, id, label, on_click)
}

fn retry_button(theme: &Theme, database: &str, cx: &mut Context<Datan>) -> AnyElement {
    let database = database.to_string();
    clickable_peso(
        theme,
        Peso::Primario,
        "retry".into(),
        "Reintentar".to_string(),
        cx.listener(move |view, _event, _window, cx| view.connect(&database, cx)),
    )
}

fn clear_button(theme: &Theme, cx: &mut Context<Datan>) -> AnyElement {
    clickable_peso(theme, Peso::Fantasma, "clear".into(), "Limpiar todo".to_string(), cx.listener(
        |view, _event, _window, cx| {
            view.filters.clear();
            view.refresh(cx);
        },
    ))
}

/// Una pastilla de filtro activo. Pincharla quita el filtro.
/// `COUNT(*)` por tabla, en el hilo de trabajo.
///
/// Una consulta por tabla y no un `UNION ALL` generado: el SQL tiene que poder fallar por
/// tabla, y decir cual fallo es la mitad del diagnostico. Una tabla que falla cuenta 0 y no
/// tumba el panel entero.
fn count_rows(
    tables: &[(ObjectId, String)],
    database: &str,
) -> Result<std::collections::BTreeMap<ObjectId, i64>, String> {
    let engine = open_engine()?;
    attach(&engine, database)?;
    let mut out = std::collections::BTreeMap::new();
    for (id, table) in tables {
        // El mismo nombre que usa el motor: la tabla del modelo, con el calificador de la
        // base adjunta. Sin comillas, una tabla con guion o palabra reservada da un error
        // de sintaxis que parece del motor.
        let sql = format!("SELECT COUNT(*) FROM pg.\"{}\"", table.replace('"', ""));
        let Ok(batches) = engine.query_arrow(&sql) else {
            out.insert(*id, 0);
            continue;
        };
        let count = batches
            .first()
            .and_then(|batch| count_of(batch))
            .unwrap_or(0);
        out.insert(*id, count);
    }
    Ok(out)
}

/// El `COUNT(*)` de la primera columna de un lote.
fn count_of(batch: &datan_storage::arrow::record_batch::RecordBatch) -> Option<i64> {
    use datan_storage::arrow::array::{Array, Int64Array};
    batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .map(|values| values.value(0))
}

/// Segundos desde la epoca, para el "actualizado hace 2 min".
fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|e| e.as_secs() as i64)
        .unwrap_or(0)
}

/// Un conteo con su sustantivo en el numero que corresponde: "1 fila", "3 filas".
///
/// Un "(es)" en la interfaz es una decision que nadie tomo, y "1 visualizacion(es)" se lee
/// como un error del programa.
fn plural(cantidad: usize, singular: &str, plural: &str) -> String {
    if cantidad == 1 {
        format!("{cantidad} {singular}")
    } else {
        format!("{cantidad} {plural}")
    }
}

/// Un numero con los puntos de miles, como los muestra la gente: 1.284.310.
fn count_text(value: i64) -> String {
    let out = formato::con_puntos(&value.unsigned_abs().to_string());
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// "hace 2 min", para la cabecera del lienzo.
fn ago_text(seconds: i64) -> String {
    let minutes = seconds / 60;
    match minutes {
        0 => "hace instantes".to_string(),
        1 => "hace 1 min".to_string(),
        m if m < 60 => format!("hace {m} min"),
        h if h < 24 => format!("hace {h} h"),
        d => format!("hace {d} d"),
    }
}

/// La ventana de `months` meses que termina HOY.
///
/// `shift` conserva el TAMANO del rango que recibe, y un rango de un solo dia se desplaza a
/// un solo dia. Pedirle una ventana de seis meses devuelve el dia de hoy hace seis meses, y
/// el informe entero se reduce a las ventas de ESE dia sin decir nada. El inicio sale de
/// `shift` y el fin es hoy: el rango es inclusivo en los dos extremos, como "ultimos tres
/// meses" en la cabeza de cualquiera.
fn window_of(today: i32, months: u8) -> datan_dax_eval::DateRange {
    let inicio = datan_dax_eval::shift(
        datan_dax_eval::TimeFunction::DateAdd {
            units: -(months as i64),
            unit: datan_dax_eval::CalendarUnit::Month,
        },
        datan_dax_eval::DateRange::new(today, today),
        today,
    )
    .start;
    datan_dax_eval::DateRange::new(inicio, today)
}

/// El boton de rango: 3M, 6M, 12M o Todo. `months` 0 es Todo.
fn range_button(
    theme: &Theme,
    label: &str,
    active: bool,
    months: u8,
    cx: &mut Context<Datan>,
) -> impl IntoElement {
    components::segmento(
        theme,
        SharedString::from(format!("range-{months}")),
        label.to_string(),
        active,
        cx.listener(move |view, _event, _window, cx| {
            let next = (months != 0).then_some(months);
            if view.date_months == next {
                return;
            }
            view.recordar();
            view.date_months = next;
            view.refresh(cx);
        }),
    )
}

/// Un boton del Top N.
///
/// Los tres van en un control segmentado porque son excluyentes, y el que esta encendido se
/// APAGA al volver a pincharlo: un boton de segmento que no se puede soltar deja el recorte
/// puesto sin una forma de quitarlo que se vea.
fn top_button(
    theme: &Theme,
    label: &str,
    count: usize,
    abajo: bool,
    actual: &Option<datan_report::TopN>,
    cx: &mut Context<Datan>,
) -> impl IntoElement {
    let active = actual
        .map(|top| top.count == count && top.abajo == abajo)
        .unwrap_or(false);
    components::segmento(
        theme,
        SharedString::from(format!("top-{count}-{abajo}")),
        label.to_string(),
        active,
        cx.listener(move |view, _event, _window, cx| {
            view.set_top_n((!active).then_some((count, abajo)), cx);
        }),
    )
}

/// El dia de hoy en dias desde 1970. Es UTC: en Chile puede ser el dia anterior, y en un
/// rango de meses un dia de borde no cambia el informe.
fn today() -> i32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| (elapsed.as_secs() / 86_400) as i32)
        .unwrap_or(0)
}

/// Un texto coincide con la busqueda. Vacio coincide con todo: sin busqueda no se filtra.
/// Si el texto coincide con lo que se busca.
///
/// Sin texto que buscar la respuesta es NO: `contains("")` es verdadero en Rust, y una
/// coincidencia universal hace que "sin busqueda" signifique "abre todas las tablas y muestra
/// todos los campos". Que todo se muestre cuando no se busca es una decision de quien llama.
fn hit(text: &str, query: &str) -> bool {
    !query.is_empty() && text.to_lowercase().contains(query)
}

fn filter_pill(
    theme: &Theme,
    filter: &ActiveFilter,
    index: usize,
    cx: &mut Context<Datan>,
) -> AnyElement {
    components::pastilla(
        theme,
        SharedString::from(format!("pill-{index}")),
        format!("{} = {}", filter.label, filter.value),
        cx.listener(move |view, _event, _window, cx| {
            view.clear_filter(index, cx);
        }),
    )
}

/// Una tarjeta KPI: el nombre de la medida, el valor grande y el delta contra el mismo
/// periodo del ano anterior.
///
/// El delta se muestra siempre que haya con que comparar, en verde si sube y en rojo si baja.
/// Sin periodo anterior se dice con esas palabras y no se esconde el hueco: una tarjeta sin
/// comparacion no dice si el numero esta bien o mal.
/// Una tarjeta KPI: nombre, total, variacion y la linea de los meses.
///
/// La `serie` es `None` cuando la medida no es aditiva o no hay columna de fecha. Una
/// tarjeta sin linea no reserva el alto de una con linea: el hueco al lado del valor dice
/// mas que una linea plana en cero.
struct Tarjeta {
    nombre: String,
    total: String,
    delta: Option<f64>,
    serie: Option<Vec<f64>>,
    /// El `format_string` de la medida, si se entiende. Sin el, el total va abreviado
    /// (`1,3M`), que es lo que cabe en una tarjeta.
    formato: Option<formato::Formato>,
}

fn tarjeta_kpi(
    theme: &Theme,
    tarjeta: &Tarjeta,
    formato: datan_report::saved::SavedFormato,
) -> AnyElement {
    let valor = if tarjeta.total.is_empty() {
        "-".to_string()
    } else {
        // Con `format_string` manda el formato de la medida, exacto y sin abreviar. Sin el,
        // la abreviatura que cabe: `1,3M`.
        match tarjeta.formato.as_ref().and_then(|formato| {
            tarjeta
                .total
                .parse::<f64>()
                .ok()
                .map(|value| formato.aplicar(value))
        }) {
            Some(texto) => texto,
            None => tarjeta
                .total
                .parse::<f64>()
                .map(format_value)
                .unwrap_or_else(|_| tarjeta.total.clone()),
        }
    };
    let comparacion: AnyElement = match tarjeta.delta.filter(|_| formato.comparacion) {
        Some(delta) => {
            // `chart_bullish` y `chart_bearish` son los dos colores con significado del
            // tema: verde y rojo. `danger_foreground` NO sirve para esto: es el color del
            // texto que va ENCIMA de un fondo rojo, o sea casi blanco, y una bajada se veia
            // como un texto invisible.
            let (color, flecha) = if delta >= 0.0 {
                (theme.chart_bullish, "\u{2191}")
            } else {
                (theme.chart_bearish, "\u{2193}")
            };
            div()
                .text_size(px(tokens::LETRA_NOTA))
                .text_color(color)
                .child(format!("{flecha} {:+.1}% vs ano anterior", delta * 100.0))
                .into_any_element()
        }
        None => components::nota(theme, "sin periodo anterior").into_any_element(),
    };

    let columna = div()
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        // El valor y su variacion se truncan antes que la linea: la linea sola, sin el numero
        // al lado, no dice nada.
        .overflow_hidden()
        .child(
            div()
                .text_size(px(tokens::LETRA_VALOR))
                .font_semibold()
                .truncate()
                .child(valor),
        )
        .child(comparacion);

    let mut cuerpo = div().flex().flex_col().gap_2();
    // El nombre de la medida es el titulo de la tarjeta: sin el, el valor es un numero sin
    // decir que mide.
    if formato.titulo {
        cuerpo = cuerpo.child(components::nota(theme, tarjeta.nombre.clone()));
    }
    cuerpo = cuerpo.child(
        div()
            .flex()
            .items_end()
            .gap_3()
            .child(columna),
    );

    if formato.comparacion
        && let Some(serie) = tarjeta.serie.as_deref().filter(|serie| serie.len() > 1)
    {
        cuerpo = cuerpo.child(div().flex().justify_end().child(sparkline(serie)));
    }

    components::superficie(theme, formato.fondo)
        .flex()
        .flex_1()
        .p_4()
        .flex_col()
        .child(cuerpo)
        .into_any_element()
}

/// La linea de la tarjeta: los meses de la medida, sin ejes ni etiquetas.
///
/// A 96 x 28 px no cabe un grafico: la rejilla, los ejes y la leyenda miden mas que el dato.
/// La forma de la linea es lo unico que se mira en una tarjeta, y para la forma alcanza un
/// `canvas` con dos trazos.
fn sparkline(serie: &[f64]) -> AnyElement {
    let datos: Vec<f64> = serie.to_vec();
    let linea = tokens::serie(0);
    let relleno = linea.opacity(0.12);
    // El tamano va EN el canvas y no en un div de afuera: un canvas se mide por su
    // contenido, que no tiene, asi que hereda un alto de cero y todos los puntos caen en la
    // misma `y`. La linea sale recta y perfecta, que es la peor clase de defecto.
    canvas(
        move |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let Some((min, max)) = datos.iter().fold(None::<(f64, f64)>, |acc, value| {
                Some(match acc {
                    None => (*value, *value),
                    Some((lo, hi)) => (lo.min(*value), hi.max(*value)),
                })
            }) else {
                return;
            };
            // Una medida constante da una division por cero. La linea queda al medio,
            // que es lo que dice un valor que no cambia.
            let rango = if max > min { max - min } else { 1.0 };
            let ultimo = datos.len().saturating_sub(1).max(1);
            let punto = |indice: usize, valor: f64| {
                let x = indice as f32 / ultimo as f32;
                let y = 1.0 - ((valor - min) / rango) as f32;
                point(
                    bounds.origin.x + bounds.size.width * x,
                    bounds.origin.y + bounds.size.height * y,
                )
            };

            // El area primero y la linea encima: al reves la linea queda tapada por el
            // relleno.
            let mut area = PathBuilder::fill();
            area.move_to(punto(0, datos[0]));
            for (indice, valor) in datos.iter().enumerate().skip(1) {
                area.line_to(punto(indice, *valor));
            }
            area.line_to(punto(datos.len() - 1, min));
            area.close();
            if let Ok(path) = area.build() {
                window.paint_path(path, relleno);
            }

            let mut trazo = PathBuilder::stroke(px(1.5));
            trazo.move_to(punto(0, datos[0]));
            for (indice, valor) in datos.iter().enumerate().skip(1) {
                trazo.line_to(punto(indice, *valor));
            }
            if let Ok(path) = trazo.build() {
                window.paint_path(path, linea);
            }
        },
    )
    .w(px(96.0))
    .h(px(28.0))
    .flex_shrink_0()
    .into_any_element()
}

fn sidebar_item(
    theme: &Theme,
    label: String,
    is_on: bool,
    column: ObjectId,
    is_measure: bool,
    cx: &mut Context<Datan>,
) -> AnyElement {
    components::fila_dato(
        theme,
        SharedString::from(format!(
            "side-{}-{}",
            if is_measure { "m" } else { "d" },
            column.0
        )),
        label,
        is_on,
        cx.listener(move |view, _event, _window, cx| {
            view.toggle(column, is_measure, cx);
        }),
    )
}

impl Render for Datan {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let main = match &self.state {
            State::Choosing => div()
                .flex()
                .flex_1()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .child(
                    div()
                        .font_semibold()
                        .text_size(px(tokens::LETRA_H1))
                        .child("Datan"),
                )
                .child(
                    div()
                        .text_size(px(tokens::LETRA_CUERPO))
                        .text_color(theme.muted_foreground)
                        .child("Elige una conexion"),
                )
                .child(self.render_connections(cx))
                .into_any_element(),
            State::Connecting => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Conectando..."),
                )
                .into_any_element(),
            State::Opening => self.render_reports(cx),
            State::Failed(error) | State::Broken(error) => {
                let titulo = match &self.state {
                    State::Failed(_) => "No se pudo conectar",
                    _ => "La conexion funciona, el informe fallo",
                };
                div()
                    .flex()
                    .flex_1()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        div().text_color(theme.muted_foreground).child(titulo),
                    )
                    .child(div().child(error.clone()))
                    .child(
                        retry_button(&theme, self.database.as_deref().unwrap_or("datan_bench"), cx),
                    )
                    .into_any_element()
            }
            _ => self.render_report(window, cx),
        };

        div()
            .flex()
            .size_full()
            .bg(theme.muted)
            .text_color(theme.foreground)
            .child(main)
    }
}

impl Datan {
    /// La barra lateral y el grid.
    fn render_report(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(report) = self.report.clone() else {
            return div().into_any_element();
        };
        let model = report.model.clone();

        // --- Barra lateral: dimensiones y medidas ---
        //
        // Solo tablas CONECTADAS a los hechos. Una base puede tener varios esquemas que no
        // se tocan, y ofrecer una columna de un esquema ajeno produce un informe que no
        // baja a SQL: el usuario ve un error de motor que es en realidad un informe
        // malformado.
        let fact = report.fact_table().ok();
        let connected = |table: &datan_model::Table| -> bool {
            match fact {
                Some(fact_id) => fact_id == table.id || model.join_path(fact_id, table.id).is_some(),
                None => false,
            }
        };
        // --- Busqueda del panel Datos ---
        //
        // Filtra por lo que se LEE, no por el nombre tecnico: buscar "fecha" tiene que
        // encontrar la columna Fecha aunque su tabla se llame dim_tiempo.
        if self.search.is_none() {
            self.search = Some(cx.new(|cx| InputState::new(window, cx).placeholder("Buscar campos")));
        }
        let query = self
            .search
            .as_ref()
            .map(|state| state.read(cx).value().to_string())
            .unwrap_or_default();
        let query = query.trim().to_lowercase();

        let dimensions: Vec<ObjectId> = model
            .tables
            .iter()
            .filter(|t| matches!(t.kind, TableKind::Dimension) && connected(t))
            .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
            // Una columna oculta (`is_hidden` del modelo del usuario) no se ofrece: es lo
            // que hace que una clave tecnica o un codigo sin nombre no estorben en el panel.
            .filter(|(t, c)| {
                !t.key_columns.contains(&c.id)
                    && !c.is_hidden
                    && !matches!(c.data_type, DataType::Date)
            })
            .filter(|(t, c)| query.is_empty() || hit(&c.name, &query) || hit(&t.name, &query))
            .map(|(_, c)| c.id)
            .collect();
        // Las tablas que aportan campos, la de hechos primero: es la que el informe agrupa
        // y la primera que uno quiere ver abierta.
        let mut tablas: Vec<&datan_model::Table> = model
            .tables
            .iter()
            .filter(|t| connected(t))
            .collect();
        tablas.sort_by_key(|t| t.id != fact.unwrap_or(ObjectId(0)));

        let measures: Vec<ObjectId> = model
            .measures
            .iter()
            .filter(|m| query.is_empty() || hit(&m.name, &query))
            .map(|m| m.id)
            .collect();

        // El alto de la zona de trabajo sale de la ventana: `h_full` (100 %) no se resuelve
        // contra un contenedor con `flex_1`, y una columna sin alto definido mide lo que mide
        // su contenido. En el panel eso deja el pie flotando a media altura. En el lienzo es
        // peor: la tabla, que mide 210 px, se sale de la ventana y se dibuja DEBAJO de la
        // barra de pestanas.
        let alto_panel =
            window.viewport_size().height - px(tokens::BARRA_SUPERIOR) - px(tokens::ALTO_PESTANAS);
        let sidebar = div()
            .flex()
            .w(px(tokens::PANEL_DATOS))
            .h(alto_panel)
            .flex_shrink_0()
            .bg(theme.background)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(tokens::ALTO_CABECERA))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(components::titulo_tarjeta(&theme, "Datos"))
                    .child(div().flex_1())
                    .child(components::nota(
                        &theme,
                        plural(tablas.len(), "tabla", "tablas"),
                    )),
            )
            .child(div().px_3().py_2().flex_shrink_0().child(
                // La lupa va DENTRO del campo y el campo dibuja su propio borde: un icono
                // afuera y una caja alrededor hacen dos marcos para un solo control.
                Input::new(self.search.as_ref().expect("recien creada"))
                    .h(px(tokens::ALTO_CONTROL))
                    .cleanable(true)
                    .prefix(components::icono(
                        IconName::Search,
                        tokens::LETRA_ICONO,
                        theme.muted_foreground,
                    )),
            ))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        div()
                            .overflow_y_scrollbar()
                            .px_2()
                            .py_2()
                            .child(self.render_tables(&tablas, &dimensions, &model, &query, cx))
                    .child(self.render_sidebar_section(
                        plural(measures.len(), "medida", "medidas"),
                        &measures,
                        true,
                        &model,
                        cx,
                    ))
                    .child(self.render_relaciones(&model, cx))
                    .child(if query.is_empty() || !tablas.is_empty() || !measures.is_empty() {
                        div().into_any_element()
                    } else {
                        div()
                            .px_2()
                            .text_size(px(tokens::LETRA_TITULO))
                            .text_color(theme.muted_foreground)
                            .child(format!("Sin coincidencias para \"{query}\"."))
                            .into_any_element()
                    }),
                    ),
            )
            .child(
                div()
                    .p_2()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(components::boton_ancho(
                        &theme,
                        "obtener-datos",
                        Some(IconName::Plus),
                        "Obtener datos",
                        cx.listener(|view, _event, _window, cx| {
                            view.state = State::Choosing;
                            cx.notify();
                        }),
                    )),
            );

        // --- Cabecera del lienzo ---
        //
        // Arriba, de que informe son estos numeros: la base, cuando se contaron las filas y
        // sobre que columna filtra el periodo. Abajo, el titulo, y solo el titulo. Compartir
        // fila con las etiquetas convertia el titulo en una etiqueta mas: centrado entre dos
        // bloques de texto gris, se lee como el encabezado de la nada.
        let mut fila_etiquetas = div().flex().items_center().gap_2().w_full();
        fila_etiquetas = fila_etiquetas.child(
            div()
                .px_2()
                .py_1()
                .rounded_xl()
                .bg(theme.muted)
                .text_size(px(tokens::LETRA_ETIQUETA))
                .child(
                    self.database
                        .clone()
                        .unwrap_or_else(|| "sin base".to_string()),
                ),
        );
        fila_etiquetas = fila_etiquetas.child(components::nota(
            &theme,
            match self.counted_at {
                Some(at) => format!("Actualizado {}", ago_text(epoch_now() - at)),
                None => "Contando filas".to_string(),
            },
        ));
        fila_etiquetas = fila_etiquetas.child(div().flex_1());

        // Un rango de fechas que no filtra nada y no lo dice es la falla silenciosa de
        // siempre. Sin columna de fecha conectada no hay control de periodo.
        if let Some((_, columna)) = date_column(&report).and_then(|date| model.column(date)) {
            fila_etiquetas = fila_etiquetas.child(components::nota(
                &theme,
                format!("sobre {}", columna.name),
            ));
            // Los rangos van en un control segmentado porque son excluyentes: elegir 3M
            // apaga 6M. Una fila de botones con borde se lee como cuatro acciones sueltas, y
            // no dice cual esta elegida.
            let mut rangos = components::caja_segmentada(&theme);
            for months in [3u8, 6, 12] {
                let label = format!("{months}M");
                rangos = rangos.child(range_button(
                    &theme,
                    &label,
                    self.date_months == Some(months),
                    months,
                    cx,
                ));
            }
            rangos = rangos.child(range_button(
                &theme,
                "Todo",
                self.date_months.is_none(),
                0,
                cx,
            ));
            fila_etiquetas = fila_etiquetas.child(rangos);
        }

        // Actualizar vive al lado del periodo: las dos cosas cambian QUE datos se ven, y
        // estaban en dos extremos de la ventana.
        fila_etiquetas = fila_etiquetas.child(components::boton_icono(
            &theme,
            "refresh",
            IconName::RefreshCw,
            cx.listener(|view, _event, _window, cx| view.refresh(cx)),
        ));

        // El drill vive ahi tambien: cambia QUE nivel de la jerarquia se ve. Con la
        // seleccion en un nivel de una jerarquia, bajar lo cambia por el siguiente y subir
        // por el anterior; en cualquier otro caso los botones estan apagados, porque el
        // drill reemplaza UN nivel y no sabe que hacer con dos.
        if self.selection.dimensions.len() == 1 && !self.lectura {
            let actual = self.selection.dimensions[0];
            let abajo = drill_objetivo(&self.jerarquias, actual, true);
            let arriba = drill_objetivo(&self.jerarquias, actual, false);
            fila_etiquetas = fila_etiquetas.child(match abajo {
                Some(nivel) => components::boton_icono(
                    &theme,
                    "drill-down",
                    IconName::ChevronsDown,
                    cx.listener(move |view, _event, _window, cx| view.drill_nivel(nivel, cx)),
                ),
                None => components::boton_icono_apagado(&theme, IconName::ChevronsDown),
            });
            fila_etiquetas = fila_etiquetas.child(match arriba {
                Some(nivel) => components::boton_icono(
                    &theme,
                    "drill-up",
                    IconName::ChevronsUp,
                    cx.listener(move |view, _event, _window, cx| view.drill_nivel(nivel, cx)),
                ),
                None => components::boton_icono_apagado(&theme, IconName::ChevronsUp),
            });
        }

        // El Top N vive en la misma fila que el periodo y el drill: las tres cosas cambian
        // QUE filas se ven. La medida que ordena se dice al lado, porque el boton solo dice
        // cuantas filas quedan, no por que.
        if self.selection.dimensions.len() == 1
            && !self.lectura
            && let Some(medida) = self.selection.measures.first()
        {
            fila_etiquetas = fila_etiquetas.child(components::nota(
                &theme,
                format!("top por {}", self.model_measure_name(*medida)),
            ));
            let mut topes = components::caja_segmentada(&theme);
            for (label, count, abajo) in [("Top 5", 5usize, false), ("Top 10", 10, false), ("Ultimos 5", 5, true)]
            {
                topes = topes.child(top_button(&theme, label, count, abajo, &self.top_n, cx));
            }
            fila_etiquetas = fila_etiquetas.child(topes);
        }

        let cabecera = div()
            .flex()
            .flex_shrink_0()
            .flex_col()
            .gap_2()
            .px_4()
            .pt_4()
            .pb_2()
            .child(fila_etiquetas)
            .child(
                div()
                    .text_size(px(tokens::LETRA_H1))
                    .font_semibold()
                    .child(self.report_title()),
            );

        // --- Filtros activos ---
        let mut filter_bar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border);

        if self.filters.is_empty() {
            filter_bar = filter_bar.child(
                div()
                    .text_size(px(tokens::LETRA_ETIQUETA))
                    .text_color(theme.muted_foreground)
                    .child("Sin filtros. Pincha una celda de una dimension."),
            );
        } else {
            for index in 0..self.filters.len() {
                let filter = self.filters[index].clone();
                filter_bar = filter_bar.child(filter_pill(&theme, &filter, index, cx));
            }
            filter_bar = filter_bar.child(clear_button(&theme, cx));
        }

        // Guardar y exportar. Van aqui y no en un menu porque son las dos acciones que
        // existen sobre un informe, y un menu que no se abre no ensena sus opciones.
        filter_bar = filter_bar.child(div().flex_1());
        filter_bar = filter_bar.child(open_button(&theme, cx));
        filter_bar = filter_bar.child(save_button(&theme, self.lectura, cx));
        filter_bar = filter_bar.child(export_button(&theme, cx));

        // --- Barra de formula DAX ---
        // El lienzo es una columna que scrollea y el grid tiene alto FIJO. Con el alto
        // repartido por flex, el grid se quedaba en cero pixeles: `uniform_list` pide las
        // filas que caben en su caja, y una caja de cero no pide ninguna. Se veia una tabla
        // con cabeceras y 500 filas debajo, sin una sola fila.
        let mut canvas = div().flex().flex_col().flex_1().overflow_y_scrollbar();
        if self.formula.is_none() {
            self.formula = Some(cx.new(|cx| {
                InputState::new(window, cx).default_value(
                    "Nueva = SUM('fact_sales'[qty])",
                )
            }));
        }

        let formula_error = self.formula_error.clone();
        canvas = canvas.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_size(px(tokens::LETRA_NOTA))
                        .text_color(theme.muted_foreground)
                        .flex_shrink_0()
                        .child("DAX"),
                )
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(self.formula.as_ref().expect("recien creada"))),
                )
                .child(add_measure_button(&theme, self.lectura, cx)),
        );
        if let Some(error) = formula_error {
            canvas = canvas.child(
                div()
                    .px_3()
                    .pb_1()
                    .text_size(px(tokens::LETRA_ETIQUETA))
                    // Rojo de texto. `danger_foreground` es el color del texto que va encima
                    // de un fondo rojo, o sea casi blanco: un error se veia invisible.
                    .text_color(theme.chart_bearish)
                    .child(error),
            );
        }
        let formato = self.formato_actual();
        if !self.totals.is_empty() {
            let cards: Vec<AnyElement> = self
                .totals
                .iter()
                .map(|tarjeta| tarjeta_kpi(&theme, tarjeta, formato))
                .collect();
            canvas = canvas.child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .gap_3()
                    .px_3()
                    .pt_3()
                    .children(cards),
            );
        }

        // --- El grafico del lienzo ---
        //
        // Que se dibuja lo decide el visual elegido. Hasta ahora el tipo elegido cambiaba el
        // color de un boton y nada mas: tres botones que pintaban lo mismo.
        let grafico = matches!(
            self.visual.as_str(),
            "Todo" | "Barras" | "Columnas" | "Lineas" | "Area" | "Anillo" | "Dispersion"
        );
        // La tabla vive con los dos visuales que la incluyen. En los demas el lienzo es el
        // grafico y nada mas.
        let tabla = matches!(self.visual.as_str(), "Todo" | "Tabla");

        if grafico
            && let Some(matrix) = self.matrix.as_ref()
            && matrix.row_count() > 0
            && !self.selection.dimensions.is_empty()
            && !self.selection.measures.is_empty()
        {
            let dim_count = self.selection.dimensions.len();
            let puntos: Vec<Punto> = matrix
                .top(0, dim_count, TOPE)
                .into_iter()
                .enumerate()
                .map(|(indice, (etiqueta, valor))| Punto {
                    indice,
                    etiqueta,
                    valor,
                })
                .collect();
            // La dispersion usa DOS medidas, una en cada eje. La primera es el X y la
            // segunda el Y.
            let nube: Vec<(f64, f64)> = if self.visual == "Dispersion" {
                matrix
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(indice, _)| matrix.row_kind(*indice) == RowKind::Data)
                    .filter_map(|(_, row)| {
                        let x = row.get(dim_count)?.parse::<f64>().ok()?;
                        let y = row.get(dim_count + 1)?.parse::<f64>().ok()?;
                        Some((x, y))
                    })
                    .collect()
            } else {
                Vec::new()
            };

            if self.visual == "Dispersion" && self.selection.measures.len() < 2 {
                // Sin dos medidas no hay dos ejes, y una linea recta de puntos no dice nada.
                canvas = canvas.child(card_aviso(
                    &theme,
                    "Dispersion necesita dos medidas, una en cada eje.",
                    formato,
                ));
            } else if self.visual == "Dispersion" && !nube.is_empty() {
                let medida = (0..2)
                    .filter_map(|offset| matrix.header(dim_count + offset).map(|h| h.label.clone()))
                    .collect::<Vec<_>>()
                    .join(" vs ");
                let dimension = self
                    .report
                    .as_ref()
                    .and_then(|report| report.model.column(self.selection.dimensions[0]))
                    .map(|(_, column)| column.name.clone())
                    .unwrap_or_default();
                canvas = canvas.child(card_grafico(
                    &theme,
                    &Grafico {
                        tipo: &self.visual,
                        puntos: &puntos,
                        nube: &nube,
                        medida: &medida,
                        dimension: &dimension,
                        dimension_id: self.selection.dimensions[0],
                        filas: matrix.row_count(),
                        formato,
                        alto: tabla.then_some(tokens::ALTO_GRAFICO),
                    },
                    cx,
                ));
            } else if self.visual != "Dispersion" && puntos.len() > 1 {
                let medida = matrix
                    .header(dim_count)
                    .map(|h| h.label.clone())
                    .unwrap_or_default();
                let dimension = self
                    .report
                    .as_ref()
                    .and_then(|report| report.model.column(self.selection.dimensions[0]))
                    .map(|(_, column)| column.name.clone())
                    .unwrap_or_default();
                canvas = canvas.child(card_grafico(
                    &theme,
                    &Grafico {
                        tipo: &self.visual,
                        puntos: &puntos,
                        nube: &[],
                        medida: &medida,
                        dimension: &dimension,
                        dimension_id: self.selection.dimensions[0],
                        filas: matrix.row_count(),
                        formato,
                        // Cuando la tabla no se pinta, el grafico se queda con el alto de la
                        // pagina: un grafico chico arriba de media pantalla vacia no es una
                        // pagina.
                        alto: tabla.then_some(tokens::ALTO_GRAFICO),
                    },
                    cx,
                ));
            }
        }

        // Un grid vacio NO se ve igual que un grid sin datos. Con 0 filas hay que decirlo:
        // un area en blanco parece una app congelada, y en una base sin ventas es la
        // respuesta correcta.
        let empty_state: Option<AnyElement> = self
            .grid
            .as_ref()
            .filter(|grid| grid.row_count == 0)
            .map(|_| {
                div()
                    // `.flex()` ademas de `.flex_1()`: sin el, el estado vacio no era una
                    // caja flexible y `items_center`/`justify_center` no centraban nada.
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child("Sin datos: la consulta no devolvio filas."),
                            )
                            .child(
                                div()
                                    .text_size(px(tokens::LETRA_ETIQUETA))
                                    .text_color(theme.muted_foreground)
                                    .child("Revisa los filtros, o prueba otra base desde la pantalla de conexion."),
                            ),
                    )
                    .into_any_element()
            });

        // --- El grid ---
        //
        // La tabla se queda con el alto que sobra en lugar de medir un alto fijo: el grafico
        // y las tarjetas ya tienen el suyo, y lo que resta es distinto en cada ventana. Con
        // un alto fijo, en una pantalla mas baja que la de referencia el sobrante se dibuja
        // fuera de la ventana.
        let body: AnyElement = if self.grid.is_some() {
            // El margen va en la caja que reparte el alto, no en la tarjeta: un margen
            // dentro de una tarjeta con `h_full` empuja la tarjeta fuera de su caja, y el
            // borde de abajo vuelve a caer detras de la barra de pestanas.
            div()
                .flex()
                .flex_1()
                .mx_3()
                .mb_3()
                .overflow_hidden()
                .child(self.render_grid(cx))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Elige una medida para ver el informe."),
                )
                .into_any_element()
        };

        // El lienzo segun el visual activo. La barra de formula vive dentro de `canvas`
        // porque es la entrada de medidas: se ve en todos los modos menos Tabla.
        let mut canvas_column = div()
            .flex()
            .flex_1()
            .h(alto_panel)
            .flex_col()
            .overflow_hidden()
            .child(filter_bar);
        if self.visual != "Tabla" {
            // El lienzo se reparte segun quien mas comparta la pagina. Con la tabla abajo,
            // el grafico mide lo suyo y la tabla se queda con el resto. Sin tabla, el
            // grafico se queda con el resto: un grafico chico arriba de media pantalla
            // vacia no es una pagina. Dos `flex_1` hermanos se reparten el alto a la mitad,
            // y ahi el grafico se queda sin su dibujo.
            let mut lienzo = div().flex().flex_col().overflow_hidden();
            lienzo = if tabla {
                lienzo.flex_shrink_0()
            } else {
                lienzo.flex_1()
            };
            canvas_column = canvas_column.child(
                lienzo.child(cabecera).child(canvas).into_any_element(),
            );
        }
        if tabla {
            canvas_column = canvas_column.child(match empty_state {
                Some(empty) => empty,
                None => body,
            });
        }

        // --- Panel Visualizaciones ---
        //
        // Los ocho tipos de la referencia, cuatro por fila. `Dispersion` se dibuja apagado y
        // no se esconde: un tipo que falta sin decirlo se busca, y no se encuentra.
        let tipos = [
            ("Tarjetas", "Tarjetas", IconName::SquareStack, true),
            ("Barras", "Barras", IconName::ChartBar, true),
            ("Columnas", "Columnas", IconName::ChartColumn, true),
            ("Lineas", "Lineas", IconName::ChartLine, true),
            ("Area", "Area", IconName::ChartArea, true),
            ("Anillo", "Anillo", IconName::ChartPie, true),
            ("Dispersion", "Dispersion", IconName::ChartScatter, true),
            ("Tabla", "Tabla", IconName::Table, true),
        ];
        let mut viz_buttons = div().flex().flex_wrap().gap_1();
        for (key, etiqueta, glifo, habilitado) in tipos {
            // El lienzo compuesto (tarjetas, grafico y tabla) no es uno de los ocho tipos: lo
            // representa `Tarjetas`, que es su zona de arriba. Sin esto la grilla se queda sin
            // ningun mosaico encendido al abrir la app, que se lee como un error.
            let activo = self.visual == key || (key == "Tarjetas" && self.visual == "Todo");
            viz_buttons = viz_buttons.child(components::mosaico(
                &theme,
                SharedString::from(format!("viz-{key}")),
                etiqueta,
                glifo,
                activo,
                !habilitado,
                cx.listener(move |view, _event, _window, cx| {
                    if view.visual == key {
                        return;
                    }
                    view.recordar();
                    view.visual = key.to_string();
                    cx.notify();
                }),
            ));
        }

        // --- Propiedades del visual elegido ---
        //
        // Cada interruptor gobierna el dibujo. Un ajuste que el visual activo no usa no se
        // dibuja: un interruptor encendido que no cambia nada ensena a desconfiar de todos.
        let formato = self.formato_actual();
        let visual = self.visual.as_str();
        let editable = !self.lectura;
        let mut propiedades = div().flex().flex_col().gap_1();
        // La tabla no lleva titulo de tarjeta: el interruptor no gobernaria nada.
        if visual != "Tabla" {
            propiedades = propiedades.child(components::interruptor(
                &theme,
                "prop-titulo",
                "Titulo",
                formato.titulo,
                editable,
                cx.listener(|view, _event, _window, cx| {
                    view.ajustar_formato(|formato| formato.titulo = !formato.titulo, cx);
                }),
            ));
        }
        propiedades = propiedades.child(components::interruptor(
            &theme,
            "prop-fondo",
            "Fondo",
            formato.fondo,
            editable,
            cx.listener(|view, _event, _window, cx| {
                view.ajustar_formato(|formato| formato.fondo = !formato.fondo, cx);
            }),
        ));
        // El valor al lado de la barra solo existe en las barras horizontales.
        if matches!(visual, "Barras" | "Todo") {
            propiedades = propiedades.child(components::interruptor(
                &theme,
                "prop-etiquetas",
                "Etiquetas",
                formato.etiquetas,
                editable,
                cx.listener(|view, _event, _window, cx| {
                    view.ajustar_formato(|formato| formato.etiquetas = !formato.etiquetas, cx);
                }),
            ));
        }
        // El eje de valores es el de los graficos del sistema. Las barras horizontales no
        // tienen eje: tienen la etiqueta pegada a la barra.
        if matches!(visual, "Columnas" | "Lineas" | "Area" | "Dispersion") {
            propiedades = propiedades.child(components::interruptor(
                &theme,
                "prop-eje",
                "Eje",
                formato.eje,
                editable,
                cx.listener(|view, _event, _window, cx| {
                    view.ajustar_formato(|formato| formato.eje = !formato.eje, cx);
                }),
            ));
        }
        // La comparacion es de las tarjetas KPI: la variacion y su linea.
        if matches!(visual, "Tarjetas" | "Todo") {
            propiedades = propiedades.child(components::interruptor(
                &theme,
                "prop-comparacion",
                "Comparacion",
                formato.comparacion,
                editable,
                cx.listener(|view, _event, _window, cx| {
                    view.ajustar_formato(
                        |formato| formato.comparacion = !formato.comparacion,
                        cx,
                    );
                }),
            ));
        }
        if matches!(visual, "Tabla" | "Todo") {
            propiedades = propiedades.child(components::interruptor(
                &theme,
                "prop-cuadricula",
                "Cuadricula",
                formato.cuadricula,
                editable,
                cx.listener(|view, _event, _window, cx| {
                    view.ajustar_formato(|formato| formato.cuadricula = !formato.cuadricula, cx);
                }),
            ));
        }

        // Los campos que alimentan el visual. No son ajustes: son la respuesta a "que esta
        // pintando esto", y con dos o tres medidas en la lista no se lee en otro sitio.
        let eje = self
            .selection
            .dimensions
            .iter()
            .map(|id| self.model_column_name(*id))
            .collect::<Vec<_>>()
            .join(", ");
        let valores = self
            .selection
            .measures
            .iter()
            .map(|id| self.model_measure_name(*id))
            .collect::<Vec<_>>()
            .join(", ");
        let mut campos = div().flex().flex_col().gap_2().pt_1();
        for (rotulo, valor) in [
            (
                "Eje",
                if eje.is_empty() {
                    "sin eje".to_string()
                } else {
                    eje
                },
            ),
            (
                "Valores",
                if valores.is_empty() {
                    "sin valores".to_string()
                } else {
                    valores
                },
            ),
        ] {
            campos = campos.child(components::fila_propiedad(&theme, rotulo, valor));
        }
        propiedades = propiedades.child(campos);

        let visualizaciones = div()
            .w(px(tokens::PANEL_VISUALES))
            .h_full()
            .flex_shrink_0()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.sidebar_border)
            // `.flex()` ademas de `.flex_col()`: sin el, el div queda en modo bloque y los
            // hijos se apilan sin separacion ni estiramiento. Es el mismo defecto que ya
            // aparecio en el lienzo y en la tarjeta.
            .flex()
            .flex_col()
            .child(
                // La misma cabecera que la del panel de Datos: alto, relleno y linea. Dos
                // paneles hermanos con la cabecera a distinta altura se leen como dos
                // pantallas pegadas.
                div()
                    .flex()
                    .items_center()
                    .px_3()
                    .h(px(tokens::ALTO_CABECERA))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(components::titulo_tarjeta(&theme, "Visualizaciones")),
            )
            .child(div().px_2().pb_3().child(viz_buttons))
            .child(
                div()
                    .px_3()
                    .py_3()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(components::titulo_tarjeta(&theme, "Propiedades"))
                    .child(propiedades),
            );

        // --- Barra superior ---
        //
        // A la izquierda, que informe se mira: la marca, la base, la pagina y desde cuando
        // esta guardado. A la derecha, que se puede hacer con el. El estado de guardado va
        // junto al nombre del informe porque es un estado DEL informe.
        let guardado: AnyElement = match self.last_saved {
            Some(at) => components::nota(
                &theme,
                format!("\u{2713} Guardado {}", ago_text(epoch_now() - at)),
            )
            .into_any_element(),
            None => components::nota(&theme, "Sin guardar").into_any_element(),
        };
        // Deshacer y rehacer se apagan cuando no hay historial o cuando no hay futuro. Un
        // icono encendido que no lleva a ningun lado, al lado de uno apagado que si, ensena a
        // no confiar en ninguno de los dos. Compartir y Publicar siguen apagados: no hay
        // destino.
        let mut modo = components::caja_segmentada(&theme);
        for (etiqueta, lectura) in [("Editar", false), ("Lectura", true)] {
            modo = modo.child(components::segmento(
                &theme,
                SharedString::from(format!(
                    "modo-{}",
                    if lectura { "lectura" } else { "editar" }
                )),
                etiqueta.to_string(),
                self.lectura == lectura,
                cx.listener(move |view, _event, _window, cx| {
                    view.lectura = lectura;
                    cx.notify();
                }),
            ));
        }

        let top_bar = div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .h(px(tokens::BARRA_SUPERIOR))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .w(px(26.0))
                    .h(px(26.0))
                    .rounded_xl()
                    .bg(theme.primary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(components::icono(
                        IconName::ChartColumn,
                        15.0,
                        theme.primary_foreground,
                    )),
            )
            .child(
                div()
                    .font_semibold()
                    .text_size(px(tokens::LETRA_CUERPO))
                    .child("Datan"),
            )
            .child(components::nota(
                &theme,
                format!(
                    "{}  \u{203a}  {}",
                    self.database.as_deref().unwrap_or("sin base"),
                    self.report_title()
                ),
            ))
            .child(guardado)
            .child(div().flex_1())
            .child(if self.historial.atras.is_empty() {
                components::boton_icono_apagado(&theme, IconName::Undo2)
            } else {
                components::boton_icono(
                    &theme,
                    "undo",
                    IconName::Undo2,
                    cx.listener(|view, _event, _window, cx| view.deshacer(cx)),
                )
            })
            .child(if self.historial.adelante.is_empty() {
                components::boton_icono_apagado(&theme, IconName::Redo2)
            } else {
                components::boton_icono(
                    &theme,
                    "redo",
                    IconName::Redo2,
                    cx.listener(|view, _event, _window, cx| view.rehacer(cx)),
                )
            })
            .child(modo)
            .child(components::boton_apagado(&theme, "Compartir"))
            .child(components::boton_apagado(&theme, "Publicar"));

        // Cuantos visuales hay ahora en el lienzo. Se cuenta lo que se PINTA, no lo que
        // el informe podria pintar.
        let con_filas = self.matrix.as_ref().is_some_and(|m| m.row_count() > 0);
        let grafico_listo = con_filas
            && !self.selection.dimensions.is_empty()
            && !self.selection.measures.is_empty();
        let visuales = match self.visual.as_str() {
            "Tarjetas" => usize::from(!self.totals.is_empty()),
            "Tabla" => usize::from(con_filas),
            "Todo" => {
                usize::from(!self.totals.is_empty())
                    + usize::from(grafico_listo)
                    + usize::from(con_filas)
            }
            _ => usize::from(!self.totals.is_empty()) + usize::from(grafico_listo),
        };

        // --- Pestañas de paginas ---
        //
        // Una sola pagina. El "+" esta y no se puede tocar: la app sabe pintar un informe, no
        // varios guardados en la misma ventana.
        let tabs = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(tokens::ALTO_PESTANAS))
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(tokens::ALTO_PASTILLA))
                    .px_3()
                    .rounded_xl()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .font_semibold()
                    .text_size(px(tokens::LETRA_TITULO))
                    .child(self.report_title()),
            )
            .child(components::boton_icono_apagado(&theme, IconName::Plus))
            .child(div().flex_1())
            .child(components::nota(&theme, self.linea_estado(visuales)));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.muted)
            .text_color(theme.foreground)
            .child(top_bar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .overflow_hidden()
                    .child(sidebar)
                    .child(canvas_column)
                    .child(visualizaciones),
            )
            .child(tabs)
            .into_any_element()
    }

    /// Las relaciones del modelo, con su rol.
    ///
    /// Es el unico sitio donde se ve POR QUE dos tablas estan conectadas. Una relacion que la
    /// base no declara no se puede inventar sola, y una que el catalogo trae mal manda los
    /// filtros por el camino equivocado sin dar ningun error: por eso el rol se cambia a mano
    /// y se guarda en el documento del modelo.
    fn render_relaciones(&self, model: &Model, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let conectando = self.conexion != Conexion::Apagada;
        let mut list = div()
            .flex_col()
            .gap_1()
            .mt_2()
            .child(div().px_2().pt_2().child(components::nota(&theme, "Relaciones")));

        // En modo Lectura no se dibuja: no hay nada que conectar.
        if !self.lectura {
            list = list.child(div().px_2().child(components::fila_dato(
                &theme,
                "rel-conectar",
                if conectando {
                    "Cancelar"
                } else {
                    "Conectar dos columnas"
                },
                conectando,
                cx.listener(|view, _event, _window, cx| view.comenzar_conexion(cx)),
            )));
        }

        if model.relationships.is_empty() {
            return list.child(div().px_2().pb_2().child(components::nota(
                &theme,
                "la base no declara ninguna",
            )));
        }

        for relating in &model.relationships {
            let (Some((from_table, from_column)), Some((to_table, to_column))) = (
                model.column(relating.from_column),
                model.column(relating.to_column),
            ) else {
                continue;
            };
            let activa = relating.role == RelationshipRole::Active;
            let from = relating.from_column;
            let to = relating.to_column;
            let origen = format!("{}[{}]", from_table.name, from_column.name);
            let destino = format!("a {}[{}]", to_table.name, to_column.name);
            let row = div()
                .id(ElementId::Name(format!("rel-{}-{}", from.0, to.0).into()))
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_xl()
                .overflow_hidden()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(tokens::LETRA_NOTA))
                                .overflow_hidden()
                                .child(origen.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(tokens::LETRA_NOTA))
                                .text_color(theme.muted_foreground)
                                .overflow_hidden()
                                .child(destino.clone()),
                        ),
                )
                .child(
                    div()
                        .px_1()
                        .rounded_md()
                        .text_size(px(tokens::LETRA_NOTA))
                        .text_color(if activa {
                            theme.primary_foreground
                        } else {
                            theme.muted_foreground
                        })
                        .bg(if activa { theme.primary } else { theme.muted })
                        .child(if activa { "activa" } else { "inactiva" }),
                );
            // En modo Lectura el rol se ve y no se toca: un interruptor que no hace nada es
            // peor que no dibujarlo.
            if self.lectura {
                list = list.child(row);
            } else {
                list = list.child(
                    row.cursor_pointer()
                        .hover(|style| style.bg(theme.list_hover))
                        .on_click(cx.listener(move |view, _event, _window, cx| {
                            view.toggle_relationship(from, to, cx);
                        })),
                );
            }
        }
        list
    }

    /// El panel de Datos como lo ve el usuario: tablas con su conteo de filas, y dentro
    /// los campos de cada una.
    ///
    /// Una lista plana de columnas obliga a recordar de que tabla era cada una. Con el
    /// nombre de la tabla al lado, el conteo de filas se lee en el sitio donde se decide si
    /// esa tabla sirve para algo.
    fn render_tables(
        &mut self,
        tablas: &[&datan_model::Table],
        dimensions: &[ObjectId],
        model: &Model,
        query: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut list = div().flex_col();
        if !tablas.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .pt_2()
                    .child(components::nota(&theme, "Tablas")),
            );
        }

        for table in tablas {
            // Con busqueda, una tabla se abre sola si tiene un campo que coincide: buscar
            // "region" tiene que mostrar el campo, no una lista de tablas cerradas.
            let auto = table.columns.iter().any(|c| hit(&c.name, query))
                || hit(&table.name, query);
            let open = self.expanded.contains(&table.id) || auto;
            let count = self.row_counts.get(&table.id).copied();
            let id = table.id;
            let mut header = div()
                .id(ElementId::Name(SharedString::from(format!("tbl-{id:?}"))))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(tokens::ALTO_FILA))
                .rounded_xl()
                .cursor_pointer()
                .hover(|style| style.bg(theme.list_hover))
                .child(components::icono(
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    tokens::LETRA_ICONO,
                    theme.muted_foreground,
                ))
                .child(components::icono(
                    IconName::Table,
                    tokens::LETRA_ICONO,
                    theme.muted_foreground,
                ))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .truncate()
                        .text_size(px(tokens::LETRA_TITULO))
                        .child(table.name.clone()),
                );
            if let Some(count) = count {
                // El conteo va en mono y alineado a la derecha: en una lista de seis tablas
                // los numeros tienen que caer en la misma columna para poder compararlos de
                // un vistazo.
                header = header.child(
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(tokens::LETRA_NOTA))
                        .text_color(theme.muted_foreground)
                        .child(count_text(count)),
                );
            }
            list = list.child(header.on_click(cx.listener(move |view, _event, _window, cx| {
                match view.expanded.iter().position(|t| *t == id) {
                    Some(index) => {
                        view.expanded.remove(index);
                    }
                    None => view.expanded.push(id),
                }
                cx.notify();
            })));

            if open {
                // Un escalon debajo de la tabla: los campos son de la tabla de arriba, y
                // alineados con su nombre se leen como tablas hermanas.
                let mut campos = div().flex_col().pl_4();
                for definition in &table.columns {
                    let column = definition.id;
                    let is_measure = false;
                    let is_on = self.selection.dimensions.contains(&column);
                    // Con busqueda activa, la tabla abierta solo muestra los campos que
                    // coinciden: buscar "region" tiene que ENSEÑAR el campo Region.
                    if !query.is_empty() && !hit(&definition.name, query) {
                        continue;
                    }
                    campos = campos.child(sidebar_item(
                        &theme,
                        definition.name.clone(),
                        is_on,
                        column,
                        is_measure,
                        cx,
                    ));
                }
                list = list.child(campos);
            }
        }

        let _ = (dimensions, model);
        list
    }

    fn render_sidebar_section(
        &self,
        etiqueta: String,
        columns: &[ObjectId],
        is_measure: bool,
        model: &Model,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected = if is_measure {
            &self.selection.measures
        } else {
            &self.selection.dimensions
        };

        div()
            .flex_col()
            .gap_1()
            .mt_2()
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .child(components::nota(&theme, etiqueta)),
            )
            .children(columns.iter().map(|column| {
                let is_on = selected.contains(column);
                let label = if is_measure {
                    model
                        .measures
                        .iter()
                        .find(|m| m.id == *column)
                        .map(|m| m.name.clone())
                        .unwrap_or_default()
                } else {
                    model
                        .column(*column)
                        .map(|(_, c)| c.name.clone())
                        .unwrap_or_default()
                };
                let column = *column;
                sidebar_item(&theme, label, is_on, column, is_measure, cx)
            }))
    }

    /// El grid.
    ///
    /// `uniform_list` y no el componente `Table` de gpui-kit: el primero es el que la
    /// Fase 0 midio a 23 microsegundos por frame con un millon de filas, y el segundo trae
    /// su propia virtualizacion sin medir. gpui-kit se usa para el tema y los estilos.
    fn render_grid(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if let Some(matrix) = self.matrix.as_mut()
            && let Some((column, descending)) = self.sort
        {
            // Ordenar en su sitio para que `drill` lea la misma fila que pinta. Un orden en
            // la pantalla y otro en el modelo haria que el clic filtrara por el valor de
            // OTRO producto.
            //
            // Con subtotales, cada corrida de renglones de dato se ordena DENTRO de su
            // grupo: mover un subtotal de sitio desarmaria el arbol, y una corrida de datos
            // es exactamente los detalles de un grupo.
            let parse = |row: &Vec<String>| {
                row.get(column)
                    .and_then(|value| value.parse::<f64>().ok())
            };
            let mut inicio = 0;
            while inicio < matrix.rows.len() {
                if matrix.row_kind(inicio) != RowKind::Data {
                    inicio += 1;
                    continue;
                }
                let mut fin = inicio;
                while fin < matrix.rows.len() && matrix.row_kind(fin) == RowKind::Data {
                    fin += 1;
                }
                matrix.rows[inicio..fin].sort_by(|a, b| {
                    parse(a)
                        .partial_cmp(&parse(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                if descending {
                    matrix.rows[inicio..fin].reverse();
                }
                inicio = fin;
            }
        }

        let Some(grid) = self.grid.clone() else {
            return div().into_any_element();
        };
        let columns = Arc::clone(&grid.columns);
        let kinds = Arc::clone(&grid.kinds);
        let row_count = grid.row_count;
        let cuadricula = self.formato_actual().cuadricula;
        let dimension_count = self
            .report
            .as_ref()
            .map(|report| report.dimensions.len())
            .unwrap_or(0);
        // `uniform_list` entrega un `&mut App`, no un `&mut Context`. Para tocar la vista
        // desde el manejador del clic hay que pasar por la entidad.
        let view = cx.entity();
        let scroll = self.scroll.clone();

        let header = div()
            .flex()
            .flex_shrink_0()
            .rounded_xl()
            .bg(theme.muted)
            .children(columns.iter().enumerate().map(|(index, column)| {
                // Solo las MEDIDAS ordenan. Una dimension ya define las filas; una flecha
                // sobre una columna que no ordena es una promesa que el clic no cumple.
                let arrow = match self.sort {
                    Some((sorted_column, false)) if sorted_column == index => "  \u{2191}",
                    Some((sorted_column, true)) if sorted_column == index => "  \u{2193}",
                    _ if index >= dimension_count => "  \u{21c5}",
                    _ => "",
                };
                let base = div()
                    .w(px(tokens::ANCHO_CELDA))
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .overflow_hidden()
                    .truncate()
                    .text_size(px(tokens::LETRA_ETIQUETA))
                    .text_color(theme.muted_foreground)
                    .child(format!("{}{arrow}", column.header));

                if index >= dimension_count {
                    let column_index = index;
                    base.id(ElementId::Name(SharedString::from(format!(
                        "head-{column_index}"
                    ))))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.list_hover))
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.recordar();
                        view.sort = match view.sort {
                            Some((c, false)) if c == column_index => Some((c, true)),
                            Some((c, true)) if c == column_index => None,
                            _ => Some((column_index, false)),
                        };
                        cx.notify();
                    }))
                    .into_any_element()
                } else {
                    base.into_any_element()
                }
            }));

        let body = uniform_list(
            ElementId::View(cx.entity().entity_id()),
            row_count,
            move |range, _window, _cx| {
                range
                    .map(|row| {
                        // Un subtotal resume un grupo: se lee con peso y sin clic. El gran
                        // total cierra la tabla con el mismo trato.
                        let destacado =
                            !matches!(kinds.get(row), None | Some(RowKind::Data));
                        let cells = columns.iter().enumerate().map(|(index, column)| {
                            // Las celdas de medida y las de dimension son tipos distintos
                            // (Div y Stateful<Div>), asi que la fila se arma de elementos ya
                            // convertidos.
                            
                            let mut cell = div()
                                .w(px(tokens::ANCHO_CELDA))
                                .flex_shrink_0()
                                .px_3()
                                .py_1()
                                .text_size(px(tokens::LETRA_CUERPO))
                                .child(
                                    column
                                        .values
                                        .get(row)
                                        .cloned()
                                        .unwrap_or_default(),
                                );
                            if destacado {
                                cell = cell.font_semibold();
                            }
                            if column.numeric {
                                cell = cell.text_align(TextAlign::Right);
                            }

                            // Solo un renglon de dato filtra al pinchar. Pinchar un subtotal
                            // seria filtrar por un valor que no existe en la columna.
                            if destacado || index >= dimension_count {
                                return cell.into_any_element();
                            }

                            let row_index = row;
                            let column_index = index;
                            let view = view.clone();
                            let id = ElementId::Name(SharedString::from(format!(
                                "cell-{row_index}-{column_index}"
                            )));
                            cx_hover(theme.list_hover, cell)
                                .cursor_pointer()
                                .id(id)
                                .on_click(move |_event, _window, app| {
                                    view.update(app, |view, cx| {
                                        view.drill(row_index, column_index, cx);
                                    });
                                })
                                .into_any_element()
                        });

                        let mut fila = div().flex().children(cells);
                        // Sin cuadricula la tabla sigue separando el subtotal del dato: el
                        // relleno no es una linea, es el grupo.
                        if cuadricula {
                            fila = fila.border_b_1().border_color(theme.border);
                        }
                        if destacado {
                            fila = fila.bg(theme.muted);
                        }
                        fila
                    })
                    .collect::<Vec<_>>()
            },
        )
        // `ListSizingBehavior` es `Auto` por defecto, y `Auto` significa "no calcules un
        // tamano fijo": la lista se queda con el alto del estilo, que es automatico, o sea
        // cero. Con cero de alto no pinta NINGUNA fila y la tabla se ve con cabeceras y sin
        // datos. `Infer` mide una fila y pide las que caben en el alto disponible.
        .with_sizing_behavior(ListSizingBehavior::Infer)
        // Sin el handle, `scroll_to_item(0)` de `refresh` no mueve nada: la lista no tiene
        // a quien pedirle que suba.
        .track_scroll(&scroll);

        // La tabla vive en una tarjeta como el grafico: las dos son superficies del mismo
        // lienzo, y una tabla suelta sobre el gris se lee como un volcado de datos.
        components::superficie(cx.theme(), self.formato_actual().fondo)
            .p_2()
            // `h_full` y no un alto propio: la caja ya tiene el suyo (el que sobra en la
            // ventana) y la lista de filas se mide contra el. Un alto fijo aca adentro
            // vuelve a sacar la tabla de la ventana.
            .h_full()
            .flex_shrink_0()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().overflow_hidden().child(body))
            .into_any_element()
    }
}

fn add_measure_button(theme: &Theme, lectura: bool, cx: &mut Context<Datan>) -> AnyElement {
    if lectura {
        return components::boton_apagado(theme, "Crear medida");
    }
    clickable(
        theme,
        "add-measure".into(),
        "Crear medida".to_string(),
        cx.listener(|view, _event, _window, cx| view.add_measure(cx)),
    )
}

fn save_button(theme: &Theme, lectura: bool, cx: &mut Context<Datan>) -> AnyElement {
    if lectura {
        return components::boton_apagado(theme, "Guardar");
    }
    clickable_peso(theme, Peso::Primario, "save".into(), "Guardar".to_string(), cx.listener(
        |view, _event, _window, cx| view.save_report(cx),
    ))
}

/// El formato de archivo de una exportacion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Archivo {
    Csv,
    Excel,
    Pdf,
}

/// Los tres formatos, uno al lado del otro. Un menu que no se abre no ensena sus opciones,
/// y son tres: caben.
fn export_button(theme: &Theme, cx: &mut Context<Datan>) -> AnyElement {
    div()
        .flex()
        .gap_1()
        .child(clickable(theme, "export-csv".into(), "CSV".to_string(), cx.listener(
            |view, _event, _window, cx| view.export_csv(cx),
        )))
        .child(clickable(theme, "export-xlsx".into(), "Excel".to_string(), cx.listener(
            |view, _event, _window, cx| view.export_excel(cx),
        )))
        .child(clickable(theme, "export-pdf".into(), "PDF".to_string(), cx.listener(
            |view, _event, _window, cx| view.export_pdf(cx),
        )))
        .into_any_element()
}

/// "Abrir" lleva a la lista de informes guardados. No abre el ultimo informe por su cuenta:
/// abrir un informe encima de otro sin preguntar pierde lo que se estaba mirando.
fn open_button(theme: &Theme, cx: &mut Context<Datan>) -> AnyElement {
    clickable(theme, "open".into(), "Abrir".to_string(), cx.listener(
        |view, _event, _window, cx| {
            view.state = State::Opening;
            view.stats = None;
            cx.notify();
        },
    ))
}

/// Un punto de un grafico: la categoria y el valor de la primera medida.
#[derive(Clone)]
struct Punto {
    /// El lugar que ocupa en la serie. Es el indice de la paleta: sin el, el color de una
    /// porcion se elige desde afuera del dato.
    indice: usize,
    etiqueta: String,
    valor: f64,
}

/// Cuantos valores entran en un grafico.
///
/// Ocho barras se leen de un golpe. Veinte no: el eje de categorias se convierte en una
/// lista de nombres imposible de seguir.
const TOPE: usize = 8;

/// Lo que hace falta para armar la tarjeta de un grafico.
#[derive(Clone, Copy)]
struct Grafico<'a> {
    /// La clave del tipo elegido.
    tipo: &'a str,
    puntos: &'a [Punto],
    /// Los puntos de una dispersion: (x, y) por categoria. Vacio en los demas graficos.
    nube: &'a [(f64, f64)],
    /// El nombre de la medida, para el titulo.
    medida: &'a str,
    /// El nombre de la dimension, para el titulo.
    dimension: &'a str,
    /// La columna que se filtra al tocar una categoria.
    dimension_id: ObjectId,
    /// Cuantas filas tiene la matriz: el titulo dice cuantas quedaron afuera.
    filas: usize,
    /// El formato del visual: titulo, fondo, etiquetas y eje.
    formato: datan_report::saved::SavedFormato,
    /// El alto del dibujo. `None` cuando el grafico es lo unico del lienzo: ahi ocupa lo
    /// que sobra en la ventana, que es lo que hace la referencia.
    alto: Option<f32>,
}

/// La tarjeta de un grafico: el titulo, el dibujo y, en el anillo, la leyenda.
///
/// El titulo se arma con el nombre de la medida y el de la dimension. Son los nombres del
/// modelo y no se traducen: un grafico que dice "Ventas" cuando la medida se llama
/// `product_boleta quantity` manda al usuario a buscar un campo que no existe.
fn card_grafico(theme: &Theme, grafico: &Grafico, cx: &mut Context<Datan>) -> AnyElement {
    let Grafico {
        tipo,
        puntos,
        nube,
        medida,
        dimension,
        dimension_id,
        filas,
        formato,
        alto,
    } = *grafico;
    let mut tarjeta_titulo = div().flex().items_center().gap_2();
    // Cuantos valores quedaron afuera. Un grafico que dibuja ocho de quinientas filas y no
    // lo dice es un grafico que miente con el eje. La nota va con el titulo: sin titulo, el
    // aviso del recorte flota sobre el dibujo sin decir de que habla.
    if formato.titulo {
        tarjeta_titulo = tarjeta_titulo
            .child(components::titulo_tarjeta(theme, titulo_de(medida, dimension)))
            .child(div().flex_1());
        if filas > puntos.len() {
            tarjeta_titulo = tarjeta_titulo.child(components::nota(
                theme,
                format!("los {} valores mas altos", puntos.len()),
            ));
        }
    }

    let cuerpo: AnyElement = match tipo {
        // El anillo no lleva etiquetas sobre las porciones: ocho lineas de guia alrededor de
        // un circulo se pisan entre ellas. La leyenda dice lo mismo, en una columna, y cada
        // fila filtra.
        "Anillo" => div()
            .flex()
            .flex_1()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .child(dibujo(tipo, puntos, medida, formato.eje)),
            )
            .child(leyenda(theme, puntos, dimension_id, cx))
            .into_any_element(),
        // Las barras horizontales se dibujan a mano porque son el clic: un grafico del
        // sistema no avisa que barra se toco, y filtrar por categoria desde el grafico es una
        // funcion que ya existe.
        "Barras" | "Todo" => barras(theme, puntos, dimension_id, formato.etiquetas, cx),
        // El titulo dice las dos medidas, y el dibujo las cruza. Cada punto es una
        // categoria: pinchar el punto no filtra, porque un canvas no dice que punto se toco
        // y el filtro por categoria ya esta en la tabla y en las barras.
        "Dispersion" => dispersion(theme, nube, formato.eje),
        _ => div()
            .flex()
            .flex_1()
            .child(dibujo(tipo, puntos, medida, formato.eje))
            .into_any_element(),
    };

    let mut tarjeta = components::superficie(theme, formato.fondo)
        .mx_3()
        .mt_3()
        .p_4()
        .flex()
        .flex_col()
        .overflow_hidden()
        .gap_3();
    if formato.titulo {
        tarjeta = tarjeta.child(tarjeta_titulo);
    }
    tarjeta = tarjeta.child(cuerpo);
    tarjeta = match alto {
        Some(alto) => tarjeta.h(px(alto)).flex_shrink_0(),
        None => tarjeta.flex_1(),
    };
    tarjeta.into_any_element()
}

/// Una tarjeta con un aviso, para el visual que no puede dibujar todavia.
fn card_aviso(
    theme: &Theme,
    texto: impl Into<SharedString>,
    formato: datan_report::saved::SavedFormato,
) -> AnyElement {
    components::superficie(theme, formato.fondo)
        .mx_3()
        .mt_3()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(components::nota(theme, texto))
        .into_any_element()
}

/// Una dispersion: cada categoria es un punto, con una medida en cada eje.
///
/// Los dos ejes se escalan al minimo y al maximo de los datos. Un eje que arranca en cero
/// junta todos los puntos contra la izquierda el dia que las dos medidas tienen un piso alto,
/// que es el caso de casi cualquier tabla de hechos.
fn dispersion(theme: &Theme, nube: &[(f64, f64)], eje: bool) -> AnyElement {
    let extremos = |valores: &mut dyn Iterator<Item = f64>| {
        valores.fold(None::<(f64, f64)>, |acc, valor| {
            Some(match acc {
                None => (valor, valor),
                Some((lo, hi)) => (lo.min(valor), hi.max(valor)),
            })
        })
    };
    let (min_x, max_x) = extremos(&mut nube.iter().map(|punto| punto.0)).unwrap_or((0.0, 1.0));
    let (min_y, max_y) = extremos(&mut nube.iter().map(|punto| punto.1)).unwrap_or((0.0, 1.0));
    let span_x = (max_x - min_x).max(f64::MIN_POSITIVE);
    let span_y = (max_y - min_y).max(f64::MIN_POSITIVE);
    let datos: Vec<(f32, f32)> = nube
        .iter()
        .map(|punto| {
            (
                ((punto.0 - min_x) / span_x) as f32,
                1.0 - ((punto.1 - min_y) / span_y) as f32,
            )
        })
        .collect();
    let color = tokens::serie_unica();
    let rejilla = theme.chart_grid;

    let dibujo = canvas(
        move |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            // Un margen de 6 px: un punto en el borde del canvas se ve cortado a la mitad.
            let margen = px(6.);
            let ancho = (bounds.size.width - margen * 2.0).max(px(1.));
            let alto = (bounds.size.height - margen * 2.0).max(px(1.));
            let x = |t: f32| bounds.origin.x + margen + ancho * t;
            let y = |t: f32| bounds.origin.y + margen + alto * t;

            if eje {
                let mut trazo = PathBuilder::stroke(px(1.));
                for i in 0..=4 {
                    let t = i as f32 / 4.0;
                    trazo.move_to(point(x(0.0), y(t)));
                    trazo.line_to(point(x(1.0), y(t)));
                    trazo.move_to(point(x(t), y(0.0)));
                    trazo.line_to(point(x(t), y(1.0)));
                }
                if let Ok(path) = trazo.build() {
                    window.paint_path(path, rejilla);
                }
            }

            for (tx, ty) in &datos {
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(x(*tx) - px(4.0), y(*ty) - px(4.0)),
                            size(px(8.0), px(8.0)),
                        ),
                        color,
                    )
                    .corner_radii(Corners::all(px(4.0))),
                );
            }
        },
    )
    .h(px(tokens::ALTO_DISPERSION))
    .w_full();

    let rotulo = |texto: String| {
        div()
            .text_size(px(tokens::LETRA_NOTA))
            .text_color(theme.muted_foreground)
            .child(texto)
    };
    let margen_eje = 64.0;
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .w(px(margen_eje - 8.0))
                        .h(px(tokens::ALTO_DISPERSION))
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .child(rotulo(format_value(max_y)))
                        .child(rotulo(format_value(min_y))),
                )
                .child(div().flex_1().child(dibujo)),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .pl(px(margen_eje))
                .child(rotulo(format_value(min_x)))
                .child(rotulo(format_value(max_x))),
        )
        .into_any_element()
}

/// El dibujo de un grafico del sistema, segun el tipo elegido.
fn dibujo(tipo: &str, puntos: &[Punto], medida: &str, eje: bool) -> AnyElement {
    let datos = puntos.to_vec();
    let nombre = SharedString::from(medida.to_string());
    // Una sola medida: un solo color. El color por categoria es para el anillo, donde cada
    // porcion ES una categoria.
    let color = tokens::serie_unica();
    let id = ElementId::Name(SharedString::from(format!("grafico-{tipo}")));

    match tipo {
        "Lineas" => LineChart::new(datos)
            .id(id)
            .x(|punto: &Punto| punto.etiqueta.clone())
            .y(|punto: &Punto| punto.valor)
            .stroke(color)
            .name(nombre)
            .tooltip_title(|punto: &Punto| SharedString::from(punto.etiqueta.clone()))
            .tooltip_value(|_, valor| SharedString::from(format_value(valor)))
            .linear()
            .y_axis(eje)
            .y_tick_count(4)
            .y_tick_format(format_value)
            .grid(eje)
            .grid_dashed(false)
            .into_any_element(),
        "Area" => AreaChart::new(datos)
            .id(id)
            .x(|punto: &Punto| punto.etiqueta.clone())
            .y(|punto: &Punto| punto.valor)
            .stroke(color)
            .fill(color.opacity(0.18))
            .name(nombre)
            .tooltip_title(|punto: &Punto| SharedString::from(punto.etiqueta.clone()))
            .tooltip_value(|_, _, valor| SharedString::from(format_value(valor)))
            .linear()
            .y_axis(eje)
            .y_tick_count(4)
            .y_tick_format(format_value)
            .grid(eje)
            .grid_dashed(false)
            .into_any_element(),
        // `inner_radius` esta en pixeles, como el radio exterior, que sale del alto de la
        // caja.
        "Anillo" => PieChart::new(datos)
            .id(id)
            .value(|punto: &Punto| punto.valor as f32)
            .color(|punto: &Punto| tokens::serie(punto.indice))
            .inner_radius(64.0)
            .name(nombre)
            .tooltip_name(|punto: &Punto| SharedString::from(punto.etiqueta.clone()))
            .into_any_element(),
        _ => BarChart::new(datos)
            .id(id)
            .band(|punto: &Punto| SharedString::from(punto.etiqueta.clone()))
            .value(|punto: &Punto| punto.valor)
            .fill(move |_, _, _, _| color)
            .name(nombre)
            .tooltip_title(|punto: &Punto| SharedString::from(punto.etiqueta.clone()))
            .tooltip_value(|_, valor| SharedString::from(format_value(valor)))
            .value_axis(eje)
            .value_tick_count(4)
            .value_tick_format(format_value)
            .grid(eje)
            .grid_dashed(false)
            .corner_radii(Corners::all(px(4.)))
            .into_any_element(),
    }
}

/// La leyenda del anillo: el color, la categoria y su valor, y cada fila filtra.
///
/// Dos columnas y no `flex_wrap`: el envoltorio reparte las lineas por el alto que sobre, y
/// ocho filas que se separan solas se leen como ocho bloques sueltos.
fn leyenda(
    theme: &Theme,
    puntos: &[Punto],
    dimension_id: ObjectId,
    cx: &mut Context<Datan>,
) -> AnyElement {
    let mitad = puntos.len().div_ceil(2).max(1);
    let (izquierda, derecha) = puntos.split_at(mitad.min(puntos.len()));
    div()
        .w(px(360.0))
        .flex_shrink_0()
        .flex()
        .gap_x_2()
        .child(leyenda_columna(theme, izquierda, dimension_id, cx))
        .child(leyenda_columna(theme, derecha, dimension_id, cx))
        .into_any_element()
}

/// Una columna de la leyenda: una fila por categoria.
fn leyenda_columna(
    theme: &Theme,
    puntos: &[Punto],
    dimension_id: ObjectId,
    cx: &mut Context<Datan>,
) -> AnyElement {
    let mut columna = div().flex_1().flex_col().gap_1();
    for punto in puntos {
        let etiqueta = punto.etiqueta.clone();
        columna = columna.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "leyenda-{}",
                    punto.indice
                ))))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_xl()
                .cursor_pointer()
                .hover(|style| style.bg(theme.list_hover))
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(tokens::serie(punto.indice)),
                )
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .truncate()
                        .text_size(px(tokens::LETRA_ETIQUETA))
                        .child(punto.etiqueta.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(tokens::LETRA_ETIQUETA))
                        .text_color(theme.muted_foreground)
                        .child(format_value(punto.valor)),
                )
                .on_click(cx.listener(move |view, _event, _window, cx| {
                    view.filter_by_label(dimension_id, &etiqueta, cx);
                })),
        );
    }
    columna.into_any_element()
}

/// Las barras horizontales de una medida, una por categoria, y cada barra filtra.
///
/// Se dibujan con divs y no con un grafico del sistema porque el clic tiene que saber QUE
/// barra se toco, y los graficos no lo dicen.
fn barras(
    theme: &Theme,
    puntos: &[Punto],
    dimension_id: ObjectId,
    etiquetas: bool,
    cx: &mut Context<Datan>,
) -> AnyElement {
    let max = puntos
        .iter()
        .map(|punto| punto.valor)
        .fold(0.0f64, f64::max)
        .max(f64::MIN_POSITIVE);

    let filas: Vec<AnyElement> = puntos
        .iter()
        .map(|punto| {
            let fraccion = (punto.valor / max).clamp(0.02, 1.0);
            let etiqueta = punto.etiqueta.clone();
            let mut fila = div()
                .id(ElementId::Name(SharedString::from(format!(
                    "barra-{}",
                    punto.etiqueta
                ))))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(|style| style.bg(theme.list_hover))
                .child(
                    div()
                        .w(px(150.0))
                        .flex_shrink_0()
                        .text_size(px(tokens::LETRA_ETIQUETA))
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .truncate()
                        .child(punto.etiqueta.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_1()
                        // La esquina de una marca de dato, no la de un control: la misma
                        // que la columna del grafico de columnas.
                        .h(px(18.0))
                        .rounded_sm()
                        .child(
                            div()
                                .flex_grow(fraccion as f32 * 1000.0)
                                .h_full()
                                .rounded_sm()
                                .bg(tokens::serie_unica()),
                        )
                        .child(div().flex_grow((1.0 - fraccion) as f32 * 1000.0)),
                );
            // Sin etiqueta el valor no se dibuja, que es lo que pide apagar las etiquetas. El
            // ancho de la columna no se reserva: la barra llega al borde y la fila no queda
            // coja.
            if etiquetas {
                fila = fila.child(
                    div()
                        .w(px(80.0))
                        .flex_shrink_0()
                        .text_size(px(tokens::LETRA_ETIQUETA))
                        .text_align(TextAlign::Right)
                        .child(format_value(punto.valor)),
                );
            }
            fila.on_click(cx.listener(move |view, _event, _window, cx| {
                view.filter_by_label(dimension_id, &etiqueta, cx);
            }))
            .into_any_element()
        })
        .collect();

    div()
        .flex()
        .flex_1()
        .flex_col()
        .gap_1()
        .justify_center()
        .children(filas)
        .into_any_element()
}

/// Un numero para una tarjeta o un eje: sin decimales inutiles.
///
/// `10000.0` y `10000.50` se leen distinto; `10000.000000` y `10000.500000` no. Tres
/// decimales es el punto donde un margen sigue siendo un margen y el ruido ya no molesta.
fn format_value(value: f64) -> String {
    if value.abs() >= 1e12 {
        format!("{:.1}B", value / 1e12)
    } else if value.abs() >= 1e9 {
        format!("{:.1}MM", value / 1e9)
    } else if value.abs() >= 1e6 {
        format!("{:.1}M", value / 1e6)
    } else if value.abs() >= 1e3 {
        format!("{:.1}K", value / 1e3)
    } else if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

/// Un `Div` con estilo de hover.
///
/// `Div` no tiene `when`, asi que el hover se aplica siempre y el color se decide aqui.
fn cx_hover(color: Hsla, cell: Div) -> Div {
    cell.hover(move |style| style.bg(color))
}

fn main() {
    application()
        // `Assets` es el juego corto de 106 iconos del componente, y no tiene los siete que
        // usa esta pantalla: el logo, los cuatro tipos de grafico, el anillo, los mosaicos y
        // la tabla quedaban como un hueco en blanco. `AllAssets` es el catalogo completo.
        .with_assets(assets::AllAssets)
        .run(|cx| {
            init(cx);
            let database = std::env::var("DATAN_AUTOCONNECT").unwrap_or_default();
            open_window(WindowOptions::default(), cx, |_, cx| {
                let view = cx.new(Datan::new);
                // `DATAN_AUTOCONNECT=datan_bench` arranca conectado. Es para desarrollo: en
                // pruebas hay que poder mirar el lienzo sin un clic de por medio. Sin la
                // variable, la app arranca en la pantalla de conexion, como debe.
                if !database.is_empty() {
                    view.update(cx, |view, cx| view.connect(&database, cx));
                }
                view
            })
            .expect("no se pudo abrir la ventana");
        });
}
#[cfg(test)]
mod tests {
    use super::{
        AtomKind, BinaryOp, HISTORIAL_MAX, Historial, Selection, Shape, Vista, forma_aditiva, hit,
    };

    /// Una vista con lo unico que distingue un paso de otro en estos tests: el visual.
    fn vista(visual: &str) -> Vista {
        Vista {
            selection: Selection {
                dimensions: Vec::new(),
                measures: Vec::new(),
            },
            filters: Vec::new(),
            visual: visual.to_string(),
            sort: None,
            top_n: None,
            date_months: None,
            formatos: Default::default(),
        }
    }

    #[test]
    fn deshacer_recorre_los_pasos_al_reves() {
        let mut historial = Historial::default();
        historial.recordar(vista("A"));
        historial.recordar(vista("B"));

        // La app esta en C, que es el estado que el usuario tiene delante.
        let paso = historial.deshacer(vista("C")).expect("vuelve a B");
        assert_eq!(paso.visual, "B");
        let paso = historial.deshacer(paso).expect("vuelve a A");
        assert_eq!(paso.visual, "A");
        assert!(historial.deshacer(paso).is_none());
    }

    #[test]
    fn rehacer_devuelve_el_paso_deshecho() {
        let mut historial = Historial::default();
        historial.recordar(vista("A"));
        historial.recordar(vista("B"));

        let b = historial.deshacer(vista("C")).expect("vuelve a B");
        let a = historial.deshacer(b).expect("vuelve a A");
        assert_eq!(historial.rehacer(a).map(|vista| vista.visual), Some("B".to_string()));
        assert_eq!(
            historial.rehacer(vista("B")).map(|vista| vista.visual),
            Some("C".to_string())
        );
        assert!(historial.rehacer(vista("C")).is_none());
    }

    #[test]
    fn un_cambio_nuevo_borra_el_futuro() {
        // Despues de deshacer, tocar otra cosa deja el futuro apuntando a un estado que ya no
        // sigue de este. Rehacer tiene que llevar a ninguna parte, no a un salto raro.
        let mut historial = Historial::default();
        historial.recordar(vista("A"));
        historial.recordar(vista("B"));
        let _ = historial.deshacer(vista("C"));
        historial.recordar(vista("D"));
        assert!(historial.rehacer(vista("D")).is_none());
    }

    #[test]
    fn el_historial_tiene_tope() {
        // Sin tope, una sesion larga guarda una vista por clic para siempre. Con tope, se
        // pierden los pasos mas viejos y se conservan los ultimos.
        let mut historial = Historial::default();
        for paso in 0..HISTORIAL_MAX + 5 {
            historial.recordar(vista(&paso.to_string()));
        }
        assert_eq!(historial.atras.len(), HISTORIAL_MAX);
        assert_eq!(
            historial.atras.first().map(|vista| vista.visual.as_str()),
            Some("5")
        );
    }

    #[test]
    fn sin_busqueda_no_hay_coincidencia() {
        // `contains("")` es verdadero para cualquier texto, asi que la comprobacion
        // ingenua dice que TODAS las tablas coinciden cuando no se busca nada. El panel
        // entonces abre las tres tablas y muestra los treinta campos: la lista deja de
        // caber y no se ve lo que hay.
        assert!(!hit("region", ""));
        assert!(!hit("catalogo_productos", ""));
    }

    fn hoja(op: BinaryOp, left: Shape, right: Shape) -> Shape {
        Shape::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    #[test]
    fn la_linea_de_una_tarjeta_solo_suma_formas_que_suman() {
        // Sumar doce meses vale para las medidas que suman filas. Un promedio de promedios y
        // un `DISTINCTCOUNT` de meses no son el total del anio, y una division tampoco.
        let kinds = [
            AtomKind::Sum,
            AtomKind::Count,
            AtomKind::Average,
            AtomKind::DistinctCount,
        ];
        let suma = Shape::Atom(0);
        let cuenta = Shape::Atom(1);
        let promedio = Shape::Atom(2);
        let distintos = Shape::Atom(3);

        assert!(forma_aditiva(&suma, &kinds));
        assert!(forma_aditiva(&cuenta, &kinds));
        assert!(forma_aditiva(&Shape::Number(1.0), &kinds));
        assert!(forma_aditiva(
            &hoja(BinaryOp::Subtract, suma.clone(), Shape::Atom(0)),
            &kinds
        ));
        assert!(!forma_aditiva(&promedio, &kinds));
        assert!(!forma_aditiva(&distintos, &kinds));
        assert!(!forma_aditiva(
            &hoja(BinaryOp::Divide, suma.clone(), Shape::Atom(0)),
            &kinds
        ));
        assert!(!forma_aditiva(
            &hoja(BinaryOp::Multiply, suma.clone(), Shape::Number(2.0)),
            &kinds
        ));
    }

    #[test]
    fn la_busqueda_ignora_mayusculas_y_mira_lo_que_se_lee() {
        assert!(hit("Fecha", "fecha"));
        assert!(hit("catalogo_productos", "catalogo"));
        assert!(hit("precio_unitario", "unitario"));
        assert!(!hit("catalogo_productos", "region"));
    }
}

#[cfg(test)]
mod fecha_tests {
    use super::date_column;
    use datan_model::{
        Cardinality, Column, DataType, Model, ObjectId, Relationship, RelationshipRole, Table,
        TableKind,
    };
    use datan_report::Report;

    /// Una tabla de hechos con timestamp de insercion Y fecha de negocio. El periodo tiene
    /// que caer en la fecha de negocio: el timestamp concentra todo en el dia en que se
    /// cargo la tabla, y una ventana de doce meses sobre un solo dia no compara nada.
    #[test]
    fn la_fecha_de_negocio_gana_al_timestamp_de_insercion() {
        let model = Model {
            tables: vec![
                Table {
                    id: ObjectId(1),
                    name: "boleta".to_string(),
                    kind: TableKind::Facts,
                    columns: vec![
                        Column::new(ObjectId(10), "product_id", DataType::Integer),
                        Column::new(ObjectId(11), "bruto", DataType::Integer),
                        // Orden de declaracion: el timestamp ANTES que la fecha.
                        Column::new(ObjectId(12), "created_at", DataType::DateTime),
                        Column::new(ObjectId(13), "fecha", DataType::Date),
                    ],
                    key_columns: vec![],
                    is_date_table: false,
                },
                Table {
                    id: ObjectId(2),
                    name: "dim_producto".to_string(),
                    kind: TableKind::Dimension,
                    columns: vec![
                        Column::new(ObjectId(21), "product_id", DataType::Integer),
                        Column::new(ObjectId(22), "nombre", DataType::Text),
                    ],
                    key_columns: vec![ObjectId(21)],
                    is_date_table: false,
                },
            ],
            measures: vec![datan_model::Measure::new(
                ObjectId(100),
                "Bruto",
                "SUM('boleta'[bruto])",
            )],
            relationships: vec![Relationship {
                from_column: ObjectId(21),
                to_column: ObjectId(10),
                cardinality: Cardinality::OneToMany,
                direction: datan_model::FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            }],
        };
        let report = Report::new(model);
        // 13 es `fecha`, no 12 (`created_at`).
        assert_eq!(date_column(&report), Some(ObjectId(13)));
    }
}

#[cfg(test)]
mod drill_tests {
    use super::drill_objetivo;
    use datan_model::ObjectId;

    /// Categoria -> region: bajar llega a region, subir desde categoria no hay, y una
    /// dimension que no es nivel de ninguna jerarquia no taladra.
    #[test]
    fn el_drill_reemplaza_el_nivel_y_no_apila() {
        let jerarquias = vec![(
            "Producto".to_string(),
            vec![ObjectId(22), ObjectId(21)],
        )];

        assert_eq!(
            drill_objetivo(&jerarquias, ObjectId(22), true),
            Some(ObjectId(21)),
            "bajar desde categoria"
        );
        assert_eq!(
            drill_objetivo(&jerarquias, ObjectId(21), false),
            Some(ObjectId(22)),
            "subir desde region"
        );
        assert_eq!(
            drill_objetivo(&jerarquias, ObjectId(22), false),
            None,
            "desde el primer nivel no hay arriba"
        );
        assert_eq!(
            drill_objetivo(&jerarquias, ObjectId(21), true),
            None,
            "desde el ultimo nivel no hay abajo"
        );
        assert_eq!(
            drill_objetivo(&jerarquias, ObjectId(99), true),
            None,
            "una dimension ajena no taladra"
        );
    }
}
