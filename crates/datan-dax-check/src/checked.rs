//! AST resuelto. Cada referencia tiene su `ObjectId` del modelo.
//!
//! A partir de ahi nadie vuelve a mirar strings. El motor de medidas no puede equivocarse al
//! resolver un nombre, porque la resolucion ya ocurrio.

use datan_dax_syntax::{BinaryOp, Span, UnaryOp};
use datan_model::ObjectId;

/// Una referencia ya resuelta contra el modelo.
#[derive(Debug, Clone, PartialEq)]
pub enum Reference {
    Measure(ObjectId),
    Column(ObjectId),
    Table(ObjectId),
    Variable(String),
}

/// Expresion con todas sus referencias resueltas.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckedExpr {
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

    /// Referencia a una medida.
    Measure {
        measure: ObjectId,
        name: String,
        span: Span,
    },

    /// Referencia a una columna. `table` es `None` solo si la columna se resolvio por
    /// unicidad y la tabla no se pudo determinar, lo que el modelo no permite.
    Column {
        column: ObjectId,
        name: String,
        table: Option<String>,
        span: Span,
    },

    /// Referencia a una tabla.
    Table {
        table: ObjectId,
        name: String,
        span: Span,
    },

    /// Variable local. `value` apunta al AST ya resuelto de su binding.
    Variable {
        name: String,
        value: Box<CheckedExpr>,
        span: Span,
    },

    Binary {
        op: BinaryOp,
        left: Box<CheckedExpr>,
        right: Box<CheckedExpr>,
        span: Span,
    },

    Unary {
        op: UnaryOp,
        operand: Box<CheckedExpr>,
        span: Span,
    },

    Block {
        vars: Vec<CheckedExpr>,
        body: Box<CheckedExpr>,
        span: Span,
    },

    /// `CALCULATE` y `CALCULATETABLE`, con sus modificadores aun sin interpretar. Es
    /// `datan-dax-eval` quien decide como muta el filtro.
    Calculate {
        name: String,
        args: Vec<CheckedExpr>,
        span: Span,
    },

    /// Llamada a funcion. `table_expr` es el source de un iterador, si lo hay.
    ///
    /// `Box` porque si no el enum es recursivo sin indireccion y no tiene tamano finito.
    Call {
        name: String,
        args: Vec<CheckedExpr>,
        table_expr: Option<Box<CheckedExpr>>,
        span: Span,
    },

    RowConstructor {
        columns: Vec<String>,
        values: Vec<CheckedExpr>,
        span: Span,
    },
}

impl CheckedExpr {
    pub fn span(&self) -> Span {
        match self {
            CheckedExpr::Number { span, .. }
            | CheckedExpr::Text { span, .. }
            | CheckedExpr::Boolean { span, .. }
            | CheckedExpr::Blank { span }
            | CheckedExpr::Measure { span, .. }
            | CheckedExpr::Column { span, .. }
            | CheckedExpr::Table { span, .. }
            | CheckedExpr::Variable { span, .. }
            | CheckedExpr::Binary { span, .. }
            | CheckedExpr::Unary { span, .. }
            | CheckedExpr::Block { span, .. }
            | CheckedExpr::Calculate { span, .. }
            | CheckedExpr::Call { span, .. }
            | CheckedExpr::RowConstructor { span, .. } => *span,
        }
    }

    /// Toda medida referenciada en esta expresion, en cualquier profundidad.
    pub fn referenced_measures(&self) -> Vec<ObjectId> {
        let mut found = Vec::new();
        self.collect_measures(&mut found);
        found
    }

    fn collect_measures(&self, out: &mut Vec<ObjectId>) {
        match self {
            CheckedExpr::Measure { measure, .. } => {
                if !out.contains(measure) {
                    out.push(*measure);
                }
            }
            CheckedExpr::Variable { value, .. } => value.collect_measures(out),
            CheckedExpr::Binary { left, right, .. } => {
                left.collect_measures(out);
                right.collect_measures(out);
            }
            CheckedExpr::Unary { operand, .. } => operand.collect_measures(out),
            CheckedExpr::Block { vars, body, .. } => {
                for var in vars {
                    var.collect_measures(out);
                }
                body.collect_measures(out);
            }
            CheckedExpr::Calculate { args, .. } | CheckedExpr::Call { args, .. } => {
                for arg in args {
                    arg.collect_measures(out);
                }
            }
            CheckedExpr::RowConstructor { values, .. } => {
                for value in values {
                    value.collect_measures(out);
                }
            }
            _ => {}
        }
    }

    /// Toda columna referenciada, en cualquier profundidad.
    pub fn referenced_columns(&self) -> Vec<ObjectId> {
        let mut found = Vec::new();
        self.collect_columns(&mut found);
        found
    }

    fn collect_columns(&self, out: &mut Vec<ObjectId>) {
        match self {
            CheckedExpr::Column { column, .. } => {
                if !out.contains(column) {
                    out.push(*column);
                }
            }
            CheckedExpr::Variable { value, .. } => value.collect_columns(out),
            CheckedExpr::Binary { left, right, .. } => {
                left.collect_columns(out);
                right.collect_columns(out);
            }
            CheckedExpr::Unary { operand, .. } => operand.collect_columns(out),
            CheckedExpr::Block { vars, body, .. } => {
                for var in vars {
                    var.collect_columns(out);
                }
                body.collect_columns(out);
            }
            CheckedExpr::Calculate { args, .. } | CheckedExpr::Call { args, .. } => {
                for arg in args {
                    arg.collect_columns(out);
                }
            }
            CheckedExpr::RowConstructor { values, .. } => {
                for value in values {
                    value.collect_columns(out);
                }
            }
            _ => {}
        }
    }
}
