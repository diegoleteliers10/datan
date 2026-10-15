//! Spike de la Fase 0. Ver seccion 6, Fase 0 de PLAN.md.
//!
//! Objetivo: validar antes de escribir una linea de motor DAX que
//!
//! 1. Un resultado de 1M x 9 cruza la frontera hacia el hilo de UI como `Send + Sync`.
//! 2. El hilo de UI no contiene ningun tipo de `duckdb` ni de `arrow`.
//! 3. El scroll mantiene 16 ms con la tabla entera residente en memoria.
//! 4. `cargo tree -d | grep arrow` imprime exactamente una version de arrow.
//!
//! Sobre el punto 2: la extraccion de Arrow a `ColumnarTable` ocurre en `datan-storage`,
//! en el hilo de trabajo. Este archivo no importa `duckdb`. Ese es el protocolo completo del
//! contrato motor/UI de la seccion 9 de PLAN.md.

use datan_storage::{ColumnData, ColumnKind};
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Columna formateada, lista para pintar. Se construye en el hilo de trabajo.
///
/// Cada valor es un `SharedString`, que es un string compartido por referencia y no una
/// asignacion por celda. Para columnas repetidas el coste real es el numero de valores
/// distintos, no el de filas.
struct Column {
    header: SharedString,
    values: Arc<[SharedString]>,
    numeric: bool,
}

struct Table {
    columns: Arc<[Column]>,
    row_count: usize,
}

enum State {
    Loading,
    Ready(Table),
    Failed(String),
}

struct Spike {
    state: State,
    scroll: UniformListScrollHandle,
    /// Cuanto falta para el siguiente paso de auto-scroll.
    scroll_tick: usize,
}

impl Spike {
    fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            // `ColumnarTable` es dato plano sin tipos de GPUI ni de arrow, asi que es
            // `Send`. Todo el trabajo pesado ocurre en el executor de background.
            let built = cx
                .background_spawn(async move { build_table() })
                .await;

            _ = this.update(cx, |view, cx| {
                view.state = match built {
                    Ok(table) => State::Ready(table),
                    Err(error) => State::Failed(error),
                };
                cx.notify();
            });
        })
        .detach();

        cx.spawn(async move |this, cx| {
            // Un timer de 16 ms fuerza un repintado por frame. MacOS bloquea la inyeccion de
            // eventos del sistema, asi que el spike genera su propio scroll.
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                _ = this.update(cx, |view, cx| view.tick(cx));
            }
        })
        .detach();

        Self {
            state: State::Loading,
            scroll: UniformListScrollHandle::default(),
            scroll_tick: 0,
        }
    }

    /// Auto-scroll. Scrollea el grid desde codigo para que el spike mida frames reales sin
    /// depender de inyeccion de eventos del sistema, que macOS bloquea.
    ///
    /// `scroll_to_item` salta al indice pedido. El spike recorre la tabla completa para
    /// que `uniform_list` tenga que montar filas distintas en cada frame.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let State::Ready(table) = &self.state else {
            return;
        };
        self.scroll_tick += 1;
        let row_count = table.row_count;
        let target = (self.scroll_tick * 977) % row_count.max(1);
        self.scroll.scroll_to_item(target, ScrollStrategy::Top);
        cx.notify();
    }
}

impl Render for Spike {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = Instant::now();

        let body: AnyElement = match &self.state {
            State::Loading => div()
                .flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("Generando 1M filas...")
                .into_any_element(),
            State::Failed(error) => div()
                .flex()
                .flex_col()
                .size_full()
                .gap_2()
                .p_4()
                .items_center()
                .justify_center()
                .child("El spike fallo")
                .child(error.clone())
                .into_any_element(),
            State::Ready(table) => {
                let row_count = table.row_count;
                let scroll = self.scroll.clone();
                render_grid(table, cx, row_count, scroll)
            }
        };

        let build_us = started.elapsed().as_micros();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(format!("Datan spike, auto-scroll {}", self.scroll_tick))
                    .child(format!("build {build_us} us")),
            )
            .child(body)
    }
}

/// Acumuladores de telemetria del grid. Los lee un hilo de reporte aparte para no
/// perturbar el frame que se quiere medir.
static FRAME_US: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// `uniform_list` en gpui 0.3.9 recibe `(range, window, cx)` y devuelve `Vec<R>`.
///
/// Mide el primer elemento y lays out el resto en linea. Por eso todas las filas deben
/// tener la misma altura. Es exactamente lo que quiere un grid de datos.
fn render_grid(
    table: &Table,
    cx: &mut Context<Spike>,
    row_count: usize,
    scroll: UniformListScrollHandle,
) -> AnyElement {
    let theme = cx.theme().clone();
    let columns = Arc::clone(&table.columns);


    let header = div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .border_b_1()
        .border_color(theme.border)
        .children(table.columns.iter().map(|column| {
            div()
                .px_3()
                .py_1()
                .text_color(theme.muted_foreground)
                .child(column.header.clone())
        }));

    let body = uniform_list(
        ElementId::View(cx.entity().entity_id()),
        row_count,
        move |range, _window, _cx| {
            let started = Instant::now();
            let rows = range.map(|row| {
                div().flex().flex_row().children(columns.iter().enumerate().map(
                    |(_index, column)| {
                        let cell = &column.values[row];
                        if column.numeric {
                            div().flex_1().px_3().flex().justify_end().child(cell.clone())
                        } else {
                            div().flex_1().px_3().child(cell.clone())
                        }
                    },
                ))
            });
            let built = rows.collect::<Vec<_>>();

            // Mide el coste de construir los elementos visibles. Es el termino dominante del
            // frame en un grid y el que revela si la virtualizacion funciona. Si este numero
            // crece con el numero de filas, la virtualizacion esta rota.
            FRAME_US.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
            FRAMES.fetch_add(1, Ordering::Relaxed);

            built
        },
    );

    div()
        .flex()
        .flex_col()
        .size_full()
        .child(header)
        .child(body.track_scroll(&scroll).flex_1())
        .into_any_element()
}

/// Genera 1M x 9 en DuckDB, extrae a `ColumnarTable`, y formatea a `SharedString`.
///
/// Todo corre en el hilo de trabajo. Devuelve algo `Send` que no contiene tipos de GPUI
/// fuera de `SharedString`, que es precisamente la frontera que se quiere validar.
fn build_table() -> Result<Table, String> {
    let engine = datan_storage::Engine::open(":memory:").map_err(|e| e.to_string())?;

    engine
        .execute(
            "CREATE TABLE fact AS
             SELECT
                 i AS id,
                 'Cliente ' || (i % 5000)::VARCHAR AS customer,
                 'Producto ' || (i % 500)::VARCHAR AS product,
                 ['Norte','Sur','Este','Oeste'][(i % 4) + 1] AS region,
                 ['2025-Q1','2025-Q2','2025-Q3','2025-Q4'][(i % 4) + 1] AS quarter,
                 (i % 5000 + 1)::DOUBLE * 9.99 AS revenue,
                 (i % 5000 + 1)::DOUBLE * 4.49 AS cogs,
                 (i % 37)::DOUBLE AS qty
             FROM range(1000000) t(i)",
        )
        .map_err(|e| e.to_string())?;

    let schema: Vec<(&str, ColumnKind)> = vec![
        ("id", ColumnKind::Int),
        ("customer", ColumnKind::Text),
        ("product", ColumnKind::Text),
        ("region", ColumnKind::Text),
        ("quarter", ColumnKind::Text),
        ("revenue", ColumnKind::Float),
        ("cogs", ColumnKind::Float),
        ("qty", ColumnKind::Float),
    ];

    let batches = engine
        .query_arrow(
            "SELECT id, customer, product, region, quarter, revenue, cogs, qty FROM fact",
        )
        .map_err(|e| e.to_string())?;

    let extracted = datan_storage::extract_columns(&batches, &schema).map_err(|e| e.to_string())?;

    let mut columns = Vec::with_capacity(extracted.columns.len());
    for column in extracted.columns {
        let numeric = !matches!(column.data, ColumnData::Text(_));
        let values: Vec<SharedString> = match column.data {
            ColumnData::Text(text) => text.into_iter().map(SharedString::from).collect(),
            ColumnData::Float(values) => values
                .into_iter()
                .map(|v| match v {
                    Some(v) => format!("{v:.2}"),
                    None => String::new(),
                })
                .map(SharedString::from)
                .collect(),
            ColumnData::Int(values) => values
                .into_iter()
                .map(|v| match v {
                    Some(v) => v.to_string(),
                    None => String::new(),
                })
                .map(SharedString::from)
                .collect(),
        };
        columns.push(Column {
            header: SharedString::from(column.name),
            values: values.into(),
            numeric,
        });
    }

    eprintln!(
        "[spike] tabla construida: {} filas x {} columnas en el hilo de trabajo",
        extracted.row_count,
        columns.len()
    );

    Ok(Table {
        columns: columns.into(),
        row_count: extracted.row_count,
    })
}

fn main() {
    // Reporte de telemetria. Corre en un hilo aparte para no tocar el frame que se mide.
    std::thread::spawn(|| {
        let mut last_frames = 0u64;
        let mut last_us = 0u64;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let frames = FRAMES.load(Ordering::Relaxed);
            let us = FRAME_US.load(Ordering::Relaxed);
            let delta_frames = frames - last_frames;
            let delta_us = us - last_us;
            last_frames = frames;
            last_us = us;
            if delta_frames > 0 {
                let mean = delta_us / delta_frames;
                eprintln!(
                    "[spike] {delta_frames} frames, media {mean} us por construccion de filas visibles"
                );
            }
        }
    });

    application()
        .with_assets(assets::Assets)
        .run(|cx| {
            init(cx);
            open_window(WindowOptions::default(), cx, |_, cx| cx.new(Spike::new))
                .expect("no se pudo abrir la ventana");
        });
}