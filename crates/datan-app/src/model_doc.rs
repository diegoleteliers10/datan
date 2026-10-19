//! El modelo del usuario: lo que se agrega encima de lo que devuelve la introspeccion.
//!
//! # Por que hace falta
//!
//! La introspeccion se vuelve a correr cuando se conecta: lee el catalogo de Postgres y
//! devuelve lo que hay. Una medida escrita por el usuario, no. Si vive solo en RAM, crear una
//! medida y guardar el informe produce un archivo que no abre: al reabrir, la medida no esta
//! en la base, la resolucion por nombre falla y el informe muere con `UnknownMeasure`.
//!
//! El modelo del usuario se guarda aparte, por base, y se vuelve a aplicar encima de la
//! introspeccion cada vez que se conecta. Un informe guardado guarda NOMBRES, y este archivo
//! es donde esos nombres existen fuera del proceso.
//!
//! # Formato
//!
//! TOML en `~/.datan/models/<base>.toml`. Un archivo por base, como las conexiones. El
//! archivo se puede leer y editar a mano: son medidas, ajustes de columnas, columnas
//! calculadas y jerarquias, nada mas.
//!
//! # Que gana la introspeccion y que gana el documento
//!
//! Gana la introspeccion. Una medida que ya existe en el modelo (la genero la introspeccion,
//! o ya se aplico) no se toca. Aplicar el documento dos veces no duplica nada, y un nombre
//! que la base ya trae no se pisa.

use std::path::{Path, PathBuf};

use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole,
};
use serde::{Deserialize, Serialize};

/// El archivo, con las dos listas que el formato TOML exige en la raiz.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    measure: Vec<MeasureDoc>,
    #[serde(default)]
    column: Vec<ColumnDoc>,
    #[serde(default)]
    jerarquia: Vec<JerarquiaDoc>,
    #[serde(default)]
    relacion: Vec<RelationshipDoc>,
}

/// Una medida del usuario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasureDoc {
    pub name: String,
    /// Texto DAX sin parsear, tal como lo escribio el usuario.
    pub expression: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl MeasureDoc {
    pub fn new(name: impl Into<String>, expression: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            expression: expression.into(),
            format_string: None,
            description: None,
        }
    }
}

/// Una columna del documento: un ajuste sobre una que ya existe, o una calculada nueva.
///
/// Los campos de ajuste son `Option` a proposito: `None` es "no lo toco". Un `false` en
/// `is_hidden` significa que el usuario la mostro a mano, y no es lo mismo que no haber
/// dicho nada.
///
/// ```toml
/// # Un ajuste: la columna ya esta en la base.
/// [[column]]
/// table = "catalogo_productos"
/// column = "categoria"
/// sort_by = "categoria_orden"
///
/// # Una columna calculada: la declara este archivo, no la base.
/// [[column]]
/// table = "boleta"
/// column = "anio"
/// expression = "YEAR('boleta'[fecha])"
/// data_type = "entero"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDoc {
    pub table: String,
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_hidden: Option<bool>,
    /// El texto DAX de una columna calculada. Se evalua FILA A FILA sobre su tabla.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    /// El tipo de una columna calculada. Solo hace falta al crearla: una que ya existe tiene
    /// el suyo. Sin el, la columna queda como texto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
}

impl ColumnDoc {
    /// Una columna calculada: no esta en la base, la define el documento.
    pub fn calculada(
        table: impl Into<String>,
        column: impl Into<String>,
        expression: impl Into<String>,
        data_type: impl Into<String>,
    ) -> Self {
        Self {
            table: table.into(),
            column: column.into(),
            sort_by: None,
            is_hidden: None,
            expression: Some(expression.into()),
            data_type: Some(data_type.into()),
        }
    }
}

/// Una relationship del documento.
///
/// Dos usos, y el mismo tipo para los dos: ajustar la que infirio el catalogo, o declarar una
/// que el catalogo no puede ver. La introspeccion solo lee foreign keys, y una base con datos
/// reales casi nunca las tiene completas: sin esto, agrupar por una dimension que no declara
/// la constraint falla con "no hay relationship entre la tabla de hechos y la tabla 2".
///
/// `from` es el lado UNO (la clave de la dimension) y `to` el lado MUCHOS (la clave foranea
/// de los hechos). Si estan al reves, el filtro propaga en la direccion equivocada y los
/// numeros salen mal sin dar error.
///
/// ```toml
/// [[relacion]]
/// from_table = "catalogo_productos"
/// from_column = "id"
/// to_table = "product_boleta"
/// to_column = "product_id"
/// # Opcionales. Sin ellos, activa y de la dimension a los hechos.
/// role = "activa"     # activa | inactiva
/// direction = "simple" # simple | ambas | ninguna
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationshipDoc {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

impl RelationshipDoc {
    pub fn new(
        from_table: impl Into<String>,
        from_column: impl Into<String>,
        to_table: impl Into<String>,
        to_column: impl Into<String>,
    ) -> Self {
        Self {
            from_table: from_table.into(),
            from_column: from_column.into(),
            to_table: to_table.into(),
            to_column: to_column.into(),
            role: None,
            direction: None,
        }
    }

    /// La misma relacion, con el rol ya escrito como texto.
    pub fn con_rol(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    pub fn activa(&self) -> bool {
        !self
            .role
            .as_deref()
            .is_some_and(|role| role.eq_ignore_ascii_case("inactiva"))
    }
}

/// El documento. Se lee, se edita y se escribe entero: es un archivo de decenas de lineas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelDoc {
    pub measures: Vec<MeasureDoc>,
    pub columns: Vec<ColumnDoc>,
    pub jerarquias: Vec<JerarquiaDoc>,
    pub relaciones: Vec<RelationshipDoc>,
}

/// Una jerarquia del usuario: niveles ordenados de UNA misma tabla.
///
/// El drill reemplaza el nivel del informe por el siguiente o el anterior, y para eso hace
/// falta saber QUE columnas forman una jerarquia y en QUE orden. La base no lo declara: es
/// una decision del usuario, como las medidas.
///
/// ```toml
/// [[jerarquia]]
/// name = "Producto"
/// table = "catalogo_productos"
/// levels = ["categoria", "nombre"]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JerarquiaDoc {
    pub name: String,
    pub table: String,
    /// Los niveles, de FUERA hacia dentro.
    pub levels: Vec<String>,
}

impl JerarquiaDoc {
    /// Los ids de los niveles, o el motivo por el que no se puede usar.
    ///
    /// Una jerarquia con MENOS de dos niveles no taladra nada y se avisa: declarar una es
    /// pedir el boton de drill, y con un nivel el boton no hace nada.
    pub fn resolver(&self, model: &Model) -> Result<Vec<ObjectId>, String> {
        if self.levels.len() < 2 {
            return Err(format!(
                "la jerarquia `{}` necesita al menos dos niveles",
                self.name
            ));
        }
        let Some(table) = model.table_by_name(&self.table) else {
            return Err(format!(
                "la jerarquia `{}` habla de la tabla `{}`, que no esta en el modelo",
                self.name, self.table
            ));
        };
        let mut niveles = Vec::with_capacity(self.levels.len());
        for nombre in &self.levels {
            let Some(column) = table.column_by_name(nombre) else {
                return Err(format!(
                    "la jerarquia `{}` usa la columna `{}[{}]`, que no existe",
                    self.name, self.table, nombre
                ));
            };
            niveles.push(column.id);
        }
        Ok(niveles)
    }
}

/// Un error del documento del modelo.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("no se pudo leer {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no se pudo escribir {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} no es un TOML valido: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl ModelDoc {
    /// El archivo de una base. `~/.datan/models/<base>.toml`.
    pub fn path(database: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".datan")
            .join("models")
            .join(format!("{database}.toml"))
    }

    /// Carga el documento de una base. Si no existe, devuelve el vacio.
    ///
    /// Un documento corrupto NO se sobreescribe: se devuelve el error. La medida que el
    /// usuario escribio es trabajo que no se puede volver a generar.
    pub fn load(database: &str) -> Result<Self, DocError> {
        Self::load_from(&Self::path(database))
    }

    /// Escribe el documento de una base.
    pub fn save(&self, database: &str) -> Result<(), DocError> {
        self.save_to(&Self::path(database))
    }

    pub fn load_from(path: &Path) -> Result<Self, DocError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(DocError::Read {
                    path: path.to_path_buf(),
                    source: error,
                });
            }
        };

        // El TOML no acepta un array en la raiz: las dos listas van dentro de una tabla.
        let file: File = toml::from_str(&text).map_err(|source| DocError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            measures: file.measure,
            columns: file.column,
            jerarquias: file.jerarquia,
            relaciones: file.relacion,
        })
    }

    pub fn save_to(&self, path: &Path) -> Result<(), DocError> {
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return Err(DocError::Write {
                path: parent.to_path_buf(),
                source: error,
            });
        }

        let file = File {
            measure: self.measures.clone(),
            column: self.columns.clone(),
            jerarquia: self.jerarquias.clone(),
            relacion: self.relaciones.clone(),
        };
        let text = toml::to_string_pretty(&file).map_err(|source| DocError::Write {
            path: path.to_path_buf(),
            source: std::io::Error::other(source.to_string()),
        })?;
        std::fs::write(path, text).map_err(|source| DocError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Agrega una medida, o reemplaza la que ya tiene ese nombre.
    ///
    /// Reemplazar mantiene la POSICION: el archivo se lee como una lista de lo que hay, y
    /// mover una medida editada al final haria que el diff de una linea tocara dos.
    pub fn upsert_measure(&mut self, measure: MeasureDoc) {
        match self
            .measures
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(&measure.name))
        {
            Some(existing) => *existing = measure,
            None => self.measures.push(measure),
        }
    }

    /// Agrega una relationship, o reemplaza la que ya tiene ese par de columnas.
    ///
    /// Es lo que usa el interruptor de la app: la relacion ya existe (la infirio el catalogo)
    /// y lo unico que cambia es el rol. Se guarda el par, no un id, para que el archivo siga
    /// diciendo lo mismo si la introspeccion renumera.
    pub fn upsert_relationship(&mut self, relationship: RelationshipDoc) {
        let same = |existing: &RelationshipDoc| {
            existing.from_table.eq_ignore_ascii_case(&relationship.from_table)
                && existing.from_column.eq_ignore_ascii_case(&relationship.from_column)
                && existing.to_table.eq_ignore_ascii_case(&relationship.to_table)
                && existing.to_column.eq_ignore_ascii_case(&relationship.to_column)
        };
        match self.relaciones.iter_mut().find(|existing| same(existing)) {
            Some(existing) => *existing = relationship,
            None => self.relaciones.push(relationship),
        }
    }

    /// Aplica el documento encima del modelo y devuelve lo que no se pudo aplicar.
    ///
    /// Las COLUMNAS van primero. Una columna calculada no existe en la base hasta que este
    /// documento la crea, y una medida que la suma tiene que encontrarla. Al reves, la medida
    /// fallaria con "esa columna no existe" por un motivo que no es del usuario.
    ///
    /// Cada pieza se chequea contra el modelo EN SU ESTADO ACTUAL, que ya incluye lo aplicado
    /// antes: una medida que referencia otra del documento, o una calculada que usa otra,
    /// chequean igual que si todas estuvieran escritas en el modelo.
    ///
    /// Lo que no se pudo aplicar no se borra del documento. La columna puede volver: renombrar
    /// algo en la base no es razon para tirar la medida que lo usaba.
    pub fn apply(&self, model: &mut Model) -> Vec<String> {
        let mut avisos = Vec::new();

        for doc in &self.columns {
            // Una columna CALCULADA: la declara el documento y no esta en la base. Se
            // chequea en contexto de FILA sobre su tabla, que es donde se va a evaluar, y se
            // agrega al final de su tabla. El orden importa: una calculada que usa otra ya
            // aplicada chequea igual que si las dos estuvieran escritas en el modelo.
            if let Some(expression) = &doc.expression {
                let Some(table) = model.table_by_name(&doc.table) else {
                    avisos.push(format!(
                        "la columna calculada `{}`[{}] habla de una tabla que no esta en el modelo",
                        doc.table, doc.column
                    ));
                    continue;
                };
                if table.column_by_name(&doc.column).is_some() {
                    avisos.push(format!(
                        "la columna calculada `{}`[{}] no se aplico: esa columna ya existe",
                        doc.table, doc.column
                    ));
                    continue;
                }
                let table_id = table.id;
                let checked = match datan_dax_check::check_row(expression, model, table_id) {
                    Ok(checked) => checked,
                    Err(error) => {
                        avisos.push(format!(
                            "la columna calculada `{}`[{}] no se aplico: {error}",
                            doc.table, doc.column
                        ));
                        continue;
                    }
                };
                // Una medida no tiene fila actual. El chequeo lo permite con un aviso, pero
                // una columna que la use no puede dar valor nunca, y es mejor decirlo al
                // aplicar que dejar una columna que rompe el informe al leerla.
                if !checked.measure_dependencies.is_empty() {
                    avisos.push(format!(
                        "la columna calculada `{}`[{}] no se aplico: usa una medida, y una \
                         medida no tiene valor fila a fila",
                        doc.table, doc.column
                    ));
                    continue;
                }
                let data_type = match &doc.data_type {
                    Some(text) => match parse_data_type(text) {
                        Some(data_type) => data_type,
                        None => {
                            avisos.push(format!(
                                "la columna calculada `{}`[{}] no se aplico: el tipo `{text}` no existe",
                                doc.table, doc.column
                            ));
                            continue;
                        }
                    },
                    None => DataType::Text,
                };
                let id = next_column_id(model);
                if let Some(table) = model.tables.iter_mut().find(|t| t.id == table_id) {
                    let mut column =
                        Column::calculated(id, doc.column.clone(), data_type, expression.clone());
                    column.sort_by = doc
                        .sort_by
                        .as_ref()
                        .and_then(|name| table.column_by_name(name))
                        .map(|column| column.id);
                    column.is_hidden = doc.is_hidden.unwrap_or(false);
                    table.columns.push(column);
                }
                continue;
            }

            // Los ids se resuelven antes de tocar nada: `table_by_name` presta el modelo, y
            // la escritura necesita prestarlo mutable.
            let resolved = model.table_by_name(&doc.table).and_then(|table| {
                let column = table.column_by_name(&doc.column)?;
                let sort_by = match &doc.sort_by {
                    Some(name) => Some(table.column_by_name(name)?.id),
                    None => None,
                };
                Some((table.id, column.id, sort_by))
            });

            let Some((table_id, column_id, sort_by)) = resolved else {
                avisos.push(format!(
                    "la columna `{}`[{}] no esta en el modelo",
                    doc.table, doc.column
                ));
                continue;
            };

            if let Some(table) = model.tables.iter_mut().find(|t| t.id == table_id)
                && let Some(column) = table.columns.iter_mut().find(|c| c.id == column_id)
            {
                if sort_by.is_some() {
                    column.sort_by = sort_by;
                }
                if let Some(hidden) = doc.is_hidden {
                    column.is_hidden = hidden;
                }
            }
        }

        for doc in &self.measures {
            if model.measure_by_name(&doc.name).is_some() {
                continue;
            }
            if let Err(error) = datan_dax_check::check(&doc.expression, model) {
                avisos.push(format!("la medida `{}` no se aplico: {error}", doc.name));
                continue;
            }
            let mut measure = Measure::new(next_id(model), doc.name.clone(), doc.expression.clone());
            measure.format_string = doc.format_string.clone();
            measure.description = doc.description.clone();
            model.measures.push(measure);
        }

        self.apply_relationships(model, &mut avisos);

        avisos
    }

    /// Aplica las relationships del documento.
    ///
    /// Una que ya existe se AJUSTA: el rol y la direccion se pisap. Una que no existe se
    /// AGREGA. El segundo caso es el que importa: la introspeccion solo lee foreign keys, y
    /// una base sin constraints declaradas llega sin ninguna relacion, con lo que no se puede
    /// agrupar los hechos por ninguna dimension.
    fn apply_relationships(&self, model: &mut Model, avisos: &mut Vec<String>) {
        for doc in &self.relaciones {
            let Some((from_table, from_column)) =
                column_id(model, &doc.from_table, &doc.from_column)
            else {
                avisos.push(format!(
                    "la relacion `{}`[{}] -> `{}`[{}] no se aplico: `{}`[{}] no esta en el modelo",
                    doc.from_table,
                    doc.from_column,
                    doc.to_table,
                    doc.to_column,
                    doc.from_table,
                    doc.from_column
                ));
                continue;
            };
            let Some((_, to_column)) = column_id(model, &doc.to_table, &doc.to_column) else {
                avisos.push(format!(
                    "la relacion `{}`[{}] -> `{}`[{}] no se aplico: `{}`[{}] no esta en el modelo",
                    doc.from_table,
                    doc.from_column,
                    doc.to_table,
                    doc.to_column,
                    doc.to_table,
                    doc.to_column
                ));
                continue;
            };

            let Some(role) = parse_role(doc.role.as_deref()) else {
                avisos.push(format!(
                    "la relacion `{}`[{}] -> `{}`[{}] no se aplico: el rol `{}` no existe",
                    doc.from_table,
                    doc.from_column,
                    doc.to_table,
                    doc.to_column,
                    doc.role.as_deref().unwrap_or("")
                ));
                continue;
            };
            let Some(direction) = parse_direction(doc.direction.as_deref()) else {
                avisos.push(format!(
                    "la relacion `{}`[{}] -> `{}`[{}] no se aplico: la direccion `{}` no existe",
                    doc.from_table,
                    doc.from_column,
                    doc.to_table,
                    doc.to_column,
                    doc.direction.as_deref().unwrap_or("")
                ));
                continue;
            };

            if let Some(existing) = model
                .relationships
                .iter_mut()
                .find(|r| r.from_column == from_column && r.to_column == to_column)
            {
                existing.role = role;
                existing.direction = direction;
                continue;
            }

            // Una relacion NUEVA. La clave tiene que estar del lado UNO: si el usuario escribio
            // el par al reves, la propagacion va en la direccion contraria y los numeros salen
            // mal sin ningun error. Se avisa y no se aplica.
            let has_keys = model
                .table(from_table)
                .is_some_and(|table| !table.key_columns.is_empty());
            let is_key = model
                .table(from_table)
                .is_some_and(|table| table.key_columns.contains(&from_column));
            if has_keys && !is_key {
                avisos.push(format!(
                    "la relacion `{}`[{}] -> `{}`[{}] no se aplico: `{}`[{}] no es la clave de su tabla,                      y el lado UNO de una relacion es la clave",
                    doc.from_table,
                    doc.from_column,
                    doc.to_table,
                    doc.to_column,
                    doc.from_table,
                    doc.from_column
                ));
                continue;
            }

            model.relationships.push(Relationship {
                from_column,
                to_column,
                cardinality: Cardinality::OneToMany,
                direction,
                role,
                assume_referential_integrity: false,
            });
        }
    }
}

/// El id de la proxima medida. Las del modelo arrancan en 100; las del usuario, mas arriba.
fn next_id(model: &Model) -> ObjectId {
    ObjectId(model.measures.iter().map(|m| m.id.0).max().unwrap_or(10_000) + 1)
}

/// El id de la proxima columna. Por encima de todas las de la introspeccion, para no chocar
/// con la que el motor agregue manana.
fn next_column_id(model: &Model) -> ObjectId {
    let max = model
        .tables
        .iter()
        .flat_map(|table| table.columns.iter().map(|column| column.id.0))
        .max()
        .unwrap_or(0);
    ObjectId(max + 1)
}

/// El id de la tabla y el de la columna de un nombre del documento.
fn column_id(model: &Model, table: &str, column: &str) -> Option<(ObjectId, ObjectId)> {
    let table = model.table_by_name(table)?;
    let column = table.column_by_name(column)?;
    Some((table.id, column.id))
}

/// El rol de una relationship, en el espanol del documento. Sin texto es activa: es el
/// default de Power BI y el que el usuario espera al declarar una relacion nueva.
fn parse_role(text: Option<&str>) -> Option<RelationshipRole> {
    match text {
        None => Some(RelationshipRole::Active),
        Some(text) if text.trim().eq_ignore_ascii_case("activa") => Some(RelationshipRole::Active),
        Some(text) if text.trim().eq_ignore_ascii_case("inactiva") => {
            Some(RelationshipRole::Inactive)
        }
        Some(_) => None,
    }
}

/// La direccion del filtro, en el espanol del documento. Sin texto es de la dimension a los
/// hechos, que es lo que hace un star schema.
fn parse_direction(text: Option<&str>) -> Option<FilterDirection> {
    match text {
        None => Some(FilterDirection::Single),
        Some(text) if text.trim().eq_ignore_ascii_case("simple") => Some(FilterDirection::Single),
        Some(text) if text.trim().eq_ignore_ascii_case("ambas") => Some(FilterDirection::Both),
        Some(text) if text.trim().eq_ignore_ascii_case("ninguna") => Some(FilterDirection::None),
        Some(_) => None,
    }
}

/// El tipo de una columna calculada, en el espanol del documento.
fn parse_data_type(text: &str) -> Option<DataType> {
    match text.trim().to_lowercase().as_str() {
        "texto" | "text" => Some(DataType::Text),
        "entero" | "integer" => Some(DataType::Integer),
        "decimal" => Some(DataType::Decimal),
        "fecha" | "date" => Some(DataType::Date),
        "fecha-hora" | "fecha_hora" | "datetime" => Some(DataType::DateTime),
        "booleano" | "boolean" => Some(DataType::Boolean),
        "moneda" | "currency" => Some(DataType::Moneda),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datan_model::{Column, DataType, Table, TableKind};

    #[test]
    fn la_jerarquia_viaja_en_el_archivo() {
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![],
            relaciones: vec![],
            jerarquias: vec![JerarquiaDoc {
                name: "Producto".to_string(),
                table: "catalogo_productos".to_string(),
                levels: vec!["categoria".to_string(), "nombre".to_string()],
            }],
        };
        let ruta = std::env::temp_dir().join(format!(
            "datan-jerarquia-{}.toml",
            std::process::id()
        ));
        documento.save_to(&ruta).expect("guarda");
        let leido = ModelDoc::load_from(&ruta).expect("lee");
        let _ = std::fs::remove_file(&ruta);
        assert_eq!(leido, documento);
    }

    #[test]
    fn una_jerarquia_que_no_resuelve_se_avisa() {
        let model = modelo();
        let corta = JerarquiaDoc {
            name: "Corta".to_string(),
            table: "ventas".to_string(),
            levels: vec!["mes".to_string()],
        };
        let sin_tabla = JerarquiaDoc {
            name: "Fantasma".to_string(),
            table: "otra_tabla".to_string(),
            levels: vec!["a".to_string(), "b".to_string()],
        };
        let sin_columna = JerarquiaDoc {
            name: "Rota".to_string(),
            table: "ventas".to_string(),
            levels: vec!["mes".to_string(), "no_existe".to_string()],
        };
        let buena = JerarquiaDoc {
            name: "Ventas".to_string(),
            table: "ventas".to_string(),
            levels: vec!["mes".to_string(), "mes".to_string()],
        };

        assert!(corta.resolver(&model).unwrap_err().contains("dos niveles"));
        assert!(sin_tabla.resolver(&model).unwrap_err().contains("otra_tabla"));
        assert!(sin_columna.resolver(&model).unwrap_err().contains("no_existe"));
        assert_eq!(buena.resolver(&model), Ok(vec![ObjectId(11), ObjectId(11)]));
    }

    /// Una tabla de hechos con una columna sumable y una de texto.
    fn modelo() -> Model {
        Model {
            tables: vec![Table {
                id: ObjectId(1),
                name: "ventas".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(ObjectId(10), "total", DataType::Moneda),
                    Column::new(ObjectId(11), "mes", DataType::Text),
                ],
                key_columns: vec![],
                is_date_table: false,
            }],
            measures: vec![],
            relationships: vec![],
        }
    }

    /// La tabla de hechos de `modelo()` mas una dimension con clave, para las relationships.
    fn modelo_con_dimension() -> Model {
        let mut model = modelo();
        model.tables.push(Table {
            id: ObjectId(2),
            name: "productos".to_string(),
            kind: TableKind::Dimension,
            columns: vec![
                Column::new(ObjectId(20), "id", DataType::Integer),
                Column::new(ObjectId(21), "nombre", DataType::Text),
            ],
            key_columns: vec![ObjectId(20)],
            is_date_table: false,
        });
        model
    }

    fn relaciones(relaciones: Vec<RelationshipDoc>) -> ModelDoc {
        ModelDoc {
            relaciones,
            ..ModelDoc::default()
        }
    }

    #[test]
    fn declarar_una_relacion_que_la_base_no_trae() {
        // La introspeccion solo lee foreign keys. Sin constraint declarada, el modelo llega
        // sin relaciones y no hay forma de agrupar los hechos por una dimension.
        let mut model = modelo_con_dimension();
        let documento = relaciones(vec![RelationshipDoc::new(
            "productos", "id", "ventas", "producto_id",
        )]);
        // La columna de los hechos existe en el modelo, con otro id.
        model
            .tables
            .iter_mut()
            .find(|table| table.name == "ventas")
            .expect("la tabla")
            .columns
            .push(Column::new(ObjectId(12), "producto_id", DataType::Integer));

        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        assert_eq!(model.relationships.len(), 1);
        let relacion = &model.relationships[0];
        assert_eq!(relacion.from_column, ObjectId(20));
        assert_eq!(relacion.to_column, ObjectId(12));
        assert_eq!(relacion.role, RelationshipRole::Active);
        assert_eq!(relacion.direction, FilterDirection::Single);
    }

    #[test]
    fn una_relacion_que_ya_existe_se_ajusta_y_no_se_duplica() {
        let mut model = modelo_con_dimension();
        model
            .tables
            .iter_mut()
            .find(|table| table.name == "ventas")
            .expect("la tabla")
            .columns
            .push(Column::new(ObjectId(12), "producto_id", DataType::Integer));
        model.relationships.push(Relationship {
            from_column: ObjectId(20),
            to_column: ObjectId(12),
            cardinality: Cardinality::OneToMany,
            direction: FilterDirection::Single,
            role: RelationshipRole::Active,
            assume_referential_integrity: false,
        });

        let documento = relaciones(vec![
            RelationshipDoc::new("productos", "id", "ventas", "producto_id")
                .con_rol("inactiva"),
        ]);
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.relationships[0].role, RelationshipRole::Inactive);
    }

    #[test]
    fn el_lado_uno_tiene_que_ser_una_clave() {
        // Con el lado UNO en una columna que no es la clave, el filtro propaga por el camino
        // equivocado y los numeros salen mal sin ningun error. Se rechaza al aplicar.
        let mut model = modelo_con_dimension();
        model
            .tables
            .iter_mut()
            .find(|table| table.name == "ventas")
            .expect("la tabla")
            .columns
            .push(Column::new(ObjectId(12), "producto_id", DataType::Integer));

        let documento = relaciones(vec![RelationshipDoc::new(
            "productos", "nombre", "ventas", "producto_id",
        )]);
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("clave"), "{avisos:?}");
        assert!(model.relationships.is_empty());
    }

    #[test]
    fn una_tabla_sin_claves_declaradas_se_aplica_sin_validar() {
        // Una vista o una tabla sin clave primaria no declara `key_columns`, y no hay contra
        // que validar. Se aplica: negar la relacion dejaria el informe sin ninguna salida, y
        // una relacion mal escrita ya se nota en los numeros.
        let mut model = modelo_con_dimension();
        model
            .tables
            .iter_mut()
            .find(|table| table.name == "ventas")
            .expect("la tabla")
            .columns
            .push(Column::new(ObjectId(12), "producto_id", DataType::Integer));

        let documento = relaciones(vec![RelationshipDoc::new(
            "ventas", "producto_id", "productos", "id",
        )]);
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        assert_eq!(model.relationships.len(), 1);
    }

    #[test]
    fn una_relacion_a_una_columna_que_no_esta_avisa() {
        let mut model = modelo_con_dimension();
        let documento = relaciones(vec![RelationshipDoc::new(
            "productos", "id", "ventas", "no_existe",
        )]);
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("no_existe"), "{avisos:?}");
        assert!(model.relationships.is_empty());
    }

    #[test]
    fn un_rol_que_no_existe_avisa() {
        let mut model = modelo_con_dimension();
        model
            .tables
            .iter_mut()
            .find(|table| table.name == "ventas")
            .expect("la tabla")
            .columns
            .push(Column::new(ObjectId(12), "producto_id", DataType::Integer));
        let documento = relaciones(vec![
            RelationshipDoc::new("productos", "id", "ventas", "producto_id").con_rol("a veces"),
        ]);
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("a veces"), "{avisos:?}");
        assert!(model.relationships.is_empty());
    }

    #[test]
    fn la_relacion_da_la_vuelta_por_disco() {
        let documento = relaciones(vec![RelationshipDoc::new(
            "productos", "id", "ventas", "producto_id",
        )
        .con_rol("inactiva")]);
        let file = File {
            measure: vec![],
            column: vec![],
            jerarquia: vec![],
            relacion: documento.relaciones.clone(),
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(text.contains("[[relacion]]"));
        assert!(text.contains("from_table"));

        let back: File = toml::from_str(&text).expect("vuelve a parsear");
        assert_eq!(back.relacion, documento.relaciones);
        assert!(!back.relacion[0].activa());
    }

    fn doc(measures: Vec<MeasureDoc>) -> ModelDoc {
        ModelDoc {
            measures,
            columns: vec![],
            jerarquias: vec![],
            relaciones: vec![],
        }
    }

    #[test]
    fn aplicar_agrega_las_medidas_del_usuario() {
        let mut model = modelo();
        let avisos = doc(vec![MeasureDoc::new("Ventas", "SUM('ventas'[total])")]).apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        assert_eq!(model.measures.len(), 1);
        assert_eq!(model.measures[0].name, "Ventas");
        assert_eq!(model.measures[0].expression, "SUM('ventas'[total])");
    }

    #[test]
    fn aplicar_dos_veces_no_duplica() {
        let documento = doc(vec![MeasureDoc::new("Ventas", "SUM('ventas'[total])")]);
        let mut model = modelo();
        documento.apply(&mut model);
        documento.apply(&mut model);

        assert_eq!(model.measures.len(), 1);
    }

    #[test]
    fn una_medida_del_modelo_no_se_pisa() {
        let mut model = modelo();
        model.measures.push(Measure::new(
            ObjectId(100),
            "Ventas",
            "SUM('ventas'[total])",
        ));
        // La introspeccion ya trae `Ventas`. El documento tiene otra definicion con ese
        // nombre, y la del modelo manda.
        doc(vec![MeasureDoc::new("Ventas", "COUNTROWS('ventas')")]).apply(&mut model);

        assert_eq!(model.measures.len(), 1);
        assert_eq!(model.measures[0].expression, "SUM('ventas'[total])");
    }

    #[test]
    fn una_medida_que_no_chequea_se_salta_con_aviso() {
        let mut model = modelo();
        let avisos = doc(vec![MeasureDoc::new("Rota", "SUM('ventas'[no_existe])")]).apply(&mut model);

        assert!(model.measures.is_empty());
        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("Rota"), "{avisos:?}");
        assert!(avisos[0].contains("no se aplico"), "{avisos:?}");
    }

    #[test]
    fn una_medida_del_documento_puede_usar_otra_del_documento() {
        let mut model = modelo();
        let documento = doc(vec![
            MeasureDoc::new("Ventas", "SUM('ventas'[total])"),
            MeasureDoc::new("Doble", "[Ventas] * 2"),
        ]);
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        assert_eq!(model.measures.len(), 2);
    }

    #[test]
    fn el_id_del_usuario_no_pisa_las_medidas_del_modelo() {
        let mut model = modelo();
        model
            .measures
            .push(Measure::new(ObjectId(100), "Base", "SUM('ventas'[total])"));
        doc(vec![MeasureDoc::new("Ventas", "SUM('ventas'[total])")]).apply(&mut model);

        assert_eq!(model.measures[1].id, ObjectId(101));
    }

    #[test]
    fn el_formato_y_la_descripcion_llegan_al_modelo() {
        let mut model = modelo();
        let mut measure = MeasureDoc::new("Ventas", "SUM('ventas'[total])");
        measure.format_string = Some("#,0 €".to_string());
        measure.description = Some("Total sin IVA".to_string());
        doc(vec![measure]).apply(&mut model);

        assert_eq!(model.measures[0].format_string.as_deref(), Some("#,0 €"));
        assert_eq!(
            model.measures[0].description.as_deref(),
            Some("Total sin IVA")
        );
    }

    #[test]
    fn ocultar_una_columna_se_aplica_por_nombre() {
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc {
                table: "ventas".to_string(),
                column: "mes".to_string(),
                sort_by: None,
                is_hidden: Some(true),
                expression: None,
                data_type: None,
            }],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        let table = model.table_by_name("ventas").expect("la tabla");
        assert!(table.column_by_name("mes").expect("la columna").is_hidden);
        assert!(!table.column_by_name("total").expect("la columna").is_hidden);
    }

    #[test]
    fn una_columna_que_no_esta_avisa_y_no_toca_nada() {
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc {
                table: "ventas".to_string(),
                column: "anio".to_string(),
                sort_by: None,
                is_hidden: Some(true),
                expression: None,
                data_type: None,
            }],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("anio"), "{avisos:?}");
    }

    #[test]
    fn aplicar_agrega_una_columna_calculada() {
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc::calculada(
                "ventas",
                "doble",
                "total * 2",
                "moneda",
            )],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        let column = model
            .table_by_name("ventas")
            .and_then(|table| table.column_by_name("doble"))
            .expect("la columna calculada");
        assert!(column.is_calculated());
        assert_eq!(column.expression(), Some("total * 2"));
        assert_eq!(column.data_type, DataType::Moneda);
        // El id sale por encima de los de la introspeccion: no pisa una columna ni se pisa
        // con la proxima que la base agregue.
        assert_eq!(column.id, ObjectId(12));
    }

    #[test]
    fn una_columna_calculada_puede_usar_otra_calculada() {
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![
                ColumnDoc::calculada("ventas", "doble", "total * 2", "moneda"),
                ColumnDoc::calculada("ventas", "cuadruple", "doble * 2", "moneda"),
            ],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        assert!(model
            .table_by_name("ventas")
            .and_then(|table| table.column_by_name("cuadruple"))
            .is_some());
    }

    #[test]
    fn una_columna_calculada_que_no_chequea_se_avisa() {
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc::calculada(
                "ventas",
                "rota",
                "no_existe * 2",
                "moneda",
            )],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("rota"), "{avisos:?}");
        assert!(model
            .table_by_name("ventas")
            .and_then(|table| table.column_by_name("rota"))
            .is_none());
    }

    #[test]
    fn una_columna_calculada_no_pisa_una_que_ya_existe() {
        // La base manda. Una columna con ese nombre ya esta, y su valor sale de la tabla.
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc::calculada("ventas", "mes", "total * 2", "texto")],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("ya existe"), "{avisos:?}");
        assert!(!model
            .table_by_name("ventas")
            .and_then(|table| table.column_by_name("mes"))
            .expect("la columna")
            .is_calculated());
    }

    #[test]
    fn un_tipo_que_no_existe_avisa() {
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc::calculada("ventas", "rara", "total * 2", "magico")],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("magico"), "{avisos:?}");
    }

    #[test]
    fn una_columna_calculada_sin_tipo_queda_como_texto() {
        let mut model = modelo();
        let mut column_doc = ColumnDoc::calculada("ventas", "sin_tipo", "total * 2", "texto");
        column_doc.data_type = None;
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![column_doc],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        let column = model
            .table_by_name("ventas")
            .and_then(|table| table.column_by_name("sin_tipo"))
            .expect("la columna");
        assert_eq!(column.data_type, DataType::Text);
    }

    #[test]
    fn una_medida_del_documento_puede_sumar_una_columna_calculada_del_documento() {
        // Las columnas se aplican ANTES que las medidas: la columna no existe en la base
        // hasta que este documento la crea, y la medida que la suma tiene que encontrarla.
        let mut model = modelo();
        let documento = ModelDoc {
            measures: vec![MeasureDoc::new("Doble", "SUM('ventas'[doble])")],
            columns: vec![ColumnDoc::calculada("ventas", "doble", "total * 2", "moneda")],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert!(avisos.is_empty(), "{avisos:?}");
        assert_eq!(model.measures.len(), 1);
    }

    #[test]
    fn una_columna_calculada_que_usa_una_medida_se_avisa() {
        // Una medida no tiene valor fila a fila. El chequeo lo permite con un aviso, y una
        // columna que la use no puede dar valor nunca: mejor decirlo al aplicar.
        let mut model = modelo();
        model
            .measures
            .push(Measure::new(ObjectId(100), "TotalVentas", "SUM('ventas'[total])"));
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc::calculada("ventas", "rota", "[TotalVentas] + 1", "moneda")],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("rota"), "{avisos:?}");
        assert!(avisos[0].contains("medida"), "{avisos:?}");
        assert!(model
            .table_by_name("ventas")
            .and_then(|table| table.column_by_name("rota"))
            .is_none());
    }

    #[test]
    fn una_columna_calculada_honra_sort_by_y_hidden() {
        let mut model = modelo();
        let mut column_doc = ColumnDoc::calculada("ventas", "doble", "total * 2", "moneda");
        column_doc.sort_by = Some("mes".to_string());
        column_doc.is_hidden = Some(true);
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![column_doc],
            jerarquias: vec![],
            relaciones: vec![],
        };
        documento.apply(&mut model);

        let column = model
            .table_by_name("ventas")
            .and_then(|table| table.column_by_name("doble"))
            .expect("la columna");
        assert_eq!(column.sort_by, Some(ObjectId(11)));
        assert!(column.is_hidden);
    }

    #[test]
    fn una_columna_calculada_da_la_vuelta_por_disco() {
        let documento = ModelDoc {
            measures: vec![],
            columns: vec![ColumnDoc::calculada(
                "ventas",
                "anio",
                "YEAR('ventas'[fecha])",
                "entero",
            )],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let file = File {
            measure: vec![],
            column: documento.columns.clone(),
            jerarquia: vec![],
            relacion: vec![],
        };
        let text = toml::to_string_pretty(&file).expect("serializa");
        assert!(text.contains("expression"));
        assert!(text.contains("data_type"));

        let back: File = toml::from_str(&text).expect("vuelve a parsear");
        assert_eq!(back.column, documento.columns);
    }

    #[test]
    fn upsert_reemplaza_en_el_lugar() {
        let mut documento = doc(vec![
            MeasureDoc::new("A", "SUM('ventas'[total])"),
            MeasureDoc::new("B", "SUM('ventas'[total])"),
        ]);
        documento.upsert_measure(MeasureDoc::new("A", "COUNTROWS('ventas')"));

        assert_eq!(documento.measures.len(), 2);
        assert_eq!(documento.measures[0].name, "A");
        assert_eq!(documento.measures[0].expression, "COUNTROWS('ventas')");
        assert_eq!(documento.measures[1].name, "B");
    }

    #[test]
    fn el_toml_es_un_array_de_tablas_planas() {
        // El archivo lo escribe una app y lo leen ojos. Sin el envoltorio, el TOML no acepta
        // un array en la raiz y `to_string` falla en tiempo de ejecucion, no al compilar.
        let documento = ModelDoc {
            measures: vec![MeasureDoc::new("Ventas", "SUM('ventas'[total])")],
            columns: vec![ColumnDoc {
                table: "ventas".to_string(),
                column: "mes".to_string(),
                sort_by: None,
                is_hidden: Some(true),
                expression: None,
                data_type: None,
            }],
            jerarquias: vec![],
            relaciones: vec![],
        };
        let file = File {
            measure: documento.measures.clone(),
            column: documento.columns.clone(),
            jerarquia: documento.jerarquias.clone(),
            relacion: documento.relaciones.clone(),
        };
        let text = toml::to_string_pretty(&file).expect("serializa");

        assert!(text.contains("[[measure]]"));
        assert!(text.contains("[[column]]"));
        // Un campo opcional que no esta no se escribe: el archivo dice lo que el usuario
        // toco, y nada mas.
        assert!(!text.contains("format_string"));
        assert!(!text.contains("description"));
        assert!(!text.contains("[[jerarquia]]"));

        let back: File = toml::from_str(&text).expect("vuelve a parsear");
        assert_eq!(back.measure, documento.measures);
        assert_eq!(back.column, documento.columns);
    }

    #[test]
    fn el_documento_da_la_vuelta_por_disco() {
        let path = std::env::temp_dir().join("datan_model_doc_prueba.toml");
        let documento = ModelDoc {
            measures: vec![MeasureDoc::new("Ventas", "SUM('ventas'[total])")],
            columns: vec![],
            jerarquias: vec![],
            relaciones: vec![],
        };
        documento.save_to(&path).expect("escribe");
        let back = ModelDoc::load_from(&path).expect("lee");

        assert_eq!(back, documento);
        std::fs::remove_file(&path).expect("limpia");
    }

    #[test]
    fn un_archivo_que_no_existe_es_un_documento_vacio() {
        let path = std::env::temp_dir().join("datan_model_doc_no_existe.toml");
        let _ = std::fs::remove_file(&path);
        let documento = ModelDoc::load_from(&path).expect("no falla");

        assert_eq!(documento, ModelDoc::default());
    }

    #[test]
    fn la_ruta_vive_en_la_carpeta_del_usuario() {
        let path = ModelDoc::path("datan_bench");

        assert!(path.to_string_lossy().contains(".datan"), "{path:?}");
        assert!(path.to_string_lossy().contains("models"), "{path:?}");
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("datan_bench.toml"));
    }
}
