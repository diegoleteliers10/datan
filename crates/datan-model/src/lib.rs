//! Modelo semantico de Datan: tablas, columnas, lineage, relaciones, jerarquias, medidas, RLS.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. Ver seccion 4 de PLAN.md.
//!
//! Este crate arranca vacio. El primer contenido llega con la Fase 1.

/// Identificador estable de un objeto del modelo.
///
/// Power BI usa lineage tags (UUID) para que un filtro solo se propague a columnas del mismo
/// linaje. Datan necesita lo mismo desde el dia 1, porque el algebra de filter context de la
/// Fase 2 depende de ello. Sin tags, propagar filtros entre tablas es ambiguo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub u64);

/// Direccion en que unarelationship filtra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterDirection {
    /// La dimension filtra la tabla de hechos. Es el default de Power BI.
    Single,
    /// Ambos lados filtran. Produce ciclos si el grafo se encadena.
    Both,
}

/// Cardinalidad de una relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}

/// Una relationship entre dos columnas.
///
/// Nota de nomenclatura: `Relationship` esta en singular porque es una. El nombre del
/// proyecto usa terminologia en ingles para el dominio tecnico (table, column, measure) y
/// castellano para el resto.
#[derive(Debug, Clone)]
pub struct Relationship {
    pub from_column: ObjectId,
    pub to_column: ObjectId,
    pub cardinality: Cardinality,
    pub direction: FilterDirection,
    /// Una relationship inactiva no filtra por defecto. Se activa con `USERELATIONSHIP`.
    pub is_active: bool,
    /// Power BI exige `AssumeReferentialIntegrity` para propagar por una relationship
    /// many-to-many en lugar de resolver por materializacion.
    pub assume_referential_integrity: bool,
}