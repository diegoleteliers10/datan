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
    /// Tabla agregada. Solo la usan `COUNTROWS` y las agregaciones sobre tabla, que no
    /// tienen columna.
    pub table: Option<ObjectId>,
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
    /// Todas las columnas que el atomo toca.
    ///
    /// Un atomo no tiene SIEMPRE una columna. `SUM('Fact Sales'[Qty])` tiene una, pero
    /// `SUM('Fact Sales'[Qty] * 'Fact Sales'[Price])` agrega sobre una EXPRESION de dos, y
    /// `SUM('Fact Sales'[Qty] * 'Dim Product'[Cost])` sobre dos de tablas distintas.
    ///
    /// El campo `column` sigue siendo `Option` porque se necesita distinguir "agrega sobre
    /// esta columna" de "agrega sobre una expresion", y esa distincion decide si el
    /// lowering puede emitir `SUM(t0.qty)` o tiene que emitir la expresion entera. Lo que
    /// no puede hacer es decidir la tabla de hechos mirando solo `column`: en una
    /// agregacion sobre expresion no hay columna, y el lowering terminaba con "no se
    /// encontro la tabla de hechos" en medidas tan simples como `Revenue`.
    pub fn referenced_columns(&self) -> Vec<ObjectId> {
        if let Some(column) = self.column {
            return vec![column];
        }
        let Some(expression) = &self.expression else {
            return Vec::new();
        };
        let mut columns = Vec::new();
        collect_columns(expression, &mut columns);
        columns
    }

    /// La tabla que aporta la columna agregada, si se puede determinar.
    pub fn anchor_table(&self, model: &Model) -> Option<ObjectId> {
        if let Some(table) = self.table {
            return Some(table);
        }
        self.referenced_columns()
            .into_iter()
            .find_map(|column| model.column(column).map(|(table, _)| table.id))
    }

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
    /// Modificadores de `CALCULATE` que no se pudieron interpretar. Sefilled para que el
    /// linter avise en vez de perder el filtro en silencio.
    pub(crate) unsupported: Vec<String>,
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
                        self.extract(arg, context)?;
                    }
                }
            }

            // `CALCULATE` no llega como `Call`: el chequeo lo emite como `Calculate`,
            // porque sus modificadores tienen semantica propia.
            CheckedExpr::Calculate { args, .. } => {
                self.extract_calculate(args, context)?;
            }

            // Un binario NO sube la profundidad. `SUM(A) + COUNT(B)` son dos agregaciones al
            // mismo nivel, no una anidada: bajan a dos agregados en el mismo SELECT.
            // Solo los iteradores y CALCULATE crean un nivel mas.
            CheckedExpr::Binary { left, right, .. } => {
                self.extract(left, context)?;
                self.extract(right, context)?;
            }

            CheckedExpr::Unary { operand, .. } => {
                self.extract(operand, context)?;
            }

            // Un bloque VAR no cambia el contexto. Se recorren sus bindings por si
            // contienen agregaciones.
            CheckedExpr::Block { vars, body, .. } => {
                for var in vars {
                    self.extract(var, context)?;
                }
                self.extract(body, context)?;
            }

            CheckedExpr::RowConstructor { values, .. } => {
                for value in values {
                    self.depth += 1;
                    self.extract(value, context)?;
                    self.depth -= 1;
                }
            }

            CheckedExpr::Variable { value, .. } => {
                self.extract(value, context)?;
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

        // Los modificadores se resuelven y SE APLICAN antes de extraer el cuerpo. Eso es lo
        // que hace que `CALCULATE([Revenue], ALL('Dim Product'))` se evalue sin el filtro de
        // la dimension: el atomo que sale lleva un contexto ya modificado.
        //
        // Antes el atomo solo se marcaba "para el interprete" y conservaba el contexto
        // original. Eso hacia que los dos caminos dieran numeros distintos siempre que
        // hubiera modificadores, y la suite no lo cubria porque ningun caso los usaba.
        let mut effective = context.clone();
        let mut resolved = Vec::new();
        for modifier in &args[1..] {
            match crate::modifiers::resolve_modifier(modifier) {
                Ok(modifier) => {
                    effective = crate::modifiers::apply_modifiers(
                        self.model,
                        &effective,
                        std::slice::from_ref(&modifier),
                    )?;
                    resolved.push(modifier);
                }
                Err(error) => {
                    // Un modificador que no se sabe interpretar no se ignora en silencio:
                    // el atomo cae al interprete con un diagnostico, no se pierde el filtro.
                    self.unsupported.push(error.to_string());
                    return Err(crate::EvalError::Unsupported(error.to_string()));
                }
            }
        }

        let before = self.atoms.len();
        self.extract(&args[0], &effective)?;
        if !resolved.is_empty() {
            for atom in &mut self.atoms[before..] {
                atom.requires_interpreter = true;
            }
        }
        for modifier in &args[1..] {
            self.extract(modifier, context)?;
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
        let (column, table, expression) = match argument {
            CheckedExpr::Column { column, .. } => (Some(*column), None, None),
            // `COUNTROWS('Fact Sales')` recibe una tabla, no una columna.
            CheckedExpr::Table { table, .. } => (None, Some(*table), None),
            other => (None, None, Some(other.clone())),
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
            table,
            expression,
            // El contexto se propaga UNA vez, aqui. Propagarlo en el interprete lo
            // repropagaria y los filtros modificados por `ALL` volverian a aparecer: la
            // propagacion es idempotente solo si el contexto no cambio entre medias, y
            // con modificadores si cambio.
            context: context.clone(),
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

/// Todas las columnas de un `CheckedExpr`, en orden de aparicion y sin repetir.
pub fn columns_of(expr: &CheckedExpr) -> Vec<ObjectId> {
    let mut out = Vec::new();
    collect_columns(expr, &mut out);
    out
}

fn collect_columns(expr: &CheckedExpr, out: &mut Vec<ObjectId>) {
    match expr {
        CheckedExpr::Column { column, .. } => {
            if !out.contains(column) {
                out.push(*column);
            }
        }
        CheckedExpr::Binary { left, right, .. } => {
            collect_columns(left, out);
            collect_columns(right, out);
        }
        CheckedExpr::Unary { operand, .. } => collect_columns(operand, out),
        CheckedExpr::Call { args, .. } => {
            for arg in args {
                collect_columns(arg, out);
            }
        }
        _ => {}
    }
}
