//! El formato de los numeros de una medida.
//!
//! `format_string` es texto de Power BI y de .NET: `#,##0.00`, `0.0%`, `Currency`, `N2`,
//! `C0`, `P1`. No se implementa el lenguaje entero: el subconjunto que cubre lo que una
//! medida de negocio escribe. Un formato que no se reconoce deja el numero como esta, que es
//! mejor que un numero inventado.
//!
//! La salida es en espanol: punto para los miles y coma para los decimales, que es como se
//! leen los numeros en la interfaz. Solo se usa para MOSTRAR: el dato que viaja (el clic que
//! filtra, la pastilla) sigue siendo el texto crudo del motor.

/// El formato de una medida, ya reconocido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formato {
    /// Miles y decimales fijos: `#,##0`, `#,##0.00`, `N2`.
    Numero { miles: bool, decimales: usize },
    /// El valor se multiplica por 100 y lleva `%` pegado: `0%`, `0.0%`, `P1`.
    Porcentaje { decimales: usize },
    /// Un simbolo delante y miles: `Currency`, `C2`, `EUR #,##0`.
    Moneda { simbolo: String, decimales: usize },
}

impl Formato {
    /// Reconoce un `format_string`. Devuelve `None` cuando no sabe que es.
    pub fn parse(texto: &str) -> Option<Formato> {
        let texto = texto.trim();
        if texto.is_empty() {
            return None;
        }
        let lower = texto.to_ascii_lowercase();

        // Los nombres de .NET que Power BI tambien acepta.
        if lower == "currency" {
            return Some(Formato::Moneda {
                simbolo: "\u{20AC}".to_string(),
                decimales: 2,
            });
        }
        if let Some(decimales) = con_sufijo(&lower, "c") {
            return Some(Formato::Moneda {
                simbolo: "\u{20AC}".to_string(),
                decimales,
            });
        }
        if let Some(decimales) = con_sufijo(&lower, "p") {
            return Some(Formato::Porcentaje { decimales });
        }
        if let Some(decimales) = con_sufijo(&lower, "n") {
            return Some(Formato::Numero {
                miles: true,
                decimales,
            });
        }

        // Los patrones con `#` y `0`. `%` al final es porcentaje; un simbolo de moneda
        // delante es moneda; lo demas es numero.
        let es_porcentaje = lower.ends_with('%');
        let mut cuerpo = lower.trim_end_matches('%');
        let mut simbolo = None;
        for moneda in ["\u{20AC}", "eur ", "$"] {
            if let Some(resto) = cuerpo.strip_prefix(moneda) {
                simbolo = Some(moneda.trim_end().to_string());
                cuerpo = resto;
                break;
            }
        }
        let valido = cuerpo
            .chars()
            .all(|c| c.is_ascii_digit() || c == '#' || c == ',' || c == '.')
            && cuerpo.chars().any(|c| c.is_ascii_digit());
        if !valido {
            return None;
        }
        let decimales = cuerpo.split_once('.').map_or(0, |(_, d)| d.len());
        let miles = cuerpo.contains(',');
        let resultado = match (es_porcentaje, simbolo) {
            (true, _) => Formato::Porcentaje { decimales },
            (false, Some(simbolo)) => Formato::Moneda { simbolo, decimales },
            (false, None) => Formato::Numero { miles, decimales },
        };
        Some(resultado)
    }

    /// El numero, en el formato.
    pub fn aplicar(&self, value: f64) -> String {
        match self {
            Formato::Numero { miles, decimales } => numero(value, *miles, *decimales),
            Formato::Porcentaje { decimales } => {
                format!("{}%", numero(value * 100.0, true, *decimales))
            }
            Formato::Moneda { simbolo, decimales } => {
                format!("{simbolo}{}", numero(value, true, *decimales))
            }
        }
    }
}

/// `n2` -> `2`, `p0` -> `0`. Una letra y un digito de decimales.
fn con_sufijo(texto: &str, letra: &str) -> Option<usize> {
    texto
        .strip_prefix(letra)?
        .parse::<usize>()
        .ok()
        .filter(|n| *n <= 6)
}

/// Un numero con punto de miles y coma decimal: `1.234,5`.
fn numero(value: f64, miles: bool, decimales: usize) -> String {
    if !value.is_finite() {
        return String::new();
    }
    let texto = format!("{value:.*}", decimales);
    let (signo, resto) = texto
        .strip_prefix('-')
        .map_or(("", texto.as_str()), |resto| ("-", resto));
    let (entera, decimal) = resto.split_once('.').unwrap_or((resto, ""));
    let entera = if miles {
        con_puntos(entera)
    } else {
        entera.to_string()
    };
    if decimal.is_empty() {
        format!("{signo}{entera}")
    } else {
        format!("{signo}{entera},{decimal}")
    }
}

/// Los puntos de miles: `1234567` -> `1.234.567`.
pub(crate) fn con_puntos(digits: &str) -> String {
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_patrones_que_la_gente_escribe() {
        assert_eq!(
            Formato::parse("#,##0.00"),
            Some(Formato::Numero {
                miles: true,
                decimales: 2
            })
        );
        assert_eq!(
            Formato::parse("#,##0"),
            Some(Formato::Numero {
                miles: true,
                decimales: 0
            })
        );
        assert_eq!(
            Formato::parse("0.0"),
            Some(Formato::Numero {
                miles: false,
                decimales: 1
            })
        );
        assert_eq!(
            Formato::parse("0.0%"),
            Some(Formato::Porcentaje { decimales: 1 })
        );
        assert_eq!(
            Formato::parse("Currency"),
            Some(Formato::Moneda {
                simbolo: "\u{20AC}".to_string(),
                decimales: 2
            })
        );
        assert_eq!(
            Formato::parse("N2"),
            Some(Formato::Numero {
                miles: true,
                decimales: 2
            })
        );
    }

    #[test]
    fn un_formato_desconocido_no_es_ninguno() {
        assert_eq!(Formato::parse("dd/MM/yyyy"), None);
        assert_eq!(Formato::parse(""), None);
        assert_eq!(Formato::parse("Texto"), None);
    }

    #[test]
    fn los_numeros_salen_como_se_leen_en_espanol() {
        let formato = Formato::parse("#,##0.00").expect("formato");
        assert_eq!(formato.aplicar(1234567.891), "1.234.567,89");
        assert_eq!(formato.aplicar(-1234.5), "-1.234,50");

        let entero = Formato::parse("#,##0").expect("formato");
        assert_eq!(entero.aplicar(74000.0), "74.000");

        let porcentaje = Formato::parse("0.0%").expect("formato");
        assert_eq!(porcentaje.aplicar(0.1234), "12,3%");

        let moneda = Formato::parse("Currency").expect("formato");
        assert_eq!(moneda.aplicar(13.1), "\u{20AC}13,10");
    }
}
