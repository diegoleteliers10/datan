//! Front-end de DAX: tokens, spans, lexer, AST y parser.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. `cargo test` corre en milisegundos.
//!
//! Esto es el primer crate del grupo motor, y el unico cuya salida tiene que ser
//! inspeccionable: el linter, el go-to-definition y el diff de una medida dependen del AST.
//! Por eso todo nodo lleva `Span`.

mod ast;
mod lexer;
mod parser;

pub use ast::{BinaryOp, Expr, Modifier, TableExpr, UnaryOp, VarBinding};
pub use lexer::{Lexer, Token, TokenKind};
pub use parser::{ParseError, parse_expression};

/// Posicion de un token en el texto fuente.
///
/// Los spans se guardan en todas partes. El linter, el go-to-definition y el diff de una
/// medida dependen de ellos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// Parsea una expresion de medida DAX.
///
/// Devuelve el AST con spans, o el primer error con su posicion. No hace resolucion de
/// nombres ni typecheck: eso vive en `datan-dax-check`, que necesita el modelo.
pub fn parse(source: &str) -> Result<Expr, ParseError> {
    let tokens = Lexer::new(source).tokenize();
    parser::parse_expression(&tokens)
}
