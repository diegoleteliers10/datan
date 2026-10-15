//! Diagnosticos del linter.
//!
//! Un error fatal invalida la expresion y vive en `CheckError`. Un diagnostico no la
//! invalida: la expresion sigue siendo evaluable, pero el autor deberia saber algo.
//!
//! Power BI no tiene ninguno de los dos. Su barra de formulas acepta `Revenue =
//! Fact Sales[Qty]` y falla en tiempo de ejecucion con un error opaco. Ese es el problema
//! que la apuesta 3 del plan ataca.

use datan_dax_syntax::Span;

/// Un problema encontrado, con su posicion y su severidad.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Codigo estable, para silenciar reglas y para enlazar documentacion.
    pub code: &'static str,
    pub message: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Diagnostic {
    pub fn error(span: Span, code: &'static str, message: String) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message,
            span,
        }
    }

    pub fn warning(span: Span, code: &'static str, message: String) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            message,
            span,
        }
    }

    pub fn info(span: Span, code: &'static str, message: String) -> Self {
        Self {
            severity: Severity::Info,
            code,
            message,
            span,
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "aviso",
            Severity::Info => "info",
        };
        write!(
            f,
            "{severity}[{}] en {}: {}",
            self.code, self.span.start, self.message
        )
    }
}