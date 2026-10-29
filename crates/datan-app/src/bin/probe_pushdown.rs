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

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some("--bi") && args.len() >= 2 {
        probe_bi_shape(&args[1]);
    } else if args.first().map(String::as_str) == Some("--directo") && args.len() >= 2 {
        probe_pushdown_directo(&args[1]);
    } else if args.len() >= 2 {
        probe_real_postgres(&args[0], &args[1]);
    } else if args.len() == 1 {
        list_tables(&args[0]);
    } else {
        probe_local();
    }
}

/// Prueba definitiva del pushdown con la funcion documentada.
///
/// Si `postgres_scan_pushdown` resuelve la agregacion en Postgres, la capacidad existe y lo
/// que falta es que el optimizador la elija. Si tampoco la usa, DuckDB 1.5.6 no empuja
/// agregaciones a Postgres y la apuesta 2 hay que reescribirla.
fn probe_pushdown_directo(url: &str) {
    println!("\nDatan: pushdown directo con postgres_scan_pushdown\n");

    let engine = match Engine::open_with_bundled_extensions(":memory:", &extension_root()) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("no se pudo abrir el motor: {error}");
            return;
        }
    };
    if let Err(error) = engine.load_bundled_extension("postgres") {
        eprintln!("no se pudo cargar postgres: {error}");
        return;
    }
    if let Err(error) = engine.execute(&format!("ATTACH '{url}' AS src (TYPE postgres)")) {
        eprintln!("no se pudo conectar: {error}");
        return;
    }

    // 1. La misma agregacion por las dos vias, sobre la misma tabla.
    for (label, sql) in [
        (
            "via normal",
            "SELECT region, SUM(price) AS total FROM src.sales.dim_product GROUP BY region ORDER BY 1",
        ),
        (
            "via postgres_scan_pushdown",
            "SELECT region, total FROM postgres_scan_pushdown('src', 'sales.dim_product', \
             'region:=region, total:=SUM(price)') ORDER BY 1",
        ),
    ] {
        let started = Instant::now();
        match engine.query_arrow(sql) {
            Ok(batches) => {
                let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
                println!("   {label}: {:?}, {rows} celdas", started.elapsed());
            }
            Err(error) => println!("   {label}: FALLA {error}"),
        }
    }

    // 2. El caso que decide el producto: una agregacion sobre 10M de filas.
    let grande = "SELECT COUNT(*) AS n, SUM(qty) AS q FROM src.sales.fact_sales";
    let started = Instant::now();
    match engine.query_arrow(grande) {
        Ok(batches) => {
            println!("\n   10M de filas agregadas: {:?}", started.elapsed());
            let n: usize = batches.iter().map(|b| b.num_rows()).sum();
            println!("   {n} fila de resultado");
        }
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n   Si la agregacion de 10M tarda lo mismo que COUNT(*) sobre la tabla,");
    println!("   las filas viajan todas y no hay pushdown de agregacion.");
}

/// La forma real de BI: un GROUP BY con join a una dimension.
///
/// Es la que Datan genera para `Revenue = SUMX('Fact Sales', [Qty] * [Price])` agrupado por
/// region y quarter. Si esta forma no empuja, la apuesta 2 no se sostiene como esta escrita.
fn probe_bi_shape(url: &str) {
    println!("\nDatan: la forma real de BI, GROUP BY con join\n");

    let engine = match Engine::open_with_bundled_extensions(":memory:", &extension_root()) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("no se pudo abrir el motor: {error}");
            return;
        }
    };
    if let Err(error) = engine.load_bundled_extension("postgres") {
        eprintln!("no se pudo cargar postgres: {error}");
        return;
    }
    if let Err(error) = engine.execute(&format!("ATTACH '{url}' AS src (TYPE postgres)")) {
        eprintln!("no se pudo conectar: {error}");
        return;
    }

    let bi = "
        SELECT p.region, p.quarter,
               SUM(f.qty * p.price) AS revenue,
               SUM(f.qty * p.cogs)  AS cogs,
               COUNT(*)             AS ventas
        FROM src.sales.fact_sales f
        JOIN src.sales.dim_product p ON p.product_id = f.product_id
        GROUP BY p.region, p.quarter
        ORDER BY 1, 2";

    println!("1. GROUP BY con join, directo a Postgres");
    let started = Instant::now();
    match engine.query_arrow(bi) {
        Ok(batches) => {
            let elapsed = started.elapsed();
            let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            let plan = engine.explain(bi);
            println!("   {elapsed:?}, {rows} celdas de grupo");
            println!("   nodo de scan: {}", scan_node(&plan));
            println!(
                "   la agregacion llego a Postgres: {}",
                plan.to_uppercase().contains("POSTGRES_SCAN_PUSHDOWN")
            );
        }
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n2. La misma forma, contra la cache local");
    if let Err(error) = engine.execute(
        "CREATE TABLE fact_local AS SELECT * FROM src.sales.fact_sales;
         CREATE TABLE product_local AS SELECT * FROM src.sales.dim_product",
    ) {
        println!("   FALLA al copiar: {error}");
        return;
    }
    let cached = bi
        .replace("src.sales.fact_sales", "fact_local")
        .replace("src.sales.dim_product", "product_local");
    let started = Instant::now();
    match engine.query_arrow(&cached) {
        Ok(batches) => {
            println!(
                "   {:?}, {} celdas, nodo de scan: {}",
                started.elapsed(),
                batches.iter().map(|b| b.num_rows()).sum::<usize>(),
                scan_node(&engine.explain(&cached))
            );
            println!("\n   La cache local no necesita pushdown. Esa es la apuesta 2:");
            println!("   el motor elige por query entre fuente y cache.");
        }
        Err(error) => println!("   FALLA: {error}"),
    }
}

/// Extrae el nombre del nodo de scan del plan.
fn scan_node(plan: &str) -> String {
    for line in plan.lines() {
        let upper = line.to_uppercase();
        if upper.contains("_SCAN") {
            return line
                .trim()
                .trim_matches(|c: char| c == '│' || c == '┌' || c == '┐' || c == '└' || c == '┘')
                .trim()
                .to_string();
        }
    }
    "ninguno".to_string()
}

/// Conecta y lista tablas. No asume nombres ni el schema: una base nueva no tiene nada que
/// ver con el star schema de la Fase 1, y adivinar una tabla produce un error que no dice
/// nada sobre el pushdown.
fn list_tables(url: &str) {
    println!("Conectando y listando tablas\n");

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

    println!("1. ATTACH");
    match engine.execute(&format!("ATTACH '{url}' AS src (TYPE postgres)")) {
        Ok(()) => println!("   OK."),
        Err(error) => {
            println!("   FALLA: {error}");
            println!("\nSi es Supabase, hace falta sslmode=require en la URL, y si la red");
            println!("es solo IPv4 hay que usar el host del pooler en vez del directo.");
            return;
        }
    }

    println!("\n2. Tablas visibles");
    // `duckdb_tables()` usa `schema_name`, no `table_schema`. Y `information_schema` sin
    // calificar muestra el de DuckDB, no el de Postgres: hay que prefijarlo con el alias.
    match engine.query_arrow(
        "SELECT schema_name || '.' || table_name FROM duckdb_tables() \
         WHERE database_name = 'src' \
           AND schema_name NOT IN ('information_schema', 'pg_catalog') \
         ORDER BY 1 LIMIT 40",
    ) {
        Ok(batches) => {
            println!(
                "   {} tablas",
                batches.iter().map(|b| b.num_rows()).sum::<usize>()
            );
            println!("   {}", datan_storage::text_of_first_column(&batches));
        }
        Err(error) => {
            println!("   duckdb_tables no funciona: {error}");
            match engine.query_arrow(
                "SELECT table_schema || '.' || table_name FROM src.information_schema.tables \
                 WHERE table_schema NOT LIKE 'pg_%' ORDER BY 1 LIMIT 40",
            ) {
                Ok(batches) => {
                    println!("   {}", datan_storage::text_of_first_column(&batches))
                }
                Err(error) => println!("   FALLA: {error}"),
            }
        }
    }
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

    // En DuckDB las tablas de un attach se califican con el alias: `src.sales.dim_product`.
    let qualified = format!("src.{qualified_table}");
    let table = qualified_table
        .rsplit('.')
        .next()
        .unwrap_or(qualified_table);

    // La columna de la agregacion se descubre, no se adivina. Una base real no tiene por
    // que llamarse `a`.
    let numeric_column = match engine.query_arrow(&format!(
        "SELECT column_name FROM src.information_schema.columns \
         WHERE table_name = '{table}' \
           AND data_type IN ('smallint','integer','bigint','numeric','real','double precision') \
         ORDER BY ordinal_position LIMIT 1"
    )) {
        Ok(batches) => datan_storage::text_of_first_column(&batches)
            .trim()
            .trim_end_matches(',')
            .trim()
            .to_string(),
        Err(error) => {
            println!("   no se pudo leer el catalogo: {error}");
            return;
        }
    };

    if numeric_column.is_empty() {
        println!("   la tabla {qualified_table} no tiene columnas numericas");
        return;
    }
    println!("   columna de agregacion: {numeric_column}");

    println!("\n2. Pushdown: agregacion sobre la fuente");
    let pushed = format!(
        "SELECT COUNT(*) AS n, MIN({numeric_column}) AS min_c, MAX({numeric_column}) AS max_c FROM {qualified}"
    );
    let started = Instant::now();
    match engine.query_arrow(&pushed) {
        Ok(batches) => {
            let elapsed = started.elapsed();
            let plan = engine.explain(&pushed);
            let pushed_down = plan.to_lowercase().contains("postgres");
            println!("   {elapsed:?}, {} batches", batches.len());
            println!("   el plan usa la fuente: {pushed_down}");
            println!("\n   PLAN:");
            for line in plan.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('│')
                    || trimmed.contains(|c: char| c.is_ascii_alphabetic() && c.is_uppercase())
                {
                    println!("   {trimmed}");
                }
            }
            if pushed_down {
                println!("   APUESTA 2 SOSTENIDA. La agregacion se resuelve en Postgres.");
            } else {
                println!("   AVISO: la agregacion no llego a la fuente.");
            }
        }
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n3. Cache local: lo mismo, pero sobre datos ya copiados");
    if let Err(error) = engine.execute(&format!("CREATE TABLE cached AS SELECT * FROM {qualified}"))
    {
        println!("   FALLA al copiar: {error}");
        return;
    }
    let cached = pushed.replace(&qualified, "cached");
    let started = Instant::now();
    match engine.query_arrow(&cached) {
        Ok(_) => {
            let plan = engine.explain(&cached);
            println!(
                "   {:?}, plan usa postgres: {}",
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
