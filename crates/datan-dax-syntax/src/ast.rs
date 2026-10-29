//! AST de DAX.
//!
//! Cada nodo lleva `Span`. El linter, el go-to-definition y el diff de una medida dependen
//! de poder señalar la posicion exacta en el texto fuente.
//!
//! La distincion que importa para el motor: una referencia sin calificar `[X]` puede ser
//! una medida o una columna, y el AST no lo sabe. Lo decide `datan-dax-check` contra el
//! modelo. Por eso hay dos variantes en vez de una.

use crate::Span;

/// Expresion DAX.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number {
        value: f64,
        span: Span,
    },
    Text {
        value: String,
        span: Span,
    },
    Boolean {
        value: bool,
        span: Span,
    },
    Blank {
        span: Span,
    },

    /// Referencia sin calificar: `[Qty]` o `Revenue`.
    /// Un nombre sin calificar: `[Qty]`, `Qty` o `'Nombre entre comillas'`.
    ///
    /// `quoted` decide el caso ambiguo de DAX. Un nombre entre comillas sin `[` detras es
    /// nombre de tabla (`COUNTROWS('Fact Sales')`) o literal de cadena
    /// (`'dim_product'[region] = 'Oeste'`), y el parser no puede distinguirlo con la
    /// informacion que tiene. Lo resuelve el chequeo contra el modelo: si el nombre
    /// corresponde a una tabla es tabla, y si no, es texto.
    ///
    /// Sin este campo, `= 'Oeste'` falla con "`Oeste` no existe en el modelo", que es un
    /// error de nombre donde el usuario escribio un literal.
    Reference {
        name: String,
        quoted: bool,
        span: Span,
    },

    /// Referencia calificada: `'Fact Sales'[Qty]`.
    QualifiedReference {
        table: String,
        column: String,
        span: Span,
    },

    /// Llamada a funcion: `SUM(...)`, `CALCULATE(...)`, `ROW(...)`.
    Call {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },

    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },

    /// Bloque `VAR a = 1 VAR b = 2 RETURN a + b`.
    Block {
        vars: Vec<VarBinding>,
        body: Box<Expr>,
        span: Span,
    },

    /// Constructor de tabla: `ROW("x", [Qty])` o `{1, 2, 3}`.
    Table(TableExpr),
}

/// `VAR nombre = expresion`.
#[derive(Debug, Clone, PartialEq)]
pub struct VarBinding {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

/// Constructor de tabla.
#[derive(Debug, Clone, PartialEq)]
pub enum TableExpr {
    /// `ROW("nombre", expr, ...)`.
    Row {
        columns: Vec<String>,
        values: Vec<Expr>,
        span: Span,
    },
    /// `{1, 2, 3}`.
    Literal { values: Vec<Expr>, span: Span },
    /// `TABLE(...)` o `SELECTCOLUMNS(...)`, guardado como llamada.
    Call {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
}

/// Modificador de `CALCULATE`.
///
/// Cada uno muta el filter context de forma distinta. Son el corazon de la semantica de DAX
/// y el punto donde el fast path SQL y el interprete divergen.
#[derive(Debug, Clone, PartialEq)]
pub enum Modifier {
    /// `ALL(tabla)` o `ALL()`.
    RemoveAll { table: Option<String>, span: Span },
    /// `ALLEXCEPT(tabla, col, ...)`.
    AllExcept {
        table: String,
        keep: Vec<String>,
        span: Span,
    },
    /// `KEEPFILTERS(expr)`.
    KeepFilters { expr: Expr, span: Span },
    /// `REMOVEFILTERS(tabla)`.
    RemoveFilters { table: Option<String>, span: Span },
    /// `CROSSFILTER(a, b, direccion)`.
    CrossFilter {
        from: String,
        to: String,
        span: Span,
    },
    /// `USERELATIONSHIP(a, b)`.
    UseRelationship {
        from: String,
        to: String,
        span: Span,
    },
    /// Expresion booleana como filtro.
    Boolean { expr: Expr, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Concat,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
    Power,
}

impl BinaryOp {
    /// Los operadores de filtro. Un filtro aplicado dentro de un contexto de fila produce
    /// transicion de contexto, que es el comportamiento mas sutil de DAX.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Number { span, .. }
            | Expr::Text { span, .. }
            | Expr::Boolean { span, .. }
            | Expr::Blank { span }
            | Expr::Reference { span, .. }
            | Expr::QualifiedReference { span, .. }
            | Expr::Call { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Block { span, .. } => *span,
            Expr::Table(table) => match table {
                TableExpr::Row { span, .. }
                | TableExpr::Literal { span, .. }
                | TableExpr::Call { span, .. } => *span,
            },
        }
    }
}

impl std::fmt::Display for Expr {
    /// Reconstruye la expresion. Es la base del pretty-printer y del export a TMDL.
    ///
    /// Verificar que `parse(expr.to_string()) == expr` es una property test que corre en
    /// milisegundos y detecta casi todos los errores de round-trip.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expr::Number { value, .. } => {
                if value.fract() == 0.0 && value.abs() < 1e15 {
                    write!(f, "{}", *value as i64)
                } else {
                    write!(f, "{value}")
                }
            }
            Expr::Text { value, .. } => write!(f, "\"{value}\""),
            Expr::Boolean { value, .. } => {
                write!(f, "{}", if *value { "TRUE()" } else { "FALSE()" })
            }
            Expr::Blank { .. } => write!(f, "BLANK()"),
            Expr::Reference { name, .. } => write!(f, "[{name}]"),
            Expr::QualifiedReference { table, column, .. } => write!(f, "'{table}'[{column}]"),
            Expr::Call { name, args, .. } => {
                write!(f, "{name}(")?;
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            Expr::Binary {
                op, left, right, ..
            } => {
                // Sin parentesis. El parser reconstruye la precedencia por la precedencia
                // de operadores, y meter parentesis rompe el round-trip porque el parser
                // tiene que aceptar un `(` como inicio de expresion primaria.
                write!(f, "{left} {} {right}", op.as_str())
            }
            Expr::Unary { op, operand, .. } => match op {
                UnaryOp::Negate => write!(f, "-{operand}"),
                UnaryOp::Not => write!(f, "NOT({operand})"),
            },
            Expr::Block { vars, body, .. } => {
                for var in vars {
                    write!(f, "VAR {} = {} ", var.name, var.value)?;
                }
                write!(f, "RETURN {body}")
            }
            Expr::Table(table) => match table {
                TableExpr::Row {
                    columns, values, ..
                } => {
                    write!(f, "ROW(")?;
                    for (index, column) in columns.iter().enumerate() {
                        if index > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "\"{column}\", {}", values[index])?;
                    }
                    write!(f, ")")
                }
                TableExpr::Literal { values, .. } => {
                    write!(f, "{{")?;
                    for (index, value) in values.iter().enumerate() {
                        if index > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{value}")?;
                    }
                    write!(f, "}}")
                }
                TableExpr::Call { name, args, .. } => {
                    write!(f, "{name}(")?;
                    for (index, arg) in args.iter().enumerate() {
                        if index > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ")")
                }
            },
        }
    }
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Subtract => "-",
            BinaryOp::Multiply => "*",
            BinaryOp::Divide => "/",
            BinaryOp::Concat => "&",
            BinaryOp::Equal => "=",
            BinaryOp::NotEqual => "<>",
            BinaryOp::Less => "<",
            BinaryOp::LessEqual => "<=",
            BinaryOp::Greater => ">",
            BinaryOp::GreaterEqual => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
            BinaryOp::Power => "^",
        }
    }
}
