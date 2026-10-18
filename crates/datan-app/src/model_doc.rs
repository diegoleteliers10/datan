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
//! archivo se puede leer y editar a mano: son medidas y ajustes de columnas, nada mas.
//!
//! # Que gana la introspeccion y que gana el documento
//!
//! Gana la introspeccion. Una medida que ya existe en el modelo (la genero la introspeccion,
//! o ya se aplico) no se toca. Aplicar el documento dos veces no duplica nada, y un nombre
//! que la base ya trae no se pisa.

use std::path::{Path, PathBuf};

use datan_model::{Measure, Model, ObjectId};
use serde::{Deserialize, Serialize};

/// El archivo, con las dos listas que el formato TOML exige en la raiz.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    measure: Vec<MeasureDoc>,
    #[serde(default)]
    column: Vec<ColumnDoc>,
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

/// Un ajuste de una columna del modelo introspeccionado.
///
/// Los dos campos son `Option` a proposito: `None` es "no lo toco". Un `false` en
/// `is_hidden` significa que el usuario la mostro a mano, y no es lo mismo que no haber
/// dicho nada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDoc {
    pub table: String,
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_hidden: Option<bool>,
}

/// El documento. Se lee, se edita y se escribe entero: es un archivo de decenas de lineas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelDoc {
    pub measures: Vec<MeasureDoc>,
    pub columns: Vec<ColumnDoc>,
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

    /// Aplica el documento encima del modelo y devuelve lo que no se pudo aplicar.
    ///
    /// Cada medida se chequea contra el modelo EN SU ESTADO ACTUAL, que ya incluye las
    /// medidas aplicadas antes: una medida que referencia otra del documento tiene que
    /// chequear igual que si las dos estuvieran escritas en el modelo.
    ///
    /// Lo que no se pudo aplicar no se borra del documento. La columna puede volver: renombrar
    /// algo en la base no es razon para tirar la medida que lo usaba.
    pub fn apply(&self, model: &mut Model) -> Vec<String> {
        let mut avisos = Vec::new();

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

        for doc in &self.columns {
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

        avisos
    }
}

/// El id de la proxima medida. Las del modelo arrancan en 100; las del usuario, mas arriba.
fn next_id(model: &Model) -> ObjectId {
    ObjectId(model.measures.iter().map(|m| m.id.0).max().unwrap_or(10_000) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use datan_model::{Column, DataType, Table, TableKind};

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

    fn doc(measures: Vec<MeasureDoc>) -> ModelDoc {
        ModelDoc {
            measures,
            columns: vec![],
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
            }],
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
            }],
        };
        let avisos = documento.apply(&mut model);

        assert_eq!(avisos.len(), 1);
        assert!(avisos[0].contains("anio"), "{avisos:?}");
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
            }],
        };
        let file = File {
            measure: documento.measures.clone(),
            column: documento.columns.clone(),
        };
        let text = toml::to_string_pretty(&file).expect("serializa");

        assert!(text.contains("[[measure]]"));
        assert!(text.contains("[[column]]"));
        // Un campo opcional que no esta no se escribe: el archivo dice lo que el usuario
        // toco, y nada mas.
        assert!(!text.contains("format_string"));
        assert!(!text.contains("description"));

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
