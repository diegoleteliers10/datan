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
//! La escala de radios es la del sistema de estilos. La app usa dos: 8 px (`rounded_xl`) en
//! todo lo que se toca y en todo lo que agrupa una fila, y 16 px (`rounded_2xl`) en una
//! tarjeta. Las marcas de un grafico van mas abajo, en 4 px, porque una barra de 18 px con la
//! esquina de un boton se lee como un boton.

use gpui::{Hsla, rgb};

// --- Medidas de la ventana -------------------------------------------------------------------

/// El alto de la barra superior.
pub const BARRA_SUPERIOR: f32 = 48.0;
/// El ancho del panel de Datos.
pub const PANEL_DATOS: f32 = 264.0;
/// El ancho del panel de Visualizaciones.
pub const PANEL_VISUALES: f32 = 264.0;
pub const PANEL_FILTROS: f32 = 248.0;
pub const PANEL_PLEGADO: f32 = 36.0;
/// El alto de la barra de pestanas.
pub const ALTO_PESTANAS: f32 = 40.0;
/// El alto de la tarjeta del grafico cuando comparte el lienzo con la tabla.
///
/// Cuando el grafico es lo unico del lienzo NO se usa: ahi la tarjeta se queda con lo que
/// sobra, que es lo que hace la referencia. Con un alto fijo, "lo que sobra" se dibujaba
/// fuera de la ventana.
pub const ALTO_GRAFICO: f32 = 220.0;
/// El ancho de una columna del grid.
///
/// Una columna mide lo que la cabecera mas larga: `catalogo_productos[nombre]` con su flecha.
/// Mas angosta parte la cabecera en dos lineas y la fila de titulos ocupa el doble de alto.
pub const ANCHO_CELDA: f32 = 240.0;
/// El alto de un boton, un input y un boton de segmento.
pub const ALTO_CONTROL: f32 = 28.0;
/// El alto de la caja de un control segmentado.
///
/// `ALTO_CONTROL` menos el relleno de la caja (4 px arriba y abajo). Sin esto el segmentado
/// mide dos pixeles mas que los botones de al lado y la barra queda desalineada.
pub const ALTO_SEGMENTO: f32 = 20.0;
/// El alto de una pastilla: el nombre de la base, la pestana de la pagina.
pub const ALTO_PASTILLA: f32 = 24.0;
/// El alto de la cabecera de un panel.
pub const ALTO_CABECERA: f32 = 40.0;
/// El alto de una fila de la barra lateral.
pub const ALTO_FILA: f32 = 26.0;
/// El ancho de un mosaico del selector de visuales.
///
/// Tres por fila en el panel, con espacio para etiquetas como "Candlestick" y "Segmentador".
/// El alto del area de dibujo de una dispersion.
///
/// Un canvas se mide por lo que le pide el estilo: sin alto propio hereda cero y los puntos
/// caen todos en el mismo lugar. Este alto es el del dibujo, no el de la tarjeta.
pub const ALTO_DISPERSION: f32 = 190.0;
/// El ancho de la pista de un interruptor.
///
/// La pista y el boton de adentro se miden juntos: 28 de pista con 2 de relleno deja un
/// boton de 12 que se ve como un control y no como un punto.
pub const ANCHO_INTERRUPTOR: f32 = 28.0;

// --- Escala ---------------------------------------------------------------------------------

/// El titulo de la pagina.
pub const LETRA_H1: f32 = 24.0;
/// El valor grande de una tarjeta KPI.
pub const LETRA_VALOR: f32 = 22.0;
/// Un titulo de tarjeta, un boton, el nombre de una tabla.
pub const LETRA_TITULO: f32 = 12.0;
/// Una etiqueta, un eje, una pastilla.
pub const LETRA_ETIQUETA: f32 = 12.0;
/// El texto corriente: una celda del grid, la marca de la barra superior.
pub const LETRA_CUERPO: f32 = 13.0;
/// Un conteo, una nota al pie, la leyenda.
pub const LETRA_NOTA: f32 = 11.0;
/// Un icono de control.
pub const LETRA_ICONO: f32 = 16.0;

// --- Paleta de series -----------------------------------------------------------------------

/// Los cinco matices de la paleta de series en claro.
///
/// La paleta del tema (`chart.1` a `chart.5`) es una rampa de AZUL: cinco tonos del mismo
/// color no distinguen cinco categorias, y en un anillo salen cinco porciones que parecen la
/// misma. Estos son cinco matices distintos, en las familias que el tema ya usa.
const SERIES_CLARO: [u32; 5] = [
    0x2563eb, // azul
    0x0d9488, // verde azulado
    0xea580c, // naranja
    0x7c3aed, // violeta
    0xdc2626, // rojo
];

/// La misma paleta un escalon mas clara, para el fondo oscuro.
///
/// Sobre `neutral-950` los tonos 600 quedan cerca de 3:1 y el violeta se hunde en el fondo.
/// Cambia el escalon, no la familia: la misma categoria conserva su color al cambiar de tema,
/// que es lo que hace que dos capturas del mismo informe se puedan comparar.
const SERIES_OSCURO: [u32; 5] = [
    0x60a5fa, // azul
    0x2dd4bf, // verde azulado
    0xfb923c, // naranja
    0xa78bfa, // violeta
    0xf87171, // rojo
];

/// El color de la serie `indice`. Cicla: ocho series usan los cinco colores otra vez.
///
/// `oscuro` sale del tema vivo (`theme.is_dark()`), no de una preferencia guardada aparte: el
/// color tiene que seguir el fondo que se esta pintando, no el que se eligio al arrancar.
pub fn serie(indice: usize, oscuro: bool) -> Hsla {
    let paleta = if oscuro { SERIES_OSCURO } else { SERIES_CLARO };
    Hsla::from(rgb(paleta[indice % paleta.len()]))
}

/// El color de una barra segun su lugar, para el grafico de barras de una sola medida.
pub fn serie_unica(oscuro: bool) -> Hsla {
    serie(0, oscuro)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_paleta_cicla_y_no_se_sale() {
        assert_eq!(serie(0, false), Hsla::from(rgb(SERIES_CLARO[0])));
        assert_eq!(
            serie(5, false),
            serie(0, false),
            "el sexto color repite el primero"
        );
        assert_eq!(serie(7, false), serie(2, false));
    }

    #[test]
    fn los_cinco_colores_son_distintos_en_los_dos_temas() {
        // Una paleta con un color repetido dibuja dos categorias con el mismo tono, y la
        // leyenda miente sin decirlo. Vale para el claro y para el oscuro.
        for oscuro in [false, true] {
            let mut vistos: Vec<Hsla> = (0..5).map(|indice| serie(indice, oscuro)).collect();
            vistos.sort_by(|a, b| a.h.partial_cmp(&b.h).expect("sin NaN"));
            let antes = vistos.len();
            vistos.dedup();
            assert_eq!(vistos.len(), antes, "oscuro = {oscuro}");
        }
    }

    #[test]
    fn la_misma_categoria_conserva_el_matiz_al_cambiar_de_tema() {
        // El escalon cambia; la familia no. Si el indice 2 fuera violeta en claro y naranja
        // en oscuro, cambiar de tema reordenaria las categorias de un grafico.
        for indice in 0..5 {
            let claro = serie(indice, false).h;
            let oscuro = serie(indice, true).h;
            // Diferencia circular: el rojo vive en 0 y en 1 a la vez.
            let mut diferencia = (claro - oscuro).abs();
            diferencia = diferencia.min(1.0 - diferencia);
            assert!(
                diferencia < 0.03,
                "el indice {indice} cambia de familia: {claro} contra {oscuro}"
            );
        }
    }
}
