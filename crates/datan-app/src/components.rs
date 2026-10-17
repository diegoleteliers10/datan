//! Las piezas de la interfaz.
//!
//! Un componente por FORMA, no por sitio: la misma tarjeta sirve en el lienzo, en
//! Propiedades y en el panel de conexiones. Sin esto cada zona repite relleno, borde y radio,
//! y el lienzo termina con cuatro maneras de dibujar el mismo boton.
//!
//! Todos los componentes piden el tema en vez de leer un color global. Un elemento de una
//! lista se construye con el tema del render que lo pide, y asi el dia que el tema cambie no
//! hay una copia del color viejo guardada en una constante.

use gpui_kit::assets::IconName;
use gpui_kit::component::Theme;
use gpui_kit::*;
// `Div::on_click` y `Div::hover` viven en este trait. `gpui-kit` reexporta los tipos de gpui
// pero no sus traits de interactividad.
use ::gpui::InteractiveElement;
// `font_semibold` y los demas pesos cortos.
use gpui_kit::base::StyledExt as _;

use crate::tokens;

/// El peso de un boton.
///
/// Tres pesos y no un juego de estilos sueltos: la accion principal de una zona es una sola,
/// y una barra con dos botones oscuros no dice cual es.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Peso {
    /// Relleno oscuro, texto claro. La accion principal.
    Primario,
    /// Borde y fondo de la superficie. La accion de al lado.
    Secundario,
    /// Sin borde ni fondo. Una accion de la que nadie tiene que darse cuenta.
    Fantasma,
}

/// Un boton de texto.
pub fn boton<F>(
    theme: &Theme,
    peso: Peso,
    id: impl Into<SharedString>,
    etiqueta: impl Into<SharedString>,
    on_click: F,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    boton_con_icono(theme, peso, id, None, etiqueta, on_click)
}

/// Un boton con un icono adelante del texto.
pub fn boton_con_icono<F>(
    theme: &Theme,
    peso: Peso,
    id: impl Into<SharedString>,
    nombre: Option<IconName>,
    etiqueta: impl Into<SharedString>,
    on_click: F,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let color = match peso {
        Peso::Primario => theme.primary_foreground,
        _ => theme.foreground,
    };
    let mut base = div()
        .id(ElementId::Name(id.into()))
        .h(px(tokens::ALTO_CONTROL))
        .px_3()
        .rounded_lg()
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(tokens::LETRA_TITULO))
        .cursor_pointer();
    if let Some(nombre) = nombre {
        base = base.child(icono(nombre, tokens::LETRA_ICONO, color));
    }
    let base = base.child(etiqueta.into());
    let pintado = match peso {
        Peso::Primario => base
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .hover(|style| style.bg(theme.primary_hover)),
        Peso::Secundario => base
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .hover(|style| style.bg(theme.list_hover)),
        Peso::Fantasma => base
            .text_color(theme.muted_foreground)
            .hover(|style| style.bg(theme.list_hover)),
    };
    pintado.on_click(on_click).into_any_element()
}

/// Un boton que existe pero no hace nada, porque el flujo que lo respalda no existe.
///
/// Se dibuja apagado y no recibe clic. Un boton que se ve activo y no responde es peor que
/// uno apagado: el usuario cree que la app se colgo. Y uno pintado con el color de la accion
/// principal, aunque este apagado, se sigue leyendo como la accion principal.
pub fn boton_apagado(theme: &Theme, etiqueta: impl Into<SharedString>) -> AnyElement {
    boton_apagado_con_icono(theme, None, etiqueta)
}

/// Un boton apagado con un icono adelante.
pub fn boton_apagado_con_icono(
    theme: &Theme,
    nombre: Option<IconName>,
    etiqueta: impl Into<SharedString>,
) -> AnyElement {
    let mut base = div()
        .h(px(tokens::ALTO_CONTROL))
        .px_3()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(tokens::LETRA_TITULO))
        .text_color(theme.muted_foreground);
    if let Some(nombre) = nombre {
        base = base.child(icono(nombre, tokens::LETRA_ICONO, theme.muted_foreground));
    }
    base.child(etiqueta.into()).into_any_element()
}

/// Un boton de solo icono.
pub fn boton_icono<F>(
    theme: &Theme,
    id: impl Into<SharedString>,
    nombre: IconName,
    on_click: F,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let base = div()
        .id(ElementId::Name(id.into()))
        .w(px(tokens::ALTO_CONTROL))
        .h(px(tokens::ALTO_CONTROL))
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(tokens::LETRA_ICONO))
        .text_color(theme.muted_foreground)
        .cursor_pointer()
        .hover(|style| style.bg(theme.list_hover))
        .child(nombre);
    base.on_click(on_click).into_any_element()
}

/// Un boton de solo icono que no se puede tocar.
pub fn boton_icono_apagado(theme: &Theme, nombre: IconName) -> AnyElement {
    div()
        .w(px(tokens::ALTO_CONTROL))
        .h(px(tokens::ALTO_CONTROL))
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(tokens::LETRA_ICONO))
        // El borde del tema, que es el tono mas tenue: un icono apagado tiene que leerse
        // como apagado, no como un icono gris mas.
        .text_color(theme.border)
        .child(nombre)
        .into_any_element()
}

/// Un icono del catalogo, con su tamano y su color.
pub fn icono(nombre: IconName, tamano: f32, color: Hsla) -> AnyElement {
    div()
        .text_size(px(tamano))
        .text_color(color)
        .child(nombre)
        .into_any_element()
}

/// La caja de un control segmentado: un fondo tenue con la opcion activa adentro.
///
/// Una fila de botones con borde se lee como acciones sueltas. Un control segmentado dice
/// que las opciones son excluyentes.
pub fn caja_segmentada(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .p_1()
        .rounded_lg()
        .bg(theme.muted)
}

/// Una opcion de un control segmentado.
pub fn segmento<F>(
    theme: &Theme,
    id: impl Into<SharedString>,
    etiqueta: impl Into<SharedString>,
    activo: bool,
    on_click: F,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let base = div()
        .id(ElementId::Name(id.into()))
        .h(px(22.0))
        .px_2()
        .rounded_md()
        .flex()
        .items_center()
        .text_size(px(tokens::LETRA_NOTA))
        .cursor_pointer()
        .child(etiqueta.into());
    let pintado = if activo {
        base.bg(theme.primary).text_color(theme.primary_foreground)
    } else {
        base.text_color(theme.muted_foreground)
            .hover(|style| style.bg(theme.list_hover))
    };
    pintado.on_click(on_click).into_any_element()
}

/// Una pastilla con una `x`: un filtro activo. Pincharla quita el filtro.
pub fn pastilla<F>(
    theme: &Theme,
    id: impl Into<SharedString>,
    etiqueta: impl Into<SharedString>,
    on_click: F,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(ElementId::Name(id.into()))
        .h(px(24.0))
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(tokens::LETRA_NOTA))
        .cursor_pointer()
        .hover(|style| style.bg(theme.list_hover))
        .child(etiqueta.into())
        .child(
            div()
                .text_size(px(tokens::LETRA_NOTA))
                .text_color(theme.muted_foreground)
                .child("\u{00d7}"),
        )
        .on_click(on_click)
        .into_any_element()
}

/// Una fila de la lista de campos o de medidas.
///
/// El estado de seleccion se ve en el relleno y en el peso de la letra, no en un punto al
/// lado: un punto de dos pixeles no dice cual de las treinta filas esta elegida.
pub fn fila_dato<F>(
    theme: &Theme,
    id: impl Into<SharedString>,
    etiqueta: impl Into<SharedString>,
    activa: bool,
    on_click: F,
) -> AnyElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let base = div()
        .id(ElementId::Name(id.into()))
        .h(px(tokens::ALTO_FILA))
        .px_2()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(tokens::LETRA_TITULO))
        .overflow_hidden()
        .cursor_pointer()
        .child(etiqueta.into());
    let pintado = if activa {
        base.bg(theme.primary)
            .text_color(theme.primary_foreground)
            .font_semibold()
    } else {
        base.text_color(theme.foreground)
            .hover(|style| style.bg(theme.list_hover))
    };
    pintado.on_click(on_click).into_any_element()
}

/// La superficie de una tarjeta: el fondo de la app, con borde suave y sin sombra.
///
/// Una tarjeta se separa por tono y por una linea de un pixel. Una sombra pesada sobre un
/// fondo claro se ve como un elemento flotando, y en un lienzo con ocho visuales son ocho
/// sombras peleando.
pub fn tarjeta(theme: &Theme) -> Div {
    div()
        .rounded_2xl()
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
}

/// El titulo de una tarjeta.
pub fn titulo_tarjeta(theme: &Theme, texto: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(tokens::LETRA_TITULO))
        .font_semibold()
        .text_color(theme.foreground)
        .child(texto.into())
}

/// Una linea de texto secundario: un conteo, una nota, la leyenda.
pub fn nota(theme: &Theme, texto: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(tokens::LETRA_NOTA))
        .text_color(theme.muted_foreground)
        .child(texto.into())
}
