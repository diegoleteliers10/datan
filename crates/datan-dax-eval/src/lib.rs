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
mod filter;
mod modifiers;

pub use atom::{Atom, AtomKind, AtomSet, Granularity};
pub use filter::{
    ColumnInfo, Filter, FilterContext, FilterOperator, FilterSource, FilterValue,
};
pub use modifiers::{Modifier, ModifierError, apply_modifiers};

use datan_dax_check::{Checked, CheckedExpr, check};
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
    };
    extractor.extract(&checked.expr, &context)?;
    Ok(AtomSet {
        atoms: extractor.atoms,
        grouping: grouping.to_vec(),
    })
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
pub use datan_dax_check::CheckedExpr as _CheckedExpr;
const _: Option<CheckedExpr> = None;