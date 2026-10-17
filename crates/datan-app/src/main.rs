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
use datan_model::{DataType, Model, ObjectId, TableKind};
mod config;

use config::Connections;
use datan_report::{Header, Matrix, Report};
use datan_storage::Engine;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
// `Div::on_click` vive en este trait y `gpui-kit` reexporta `gpui::*`, asi que entra aqui.
use gpui_kit::component::Theme;
// `gpui-kit` es una fachada sobre gpui: reexporta sus tipos pero no sus traits de
// interactividad, que son los que traen `on_click`, `when` y los estilos de hover.
// El crate se llama `gpui-pre` pero su libreria se llama `gpui`. Por eso el alias.
use ::gpui::InteractiveElement;
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
    row_count: usize,
}

enum State {
    Connecting,
    Choosing,
    Ready,
    Failed(String),
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
    matrix: Option<Matrix>,
    grid: Option<Grid>,
    /// El total de cada medida seleccionada, para las tarjetas.
    totals: Vec<(String, String)>,
    /// La barra de formula DAX.
    ///
    /// Se crea en el primer render porque `InputState::new` pide una `Window`, y `new` de la
    /// vista no la tiene. Es el patron habitual en gpui.
    formula: Option<Entity<InputState>>,
    /// Cuantas consultas costo el ultimo informe, y cuantos ms.
    stats: Option<String>,
    database: Option<String>,
    scroll: UniformListScrollHandle,
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
            formula: None,
            stats: None,
            database: None,
            scroll: UniformListScrollHandle::default(),
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
                    .rounded_md()
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
                            .text_size(px(12.0))
                            .text_color(theme.muted_foreground)
                            .child(connection.database.clone()),
                    )
                    .on_click(cx.listener({
                        move |view, _event, _window, cx| {
                            eprintln!("[datan] click en conexion: {database}");
                            view.connect(&database, cx)
                        }
                    })),
            );
        }

        list.into_any_element()
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

        let next_id = model
            .measures
            .iter()
            .map(|m| m.id.0)
            .max()
            .unwrap_or(10_000)
            + 1;
        let id = ObjectId(next_id);
        model.measures.push(datan_model::Measure::new(
            id,
            name.clone(),
            expression.clone(),
        ));

        self.formula_error = None;
        self.report = Some(
            Report::new(model)
                .attached_to(&report.qualifier)
                .with_dimensions(self.selection.dimensions.clone())
                .with_measures(self.selection.measures.clone()),
        );
        self.toggle(id, true, cx);
        eprintln!("[datan] medida creada: {name} = {expression}");
    }

    /// Guarda el informe actual. Sin nombre de archivo elegido a mano: el informe se llama
    /// por la conexion y la primera dimension, que es lo que lo distingue. Un dialogo de
    /// nombre no se puede verificar sin pantalla.
    fn save_report(&mut self, cx: &mut Context<Self>) {
        let Some(report) = self.report.clone() else {
            return;
        };
        let Some(matrix) = self.matrix.clone() else {
            return;
        };

        let name = report
            .model
            .column(*report.dimensions.first().unwrap_or(&ObjectId(0)))
            .map(|(_, c)| c.name.clone())
            .unwrap_or_else(|| "informe".to_string());
        let label = format!("{name} ({} filas)", matrix.row_count());

        match datan_report::saved::save(&report, &name) {
            Ok(()) => {
                self.stats = Some(format!("guardado como `{label}`"));
                cx.notify();
            }
            Err(error) => {
                self.stats = Some(format!("no se pudo guardar: {error}"));
                cx.notify();
            }
        }
    }

    /// Exporta la matriz a CSV junto a la carpeta del usuario.
    fn export_csv(&mut self, cx: &mut Context<Self>) {
        let Some(matrix) = self.matrix.clone() else {
            return;
        };
        let path = std::env::var("HOME")
            .map(|home| std::path::PathBuf::from(home).join("Desktop").join("datan.csv"))
            .unwrap_or_else(|_| std::path::PathBuf::from("datan.csv"));

        match datan_report::export::write_csv(&matrix, &path) {
            Ok(()) => {
                self.stats = Some(format!("exportado a {}", path.display()));
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
                    Ok((model, report)) => {
                        view.selection = suggested(&model);
                        view.report = Some(report);

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

    /// Cambia una dimension o medida de la seleccion y vuelve a correr el informe.
    fn toggle(&mut self, column: ObjectId, is_measure: bool, cx: &mut Context<Self>) {
        if is_measure {
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
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(base) = self.report.clone() else {
            return;
        };
        let mut report = base
            .with_dimensions(self.selection.dimensions.clone())
            .with_measures(self.selection.measures.clone());
        // Los filtros activos se vuelven a aplicar en orden. Vienen del cross-filter, y el
        // informe base es el que tiene el filtro de la ULTIMA pulsacion: reconstruir desde
        // cero con la lista de filtros hace que quitar uno no deshaga los de despues.
        for filter in &self.filters {
            if let Some(definition) = report.model.column(filter.column) {
                let value = match definition.1.data_type {
                    DataType::Integer | DataType::Moneda | DataType::Decimal => {
                        datan_dax_eval::FilterValue::Integer(filter.value.parse().unwrap_or(0))
                    }
                    _ => datan_dax_eval::FilterValue::Text(filter.value.to_string()),
                };
                report = report.filtered_by(filter.column, value);
            }
        }

        let database = self
            .database
            .clone()
            .unwrap_or_else(|| "datan_bench".to_string());
        cx.spawn(async move |this, cx| {
            let built = cx
                .background_spawn(async move { run_report(report, database, None) })
                .await;
            _ = this.update(cx, |view, cx| {
                match built {
                    Ok((matrix, stats, cards)) => {
                        // Volver arriba tras filtrar. Sin esto, el usuario pincha una fila
                        // del medio, el resultado baja a tres filas, y se queda mirando la
                        // posicion 400 de una lista de tres: el grid parece vacio.
                        view.scroll.scroll_to_item(0, ScrollStrategy::Top);
                        view.grid = Some(build_grid(&matrix));
                        view.matrix = Some(matrix);
                        view.stats = Some(stats);
                        view.totals = cards;
                        view.formula_error = None;
                        view.state = State::Ready;
                    }
                    Err(error) => view.state = State::Failed(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Cross-filter: pinchar una celda de una dimension.
    fn drill(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
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
fn build_model(database: &str) -> Result<(Model, Report), String> {
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
    let model = introspection.model;

    let report = Report::new(model.clone()).attached_to("pg");
    Ok((model, report))
}

fn run_report(
    report: Report,
    database: String,
    _unused: Option<()>,
) -> Result<(Matrix, String, Vec<(String, String)>), String> {
    let queries = report.query_count().unwrap_or(0);
    let started = Instant::now();

    // El motor se abre aqui y se cae al final: `Engine` es `Send` pero no `Sync`, asi que
    // compartirlo entre tareas exigiria un canal. Abrirlo cuesta unos milisegundos y hace el
    // hilo de trabajo simple.
    let engine = open_engine()?;
    attach(&engine, &database)?;

    let matrix = report.execute(&engine, None).map_err(|e| e.to_string())?;
    let totals = report.totals(&engine, None).map_err(|e| e.to_string())?;

    let elapsed = started.elapsed();
    let stats = format!(
        "{queries} consulta(s), {} filas, {} columnas, {elapsed:?}",
        matrix.row_count(),
        matrix.width()
    );
    // Los nombres de las medidas, en el MISMO orden que los totales: la tarjeta i es la
    // medida i.
    let cards = report
        .measures
        .iter()
        .zip(&totals)
        .map(|(id, total)| {
            let name = report
                .measure_name(*id)
                .unwrap_or("?")
                .to_string();
            (name, total.clone())
        })
        .collect();

    let _ = _unused;
    Ok((matrix, stats, cards))
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

fn build_grid(matrix: &Matrix) -> Grid {
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
            let values: Vec<SharedString> = (0..matrix.row_count())
                .map(|row| SharedString::from(matrix.cell(row, column).unwrap_or("")))
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
        row_count: matrix.row_count(),
    }
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
    div()
        .id(ElementId::Name(id))
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.0))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
        .into_any_element()
}

fn retry_button(theme: &Theme, database: &str, cx: &mut Context<Datan>) -> AnyElement {
    let database = database.to_string();
    clickable(
        theme,
        "retry".into(),
        "Reintentar".to_string(),
        cx.listener(move |view, _event, _window, cx| view.connect(&database, cx)),
    )
}

fn clear_button(theme: &Theme, cx: &mut Context<Datan>) -> AnyElement {
    clickable(theme, "clear".into(), "Limpiar todo".to_string(), cx.listener(
        |view, _event, _window, cx| {
            view.filters.clear();
            view.refresh(cx);
        },
    ))
}

/// Una pastilla de filtro activo. Pincharla quita el filtro.
fn filter_pill(
    theme: &Theme,
    filter: &ActiveFilter,
    index: usize,
    cx: &mut Context<Datan>,
) -> AnyElement {
    let label = format!("{} = {}  x", filter.label, filter.value);
    div()
        .id(ElementId::Name(SharedString::from(format!("pill-{index}"))))
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.0))
        .cursor_pointer()
        .hover(|style| style.opacity(0.7))
        .child(label)
        .on_click(cx.listener(move |view, _event, _window, cx| {
            view.clear_filter(index, cx);
        }))
        .into_any_element()
}

/// Una fila de la barra lateral.
fn sidebar_item(
    theme: &Theme,
    label: String,
    is_on: bool,
    column: ObjectId,
    is_measure: bool,
    cx: &mut Context<Datan>,
) -> AnyElement {
    let mut element = div()
        .id(ElementId::Name(SharedString::from(format!(
            "side-{}-{}",
            if is_measure { "m" } else { "d" },
            column.0
        ))))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(13.0))
        .cursor_pointer()
        .hover(|style| style.bg(theme.list_hover))
        .child(div().w(px(12.0)).child(if is_on { "\u{2022}" } else { "" }))
        .child(label);
    if is_on {
        element = element.bg(theme.accent);
    }
    element
        .on_click(cx.listener(move |view, _event, _window, cx| {
            view.toggle(column, is_measure, cx);
        }))
        .into_any_element()
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
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(20.0))
                        .child("Datan"),
                )
                .child(
                    div()
                        .text_size(px(13.0))
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
            State::Failed(error) => div()
                .flex()
                .flex_1()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("No se pudo conectar"),
                )
                .child(div().child(error.clone()))
                .child(
                retry_button(&theme, self.database.as_deref().unwrap_or("datan_bench"), cx),
            )
                .into_any_element(),
            _ => self.render_report(window, cx),
        };

        div()
            .flex()
            .size_full()
            .bg(theme.background)
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
        let dimensions: Vec<ObjectId> = model
            .tables
            .iter()
            .filter(|t| matches!(t.kind, TableKind::Dimension) && connected(t))
            .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
            .filter(|(t, c)| !t.key_columns.contains(&c.id) && !matches!(c.data_type, DataType::Date))
            .map(|(_, c)| c.id)
            .collect();
        let measures: Vec<ObjectId> = model.measures.iter().map(|m| m.id).collect();

        let sidebar = div()
            .w(px(264.0))
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Datan"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px_2()
                    .py_2()
                    .child(self.render_sidebar_section("Dimensiones", &dimensions, false, &model, cx))
                    .child(self.render_sidebar_section("Medidas", &measures, true, &model, cx)),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child(self.stats.clone().unwrap_or_else(|| "sin datos".into())),
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
                    .text_size(px(12.0))
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
        filter_bar = filter_bar.child(save_button(&theme, cx));
        filter_bar = filter_bar.child(export_button(&theme, cx));

        // --- Barra de formula DAX ---
        let mut canvas = div().flex().flex_col().flex_1().overflow_hidden();
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
                .px_2()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.muted_foreground)
                        .flex_shrink_0()
                        .child("DAX"),
                )
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(self.formula.as_ref().expect("recien creada"))),
                )
                .child(add_measure_button(&theme, cx)),
        );
        if let Some(error) = formula_error {
            canvas = canvas.child(
                div()
                    .px_3()
                    .pb_1()
                    .text_size(px(12.0))
                    .text_color(theme.danger_foreground)
                    .child(error),
            );
        }
        if !self.totals.is_empty() {
            let cards: Vec<AnyElement> = self
                .totals
                .iter()
                .map(|(name, total)| {
                    div()
                        .flex_1()
                        .mx_2()
                        .my_2()
                        .p_4()
                        .rounded_lg()
                        .bg(theme.muted)
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.muted_foreground)
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(20.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(if total.is_empty() {
                                    "-".to_string()
                                } else {
                                    total.clone()
                                }),
                        )
                        .into_any_element()
                })
                .collect();
            canvas = canvas.child(div().flex().flex_shrink_0().children(cards));
        }

        // --- Grafico de barras: la primera dimension contra la primera medida ---
        if let Some(matrix) = self.matrix.as_ref()
            && matrix.row_count() > 0
            && self.selection.dimensions.len() > 0
            && self.selection.measures.len() > 0
        {
            let dim_count = self.selection.dimensions.len();
            let top = matrix.top(0, dim_count, 8);
            let max = top.iter().map(|(_, v)| *v).fold(0.0f64, f64::max);
            if max > 0.0 && top.len() > 1 {
                let measure_name = matrix
                    .header(dim_count)
                    .map(|h| h.label.clone())
                    .unwrap_or_default();

                let bars: Vec<AnyElement> = top
                    .iter()
                    .map(|(label, value)| {
                        let fraction = (value / max).clamp(0.02, 1.0);
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(150.0))
                                    .flex_shrink_0()
                                    .text_size(px(12.0))
                                    .text_color(theme.muted_foreground)
                                    .overflow_hidden()
                                    .child(label.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .h(px(18.0))
                                    .rounded_md()
                                    .child(
                                        div()
                                            .flex_grow(fraction as f32 * 1000.0)
                                            .h_full()
                                            .rounded_md()
                                            .bg(theme.accent),
                                    )
                                    .child(div().flex_grow((1.0 - fraction) as f32 * 1000.0)),
                            )
                            .child(
                                div()
                                    .w(px(90.0))
                                    .flex_shrink_0()
                                    .text_size(px(12.0))
                                    .text_align(TextAlign::Right)
                                    .child(format_value(*value)),
                            )
                            .into_any_element()
                    })
                    .collect();

                canvas = canvas.child(
                    div()
                        .mx_2()
                        .my_2()
                        .p_4()
                        .rounded_lg()
                        .bg(theme.muted)
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} por {} (top {})",
                                    measure_name,
                                    self.report
                                        .as_ref()
                                        .and_then(|r| r
                                            .model
                                            .column(self.selection.dimensions[0])
                                            .map(|(_, c)| c.name.clone()))
                                        .unwrap_or_default(),
                                    top.len()
                                )),
                        )
                        .child(div().flex_col().gap_1().children(bars)),
                );
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
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
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
                                    .text_size(px(12.0))
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        "Revisa los filtros, o prueba la base de pruebas                                          desde la pantalla de conexion.",
                                    ),
                            ),
                    )
                    .into_any_element()
            });

        // --- El grid ---
        let body: AnyElement = if self.grid.is_some() {
            self.render_grid(cx).into_any_element()
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

        let content = match empty_state {
            Some(empty) => div().flex().flex_1().flex_col().child(empty),
            None => div()
                .flex()
                .flex_1()
                .flex_col()
                .child(filter_bar)
                .child(canvas)
                .child(body),
        };

        div()
            .flex()
            .flex_1()
            .child(sidebar)
            .child(content)
            .into_any_element()
    }

    fn render_sidebar_section(
        &self,
        title: &str,
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
            .mb_3()
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .text_size(px(11.0))
                    .text_color(theme.muted_foreground)
                    .child(title.to_uppercase()),
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
        let Some(grid) = self.grid.clone() else {
            return div().into_any_element();
        };
        let columns = Arc::clone(&grid.columns);
        let row_count = grid.row_count;
        let dimension_count = self
            .report
            .as_ref()
            .map(|report| report.dimensions.len())
            .unwrap_or(0);
        // `uniform_list` entrega un `&mut App`, no un `&mut Context`. Para tocar la vista
        // desde el manejador del clic hay que pasar por la entidad.
        let view = cx.entity();

        let header = div()
            .flex()
            .flex_shrink_0()
            .bg(theme.muted)
            .children(columns.iter().enumerate().map(|(index, column)| {
                div()
                    .w(px(200.0))
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{}{}",
                        column.header,
                        // La flecha solo en las dimensiones: es lo que se puede filtrar.
                        if index < dimension_count { "  \u{25be}" } else { "" }
                    ))
            }));

        let body = uniform_list(
            ElementId::View(cx.entity().entity_id()),
            row_count,
            move |range, _window, _cx| {
                range
                    .map(|row| {
                        let cells = columns.iter().enumerate().map(|(index, column)| {
                            // Las celdas de medida y las de dimension son tipos distintos
                            // (Div y Stateful<Div>), asi que la fila se arma de elementos ya
                            // convertidos.
                            
                            let mut cell = div()
                                .w(px(200.0))
                                .flex_shrink_0()
                                .px_3()
                                .py_1()
                                .text_size(px(13.0))
                                .child(
                                    column
                                        .values
                                        .get(row)
                                        .cloned()
                                        .unwrap_or_default(),
                                );
                            if column.numeric {
                                cell = cell.text_align(TextAlign::Right);
                            }

                            if index >= dimension_count {
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
                                    eprintln!(
                                        "[datan] click en celda ({row_index}, {column_index})"
                                    );
                                    view.update(app, |view, cx| {
                                        view.drill(row_index, column_index, cx);
                                    });
                                })
                                .into_any_element()
                        });

                        div()
                            .flex()
                            .border_b_1()
                            .border_color(theme.border)
                            .children(cells)
                    })
                    .collect::<Vec<_>>()
            },
        );

        div()
            .flex_1()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

fn add_measure_button(theme: &Theme, cx: &mut Context<Datan>) -> AnyElement {
    clickable(
        theme,
        "add-measure".into(),
        "Crear medida".to_string(),
        cx.listener(|view, _event, _window, cx| view.add_measure(cx)),
    )
}

fn save_button(theme: &Theme, cx: &mut Context<Datan>) -> AnyElement {
    clickable(theme, "save".into(), "Guardar".to_string(), cx.listener(
        |view, _event, _window, cx| view.save_report(cx),
    ))
}

fn export_button(theme: &Theme, cx: &mut Context<Datan>) -> AnyElement {
    clickable(theme, "export".into(), "CSV".to_string(), cx.listener(
        |view, _event, _window, cx| view.export_csv(cx),
    ))
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
        .with_assets(assets::Assets)
        .run(|cx| {
            init(cx);
            open_window(WindowOptions::default(), cx, |_, cx| cx.new(Datan::new))
                .expect("no se pudo abrir la ventana");
        });
}