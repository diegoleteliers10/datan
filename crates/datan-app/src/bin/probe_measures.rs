//! Test de semana 1 de la Fase 1, tal y como lo pide el plan.
//!
//! ANTES de escribir el generador: escribir las cuatro medidas A MANO en SQL, correr
//! `EXPLAIN ANALYZE`, y afirmar que hay UN solo nodo de agregacion hash y ningun
//! `DELIM_JOIN`.
//!
//! Si el conjunto fijo de medidas no colapsa en una pasada, la arquitectura del proyecto
//! cambia: habria que ir a subconsulta por celda, y el grid de 1M filas dejaria de tener
//! un presupuesto de latencia.
//!
//! Las medidas:
//!
//! ```text
//! Revenue = SUM('pb'[quantity] * 'pb'[precio_unitario])
//! COGS    = SUM('pb'[quantity] * 'cp'[costo])
//! Qty     = SUM('pb'[quantity])
//! Margin  = (Revenue - COGS) / Revenue
//! ```
//!
//! `Margin` es la que decide: referencia OTRAS medidas dos veces. Si el motor emite una
//! subconsulta por medida para resolver la referencia, aparece un `DELIM_JOIN` y la
//! pregunta queda contestada antes de escribir una linea de generador.
//!
//! Uso: cargo run --release -p datan-app --bin probe_measures

use datan_storage::Engine;
use datan_storage::arrow::array::{
    Array, Decimal128Array, Float64Array, Int32Array, Int64Array, StringArray,
};
use datan_storage::arrow::record_batch::RecordBatch;

/// Las cuatro medidas, escritas a mano. Esta es la linea base contra la que se compara el
/// generador mas adelante.
const MANUAL: &str = "SELECT
    cp.categoria,
    SUM(pb.quantity * pb.precio_unitario) AS revenue,
    SUM(pb.quantity * cp.costo) AS cogs,
    SUM(pb.quantity) AS qty,
    (SUM(pb.quantity * pb.precio_unitario) - SUM(pb.quantity * cp.costo))
        / NULLIF(SUM(pb.quantity * pb.precio_unitario), 0) AS margin
FROM fact_line pb
JOIN dim_product cp ON cp.product_key = pb.product_key
GROUP BY cp.categoria";

fn fixtures() -> Engine {
    let engine = Engine::open(":memory:").expect("motor");

    // El hecho tiene la forma de `product_boleta`: una linea por venta, con cantidad y
    // precio unitario. El producto aporta el costo, que en el esquema real no existe.
    engine
        .execute(
            "CREATE TABLE fact_line AS
             SELECT
                 i AS line_key,
                 ((i * 7) % 500)::INTEGER AS product_key,
                 1 + (i % 5) AS quantity,
                 (1000 + ((i * 13) % 50) * 500)::INTEGER AS precio_unitario
             FROM range(1, 1000001) t(i)",
        )
        .expect("hechos");

    engine
        .execute(
            "CREATE TABLE dim_product AS
             SELECT
                 i AS product_key,
                 'Producto ' || i AS nombre,
                 ['Almacen', 'Ferreteria', 'Limpieza', 'Oficina'][1 + (i % 4)] AS categoria,
                 (400 + (i % 30) * 300)::INTEGER AS costo
             FROM range(0, 500) t(i)",
        )
        .expect("productos");

    engine
}

/// Una celda del resultado como texto.
///
/// `SUM` de enteros devuelve `HUGEINT` y el ratio devuelve un decimal, asi que cada columna
/// puede ser de un tipo distinto. Downcastear todo a `Int32` no funciona y formatear cada
/// tipo a mano duplica el codigo del probe.
fn cell(batch: &RecordBatch, column: usize, row: usize) -> String {
    let array = batch.column(column);
    if array.null_count() > 0 && array.is_null(row) {
        return "NULL".to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        return values.value(row).to_string();
    }
    // El resto son enteros grandes y decimales. `SUM` de enteros devuelve `HUGEINT` y el
    // ratio un decimal, asi que cada columna tiene su tipo y el formateo va por tipo.
    if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        return values.value(row).to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<Int32Array>() {
        return values.value(row).to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        return format!("{:.4}", values.value(row));
    }
    // `SUM` de enteros devuelve `HUGEINT`, que Arrow representa como `Decimal128`. No es
    // un `Int64` con otro nombre: son tipos distintos y hay que formatearlos aparte.
    if let Some(values) = array.as_any().downcast_ref::<Decimal128Array>() {
        return values.value(row).to_string();
    }
    format!("{:?}", array.data_type())
}

fn main() {
    println!("Datan, Fase 1, test de semana 1: las cuatro medidas en una pasada\n");

    let engine = fixtures();

    // 1. El plan. Un solo nodo de agregacion, sin DELIM_JOIN.
    println!("--- Plan de ejecucion ---");
    let plan = engine.explain(&MANUAL);
    print!("{plan}");

    let aggregate_nodes = plan.matches("HASH_GROUP_BY").count();
    let delim_joins = plan.matches("DELIM_JOIN").count();

    println!();
    println!("  HASH_GROUP_BY: {aggregate_nodes}");
    println!("  DELIM_JOIN:    {delim_joins}");

    // 2. El veredicto, con las palabras del plan.
    println!();
    if aggregate_nodes == 1 && delim_joins == 0 {
        println!("  PASA. Un conjunto fijo de medidas SI colapsa en una pasada.");
        println!("  Margin puede referenciar Revenue dos veces sin subconsulta: la");
        println!("  expresion se repite, no se resuelve por join.");
        println!("  El generador tiene entonces un objetivo concreto y medible:");
        println!("  producir SQL de esta forma, celda de grupo incluida.");
    } else {
        println!("  FALLA. El conjunto de medidas no cabe en una pasada.");
        println!("  Habria que ir a subconsulta por celda, y el presupuesto de latencia del");
        println!("  grid de 1M filas cambia por completo. La arquitectura depende de esto.");
    }

    // 3. Los numeros, que son la linea base del EXCEPT de mas adelante.
    println!("\n--- Resultados de la linea base ---");
    let rows = engine
        .query_arrow(&format!("{MANUAL} ORDER BY 1"))
        .unwrap_or_default();
    println!("  categoria   revenue      cogs       qty      margin");
    for row in 0..rows.first().map(|b| b.num_rows()).unwrap_or(0) {
        let batch = rows.first().expect("batch");
        println!(
            "  {:11} {:>9} {:>10} {:>7} {:>9}",
            cell(batch, 0, row),
            cell(batch, 1, row),
            cell(batch, 2, row),
            cell(batch, 3, row),
            cell(batch, 4, row),
        );
    }

    // 4. El presupuesto de latencia. Sin esto el test dice la verdad sin decir si sirve.
    println!("\n--- Latencia sobre 1M de lineas ---");
    let mut samples = Vec::new();
    for _ in 0..20 {
        samples.push(engine.timed(&MANUAL).expect("cronometra"));
    }
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95) / 100];
    println!("  p50 {p50} ms, p95 {p95} ms");

    if p95 < 300 {
        println!("  Cabe en el presupuesto de 300 ms de la fase.");
    } else {
        println!("  FUERA de presupuesto. El presupuesto de la fase hay que revisarlo");
        println!("  antes de prometer un cross-filter por debajo de 300 ms.");
    }
}
