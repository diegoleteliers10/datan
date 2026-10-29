//! Spike de la Fase 0, dia 2. Ver seccion 6 de PLAN.md.
//!
//! La pregunta que decide la arquitectura: una medida tipica de Power BI se resuelve con
//! UN hash aggregate de DuckDB, o cae en subconsultas correlacionadas?
//!
//! La forma probada es la de la Fase 1:
//!
//! ```dax
//! Revenue  = SUMX('Fact Sales', 'Fact Sales'[Qty] * 'Dim Product'[Price])
//! Margin   = ( [Revenue] - [COGS] ) / [Revenue]
//! ```
//!
//! que en SQL es un join entre hechos y dimension mas un `SUM(qty * price)`. El riesgo
//! R6 de la seccion 6 es que un GROUP BY ingenuo produzca un `DELIM_JOIN` por celda de
//! grupo, lo que haria inviable el fast path y obligaria a tirar el backend SQL entero.
//!
//! Uso: cargo run -p datan-app --bin spike-day2 --release

use std::time::Instant;

fn main() {
    println!("Datan spike, dia 2: una query o N subconsultas?\n");

    let engine = datan_storage::Engine::open(":memory:").expect("abre motor");

    setup_fixtures(&engine);
    report(engine);

    println!("\nCriterio de aceptacion de PLAN.md, seccion 6, dia 2:");
    println!("  - un hash aggregate");
    println!("  - ninguna subconsulta correlacionada");
    println!("  - menos de 500 ms sobre 10M de filas de hechos");
}

/// Crea el star schema de la Fase 1: 10M de hechos, 500 productos, 5000 clientes.
fn setup_fixtures(engine: &datan_storage::Engine) {
    let started = Instant::now();
    engine
        .execute(
            "CREATE TABLE dim_product AS
             SELECT
                 i AS product_id,
                 'Producto ' || i::VARCHAR AS product,
                 ['Norte','Sur','Este','Oeste'][(i % 4) + 1] AS region,
                 ['2025-Q1','2025-Q2','2025-Q3','2025-Q4'][(i % 4) + 1] AS quarter
             FROM range(500) t(i)",
        )
        .expect("crea dim_product");

    engine
        .execute(
            "CREATE TABLE dim_customer AS
             SELECT
                 i AS customer_id,
                 'Cliente ' || i::VARCHAR AS customer,
                 ['Norte','Sur','Este','Oeste'][(i % 4) + 1] AS region
             FROM range(5000) t(i)",
        )
        .expect("crea dim_customer");

    // 10M de hechos. El precio vive en el producto, como en un star schema real.
    // El join tiene que resolver por indice, no por hash de 10M de filas.
    engine
        .execute(
            "CREATE TABLE fact_sales AS
             SELECT
                 i AS sale_id,
                 (hash(i) % 500) + 1 AS product_id,
                 (hash(i * 7) % 5000) + 1 AS customer_id,
                 (hash(i * 13) % 37) + 1 AS qty
             FROM range(10000000) t(i)",
        )
        .expect("crea fact_sales");

    // Analitico: precio y categoria pre-resueltos sobre la dimension. Esto es lo que hace
    // que el join sea de 500 filas y no de 10M.
    engine
        .execute(
            "CREATE TABLE fact_sales_enriched AS
             SELECT
                 f.sale_id,
                 f.product_id,
                 f.customer_id,
                 f.qty,
                 f.qty * ((p.product_id * 7 % 90) + 10)::DOUBLE AS revenue,
                 f.qty * ((p.product_id * 3 % 50) + 5)::DOUBLE AS cogs
             FROM fact_sales f
             JOIN dim_product p USING (product_id)",
        )
        .expect("crea fact_sales_enriched");

    println!(
        "fixtures listos en {:.0} ms: 10M hechos, 500 productos, 5000 clientes\n",
        started.elapsed().as_millis()
    );
}

fn report(engine: datan_storage::Engine) {
    // La query que Datan genera para `Revenue = SUMX('Fact Sales', [Qty] * [Price])`
    // agrupado por region y quarter. Todas las celdas de grupo en una pasada.
    const QUERY: &str = "\
        SELECT
            product_region,
            product_quarter,
            SUM(qty * price) AS revenue,
            SUM(cogs) AS cogs_total,
            SUM(qty) AS qty_total
        FROM (
            SELECT
                p.region AS product_region,
                p.quarter AS product_quarter,
                f.qty,
                ((p.product_id * 7 % 90) + 10)::DOUBLE AS price,
                f.qty * ((p.product_id * 3 % 50) + 5)::DOUBLE AS cogs
            FROM fact_sales f
            JOIN dim_product p USING (product_id)
        ) sales
        GROUP BY product_region, product_quarter
        ORDER BY product_region, product_quarter";

    let started = Instant::now();
    let batches = engine.query_arrow(QUERY).expect("ejecuta la query");
    let elapsed = started.elapsed();

    let row_count: usize = batches.iter().map(|b| b.num_rows()).sum();
    println!(
        "GRUPOS: {row_count} celdas en {:.0} ms",
        elapsed.as_millis()
    );

    // `EXPLAIN ANALYZE` devuelve el plan como texto en una columna. La lectura pasa por
    // `datan-storage` para que este binario no importe tipos de duckdb ni de arrow, igual
    // que la app principal.
    let text = engine.explain(QUERY);

    {
        let mut relevant: Vec<String> = Vec::new();
        for line in text.lines() {
            let upper = line.to_uppercase();
            if upper.contains("HASH_GROUP_BY")
                || upper.contains("PERFECT_HASH_GROUP_BY")
                || upper.contains("DELIM_JOIN")
                || upper.contains("CROSS_PRODUCT")
                || upper.contains("RIGHT_DELIM_JOIN")
                || upper.contains("IE_JOIN")
                || upper.contains("LEFT_DELIM_JOIN")
                || upper.contains("SINGLE_JOIN")
            {
                relevant.push(line.trim().to_string());
            }
        }
        println!("\nNODOS DE PLAN RELEVANTES:");
        if relevant.is_empty() {
            println!("  (ninguno)");
        }
        for line in relevant {
            println!("  {line}");
        }

        let delim = text.to_uppercase().contains("DELIM_JOIN");
        let has_aggregate = text.to_uppercase().contains("HASH_GROUP_BY")
            || text.to_uppercase().contains("PERFECT_HASH_GROUP_BY");

        println!("\nVEREDICTO:");
        println!("  hash aggregate presente:  {has_aggregate}");
        println!("  DELIM_JOIN (subconsulta por celda): {delim}");
        println!(
            "  tiempo: {:.0} ms (criterio: < 500 ms)",
            elapsed.as_millis()
        );

        let pass = has_aggregate && !delim && elapsed.as_millis() < 500;
        println!(
            "\n  {}",
            if pass {
                "PASA. El fast path SQL es viable. La arquitectura se sostiene."
            } else {
                "FALLA. El fast path SQL no sirve. Hay que revisar la arquitectura."
            }
        );
    }
}
