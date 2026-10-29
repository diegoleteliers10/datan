//! Escape de identificadores y literales para SQL de DuckDB.
//!
//! Un identificador con espacio necesita comillas dobles: `"Dim Product"`. Un literal de
//! texto necesita comillas simples y duplicar las comillas internas: `'O''Brien'`.
//!
//! No se permite un identificador sin escapar nunca. Un nombre de tabla que viene de la
//! conexion no se concatena a mano.

/// Un identificador de SQL, siempre entre comillas dobles.
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Un literal de texto de SQL, con las comillas simples duplicadas.
pub fn escape_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapa_identificadores_con_espacios() {
        assert_eq!(quote_identifier("Dim Product"), "\"Dim Product\"");
        assert_eq!(quote_identifier("Qty"), "\"Qty\"");
    }

    #[test]
    fn duplica_las_comillas_dobles_de_un_identificador() {
        assert_eq!(quote_identifier("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn duplica_las_comillas_simples_de_un_literal() {
        assert_eq!(escape_string("O'Brien"), "'O''Brien'");
        assert_eq!(escape_string("Norte"), "'Norte'");
    }

    #[test]
    fn un_nombre_con_comilla_simple_no_rompe_el_sql() {
        // El caso que importa: un valor de filtro que viene de datos, no de codigo.
        let rendered = escape_string("Robert'); DROP TABLE fact; --");
        assert_eq!(rendered, "'Robert''); DROP TABLE fact; --'");
        // El literal abre con una comilla simple y cierra con otra: dos en total. La
        // comilla del contenido va duplicada y cuenta como dos.
        assert_eq!(rendered.matches('\'').count(), 4);
        assert!(rendered.starts_with('\'') && rendered.ends_with('\''));
    }

    #[test]
    fn un_valor_sin_comillas_queda_balanceado() {
        let rendered = escape_string("Norte");
        assert_eq!(rendered, "'Norte'");
        assert_eq!(rendered.matches('\'').count(), 2);
    }
}

/// Un nombre que puede traer un calificador de base o de esquema.
///
/// `pg.product_boleta` se escapa como `"pg"."product_boleta"`, no como
/// `"pg.product_boleta"`. La segunda forma es un identificador UNICO con un punto dentro,
/// que DuckDB acepta y despues no encuentra: el SQL se genera bien y la consulta falla con
/// "table does not exist" y una sugerencia que es justo el nombre con el prefijo que faltaba.
pub fn quote_qualified(name: &str) -> String {
    match name.split_once('.') {
        Some((qualifier, table)) => {
            format!(
                "{}.{}",
                quote_identifier(qualifier),
                quote_identifier(table)
            )
        }
        None => quote_identifier(name),
    }
}
