//! Mapeo entre el modelo logico y el SQL fisico.
//!
//! El modelo no sabe donde vive cada tabla. Un `ObjectId` es estable y legible; el nombre de
//! la tabla en DuckDB cambia con el proyecto. Separar las dos cosas permite que el modelo se
//! guarde en git sin depender de la conexion, y que la app cambie de base sin reescribir el
//! modelo.

use datan_model::{Model, ObjectId};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SourceMapError {
    #[error("la tabla del modelo {0} no tiene nombre fisico")]
    MissingTable(ObjectId),
    #[error("la columna del modelo {0} no tiene nombre fisico")]
    MissingColumn(ObjectId),
}

/// El mapeo logico a fisico.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    tables: HashMap<ObjectId, String>,
    columns: HashMap<ObjectId, String>,
    /// De que tabla es cada columna. Se necesita para el semijoin de un filtro propagado.
    column_owners: HashMap<ObjectId, ObjectId>,
}

impl SourceMap {
    /// Mapea los nombres del modelo tal cual. Es el caso por defecto: la tabla `Dim Product`
    /// vive en una tabla de DuckDB llamada `Dim Product`.
    ///
    /// Conveniente, y lo que hace el spike. En una app real el mapeo sale de la conexion.
    pub fn from_model(model: &Model) -> Self {
        let mut map = Self::default();
        for table in &model.tables {
            map.tables.insert(table.id, table.name.clone());
            for column in &table.columns {
                map.columns.insert(column.id, column.name.clone());
                map.column_owners.insert(column.id, table.id);
            }
        }
        map
    }

    /// Sobreescribe el nombre fisico de una tabla. Para renombrar en la conexion sin tocar
    /// el modelo.
    pub fn with_table(mut self, table: ObjectId, physical: impl Into<String>) -> Self {
        self.tables.insert(table, physical.into());
        self
    }

    /// Sobreescribe el nombre fisico de una columna.
    pub fn with_column(mut self, column: ObjectId, physical: impl Into<String>) -> Self {
        self.columns.insert(column, physical.into());
        self
    }

    /// El id de la tabla que contiene una columna.
    pub fn owner_of(&self, column: ObjectId) -> Option<ObjectId> {
        self.column_owners.get(&column).copied()
    }

    /// El nombre fisico de una tabla y su id.
    pub fn table(&self, table: ObjectId) -> Result<(&str, ObjectId), SourceMapError> {
        self.tables
            .get(&table)
            .map(|name| (name.as_str(), table))
            .ok_or(SourceMapError::MissingTable(table))
    }

    /// El nombre fisico de una columna, y el id de la TABLA que la contiene.
    ///
    /// El segundo valor es el id de la tabla, no el de la columna. Es lo que hace falta para
    /// resolver el `IN` de un filtro propagado, que necesita saber de que tabla sale el
    /// subselect. Devolver el id de la columna aqui es un bug silencioso que aparece dos
    /// niveles mas abajo, cuando el `SourceMap` busca una tabla que no existe.
    pub fn column(&self, column: ObjectId) -> Result<(&str, ObjectId), SourceMapError> {
        let name = self
            .columns
            .get(&column)
            .ok_or(SourceMapError::MissingColumn(column))?;
        let table = self
            .column_owners
            .get(&column)
            .copied()
            .ok_or(SourceMapError::MissingColumn(column))?;
        Ok((name.as_str(), table))
    }
}