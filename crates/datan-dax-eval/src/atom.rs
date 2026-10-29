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
use datan_dax_syntax::{BinaryOp, UnaryOp};
use datan_model::{Model, ObjectId};

use crate::filter::FilterContext;
use crate::modifiers::Modifier;

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
    Median,
    Variance,
    StandardDeviation,
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
    /// Los modificadores de `CALCULATE` ya resueltos, en orden.
    ///
    /// No son decoracion: deciden el CONTEXTO de evaluacion. Un atomo sin modificadores se
    /// evalua con `contexto del atomo UNIDO al de la celda`; uno con modificadores parte del
    /// contexto de CELDA y se los aplica encima, que es la regla de DAX. Con la union, un
    /// `ALL('dim_product')` agrupado por region devolvia el valor de la region y no el total:
    /// la union le devolvia el filtro que el `ALL` habia quitado.
    pub modifiers: Vec<Modifier>,
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

/// Una expresion sobre atomos.
///
/// Un atomo es una HOJA: `SUM(Qty * Price)`. `Margin = ([Revenue] - [COGS]) / [Revenue]` es
/// una EXPRESION sobre tres hojas, y sin esto el lowering no tiene donde poner la division.
///
/// El indice es a `AtomSet::atoms`, no una referencia: las hojas se deduplican y la misma
/// hoja aparece varias veces en la misma forma. `Revenue` sale dos veces en `Margin` y es UN
/// solo atomo.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Atom(usize),
    Number(f64),
    Binary {
        op: BinaryOp,
        left: Box<Shape>,
        right: Box<Shape>,
    },
    /// Una funcion escalar sobre otras formas: `DIVIDE`, `IF`, `COALESCE`, `SWITCH`.
    ///
    /// Hace falta porque el nivel superior de una medida no tiene por que ser una agregacion.
    /// Sin esta forma, `DIVIDE([A],[B])` no tenia representacion: el conjunto de atomos
    /// quedaba con dos hojas y SIN salidas, el plan emitia una columna por hoja, y el
    /// informe pegaba la primera en la columna de la medida. O sea, el valor de `[A]`, sin
    /// error y sin aviso. El nombre va en MAYUSCULAS, como llega del chequeo.
    Call {
        name: String,
        args: Vec<Shape>,
    },
}

/// Una columna de salida del informe.
///
/// `label` es la etiqueta estable para el cache de planes. `shape` es como se construye el
/// valor a partir de las hojas.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasureOutput {
    pub label: String,
    pub shape: Shape,
}

/// El conjunto de atomos de una medida bajo un contexto y un agrupamiento.
///
/// Si dos medidas del mismo informe producen el mismo conjunto de atomos, comparten la
/// lectura. Es lo que hace que una matriz con 6 columnas y 4 celdas de grupo lea una sola
/// vez la tabla de hechos.
///
/// `outputs` va VACIO cuando la medida no es una simple combinacion de agregaciones: en ese
/// caso el lowering emite una columna por atomo, que es el comportamiento de siempre. La
/// lista plana no se tira: sigue siendo la que el resto del codigo consume para el cache y
/// para el interprete.
#[derive(Debug, Clone, PartialEq)]
pub struct AtomSet {
    pub atoms: Vec<Atom>,
    pub grouping: Vec<ObjectId>,
    pub outputs: Vec<MeasureOutput>,
}

impl AtomSet {
    /// Si alguno de los atomos obliga al interprete.
    pub fn needs_interpreter(&self) -> bool {
        self.atoms.iter().any(Atom::needs_interpreter)
    }

    /// La columna del modelo que alimenta la primera hoja de una forma.
    ///
    /// La columna de SALIDA es `None` cuando la forma combina varios atomos: no hay una sola
    /// columna que la produzca, y adivinar una haria que el grid ofrece un filtro por la
    /// columna equivocada.
    pub fn outputs_atom_column(&self, shape: &Shape) -> Option<ObjectId> {
        match shape {
            Shape::Atom(index) => self.atoms.get(*index).and_then(|atom| atom.column),
            Shape::Number(_) => None,
            Shape::Binary { left, .. } => self.outputs_atom_column(left),
            // Una llamada elige o combina varias hojas: no hay UNA columna que la produzca.
            Shape::Call { .. } => None,
        }
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
    /// Saltos de inlineacion de medidas. Un tope distinto del de agregaciones: inlinear una
    /// medida no cambia el grano, y un `Margin` que referencia `[Revenue]` dos veces ya
    /// gasta dos saltos sin que haya nada anidado.
    pub(crate) measure_hops: usize,
    /// Modificadores del `CALCULATE` que se esta recorriendo, en orden de aplicacion.
    ///
    /// Los estampa `emit` en cada atomo que nace dentro del cuerpo. Es lo que distingue dos
    /// hojas con la MISMA etiqueta y el MISMO contexto efectivo: en un modelo sin filtros,
    /// `[Qty]` y `CALCULATE([Qty], ALL('dim_product'))` son los dos `SUM(qty)` con contexto
    /// vacio, y la deduplicacion los fundia en uno. La resta contra el total daba cero.
    pub(crate) pending_modifiers: Vec<Modifier>,
}

impl Extractor<'_> {
    /// Convierte la expresion de una medida en una forma sobre atomos.
    ///
    /// Devuelve `None` cuando la medida no es una combinacion aritmetica de agregaciones: un
    /// `CALCULATE`, un iterador, un `IF`. En ese caso no hay forma y el modelo se queda con
    /// la lista plana de atomos, que es lo que hace hoy y lo que resuelve el interprete.
    ///
    /// Una referencia a medida se INLINA: `Revenue` dentro de `Margin` es la forma de la
    /// definicion de `Revenue`, no un nodo opaco. Sin el inlining, `Margin` seria un arbol de
    /// referencias y el lowering tendria que resolverlas en SQL, que es lo que no se quiere.
    pub fn shape_of(&mut self, expr: &CheckedExpr, context: &FilterContext) -> Option<Shape> {
        match expr {
            CheckedExpr::Number { value, .. } => Some(Shape::Number(*value)),

            CheckedExpr::Binary {
                op, left, right, ..
            } => Some(Shape::Binary {
                op: *op,
                left: Box::new(self.shape_of(left, context)?),
                right: Box::new(self.shape_of(right, context)?),
            }),

            // `-x` se baja como `0 - x`. Cualquier otra forma de representarlo obligaria a
            // los dos caminos a tratar el menos unario como una resta por otra rama.
            // Sin este caso, `DIVIDE(a, b, -1)` no tenia forma y caia al camino viejo.
            CheckedExpr::Unary {
                op: UnaryOp::Negate,
                operand,
                ..
            } => Some(Shape::Binary {
                op: BinaryOp::Subtract,
                left: Box::new(Shape::Number(0.0)),
                right: Box::new(self.shape_of(operand, context)?),
            }),

            // Una variable ya resuelta es su cuerpo. `VAR x = SUM(a) RETURN x` vale lo que
            // `x`, y sin esto la medida perdia la forma y el informe pegaba la primera hoja
            // en su columna: el valor de `SUM(a)`, sin error y sin aviso.
            CheckedExpr::Variable { value, .. } => self.shape_of(value, context),

            // `VAR`/`RETURN` es un bloque: vale lo que su cuerpo. Las ligaduras no tienen
            // efecto, y las referencias ya vienen resueltas a su valor, asi que no hay ningun
            // orden que respetar.
            CheckedExpr::Block { body, .. } => self.shape_of(body, context),

            CheckedExpr::Call { name, args, .. } => {
                // Una funcion que no es agregacion no nace un atomo: combina los que ya hay.
                // Sin este caso, `DIVIDE([A],[B])` no tenia forma y el conjunto de atomos
                // quedaba con dos hojas y ninguna salida.
                let Some(kind) = aggregation_of(name) else {
                    return self.scalar_call(name, args, context);
                };
                // Los iteradores cambian de verdad el grano: `SUMX(VALUES(A), ...)`
                // recalcula por fila. Igual son UNA hoja: la agregacion sobre la
                // expresion. Antes no tenian forma, la medida caia al camino viejo y el
                // informe pegaba la primera hoja en la columna de la medida.
                if matches!(kind, AtomKind::Expression) {
                    let before = self.atoms.len();
                    self.extract_iterator(name, args, context).ok()?;
                    if self.atoms.len() == before {
                        return None;
                    }
                    return Some(Shape::Atom(before));
                }
                let argument = args.first()?;
                let before = self.atoms.len();
                let label = format!("{name}({})", self.label_of(argument));
                if self
                    .emit_dedup(kind, argument, context, name, &label)
                    .is_err()
                {
                    return None;
                }
                if self.atoms.len() > before {
                    return Some(Shape::Atom(before));
                }
                // No se anadio nada: la hoja ya existia. Se busca por etiqueta y contexto,
                // porque `SUM(Qty)` con dos filtros distintos son dos hojas. Los
                // modificadores entran en la comparacion por la misma razon.
                Some(Shape::Atom(self.atoms.iter().position(|atom| {
                    atom.label == label
                        && atom.context == *context
                        && atom.kind == kind
                        && atom.modifiers == self.pending_modifiers
                })?))
            }

            // `CALCULATE` no llega como `Call`: el chequeo lo emite como `Calculate`, porque
            // sus modificadores tienen semantica propia. Su cuerpo bajo el contexto
            // MODIFICADO si tiene forma.
            CheckedExpr::Calculate { args, .. } => self.calculate_shape(args, context),

            CheckedExpr::Measure { measure, .. } => {
                let definition = self.model.measures.iter().find(|m| m.id == *measure)?;
                if self.measure_hops > 32 {
                    return None;
                }
                let parsed = datan_dax_syntax::parse(&definition.expression).ok()?;
                let checked = datan_dax_check::check_expr(&parsed, self.model).ok()?;

                // La profundidad de AGREGACION se reinicia al inlinear una medida.
                //
                // `depth` marca "hay una agregacion dentro de otra", que es lo que rompe el
                // fast path porque recalcula por fila. `Margin = ([Revenue] - [COGS]) /
                // [Revenue]` no es eso: es aritmetica entre tres agregaciones al MISMO nivel.
                // Con la profundidad incrementada, las tres hojas salian con
                // `Granularity::Finer` y caian al interprete sin motivo.
                //
                // Un iterador dentro del cuerpo de la medida SI cambia el grano, y por eso
                // `shape_of` rechaza los iteradores mas arriba: no llegan hasta aqui.
                let saved = std::mem::replace(&mut self.depth, 0);
                self.measure_hops += 1;
                let shape = self.shape_of(&checked.expr, context);
                self.measure_hops -= 1;
                self.depth = saved;
                shape
            }

            _ => None,
        }
    }

    /// La forma de un `CALCULATE`: la de su cuerpo bajo el contexto ya modificado.
    ///
    /// Es lo que ya hace `extract_calculate`, con una diferencia: devuelve la FORMA en vez de
    /// dejar la lista plana. `CALCULATE([Qty], ALL('dim_product'))` vale lo que `SUM(qty)` sin
    /// el filtro de la dimension, y eso es una hoja con su contexto. Sin este caso la medida
    /// quedaba sin forma y el informe pegaba la primera hoja en su columna.
    ///
    /// Un modificador que no se resuelve a un filtro concreto devuelve `None`, y entonces el
    /// camino es el viejo: `extract` vuelve a correr y reporta el error. No se pierde el
    /// filtro en silencio.
    fn calculate_shape(&mut self, args: &[CheckedExpr], context: &FilterContext) -> Option<Shape> {
        let body = args.first()?;
        let mut effective = context.clone();
        let mut resolved = Vec::new();
        for modifier in &args[1..] {
            let modifier = crate::modifiers::resolve_modifier(modifier).ok()?;
            effective = crate::modifiers::apply_modifiers(
                self.model,
                &effective,
                std::slice::from_ref(&modifier),
            )
            .ok()?;
            resolved.push(modifier);
        }

        let before = self.atoms.len();
        // Los modificadores viajan con el atomo desde que nace, estampados por `emit`.
        // Se APILAN: un CALCULATE dentro de otro aplica los de fuera y luego los de
        // dentro, que es el orden con el que `apply_modifiers` los re-aplicara sobre la
        // celda. Sin el apilado, el cuerpo de un CALCULATE interior perderia los de fuera.
        let saved = std::mem::take(&mut self.pending_modifiers);
        self.pending_modifiers = saved.iter().chain(resolved.iter()).cloned().collect();
        let shape = self.shape_of(body, &effective);
        self.pending_modifiers = saved;
        let shape = shape?;
        // Un `CALCULATE` con modificadores no baja a SQL: cambia el filtro de forma no
        // uniforme. La marca manda la medida al interprete.
        if !resolved.is_empty() {
            for atom in &mut self.atoms[before..] {
                atom.requires_interpreter = true;
            }
        }
        // Los modificadores tambien se recorren, igual que en `extract_calculate`: un
        // `KEEPFILTERS` lleva una expresion dentro.
        for modifier in &args[1..] {
            self.extract(modifier, context).ok()?;
        }
        Some(shape)
    }

    /// La forma de una funcion escalar sobre las formas de sus argumentos.
    ///
    /// Solo las funciones que los DOS caminos saben calcular. Cualquier otra devuelve `None`,
    /// y `None` es lo correcto: una forma a medias seria peor que no tener forma, porque el
    /// plan la tomaria por valida y el numero saldria de la hoja equivocada.
    ///
    /// Las comparaciones entran: `IF([A] > 0, ...)` necesita que la condicion tenga forma, y
    /// el lowering la baja a un predicado de SQL y el interprete a un booleano.
    fn scalar_call(
        &mut self,
        name: &str,
        args: &[CheckedExpr],
        context: &FilterContext,
    ) -> Option<Shape> {
        // El rango de argumentos, para no aceptar una llamada que despues no se sabe
        // calcular. `IF` y `DIVIDE` tienen un tercer argumento opcional.
        let allowed = match name {
            "DIVIDE" | "IF" => (2, 3),
            "COALESCE" => (1, usize::MAX),
            "SWITCH" => (3, usize::MAX),
            _ => return None,
        };
        if args.len() < allowed.0 || args.len() > allowed.1 {
            return None;
        }
        let args = args
            .iter()
            .map(|arg| self.shape_of(arg, context))
            .collect::<Option<Vec<_>>>()?;
        Some(Shape::Call {
            name: name.to_string(),
            args,
        })
    }

    /// Emite una hoja, o no hace nada si ya existe una identica.
    ///
    /// La deduplicacion compara etiqueta, contexto y tipo de agregado. Contexto es lo que
    /// importa: dos `SUM(Qty)` con filtros distintos NO son la misma hoja, y compartirlas
    /// haria que una medida con `CALCULATE` mostrara el total sin filtrar.
    fn emit_dedup(
        &mut self,
        kind: AtomKind,
        argument: &CheckedExpr,
        context: &FilterContext,
        function: &str,
        label: &str,
    ) -> Result<(), crate::EvalError> {
        let exists = self.atoms.iter().any(|atom| {
            atom.label == *label
                && atom.context == *context
                && atom.kind == kind
                && atom.modifiers == self.pending_modifiers
        });
        if exists {
            return Ok(());
        }
        self.emit(kind, argument, context, function)?;
        // `emit` calcula la etiqueta con su propia regla; se fuerza la compartida para que la
        // deduplicacion de la forma y la de `emit` no se contradigan.
        if let Some(atom) = self.atoms.last_mut() {
            atom.label = label.to_string();
        }
        Ok(())
    }

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
                let Some(definition) = self.model.measures.iter().find(|m| m.id == *measure) else {
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
        // El chequeo deja el iterador con UN argumento: el cuerpo, ya en contexto de fila.
        // La tabla queda en `table_expr`, y el ancla del atomo sale de las columnas del
        // cuerpo. Antes se esperaban dos argumentos y con uno el atomo no salia NUNCA: una
        // medida `SUMX` se quedaba sin hojas y sin salida, sin error.
        let Some(body) = args.first() else {
            return Ok(());
        };
        // El atomo es la agregacion sobre la expresion del iterador. Se emite con `depth`
        // incrementado, asi que si el iterador esta dentro de otro atomo, el interno queda
        // marcado como `Finer` y cae al interprete.
        let kind = aggregation_of(name).unwrap_or(AtomKind::Expression);
        self.depth += 1;
        self.emit(kind, body, context, name)?;
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
        // Los modificadores viajan con el atomo desde que nace, estampados por `emit`.
        // Se APILAN, igual que en `calculate_shape`: los de fuera primero, los de dentro
        // despues.
        let saved = std::mem::take(&mut self.pending_modifiers);
        self.pending_modifiers = saved.iter().chain(resolved.iter()).cloned().collect();
        self.extract(&args[0], &effective)?;
        self.pending_modifiers = saved;
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
            modifiers: self.pending_modifiers.clone(),
        });
        Ok(())
    }

    fn label_of(&self, expr: &CheckedExpr) -> String {
        match expr {
            CheckedExpr::Column { name, .. } => name.clone(),
            CheckedExpr::Measure { name, .. } => name.clone(),
            CheckedExpr::Binary {
                left, right, op, ..
            } => {
                format!(
                    "({} {} {})",
                    self.label_of(left),
                    op.as_str(),
                    self.label_of(right)
                )
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
        "MEDIAN" => AtomKind::Median,
        "VAR.S" => AtomKind::Variance,
        "STDEV.S" => AtomKind::StandardDeviation,
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
