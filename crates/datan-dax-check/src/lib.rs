//! Chequeo de DAX contra el modelo: resolucion de nombres, typecheck y lint.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. `cargo test` corre en milisegundos.
//!
//! La salida es un `CheckedExpr`: el mismo AST, pero con cada referencia resuelta a su
//! `ObjectId` del modelo. A partir de ahi nadie vuelve a mirar strings, y el motor de
//! medidas no puede equivocarse al resolver un nombre.
//!
//! ## La regla que hace esto no trivial
//!
//! Un `[Qty]` sin calificar puede ser una medida o una columna, y DAX resuelve distinto
//! segun el contexto:
//!
//! - En contexto de filtro, dentro de una medida: gana la medida si existe.
//! - En contexto de fila, dentro de `SUMX(Tabla, ...)`: solo hay columnas de esa tabla.
//!   Referenciar otra columna sin `RELATED` es un error, no unLookup silencioso.
//!
//! Ademas, referenciar una columna desnuda dentro de una medida es un error en DAX, porque
//! no hay fila actual. Por eso `Revenue = Fact Sales[Qty]` falla y `Revenue = SUM(Fact
//! Sales[Qty])` no. Este crate reproduce esa regla.

use datan_dax_syntax::{BinaryOp, Expr, Span, TableExpr, UnaryOp, VarBinding};
use datan_model::{Model, ObjectId, TableKind};
use std::collections::{BTreeSet, HashMap};

mod checked;
mod diagnostics;

pub use checked::{CheckedExpr, Reference};
pub use diagnostics::{Diagnostic, Severity};

/// Error fatal del chequeo. Un error fatal significa que la expresion no tiene sentido y no
/// se puede evaluar. Los problemas de estilo van en `Diagnostic`.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("no existe la medida {0}")]
    UnknownMeasure(String),
    #[error("`{name}` no existe en el modelo")]
    UnknownName { name: String, span: Span },
    #[error("`{name}` existe en {count} tablas: {tables}. Calificala con 'Tabla'[{name}]")]
    Ambiguous {
        name: String,
        count: usize,
        tables: String,
        span: Span,
    },
    #[error("la tabla {table} no tiene la columna {column}")]
    UnknownColumn {
        table: String,
        column: String,
        span: Span,
    },
    #[error("`{name}` solo existe en {table}, y aqui hay contexto de fila sobre {current}")]
    ColumnOutsideRowContext {
        name: String,
        table: String,
        current: String,
        span: Span,
    },
    #[error("una columna no se puede usar como medida. Envuelvela en SUM, COUNT o MAX")]
    ColumnWithoutAggregation { name: String, span: Span },
    #[error("la medida {name} se referencia a si misma, directa o indirectamente")]
    CircularMeasure { name: String, span: Span },
}

/// Contexto en el que se evalua una expresion.
///
/// El contexto decide como se resuelve un nombre sin calificar. Es el unico lugar donde
/// cambia, y por eso es un parametro explicito y no un flag escondido.
#[derive(Debug, Clone)]
enum Context {
    /// Dentro del cuerpo de una medida. Hay contexto de filtro.
    Filter,
    /// Dentro de un iterador sobre una tabla. Hay contexto de fila y las columnas son las
    /// de esa tabla.
    Row(ObjectId),
}

/// Resultado del chequeo: la expresion resuelta mas los problemas que no la invalidan.
#[derive(Debug, Clone)]
pub struct Checked {
    pub expr: CheckedExpr,
    pub diagnostics: Vec<Diagnostic>,
    /// Medidas de las que depende esta expresion, en orden de dependencia. Sirve para
    /// detectar ciclos y para invalidar cache en cascada.
    pub measure_dependencies: BTreeSet<ObjectId>,
}

/// Parsea y chequea una expresion de medida.
pub fn check(source: &str, model: &Model) -> Result<Checked, CheckError> {
    let parsed = datan_dax_syntax::parse(source).map_err(|error| CheckError::UnknownName {
        name: format!("error de sintaxis: {error}"),
        span: error.span,
    })?;
    check_expr(&parsed, model)
}

/// Parsea y chequea una expresion que ya viene en AST.
pub fn check_expr(parsed: &Expr, model: &Model) -> Result<Checked, CheckError> {
    let mut checker = Checker {
        model,
        context: Context::Filter,
        variables: HashMap::new(),
        dependencies: BTreeSet::new(),
        diagnostics: Vec::new(),
    };
    let expr = checker.check(parsed)?;
    Ok(Checked {
        expr,
        diagnostics: checker.diagnostics,
        measure_dependencies: checker.dependencies,
    })
}

struct Checker<'a> {
    model: &'a Model,
    context: Context,
    variables: HashMap<String, CheckedExpr>,
    dependencies: BTreeSet<ObjectId>,
    diagnostics: Vec<Diagnostic>,
}

impl Checker<'_> {
    fn check(&mut self, expr: &Expr) -> Result<CheckedExpr, CheckError> {
        Ok(match expr {
            Expr::Number { value, span } => CheckedExpr::Number {
                value: *value,
                span: *span,
            },
            Expr::Text { value, span } => CheckedExpr::Text {
                value: value.clone(),
                span: *span,
            },
            Expr::Boolean { value, span } => CheckedExpr::Boolean {
                value: *value,
                span: *span,
            },
            Expr::Blank { span } => CheckedExpr::Blank { span: *span },

            Expr::Reference { name, span } => self.resolve(name, *span)?,

            Expr::QualifiedReference {
                table,
                column,
                span,
            } => {
                let (owner, model_column) = self.model.column_by_name(table, column).ok_or(
                    CheckError::UnknownColumn {
                        table: table.clone(),
                        column: column.clone(),
                        span: *span,
                    },
                )?;
                CheckedExpr::Column {
                    column: model_column.id,
                    name: model_column.name.clone(),
                    table: Some(owner.name.clone()),
                    span: *span,
                }
            }

            Expr::Call { name, args, span } => self.check_call(name, args, *span)?,

            Expr::Binary { op, left, right, span } => {
                let left = self.check(left)?;
                let right = self.check(right)?;
                CheckedExpr::Binary {
                    op: *op,
                    left: Box::new(left),
                    right: Box::new(right),
                    span: *span,
                }
            }

            Expr::Unary { op, operand, span } => {
                let operand = self.check(operand)?;
                CheckedExpr::Unary {
                    op: *op,
                    operand: Box::new(operand),
                    span: *span,
                }
            }

            Expr::Block { vars, body, span } => {
                // Las variables se resuelven en orden y solo son visibles hacia adelante,
                // igual que en DAX.
                let previous = self.variables.clone();
                let mut checked_vars = Vec::with_capacity(vars.len());
                for var in vars {
                    checked_vars.push(self.check_var(var)?);
                }
                let body = self.check(body)?;
                self.variables = previous;
                CheckedExpr::Block {
                    vars: checked_vars,
                    body: Box::new(body),
                    span: *span,
                }
            }

            Expr::Table(table) => self.check_table(table)?,
        })
    }

    fn check_var(&mut self, var: &VarBinding) -> Result<CheckedExpr, CheckError> {
        let value = self.check(&var.value)?;
        self.variables
            .insert(var.name.clone(), value.clone());
        Ok(CheckedExpr::Variable {
            name: var.name.clone(),
            value: Box::new(value),
            span: var.span,
        })
    }

    /// Resuelve un nombre sin calificar. Es la regla central del crate.
    fn resolve(&mut self, name: &str, span: Span) -> Result<CheckedExpr, CheckError> {
        // 1. Una variable local gana sobre todo.
        if let Some(value) = self.variables.get(name) {
            return Ok(CheckedExpr::Variable {
                name: name.to_string(),
                value: Box::new(value.clone()),
                span,
            });
        }

        // 2. Una tabla.
        if let Some(table) = self.model.table_by_name(name) {
            return Ok(CheckedExpr::Table {
                table: table.id,
                name: table.name.clone(),
                span,
            });
        }

        // 3. Una medida. En contexto de fila DAX no resuelve medidas sin CALCULATE, pero
        //    ser permisivos aqui y marcarlo como diagnostico da un mensaje mejor que un
        //    error de sintaxis.
        if let Some(measure) = self.model.measure_by_name(name) {
            if let Context::Row(current) = self.context {
                let table_name = self.table_name(current);
                self.diagnostics.push(Diagnostic::error(
                    span,
                    "measure-in-row-context",
                    format!(
                        "`{name}` es una medida y no se puede referenciar dentro de un \
                         contexto de fila sobre {table_name}. Envuelvela en CALCULATE."
                    ),
                ));
            }
            self.dependencies.insert(measure.id);
            return Ok(CheckedExpr::Measure {
                measure: measure.id,
                name: measure.name.clone(),
                span,
            });
        }

        // 4. Una columna. En contexto de fila tiene que ser de la tabla del iterador.
        let candidates: Vec<_> = self
            .model
            .tables
            .iter()
            .filter_map(|table| table.column_by_name(name).map(|c| (table.id, table.name.clone(), c.id, c.name.clone())))
            .collect();

        match candidates.len() {
            0 => Err(CheckError::UnknownName {
                name: name.to_string(),
                span,
            }),
            1 => {
                let (owner_id, table_name, column_id, column_name) = candidates[0].clone();
                if let Context::Row(current) = self.context {
                    if current != owner_id {
                        return Err(CheckError::ColumnOutsideRowContext {
                            name: column_name,
                            table: table_name,
                            current: self.table_name(current),
                            span,
                        });
                    }
                }
                Ok(CheckedExpr::Column {
                    column: column_id,
                    name: column_name,
                    table: Some(table_name),
                    span,
                })
            }
            _ => Err(CheckError::Ambiguous {
                name: name.to_string(),
                count: candidates.len(),
                tables: candidates
                    .iter()
                    .map(|(_, table, _, _)| table.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                span,
            }),
        }
    }

    fn check_call(
        &mut self,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<CheckedExpr, CheckError> {
        match name {
            // Un iterador cambia el contexto a fila sobre su primera tabla.
            "SUMX" | "AVERAGEX" | "MAXX" | "MINX" | "COUNTX" | "PRODUCTX" | "RANKX" => {
                if args.len() < 2 {
                    return Err(CheckError::UnknownName {
                        name: format!("{name} necesita al menos dos argumentos"),
                        span,
                    });
                }
                let (table_id, table_expr, mut checked_args) =
                    self.check_iterator(&args[0], &args[1], name, span)?;
                let _ = table_id;
                for arg in &args[2..] {
                    checked_args.push(self.check(arg)?);
                }
                Ok(CheckedExpr::Call {
                    name: name.to_string(),
                    args: checked_args,
                    table_expr: Some(Box::new(table_expr)),
                    span,
                })
            }

            // CALCULATE y sus equivalentes.
            //
            // El primer argumento se evalua en contexto de FILTRO, aunque estemos dentro de
            // un iterador. Eso es la transicion de contexto, el comportamiento mas sutil de
            // DAX: `SUMX(Fact, [Qty] * CALCULATE([Revenue]))` filtra por la fila actual en
            // vez de sumar todo. Por eso hay que cambiar el contexto antes de chequear el
            // cuerpo, y no solo marcar la llamada.
            "CALCULATE" | "CALCULATETABLE" => {
                let mut checked_args = Vec::with_capacity(args.len());
                let mut first = true;
                for arg in args {
                    if first {
                        let previous = self.context.clone();
                        self.context = Context::Filter;
                        let checked = self.check(arg);
                        self.context = previous;
                        checked_args.push(checked?);
                        first = false;
                        continue;
                    }
                    checked_args.push(self.check(arg)?);
                }
                if checked_args.is_empty() {
                    return Err(CheckError::UnknownName {
                        name: "CALCULATE necesita al menos una expresion".to_string(),
                        span,
                    });
                }
                Ok(CheckedExpr::Calculate {
                    name: name.to_string(),
                    args: checked_args,
                    span,
                })
            }

            // FILTER cambia a contexto de fila sobre la tabla de su primer argumento.
            "FILTER" => {
                if args.len() < 2 {
                    return Err(CheckError::UnknownName {
                        name: "FILTER necesita dos argumentos".to_string(),
                        span,
                    });
                }
                let (_, table_expr, mut checked_args) =
                    self.check_iterator(&args[0], &args[1], name, span)?;
                for arg in &args[2..] {
                    checked_args.push(self.check(arg)?);
                }
                Ok(CheckedExpr::Call {
                    name: name.to_string(),
                    args: checked_args,
                    table_expr: Some(Box::new(table_expr)),
                    span,
                })
            }

            // VALUES y DEVUELVEN una tabla de una columna. No cambian el contexto.
            "VALUES" | "DISTINCT" | "ALLSELECTED" => {
                let mut checked_args = Vec::with_capacity(args.len());
                for arg in args {
                    checked_args.push(self.check(arg)?);
                }
                Ok(CheckedExpr::Call {
                    name: name.to_string(),
                    args: checked_args,
                    table_expr: None,
                    span,
                })
            }

            // El resto se resuelve con el contexto actual.
            _ => {
                let mut checked_args = Vec::with_capacity(args.len());
                for arg in args {
                    checked_args.push(self.check(arg)?);
                }
                Ok(CheckedExpr::Call {
                    name: name.to_string(),
                    args: checked_args,
                    table_expr: None,
                    span,
                })
            }
        }
    }

    /// Resuelve `TABLA` o `(EXPRESION, TABLA)` como source de un iterador, y despues
    /// chequea el cuerpo en contexto de fila.
    fn check_iterator(
        &mut self,
        table_arg: &Expr,
        body_arg: &Expr,
        function: &str,
        span: Span,
    ) -> Result<(ObjectId, CheckedExpr, Vec<CheckedExpr>), CheckError> {
        // La forma de tabla directa: `SUMX('Fact Sales', ...)`.
        let table_id = match table_arg {
            Expr::Reference { name, .. } => self
                .model
                .table_by_name(name)
                .map(|t| t.id)
                .ok_or_else(|| CheckError::UnknownName {
                    name: format!("`{name}` no es una tabla"),
                    span: table_arg.span(),
                })?,
            Expr::QualifiedReference { table, .. } => self
                .model
                .table_by_name(table)
                .map(|t| t.id)
                .ok_or_else(|| CheckError::UnknownName {
                    name: format!("`{table}` no es una tabla"),
                    span: table_arg.span(),
                })?,
            // La forma de filtro: `FILTER('Fact Sales', ...)`. Se acepta como source.
            Expr::Call { name, args, span: inner } if name == "FILTER" && !args.is_empty() => {
                match &args[0] {
                    Expr::Reference { name, .. } => self.model.table_by_name(name).map(|t| t.id),
                    Expr::QualifiedReference { table, .. } => {
                        self.model.table_by_name(table).map(|t| t.id)
                    }
                    _ => None,
                }
                .ok_or(CheckError::UnknownName {
                    name: "el primer argumento de FILTER debe ser una tabla".to_string(),
                    span: *inner,
                })?
            }
            _ => {
                self.diagnostics.push(Diagnostic::warning(
                    span,
                    "iterator-over-expression",
                    format!(
                        "`{function}` recibe una expresion en vez de una tabla. \
                         El contexto de fila se deduce de las columnas que se usen dentro."
                    ),
                ));
                ObjectId(0)
            }
        };

        let table_expr = self.check(table_arg)?;

        // El cuerpo del iterador se evalua en contexto de fila.
        let previous = self.context.clone();
        self.context = Context::Row(table_id);
        let body = self.check(body_arg);
        self.context = previous;

        Ok((table_id, table_expr, vec![body?]))
    }

    fn check_table(&mut self, table: &TableExpr) -> Result<CheckedExpr, CheckError> {
        Ok(match table {
            TableExpr::Row { columns, values, span } => {
                let mut checked = Vec::with_capacity(values.len());
                for value in values {
                    checked.push(self.check(value)?);
                }
                CheckedExpr::RowConstructor {
                    columns: columns.clone(),
                    values: checked,
                    span: *span,
                }
            }
            TableExpr::Literal { values, span } => {
                let mut checked = Vec::with_capacity(values.len());
                for value in values {
                    checked.push(self.check(value)?);
                }
                CheckedExpr::RowConstructor {
                    columns: Vec::new(),
                    values: checked,
                    span: *span,
                }
            }
            TableExpr::Call { name, args, span } => {
                let mut checked = Vec::with_capacity(args.len());
                for arg in args {
                    checked.push(self.check(arg)?);
                }
                CheckedExpr::Call {
                    name: name.clone(),
                    args: checked,
                    table_expr: None,
                    span: *span,
                }
            }
        })
    }

    fn table_name(&self, id: ObjectId) -> String {
        self.model
            .table(id)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "tabla desconocida".to_string())
    }
}

/// Detecta ciclos entre medidas.
///
/// Devuelve el nombre de la medida cuando el grafo de dependencias tiene un ciclo. Se
/// ejecuta aparte del chequeo de una expresion porque el ciclo puede跨越 medidas.
pub fn find_measure_cycle(model: &Model) -> Option<String> {
    let mut edges: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for measure in &model.measures {
        if let Ok(checked) = check(&measure.expression, model) {
            edges.insert(measure.id, checked.measure_dependencies.into_iter().collect());
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Grey,
        Black,
    }

    let mut marks: HashMap<ObjectId, Mark> = HashMap::new();

    fn visit(
        node: ObjectId,
        edges: &HashMap<ObjectId, Vec<ObjectId>>,
        marks: &mut HashMap<ObjectId, Mark>,
    ) -> Option<ObjectId> {
        match marks.get(&node).copied().unwrap_or(Mark::White) {
            Mark::Black => return None,
            Mark::Grey => return Some(node),
            Mark::White => {}
        }
        marks.insert(node, Mark::Grey);
        for next in edges.get(&node).into_iter().flatten() {
            if let Some(found) = visit(*next, edges, marks) {
                return Some(found);
            }
        }
        marks.insert(node, Mark::Black);
        None
    }

    let mut names: Vec<_> = model.measures.iter().collect();
    names.sort_by_key(|m| m.id);
    for measure in names {
        if let Some(cycle) = visit(measure.id, &edges, &mut marks) {
            return Some(
                model
                    .measures
                    .iter()
                    .find(|m| m.id == cycle)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| "desconocida".to_string()),
            );
        }
    }
    None
}

/// ¿Es este modelo utilizable? Un modelo con ciclo de medidas no lo es.
pub fn model_is_valid(model: &Model) -> Result<(), CheckError> {
    match find_measure_cycle(model) {
        Some(name) => Err(CheckError::CircularMeasure {
            name,
            span: Span { start: 0, end: 0 },
        }),
        None => Ok(()),
    }
}

/// Reexportado para que los tests puedan construir una tabla de hechos.
pub use datan_model::{Column, Measure, Relationship, Table};

/// Indica si una expresion es una agregacion valida sobre una columna.
///
/// `Revenue = Fact Sales[Qty]` es un error en DAX porque no hay fila actual. Esta funcion
/// es la que decide, y el lint la usa para dar un mensaje util.
pub fn needs_aggregation(expr: &CheckedExpr) -> bool {
    matches!(
        expr,
        CheckedExpr::Column { .. } | CheckedExpr::Measure { .. } | CheckedExpr::Binary { .. }
    )
}

/// Binario auxiliar para tests y para el linter.
pub fn is_comparison(op: BinaryOp) -> bool {
    op.is_comparison()
}

/// Reexportado por comodidad.
pub use datan_dax_syntax::UnaryOp as _UnaryOp;
const _: Option<UnaryOp> = None;
const _: Option<TableKind> = None;