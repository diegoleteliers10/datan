//! Front-end de DAX: tokens, spans, lexer, parser.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. Arranca vacio. Llega en la Fase 2.

/// Posicion de un token en el texto fuente.
///
/// Los spans se guardan en todas partes. El linter, el go-to-definition y el diff de
/// una medida dependen de ellos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}