//! Parser de DAX. Recursivo descendente sobre el flujo de tokens.
//!
//! La precedencia de DAX no coincide con la de SQL. El orden aqui es el de la
//! especificacion de dax.guide: `^` es el mas fuerte, luego `* /`, luego `+ - &`, luego las
//! comparaciones, luego `&&`, luego `||`.

use crate::ast::{BinaryOp, Expr, TableExpr, UnaryOp, VarBinding};
use crate::lexer::{Token, TokenKind};
use crate::Span;

/// Error de sintaxis, con la posicion donde fallo.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} en offset {}", self.message, self.span.start)
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

/// Parsea una expresion completa. El token `Eof` es obligatorio al final.
pub fn parse_expression(tokens: &[Token]) -> Result<Expr> {
    if tokens.is_empty() {
        return Err(ParseError {
            message: "entrada vacia".to_string(),
            span: Span { start: 0, end: 0 },
        });
    }
    let mut parser = Parser { tokens, position: 0 };
    let expr = parser.parse_or()?;
    parser.expect_eof()?;
    Ok(expr)
}

struct Parser<'a> {
    tokens: &'a [Token],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.position.min(self.tokens.len() - 1)]
    }

    fn peek_kind(&self) -> TokenKind {
        self.peek().kind
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.position < self.tokens.len() - 1 {
            self.position += 1;
        }
        token
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.peek_kind() == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_operator(&mut self, operator: &str) -> bool {
        if self.peek().kind == TokenKind::Operator && self.peek().text == operator {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Result<Token> {
        if self.peek_kind() == kind {
            Ok(self.advance())
        } else {
            Err(ParseError {
                message: format!("se esperaba {what}"),
                span: self.peek().span,
            })
        }
    }

    fn expect_eof(&mut self) -> Result<()> {
        if self.peek_kind() == TokenKind::Eof {
            Ok(())
        } else {
            Err(ParseError {
                message: format!("token inesperado: {:?}", self.peek().text),
                span: self.peek().span,
            })
        }
    }

    /// `||`, el mas debil.
    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.eat_operator("||") {
            let right = self.parse_and()?;
            let span = join(left.span(), right.span());
            left = Expr::Binary {
                op: BinaryOp::Or,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_comparison()?;
        while self.eat_operator("&&") {
            let right = self.parse_comparison()?;
            let span = join(left.span(), right.span());
            left = Expr::Binary {
                op: BinaryOp::And,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek().text.as_str() {
                "=" => BinaryOp::Equal,
                "<>" => BinaryOp::NotEqual,
                "<" => BinaryOp::Less,
                "<=" => BinaryOp::LessEqual,
                ">" => BinaryOp::Greater,
                ">=" => BinaryOp::GreaterEqual,
                _ => break,
            };
            if self.peek().kind != TokenKind::Operator {
                break;
            }
            self.advance();
            let right = self.parse_additive()?;
            let span = join(left.span(), right.span());
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = if self.peek().kind != TokenKind::Operator {
                break;
            } else {
                match self.peek().text.as_str() {
                    "+" => BinaryOp::Add,
                    "-" => BinaryOp::Subtract,
                    "&" => BinaryOp::Concat,
                    _ => break,
                }
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            let span = join(left.span(), right.span());
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr> {
        let mut left = self.parse_power()?;
        loop {
            let op = if self.peek().kind != TokenKind::Operator {
                break;
            } else {
                match self.peek().text.as_str() {
                    "*" => BinaryOp::Multiply,
                    "/" => BinaryOp::Divide,
                    _ => break,
                }
            };
            self.advance();
            let right = self.parse_power()?;
            let span = join(left.span(), right.span());
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_power(&mut self) -> Result<Expr> {
        let base = self.parse_unary()?;
        if self.eat_operator("^") {
            // `^` es asociativo por la derecha: `2^3^2` es `2^(3^2)`.
            let exponent = self.parse_power()?;
            let span = join(base.span(), exponent.span());
            return Ok(Expr::Binary {
                op: BinaryOp::Power,
                left: Box::new(base),
                right: Box::new(exponent),
                span,
            });
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if self.peek().kind == TokenKind::Operator {
            let operator = self.peek().text.clone();
            if operator == "-" {
                let start = self.advance().span;
                let operand = self.parse_unary()?;
                let span = join(start, operand.span());
                return Ok(Expr::Unary {
                    op: UnaryOp::Negate,
                    operand: Box::new(operand),
                    span,
                });
            }
            if operator == "+" {
                self.advance();
                return self.parse_unary();
            }
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let token = self.peek().clone();

        match token.kind {
            TokenKind::Number => {
                self.advance();
                Ok(Expr::Number {
                    value: token.number.unwrap_or(0.0),
                    span: token.span,
                })
            }
            TokenKind::Text => {
                self.advance();
                Ok(Expr::Text {
                    value: token.text,
                    span: token.span,
                })
            }
            TokenKind::QuotedName => {
                // Nombre de tabla sin columna. Solo tiene sentido como tabla, pero el parser
                // no juzga: lo resuelve el chequeo contra el modelo.
                self.advance();
                if self.peek_kind() == TokenKind::LBracket {
                    self.advance();
                    // DAX admite las dos formas: `'Fact Sales'[Qty]` y
                    // `'Fact Sales'['Qty']`. El nombre de columna puede venir sin comillas.
                    let column_name = match self.peek_kind() {
                        TokenKind::QuotedName | TokenKind::Identifier => self.advance().text,
                        _ => {
                            return Err(ParseError {
                                message: "se esperaba un nombre de columna".to_string(),
                                span: self.peek().span,
                            });
                        }
                    };
                    let close = self.expect(TokenKind::RBracket, "']'")?;
                    Ok(Expr::QualifiedReference {
                        table: token.text,
                        column: column_name,
                        span: join(token.span, close.span),
                    })
                } else {
                    Ok(Expr::Reference {
                        name: token.text,
                        span: token.span,
                    })
                }
            }
            TokenKind::LBracket => {
                // `[Columna]` o `[Medida]`. Sin comillas.
                self.advance();
                let mut name = String::new();
                while self.peek_kind() != TokenKind::RBracket {
                    if self.peek_kind() == TokenKind::Eof {
                        return Err(ParseError {
                            message: "']' sin cerrar".to_string(),
                            span: token.span,
                        });
                    }
                    name.push_str(&self.advance().text);
                }
                let close = self.advance();
                Ok(Expr::Reference {
                    name,
                    span: join(token.span, close.span),
                })
            }
            TokenKind::LParen => {
                // Agrupacion explicita. El `Display` del AST no emite parentesis porque la
                // precedencia ya reconstruye el arbol, pero el usuario si los escribe.
                self.advance();
                let inner = self.parse_or()?;
                self.expect(TokenKind::RParen, "')'")?;
                Ok(inner)
            }
            TokenKind::LBrace => {
                self.advance();
                let mut values = Vec::new();
                while !self.eat(TokenKind::RBrace) {
                    if self.peek_kind() == TokenKind::Eof {
                        return Err(ParseError {
                            message: "'}' sin cerrar".to_string(),
                            span: token.span,
                        });
                    }
                    values.push(self.parse_or()?);
                    if !self.eat(TokenKind::Comma) {
                        self.expect(TokenKind::RBrace, "'}'")?;
                        break;
                    }
                }
                Ok(Expr::Table(TableExpr::Literal {
                    values,
                    span: token.span,
                }))
            }
            TokenKind::Identifier => {
                // Puede abrir un bloque VAR o ser una llamada a funcion.
                if token.is_keyword("var") {
                    return self.parse_block();
                }
                self.advance();
                if self.peek_kind() == TokenKind::LParen {
                    return self.parse_call(token);
                }
                // Nombre sin parentesis: una medida referenciada.
                Ok(Expr::Reference {
                    name: token.text,
                    span: token.span,
                })
            }
            _ => Err(ParseError {
                message: format!("token inesperado: {:?}", token.text),
                span: token.span,
            }),
        }
    }

    fn parse_call(&mut self, name_token: Token) -> Result<Expr> {
        self.expect(TokenKind::LParen, "'('")?;
        let mut args = Vec::new();
        let mut last = name_token.span;
        while self.peek_kind() != TokenKind::RParen {
            if self.peek_kind() == TokenKind::Eof {
                return Err(ParseError {
                    message: format!("'(' sin cerrar en {}", name_token.text),
                    span: name_token.span,
                });
            }
            args.push(self.parse_or()?);
            last = self.peek().span;
            if self.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        // El span cubre la llamada completa, nombre y parentesis de cierre. Cubrir solo el
        // nombre romperia el resaltado de errores y el go-to-definition sobre el cuerpo.
        let close = self.expect(TokenKind::RParen, "')'")?;
        last = join(last, close.span);
        Ok(Expr::Call {
            name: name_token.text.to_uppercase(),
            args,
            span: join(name_token.span, last),
        })
    }

    /// `VAR a = 1 VAR b = 2 RETURN expr`.
    fn parse_block(&mut self) -> Result<Expr> {
        let start = self.advance().span; // VAR
        let mut vars = Vec::new();

        // El primer VAR ya se consumio. El bucle parsea esa binding y despues busca mas
        // palabras clave VAR.
        loop {
            let var_span = self.peek().span;
            let name = match self.peek_kind() {
                TokenKind::Identifier | TokenKind::QuotedName => self.advance().text,
                TokenKind::LBracket => {
                    self.advance();
                    let mut name = String::new();
                    while self.peek_kind() != TokenKind::RBracket {
                        if self.peek_kind() == TokenKind::Eof {
                            return Err(ParseError {
                                message: "']' sin cerrar en nombre de variable".to_string(),
                                span: var_span,
                            });
                        }
                        name.push_str(&self.advance().text);
                    }
                    self.advance();
                    name
                }
                _ => {
                    return Err(ParseError {
                        message: "se esperaba un nombre de variable".to_string(),
                        span: self.peek().span,
                    })
                }
            };
            if !self.eat_operator("=") {
                return Err(ParseError {
                    message: format!("se esperaba '=' tras la variable {name}"),
                    span: self.peek().span,
                });
            }
            let value = self.parse_or()?;
            let span = join(var_span, value.span());
            vars.push(VarBinding { name, value, span });

            if !self.peek().is_keyword("var") {
                break;
            }
            self.advance();
        }

        if vars.is_empty() {
            return Err(ParseError {
                message: "VAR sin ninguna binding".to_string(),
                span: start,
            });
        }

        if !self.peek().is_keyword("return") {
            return Err(ParseError {
                message: "se esperaba RETURN".to_string(),
                span: self.peek().span,
            });
        }
        self.advance();
        let body = self.parse_or()?;
        let span = join(start, body.span());
        Ok(Expr::Block {
            vars,
            body: Box::new(body),
            span,
        })
    }
}

fn join(left: Span, right: Span) -> Span {
    Span {
        start: left.start.min(right.start),
        end: left.end.max(right.end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lexer;

    fn parse(source: &str) -> Expr {
        parse_expression(&Lexer::new(source).tokenize()).unwrap_or_else(|e| panic!("{source}: {e}"))
    }

    #[test]
    fn parsea_una_agregacion_simple() {
        assert_eq!(
            parse("SUM('Fact Sales'[Qty])"),
            Expr::Call {
                name: "SUM".to_string(),
                args: vec![Expr::QualifiedReference {
                    table: "Fact Sales".to_string(),
                    column: "Qty".to_string(),
                    span: Span { start: 4, end: 21 },
                }],
                span: Span { start: 0, end: 22 },
            }
        );
    }

    #[test]
    fn parsea_precedencia_de_operadores() {
        // `*` liga mas fuerte que `+`: `1 + 2 * 3` es `1 + (2 * 3)`.
        let expr = parse("1 + 2 * 3");
        let Expr::Binary { op, left, right, .. } = expr else {
            panic!("se esperaba un binario")
        };
        assert_eq!(op, BinaryOp::Add);
        assert!(matches!(*right, Expr::Binary { op: BinaryOp::Multiply, .. }));
    }

    #[test]
    fn la_potencia_es_associativa_por_la_derecha() {
        // `2^3^2` es `2^(3^2)`.
        let expr = parse("2^3^2");
        let Expr::Binary { right, .. } = expr else {
            panic!("se esperaba un binario")
        };
        assert!(matches!(*right, Expr::Binary { op: BinaryOp::Power, .. }));
    }

    #[test]
    fn parsea_bloques_var() {
        let expr = parse("VAR x = 10 RETURN [Qty] * x");
        let Expr::Block { vars, body, .. } = expr else {
            panic!("se esperaba un bloque")
        };
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "x");
        assert!(matches!(*body, Expr::Binary { op: BinaryOp::Multiply, .. }));
    }

    #[test]
    fn parsea_comparaciones_encadenadas_no() {
        // Solo una comparacion por nivel: `a = b` primero, despues `<`.
        let expr = parse("[Qty] = 1 < 2");
        let Expr::Binary { op, .. } = expr else {
            panic!("se esperaba un binario")
        };
        assert_eq!(op, BinaryOp::Less);
    }

    /// Compara dos AST ignorando los spans.
    ///
    /// El round-trip por `Display` cambia las posiciones: `[x]` ocupa tres caracteres donde
    /// `x` ocupa uno. La propiedad que importa es que la *estructura* se conserva, que es lo
    /// que hace que el export a TMDL y el pretty-printer sean correctos.
    fn strip_spans(expr: &Expr) -> Expr {
        let zero = Span { start: 0, end: 0 };
        match expr {
            Expr::Number { value, .. } => Expr::Number { value: *value, span: zero },
            Expr::Text { value, .. } => Expr::Text { value: value.clone(), span: zero },
            Expr::Boolean { value, .. } => Expr::Boolean { value: *value, span: zero },
            Expr::Blank { .. } => Expr::Blank { span: zero },
            Expr::Reference { name, .. } => Expr::Reference { name: name.clone(), span: zero },
            Expr::QualifiedReference { table, column, .. } => Expr::QualifiedReference {
                table: table.clone(),
                column: column.clone(),
                span: zero,
            },
            Expr::Call { name, args, .. } => Expr::Call {
                name: name.clone(),
                args: args.iter().map(strip_spans).collect(),
                span: zero,
            },
            Expr::Binary { op, left, right, .. } => Expr::Binary {
                op: *op,
                left: Box::new(strip_spans(left)),
                right: Box::new(strip_spans(right)),
                span: zero,
            },
            Expr::Unary { op, operand, .. } => Expr::Unary {
                op: *op,
                operand: Box::new(strip_spans(operand)),
                span: zero,
            },
            Expr::Block { vars, body, .. } => Expr::Block {
                vars: vars
                    .iter()
                    .map(|v| VarBinding {
                        name: v.name.clone(),
                        value: strip_spans(&v.value),
                        span: zero,
                    })
                    .collect(),
                body: Box::new(strip_spans(body)),
                span: zero,
            },
            Expr::Table(table) => Expr::Table(match table {
                TableExpr::Row { columns, values, .. } => TableExpr::Row {
                    columns: columns.clone(),
                    values: values.iter().map(strip_spans).collect(),
                    span: zero,
                },
                TableExpr::Literal { values, .. } => TableExpr::Literal {
                    values: values.iter().map(strip_spans).collect(),
                    span: zero,
                },
                TableExpr::Call { name, args, .. } => TableExpr::Call {
                    name: name.clone(),
                    args: args.iter().map(strip_spans).collect(),
                    span: zero,
                },
            }),
        }
    }

    #[test]
    fn round_trip_por_display() {
        // `parse(x.to_string())` debe dar la misma estructura que `parse(x)`. Es la base del
        // export a TMDL y del pretty-printer.
        let sources = [
            "SUM('Fact Sales'[Qty])",
            "CALCULATE([Revenue], ALL('Dim Date'[Year]))",
            "VAR x = 10 RETURN [Qty] * x",
            "VAR a = 1 VAR b = 2 RETURN a + b",
            "IF([Qty] > 1, 1, 0)",
            "SUMX(VALUES('Dim Product'[Category]), CALCULATE([Revenue]))",
            "[Revenue] - [COGS]",
            "COALESCE([Qty], 0)",
            "SUM('Fact Sales'[Qty]) * 100 / 12",
            "DIVIDE([Revenue], [COGS])",
        ];
        for source in sources {
            let once = strip_spans(&parse(source));
            let twice = strip_spans(&parse(&once.to_string()));
            assert_eq!(once, twice, "round-trip fallo para {source}");
        }
    }

    #[test]
    fn los_spans_apuntan_a_la_expresion_original() {
        let source = "CALCULATE([Revenue], ALL('Dim Date'[Year]))";
        let expr = parse(source);
        assert_eq!(expr.span().start, 0);
        let Expr::Call { args, .. } = &expr else {
            panic!("se esperaba una llamada")
        };
        assert_eq!(args.len(), 2);
        assert_eq!(
            &source[args[0].span().start as usize..args[0].span().end as usize],
            "[Revenue]"
        );
        assert_eq!(
            &source[args[1].span().start as usize..args[1].span().end as usize],
            "ALL('Dim Date'[Year])"
        );
    }

    #[test]
    fn reporta_la_posicion_del_error() {
        let tokens = Lexer::new("SUM(1 +").tokenize();
        let error = parse_expression(&tokens).unwrap_err();
        assert_eq!(error.span.start, 7);
    }
}