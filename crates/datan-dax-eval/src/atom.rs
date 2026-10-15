//! Extraccion de atomos.
//!
//! Un atomo es una agregacion evaluada bajo un conjunto de filtros **concreto y resuelto**.
//! Es la unidad que los dos backends consumen:
//!
//! - El fast path SQL los traduce a agregados en una sola pasada.
//! - El interprete los evalua por celda de grupo, sin volver a razonar sobre filtros.
//!
//! Esa es la razon del split. El fallback nunca corre sobre filas: corre sobre celdas, y
//! cada agregacion hoja la sirve un escalar ya precalculado. Una matriz de 5.000 celdas con
//! 6 atomos son 30.000 busquedas hash, que son milisegundos.

use datan_dax_check::CheckedExpr;
use datan_model::{Model, ObjectId};

use crate::filter::FilterContext;

/// Como se agrega una columna.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomKind {
    Sum,
    Count,
    CountRows,
    Min,
    Max,
    Average,
    DistinctCount,
    /// Una agregacion sobre una expresion, no sobre una columna simple. `SUMX` cae aqui.
    Expression,
}

/// Granularidad a la que se evalua un atomo.
///
/// Es el campo que decide si el backend SQL puede o no. Un atomo cuya granularidad no
/// coincide con la del `GROUP BY` necesita una subconsulta correlacionada, y ahi se rompe el
/// fast path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// Se agrupa por las columnas del `GROUP BY`. Es el caso normal.
    Query,
    /// Se agrupa por una columna que no esta en el `GROUP BY`. Hay que recalcular.
    Finer(ObjectId),
    /// No hay agrupamiento: es un escalar sobre toda la tabla.
    Scalar,
}

/// Un atomo.
#[derive(Debug, Clone, PartialEq)]
pub struct Atom {
    pub kind: AtomKind,
    /// Columna agregada, si la hay.
    pub column: Option<ObjectId>,
    /// Expresion agregada, para los iteradores. Es el AST resuelto.
    pub expression: Option<CheckedExpr>,
    /// El conjunto de filtros bajo el que se evalua. Ya propagado y resuelto.
    pub context: FilterContext,
    pub granularity: Granularity,
    /// Etiqueta estable, para el cache de planes y para el diff de una medida.
    pub label: String,
    /// El atomo se genero bajo un `CALCULATE` con modificadores. El conjunto de filtros
    /// cambia de forma no uniforme y no se puede bajar a un agregado de SQL.
    pub requires_interpreter: bool,
}

impl Atom {
    /// Si este atomo necesita el interprete en vez del fast path SQL.
    pub fn needs_interpreter(&self) -> bool {
        // Una granularidad mas fina que la de la consulta obliga a una subconsulta
        // correlacionada. Ver PLAN.md seccion 3.1, caso 2. Y un CALCULATE con modificadores
        // cambia el filtro de forma no uniforme.
        self.requires_interpreter || matches!(self.granularity, Granularity::Finer(_))
    }
}

/// El conjunto de atomos de una medida bajo un contexto y un agrupamiento.
///
/// Si dos medidas del mismo informe producen el mismo conjunto de atomos, comparten la
/// lectura. Es lo que hace que una matriz con 6 columnas y 4 celdas de grupo lea una sola
/// vez la tabla de hechos.
#[derive(Debug, Clone, PartialEq)]
pub struct AtomSet {
    pub atoms: Vec<Atom>,
    pub grouping: Vec<ObjectId>,
}

impl AtomSet {
    /// Si alguno de los atomos obliga al interprete.
    pub fn needs_interpreter(&self) -> bool {
        self.atoms.iter().any(Atom::needs_interpreter)
    }

    /// Atomos identicos, para deduplicar entre medidas.
    pub fn signature(&self) -> Vec<(String, Vec<ObjectId>)> {
        self.atoms
            .iter()
            .map(|atom| {
                (
                    atom.label.clone(),
                    atom.context.filtered_columns().collect(),
                )
            })
            .collect()
    }
}

/// Recorre el AST resuelto y extrae atomos.
pub(crate) struct Extractor<'a> {
    pub(crate) model: &'a Model,
    pub(crate) grouping: Vec<ObjectId>,
    pub(crate) atoms: Vec<Atom>,
    pub(crate) depth: usize,
}

impl Extractor<'_> {
    /// Extrae los atomos de una expresion bajo un contexto.
    pub(crate) fn extract(
        &mut self,
        expr: &CheckedExpr,
        context: &FilterContext,
    ) -> Result<(), crate::EvalError> {
        // Un limite de profundidad evita que una medida recursiva通过了 el chequeo de ciclos
        // entre capas se cuele en un ciclo real de evaluacion.
        if self.depth > 64 {
            return Err(crate::EvalError::Unsupported(
                "profundidad de evaluacion excedida".to_string(),
            ));
        }

        match expr {
            // Una agregacion simple: aqui nace un atomo.
            CheckedExpr::Call { name, args, .. } => {
                if let Some(kind) = aggregation_of(name) {
                    if !args.is_empty() {
                        self.emit(kind, &args[0], context, name)?;
                    }
                    // Los argumentos restantes de una agregacion simple son filtros
                    // opcionales. Se ignoran para el atomo; el chequeo ya los valido.
                } else if is_iterator(name) {
                    // `SUMX(Fact, expr)`: la agregacion corre sobre el contexto de fila
                    // de la tabla. El atomo necesita el join implicito.
                    self.extract_iterator(name, args, context)?;
                } else if name == "CALCULATE" || name == "CALCULATETABLE" {
                    // El contexto de CALCULATE lo resuelve `apply_modifiers`. Aqui solo se
                    // propaga para que los atomos internos no vean el filtro equivocado.
                    self.extract_calculate(args, context)?;
                } else {
                    for arg in args {
                        self.depth += 1;
                        self.extract(arg, context)?;
                        self.depth -= 1;
                    }
                }
            }

            // `CALCULATE` no llega como `Call`: el chequeo lo emite como `Calculate`,
            // porque sus modificadores tienen semantica propia.
            CheckedExpr::Calculate { args, .. } => {
                self.extract_calculate(args, context)?;
            }

            CheckedExpr::Binary { left, right, .. } => {
                self.depth += 1;
                self.extract(left, context)?;
                self.extract(right, context)?;
                self.depth -= 1;
            }

            CheckedExpr::Unary { operand, .. } => {
                self.depth += 1;
                self.extract(operand, context)?;
                self.depth -= 1;
            }

            // Un bloque VAR no cambia el contexto. Se recorren sus bindings por si
            // contienen agregaciones.
            CheckedExpr::Block { vars, body, .. } => {
                for var in vars {
                    self.depth += 1;
                    self.extract(var, context)?;
                    self.depth -= 1;
                }
                self.depth += 1;
                self.extract(body, context)?;
                self.depth -= 1;
            }

            CheckedExpr::RowConstructor { values, .. } => {
                for value in values {
                    self.depth += 1;
                    self.extract(value, context)?;
                    self.depth -= 1;
                }
            }

            CheckedExpr::Variable { value, .. } => {
                self.depth += 1;
                self.extract(value, context)?;
                self.depth -= 1;
            }

            // Una referencia a medida no trae atomos propios: trae los de su expresion. Es
            // lo que hace que `[Qty] + [Qty]` genere los atomos del `SUM` de dentro, dos
            // veces, y que se puedan deduplicar despues.
            CheckedExpr::Measure { measure, .. } => {
                let Some(definition) = self
                    .model
                    .measures
                    .iter()
                    .find(|m| m.id == *measure)
                else {
                    return Ok(());
                };
                // Un ciclo entre medidas ya lo detecta `datan-dax-check`. Aqui solo se
                // limita la profundidad para que un grafo mal formado no cuelgue la
                // extraccion.
                if self.depth > 32 {
                    return Err(crate::EvalError::Unsupported(format!(
                        "la medida {} se referencia a si misma en cadena",
                        definition.name
                    )));
                }
                let parsed = datan_dax_syntax::parse(&definition.expression).map_err(|error| {
                    crate::EvalError::Unsupported(format!(
                        "no se pudo parsear la medida {}: {error}",
                        definition.name
                    ))
                })?;
                let checked = datan_dax_check::check_expr(&parsed, self.model)?;
                self.depth += 1;
                self.extract(&checked.expr, context)?;
                self.depth -= 1;
            }

            // Una columna o un literal no aporta atomos: son hojas.
            _ => {}
        }
        Ok(())
    }

    fn extract_iterator(
        &mut self,
        name: &str,
        args: &[CheckedExpr],
        context: &FilterContext,
    ) -> Result<(), crate::EvalError> {
        if args.len() < 2 {
            return Ok(());
        }
        // El atomo es la agregacion sobre la expresion del iterador. Se emite con `depth`
        // incrementado, asi que si el iterador esta dentro de otro atomo, el interno queda
        // marcado como `Finer` y cae al interprete.
        let kind = aggregation_of(name).unwrap_or(AtomKind::Expression);
        self.depth += 1;
        self.emit(kind, &args[1], context, name)?;
        self.depth -= 1;
        Ok(())
    }

    fn extract_calculate(
        &mut self,
        args: &[CheckedExpr],
        context: &FilterContext,
    ) -> Result<(), crate::EvalError> {
        if args.is_empty() {
            return Ok(());
        }
        // Los modificadores todavia no estan interpreted. Un atomo con modificadores de
        // `ALL` cambia el conjunto de filtros de forma no uniforme, asi que se marca para
        // el interprete.
        let has_modifiers = args.len() > 1;
        self.extract_with_flags(&args[0], context, has_modifiers)?;
        for modifier in &args[1..] {
            self.extract(modifier, context)?;
        }
        Ok(())
    }

    fn extract_with_flags(
        &mut self,
        expr: &CheckedExpr,
        context: &FilterContext,
        force_interpreter: bool,
    ) -> Result<(), crate::EvalError> {
        let before = self.atoms.len();
        self.extract(expr, context)?;
        if force_interpreter {
            // Los atomos generados bajo un CALCULATE con modificadores necesitan el
            // interprete: el conjunto de filtros cambia de forma no uniforme, asi que no
            // se puede expresar como un agregado de SQL con un WHERE fijo.
            for atom in &mut self.atoms[before..] {
                atom.requires_interpreter = true;
            }
        }
        Ok(())
    }

    fn emit(
        &mut self,
        kind: AtomKind,
        argument: &CheckedExpr,
        context: &FilterContext,
        function: &str,
    ) -> Result<(), crate::EvalError> {
        let (column, expression) = match argument {
            CheckedExpr::Column { column, .. } => (Some(*column), None),
            other => (None, Some(other.clone())),
        };

        // La granularidad de un atomo NO depende de que su columna este en el GROUP BY.
        //
        // `SUM(Qty) GROUP BY Region` es el caso normal: las filas ya estan filtradas por el
        // contexto, y el agregado corre sobre ellas. Se puede bajar a un hash aggregate de
        // DuckDB sin problema.
        //
        // Lo que rompe el fast path es una agregacion ANIDADA: un atomo que se evalua a una
        // granularidad distinta de la de la consulta. Eso pasa con `SUMX(VALUES(A),
        // CALCULATE(...))`, que recalcula por cada valor de A. Ver PLAN.md seccion 3.1.
        let granularity = if self.depth > 0 {
            // Hay una agregacion fuera: este atomo es interno, y puede cambiar el grano.
            Granularity::Finer(column.unwrap_or(ObjectId(0)))
        } else if self.grouping.is_empty() {
            Granularity::Scalar
        } else {
            Granularity::Query
        };

        let label = format!("{function}({})", self.label_of(argument));
        self.atoms.push(Atom {
            kind,
            column,
            expression,
            context: context.propagate(self.model),
            granularity,
            label,
            requires_interpreter: false,
        });
        Ok(())
    }

    fn label_of(&self, expr: &CheckedExpr) -> String {
        match expr {
            CheckedExpr::Column { name, .. } => name.clone(),
            CheckedExpr::Measure { name, .. } => name.clone(),
            CheckedExpr::Binary { left, right, op, .. } => {
                format!("({} {} {})", self.label_of(left), op.as_str(), self.label_of(right))
            }
            CheckedExpr::Call { name, args, .. } => {
                let inner: Vec<String> = args.iter().map(|a| self.label_of(a)).collect();
                format!("{name}({})", inner.join(", "))
            }
            other => format!("{other:?}"),
        }
    }
}

impl Atom {
    /// La columna que define la granularidad de este atomo.
    fn granularity_key(&self, grouping: Option<&ObjectId>) -> ObjectId {
        self.column
            .or_else(|| grouping.copied())
            .unwrap_or(ObjectId(0))
    }
}

fn aggregation_of(name: &str) -> Option<AtomKind> {
    Some(match name {
        "SUM" => AtomKind::Sum,
        "COUNT" | "COUNTA" => AtomKind::Count,
        "COUNTROWS" => AtomKind::CountRows,
        "MIN" => AtomKind::Min,
        "MAX" => AtomKind::Max,
        "AVERAGE" => AtomKind::Average,
        "DISTINCTCOUNT" => AtomKind::DistinctCount,
        "SUMX" => AtomKind::Expression,
        "AVERAGEX" => AtomKind::Expression,
        "MAXX" => AtomKind::Expression,
        "MINX" => AtomKind::Expression,
        "COUNTX" => AtomKind::Expression,
        "PRODUCTX" => AtomKind::Expression,
        _ => return None,
    })
}

fn is_iterator(name: &str) -> bool {
    matches!(
        name,
        "SUMX" | "AVERAGEX" | "MAXX" | "MINX" | "COUNTX" | "PRODUCTX" | "RANKX"
    )
}
