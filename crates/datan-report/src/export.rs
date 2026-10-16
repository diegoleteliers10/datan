//! Exportar la matriz del informe.
//!
//! # Por que CSV y no Excel
//!
//! CSV lo abre todo, y lo que exporta es exactamente lo que se ve: las mismas filas, en el
//! mismo orden, con las mismas cabeceras. Un XLSX exigiria una dependencia de escritura y
//! produciria un archivo que nadie puede leer sin la misma.
//!
//! # El escape es la parte que falla
//!
//! Un valor de dimension es texto libre: `Producto, 1` con una coma dentro parte la fila en
//! dos. Y un valor con comillas parte el parser. El escape de RFC 4180 es: rodear con
//! comillas si hay coma, comilla o salto de linea, y duplicar las comillas interiores.
//!
//! La alternativa de "escapar solo cuando hace falta" es la que produce archivos que abren
//! bien con datos de prueba y mal con datos reales.

use crate::Matrix;

/// Un error de exportacion.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no se pudo escribir {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// La matriz como texto CSV, con las cabeceras en la primera linea.
///
/// El separador es coma y el salto final es `\n`. No se escribe BOM: los BOM rompen
/// herramientas de linea de comando y `awk`, y la razon para ponerlo (Excel y las comillas
/// UTF-8) es un problema de Excel, no del archivo.
pub fn csv(matrix: &Matrix) -> String {
    let mut out = String::new();

    let headers: Vec<String> = matrix
        .headers
        .iter()
        .map(|header| escape(&header.label))
        .collect();
    out.push_str(&headers.join(","));
    out.push('\n');

    for row in &matrix.rows {
        let cells: Vec<String> = row.iter().map(|cell| escape(cell)).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }

    out
}

/// Escribe la matriz a un archivo.
pub fn write_csv(matrix: &Matrix, path: &std::path::Path) -> Result<(), ExportError> {
    std::fs::write(path, csv(matrix)).map_err(|source| ExportError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// El escape de RFC 4180.
///
/// Se rodea con comillas SIEMPRE que el valor tenga coma, comilla, salto de linea o retorno
/// de carro. Un valor sin nada de eso sale tal cual, para que el archivo sea legible a ojo.
fn escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datan_model::{DataType, ObjectId};
    use crate::Header;

    fn matrix() -> Matrix {
        Matrix {
            headers: vec![
                Header {
                    label: "region".to_string(),
                    column: ObjectId(21),
                    measure: None,
                    data_type: DataType::Text,
                },
                Header {
                    label: "Qty".to_string(),
                    column: ObjectId(0),
                    measure: Some(ObjectId(100)),
                    data_type: DataType::Decimal,
                },
            ],
            rows: vec![
                vec!["Norte".to_string(), "60".to_string()],
                vec!["Sur".to_string(), "48".to_string()],
            ],
            truncated: false,
        }
    }

    #[test]
    fn el_csv_tiene_cabeceras_y_una_fila_por_grupo() {
        let text = csv(&matrix());
        assert_eq!(text, "region,Qty\nNorte,60\nSur,48\n");
    }

    #[test]
    fn un_valor_con_coma_va_entre_comillas() {
        // Un nombre de producto con coma parte la fila en dos sin el escape. El numero de
        // la medida acabaria en la columna de la dimension siguiente, y el informe seguiria
        // sumando: los totales estarian bien y las filas mal, que es lo peor de los dos.
        let mut matrix = matrix();
        matrix.rows[0][0] = "Producto, 1".to_string();
        let text = csv(&matrix);

        assert!(text.contains("\"Producto, 1\",60"), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "una fila sigue siendo una fila");
    }

    #[test]
    fn las_comillas_interiores_se_duplican() {
        // `El "mejor"` escapado sin duplicar cerraria el campo a la mitad, y lo que queda
        // despues lo leeria el parser como otra columna.
        let mut matrix = matrix();
        matrix.rows[0][0] = "El \"mejor\"".to_string();
        let text = csv(&matrix);

        assert!(text.contains("\"El \"\"mejor\"\"\""), "{text}");
    }

    #[test]
    fn un_valor_con_salto_de_linea_va_entre_comillas() {
        // Las descripciones tienen saltos. Sin comillas, el salto parte el CSV en dos
        // filas y la segunda no tiene cabecera: se ve como una fila corrupta a mitad de
        // archivo, no como un error del export.
        let mut matrix = matrix();
        matrix.rows[0][0] = "linea 1\nlinea 2".to_string();
        let text = csv(&matrix);

        assert!(text.contains("\"linea 1\nlinea 2\""), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "dos filas y un valor de dos lineas");
    }

    #[test]
    fn una_celda_vacia_es_un_campo_vacio_y_no_un_espacio() {
        // Una celda vacia en un CSV es `,`. Un espacio se ve igual en Excel y cambia el
        // valor: `""` y `" "` no son lo mismo para nadie que lea el archivo.
        let mut matrix = matrix();
        matrix.rows[0][1] = String::new();
        let text = csv(&matrix);

        assert!(text.contains("Norte,\n"), "{text}");
    }

    #[test]
    fn escribir_y_volver_a_leer_da_lo_mismo() {
        let path = std::env::temp_dir().join(format!(
            "datan-csv-test-{}.csv",
            std::process::id()
        ));
        let matrix = matrix();

        write_csv(&matrix, &path).expect("escribe");
        let text = std::fs::read_to_string(&path).expect("lee");
        let _ = std::fs::remove_file(&path);

        assert_eq!(text, csv(&matrix));
    }
}