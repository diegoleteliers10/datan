//! Lexer de DAX.
//!
//! El caso dificil de DAX es que los nombres de tabla y columna van entre comillas
//! simples, y los identificadores sin calificar pueden ser medidas o columnas segun el
//! modelo. El lexer no decide eso: solo produce tokens con su posicion.

use crate::Span;

/// Tipo de token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Numero, con su valor ya parseado.
    Number,
    /// String entre comillas dobles.
    Text,
    /// Nombre de tabla o columna entre comillas simples.
    QuotedName,
    /// Identificador sin calificar.
    Identifier,
    /// `(` `)` `[` `]` `,` `:` `{` `}`
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    LBrace,
    RBrace,
    /// Un operador de dos o mas caracteres.
    Operator,
    /// Fin de la entrada.
    Eof,
}

/// Un token con su posicion en el texto fuente.
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// Texto crudo, sin comillas. Para `Operator` es el operador completo.
    pub text: String,
    /// Valor numerico, solo para `TokenKind::Number`.
    pub number: Option<f64>,
}

impl Token {
    pub fn is_keyword(&self, keyword: &str) -> bool {
        self.kind == TokenKind::Identifier && self.text.eq_ignore_ascii_case(keyword)
    }
}

/// Convierte texto DAX en tokens.
pub struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            position: 0,
        }
    }

    pub fn tokenize(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        while let Some(token) = self.next_token() {
            let is_eof = token.kind == TokenKind::Eof;
            tokens.push(token);
            if is_eof {
                break;
            }
        }
        tokens
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.position + offset).copied()
    }

    fn skip_trivia(&mut self) {
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() {
                self.position += 1;
            } else if byte == b'/' && self.peek_at(1) == Some(b'/') {
                // Comentario de linea. DAX tiene `//` y `/* */`, ambos hasta fin de linea.
                while self.peek().is_some_and(|b| b != b'\n') {
                    self.position += 1;
                }
            } else if byte == b'/' && self.peek_at(1) == Some(b'*') {
                self.position += 2;
                while self.peek().is_some() {
                    if self.peek() == Some(b'*') && self.peek_at(1) == Some(b'/') {
                        self.position += 2;
                        break;
                    }
                    self.position += 1;
                }
            } else {
                break;
            }
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        self.skip_trivia();
        let start = self.position;
        let Some(byte) = self.peek() else {
            return Some(Token {
                kind: TokenKind::Eof,
                span: Span {
                    start: start as u32,
                    end: start as u32,
                },
                text: String::new(),
                number: None,
            });
        };

        // Numero. El lexer de DAX acepta `1`, `1.5`, `.5` y notacion exponencial.
        if byte.is_ascii_digit()
            || (byte == b'.' && self.peek_at(1).is_some_and(|b| b.is_ascii_digit()))
        {
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.position += 1;
            }
            if self.peek() == Some(b'.') {
                self.position += 1;
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.position += 1;
                }
            }
            if matches!(self.peek(), Some(b'e') | Some(b'E')) {
                let save = self.position;
                self.position += 1;
                if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                    self.position += 1;
                }
                if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                        self.position += 1;
                    }
                } else {
                    // Una `e` suelta no es parte del numero: `1e` es `1` seguido de `e`.
                    self.position = save;
                }
            }
            let text = self.slice(start, self.position);
            return Some(Token {
                kind: TokenKind::Number,
                span: Span {
                    start: start as u32,
                    end: self.position as u32,
                },
                number: text.parse::<f64>().ok(),
                text,
            });
        }

        // Nombre de tabla o columna: comilla simple. `''` es un escape.
        if byte == b'\'' {
            self.position += 1;
            let mut text = String::new();
            loop {
                match self.peek() {
                    None => break,
                    Some(b'\'') if self.peek_at(1) == Some(b'\'') => {
                        text.push('\'');
                        self.position += 2;
                    }
                    Some(b'\'') => {
                        self.position += 1;
                        break;
                    }
                    Some(_) => {
                        let char_len = self.current_char_len();
                        text.push_str(&self.source[self.position..self.position + char_len]);
                        self.position += char_len;
                    }
                }
            }
            return Some(Token {
                kind: TokenKind::QuotedName,
                span: Span {
                    start: start as u32,
                    end: self.position as u32,
                },
                text,
                number: None,
            });
        }

        // String con comillas dobles.
        if byte == b'"' {
            self.position += 1;
            let text_start = self.position;
            while self.peek().is_some_and(|b| b != b'"') {
                self.position += 1;
            }
            let text = self.slice(text_start, self.position);
            self.position += 1;
            return Some(Token {
                kind: TokenKind::Text,
                span: Span {
                    start: start as u32,
                    end: self.position as u32,
                },
                text,
                number: None,
            });
        }

        // Identificador.
        if byte.is_ascii_alphabetic() || byte == b'_' {
            while self
                .peek()
                .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                self.position += 1;
            }
            if self.peek() == Some(b'.')
                && self
                    .peek_at(1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            {
                self.position += 1;
                while self
                    .peek()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    self.position += 1;
                }
            }
            return Some(Token {
                kind: TokenKind::Identifier,
                span: Span {
                    start: start as u32,
                    end: self.position as u32,
                },
                text: self.slice(start, self.position),
                number: None,
            });
        }

        // Punctuacion simple.
        let simple = match byte {
            b'(' => Some(TokenKind::LParen),
            b')' => Some(TokenKind::RParen),
            b'[' => Some(TokenKind::LBracket),
            b']' => Some(TokenKind::RBracket),
            b',' => Some(TokenKind::Comma),
            b':' => Some(TokenKind::Colon),
            b'{' => Some(TokenKind::LBrace),
            b'}' => Some(TokenKind::RBrace),
            _ => None,
        };
        if let Some(kind) = simple {
            self.position += 1;
            return Some(Token {
                kind,
                span: Span {
                    start: start as u32,
                    end: self.position as u32,
                },
                text: self.slice(start, self.position),
                number: None,
            });
        }

        // Operador de dos caracteres. El de un caracter se resuelve aqui tambien.
        let two = &self.source[self.position..self.source.len().min(self.position + 2)];
        let three = &self.source[self.position..self.source.len().min(self.position + 3)];
        for operator in ["<=", ">=", "<>", "=="] {
            if three.starts_with(operator) {
                self.position += operator.len();
                return Some(self.operator(start, operator));
            }
        }
        for operator in ["&&", "||", "+", "-", "*", "/", "^", "&", "=", "<", ">"] {
            if two.starts_with(operator) {
                self.position += operator.len();
                return Some(self.operator(start, operator));
            }
        }

        // Byte desconocido. Se avanza para no loopear infinito y se reporta al parser.
        self.position += 1;
        Some(self.operator(start, &self.source[start..self.position]))
    }

    fn operator(&self, start: usize, text: &str) -> Token {
        Token {
            kind: TokenKind::Operator,
            span: Span {
                start: start as u32,
                end: (start + text.len()) as u32,
            },
            text: text.to_string(),
            number: None,
        }
    }

    fn slice(&self, start: usize, end: usize) -> String {
        self.source[start..end.min(self.source.len())].to_string()
    }

    /// Longitud en bytes del caracter actual, para no partir un UTF-8 por la mitad.
    fn current_char_len(&self) -> usize {
        let byte = self.bytes[self.position];
        if byte < 0x80 {
            1
        } else if byte < 0xE0 {
            2
        } else if byte < 0xF0 {
            3
        } else {
            4
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        Lexer::new(source)
            .tokenize()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn tokeniza_una_medida_simple() {
        let tokens = Lexer::new("SUM ( 'Fact Sales' [ Qty ] )").tokenize();
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Identifier,
                TokenKind::LParen,
                TokenKind::QuotedName,
                TokenKind::LBracket,
                TokenKind::Identifier,
                TokenKind::RBracket,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[2].text, "Fact Sales");
        assert_eq!(tokens[4].kind, TokenKind::Identifier);
        assert_eq!(tokens[4].text, "Qty");
    }

    #[test]
    fn tokeniza_numeros() {
        assert_eq!(
            kinds("1 1.5 .5 1e3 2.5E-2"),
            vec![
                TokenKind::Number,
                TokenKind::Number,
                TokenKind::Number,
                TokenKind::Number,
                TokenKind::Number,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn tokeniza_operadores_de_dos_caracteres() {
        let tokens = Lexer::new("a <= b >= c <> d && e || f").tokenize();
        let operators: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Operator)
            .map(|t| t.text.clone())
            .collect();
        assert_eq!(operators, vec!["<=", ">=", "<>", "&&", "||"]);
    }

    #[test]
    fn escapa_comillas_simples() {
        let tokens = Lexer::new("'Tabla '' con comillas'").tokenize();
        assert_eq!(tokens[0].text, "Tabla ' con comillas");
    }

    #[test]
    fn salta_comentarios() {
        // El comentario de linea no debe producir tokens, y `Qty` sigue siendo visible.
        let tokens = Lexer::new("1 // esto es un comentario\n Qty").tokenize();
        let texts: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| t.text.clone())
            .collect();
        assert_eq!(texts, vec!["1", "Qty"]);
    }

    #[test]
    fn los_spans_apuntan_al_texto_fuente() {
        let source = "SUM('Fact Sales'[Qty])";
        let tokens = Lexer::new(source).tokenize();
        for token in &tokens {
            let slice = &source[token.span.start as usize..token.span.end as usize];
            // `SUM` y los delimitadores se emiten tal cual. Las comillas se eliminan.
            if token.kind != TokenKind::QuotedName {
                assert_eq!(slice, token.text, "span inconsistente en {token:?}");
            }
        }
    }
}
