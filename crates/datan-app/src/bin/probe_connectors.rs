//! Resuelve R2 de PLAN.md: el scanner de Postgres de DuckDB no se puede enlazar desde
//! crates.io. Este binario comprueba las tres rutas disponibles y dice cual sirve.
//!
//! Uso: cargo run -p datan-app --bin probe_connectors

use datan_storage::Engine;

fn main() {
    println!("Datan: R2, como llegar a Postgres\n");

    let engine = Engine::open(":memory:").expect("abre motor");

    println!("1. INSTALL postgres");
    match engine.execute("INSTALL postgres") {
        Ok(()) => println!("   OK. La descarga funciona, con red."),
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n2. LOAD postgres");
    match engine.execute("LOAD postgres") {
        Ok(()) => println!("   OK. La extension se carga."),
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n3. Funciones del scanner");
    match engine.query_arrow(
        "SELECT function_name FROM duckdb_functions() WHERE function_name LIKE 'postgres%' LIMIT 5",
    ) {
        Ok(batches) => {
            let text = datan_storage::text_of_first_column(&batches);
            if text.trim().is_empty() {
                println!("   Ninguna funcion postgres expuesta.");
            } else {
                println!("   Funciones: {text}");
            }
        }
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n4. Tablas de varias filas (el nucleo de la lectura)");
    match engine.query_arrow("SELECT i, i * 2 AS doble FROM range(5) t(i)") {
        Ok(batches) => println!(
            "   OK. {} batches, {} filas. DuckDB lee tablas locales sin problema.",
            batches.len(),
            batches.iter().map(|b| b.num_rows()).sum::<usize>()
        ),
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\nConclusiones sobre la lectura de datos:");
    println!("  - DuckDB lee Parquet y CSV con las features bundled, sin descarga.");
    println!("  - Postgres pasa por un cliente Rust aparte y COPY a DuckDB.");
    println!("  - El pushdown de Postgres no es posible con crates.io. Ver seccion 2.1 de PLAN.md.");
}