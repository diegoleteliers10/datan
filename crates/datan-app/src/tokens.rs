//! Los numeros y los colores del diseno, en un solo sitio.
//!
//! Antes cada zona llevaba su propio `px(12.0)` y su propio relleno. Dos tarjetas del mismo
//! lienzo salian con radios distintos, y cambiar el alto de la barra superior obligaba a
//! recorrer el archivo entero.
//!
//! Los colores que el tema YA tiene no se repiten aqui. `theme.background`, `theme.border`,
//! `theme.muted_foreground`, `theme.chart_bullish` y `theme.chart_grid` son la fuente: una
//! copia en una constante se queda atras el dia que el tema cambia. Aqui vive solo lo que el
//! tema no nombra: la paleta de series y la escala de tamanos.
//!
//! La escala de radios y de fracciones es la del sistema de estilos, que es la de Tailwind.
//! Un control mide 8 px, una tarjeta 16 px, una pastilla el alto completo.

use gpui::{Hsla, rgb};

// --- Medidas de la ventana -------------------------------------------------------------------

/// El alto de la barra superior.
pub const BARRA_SUPERIOR: f32 = 48.0;
/// El ancho del panel de Datos.
pub const PANEL_DATOS: f32 = 264.0;
/// El ancho del panel de Visualizaciones.
pub const PANEL_VISUALES: f32 = 264.0;
/// El alto de la barra de pestanas.
pub const ALTO_PESTANAS: f32 = 40.0;
/// El alto del grid.
///
/// La lista de filas mide su alto contra el de su caja: necesita un alto definido arriba, o
/// el alto disponible es "lo que sobre" y no pide ninguna fila.
pub const ALTO_GRID: f32 = 420.0;
/// El ancho de una columna del grid.
///
/// Una columna mide lo que la cabecera mas larga: `catalogo_productos[nombre]` con su flecha.
/// Mas angosta parte la cabecera en dos lineas y la fila de titulos ocupa el doble de alto.
pub const ANCHO_CELDA: f32 = 240.0;
/// El alto de un boton, un input y un boton de segmento.
pub const ALTO_CONTROL: f32 = 28.0;
/// El alto de una fila de la barra lateral.
pub const ALTO_FILA: f32 = 26.0;

// --- Escala ---------------------------------------------------------------------------------

/// El titulo de la pagina.
pub const LETRA_H1: f32 = 24.0;
/// El valor grande de una tarjeta KPI.
pub const LETRA_VALOR: f32 = 22.0;
/// Un titulo de tarjeta, un boton, el nombre de una tabla.
pub const LETRA_TITULO: f32 = 12.0;
/// Una etiqueta, un eje, una pastilla.
pub const LETRA_ETIQUETA: f32 = 12.0;
/// Un conteo, una nota al pie, la leyenda.
pub const LETRA_NOTA: f32 = 11.0;
/// Un icono de control.
pub const LETRA_ICONO: f32 = 16.0;

// --- Paleta de series -----------------------------------------------------------------------

/// Los cinco matices de la paleta de series.
///
/// La paleta del tema (`chart.1` a `chart.5`) es una rampa de AZUL: cinco tonos del mismo
/// color no distinguen cinco categorias, y en un anillo salen cinco porciones que parecen la
/// misma. Estos son cinco matices distintos, en las familias que el tema ya usa.
const SERIES: [u32; 5] = [
    0x2563eb, // azul
    0x0d9488, // verde azulado
    0xea580c, // naranja
    0x7c3aed, // violeta
    0xdc2626, // rojo
];

/// El color de la serie `indice`. Cicla: ocho series usan los cinco colores otra vez.
pub fn serie(indice: usize) -> Hsla {
    Hsla::from(rgb(SERIES[indice % SERIES.len()]))
}

/// El color de una barra segun su lugar, para el grafico de barras de una sola medida.
pub fn serie_unica() -> Hsla {
    serie(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_paleta_cicla_y_no_se_sale() {
        assert_eq!(serie(0), Hsla::from(rgb(SERIES[0])));
        assert_eq!(serie(5), serie(0), "el sexto color repite el primero");
        assert_eq!(serie(7), serie(2));
    }

    #[test]
    fn los_cinco_colores_son_distintos() {
        // Una paleta con un color repetido dibuja dos categorias con el mismo tono, y la
        // leyenda miente sin decirlo.
        let mut vistos: Vec<Hsla> = (0..5).map(serie).collect();
        vistos.sort_by(|a, b| a.h.partial_cmp(&b.h).expect("sin NaN"));
        let antes = vistos.len();
        vistos.dedup();
        assert_eq!(vistos.len(), antes);
    }
}
