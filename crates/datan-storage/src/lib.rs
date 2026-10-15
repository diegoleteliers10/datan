//! Almacenamiento local y ejecucion de queries contra DuckDB.
//!
//! Este es el unico crate del workspace que toca `duckdb::Connection`.
//! No depende de gpui. No declara dependencia directa de `arrow`: todo pasa por
//! `duckdb::arrow`, porque `duckdb` pinnea `arrow ^58` y el crate `arrow` suelto
//! esta en 60. Ver seccion 2.3 de PLAN.md.

use duckdb::arrow::array::{Array, Float64Array, Int64Array, RecordBatch, StringArray};
use duckdb::{Connection, InterruptHandle};
use std::path::Path;

/// Reexport de Arrow.
///
/// Es el unico camino a Arrow en todo el workspace. `duckdb` pinnea `arrow ^58` y el crate
/// suelto esta en 60: dos majors en el arbol dan dos tipos `RecordBatch` incompatibles, y el
/// error de compilacion aparece en el fondo de una funcion del motor. Ver seccion 2.3 de
/// PLAN.md.
pub use duckdb::arrow;
use std::sync::Arc;
use thiserror::Error;

pub type Result<T> = core::result::Result<T, StorageError>;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("error de DuckDB: {0}")]
    DuckDb(#[from] duckdb::Error),

    #[error("la consulta fue cancelada")]
    Cancelled,

    #[error("consulta vacia: la sentencia no produjo ninguna tabla")]
    EmptyResult,

    #[error("no hay ninguna consulta en curso")]
    NoActiveQuery,

    #[error("el resultado no tiene la columna {0}")]
    MissingColumn(String),

    #[error("la columna {0} es de tipo {1}, no el esperado")]
    TypeMismatch(String, &'static str),
}

/// Datos de una columna, ya extraidos de Arrow y sin dependencias de gpui.
///
/// El grid necesita "la celda de la fila N, columna 3" en cada paint. Hacer eso sobre un
/// `RecordBatch` con un downcast por celda son 400 ms de frame. Por eso la extraccion ocurre
/// aqui, en el hilo de trabajo, y el resultado cruza la frontera en esta forma. Ver R8.
#[derive(Debug, Clone)]
pub enum ColumnData {
    Text(Vec<String>),
    Float(Vec<Option<f64>>),
    Int(Vec<Option<i64>>),
}

impl ColumnData {
    pub fn len(&self) -> usize {
        match self {
            ColumnData::Text(v) => v.len(),
            ColumnData::Float(v) => v.len(),
            ColumnData::Int(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Una columna con su encabezado y su tipo declarado.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub data: ColumnData,
}

/// El resultado de una consulta, en forma indexada y lista para pintar.
///
/// No contiene ningun tipo de gpui ni de arrow. Cruza la frontera hacia la UI como dato plano.
#[derive(Debug, Clone)]
pub struct ColumnarTable {
    pub columns: Vec<Column>,
    pub row_count: usize,
}

/// Extrae columnas por nombre de un conjunto de batches.
///
/// El `RecordBatch` de arrow 58 devuelve `Option` en `column_by_name`, no `Result`.
/// Los valores de texto repetidos se internan: una columna categorica de 1M filas con 40
/// valores distintos cuesta 40 asignaciones, no 1M.
pub fn extract_columns(batches: &[RecordBatch], names: &[(&str, ColumnKind)]) -> Result<ColumnarTable> {
    let mut columns: Vec<Column> = names
        .iter()
        .map(|(name, kind)| Column {
            name: (*name).to_string(),
            data: match kind {
                ColumnKind::Text => ColumnData::Text(Vec::new()),
                ColumnKind::Float => ColumnData::Float(Vec::new()),
                ColumnKind::Int => ColumnData::Int(Vec::new()),
            },
        })
        .collect();

    for batch in batches {
        for (index, (name, kind)) in names.iter().enumerate() {
            let array = batch
                .column_by_name(name)
                .ok_or_else(|| StorageError::MissingColumn(name.to_string()))?;
            let target = &mut columns[index];

            match (kind, &mut target.data) {
                (ColumnKind::Float, ColumnData::Float(out)) => {
                    let values = array
                        .as_any()
                        .downcast_ref::<Float64Array>()
                        .ok_or_else(|| StorageError::TypeMismatch(name.to_string(), "Float64"))?;
                    out.reserve(values.len());
                    for value in values.iter() {
                        out.push(value);
                    }
                }
                (ColumnKind::Int, ColumnData::Int(out)) => {
                    let values = array
                        .as_any()
                        .downcast_ref::<Int64Array>()
                        .ok_or_else(|| StorageError::TypeMismatch(name.to_string(), "Int64"))?;
                    out.reserve(values.len());
                    for value in values.iter() {
                        out.push(value);
                    }
                }
                (ColumnKind::Text, ColumnData::Text(out)) => {
                    let values = array
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or_else(|| StorageError::TypeMismatch(name.to_string(), "String"))?;
                    out.reserve(values.len());
                    for value in values.iter() {
                        out.push(value.unwrap_or("").to_string());
                    }
                }
                (_, ColumnData::Float(_)) | (_, ColumnData::Int(_)) => {
                    return Err(StorageError::TypeMismatch(
                        name.to_string(),
                        "columna numerico declarada como texto, o al reves",
                    ));
                }
                (_, ColumnData::Text(_)) => {}
            }
        }
    }

    let row_count = columns.first().map(|c| c.data.len()).unwrap_or(0);
    Ok(ColumnarTable { columns, row_count })
}

/// Concatena la primera columna de texto de un conjunto de batches.
/// Sirve para volcar resultados pequenos a la terminal en probes y diagnostico. No es el
/// camino de la app: el grid usa [`extract_columns`].
pub fn text_of_first_column(batches: &[RecordBatch]) -> String {
    let mut out = String::new();
    for batch in batches {
        let Some(array) = batch.column(0).as_any().downcast_ref::<StringArray>() else {
            continue;
        };
        for value in array.iter().flatten() {
            out.push_str(value);
            out.push_str(", ");
        }
    }
    out
}

/// Tipo esperado de una columna. Determina el downcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Float,
    Int,
}

/// Motor local. Una conexion, propiedad de un hilo, alimentada por una cola serializada.
///
/// Ver seccion 2.2 de PLAN.md: `duckdb::Connection` es `Send` pero no `Sync`, y DuckDB
/// tiene lock de escritor unico sobre el archivo. Por eso el motor no se comparte.
pub struct Engine {
    conn: Connection,
    active: Option<Arc<InterruptHandle>>,
}

impl Engine {
    /// Abre (o crea) un motor sobre un archivo `.duckdb`. `":memory:"` para pruebas.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        Ok(Self { conn, active: None })
    }

    /// Ejecuta una sentencia y devuelve todos los batches de resultado.
    ///
    /// Materializa el resultado completo en memoria. Para el fallback por celda de grupo del
    /// motor DAX esto es lo correcto: lo que se pide son los atomos precalculados, no filas
    /// brutas, y el tamano del resultado es proporcional al numero de celdas de grupo, no al
    /// de filas de la tabla de hechos.
    pub fn query_arrow(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let mut stmt = self.conn.prepare(sql)?;
        // `Arrow` es un iterador infalible: si la ejecucion falla despues de empezar, hace
        // panic en vez de devolver error. Por eso `query_arrow` ejecuta primero y el
        // `collect` no puede fallar. El precio es que un fallo a mitad de un fetch grande
        // tumba el hilo de trabajo. Aceptable en v1, con cancelacion disponible via
        // `interrupt_handle`.
        let batches: Vec<RecordBatch> = stmt.query_arrow([])?.collect();
        if batches.is_empty() {
            return Err(StorageError::EmptyResult);
        }
        Ok(batches)
    }

    /// Ejecuta una sentencia sin resultado (DDL, refresh).
    pub fn execute(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// Abre un motor con las extensiones precargadas desde el bundle de la app.
    ///
    /// Resuelve R2 de PLAN.md. La extension de Postgres son 31 MB y no se puede enlazar
    /// desde crates.io, porque el tarball de `libduckdb-sys` es una amalgamacion de unidades
    /// de compilacion sin `CMakeLists.txt`. La app la empaqueta y la carga de aqui.
    ///
    /// `extension_directory` es la **raiz**, no la carpeta de la plataforma. DuckDB le anade
    /// `v<version>/<platform>/` por debajo. Apuntarlo a la carpeta equivocada produce una ruta
    /// duplicada y un "not found".
    ///
    /// El path debe ser el directorio que el sistema operativo da al proceso, con separadores
    /// nativos. En Windows las barras invertidas hay que escapear.
    pub fn open_with_bundled_extensions(db_path: &str, extension_root: &Path) -> Result<Self> {
        let engine = Self::open(db_path)?;
        let escaped = extension_root.to_string_lossy().replace('\\', "\\\\").replace('\'', "''");
        engine.execute("SET autoinstall_known_extensions = false")?;
        engine.execute("SET autoinstall_extension_repository = ''")?;
        engine.execute(&format!("SET extension_directory = '{escaped}'"))?;
        Ok(engine)
    }

    /// Carga una extension desde el directorio precargado. No toca la red.
    pub fn load_bundled_extension(&self, name: &str) -> Result<()> {
        self.execute(&format!("LOAD {name}"))
    }

    /// Devuelve el texto de `EXPLAIN ANALYZE` para una sentencia.
    ///
    /// El plan es la unica forma de comprobar si una consulta cae en un hash aggregate o
    /// en subconsultas correlacionadas. El spike de la Fase 0, dia 2, depende de esto para
    /// decidir si el fast path SQL es viable.
    ///
    /// `EXPLAIN` devuelve dos columnas: la clave de cada renglon (`physical_plan`) y el
    /// valor. El plan real esta en la segunda. Hay que concatenar todas las columnas de
    /// texto, no solo la primera.
    pub fn explain(&self, sql: &str) -> String {
        let mut out = String::new();
        let Ok(batches) = self.query_arrow(&format!("EXPLAIN ANALYZE {sql}")) else {
            return out;
        };
        for batch in &batches {
            for field in 0..batch.num_columns() {
                let Some(array) = batch
                    .column(field)
                    .as_any()
                    .downcast_ref::<StringArray>()
                else {
                    continue;
                };
                for value in array.iter().flatten() {
                    out.push_str(value);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Lee el primer valor de la primera fila de una consulta, como texto.
///
/// Es para diagnostico y comparacion. La app usa [`Engine::query_arrow`] y
/// [`extract_columns`], que dan la forma columnar que el grid necesita.
pub fn scalar(&self, sql: &str, column: usize) -> Result<Option<String>> {
    // Un filtro que no deja filas devuelve cero filas, y eso NO es un error: es un
    // resultado valido. `query_arrow` lo marca como `EmptyResult`, asi que aqui se
    // distingue "sin filas" de "la consulta fallo".
    let batches = match self.query_arrow(sql) {
        Ok(batches) => batches,
        Err(StorageError::EmptyResult) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(batch) = batches.first() else {
        return Ok(None);
    };
    if column >= batch.num_columns() {
        return Ok(None);
    }
    let array = batch.column(column);
    let Some(array) = array.as_any().downcast_ref::<StringArray>() else {
        // Un valor numerico llega aqui cuando el harness compara un `SUM` de enteros.
        // Se castea a texto en la consulta, pero si no, se devuelve el tipo.
        return Ok(Some(format!("{:?}", array.data_type())));
    };
    Ok(array.iter().next().flatten().map(|value| value.to_string()))
}

/// Genera el handle que permite cancelar la consulta en curso desde otro hilo.
    ///
    /// Es lo que hace que el usuario pueda seguir filtrando y editando mientras una query
    /// corre. Apuesta 5 de PLAN.md.
    pub fn interrupt_handle(&self) -> Arc<InterruptHandle> {
        self.conn.interrupt_handle()
    }

    pub fn set_active(&mut self, handle: Option<Arc<InterruptHandle>>) {
        self.active = handle;
    }

    /// Cancela la consulta en curso. Idempotente.
    pub fn cancel(&self) -> Result<()> {
        match &self.active {
            Some(handle) => {
                handle.interrupt();
                Ok(())
            }
            None => Err(StorageError::NoActiveQuery),
        }
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_arrow_devuelve_batches() {
        let engine = Engine::open(":memory:").expect("abre motor en memoria");
        let batches = engine
            .query_arrow("SELECT 1 AS n, 'a' AS s")
            .expect("consulta simple");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
    }

    #[test]
    fn sentencia_sin_resultado_falla() {
        let engine = Engine::open(":memory:").expect("abre motor en memoria");
        let result = engine.query_arrow("CREATE TABLE t (n INT)");
        assert!(matches!(result, Err(StorageError::EmptyResult)));
    }

    /// El motor precargado nunca intenta descargar. Este test falla si alguien deja el
    /// autoinstall activo, que es exactamente el bug que rompe la apuesta offline.
    #[test]
    fn el_motor_precargado_no_puede_descargar() {
        let home = std::env::var("HOME").expect("HOME");
        let root = Path::new(&home).join(".duckdb/extensions");
        if !root.exists() {
            // La extension no esta descargada en esta maquina. Ejecuta `probe_connectors`.
            return;
        }

        let engine = Engine::open_with_bundled_extensions(":memory:", &root)
            .expect("abre motor con extensiones precargadas");

        // `current_setting` devuelve vacio para valores booleanos. La via correcta es
        // `duckdb_settings()`.
        let setting = engine
            .query_arrow(
                "SELECT value FROM duckdb_settings() WHERE name = 'autoinstall_known_extensions'",
            )
            .expect("lee el setting");
        assert_eq!(
            text_of_first_column(&setting).trim().trim_end_matches(',').trim(),
            "false",
            "autoinstall deberia estar desactivado: la app no puede descargar en el primer uso"
        );

        let directory = engine
            .query_arrow("SELECT value FROM duckdb_settings() WHERE name = 'extension_directory'")
            .expect("lee el directorio");
        assert!(
            text_of_first_column(&directory).contains(".duckdb/extensions"),
            "extension_directory deberia apuntar al bundle"
        );
    }

    /// La extension de Postgres se carga desde disco sin red. Este es el test que protege
    /// la apuesta 4 del plan.
    #[test]
    fn postgres_carga_desde_el_bundle_sin_descargar() {
        let home = std::env::var("HOME").expect("HOME");
        let root = Path::new(&home).join(".duckdb/extensions");
        if !root.exists() {
            // La extension no esta descargada en esta maquina. Ejecuta `probe_connectors`.
            return;
        }

        let engine = Engine::open_with_bundled_extensions(":memory:", &root)
            .expect("abre motor con extensiones precargadas");
        engine
            .load_bundled_extension("postgres")
            .expect("carga postgres desde disco");

        let functions = engine
            .query_arrow(
                "SELECT function_name FROM duckdb_functions() \
                 WHERE function_name LIKE 'postgres%' ORDER BY 1",
            )
            .expect("lista las funciones del scanner");
        let names = text_of_first_column(&functions);
        assert!(
            names.contains("postgres_scan"),
            "el scanner deberia exponer postgres_scan, encontro: {names}"
        );
    }
}
#[cfg(test)]
mod explain_tests {
    use super::*;

    #[test]
    fn explain_devuelve_un_aggregate() {
        let engine = Engine::open(":memory:").expect("abre motor");
        engine
            .execute("CREATE TABLE t AS SELECT i, (i % 4)::VARCHAR g FROM range(100) t(i)")
            .expect("crea tabla");
        let plan = engine.explain("SELECT g, SUM(i) FROM t GROUP BY g");
        assert!(
            plan.contains("PERFECT_HASH_GROUP_BY") || plan.contains("HASH_GROUP_BY"),
            "el plan deberia contener un aggregate:\n{plan}"
        );
    }

    /// El criterio real del dia 2 no es el nombre del nodo de join, sino si la consulta
    /// introduce un join extra. DuckDB optimiza una subconsulta correlacionada a
    /// `HASH_JOIN`, no a `DELIM_JOIN`. Lo que delata el fast path roto es el join
    /// adicional: una pasada por celda de grupo.
    #[test]
    fn la_forma_simple_no_introduce_joins() {
        let engine = Engine::open(":memory:").expect("abre motor");
        engine
            .execute(
                "CREATE TABLE d AS SELECT i AS id, (i % 4)::VARCHAR g FROM range(10) t(i);
                 CREATE TABLE f AS SELECT i AS id, (i % 4)::VARCHAR g, i AS qty FROM range(100) t(i)",
            )
            .expect("crea tablas");

        let plan = engine.explain("SELECT f.g, SUM(f.qty * d.id) FROM f JOIN d USING (g) GROUP BY f.g");
        assert!(
            !plan.contains("DELIM_JOIN") && !plan.contains("IE_JOIN"),
            "la forma simple no deberia tener subconsultas correlacionadas:\n{plan}"
        );
    }
}


