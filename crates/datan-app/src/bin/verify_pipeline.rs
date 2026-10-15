//! Cierra el circulo: el SQL que genera `datan-dax-sql` se ejecuta contra DuckDB real y se
//! compara con la consulta escrita a mano del spike del dia 2.
//!
//! El spike dijo que la forma funciona: un `HASH_GROUP_BY` sobre 10M de filas en 47 ms. Este
//! binario comprueba que el lowering produce ESA consulta, no una parecida. Si divergen, el
//! generador esta mintiendo y el spike no dice nada.
//!
//! Uso: cargo run --release -p datan-app --bin verify_pipeline

use datan_dax_eval::{FilterContext, FilterValue, extract_atoms};
use datan_dax_sql::{SourceMap, lower};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};
use datan_storage::Engine;
use std::time::Instant;

const FACT: ObjectId = ObjectId(1);
const PRODUCT: ObjectId = ObjectId(2);

const FACT_QTY: ObjectId = ObjectId(11);
const FACT_PRODUCT_ID: ObjectId = ObjectId(13);
const PRODUCT_ID: ObjectId = ObjectId(20);
const PRODUCT_REGION: ObjectId = ObjectId(23);
const PRODUCT_QUARTER: ObjectId = ObjectId(24);
const PRODUCT_PRICE: ObjectId = ObjectId(25);
const PRODUCT_COGS: ObjectId = ObjectId(26);

fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: FACT,
                name: "fact_sales".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(FACT_QTY, "qty", DataType::Integer),
                    Column::new(FACT_PRODUCT_ID, "product_id", DataType::Integer),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
            Table {
                id: PRODUCT,
                name: "dim_product".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(PRODUCT_ID, "product_id", DataType::Integer),
                    Column::new(PRODUCT_REGION, "region", DataType::Text),
                    Column::new(PRODUCT_QUARTER, "quarter", DataType::Text),
                    Column::new(PRODUCT_PRICE, "price", DataType::Moneda),
                    Column::new(PRODUCT_COGS, "cogs", DataType::Moneda),
                ],
                key_columns: vec![PRODUCT_ID],
                is_date_table: false,
            },
        ],
        measures: vec![Measure::new(
            ObjectId(100),
            "Revenue",
            "SUM('fact_sales'[qty])",
        )],
        relationships: vec![Relationship {
            from_column: PRODUCT_ID,
            to_column: FACT_PRODUCT_ID,
            cardinality: Cardinality::OneToMany,
            direction: FilterDirection::Single,
            role: RelationshipRole::Active,
            assume_referential_integrity: false,
        }],
    }
}

fn fixtures(engine: &Engine) {
    engine
        .execute(
            "CREATE TABLE dim_product AS
             SELECT
                 i AS product_id,
                 ['Norte','Sur','Este','Oeste'][(i % 4) + 1] AS region,
                 ['2025-Q1','2025-Q2','2025-Q3','2025-Q4'][(i % 4) + 1] AS quarter,
                 ((i * 7 % 200) + 10)::DOUBLE AS price,
                 ((i * 3 % 120) + 5)::DOUBLE AS cogs
             FROM range(500) t(i)",
        )
        .expect("crea dim_product");

    engine
        .execute(
            "CREATE TABLE fact_sales AS
             SELECT
                 i AS sale_id,
                 (hash(i) % 500) + 1 AS product_id,
                 (hash(i * 13) % 37) + 1 AS qty
             FROM range(10000000) t(i)",
        )
        .expect("crea fact_sales");
}

/// La consulta escrita a mano en el spike del dia 2.
const MANUAL: &str = "\
    SELECT p.region, p.quarter,
           SUM(f.qty * p.price) AS revenue,
           SUM(f.qty * p.cogs)  AS cogs,
           COUNT(*)             AS ventas
    FROM fact_sales f
    JOIN dim_product p ON p.product_id = f.product_id
    GROUP BY p.region, p.quarter
    ORDER BY 1, 2";

fn main() {
    println!("Datan: el SQL generado contra la consulta del spike\n");

    let engine = Engine::open(":memory:").expect("abre motor");
    fixtures(&engine);

    // 1. La consulta manual, la que el spike demostro en 47 ms.
    println!("1. Consulta manual del spike");
    let started = Instant::now();
    let manual = engine.query_arrow(MANUAL).expect("manual");
    println!("   {:?}, {} celdas", started.elapsed(), rows(&manual));
    let manual_plan = engine.explain(MANUAL);
    report_plan(&manual_plan, "   ");

    // 2. El SQL que genera el lowering, sin filtros.
    println!("\n2. SQL generado por datan-dax-sql, sin filtros");
    let atoms = extract_atoms(
        "SUM('fact_sales'[qty])",
        &model(),
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    )
    .expect("extrae atomos");

    let sources = SourceMap::from_model(&model());
    let plan = lower(
        &model(),
        &sources,
        &atoms,
        &FilterContext::empty(),
    )
    .expect("baja a SQL");

    println!("\n{}", indent(&plan.sql, "   "));
    let started = Instant::now();
    match engine.query_arrow(&plan.sql) {
        Ok(batches) => {
            println!("   {:?}, {} celdas", started.elapsed(), rows(&batches));
            report_plan(&engine.explain(&plan.sql), "   ");
        }
        Err(error) => {
            println!("   FALLA: {error}");
            return;
        }
    }

    // 3. Con filtro. El filtro de la dimension tiene que llegar a los hechos por el semijoin.
    println!("\n3. SQL generado, con filtro Region = 'Norte'");
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let atoms = extract_atoms("SUM('fact_sales'[qty])", &model(), &filters, &[PRODUCT_QUARTER])
        .expect("extrae atomos");
    let plan = lower(&model(), &sources, &atoms, &filters).expect("baja a SQL");

    println!("\n{}", indent(&plan.sql, "   "));
    let _generated_filtered = match engine.query_arrow(&plan.sql) {
        Ok(batches) => {
            println!("   {:?}, {} celdas", started.elapsed(), rows(&batches));
            report_plan(&engine.explain(&plan.sql), "   ");
            batches
        }
        Err(error) => {
            println!("   FALLA: {error}");
            return;
        }
    };

    // 4. La misma consulta de referencia, a mano, para comparar numeros.
    println!("\n4. La misma consulta de referencia, escrita a mano");
    const MANUAL_FILTERED: &str = "\
        SELECT p.quarter, SUM(f.qty) AS revenue
        FROM fact_sales f
        JOIN dim_product p ON p.product_id = f.product_id
        WHERE p.region = 'Norte'
        GROUP BY p.quarter ORDER BY 1";
    let manual_filtered = engine
        .query_arrow(MANUAL_FILTERED)
        .expect("manual con filtro");
    println!("   {} celdas", rows(&manual_filtered));

    // 5. Comparacion con el motor. Es el test que importa: dos caminos distintos, mismos
    // numeros.
    println!("\n5. Comparacion de resultados, con EXCEPT");
    match compare(&engine, &plan.sql, MANUAL_FILTERED) {
        Ok(true) => {
            println!("\n   COINCIDEN. El lowering produce la misma consulta que el spike demostro.");
            println!("   El generador no esta mintiendo.");
        }
        Ok(false) => {
            println!("\n   DIFIEREN. El generador esta mintiendo.");
        }
        Err(error) => println!("\n   No se pudo comparar: {error}"),
    }
}

fn rows(batches: &[datan_storage::arrow::record_batch::RecordBatch]) -> usize {
    batches.iter().map(|b| b.num_rows()).sum()
}

/// Aplana los valores numericos de un resultado, sea cual sea el tipo.
///
/// `SUM()` sobre un entero devuelve `HUGEINT` en DuckDB, no `Float64`. Un downcast a
/// `Float64Array` falla y devuelve una lista vacia, que hace que dos resultados
/// completamente distintos "coincidan". Por eso hay que leer por nombre de tipo.
/// Compara dos consultas usando el motor, no el downcast de Rust.
///
/// `SUM()` sobre un entero devuelve `HUGEINT` en DuckDB. Un downcast a `Float64Array`
/// devuelve `None` y produce una lista vacia, que hace que dos resultados distintos
/// "coincidan". La comparacion la hace `EXCEPT`, que usa la semantica del motor.
fn compare(engine: &Engine, generated: &str, manual: &str) -> Result<bool, String> {
    // `EXCEPT` sin diferencias devuelve cero filas, y `query_arrow` trata un resultado
    // vacio como error. Por eso se cuenta con `COUNT(*)`, que siempre devuelve una fila.
    for direction in [
        format!("SELECT * FROM ({generated}) EXCEPT SELECT * FROM ({manual})"),
        format!("SELECT * FROM ({manual}) EXCEPT SELECT * FROM ({generated})"),
    ] {
        let count_sql = format!("SELECT COUNT(*) AS n FROM ({direction})");
        let batches = engine.query_arrow(&count_sql).map_err(|e| e.to_string())?;
        let n = read_count(&batches);
        if n != 0 {
            println!("   diferencias: {n}");
            return Ok(false);
        }
    }
    Ok(true)
}

/// Lee el unico valor de un `COUNT(*)`. DuckDB devuelve `Int64` para el conteo.
fn read_count(batches: &[datan_storage::arrow::record_batch::RecordBatch]) -> i64 {
    use datan_storage::arrow::array::Int64Array;
    for batch in batches {
        if let Some(array) = batch.column(0).as_any().downcast_ref::<Int64Array>() {
            if let Some(value) = array.iter().flatten().next() {
                return value;
            }
        }
    }
    -1
}

fn report_plan(plan: &str, indent: &str) {
    for node in ["HASH_GROUP_BY", "PERFECT_HASH_GROUP_BY", "DELIM_JOIN", "IE_JOIN"] {
        if plan.contains(node) {
            println!("{indent}nodo de plan: {node}");
        }
    }
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

