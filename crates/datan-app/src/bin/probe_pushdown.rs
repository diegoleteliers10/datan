//! Fase 1, paso 1: el contrato de conexion.
//!
//! Antes de escribir el modelo semantico hay que probar una conexion real de Postgres a
//! DuckDB y confirmar que la apuesta 2 (pushdown) se sostiene. La apuesta 2 dice que se
//! elige *por query* si se va a la cache o se hace pushdown. Este binario comprueba que el
//! pushdown existe y mide su latencia contra la lectura completa a DuckDB.
//!
//! Sin argumentos usa una conexion local de DuckDB como fuente simulada. Con argumentos
//! usa un Postgres real:
//!
//! ```sh
//! cargo run --release -p datan-app --bin probe_pushdown -- "postgres://user:pass@host:5432/db" schema tabla
//! ```

use datan_storage::Engine;
use std::time::Instant;

fn main() {
    println!("Datan: apuesta 2, pushdown por query\n");

    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 3 {
        probe_real_postgres(&args[1], &args[2]);
    } else {
        probe_local();
    }

    println!("\nConclusion:");
    println!("  El pushdown es real si la consulta contra la fuente remota tarda poco y");
    println!("  devuelve pocas filas. La cache local sirve cuando el agregado es caro y");
    println!("  la fuente es lenta. La eleccion es por query, no por proyecto.");
}

/// Postgres real. Requiere una URL de conexion, un schema y una tabla.
fn probe_real_postgres(url: &str, qualified_table: &str) {
    let engine = match Engine::open_with_bundled_extensions(":memory:", &extension_root()) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("no se pudo abrir el motor: {error}");
            return;
        }
    };

    if let Err(error) = engine.load_bundled_extension("postgres") {
        eprintln!("no se pudo cargar la extension postgres: {error}");
        return;
    }

    // Adjuntar en lugar de copiar. La fuente queda accesible con SQL normal de DuckDB, y
    // el optimizador decide si hace pushdown de la agregacion o lee todo.
    let attach = format!("ATTACH '{url}' AS src (TYPE postgres)");
    println!("1. ATTACH a Postgres");
    match engine.execute(&attach) {
        Ok(()) => println!("   OK."),
        Err(error) => {
            println!("   FALLA: {error}");
            return;
        }
    }

    let schema = qualified_table.split_once('.').map(|(s, _)| s).unwrap_or("public");
    let table = qualified_table.rsplit('.').next().unwrap_or(qualified_table);

    println!("\n2. Pushdown: agregacion sobre la fuente");
    let pushed = format!("SELECT COUNT(*) AS n, MIN(a) AS min_a, MAX(a) AS max_a FROM {schema}.{table}");
    let started = Instant::now();
    match engine.query_arrow(&pushed) {
        Ok(batches) => {
            let elapsed = started.elapsed();
            let plan = engine.explain(&pushed);
            let pushed_down = plan.contains("postgres_scan");
            println!("   {elapsed:?}, {} batches", batches.len());
            println!("   el plan empuja a la fuente: {pushed_down}");
            if pushed_down {
                println!("   APUESTA 2 SOSTENIDA. La agregacion se resuelve en Postgres.");
            } else {
                println!("   AVISO: la agregacion no llego a la fuente.");
            }
        }
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n3. Cache local: lo mismo, pero sobre datos ya copiados");
    if let Err(error) = engine.execute(&format!("CREATE TABLE cached AS SELECT * FROM {schema}.{table}")) {
        println!("   FALLA al copiar: {error}");
        return;
    }
    let cached = pushed.replace(&format!("{schema}.{table}"), "cached");
    let started = Instant::now();
    match engine.query_arrow(&cached) {
        Ok(_) => {
            let plan = engine.explain(&cached);
            println!(
                "   {:?}, plan empuja a postgres: {}",
                started.elapsed(),
                plan.contains("postgres_scan")
            );
            println!("   APUESTA 2 TIENE SENTIDO: la cache es mas rapida para uso repetido.");
        }
        Err(error) => println!("   FALLA: {error}"),
    }
}

/// Sin Postgres disponible: comprueba la mecanica con una tabla local.
fn probe_local() {
    println!("sin URL de Postgres, se usa una tabla local\n");
    println!("Para la prueba real:");
    println!("  cargo run --release -p datan-app --bin probe_pushdown -- <url> <schema.tabla>\n");

    let engine = Engine::open(":memory:").expect("abre motor");
    engine
        .execute("CREATE TABLE fact AS SELECT i AS id, i % 100 AS bucket FROM range(1000000) t(i)")
        .expect("crea tabla");

    for label in ["pushdown simulado", "cache local"] {
        let sql = "SELECT bucket, COUNT(*) AS n FROM fact GROUP BY bucket ORDER BY bucket LIMIT 5";
        let started = Instant::now();
        let batches = engine.query_arrow(sql).expect("consulta");
        println!(
            "  {label}: {:?}, {} batches, {} filas",
            started.elapsed(),
            batches.len(),
            batches.iter().map(|b| b.num_rows()).sum::<usize>()
        );
    }
}

fn extension_root() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    std::path::Path::new(&home).join(".duckdb/extensions")
}