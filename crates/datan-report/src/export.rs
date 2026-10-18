//! La exportacion del informe: CSV, Excel y PDF.
//!
//! Los tres salen de la MISMA matriz que pinta el informe, con las cabeceras en la primera
//! fila. Es el DATO de la consulta, no el formato de pantalla: un `format_string` de moneda
//! no cambia lo que viaja en el archivo, porque el que abre el Excel quiere el numero.
//!
//! El CSV conserva la decision de `1bd01af`: coma como separador, `\n` y sin BOM. Un BOM
//! rompe `awk` y las herramientas de linea, y la coma lo abre todo; el Excel de quien lo
//! necesite con `;` tiene el Excel mismo para convertirlo.
//!
//! Excel via `rust_xlsxwriter`: numeros como NUMEROS, cabecera en negrita, fila congelada y
//! ancho por columna. PDF escrito a mano con las fuentes base (Courier para la tabla,
//! Helvetica para el titulo): una tabla de informe es texto en columnas, y un generador de
//! PDF entero es dependencia que no hace falta. Horizontal cuando la tabla no cabe vertical.

use crate::Matrix;
use std::path::Path;

/// Un error de exportacion.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no se pudo escribir el archivo: {0}")]
    Io(#[from] std::io::Error),
    #[error("no se pudo armar el Excel: {0}")]
    Xlsx(String),
}

/// Un nombre de archivo que se puede escribir en cualquier disco: minusculas, sin acentos,
/// guiones donde habia cualquier otra cosa.
pub fn slug(texto: &str) -> String {
    let mut out = String::new();
    let mut ultimo_guion = true;
    for ch in texto.chars() {
        let plano = match ch {
            'a'..='z' | '0'..='9' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            '\u{E1}' => 'a',
            '\u{E9}' => 'e',
            '\u{ED}' => 'i',
            '\u{F3}' => 'o',
            '\u{FA}' => 'u',
            '\u{F1}' => 'n',
            ' ' | '-' | '_' => '-',
            _ => '-',
        };
        if plano == '-' && ultimo_guion {
            continue;
        }
        ultimo_guion = plano == '-';
        out.push(plano);
    }
    out.trim_matches('-').to_string()
}

// Cualquier otro caracter cae en `-`: en un nombre de archivo es lo que no rompe nada.

/// La ruta de un export: `~/Downloads/<slug>-<dia>.<ext>`.
///
/// Sin dialogo de guardado: el informe se llama solo, va a Descargas y la barra de estado
/// dice donde quedo.
pub fn ruta_de_export(base: &Path, nombre: &str, dia: &str, extension: &str) -> std::path::PathBuf {
    base.join(format!("{}-{}.{}", slug(nombre), slug(dia), extension))
}

/// El informe en CSV, con las cabeceras en la primera linea.
pub fn csv(matrix: &Matrix) -> String {
    let mut out = String::new();
    for column in 0..matrix.width() {
        if column > 0 {
            out.push(',');
        }
        out.push_str(&campo_csv(
            matrix.header(column).map_or("", |h| &h.label).to_string(),
        ));
    }
    out.push('\n');
    for row in 0..matrix.row_count() {
        for column in 0..matrix.width() {
            if column > 0 {
                out.push(',');
            }
            out.push_str(&campo_csv(matrix.cell(row, column).unwrap_or("").to_string()));
        }
        out.push('\n');
    }
    out
}

/// Un campo CSV, con el escape de RFC 4180: entre comillas si lleva coma, comilla o salto.
fn campo_csv(campo: String) -> String {
    if campo.contains(',') || campo.contains('"') || campo.contains('\n') || campo.contains('\r') {
        format!("\"{}\"", campo.replace('"', "\"\""))
    } else {
        campo
    }
}

/// Escribe la matriz a un archivo.
pub fn write_csv(matrix: &Matrix, path: &std::path::Path) -> Result<(), ExportError> {
    std::fs::write(path, csv(matrix)).map_err(ExportError::Io)
}

/// El informe en un Excel en memoria.
pub fn xlsx(matrix: &Matrix, nombre: &str) -> Result<Vec<u8>, ExportError> {
    // Un nombre de hoja admite 31 caracteres y ninguno de estos: [ ] : * ? / \.
    let hoja: String = nombre
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\'))
        .take(31)
        .collect();
    let hoja = if hoja.is_empty() { "informe" } else { &hoja };

    let mut book = rust_xlsxwriter::Workbook::new();
    let sheet = book.add_worksheet().set_name(hoja).map_err(|e| ExportError::Xlsx(e.to_string()))?;

    let negrita = rust_xlsxwriter::Format::new().set_bold();
    for column in 0..matrix.width() {
        let label = matrix.header(column).map_or("", |h| &h.label).to_string();
        sheet
            .write_with_format(0, column as u16, &label, &negrita)
            .map_err(|e| ExportError::Xlsx(e.to_string()))?;
    }

    for row in 0..matrix.row_count() {
        for column in 0..matrix.width() {
            let text = matrix.cell(row, column).unwrap_or("");
            // El numero como NUMERO: un "74000" de texto no suma ni ordena en el Excel, y
            // la celda queda marcada con el triangulo verde de "numero guardado como texto".
            if let Ok(value) = text.parse::<f64>() {
                sheet
                    .write(row as u32 + 1, column as u16, value)
                    .map_err(|e| ExportError::Xlsx(e.to_string()))?;
            } else if !text.is_empty() {
                sheet
                    .write(row as u32 + 1, column as u16, text)
                    .map_err(|e| ExportError::Xlsx(e.to_string()))?;
            }
        }
    }

    // La cabecera se queda arriba mientras se recorren las filas.
    sheet
        .set_freeze_panes(1, 0)
        .map_err(|e| ExportError::Xlsx(e.to_string()))?;
    // Un ancho por columna: el maximo de su contenido, con techo. Una columna de 60
    // caracteres tapa media hoja.
    for column in 0..matrix.width() {
        let mut ancho = matrix
            .header(column)
            .map_or(0, |h| h.label.len())
            .max(
                (0..matrix.row_count().min(200))
                    .filter_map(|row| matrix.cell(row, column))
                    .map(str::len)
                    .max()
                    .unwrap_or(0),
            )
            .min(40) as u16;
        if ancho < 8 {
            ancho = 8;
        }
        sheet.set_column_width(column as u16, ancho as f64 + 2.0)
            .map_err(|e| ExportError::Xlsx(e.to_string()))?;
    }

    book.save_to_buffer()
        .map_err(|e| ExportError::Xlsx(e.to_string()))
}

/// El informe en PDF, en memoria.
///
/// Con las 14 fuentes base de PDF no se embebe nada: Courier para la tabla (monoespaciada,
/// asi las columnas alinean con espacios) y Helvetica-Bold para el titulo. Horizontal si la
/// tabla no cabe vertical.
pub fn pdf(matrix: &Matrix, titulo: &str) -> Vec<u8> {
    let horizontal = matrix.width() > 6;
    let (page_w, page_h): (f64, f64) = if horizontal { (842.0, 595.0) } else { (595.0, 842.0) };
    let margen = 36.0;
    let ancho_util = page_w - 2.0 * margen;
    let columnas = matrix.width().max(1) as f64;
    let ancho_columna = ancho_util / columnas;
    // Courier: 0.6 de la altura de la fuente por caracter.
    let fuente = 7.5;
    let por_caracter = fuente * 0.6;
    let caracteres = ((ancho_columna / por_caracter) as usize).saturating_sub(2).max(3);
    let renglon = 11.0;
    let alto_tabla = page_h - 2.0 * margen - 46.0;
    let filas_por_pagina = ((alto_tabla / renglon) as usize).max(1);

    let mut filas: Vec<Vec<String>> = Vec::with_capacity(matrix.row_count() + 1);
    filas.push(
        (0..matrix.width())
            .map(|c| matrix.header(c).map_or(String::new(), |h| h.label.clone()))
            .collect(),
    );
    for row in 0..matrix.row_count() {
        filas.push(
            (0..matrix.width())
                .map(|c| matrix.cell(row, c).unwrap_or("").to_string())
                .collect(),
        );
    }

    // El titulo y la linea de contexto van solo en la primera pagina.
    let mut first = true;
    let mut paginas: Vec<String> = Vec::new();

    for lote in filas.chunks(filas_por_pagina) {
        let mut page = String::new();
        let mut y = page_h - margen - 16.0;
        if first {
            page.push_str(&texto(
                "Helvetica-Bold",
                13.0,
                margen,
                y,
                &latin1(titulo),
            ));
            page.push_str(&texto(
                "Helvetica",
                8.0,
                margen,
                y - 14.0,
                &format!(
                    "{} fila(s) x {} columna(s)",
                    matrix.row_count(),
                    matrix.width()
                ),
            ));
            y -= 34.0;
            first = false;
        } else {
            y -= 6.0;
        }

        for (index, fila) in lote.iter().enumerate() {
            let es_cabecera = index == 0;
            if es_cabecera {
                page.push_str(&texto(
                    "Courier-Bold",
                    fuente,
                    margen,
                    y,
                    &celda(fila, 0, caracteres),
                ));
            } else {
                page.push_str(&texto(
                    "Courier",
                    fuente,
                    margen,
                    y,
                    &celda(fila, 0, caracteres),
                ));
            }
            for (column, _) in fila.iter().enumerate().skip(1) {
                let x = margen + ancho_columna * column as f64;
                page.push_str(&texto(
                    if es_cabecera { "Courier-Bold" } else { "Courier" },
                    fuente,
                    x,
                    y,
                    &celda(fila, column, caracteres),
                ));
            }
            // Un renglon bajo la cabecera y lineas suaves cada fila: una tabla sin lineas no
            // se lee cuando una celda queda vacia.
            page.push_str(&format!(
                "0.5 w 0.85 G {margen} {} m {} {} l S 0 G\n",
                y - 3.0,
                page_w - margen,
                y - 3.0
            ));
            y -= renglon;
        }
        paginas.push(page);
    }

    ensamblar_pdf(&paginas, page_w, page_h)
}

/// El texto de una celda: recortado al ancho de su columna.
fn celda(fila: &[String], column: usize, caracteres: usize) -> String {
    fila.get(column)
        .map(|c| {
            if c.chars().count() > caracteres {
                c.chars().take(caracteres.saturating_sub(1)).collect::<String>()
                    + "\u{2026}"
            } else {
                c.clone()
            }
        })
        .unwrap_or_default()
}

/// Una orden de texto en el flujo de contenido.
fn texto(fuente: &str, tamano: f64, x: f64, y: f64, contenido: &str) -> String {
    format!(
        "BT /{fuente} {tamano} Tf 1 0 0 1 {x} {y} Tm ({}) Tj ET\n",
        escapar(contenido)
    )
}

/// Escape de una cadena literal de PDF: `\`, `(` y `)`.
fn escapar(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len());
    for ch in texto.chars() {
        if matches!(ch, '(' | ')' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// UTF-8 a los bytes de WinAnsi (cada char bajo 256 es su byte; el euro es 0x80). Lo que no
/// tiene byte en la pagina de codigos cae en `?`.
fn latin1(texto: &str) -> String {
    texto
        .chars()
        .map(|ch| match ch as u32 {
            0x20AC => '\u{80}',
            c @ 0x20..=0xFF => char::from_u32(c).unwrap_or('?'),
            _ => '?',
        })
        .collect()
}

/// Los objetos del PDF: catalogo, paginas, tres fuentes y un par de objetos por pagina, con
/// su tabla xref de offsets.
fn ensamblar_pdf(paginas: &[String], page_w: f64, page_h: f64) -> Vec<u8> {
    let count = paginas.len().max(1);
    let mut objetos: Vec<String> = Vec::with_capacity(5 + count * 2);

    objetos.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let hijos: Vec<String> = (0..count)
        .map(|p| format!("{} 0 R", 5 + p * 2))
        .collect();
    objetos.push(format!(
        "<< /Type /Pages /Count {count} /Kids [{}] >>",
        hijos.join(" ")
    ));
    objetos.push(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>"
            .to_string(),
    );
    objetos.push(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Bold /Encoding /WinAnsiEncoding >>"
            .to_string(),
    );
    objetos.push(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_string(),
    );
    // Helvetica regular, para la linea de contexto.
    objetos.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
    for pagina in paginas {
        let indice = objetos.len() as u32 + 1;
        objetos.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_w} {page_h}] /Resources << /Font << /Courier 3 0 R /Courier-Bold 4 0 R /Helvetica-Bold 5 0 R /Helvetica 6 0 R >> >> /Contents {indice} 0 R >>"
        ));
        objetos.push(format!("<< /Length {} >>\nstream\n{}\nendstream", pagina.len(), pagina));
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets: Vec<usize> = Vec::with_capacity(objetos.len());
    for (index, objeto) in objetos.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, objeto).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objetos.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objetos.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Header, Matrix};
    use datan_model::{DataType, ObjectId};

    fn matriz() -> Matrix {
        Matrix {
            headers: vec![
                Header {
                    label: "dim_product[region]".to_string(),
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
                vec!["Sur,Este".to_string(), String::new()],
                vec!["cantidad \"rara\"".to_string(), "-1.5".to_string()],
            ],
            truncated: false,
        }
    }

    #[test]
    fn el_csv_lleva_coma_y_comillas_cuando_toca() {
        let texto = csv(&matriz());
        assert!(texto.starts_with("dim_product[region],Qty\n"));
        assert!(texto.contains("\"Sur,Este\",\n"), "un campo con coma va entre comillas");
        assert!(texto.contains("\"cantidad \"\"rara\"\"\",-1.5\n"));
    }

    #[test]
    fn el_excel_arranca_como_zip_y_no_explota_con_nombres_largos() {
        let bytes = xlsx(&matriz(), "informe de regiones").expect("xlsx");
        assert!(bytes.starts_with(b"PK"), "un xlsx es un zip");
        // El nombre de hoja se recorta a 31 y suelta los caracteres prohibidos.
        let largo = xlsx(&matriz(), &"x".repeat(60)).expect("xlsx largo");
        assert!(largo.starts_with(b"PK"));
    }

    #[test]
    fn el_pdf_es_un_pdf_valido_de_una_pagina() {
        let bytes = pdf(&matriz(), "Informe de regiones");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        assert!(bytes.ends_with(b"%%EOF\n"));

        let texto = String::from_utf8_lossy(&bytes);
        // El titulo escapado esta dentro, y el startxref apunta de verdad a la tabla xref.
        assert!(texto.contains("(Informe de regiones) Tj"));
        let offset: usize = texto
            .rsplit("startxref\n")
            .next()
            .and_then(|cola| cola.lines().next())
            .and_then(|n| n.parse().ok())
            .expect("startxref con offset");
        assert_eq!(&bytes[offset..offset + 4], b"xref");
        assert!(texto.contains("/Count 1"));
    }

    #[test]
    fn una_tabla_ancha_va_horizontal_y_en_varias_paginas() {
        let mut matrix = matriz();
        // Diez columnas: mas de seis, horizontal.
        for index in 0..8 {
            matrix.headers.push(Header {
                label: format!("col{index}"),
                column: ObjectId(0),
                measure: Some(ObjectId(100)),
                data_type: DataType::Decimal,
            });
        }
        for row in &mut matrix.rows {
            row.extend((0..8).map(|index| format!("{index}")));
        }
        // Ochenta filas: no caben en una pagina horizontal.
        let primera = matrix.rows[0].clone();
        for index in 0..80 {
            let mut fila = primera.clone();
            fila[0] = format!("region {index}");
            matrix.rows.push(fila);
        }

        let bytes = pdf(&matrix, "ancho");
        let texto = String::from_utf8_lossy(&bytes);
        assert!(
            texto.contains("/MediaBox [0 0 842 595]"),
            "horizontal cuando hay muchas columnas"
        );
        assert!(
            texto.contains("/Count 2"),
            "ochenta filas no caben en una pagina"
        );
    }

    #[test]
    fn el_slug_no_deja_nada_que_un_disco_rechace() {
        assert_eq!(slug("Informe de Regi\u{F3}n Norte"), "informe-de-region-norte");
        assert_eq!(slug("a/b\\c:d*e?f\"g[h]"), "a-b-c-d-e-f-g-h");
        assert_eq!(slug("  --x  "), "x");
    }
}
