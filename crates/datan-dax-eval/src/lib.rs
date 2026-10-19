//! Algebra de filter context, propagacion por relaciones y extraccion de atomos.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. `cargo test` corre en milisegundos.
//!
//! Este crate es donde vive la semantica de DAX. Todo lo demas es plumbing.
//!
//! # La idea que lo hace tratable
//!
//! Un filtro es un predicado sobre una columna, no un conjunto de filas. Eso hace que un
//! contexto de filtro sea pequeno: unas pocas columnas con valores.
//!
//! El engine de VertiPaq tiene dos niveles: compila DAX simple a xmSQL y ejecuta eso en el
//! motor de almacenamiento, y cae a un motor de formulas cuando no puede. Ese es el mismo
//! corte que hace Datan, y por la misma razon: Microsoft ya lo dibujo y es el unico split
//! empiricamente validado del mundo DAX.
//!
//! # Donde el backend SQL se rinde
//!
//! Tres cosas no se pueden expresar en SQL:
//!
//! 1. `CROSSFILTER(BOTH)` sobre un grafo ciclico. SQL exige arbol de joins aciclico.
//! 2. Cambio de granularidad no acotado: `SUMX(VALUES(A), CALCULATE([Revenue], REMOVEFILTERS(A)))`
//!    agrupado por una dimension mas gruesa.
//! 3. Agregado de agregado bajo filter context que varia por fila.
//!
//! Cuando aparece cualquiera de las tres, el atomo no se resuelve con SQL y lo lleva el
//! interprete por celda de grupo.

mod atom;
mod cycle;
mod time;
mod filter;
mod interp;
mod modifiers;

pub use atom::{Atom, AtomKind, AtomSet, Granularity, MeasureOutput, Shape};
pub use filter::{
    distinct_propagation_results, propagate_ordered,
    ColumnInfo, Filter, FilterContext, FilterOperator, FilterSource, FilterValue,
};
pub use interp::{
    CellResult, ColumnView, DataSource, DateColumn, IntColumn, InterpError, Interpreter, MemorySource,
    Scalar,
    TextColumn,
};
pub use modifiers::{Modifier, ModifierError, apply_modifiers};
pub use cycle::has_bidirectional_cycle;
pub use time::{
    CalendarUnit, DateRange, TimeError, TimeFunction, date_column, filters, format_date,
    parse_day, shift,
};

use datan_dax_check::{Checked, check};
use datan_model::Model;

/// Error al evaluar una expresion de medidas.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("error de sintaxis o de resolucion: {0}")]
    Syntax(#[from] datan_dax_check::CheckError),

    #[error("la expresion no es una medida: {0}")]
    NotAMeasure(String),

    #[error("no se puede extraer un atomo de {0}")]
    Unsupported(String),

    #[error("error de modificador: {0}")]
    Modifier(#[from] ModifierError),
}

pub type Result<T> = std::result::Result<T, EvalError>;

/// Chequea y normaliza una medida bajo un contexto de filtro y un agrupamiento.
///
/// Devuelve el DAG de atomos. Cada atomo es una agregacion evaluada bajo un conjunto de
/// filtros *concreto y resuelto*. El fallback por celda de grupo lo consume sin volver a
/// razonar sobre filtros.
pub fn extract_atoms(
    expression: &str,
    model: &Model,
    context: &FilterContext,
    grouping: &[datan_model::ObjectId],
) -> Result<AtomSet> {
    let checked = check(expression, model)?;
    let context = context.propagate(model);
    let mut extractor = atom::Extractor {
        model,
        grouping: grouping.to_vec(),
        atoms: Vec::new(),
        depth: 0,
        unsupported: Vec::new(),
        measure_hops: 0,
        pending_modifiers: Vec::new(),
    };
    // Primero se intenta la forma: una medida que es aritmetica entre agregaciones produce
    // una columna de salida, no una lista plana.
    //
    // Se intenta ANTES de `extract` a proposito. `extract` recorre en anchura y pierde la
    // estructura: de `([Revenue] - [COGS]) / [Revenue]` saca tres hojas y descarta que estan
    // unidas por una division. Si se corriera primero, `shape_of` veria un `Extractor` con
    // las hojas ya a馃ran.
    let shape = extractor.shape_of(&checked.expr, &context);

    let outputs = match shape {
        Some(shape) => vec![MeasureOutput {
            // La etiqueta de la forma es la de la hoja para una medida simple, y hay que
            // distinguirla de la medida que la envuelve. Se usa la DE LA EXPRESION: asi el
            // cache de planes distingue `Margin` de `Revenue` aunque compartan hojas.
            label: shape_label(&checked.expr),
            shape,
        }],
        None => {
            // No hay forma: la medida tiene un `CALCULATE`, un iterador o un `IF`. Se vuelve
            // a la lista plana, que es lo de siempre, y las salidas vacias le dicen al
            // lowering que emita una columna por atomo.
            extractor.atoms.clear();
            extractor.extract(&checked.expr, &context)?;
            Vec::new()
        }
    };
    // Cada atomo lleva el contexto bajo el que fue extraido. La propagacion ocurre UNA
    // vez, al extraer, para que un `ALL` de un `CALCULATE` no se deshaga al evaluar.
    let atoms = extractor
        .atoms
        .into_iter()
        .map(|atom| Atom {
            context: atom.context.propagate(model),
            ..atom
        })
        .collect();
    Ok(AtomSet {
        atoms,
        grouping: grouping.to_vec(),
        outputs,
    })
}

/// Una etiqueta estable para una expresion de medida.
///
/// Para `SUM(Qty)` devuelve `SUM(Qty)`. Para `([Revenue] - [COGS]) / [Revenue]` devuelve el
/// texto de la expresion completa, no el de la primera hoja: dos medidas con las mismas
/// hojas y distinta aritmetica dan numeros distintos y no pueden compartir la etiqueta del
/// cache.
fn shape_label(expr: &CheckedExpr) -> String {
    match expr {
        CheckedExpr::Binary { op, left, right, .. } => {
            format!("({} {} {})", shape_label(left), op.as_str(), shape_label(right))
        }
        CheckedExpr::Call { name, args, .. } => {
            let inner: Vec<String> = args.iter().take(1).map(shape_label).collect();
            format!("{name}({})", inner.join(", "))
        }
        CheckedExpr::Measure { name, .. } => name.clone(),
        CheckedExpr::Column { name, .. } => name.clone(),
        CheckedExpr::Number { value, .. } => value.to_string(),
        // Un `CALCULATE` lleva sus modificadores en la etiqueta: dos medidas con el mismo
        // cuerpo y distinto `ALL` dan numeros distintos y no pueden compartir clave de plan.
        CheckedExpr::Calculate { args, .. } => {
            let inner: Vec<String> = args.iter().map(shape_label).collect();
            format!("CALCULATE({})", inner.join(", "))
        }
        // La etiqueta de una variable es la de su cuerpo: el nombre local no distingue una
        // medida de otra, y dos medidas con el mismo cuerpo no pueden compartir etiqueta si
        // el cuerpo es distinto.
        CheckedExpr::Variable { value, .. } => shape_label(value),
        CheckedExpr::Unary { operand, .. } => format!("-({})", shape_label(operand)),
        other => format!("{other:?}"),
    }
}

/// La forma completa: parsea, chequea y extrae. Para tests y para el playground.
pub fn evaluate(
    expression: &str,
    model: &Model,
    context: &FilterContext,
    grouping: &[datan_model::ObjectId],
) -> Result<Checked> {
    let _ = extract_atoms(expression, model, context, grouping)?;
    check(expression, model).map_err(EvalError::from)
}

/// Indica si una expresion necesita el interprete en vez del fast path SQL.
///
/// Un `true` aqui significa que la consulta va a caer en el motor de formulas. Es la metrica
/// que decide, al final de la Fase 2, si el backend SQL se queda o se borra. Ver PLAN.md,
/// seccion 10.
pub fn needs_interpreter(
    expression: &str,
    model: &Model,
    context: &FilterContext,
    grouping: &[datan_model::ObjectId],
) -> bool {
    match extract_atoms(expression, model, context, grouping) {
        Ok(atoms) => atoms.needs_interpreter(),
        Err(_) => true,
    }
}

/// Reexportado por comodidad para el extractor.
pub use datan_dax_check::CheckedExpr;
pub use datan_dax_syntax::{BinaryOp, UnaryOp};
const _: Option<CheckedExpr> = None;