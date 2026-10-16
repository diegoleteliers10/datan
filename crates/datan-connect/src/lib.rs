//! De una base conectada a un `Model`.
//!
//! La introspeccion no es un detalle: un modelo construido a mano se equivoca de formas que
//! nadie ve hasta que una medida devuelve un numero raro. Aqui las tres decisiones que
//! importan estan fijadas y probadas.
//!
//! # 1. La tabla de hechos es la que tiene la clave foranea mas referenciada
//!
//! No hay marca de "esto es un hecho" en Postgres. La heuristica que funciona: la tabla que
//! mas filas tiene y mas tablas apuntan a ella. `product_boleta` gana sobre `boleta` porque
//! dos claves foraneas apuntan a ella, y sobre `catalogo_productos` porque tiene 1M de filas
//! frente a 500.
//!
//! # 2. Las medidas se generan por tabla, no por intuition
//!
//! Cada columna `integer` de la tabla de hechos produce un `SUM`. Cada columna de una
//! dimension produce un `DISTINCTCOUNT`. Es una regla mecanica y por eso es auditable:
//! `Datan` no adivina que es "importante", genera lo que se puede y el usuario agrega el
//! resto. Adivinar seria peor: una medida inventada que nadie pidio es un numero sin origen.
//!
//! # 3. Los tipos enteros de Postgres son `DataType::Moneda`
//!
//! `bruto`, `neto`, `precio` y `costo` son `integer`, no centavos. La moneda chilena no
//! tiene subdivision en la practica, asi que `Moneda` es BIGINT y no `Moneda * 100`. Un
//! centavo perdido aqui no aparece como error: aparece como un 0.01 de diferencia en cada
//! fila, y en un total de 9.750.000.000 es invisible.

use datan_dax_check::{CheckError, check_expr};
use datan_dax_syntax::parse;
mod catalog;

pub use catalog::{ForeignKey, PostgresCatalog};

use datan_model::{Column, DataType, Measure, Model, ObjectId, Table, TableKind};
use std::collections::BTreeMap;
use datan_storage::{Engine, StorageError};

/// Un error de introspeccion, con la sentencia que lo produjo.
///
/// El mensaje de DuckDB sin la consulta es inutil para diagnosticar: "Catalog Error" no
/// dice que tabla fallo.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("no se pudo leer el catalogo de Postgres: {0}")]
    Catalog(String),
    #[error(
        "se leyeron {tables} tablas pero ninguna relationship. El scanner de Postgres de \
         DuckDB no entrega claves foraneas: `information_schema` las pierde y \
         `pg_constraint` no se puede unir. Un modelo sin relaciones cruza dimensiones y no \
         cruza nada, asi que Datan se niega a construirlo en vez de dar numeros que no \
         tienen sentido"
    )]
    NoRelationships { tables: usize },
    #[error("fallo la consulta de introspeccion:\n{sql}\n{error}")]
    Query { sql: String, error: String },
    #[error("la tabla {0} tiene tipo {1}, que no tiene equivalente en el modelo")]
    UnsupportedType(String, String),
    #[error("la expresion de la medida `{name}` no es DAX valido: {detail}")]
    BadMeasure { name: String, detail: String },
    #[error("la definicion de la medida `{name}` no resuelve contra el modelo: {detail}")]
    UnresolvedMeasure { name: String, detail: String },
    #[error("deduplicacion fallida de la expresion: {0}")]
    Dedupe(String),
}

/// El resultado de mirar una base de datos.
pub struct Introspection {
    pub model: Model,
    /// Un texto por tabla, para mostrarlo en la UI antes de que el usuario elija.
    pub report: Vec<String>,
}

/// Los ids empiezan en 1 y `ObjectId(0)` se reserva para "nada". Un id que empieza en 0 se
/// confunde con un `unwrap_or(ObjectId(0))` en un sitio donde deberia haber error.
const FIRST_ID: u64 = 1;

/// De donde se lee.
///
/// El nombre con el que la base esta adjunta a DuckDB, que no es el nombre de la base en
/// Postgres. `current_database()` devuelve la base local de DuckDB, no la remota: usarlo
/// aqui daria un catalogo vacio sin decir por que.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// El alias del `ATTACH`. `pg` en `ATTACH '...' AS pg`.
    pub alias: String,
}

impl Source {
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
        }
    }
}

/// Mira una base ya conectada y construye el modelo.
///
/// Las COLUMNAS y los TIPOS salen de `duckdb_columns()`: es materializado y no pierde
/// filas. Las RELACIONES salen del catalogo de Postgres por conexion directa, porque el
/// scanner de DuckDB las pierde en silencio. Ver `catalog.rs` para la medicion.
///
/// Sin `catalog` el modelo sale sin relaciones, y eso se dice con un error en vez de
/// devolver un modelo que parece funcionar: un informe que cruza productos por categoria
/// devolveria el total del informe, no el de la categoria.
pub fn introspect(
    engine: &Engine,
    source: &Source,
    catalog: Option<&mut PostgresCatalog>,
) -> Result<Introspection, ConnectError> {
    // 1. Las columnas, con su tipo y su tabla.
    //
    //    `duckdb_columns()` y NO `information_schema.columns`. La diferencia no es de
    //    estilo: `information_schema` de Postgres son vistas hechas de UNION y
    //    subconsultas, y el scanner de DuckDB no las empuja bien. Se perdio media base de
    //    datos sin un solo error. `duckdb_columns()` es el catalogo de DuckDB, ya
    //    materializado, y coincide con lo que el scanner va a leer de verdad.
    let columns_sql = "SELECT
            schema_name,
            table_name,
            column_name,
            data_type
        FROM duckdb_columns()
        WHERE database_name = '{alias}'
          AND schema_name NOT IN ('information_schema', 'pg_catalog')
        ORDER BY schema_name, table_name, column_index";

    let alias = source.alias.replace('\'', "''");
    let rows = query(engine, &columns_sql.replace("{alias}", &alias))?;

    // Se agrupa por tabla. `BTreeMap` para que el orden sea estable entre corridas: un modelo
    // que cambia de ids en cada arranque invalida el cache de planes sin motivo.
    let mut tables: std::collections::BTreeMap<(String, String), Vec<RawColumn>> =
        std::collections::BTreeMap::new();
    for row in &rows {
        let schema = text(row, 0);
        let table = text(row, 1);
        let name = text(row, 2);
        let sql_type = text(row, 3);

        tables
            .entry((schema.clone(), table.clone()))
            .or_default()
            .push(RawColumn { name, sql_type });
    }

    if tables.is_empty() {
        return Err(ConnectError::Catalog(
            "la base no tiene tablas de usuario".to_string(),
        ));
    }

    // 2. Las claves primarias y foraneas.
    //
    //    Aqui esta el bloqueo de la version. Medido con `probe_scanner`:
    //
    //      - `SELECT conname FROM pg_constraint WHERE contype='f'`   -> 2 filas  FUNCIONA
    //      - lo mismo unido a `pg_class` por oid                      -> 0 filas  FALLA
    //      - `information_schema.key_column_usage`                    -> 3 filas, faltan 2
    //      - `duckdb_constraints()`                                   -> solo NOT NULL
    //
    //    El scanner de Postgres entrega los nombres de las constraints pero no las columnas
    //    que unen, y las vistas de `information_schema` que si las traen pierden las filas
    //    de clave foranea en silencio. No hay consulta que devuelva el par de columnas.
    //
    //    Por eso `introspect` NO puede construir relaciones todavia, y por eso devuelve
    //    `NoRelationships` en vez de un modelo con cero relaciones. Un modelo sin
    //    relaciones parece funcionar y da numeros sin sentido: un informe que cruza
    //    productos por categoria devuelve el total del informe, no el total de la categoria.
    // 3. Las filas de cada tabla, para decidir cual es el hecho.
    // El catalogo se lee ANTES de decidir que es el hecho: la decision depende de cuantas
    // claves foraneas apuntan a cada tabla.
    let mut catalog = catalog;
    let (foreign_keys, primary_keys) = match catalog.as_deref_mut() {
        Some(catalog) => {
            let keys = catalog.foreign_keys()?;
            let primaries = catalog.primary_keys()?;
            (Some(keys), primaries)
        }
        None => (None, BTreeMap::new()),
    };

    let counts = table_counts(engine, &source.alias, &tables);

    // 4. Los ids. Se asignan en orden de tabla, para que sean estables.
    let mut next_id = FIRST_ID;
    let mut table_ids: std::collections::BTreeMap<(String, String), ObjectId> =
        std::collections::BTreeMap::new();
    for key in tables.keys() {
        table_ids.insert(key.clone(), ObjectId(next_id));
        next_id += 1;
    }

    let mut column_ids: std::collections::BTreeMap<(String, String, String), ObjectId> =
        std::collections::BTreeMap::new();
    for ((schema, table), columns) in &tables {
        for column in columns {
            column_ids.insert(
                (schema.clone(), table.clone(), column.name.clone()),
                ObjectId(next_id),
            );
            next_id += 1;
        }
    }

    // 5. Quien es el hecho.
    let fact = choose_fact_table(&tables, foreign_keys.as_deref().unwrap_or(&[]), &counts).ok_or_else(|| {
        ConnectError::Catalog(
            "no se pudo decidir cual es la tabla de hechos. Datan elige la que tiene la \\
             clave foranea mas referenciada; con menos de dos tablas no hay forma de saberlo"
                .to_string(),
        )
    })?;

    // 6. Las tablas.
    let mut model_tables = Vec::new();
    let mut report: Vec<String> = Vec::new();
    for ((schema, table), columns) in &tables {
        let id = table_ids[&(schema.clone(), table.clone())];
        let kind = if (schema.clone(), table.clone()) == fact {
            TableKind::Facts
        } else {
            TableKind::Dimension
        };
        let mut model_columns = Vec::new();
        for column in columns {
            model_columns.push(Column::new(
                column_ids[&(schema.clone(), table.clone(), column.name.clone())],
                column.name.clone(),
                map_type(&column.sql_type)?,
            ));
        }
        // La clave primaria viene del catalogo. Sin ella se cae a la convencion de nombre
        // (`id`, o el nombre de la tabla con sufijo `_id`), que acierta casi siempre y no
        // esta probada. Se dice en el informe para que se note cuando la convencion va a ser
        // la que decidio.
        let keys: Vec<ObjectId> = primary_keys
            .get(&(schema.clone(), table.clone()))
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| {
                        column_ids
                            .get(&(schema.clone(), table.clone(), name.clone()))
                            .copied()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let key_by_convention = keys.is_empty();

        report.push(format!(
            "  {}  {} columnas  {} filas  {}",
            format!("{schema}.{table}").replace('\t', " "),
            model_columns.len(),
            counts
                .get(&(schema.clone(), table.clone()))
                .copied()
                .unwrap_or(0),
            format!(
                "{}{}",
                if matches!(kind, TableKind::Facts) { "HECHOS" } else { "dimension" },
                if key_by_convention { "  (clave por convencion)" } else { "" }
            )
        ));

        model_tables.push(Table {
            id,
            name: table.clone(),
            kind,
            columns: model_columns,
            key_columns: keys,
            is_date_table: false,
        });
    }

    // El modelo se construye SIN relaciones primero, porque `relationships` necesita
    // consultar si la tabla que referencia es la de hechos, y eso solo se sabe despues de
    // elegir la tabla de hechos.
    let mut model = Model {
        tables: model_tables,
        measures: Vec::new(),
        relationships: Vec::new(),
    };
    model.relationships = match catalog {
        Some(catalog) => catalog.relationships(&model, &column_ids, foreign_keys.as_deref().unwrap_or(&[]))?,
        None => {
            if model.tables.len() > 1 {
                return Err(ConnectError::NoRelationships {
                    tables: model.tables.len(),
                });
            }
            Vec::new()
        }
    };

    model.measures = generate_measures(&model)?;

    report.push(String::new());
    report.push(format!(
        "  {} tablas, {} relationships, {} medidas generadas",
        model.tables.len(),
        model.relationships.len(),
        model.measures.len()
    ));

    Ok(Introspection { model, report })
}

struct RawColumn {
    name: String,
    sql_type: String,
}

/// Elige la tabla de hechos.
///
/// La regla: la tabla que mas claves foraneas ajenas apuntan. Es la unica senal que el
/// catalogo da sin heuristicas de nombres.
///
/// Con empate, la que mas filas tiene. Con empate en filas, la que mas columnas tiene.
/// Los tres criterios son deterministas y en ese orden, para que el mismo esquema produzca
/// siempre el mismo modelo.
fn choose_fact_table(
    tables: &std::collections::BTreeMap<(String, String), Vec<RawColumn>>,
    foreign_keys: &[ForeignKey],
    counts: &std::collections::BTreeMap<(String, String), i64>,
) -> Option<(String, String)> {
    // La tabla que MAS CLAVES FORANEAS DECLARA.
    //
    // La firma de un esquema en estrella: la tabla de hechos esta en el lado de muchos y
    // DECLARA las claves foraneas hacia las dimensiones. `product_boleta` declara dos.
    //
    // Contar las claves foraneas que apuntan a cada tabla da el resultado invertido:
    // `boleta` recibe una y gana por tener mas filas, y el modelo trata la cabecera de las
    // ventas como si fuera el detalle. Las medidas se generan sobre la tabla equivocada y
    // todos los numeros estan mal en la misma direccion, lo que los hace parecer coherentes.
    let mut declared: BTreeMap<(String, String), usize> = BTreeMap::new();
    for key in foreign_keys {
        *declared.entry((key.schema.clone(), key.table.clone())).or_default() += 1;
    }
    tables
        .keys()
        .max_by_key(|key| {
            (
                declared.get(key).copied().unwrap_or(0),
                counts.get(key).copied().unwrap_or(0),
                tables.get(key).map(|c| c.len()).unwrap_or(0),
            )
        })
        .cloned()
}

/// Cuantas filas tiene cada tabla.
///
/// Se consulta una por vez y no con un `UNION ALL` generado: el SQL tiene que poder fallar
/// por tabla, y decir cual fallo es la mitad del diagnostico.
///
/// El alias de DuckDB va delante. `public.boleta` no resuelve cuando la base esta adjunta
/// como `pg`: DuckDB busca el esquema `public` en su propio catalogo, no en el remoto, y
/// responde "schema public does not exist" sin suggestencia util.
fn table_counts(
    engine: &Engine,
    source: &str,
    tables: &BTreeMap<(String, String), Vec<RawColumn>>,
) -> BTreeMap<(String, String), i64> {
    let mut out = BTreeMap::new();
    for key in tables.keys() {
        let sql = format!(
            "SELECT COUNT(*) FROM {}.{}",
            quote_identifier(source),
            quote_qualified(&key.0, &key.1)
        );
        let Ok(batches) = engine.query_arrow(&sql) else {
            continue;
        };
        if let Some(batch) = batches.first() {
            out.insert(key.clone(), cell_text(batch, 0, 0).parse().unwrap_or(0));
        }
    }
    out
}

/// Los tipos de Postgres que el modelo entiende.
///
/// El resto da error con el nombre del tipo, no un `Moneda` silencioso. Un tipo sin
/// equivalencia es una decision que alguien tiene que tomar.
fn map_type(sql_type: &str) -> Result<DataType, ConnectError> {
    let lower = sql_type.to_lowercase();
    Ok(match lower.as_str() {
        "integer" | "int4" | "int" | "smallint" | "int2" | "bigint" | "int8" => {
            DataType::Moneda
        }
        "double precision" | "float8" | "real" | "float4" | "numeric" | "decimal" => {
            DataType::Decimal
        }
        "text" | "character varying" | "varchar" | "char" | "character" | "name" | "uuid" => {
            DataType::Text
        }
        "boolean" | "bool" => DataType::Boolean,
        "date" => DataType::Date,
        "timestamp with time zone" | "timestamp without time zone" | "timestamptz" => {
            DataType::DateTime
        }
        other => {
            return Err(ConnectError::UnsupportedType(
                other.to_string(),
                sql_type.to_string(),
            ));
        }
    })
}

/// Genera medidas por regla mecanica.
///
/// Un `SUM` por columna entera de los hechos, un `DISTINCTCOUNT` por columna de cada
/// dimension. Sin adivinar.
///
/// Se omiten las columnas de clave y las que solo contienen el id de la fila: `SUM(id)` es
/// un numero que no significa nada y ocupa una columna del informe.
fn generate_measures(model: &Model) -> Result<Vec<Measure>, ConnectError> {
    let mut measures = Vec::new();
    let mut next_id = 10_000u64;

    for table in &model.tables {
        let is_fact = matches!(table.kind, TableKind::Facts);
        for column in &table.columns {
            let sumable = matches!(
                column.data_type,
                DataType::Integer | DataType::Moneda | DataType::Decimal
            );
            let meaningful = !table.key_columns.contains(&column.id)
                && !is_identifier(&column.name);

            if is_fact && sumable && meaningful {
                let name = format!("{} {}", table.name, column.name);
                let expression =
                    format!("SUM('{}'[{}])", table.name, column.name);
                measures.push(build_measure(
                    ObjectId(next_id),
                    name,
                    expression,
                    model,
                )?);
                next_id += 1;
            }

            if !is_fact && matches!(column.data_type, DataType::Text) && meaningful {
                let name = format!("{} {}", table.name, column.name);
                let expression =
                    format!("DISTINCTCOUNT('{}'[{}])", table.name, column.name);
                measures.push(build_measure(
                    ObjectId(next_id),
                    name,
                    expression,
                    model,
                )?);
                next_id += 1;
            }
        }
    }

    Ok(measures)
}

/// Un nombre de columna que solo lleva el identificador de la fila.
fn is_identifier(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "id" || lower.ends_with("_id") || lower.ends_with("id")
}

/// Parsea y resuelve una medida recien generada.
///
/// Una medida generada que no valida contra el modelo es un bug de Datan, no del usuario. Si
/// `SUM('catalogo_productos'[nombre])` no baja porque la columna se llama distinto, hay que
/// saberlo al conectar, no cuando el usuario la abra en el informe.
fn build_measure(
    id: ObjectId,
    name: String,
    expression: String,
    model: &Model,
) -> Result<Measure, ConnectError> {
    let parsed = parse(&expression).map_err(|error| ConnectError::BadMeasure {
        name: name.clone(),
        detail: format!("{error} en `{expression}`"),
    })?;
    check_expr(&parsed, model).map_err(|error: CheckError| {
        ConnectError::UnresolvedMeasure {
            name: name.clone(),
            detail: format!("{error} en `{expression}`"),
        }
    })?;
    Ok(Measure::new(id, name, expression))
}

// ---------------------------------------------------------------------------------------------
// Lectura del catalogo
// ---------------------------------------------------------------------------------------------

/// Corre una consulta de introspeccion y devuelve las filas como texto.
///
/// Todo se lee como texto a proposito. El tipo de cada celda del catalogo depende de la
/// version de Postgres y de la columna, y coercionar a texto aqui evita un `downcast`
/// por columna que se rompe en la siguiente actualizacion.
fn query(engine: &Engine, sql: &str) -> Result<Vec<Vec<String>>, ConnectError> {
    let batches = engine
        .query_arrow(sql)
        .map_err(|error| ConnectError::Query {
            sql: sql.to_string(),
            error: error.to_string(),
        })?;
    let mut rows = Vec::new();
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let cells = (0..batch.num_columns())
                .map(|column| cell_text(batch, column, row))
                .collect();
            rows.push(cells);
        }
    }
    Ok(rows)
}

fn cell_text(
    batch: &datan_storage::arrow::record_batch::RecordBatch,
    column: usize,
    row: usize,
) -> String {
    use datan_storage::arrow::array::{Array, Int64Array, StringArray};
    let array = batch.column(column);
    if array.null_count() > 0 && array.is_null(row) {
        return String::new();
    }
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        return values.value(row).to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        return values.value(row).to_string();
    }
    format!("{:?}", array.data_type())
}

fn text(row: &[String], index: usize) -> String {
    row.get(index).cloned().unwrap_or_default()
}

/// `esquema.tabla`, con comillas cuando hace falta.
///
/// Un nombre de Postgres puede contener un punto o un espacio. Sin comillas, `mi.tabla` se
/// lee como el esquema `mi` y la tabla `tabla`.
fn quote_qualified(schema: &str, table: &str) -> String {
    format!(
        "{}.{}",
        quote_identifier(schema),
        quote_identifier(table)
    )
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Convierte un error del motor a uno de conexion.
///
/// El trait `From` no aplica: `StorageError::EmptyResult` no es un fallo de conexion, es un
/// resultado legitimo, y mapearlo a error haria que un catalogo vacio pareciera una base
/// rota.
pub fn storage_reason(error: &StorageError) -> String {
    match error {
        StorageError::EmptyResult => "la consulta no devolvio filas".to_string(),
        other => other.to_string(),
    }
}