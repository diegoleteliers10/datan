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
    /// El visual activo del lienzo: Todo, Tarjetas, Barras o Tabla.
    visual: String,
    /// El indicador de la barra superior tras guardar.
    last_saved: Option<SharedString>,
    /// Orden de la matriz: (indice de columna, descendente). Tercer clic lo quita.
    sort: Option<(usize, bool)>,
    matrix: Option<Matrix>,
    grid: Option<Grid>,
    /// El total de cada medida seleccionada, para las tarjetas.
    /// (nombre, total, delta contra el mismo periodo del ano anterior).
    totals: Vec<(String, String, Option<f64>)>,
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
            visual: "Todo".to_string(),
            last_saved: None,
            sort: None,
            formula: None,
            search: None,
            date_months: None,
            expanded: Vec::new(),
            row_counts: Default::default(),
            counted_at: None,
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

    /// Filtra por el valor de texto de una dimension, como pinchar una barra.
    fn filter_by_label(&mut self, column: ObjectId, label: &str, cx: &mut Context<Self>) {
        // Pinchar dos veces la misma barra CAMBIA el valor, no acumula dos filtros que se
        // intersectan y dejan la matriz vacia.
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
                self.last_saved = Some(SharedString::from("Guardado"));
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
                        // Las filas por tabla se cuentan al conectar, no en cada refresh:
                        // el numero cambia muy despacio y el panel se dibuja en cada frame.
                        // La primera tabla empieza abierta: una lista de campos vacia parece
                        // un panel que no cargo.
                        let tablas: Vec<ObjectId> =
                            model.tables.iter().map(|t| t.id).collect();
                        view.expanded = tablas.first().copied().into_iter().collect();
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
    /// La columna de fecha que gobierna el rango. La de la tabla de hechos si la hay, si no
    /// la primera fecha de una tabla conectada.
    fn date_column(&self, model: datan_model::Model) -> Option<ObjectId> {
        let fact = self.report.as_ref().and_then(|r| r.fact_table().ok());
        let connected = |id: ObjectId| match fact {
            Some(fact_id) => fact_id == id || model.join_path(fact_id, id).is_some(),
            None => false,
        };
        let dates: Vec<(ObjectId, ObjectId)> = model
            .tables
            .iter()
            .filter(|t| connected(t.id))
            .flat_map(|t| t.columns.iter().map(move |c| (t.id, c)))
            .filter(|(_, c)| matches!(c.data_type, DataType::Date))
            .map(|(table, column)| (table, column.id))
            .collect();
        dates
            .iter()
            .find(|(table, _)| Some(*table) == fact)
            .or_else(|| dates.first())
            .map(|(_, column)| *column)
    }

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
        // El rango de fechas va DESPUES de la igualdad: es otro filtro sobre otra columna y
        // el orden no altera el AND, pero aplicarlo aqui garantiza que el rango se imponga
        // aunque el cross-filter haya tocado la fecha.
        // La ventana anterior se arma sobre una COPIA del informe con los mismos filtros de
        // igualdad, antes de imponer el rango. Si se partiera del informe ya acotado, el
        // rango actual y el anterior se intersectarian y el delta compararia una ventana de
        // doce meses contra un solo dia.
        let previous = match (self.date_months, self.date_column(report.model.clone())) {
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
        if let (Some(months), Some(date)) = (self.date_months, self.date_column(report.model.clone()))
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
                        view.last_saved = None;
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
    previous: Option<Report>,
    database: String,
) -> Result<(Matrix, String, Vec<(String, String, Option<f64>)>), String> {
    let queries = report.query_count().unwrap_or(0);
    let started = Instant::now();

    // El motor se abre aqui y se cae al final: `Engine` es `Send` pero no `Sync`, asi que
    // compartirlo entre tareas exigiria un canal. Abrirlo cuesta unos milisegundos y hace el
    // hilo de trabajo simple.
    let engine = open_engine()?;
    attach(&engine, &database)?;

    let matrix = report.execute(&engine, None).map_err(|e| e.to_string())?;
    let totals = report.totals(&engine, None).map_err(|e| e.to_string())?;
    // Una pasada extra de totales, sobre la ventana anterior. Las medidas comparten sus
    // grupos de lectura, asi que no son N consultas: son las mismas de `totals` con otro
    // filtro de fecha.
    let before = match &previous {
        Some(report) => Some(report.totals(&engine, None).map_err(|e| e.to_string())?),
        None => None,
    };

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
        .enumerate()
        .map(|(index, (id, total))| {
            let name = report.measure_name(*id).unwrap_or("?").to_string();
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
            (name, total.clone(), delta)
        })
        .collect();

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

/// Un numero con los puntos de miles, como los muestra la gente: 1.284.310.
fn count_text(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push('.');
        }
        out.push(ch);
    }
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
    let mut button = div()
        .id(ElementId::Name(SharedString::from(format!("range-{months}"))))
        .px_2()
        .h(px(24.0))
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .text_size(px(11.0))
        .cursor_pointer()
        .child(label.to_string());
    button = if active {
        button.bg(theme.accent).text_color(theme.background)
    } else {
        button.hover(|style| style.bg(theme.list_hover))
    };
    button.on_click(cx.listener(move |view, _event, _window, cx| {
        eprintln!("[datan] rango: {months}M");
        view.date_months = (months != 0).then_some(months);
        view.refresh(cx);
    }))
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
fn hit(text: &str, query: &str) -> bool {
    query.is_empty() || text.to_lowercase().contains(query)
}

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
        // --- Busqueda del panel Datos ---
        //
        // Filtra por lo que se LEE, no por el nombre tecnico: buscar "fecha" tiene que
        // encontrar la columna Fecha aunque su tabla se llame dim_tiempo.
        if self.search.is_none() {
            self.search = Some(cx.new(|cx| InputState::new(window, cx)));
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
            .filter(|(t, c)| !t.key_columns.contains(&c.id) && !matches!(c.data_type, DataType::Date))
            .filter(|(t, c)| hit(&c.name, &query) || hit(&t.name, &query))
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
            .filter(|m| hit(&m.name, &query))
            .map(|m| m.id)
            .collect();

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
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Datos"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.muted_foreground)
                            .child(format!("{} tablas", tablas.len())),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .h(px(28.0))
                            .rounded_md()
                            .bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.muted_foreground)
                                    .flex_shrink_0()
                                    .child("Buscar"),
                            )
                            .child(div().flex_1().child(Input::new(
                                self.search.as_ref().expect("recien creada"),
                            ))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px_2()
                    .py_2()
                    .child(self.render_tables(&tablas, &dimensions, &model, &query, cx))
                    .child(self.render_sidebar_section("Medidas", &measures, true, &model, cx))
                    .child(if query.is_empty() || !tablas.is_empty() || !measures.is_empty() {
                        div().into_any_element()
                    } else {
                        div()
                            .px_2()
                            .text_size(px(12.0))
                            .text_color(theme.muted_foreground)
                            .child(format!("Sin coincidencias para \"{query}\"."))
                            .into_any_element()
                    }),
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

        // --- Rango de fechas ---
        let mut date_bar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .pt_4()
            .pb_2();
        // El titulo de la pagina va arriba, con el nombre de la base como etiqueta y la
        // ultima actualizacion al lado: uno tiene que saber que informe mira y de cuando
        // son los numeros sin leer nada mas.
        let mut page_header = div().flex().items_center().gap_2().flex_1();
        page_header = page_header.child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.muted)
                .text_size(px(12.0))
                .child(
                    self.database
                        .clone()
                        .unwrap_or_else(|| "sin base".to_string()),
                ),
        );
        page_header = page_header.child(
            div()
                .text_size(px(12.0))
                .text_color(theme.muted_foreground)
                .child(match self.counted_at {
                    Some(at) => format!("Actualizado {}", ago_text(epoch_now() - at)),
                    None => "Contando filas".to_string(),
                }),
        );
        date_bar = date_bar.child(page_header);
        date_bar = date_bar.child(div().w(px(16.0)));
        date_bar = date_bar.child(
            div()
                .text_size(px(18.0))
                .font_weight(FontWeight::BOLD)
                .child("Resumen de ventas"),
        );
        date_bar = date_bar.child(div().flex_1());
        for months in [3u8, 6, 12] {
            let label = format!("{months}M");
            date_bar = date_bar.child(range_button(
                &theme,
                &label,
                self.date_months == Some(months),
                months,
                cx,
            ));
        }
        date_bar = date_bar.child(range_button(
            &theme,
            "Todo",
            self.date_months.is_none(),
            0,
            cx,
        ));
        // Sin columna de fecha conectada NO se muestran los botones: un rango que no
        // filtra nada y no lo dice es la falla silenciosa de siempre.
        if let Some((_, column)) = self
            .date_column(model.clone())
            .and_then(|date| model.column(date))
        {
            date_bar = date_bar.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.muted_foreground)
                    .child(format!("sobre {}", column.name)),
            );
        }

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
                .map(|(name, total, delta)| {
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
                                    total.parse::<f64>().map(format_value).unwrap_or(total.clone())
                                }),
                        )
                        .child(match delta {
                            Some(delta) => div()
                                .text_size(px(11.0))
                                .text_color(if *delta >= 0.0 {
                                    theme.accent
                                } else {
                                    theme.danger_foreground
                                })
                                .child(format!(
                                    "{} {:+.1}% vs ano anterior",
                                    if *delta >= 0.0 { "\u{2191}" } else { "\u{2193}" },
                                    delta * 100.0
                                )),
                            None => div()
                                .text_size(px(11.0))
                                .text_color(theme.muted_foreground)
                                .child("sin periodo anterior"),
                        })
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

                let bar_dimension = self.selection.dimensions[0];
                let bars: Vec<AnyElement> = top
                    .iter()
                    .map(|(label, value)| {
                        let fraction = (value / max).clamp(0.02, 1.0);
                        let value_label = label.clone();
                        let dimension_column = bar_dimension;
                        let bar_row = div()
                            .id(ElementId::Name(SharedString::from(format!(
                                "bar-{}",
                                value_label
                            ))))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.list_hover))
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
                            );
                        bar_row
                            .on_click(cx.listener(move |view, _event, _window, cx| {
                                view.filter_by_label(dimension_column, &value_label, cx);
                            }))
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

        // El lienzo segun el visual activo. La barra de formula vive dentro de `canvas`
        // porque es la entrada de medidas: se ve en todos los modos menos Tabla.
        let canvas_column = div()
            .flex()
            .flex_1()
            .flex_col()
            .overflow_hidden()
            .child(filter_bar)
            .child(match self.visual.as_str() {
                "Tabla" => div().into_any_element(),
                _ => div()
                    .flex_1()
                    .flex_col()
                    .child(date_bar)
                    .child(canvas)
                    .into_any_element(),
            })
            .child(match (self.visual.as_str(), empty_state) {
                ("Tabla", _) => div().into_any_element(),
                (_, Some(empty)) => empty,
                (_, None) => body,
            });

        // --- Panel Visualizaciones ---
        let tipos = [
            ("Tarjetas", "Tarjetas"),
            ("Barras", "Barras"),
            ("Tabla", "Tabla"),
        ];
        let mut viz_buttons = div().flex().flex_wrap().gap_1();
        for (label, key) in tipos {
            let active = self.visual == key || (key == "Tarjetas" && self.visual == "Todo");
            let mut button = div()
                .id(ElementId::Name(SharedString::from(format!("viz-{key}"))))
                .w(px(64.0))
                .h(px(44.0))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.0))
                .cursor_pointer()
                .child(label);
            if active {
                button = button.bg(theme.accent);
            } else {
                button = button.hover(|style| style.bg(theme.list_hover));
            }
            viz_buttons = viz_buttons.child(
                button.on_click(cx.listener(move |view, _event, _window, cx| {
                    eprintln!("[datan] visual: {key}");
                    view.visual = key.to_string();
                    cx.notify();
                })),
            );
        }

        let visualizaciones = div()
            .w(px(220.0))
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_l_1()
            .border_color(theme.sidebar_border)
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(12.0))
                    .child("Visualizaciones"),
            )
            .child(div().px_2().pb_2().child(viz_buttons))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .child("Propiedades"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.muted_foreground)
                            .child("Titulo: Informe"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.muted_foreground)
                            .child("Ancho: completo"),
                    ),
            );

        // --- Barra superior ---
        let mut top_bar = div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .h(px(48.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded_md()
                    .bg(theme.foreground)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(theme.background)
                    .child("D"),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(13.0))
                    .child("Datan"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{}  \u{203a}  Informe",
                        self.database.as_deref().unwrap_or("sin base")
                    )),
            );
        if let Some(saved) = &self.last_saved {
            top_bar = top_bar.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child(format!("\u{2713} {saved}")),
            );
        }
        top_bar = top_bar.child(div().flex_1());
        top_bar = top_bar.child(clickable(
            &theme,
            "refresh".into(),
            "Actualizar".to_string(),
            cx.listener(|view, _event, _window, cx| view.refresh(cx)),
        ));

        // Cuantos visuales hay ahora en el lienzo. Se cuenta lo que se PINTA, no lo que
        // el informe podria pintar.
        let con_filas = self.matrix.as_ref().is_some_and(|m| m.row_count() > 0);
        let grafico_listo = con_filas
            && !self.selection.dimensions.is_empty()
            && !self.selection.measures.is_empty();
        let visuales = match self.visual.as_str() {
            "Tarjetas" => usize::from(!self.totals.is_empty()),
            "Barras" => usize::from(grafico_listo),
            "Tabla" => usize::from(con_filas),
            _ => usize::from(!self.totals.is_empty())
                + usize::from(grafico_listo)
                + usize::from(con_filas),
        };

        // --- Pestañas de paginas ---
        let tabs = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(theme.muted)
                    .text_size(px(12.0))
                    .child("Resumen de ventas"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.muted_foreground)
                    // Cuantos visuales hay de verdad en el lienzo, no los que caben en
                    // el papel. Con "Tarjetas" seleccionado son dos, aunque el informe tenga
                    // ocho tipos disponibles.
                    .child(format!(
                        "{} visualizacion(es){}",
                        visuales,
                        match self.stats.clone() {
                            Some(stats) => format!("  ·  {stats}"),
                            None => String::new(),
                        }
                    )),
            );

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
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

        for table in tablas {
            // Con busqueda, una tabla se abre sola si tiene un campo que coincide: buscar
            // "region" tiene que mostrar el campo, no una lista de tablas cerradas.
            let auto = !query.is_empty()
                && table.columns.iter().any(|c| hit(&c.name, query))
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
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|style| style.bg(theme.list_hover))
                .child(
                    div()
                        .w(px(10.0))
                        .text_size(px(10.0))
                        .text_color(theme.muted_foreground)
                        .child(if open { "\u{25be}" } else { "\u{25b8}" }),
                )
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(12.0))
                        .flex_1()
                        .overflow_hidden()
                        .child(table.name.clone()),
                );
            if let Some(count) = count {
                header = header.child(
                    div()
                        .text_size(px(11.0))
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
                for definition in &table.columns {
                    let column = definition.id;
                    let is_measure = false;
                    let is_on = self.selection.dimensions.contains(&column);
                    // Con busqueda activa, la tabla abierta solo muestra los campos que
                    // coinciden: buscar "region" tiene que ENSEÑAR el campo Region.
                    if !query.is_empty() && !hit(&definition.name, query) {
                        continue;
                    }
                    list = list.child(sidebar_item(
                        &theme,
                        definition.name.clone(),
                        is_on,
                        column,
                        is_measure,
                        cx,
                    ));
                }
            }
        }

        let _ = (dimensions, model);
        list
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
                    .child(title.to_string()),
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
            // Ordenar en su sitio para que `drill` lea la misma fila que pinto. Un orden en
            // la pantalla y otro en el modelo haria que el clic filtrara por el valor de
            // OTRO producto.
            matrix.rows.sort_by(|a, b| {
                let parse = |row: &Vec<String>| {
                    row.get(column)
                        .and_then(|value| value.parse::<f64>().ok())
                };
                parse(a)
                    .partial_cmp(&parse(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            if descending {
                matrix.rows.reverse();
            }
        }

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
                let arrow = if index < dimension_count {
                    "  \u{25be}"
                } else {
                    match self.sort {
                        Some((sorted_column, false)) if sorted_column == index => "  \u{2191}",
                        Some((sorted_column, true)) if sorted_column == index => "  \u{2193}",
                        _ => "  \u{21c5}",
                    }
                };
                // Solo las MEDIDAS ordenan. Una dimension ya define las filas; ordenar por
                // ella es el orden del GROUP BY, que ya esta aplicado.
                let base = div()
                    .w(px(200.0))
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .text_size(px(12.0))
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