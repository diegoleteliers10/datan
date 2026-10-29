//! Formas de visual reutilizables y sus transformaciones de datos.

use crate::formato::Formato;
use datan_report::{Matrix, RowKind};
use gpui::{AnyElement, Pixels, SharedString, div, px, rgb};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::plot::shape::SankeyLink;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
};
use std::collections::BTreeMap;

pub type CategoryClick =
    Box<dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static>;

mod tooltip_element {
    use gpui_kit::component::tooltip::Tooltip;
    use gpui_kit::*;

    pub fn add_stateful(element: Stateful<Div>, label: String) -> Stateful<Div> {
        element.tooltip(move |window, cx| {
            Tooltip::new(SharedString::from(label.clone())).build(window, cx)
        })
    }
}

const ANCHO_GRAFICO: f32 = 320.0;
const ALTO_GRAFICO: f32 = 190.0;
const AZUL: u32 = 0x2563EB;
const VERDE: u32 = 0x16A34A;
const ROJO: u32 = 0xDC2626;
const GRIS: u32 = 0xCBD5E1;

fn px_lienzo(valor: f32, zoom: f32) -> Pixels {
    px(valor * zoom)
}

/// Una columna de medida en el pivote. Cada medida aparece una vez por cada tupla de columna.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnaMatriz {
    pub categorias: Vec<String>,
    pub medida: String,
    pub indice_medida: usize,
}

/// Una celda conserva la tupla dimensional original para el cross-filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeldaMatriz {
    pub filter_key: Vec<String>,
    pub valor: String,
}

/// Una fila del pivote, agrupada por las dimensiones de fila.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilaMatriz {
    pub row_key: Vec<String>,
    /// Alineadas con `MatrizPivote::columnas`. `None` indica una combinacion sin fila origen.
    pub celdas: Vec<Option<CeldaMatriz>>,
    /// Valores de subtotal, alineados con el orden de las medidas.
    pub totales: Vec<Option<String>>,
}

/// Datos listos para dibujar una matriz sin depender de GPUI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatrizPivote {
    pub columnas: Vec<ColumnaMatriz>,
    pub filas: Vec<FilaMatriz>,
    /// Valores del total general, alineados con el orden de las medidas.
    pub total_general: Vec<Option<String>>,
}

/// Devuelve un color azul claro a azul oscuro para una celda numerica.
/// Los rangos constantes y los valores no numericos no reciben color.
pub fn color_escala_matriz(valor: &str, minimo: f64, maximo: f64) -> Option<u32> {
    let valor = valor.parse::<f64>().ok()?;
    if !valor.is_finite() || !minimo.is_finite() || !maximo.is_finite() || maximo <= minimo {
        return None;
    }
    let proporcion = ((valor - minimo) / (maximo - minimo)).clamp(0.0, 1.0);
    let claro = [219.0, 234.0, 254.0];
    let oscuro = [147.0, 197.0, 253.0];
    let [rojo, verde, azul] = std::array::from_fn(|indice| {
        (claro[indice] + (oscuro[indice] - claro[indice]) * proporcion).round() as u32
    });
    Some((rojo << 16) | (verde << 8) | azul)
}

/// Prepara una matriz pivotada. Las dimensiones deben ocupar las primeras columnas de `matrix`.
/// Los subtotales y el total general se guardan aparte de las celdas filtrables.
pub fn preparar_matriz_pivote(matrix: &Matrix, dimension_count: usize) -> MatrizPivote {
    let row_dimension_count = match dimension_count {
        0 => 0,
        1 => 1,
        count => count - 1,
    };
    preparar_matriz_pivote_con_filas(matrix, dimension_count, row_dimension_count)
}

/// Prepara una matriz con una cantidad explicita de dimensiones de fila. Las demas forman
/// la tupla del eje de columnas. Las dimensiones de fila deben ocupar el prefijo.
pub fn preparar_matriz_pivote_con_filas(
    matrix: &Matrix,
    dimension_count: usize,
    row_dimension_count: usize,
) -> MatrizPivote {
    let measure_count = matrix.headers.len().saturating_sub(dimension_count);
    if dimension_count == 0
        || dimension_count > matrix.headers.len()
        || measure_count == 0
        || row_dimension_count == 0
        || row_dimension_count > dimension_count
    {
        return MatrizPivote::default();
    }

    let measure_start = dimension_count;
    let mut result = MatrizPivote {
        total_general: vec![None; measure_count],
        ..MatrizPivote::default()
    };
    let mut row_indices = BTreeMap::<Vec<String>, usize>::new();
    let mut column_indices = BTreeMap::<(Vec<String>, usize), usize>::new();

    for (row_index, row) in matrix.rows.iter().enumerate() {
        let kind = matrix.row_kind(row_index);
        if row.len() < dimension_count + measure_count {
            continue;
        }

        if kind == RowKind::GrandTotal {
            for measure_index in 0..measure_count {
                result.total_general[measure_index] =
                    Some(row[measure_start + measure_index].clone());
            }
            continue;
        }

        if let RowKind::Subtotal { level } = kind {
            if level.checked_add(1) != Some(row_dimension_count) {
                continue;
            }

            let row_key = row[..row_dimension_count].to_vec();
            let output_row_index = *row_indices.entry(row_key.clone()).or_insert_with(|| {
                let index = result.filas.len();
                result.filas.push(FilaMatriz {
                    row_key,
                    celdas: vec![None; result.columnas.len()],
                    totales: vec![None; measure_count],
                });
                index
            });
            for measure_index in 0..measure_count {
                result.filas[output_row_index].totales[measure_index] =
                    Some(row[measure_start + measure_index].clone());
            }
            continue;
        }

        if kind != RowKind::Data {
            continue;
        }

        let filter_key = row[..dimension_count].to_vec();
        let row_key = row[..row_dimension_count].to_vec();
        let categories = row[row_dimension_count..dimension_count].to_vec();
        let output_row_index = *row_indices.entry(row_key.clone()).or_insert_with(|| {
            let index = result.filas.len();
            result.filas.push(FilaMatriz {
                row_key,
                celdas: vec![None; result.columnas.len()],
                totales: vec![None; measure_count],
            });
            index
        });

        for measure_index in 0..measure_count {
            let column_key = (categories.clone(), measure_index);
            let output_column_index =
                *column_indices.entry(column_key.clone()).or_insert_with(|| {
                    let index = result.columnas.len();
                    result.columnas.push(ColumnaMatriz {
                        categorias: column_key.0,
                        medida: matrix.headers[measure_start + measure_index].label.clone(),
                        indice_medida: measure_index,
                    });
                    for output_row in &mut result.filas {
                        output_row.celdas.push(None);
                    }
                    index
                });

            result.filas[output_row_index].celdas[output_column_index] = Some(CeldaMatriz {
                filter_key: filter_key.clone(),
                valor: row[measure_start + measure_index].clone(),
            });
        }
    }

    result
}

/// Alinea los agregados por tupla con las columnas visibles del pivote.
/// La consulta de totales tiene una columna por cada dimension de columna y cada medida.
pub fn alinear_subtotales_columna(pivot: &MatrizPivote, totals: &Matrix) -> Vec<Option<String>> {
    pivot
        .columnas
        .iter()
        .map(|column| {
            if column.categorias.is_empty() {
                return None;
            }
            totals
                .rows
                .iter()
                .enumerate()
                .find(|(index, row)| {
                    totals.row_kind(*index) == RowKind::Data
                        && row.get(..column.categorias.len()) == Some(column.categorias.as_slice())
                })
                .and_then(|(_, row)| {
                    row.get(column.categorias.len() + column.indice_medida)
                        .cloned()
                })
        })
        .collect()
}

/// Return only detail rows, with their source index, for chart data extraction.
/// Keep the index so callers can match a row with its source category and metadata.
pub fn filas_de_datos(matrix: &Matrix) -> impl Iterator<Item = (usize, &[String])> {
    matrix
        .rows
        .iter()
        .enumerate()
        .filter(|(index, _)| matrix.row_kind(*index) == datan_report::RowKind::Data)
        .map(|(index, row)| (index, row.as_slice()))
}

pub const TOPE_SERIES_LEYENDA: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub struct SerieAlineada {
    pub etiqueta: String,
    pub valores: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatosSeries {
    pub categorias: Vec<Vec<String>>,
    pub series: Vec<SerieAlineada>,
    pub series_omitidas: usize,
}

/// Alinea una medida por categoría para cada valor distinto de la dimensión final.
/// Las combinaciones ausentes conservan `None` para que las líneas no inventen valores.
pub fn series_por_leyenda(matrix: &Matrix, dimension_count: usize) -> DatosSeries {
    if dimension_count < 2 || dimension_count >= matrix.headers.len() {
        return DatosSeries::default();
    }

    let category_count = dimension_count - 1;
    let mut categories = Vec::<Vec<String>>::new();
    let mut category_indices = BTreeMap::<Vec<String>, usize>::new();
    let mut all_series = Vec::<String>::new();
    let mut values = BTreeMap::<(Vec<String>, String), f64>::new();

    for (_, row) in filas_de_datos(matrix) {
        let Some(category) = row.get(..category_count).map(<[String]>::to_vec) else {
            continue;
        };
        let Some(label) = row.get(category_count) else {
            continue;
        };
        let Some(value) = row
            .get(dimension_count)
            .and_then(|value| value.parse::<f64>().ok())
        else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }
        if !category_indices.contains_key(&category) {
            category_indices.insert(category.clone(), categories.len());
            categories.push(category.clone());
        }
        if !all_series.contains(label) {
            all_series.push(label.clone());
        }
        values.insert((category, label.clone()), value);
    }

    let series_omitidas = all_series.len().saturating_sub(TOPE_SERIES_LEYENDA);
    let series = all_series
        .into_iter()
        .take(TOPE_SERIES_LEYENDA)
        .map(|label| SerieAlineada {
            valores: categories
                .iter()
                .map(|category| values.get(&(category.clone(), label.clone())).copied())
                .collect(),
            etiqueta: label,
        })
        .collect();

    DatosSeries {
        categorias: categories,
        series,
        series_omitidas,
    }
}

/// Horizontal bar scale that uses zero as its origin and supports signed values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EscalaBarras {
    minimo: f64,
    maximo: f64,
    unidad: f64,
}

impl EscalaBarras {
    pub fn desde_valores(values: impl IntoIterator<Item = f64>) -> Self {
        let (min_value, max_value) = values
            .into_iter()
            .filter(|value| value.is_finite())
            .fold(None, |bounds: Option<(f64, f64)>, value| {
                Some(match bounds {
                    Some((min_value, max_value)) => (min_value.min(value), max_value.max(value)),
                    None => (value, value),
                })
            })
            .unwrap_or((0.0, 1.0));
        let minimo = min_value.min(0.0);
        let maximo = max_value.max(0.0);
        let unidad = minimo.abs().max(maximo.abs()).max(f64::MIN_POSITIVE);
        let escala = Self {
            minimo,
            maximo,
            unidad,
        };
        if escala.rango_normalizado() == 0.0 {
            Self {
                minimo: 0.0,
                maximo: 1.0,
                unidad: 1.0,
            }
        } else {
            escala
        }
    }

    pub fn minimo(self) -> f64 {
        self.minimo
    }

    pub fn maximo(self) -> f64 {
        self.maximo
    }

    /// Position of zero in the full bar track, from 0.0 to 1.0.
    pub fn origen(self) -> f64 {
        self.posicion(0.0)
    }

    /// Position of a value in the full bar track, from 0.0 to 1.0.
    pub fn posicion(self, value: f64) -> f64 {
        let span = self.rango_normalizado();
        if !value.is_finite() || span <= 0.0 {
            return self.origen();
        }
        ((value / self.unidad - self.minimo / self.unidad) / span).clamp(0.0, 1.0)
    }

    /// Width of a bar in the full track, from zero to the value.
    pub fn ancho(self, value: f64) -> f64 {
        let span = self.rango_normalizado();
        if !value.is_finite() || span <= 0.0 {
            return 0.0;
        }
        ((value / self.unidad).abs() / span).clamp(0.0, 1.0)
    }

    fn rango_normalizado(self) -> f64 {
        self.maximo / self.unidad - self.minimo / self.unidad
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Punto {
    pub etiqueta: String,
    pub valor: f64,
    pub formato: Option<Formato>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vela {
    pub etiqueta: String,
    pub clave_categoria: Vec<String>,
    pub apertura: f64,
    pub maximo: f64,
    pub minimo: f64,
    pub cierre: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodoFlujo {
    pub fase: usize,
    pub etiqueta: String,
}

/// Lee los cuatro valores OHLC de cada fila de una matriz.
pub fn velas_de_matriz(matrix: &Matrix, dimensiones: usize) -> Vec<Vela> {
    matrix
        .rows
        .iter()
        .enumerate()
        .filter(|(indice, _)| matrix.row_kind(*indice) == RowKind::Data)
        .filter_map(|(_, row)| {
            let numero = |indice: usize| {
                let valor = row.get(indice)?.parse::<f64>().ok()?;
                valor.is_finite().then_some(valor)
            };
            let apertura = numero(dimensiones)?;
            let maximo = numero(dimensiones + 1)?;
            let minimo = numero(dimensiones + 2)?;
            let cierre = numero(dimensiones + 3)?;
            Some(Vela {
                etiqueta: row.first()?.clone(),
                clave_categoria: row.get(..dimensiones)?.to_vec(),
                apertura,
                maximo: maximo.max(apertura).max(cierre),
                minimo: minimo.min(apertura).min(cierre),
                cierre,
            })
        })
        .collect()
}

/// Lee los enlaces de las dos primeras dimensiones y la primera medida.
pub fn flujo_de_matriz(matrix: &Matrix, dimensiones: usize) -> (Vec<NodoFlujo>, Vec<SankeyLink>) {
    let mut indices = BTreeMap::<(usize, String), usize>::new();
    let mut nodos = Vec::new();
    let mut enlaces = Vec::new();
    for (indice, row) in matrix.rows.iter().enumerate() {
        if matrix.row_kind(indice) != RowKind::Data {
            continue;
        }
        let (Some(origen), Some(destino), Some(valor)) = (
            row.first(),
            row.get(1),
            row.get(dimensiones)
                .and_then(|value| value.parse::<f64>().ok()),
        ) else {
            continue;
        };
        if !valor.is_finite() || valor <= 0.0 {
            continue;
        }
        let resolve = |fase: usize,
                       etiqueta: &str,
                       indices: &mut BTreeMap<(usize, String), usize>,
                       nodos: &mut Vec<NodoFlujo>| {
            let key = (fase, etiqueta.to_string());
            *indices.entry(key.clone()).or_insert_with(|| {
                let index = nodos.len();
                nodos.push(NodoFlujo {
                    fase,
                    etiqueta: key.1,
                });
                index
            })
        };
        let desde = resolve(0, origen, &mut indices, &mut nodos);
        let hasta = resolve(1, destino, &mut indices, &mut nodos);
        enlaces.push(SankeyLink::new(desde, hasta, valor));
    }
    (nodos, enlaces)
}

impl Punto {
    pub fn new(etiqueta: impl Into<String>, valor: f64) -> Self {
        Self {
            etiqueta: etiqueta.into(),
            valor,
            formato: None,
        }
    }

    pub fn con_formato(mut self, formato: Option<Formato>) -> Self {
        self.formato = formato;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fraccion {
    pub valor: f64,
    pub proporcion: f32,
}

/// Escala los valores de un embudo al ancho máximo disponible.
pub fn datos_embudo(puntos: &[Punto]) -> Vec<Fraccion> {
    let maximo = puntos
        .iter()
        .map(|p| p.valor.max(0.0))
        .fold(0.0_f64, f64::max);
    puntos
        .iter()
        .map(|p| Fraccion {
            valor: p.valor,
            proporcion: if maximo == 0.0 {
                0.0
            } else {
                (p.valor.max(0.0) / maximo) as f32
            },
        })
        .collect()
}

/// Devuelve el avance del medidor en el rango de cero a uno.
pub fn avance_medidor(valor: f64, minimo: f64, maximo: f64) -> f32 {
    if !valor.is_finite() || !minimo.is_finite() || !maximo.is_finite() || maximo <= minimo {
        return 0.0;
    }
    ((valor - minimo) / (maximo - minimo)).clamp(0.0, 1.0) as f32
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rectangulo {
    pub x: f32,
    pub y: f32,
    pub ancho: f32,
    pub alto: f32,
}

/// Divide el lienzo en rectángulos alternos. Cada área representa el valor positivo.
pub fn datos_treemap(puntos: &[Punto], ancho: f32, alto: f32) -> Vec<Rectangulo> {
    let total: f64 = puntos.iter().map(|p| p.valor.max(0.0)).sum();
    if puntos.is_empty() || total <= 0.0 || ancho <= 0.0 || alto <= 0.0 {
        return Vec::new();
    }

    let mut resultado = Vec::with_capacity(puntos.len());
    let (mut x, mut y, mut restante_ancho, mut restante_alto) = (0.0, 0.0, ancho, alto);
    let mut restante_peso = total;

    for (indice, punto) in puntos.iter().enumerate() {
        let peso = punto.valor.max(0.0);
        let proporcion = if restante_peso == 0.0 {
            0.0
        } else {
            (peso / restante_peso) as f32
        };
        let rect = if indice % 2 == 0 {
            let rect_ancho = restante_ancho * proporcion;
            let rect = Rectangulo {
                x,
                y,
                ancho: rect_ancho,
                alto: restante_alto,
            };
            x += rect_ancho;
            restante_ancho = (restante_ancho - rect_ancho).max(0.0);
            rect
        } else {
            let rect_alto = restante_alto * proporcion;
            let rect = Rectangulo {
                x,
                y,
                ancho: restante_ancho,
                alto: rect_alto,
            };
            y += rect_alto;
            restante_alto = (restante_alto - rect_alto).max(0.0);
            rect
        };
        resultado.push(rect);
        restante_peso -= peso;
    }
    resultado
}

#[derive(Debug, Clone, PartialEq)]
pub struct TramoCascada {
    pub etiqueta: String,
    pub inicio: f64,
    pub fin: f64,
    pub formato: Option<Formato>,
}

/// Calcula la base acumulada de cada barra de cascada.
pub fn datos_cascada(inicial: f64, cambios: &[Punto]) -> Vec<TramoCascada> {
    let mut acumulado = inicial;
    cambios
        .iter()
        .map(|punto| {
            let inicio = acumulado;
            acumulado += punto.valor;
            TramoCascada {
                etiqueta: punto.etiqueta.clone(),
                inicio,
                fin: acumulado,
                formato: punto.formato.clone(),
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstadoKpi {
    EnObjetivo,
    BajoObjetivo,
    SobreObjetivo,
    SinObjetivo,
}

pub fn estado_kpi(valor: f64, objetivo: Option<f64>, menor_es_mejor: bool) -> EstadoKpi {
    let Some(objetivo) = objetivo else {
        return EstadoKpi::SinObjetivo;
    };
    match (valor.total_cmp(&objetivo), menor_es_mejor) {
        (std::cmp::Ordering::Equal, _) => EstadoKpi::EnObjetivo,
        (std::cmp::Ordering::Less, false) | (std::cmp::Ordering::Greater, true) => {
            EstadoKpi::BajoObjetivo
        }
        _ => EstadoKpi::SobreObjetivo,
    }
}

/// Dibuja el embudo y conserva el orden recibido.
pub fn embudo(puntos: &[Punto]) -> AnyElement {
    embudo_zoom(puntos, 1.0)
}

pub fn embudo_zoom(puntos: &[Punto], zoom: f32) -> AnyElement {
    embudo_zoom_con_id(puntos, zoom, "embudo")
}

pub fn embudo_zoom_con_id(puntos: &[Punto], zoom: f32, visual_id: &str) -> AnyElement {
    embudo_zoom_con_id_interactivo(puntos, zoom, visual_id, Vec::new())
}

pub fn embudo_zoom_con_id_interactivo(
    puntos: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
) -> AnyElement {
    embudo_zoom_con_id_interactivo_y_escala(puntos, zoom, visual_id, handlers, 1.0)
}

pub fn embudo_zoom_con_id_interactivo_y_escala(
    puntos: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
    escala_texto: f32,
) -> AnyElement {
    let datos = datos_embudo(puntos);
    let mut handlers = handlers.into_iter();
    let mut filas = div()
        .flex()
        .flex_col()
        .gap(px_lienzo(8.0, zoom))
        .w(px_lienzo((ANCHO_GRAFICO), zoom));
    for (indice, (punto, fraccion)) in puntos.iter().zip(datos).enumerate() {
        let tooltip = tooltip_embudo(punto);
        let fila = div()
            .id(gpui::ElementId::Name(gpui_kit::SharedString::from(
                format!("embudo-{visual_id}-stage-{indice}"),
            )))
            .flex()
            .flex_col()
            .gap(px_lienzo(4.0, zoom))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px_lienzo(12.0 * escala_texto.clamp(0.7, 1.6), zoom))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .truncate()
                            .child(punto.etiqueta.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .child(texto_valor(punto.valor, punto.formato.as_ref())),
                    ),
            )
            .child(
                div()
                    .w(px_lienzo((ANCHO_GRAFICO), zoom))
                    .h(px_lienzo((12.0), zoom))
                    .rounded_lg()
                    .bg(rgb(GRIS))
                    .child(
                        div()
                            .h(px_lienzo((12.0), zoom))
                            .w(px_lienzo((ANCHO_GRAFICO * fraccion.proporcion), zoom))
                            .rounded_lg()
                            .bg(rgb(AZUL)),
                    ),
            );
        let fila = tooltip_element::add_stateful(fila, tooltip);
        let fila = match handlers.next() {
            Some(handler) => fila.cursor_pointer().on_click(handler),
            None => fila,
        };
        filas = filas.child(fila);
    }
    filas.into_any_element()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Medidor {
    pub id: String,
    pub etiqueta: String,
    pub valor: f64,
    pub minimo: f64,
    pub maximo: f64,
    pub objetivo: Option<f64>,
    pub formato: Option<Formato>,
}

/// Dibuja un medidor horizontal con su rango y valor.
pub fn medidor(medidor: &Medidor) -> AnyElement {
    medidor_zoom(medidor, 1.0)
}

pub fn medidor_zoom(medidor: &Medidor, zoom: f32) -> AnyElement {
    medidor_zoom_con_escala(medidor, zoom, 1.0)
}

pub fn medidor_zoom_con_escala(medidor: &Medidor, zoom: f32, escala_texto: f32) -> AnyElement {
    let avance = avance_medidor(medidor.valor, medidor.minimo, medidor.maximo);
    let objetivo = medidor
        .objetivo
        .map(|value| avance_medidor(value, medidor.minimo, medidor.maximo));
    let tooltip = tooltip_medidor(medidor);
    let contenedor = div().id(gpui::ElementId::Name(gpui_kit::SharedString::from(
        format!("medidor-{}", medidor.id),
    )));
    tooltip_element::add_stateful(contenedor, tooltip)
        .w(px_lienzo((ANCHO_GRAFICO), zoom))
        .flex()
        .flex_col()
        .gap(px_lienzo(8.0, zoom))
        .child(
            div()
                .overflow_hidden()
                .truncate()
                .text_size(px_lienzo(13.0 * escala_texto.clamp(0.7, 1.6), zoom))
                .child(medidor.etiqueta.clone()),
        )
        .child(
            div()
                .w(px_lienzo((ANCHO_GRAFICO), zoom))
                .h(px_lienzo((18.0), zoom))
                .relative()
                .overflow_hidden()
                .rounded_lg()
                .bg(rgb(GRIS))
                .child(
                    div()
                        .h(px_lienzo((18.0), zoom))
                        .w(px_lienzo((ANCHO_GRAFICO * avance), zoom))
                        .rounded_lg()
                        .bg(rgb(AZUL)),
                )
                .children(objetivo.map(|avance| {
                    div()
                        .absolute()
                        .left(px_lienzo(ANCHO_GRAFICO * avance, zoom))
                        .top(px_lienzo(0.0, zoom))
                        .w(px_lienzo(2.0, zoom))
                        .h(px_lienzo(18.0, zoom))
                        .bg(rgb(0x111827))
                })),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .text_size(px_lienzo(11.0 * escala_texto.clamp(0.7, 1.6), zoom))
                .child(texto_valor(medidor.minimo, medidor.formato.as_ref()))
                .child(texto_valor(medidor.valor, medidor.formato.as_ref()))
                .child(texto_valor(medidor.maximo, medidor.formato.as_ref())),
        )
        .children(medidor.objetivo.map(|objetivo| {
            div()
                .text_size(px_lienzo(11.0 * escala_texto.clamp(0.7, 1.6), zoom))
                .text_color(rgb(0x6b7280))
                .child(format!(
                    "Objetivo: {}",
                    texto_valor(objetivo, medidor.formato.as_ref())
                ))
        }))
        .into_any_element()
}

fn tooltip_embudo(punto: &Punto) -> String {
    format!(
        "{} · {}",
        punto.etiqueta,
        texto_valor(punto.valor, punto.formato.as_ref())
    )
}

fn tooltip_medidor(medidor: &Medidor) -> String {
    let base = format!(
        "{} · Valor: {} · Mínimo: {} · Máximo: {}",
        medidor.etiqueta,
        texto_valor(medidor.valor, medidor.formato.as_ref()),
        texto_valor(medidor.minimo, medidor.formato.as_ref()),
        texto_valor(medidor.maximo, medidor.formato.as_ref()),
    );
    medidor.objetivo.map_or(base.clone(), |objetivo| {
        format!(
            "{base} · Objetivo: {}",
            texto_valor(objetivo, medidor.formato.as_ref())
        )
    })
}

/// Dibuja una partición de área para un treemap.
pub fn treemap(puntos: &[Punto]) -> AnyElement {
    treemap_zoom(puntos, 1.0)
}

pub fn treemap_zoom(puntos: &[Punto], zoom: f32) -> AnyElement {
    treemap_zoom_con_id(puntos, zoom, "treemap")
}

pub fn treemap_zoom_con_id(puntos: &[Punto], zoom: f32, visual_id: &str) -> AnyElement {
    treemap_zoom_con_id_interactivo(puntos, zoom, visual_id, Vec::new())
}

pub fn treemap_zoom_con_id_interactivo(
    puntos: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
) -> AnyElement {
    treemap_zoom_con_id_interactivo_y_escala(puntos, zoom, visual_id, handlers, 1.0)
}

pub fn treemap_zoom_con_id_interactivo_y_escala(
    puntos: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
    escala_texto: f32,
) -> AnyElement {
    treemap_zoom_con_id_interactivo_y_escala_con_color(
        puntos,
        zoom,
        visual_id,
        handlers,
        escala_texto,
        0,
    )
}

pub fn treemap_zoom_con_id_interactivo_y_escala_con_color(
    puntos: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
    escala_texto: f32,
    color_base: u32,
) -> AnyElement {
    treemap_zoom_con_id_interactivo_y_escala_con_colores(
        puntos,
        zoom,
        visual_id,
        handlers,
        escala_texto,
        color_base,
        0,
    )
}

pub fn treemap_zoom_con_id_interactivo_y_escala_con_colores(
    puntos: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
    escala_texto: f32,
    color_base: u32,
    color_texto: u32,
) -> AnyElement {
    let rectangulos = datos_treemap(puntos, ANCHO_GRAFICO, ALTO_GRAFICO);
    let mut handlers = handlers.into_iter();
    let mut mosaico = div()
        .w(px_lienzo((ANCHO_GRAFICO), zoom))
        .h(px_lienzo((ALTO_GRAFICO), zoom))
        .relative();
    for (indice, (punto, rect)) in puntos.iter().zip(rectangulos).enumerate() {
        let tono = color_treemap(indice, color_base);
        let tooltip = tooltip_treemap(punto);
        let bloque = div()
            .id(gpui::ElementId::Name(gpui_kit::SharedString::from(
                format!("treemap-{visual_id}-tile-{indice}"),
            )))
            .absolute()
            .left(px_lienzo(rect.x, zoom))
            .top(px_lienzo(rect.y, zoom))
            .w(px_lienzo(rect.ancho.max(2.0), zoom))
            .h(px_lienzo(rect.alto.max(2.0), zoom))
            .p(px_lienzo(4.0, zoom))
            .bg(rgb(tono))
            .text_color(rgb(if color_texto == 0 {
                color_texto_treemap(tono)
            } else {
                color_texto & 0x00ff_ffff
            }))
            .text_size(px_lienzo(11.0 * escala_texto.clamp(0.7, 1.6), zoom))
            .overflow_hidden()
            .truncate()
            .child(tooltip_treemap(punto));
        let bloque = tooltip_element::add_stateful(bloque, tooltip);
        let bloque = match handlers.next() {
            Some(handler) => bloque.cursor_pointer().on_click(handler),
            None => bloque,
        };
        mosaico = mosaico.child(bloque);
    }
    mosaico.into_any_element()
}

fn color_treemap(indice: usize, color_base: u32) -> u32 {
    if color_base == 0 {
        return if indice % 2 == 0 { AZUL } else { 0x0F766E };
    }

    let color = color_base & 0x00ff_ffff;
    let (rojo, verde, azul) = (
        (color >> 16) & 0xff,
        (color >> 8) & 0xff,
        color & 0xff,
    );
    let (factor_numerador, factor_denominador) = if indice % 2 == 0 {
        (9, 10)
    } else {
        (11, 10)
    };
    let ajustar = |canal: u32| {
        if factor_numerador <= factor_denominador {
            canal * factor_numerador / factor_denominador
        } else {
            canal + ((255 - canal) * (factor_numerador - factor_denominador)
                / factor_denominador)
        }
    };
    (ajustar(rojo) << 16) | (ajustar(verde) << 8) | ajustar(azul)
}

fn color_texto_treemap(color: u32) -> u32 {
    let luminancia = [16, 8, 0].into_iter().fold(0.0, |total, shift| {
        let canal = ((color >> shift) & 0xff) as f64 / 255.0;
        let lineal = if canal <= 0.04045 {
            canal / 12.92
        } else {
            ((canal + 0.055) / 1.055).powf(2.4)
        };
        total
            + lineal
                * match shift {
                    16 => 0.2126,
                    8 => 0.7152,
                    _ => 0.0722,
                }
    });
    let contraste_negro = (luminancia + 0.05) / 0.05;
    let contraste_blanco = 1.05 / (luminancia + 0.05);
    if contraste_negro >= contraste_blanco {
        0x000000
    } else {
        0xFFFFFF
    }
}

fn tooltip_treemap(punto: &Punto) -> String {
    format!(
        "{} · {}",
        punto.etiqueta,
        texto_valor(punto.valor, punto.formato.as_ref())
    )
}

/// Dibuja los cambios de una cascada como barras centradas en su base acumulada.
pub fn cascada(inicial: f64, cambios: &[Punto]) -> AnyElement {
    cascada_zoom(inicial, cambios, 1.0)
}

pub fn cascada_zoom(inicial: f64, cambios: &[Punto], zoom: f32) -> AnyElement {
    cascada_zoom_con_id(inicial, cambios, zoom, "cascada")
}

pub fn cascada_zoom_con_id(
    inicial: f64,
    cambios: &[Punto],
    zoom: f32,
    visual_id: &str,
) -> AnyElement {
    cascada_zoom_con_id_interactivo(inicial, cambios, zoom, visual_id, Vec::new())
}

pub fn cascada_zoom_con_id_interactivo(
    inicial: f64,
    cambios: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
) -> AnyElement {
    cascada_zoom_con_id_interactivo_y_escala(inicial, cambios, zoom, visual_id, handlers, 1.0)
}

pub fn cascada_zoom_con_id_interactivo_y_escala(
    inicial: f64,
    cambios: &[Punto],
    zoom: f32,
    visual_id: &str,
    handlers: Vec<CategoryClick>,
    escala_texto: f32,
) -> AnyElement {
    let tramos = datos_cascada(inicial, cambios);
    let mut handlers = handlers.into_iter();
    let minimo = tramos
        .iter()
        .flat_map(|t| [t.inicio, t.fin])
        .fold(inicial, f64::min)
        .min(0.0);
    let maximo = tramos
        .iter()
        .flat_map(|t| [t.inicio, t.fin])
        .fold(inicial, f64::max)
        .max(0.0);
    let rango = (maximo - minimo).max(f64::EPSILON);
    let mut filas = div()
        .w(px_lienzo((ANCHO_GRAFICO), zoom))
        .flex()
        .flex_col()
        .gap(px_lienzo(8.0, zoom));
    for (indice, tramo) in tramos.into_iter().enumerate() {
        let tooltip = tooltip_cascada(&tramo);
        let positivo = tramo.fin >= tramo.inicio;
        let (desde, hasta) = if positivo {
            (tramo.inicio, tramo.fin)
        } else {
            (tramo.fin, tramo.inicio)
        };
        let desplazamiento = ((desde - minimo) / rango) as f32 * ANCHO_GRAFICO;
        let ancho = (((hasta - desde) / rango) as f32 * ANCHO_GRAFICO).max(2.0);
        let fila = div()
            .id(gpui::ElementId::Name(gpui_kit::SharedString::from(
                format!("cascada-{visual_id}-stage-{indice}"),
            )))
            .flex()
            .items_center()
            .gap(px_lienzo(8.0, zoom))
            .child(
                div()
                    .w(px_lienzo((88.0), zoom))
                    .overflow_hidden()
                    .truncate()
                    .text_size(px_lienzo(11.0 * escala_texto.clamp(0.7, 1.6), zoom))
                    .child(tramo.etiqueta),
            )
            .child(
                div()
                    .w(px_lienzo((ANCHO_GRAFICO - 88.0), zoom))
                    .h(px_lienzo((16.0), zoom))
                    .bg(rgb(GRIS))
                    .child(
                        div()
                            .ml(px_lienzo(
                                (desplazamiento * (ANCHO_GRAFICO - 88.0) / ANCHO_GRAFICO),
                                zoom,
                            ))
                            .w(px_lienzo((ancho), zoom))
                            .h(px_lienzo((16.0), zoom))
                            .bg(rgb(if positivo { VERDE } else { ROJO })),
                    ),
            );
        let fila = tooltip_element::add_stateful(fila, tooltip);
        let fila = match handlers.next() {
            Some(handler) => fila.cursor_pointer().on_click(handler),
            None => fila,
        };
        filas = filas.child(fila);
    }
    filas.into_any_element()
}

fn tooltip_cascada(tramo: &TramoCascada) -> String {
    format!(
        "{} · Cambio: {} · Acumulado: {}",
        tramo.etiqueta,
        texto_valor(tramo.fin - tramo.inicio, tramo.formato.as_ref()),
        texto_valor(tramo.fin, tramo.formato.as_ref()),
    )
}

fn texto_valor(valor: f64, formato: Option<&Formato>) -> String {
    formato.map_or_else(|| format_numero(valor), |formato| formato.aplicar(valor))
}

#[derive(Debug, Clone, PartialEq)]
pub struct TarjetaKpi {
    pub etiqueta: String,
    pub valor: f64,
    pub objetivo: Option<(String, f64)>,
    pub menor_es_mejor: bool,
    pub color_valor: Option<u32>,
    pub escala_texto: f32,
}

/// Dibuja un KPI y solo muestra el estado si tiene una medida objetivo.
pub fn tarjeta_kpi(kpi: &TarjetaKpi) -> AnyElement {
    tarjeta_kpi_zoom(kpi, 1.0)
}

pub fn tarjeta_kpi_zoom(kpi: &TarjetaKpi, zoom: f32) -> AnyElement {
    let escala = (kpi.escala_texto * 100.0).round().clamp(70.0, 160.0) as u8;
    tarjeta_kpi_zoom_con_formato(
        kpi,
        datan_report::saved::SavedFormato {
            escala_texto: escala,
            ..datan_report::saved::SavedFormato::default()
        },
        None,
        zoom,
    )
}

pub fn tarjeta_kpi_zoom_con_formato(
    kpi: &TarjetaKpi,
    formato: datan_report::saved::SavedFormato,
    mono_font_family: Option<SharedString>,
    zoom: f32,
) -> AnyElement {
    tarjeta_kpi_con_formato(kpi, formato, mono_font_family, zoom, true)
}

pub fn contenido_kpi_zoom_con_formato(
    kpi: &TarjetaKpi,
    formato: datan_report::saved::SavedFormato,
    mono_font_family: Option<SharedString>,
    zoom: f32,
) -> AnyElement {
    tarjeta_kpi_con_formato(kpi, formato, mono_font_family, zoom, false)
}

fn tarjeta_kpi_con_formato(
    kpi: &TarjetaKpi,
    formato: datan_report::saved::SavedFormato,
    mono_font_family: Option<SharedString>,
    zoom: f32,
    con_marco: bool,
) -> AnyElement {
    let scale = f32::from(formato.escala_texto.clamp(70, 160)) / 100.0;
    let title_scale = scale * f32::from(formato.escala_titulo.clamp(70, 160)) / 100.0;
    let value_scale = scale * f32::from(formato.escala_valor.clamp(70, 160)) / 100.0;
    let foreground = (formato.color_texto != 0).then_some(formato.color_texto);
    let title_foreground = (formato.color_titulo != 0).then_some(formato.color_titulo);
    let value_foreground = (formato.color_valor != 0).then_some(formato.color_valor);
    let background = (formato.color_fondo != 0).then_some(formato.color_fondo);
    let favorable = if formato.color_estado_favorable == 0 {
        VERDE
    } else {
        formato.color_estado_favorable
    };
    let unfavorable = if formato.color_estado_desfavorable == 0 {
        ROJO
    } else {
        formato.color_estado_desfavorable
    };
    let estado = estado_kpi(
        kpi.valor,
        kpi.objetivo.as_ref().map(|(_, objetivo)| *objetivo),
        kpi.menor_es_mejor,
    );
    let indicador: AnyElement = match (estado, kpi.objetivo.as_ref()) {
        (EstadoKpi::EnObjetivo, Some((nombre, objetivo))) => div()
            .flex()
            .justify_between()
            .text_size(px_lienzo((11.0), zoom))
            .child(format!("{nombre}: {}", format_numero(*objetivo)))
            .child(
                div()
                    .text_color(rgb(favorable & 0x00ff_ffff))
                    .child("En objetivo"),
            )
            .into_any_element(),
        (EstadoKpi::BajoObjetivo, Some((nombre, objetivo))) => div()
            .flex()
            .justify_between()
            .text_size(px_lienzo((11.0), zoom))
            .child(format!("{nombre}: {}", format_numero(*objetivo)))
            .child(
                div()
                    .text_color(rgb((if kpi.menor_es_mejor {
                        favorable
                    } else {
                        unfavorable
                    }) & 0x00ff_ffff))
                    .child("Bajo objetivo"),
            )
            .into_any_element(),
        (EstadoKpi::SobreObjetivo, Some((nombre, objetivo))) => div()
            .flex()
            .justify_between()
            .text_size(px_lienzo((11.0), zoom))
            .child(format!("{nombre}: {}", format_numero(*objetivo)))
            .child(
                div()
                    .text_color(rgb((if kpi.menor_es_mejor {
                        unfavorable
                    } else {
                        favorable
                    }) & 0x00ff_ffff))
                    .child("Sobre objetivo"),
            )
            .into_any_element(),
        // Sin objetivo no hay texto de apoyo: el KPI muestra el valor y listo. El objetivo
        // se asigna desde Propiedades, y la tarjeta no explica lo que no muestra.
        _ => div().into_any_element(),
    };
    let mut tarjeta = div().flex().flex_col().gap(px_lienzo(8.0, zoom));
    tarjeta = if con_marco {
        tarjeta
            .w(px_lienzo(ANCHO_GRAFICO, zoom))
            .p(px_lienzo(12.0, zoom))
    } else {
        tarjeta.w_full().flex_1().justify_center()
    };
    if con_marco && formato.titulo {
        tarjeta = tarjeta.child(
            div()
                .text_size(px_lienzo(12.0 * title_scale, zoom))
                .when_some(title_foreground, |label, color| {
                    label.text_color(rgb(color & 0x00ff_ffff))
                })
                .child(kpi.etiqueta.clone()),
        );
    }
    let mut value = div()
        .text_size(px_lienzo(26.0 * value_scale, zoom))
        .font_semibold();
    if let Some(color) = kpi.color_valor {
        value = value.text_color(rgb(color & 0x00ff_ffff));
    } else if let Some(color) = value_foreground {
        value = value.text_color(rgb(color & 0x00ff_ffff));
    } else if let Some(color) = foreground {
        value = value.text_color(rgb(color & 0x00ff_ffff));
    }
    tarjeta = tarjeta
        .child(value.child(format_numero(kpi.valor)))
        .child(indicador);
    if let Some(color) = foreground {
        tarjeta = tarjeta.text_color(rgb(color & 0x00ff_ffff));
    }
    if let Some(font_family) = mono_font_family {
        tarjeta = tarjeta.font_family(font_family);
    }
    if con_marco
        && formato.fondo
        && let Some(color) = background
    {
        tarjeta = tarjeta.bg(rgb(color & 0x00ff_ffff));
    }
    if con_marco && formato.ancho_borde > 0 {
        tarjeta = tarjeta.border_1();
        if formato.ancho_borde > 1 {
            tarjeta = tarjeta.border_2();
        }
        if formato.color_borde != 0 {
            tarjeta = tarjeta.border_color(rgb(formato.color_borde & 0x00ff_ffff));
        }
    }
    if con_marco {
        tarjeta = match formato.radio_borde {
            0..=8 => tarjeta.rounded_md(),
            9..=12 => tarjeta.rounded_lg(),
            _ => tarjeta.rounded_2xl(),
        };
    }
    tarjeta.into_any_element()
}

fn format_numero(valor: f64) -> String {
    if valor.fract() == 0.0 {
        format!("{valor:.0}")
    } else {
        format!("{valor:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datan_model::{DataType, ObjectId};

    #[test]
    fn treemap_usa_el_color_personalizado_y_mantiene_variacion_por_categoria() {
        let custom = [color_treemap(0, 0x2457d6), color_treemap(1, 0x2457d6)];

        assert_ne!(custom[0], custom[1]);
        assert_ne!(custom[0], AZUL);
        assert_ne!(custom[1], 0x0F766E);
        assert_eq!(color_treemap(0, 0), AZUL);
        assert_eq!(color_treemap(1, 0), 0x0F766E);
        assert_eq!(color_texto_treemap(0xffff00), 0x000000);
        assert_eq!(color_texto_treemap(0x2457d6), 0xffffff);
    }

    #[test]
    fn escala_condicional_maneja_extremos_y_valores_invalidos() {
        assert_eq!(color_escala_matriz("10", 10.0, 20.0), Some(0xDBEAFE));
        assert_eq!(color_escala_matriz("20", 10.0, 20.0), Some(0x93C5FD));
        assert_eq!(color_escala_matriz("15", 10.0, 20.0), Some(0xB7D8FE));
        assert_eq!(color_escala_matriz("texto", 10.0, 20.0), None);
        assert_eq!(color_escala_matriz("15", 10.0, 10.0), None);
    }
    use datan_report::Header;

    fn matrix(rows: Vec<Vec<&str>>) -> Matrix {
        Matrix {
            headers: Vec::new(),
            kinds: vec![RowKind::Data; rows.len()],
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(str::to_string).collect())
                .collect(),
            truncated: false,
        }
    }

    fn matriz_con_cabeceras(
        labels: &[&str],
        dimension_count: usize,
        rows: Vec<Vec<&str>>,
    ) -> Matrix {
        let kinds = vec![RowKind::Data; rows.len()];
        Matrix {
            headers: labels
                .iter()
                .enumerate()
                .map(|(index, label)| Header {
                    label: (*label).into(),
                    column: ObjectId(index as u64 + 1),
                    measure: (index >= dimension_count).then_some(ObjectId(index as u64 + 100)),
                    data_type: if index < dimension_count {
                        DataType::Text
                    } else {
                        DataType::Decimal
                    },
                })
                .collect(),
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(str::to_string).collect())
                .collect(),
            kinds,
            truncated: false,
        }
    }

    #[test]
    fn filas_de_datos_omite_subtotales_y_total_general() {
        let mut datos = matriz_con_cabeceras(
            &["Región", "Ventas"],
            1,
            vec![
                vec!["Norte", "10"],
                vec!["Norte · subtotal", "10"],
                vec!["Total", "10"],
            ],
        );
        datos.kinds = vec![
            RowKind::Data,
            RowKind::Subtotal { level: 0 },
            RowKind::GrandTotal,
        ];

        let rows = super::filas_de_datos(&datos).collect::<Vec<_>>();

        assert_eq!(
            rows.iter()
                .map(|(index, row)| (*index, row.iter().map(String::as_str).collect::<Vec<_>>(),))
                .collect::<Vec<_>>(),
            vec![(0, vec!["Norte", "10"])]
        );
    }

    #[test]
    fn escala_de_barras_ubica_valores_negativos_a_la_izquierda_del_cero() {
        let escala = super::EscalaBarras::desde_valores([-10.0, -100.0]);

        assert_eq!(escala.minimo(), -100.0);
        assert_eq!(escala.maximo(), 0.0);
        assert_eq!(escala.origen(), 1.0);
        assert_eq!(escala.ancho(-100.0), 1.0);
        assert_eq!(escala.ancho(-10.0), 0.1);
        assert_eq!(escala.posicion(-100.0), 0.0);
        assert_eq!(escala.posicion(-10.0), 0.9);
    }

    #[test]
    fn escala_de_barras_combinada_muestra_los_dos_lados_del_cero() {
        let escala = super::EscalaBarras::desde_valores([-20.0, 60.0]);

        assert_eq!(escala.minimo(), -20.0);
        assert_eq!(escala.maximo(), 60.0);
        assert_eq!(escala.origen(), 0.25);
        assert_eq!(escala.ancho(-20.0), 0.25);
        assert_eq!(escala.ancho(60.0), 0.75);
        assert_eq!(escala.posicion(-20.0), 0.0);
        assert_eq!(escala.posicion(60.0), 1.0);
    }

    #[test]
    fn escala_de_barras_ignora_valores_no_finitos_y_maneja_valores_vacios() {
        let escala = super::EscalaBarras::desde_valores([f64::NAN, f64::INFINITY]);

        assert_eq!(escala.minimo(), 0.0);
        assert_eq!(escala.maximo(), 1.0);
        assert_eq!(escala.origen(), 0.0);
        assert_eq!(escala.ancho(f64::NAN), 0.0);
        assert_eq!(
            super::EscalaBarras::desde_valores(std::iter::empty::<f64>()).ancho(0.0),
            0.0
        );
    }

    #[test]
    fn matriz_pivote_conserva_orden_y_tupla_completa() {
        let mut datos = matriz_con_cabeceras(
            &["Region", "Producto", "Ventas", "Unidades"],
            2,
            vec![
                vec!["Norte", "Mesa", "10", "2"],
                vec!["Sur", "Silla", "20", "3"],
                vec!["Norte", "Silla", "30", "4"],
            ],
        );
        datos
            .kinds
            .extend([RowKind::Subtotal { level: 0 }, RowKind::GrandTotal]);
        datos.rows.extend([
            vec!["Norte".into(), "Total".into(), "40".into(), "6".into()],
            vec!["Total".into(), "".into(), "60".into(), "9".into()],
        ]);

        let pivot = preparar_matriz_pivote(&datos, 2);

        assert_eq!(
            pivot
                .filas
                .iter()
                .map(|row| row.row_key.clone())
                .collect::<Vec<_>>(),
            vec![vec!["Norte"], vec!["Sur"]]
        );
        assert_eq!(
            pivot
                .columnas
                .iter()
                .map(|column| {
                    (
                        column
                            .categorias
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>(),
                        column.medida.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (vec!["Mesa"], "Ventas"),
                (vec!["Mesa"], "Unidades"),
                (vec!["Silla"], "Ventas"),
                (vec!["Silla"], "Unidades"),
            ]
        );
        let norte = &pivot.filas[0];
        assert_eq!(
            norte.celdas[0].as_ref().unwrap().filter_key,
            vec!["Norte", "Mesa"]
        );
        assert_eq!(norte.celdas[1].as_ref().unwrap().valor, "2");
        assert_eq!(
            norte.celdas[2].as_ref().unwrap().filter_key,
            vec!["Norte", "Silla"]
        );
        assert_eq!(norte.celdas[3].as_ref().unwrap().valor, "4");
        assert_eq!(norte.totales, vec![Some("40".into()), Some("6".into())]);
        assert_eq!(pivot.filas[1].celdas[0], None);
        assert_eq!(
            pivot.total_general,
            vec![Some("60".into()), Some("9".into())]
        );
        assert!(
            pivot
                .columnas
                .iter()
                .all(|column| !column.categorias.iter().any(|value| value == "Total"))
        );
    }

    #[test]
    fn alinea_totales_de_columna_por_categoria_y_medida() {
        let datos = matriz_con_cabeceras(
            &["Region", "Producto", "Ventas", "Unidades"],
            2,
            vec![vec!["Norte", "Mesa", "10", "2"]],
        );
        let pivot = preparar_matriz_pivote(&datos, 2);
        let totales = matriz_con_cabeceras(
            &["Producto", "Ventas", "Unidades"],
            1,
            vec![vec!["Silla", "80", "8"], vec!["Mesa", "30", "3"]],
        );

        assert_eq!(
            alinear_subtotales_columna(&pivot, &totales),
            vec![Some("30".into()), Some("3".into())]
        );
    }

    #[test]
    fn matriz_admite_varios_niveles_de_columna_y_alinea_tuplas_completas() {
        let mut datos = matriz_con_cabeceras(
            &["Region", "Año", "Mes", "Ventas"],
            3,
            vec![
                vec!["Norte", "2025", "Enero", "10"],
                vec!["Norte", "2026", "Enero", "20"],
            ],
        );
        datos.kinds.push(RowKind::Subtotal { level: 0 });
        datos
            .rows
            .push(vec!["Norte".into(), "".into(), "".into(), "30".into()]);

        let pivot = preparar_matriz_pivote_con_filas(&datos, 3, 1);
        assert_eq!(pivot.filas.len(), 1);
        assert_eq!(pivot.filas[0].row_key, vec!["Norte"]);
        assert_eq!(pivot.filas[0].totales, vec![Some("30".into())]);
        assert_eq!(
            pivot
                .columnas
                .iter()
                .map(|column| column.categorias.clone())
                .collect::<Vec<_>>(),
            vec![
                vec!["2025".to_string(), "Enero".to_string()],
                vec!["2026".to_string(), "Enero".to_string()],
            ]
        );

        let totales = matriz_con_cabeceras(
            &["Año", "Mes", "Ventas"],
            2,
            vec![vec!["2026", "Enero", "20"], vec!["2025", "Enero", "10"]],
        );
        assert_eq!(
            alinear_subtotales_columna(&pivot, &totales),
            vec![Some("10".into()), Some("20".into())]
        );
    }

    #[test]
    fn matriz_pivote_de_una_dimension_usa_medidas_como_columnas() {
        let mut datos = matriz_con_cabeceras(
            &["Producto", "Ventas", "Unidades"],
            1,
            vec![vec!["Mesa", "10", "2"], vec!["Silla", "20", "3"]],
        );
        datos.rows.push(vec!["".into(), "30".into(), "5".into()]);
        datos.kinds.push(RowKind::GrandTotal);

        let pivot = preparar_matriz_pivote(&datos, 1);

        assert_eq!(
            pivot
                .columnas
                .iter()
                .map(|column| column.medida.as_str())
                .collect::<Vec<_>>(),
            vec!["Ventas", "Unidades"]
        );
        assert!(
            pivot
                .columnas
                .iter()
                .all(|column| column.categorias.is_empty())
        );
        assert_eq!(pivot.filas[0].row_key, vec!["Mesa"]);
        assert_eq!(
            pivot.filas[0].celdas[1].as_ref().unwrap().filter_key,
            vec!["Mesa"]
        );
        assert_eq!(
            pivot.total_general,
            vec![Some("30".into()), Some("5".into())]
        );
        assert!(
            pivot
                .filas
                .iter()
                .all(|row| row.totales == vec![None, None])
        );
    }

    #[test]
    fn series_por_leyenda_alinea_categorias_y_deja_huecos() {
        let matrix = matriz_con_cabeceras(
            &["Mes", "Región", "Ventas"],
            2,
            vec![
                vec!["Enero", "Norte", "10"],
                vec!["Febrero", "Norte", "12"],
                vec!["Enero", "Sur", "7"],
            ],
        );

        let data = series_por_leyenda(&matrix, 2);

        assert_eq!(data.categorias, vec![vec!["Enero"], vec!["Febrero"]]);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].etiqueta, "Norte");
        assert_eq!(data.series[0].valores, vec![Some(10.0), Some(12.0)]);
        assert_eq!(data.series[1].etiqueta, "Sur");
        assert_eq!(data.series[1].valores, vec![Some(7.0), None]);
    }

    #[test]
    fn matriz_pivote_sin_totales_deja_campos_vacios() {
        let datos = matriz_con_cabeceras(
            &["Region", "Producto", "Ventas"],
            2,
            vec![vec!["Norte", "Mesa", "10"]],
        );

        let pivot = preparar_matriz_pivote(&datos, 2);

        assert_eq!(pivot.total_general, vec![None]);
        assert_eq!(pivot.filas[0].totales, vec![None]);
    }

    #[test]
    fn matriz_pivote_sin_dimensiones_no_agrupa_filas() {
        let datos = matriz_con_cabeceras(
            &["Region", "Ventas"],
            1,
            vec![vec!["Norte", "10"], vec!["Sur", "20"]],
        );

        assert_eq!(preparar_matriz_pivote(&datos, 0), MatrizPivote::default());
    }

    #[test]
    fn matriz_pivote_omite_filas_cortas_y_totales_y_acepta_kinds_faltantes() {
        let mut datos = matriz_con_cabeceras(
            &["Region", "Producto", "Ventas"],
            2,
            vec![
                vec!["Norte", "Mesa", "10"],
                vec!["Sur"],
                vec!["Oeste", "Mesa", "20"],
                vec!["Este", "Silla", "30"],
            ],
        );
        datos.kinds = vec![RowKind::Data, RowKind::Data, RowKind::Subtotal { level: 1 }];

        let pivot = preparar_matriz_pivote(&datos, 2);

        assert_eq!(pivot.filas.len(), 2);
        assert_eq!(pivot.filas[0].celdas[0].as_ref().unwrap().valor, "10");
        assert_eq!(pivot.filas[1].row_key, vec!["Este"]);
        assert_eq!(preparar_matriz_pivote(&datos, 4), MatrizPivote::default());
    }

    fn puntos_de_matriz(matrix: &Matrix) -> Vec<Punto> {
        matrix
            .rows
            .iter()
            .filter_map(|row| Some(Punto::new(row.first()?.clone(), row.get(1)?.parse().ok()?)))
            .collect()
    }

    #[test]
    fn embudo_normaliza_al_maximo_positivo() {
        assert_eq!(
            datos_embudo(&[
                Punto::new("A", 10.0),
                Punto::new("B", 5.0),
                Punto::new("C", -1.0)
            ]),
            vec![
                Fraccion {
                    valor: 10.0,
                    proporcion: 1.0
                },
                Fraccion {
                    valor: 5.0,
                    proporcion: 0.5
                },
                Fraccion {
                    valor: -1.0,
                    proporcion: 0.0
                },
            ]
        );
    }

    #[test]
    fn dibuja_embudo_con_valores_de_matriz() {
        let datos = matrix(vec![vec!["Visitas", "100"], vec!["Compras", "25"]]);
        let puntos = puntos_de_matriz(&datos);

        let _elemento = embudo(&puntos);

        assert_eq!(
            puntos,
            vec![Punto::new("Visitas", 100.0), Punto::new("Compras", 25.0)]
        );
        assert_eq!(datos_embudo(&puntos)[1].proporcion, 0.25);
    }

    #[test]
    fn medidor_acota_el_rango_y_rechaza_rangos_invalidos() {
        assert_eq!(avance_medidor(15.0, 10.0, 20.0), 0.5);
        assert_eq!(avance_medidor(30.0, 10.0, 20.0), 1.0);
        assert_eq!(avance_medidor(12.0, 10.0, 10.0), 0.0);
    }

    #[test]
    fn dibuja_medidor_con_valor_y_rango_de_matriz() {
        let datos = matrix(vec![vec!["Uso", "75"]]);
        let punto = puntos_de_matriz(&datos).remove(0);
        let medidor_dato = Medidor {
            id: "medidor-test".to_string(),
            etiqueta: punto.etiqueta,
            valor: punto.valor,
            minimo: 0.0,
            maximo: 100.0,
            objetivo: None,
            formato: None,
        };

        let _elemento = medidor(&medidor_dato);

        assert_eq!(
            avance_medidor(medidor_dato.valor, medidor_dato.minimo, medidor_dato.maximo),
            0.75
        );
    }

    #[test]
    fn treemap_asigna_un_area_proporcional_a_cada_punto() {
        let rects = datos_treemap(&[Punto::new("A", 1.0), Punto::new("B", 3.0)], 100.0, 40.0);
        assert_eq!(rects.len(), 2);
        let area_a = rects[0].ancho * rects[0].alto;
        let area_b = rects[1].ancho * rects[1].alto;
        assert!((area_a + area_b - 4_000.0).abs() < 0.01);
        assert!((area_b / area_a - 3.0).abs() < 0.01);
    }

    #[test]
    fn tooltip_treemap_muestra_la_categoria_y_el_valor_completo() {
        let punto = Punto::new("Categoría extensa", 1234.5);

        assert_eq!(tooltip_treemap(&punto), "Categoría extensa · 1234.50");
    }

    #[test]
    fn tooltips_personalizados_muestran_valor_y_contexto() {
        let formato = Formato::Moneda {
            simbolo: "€".to_string(),
            decimales: 2,
        };
        let punto = Punto::new("Sur", 1234.0).con_formato(Some(formato.clone()));
        let medidor = Medidor {
            id: "medidor-test".to_string(),
            etiqueta: "Uso".to_string(),
            valor: 75.0,
            minimo: 0.0,
            maximo: 100.0,
            objetivo: None,
            formato: Some(formato.clone()),
        };
        let tramo = TramoCascada {
            etiqueta: "Marzo".to_string(),
            inicio: 10.0,
            fin: 13.0,
            formato: Some(formato),
        };

        assert_eq!(tooltip_embudo(&punto), "Sur · €1.234,00");
        assert_eq!(tooltip_treemap(&punto), "Sur · €1.234,00");
        assert_eq!(
            tooltip_medidor(&medidor),
            "Uso · Valor: €75,00 · Mínimo: €0,00 · Máximo: €100,00"
        );
        assert_eq!(
            tooltip_cascada(&tramo),
            "Marzo · Cambio: €3,00 · Acumulado: €13,00"
        );
    }

    #[test]
    fn dibuja_treemap_con_areas_de_matriz() {
        let datos = matrix(vec![vec!["A", "1"], vec!["B", "3"]]);
        let puntos = puntos_de_matriz(&datos);

        let _elemento = treemap(&puntos);

        let rectangulos = datos_treemap(&puntos, ANCHO_GRAFICO, ALTO_GRAFICO);
        assert_eq!(rectangulos.len(), 2);
        let area_a = rectangulos[0].ancho * rectangulos[0].alto;
        let area_b = rectangulos[1].ancho * rectangulos[1].alto;
        assert!((area_b / area_a - 3.0).abs() < 0.01);
    }

    #[test]
    fn cascada_usa_la_base_acumulada() {
        assert_eq!(
            datos_cascada(
                100.0,
                &[Punto::new("Venta", 25.0), Punto::new("Costo", -10.0)]
            ),
            vec![
                TramoCascada {
                    etiqueta: "Venta".into(),
                    inicio: 100.0,
                    fin: 125.0,
                    formato: None,
                },
                TramoCascada {
                    etiqueta: "Costo".into(),
                    inicio: 125.0,
                    fin: 115.0,
                    formato: None,
                },
            ]
        );
    }

    #[test]
    fn dibuja_cascada_con_cambios_de_matriz() {
        let datos = matrix(vec![vec!["Venta", "25"], vec!["Costo", "-10"]]);
        let cambios = puntos_de_matriz(&datos);

        let _elemento = cascada(100.0, &cambios);

        assert_eq!(
            datos_cascada(100.0, &cambios),
            vec![
                TramoCascada {
                    etiqueta: "Venta".into(),
                    inicio: 100.0,
                    fin: 125.0,
                    formato: None,
                },
                TramoCascada {
                    etiqueta: "Costo".into(),
                    inicio: 125.0,
                    fin: 115.0,
                    formato: None,
                },
            ]
        );
    }

    #[test]
    fn kpi_evalua_la_direccion_de_objetivo() {
        assert_eq!(estado_kpi(8.0, Some(10.0), false), EstadoKpi::BajoObjetivo);
        assert_eq!(estado_kpi(8.0, Some(10.0), true), EstadoKpi::SobreObjetivo);
        assert_eq!(estado_kpi(10.0, Some(10.0), false), EstadoKpi::EnObjetivo);
        assert_eq!(estado_kpi(8.0, None, false), EstadoKpi::SinObjetivo);
    }

    #[test]
    fn dibuja_kpi_con_valor_y_objetivo_de_matriz() {
        let datos = matrix(vec![vec!["Ingresos", "120"]]);
        let valor = puntos_de_matriz(&datos).remove(0).valor;
        let kpi = TarjetaKpi {
            etiqueta: "Ingresos".into(),
            valor,
            objetivo: Some(("Objetivo".into(), 100.0)),
            menor_es_mejor: false,
            color_valor: None,
            escala_texto: 1.0,
        };

        let _elemento = tarjeta_kpi(&kpi);

        assert_eq!(
            estado_kpi(
                kpi.valor,
                kpi.objetivo.as_ref().map(|(_, meta)| *meta),
                kpi.menor_es_mejor
            ),
            EstadoKpi::SobreObjetivo
        );
    }

    #[test]
    fn dibuja_kpi_con_el_formato_guardado() {
        let datos = matrix(vec![vec!["Ingresos", "120"]]);
        let valor = puntos_de_matriz(&datos).remove(0).valor;
        let kpi = TarjetaKpi {
            etiqueta: "Ingresos".into(),
            valor,
            objetivo: Some(("Objetivo".into(), 100.0)),
            menor_es_mejor: false,
            color_valor: Some(0x123456),
            escala_texto: 1.2,
        };
        let formato = datan_report::saved::SavedFormato {
            escala_texto: 120,
            color_texto: 0x1f2937,
            color_fondo: 0xf8fafc,
            color_borde: 0x2563eb,
            ancho_borde: 2,
            radio_borde: 8,
            fuente_mono: true,
            ..datan_report::saved::SavedFormato::default()
        };

        let _elemento = tarjeta_kpi_zoom_con_formato(&kpi, formato, Some("monospace".into()), 1.0);
    }

    #[test]
    fn vela_lee_los_campos_ohlc_de_la_matriz() {
        let candles = velas_de_matriz(&matrix(vec![vec!["2026-01", "10", "15", "8", "13"]]), 1);
        assert_eq!(
            candles,
            vec![Vela {
                etiqueta: "2026-01".into(),
                clave_categoria: vec!["2026-01".into()],
                apertura: 10.0,
                maximo: 15.0,
                minimo: 8.0,
                cierre: 13.0,
            }]
        );
        assert!(
            velas_de_matriz(&matrix(vec![vec!["2026-02", "NaN", "15", "8", "13"]]), 1).is_empty()
        );
    }

    #[test]
    fn sankey_lee_enlaces_de_las_dos_dimensiones_y_la_medida() {
        let (nodes, links) = flujo_de_matriz(
            &matrix(vec![
                vec!["Web", "Carro", "12"],
                vec!["Tienda", "Caja", "7"],
            ]),
            2,
        );
        assert_eq!(
            nodes,
            vec![
                NodoFlujo {
                    fase: 0,
                    etiqueta: "Web".into()
                },
                NodoFlujo {
                    fase: 1,
                    etiqueta: "Carro".into()
                },
                NodoFlujo {
                    fase: 0,
                    etiqueta: "Tienda".into()
                },
                NodoFlujo {
                    fase: 1,
                    etiqueta: "Caja".into()
                },
            ]
        );
        assert_eq!(
            links,
            vec![SankeyLink::new(0, 1, 12.0), SankeyLink::new(2, 3, 7.0)]
        );
    }
}
