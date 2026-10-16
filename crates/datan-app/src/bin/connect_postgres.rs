//! Conectar a Postgres y ver el modelo que sale.
//!
//! El camino completo de la app, en un binario sin GPU: extension, `ATTACH`, catalogo de
//! Postgres, modelo, medidas generadas. Si esto funciona, la app tiene la mitad del
//! pipeline y solo falta pintarlo.
//!
//! Uso:
//!   cargo run --release -p datan-app --bin connect_postgres -- [base] [esquema]

use std::path::Path;

use datan_connect::introspect;
use datan_dax_eval::extract_atoms;
use datan_dax_sql::{SourceMap, lower};
use datan_model::{DataType, ObjectId, TableKind};
use datan_storage::Engine;

fn main() {
    let mut args = std::env::args().skip(1);
    let database = args.next().unwrap_or_else(|| "datan_bench".to_string());
    // El esquema se acepta y se ignora a proposito: `introspect` lee todos los esquemas de
    // usuario, que es lo que quiere alguien que abre la app. Filtrar por uno seria una
    // decision de producto, y no esta tomada.
    let _schema = args.next().unwrap_or_else(|| "public".to_string());

    println!("Datan: conectar a Postgres y mirar el modelo\n");

    // 1. El motor con la extension precargada. Sin red: si aqui falla, la app tampoco
    //    arranca en la maquina de un usuario.
    let home = std::env::var("HOME").expect("HOME");
    let root = Path::new(&home).join(".duckdb").join("extensions");

    let engine = match Engine::open_with_bundled_extensions(":memory:", &root) {
        Ok(engine) => engine,
        Err(error) => {
            println!("  FALLO abrir el motor: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = engine.load_bundled_extension("postgres") {
        println!("  FALLO cargar la extension: {error}");
        std::process::exit(1);
    }
    println!("  1. Extension postgres cargada desde {root:?}");

    // 2. Conectar. `ATTACH` con el DSN de libpq; DuckDB reenvia la conexion al driver de
    //    Postgres, asi que no hay dosPooling de conexiones.
    let attach = format!(
        "ATTACH 'host=localhost dbname={database}' AS pg (TYPE POSTGRES, READ_ONLY)"
    );
    if let Err(error) = engine.execute(&attach) {
        println!("  FALLO conectar a la base `{database}`: {error}");
        println!();
        println!("  Comprobaciones:");
        println!("    brew services list | grep postgres");
        println!("    psql -d {database} -c '\\\\dt'");
        std::process::exit(1);
    }
    println!("  2. Conectado a `{database}` como `pg`, solo lectura");

    // 3. El catalogo. `information_schema` de Postgres, no el de DuckDB: el de DuckDB
    //    describiría la vista que el scanner projecting, que no es el esquema real.
    // El catalogo se lee por conexion directa. El scanner de DuckDB pierde las claves
    // foraneas en silencio: `probe_scanner` lo mide.
    let dsn = format!("host=localhost dbname={database}");
    let mut catalog = match datan_connect::PostgresCatalog::connect(&dsn) {
        Ok(catalog) => catalog,
        Err(error) => {
            println!("  FALLO abrir el catalogo de Postgres: {error}");
            std::process::exit(1);
        }
    };
    println!("  3. Catalogo de Postgres leido por conexion directa");

    let introspection =
        match introspect(
        &engine,
        &datan_connect::Source::new("pg"),
        Some(&mut catalog),
        std::env::args().nth(2).as_deref(),
    ) {
        Ok(result) => result,
        Err(error) => {
            println!("  FALLO introspeccionar: {error}");
            std::process::exit(1);
        }
    };
    let model = &introspection.model;

    println!("\n  4. El modelo\n");
    println!("  candidatas a hechos: {:?}", introspection.fact_candidates);
    for line in &introspection.report {
        println!("{line}");
    }

    // 4. Las relaciones, que es donde se decide si el modelo es utilizable.
    println!("\n  5. Relationships");
    if model.relationships.is_empty() {
        println!("     ninguna. Sin claves foraneas no hay modelo: solo una tabla grande.");
    }
    for relationship in &model.relationships {
        let from = column_name(model, relationship.from_column);
        let to = column_name(model, relationship.to_column);
        println!(
            "     '{}' filtra a '{}'  ({:?}, {:?})",
            from, to, relationship.cardinality, relationship.direction
        );
    }

    // 5. Las medidas generadas. Todas tienen que bajar a SQL. Una medida generada que cae al
    //    interprete es un bug de Datan, no del usuario.
    println!("\n  6. Medidas");
    // La columna de agrupamiento es de una DIMENSION y no es clave.
    //
    // Agrupar por una clave del hecho no es un error: agrupa por 1M de valores distintos y
    // devuelve 1M de filas de una columna. Se ve como un grid lentisimo, no como un fallo,
    // que es la peor forma.
    let grouping: Vec<ObjectId> = model
        .tables
        .iter()
        .filter(|t| {
            matches!(t.kind, TableKind::Dimension)
                && model
                    .fact_table()
                    .map(|fact| model.join_path(fact, t.id).is_some())
                    .unwrap_or(false)
        })
        .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
        .filter(|(t, c)| matches!(c.data_type, DataType::Text) && !t.key_columns.contains(&c.id))
        .map(|(_, c)| c.id)
        .take(1)
        .collect();

    // El calificador es el alias del `ATTACH`. Sin el, DuckDB responde "table does not
    // exist" con una pista que es justo el nombre con el prefijo que falta.
    let sources = SourceMap::from_model(model).with_qualifier("pg");
    let filters = datan_dax_eval::FilterContext::empty();
    let mut bajan = 0usize;
    let mut no_bajan = 0usize;

    for measure in &model.measures {
        match extract_atoms(&measure.expression, model, &filters, &grouping) {
            Err(error) => {
                no_bajan += 1;
                println!("     {}  ERROR al extraer: {error}", measure.name);
            }
            Ok(atoms) => match lower(model, &sources, &atoms, &filters) {
                Ok(_) => bajan += 1,
                Err(error) => {
                    no_bajan += 1;
                    println!("     {}  NO baja: {error}", measure.name);
                }
            },
        }
    }
    println!("     {bajan} de {} medidas bajan a SQL", model.measures.len());
    if no_bajan > 0 {
        println!("     {no_bajan} no bajan. Una medida autogenerada que no baja es un bug.");
    }

    // 7. Una consulta de verdad, de punta a punta, contra Postgres. Un modelo que se
    //    imprime bien y una consulta que no corre son dos mitades, y aqui solo se prueba
    //    una.
    println!("\n  7. Consulta de punta a punta, agrupada por la dimension de texto");
    println!("     medidas generadas:");
    for measure in &model.measures {
        println!("       {} = {}", measure.name, measure.expression);
    }

    let dimension = grouping
        .first()
        .copied()
        .unwrap_or(model.tables[0].columns[0].id);
    let (dimension_table, dimension_column) = match model.column(dimension) {
        Some((table, column)) => (table.name.clone(), column.name.clone()),
        None => (String::from("?"), String::from("?")),
    };
    let fact = model
        .tables
        .iter()
        .find(|t| matches!(t.kind, TableKind::Facts))
        .expect("el modelo tiene una tabla de hechos");

    let measure = model
        .measures
        .iter()
        .find(|m| m.name.contains(&fact.name) && m.name.ends_with("quantity"))
        .cloned();
    let Some(measure) = measure else {
        println!("     ninguna medida de suma sobre `quantity`");
        return;
    };

    let atoms = extract_atoms(&measure.expression, model, &filters, &[dimension])
        .expect("extrae atomos");
    match lower(model, &sources, &atoms, &filters) {
        Ok(plan) => {
            println!("\n{}", indent(&plan.sql));
            // Se mide y se leen las filas directamente. Envolver la consulta en otro `SUM`
            // daria `SUM(SUM(quantity))`, que es un error de tipo, y quitar el envoltorio
            // compararia solo la primera celda.
            match engine.timed(&plan.sql) {
                Ok(ms) => {
                    println!(
                        "\n     {} en {ms} ms, agrupado por {dimension_table}[{dimension_column}]:",
                        measure.name
                    );
                    match engine.render_rows(&plan.sql) {
                        Ok(rows) => {
                            let mut total = 0.0;
                            for row in rows.iter().take(8) {
                                println!("       {}", row.join("  |  "));
                            }
                            if rows.len() > 8 {
                                println!("       ... y {} mas", rows.len() - 8);
                            }
                            for row in &rows {
                                if let Some(value) = row.get(1)
                                    && let Ok(parsed) = value.parse::<f64>()
                                {
                                    total += parsed;
                                }
                            }
                            println!("     {} grupos, total {:.0}", rows.len(), total);
                        }
                        Err(error) => println!("     ERROR al ejecutar: {error}"),
                    }
                }
                Err(error) => println!("     ERROR al ejecutar: {error}"),
            }
        }
        Err(error) => println!("     NO baja: {error}"),
    }
}

fn column_name(model: &datan_model::Model, column: ObjectId) -> String {
    match model.column(column) {
        Some((table, column)) => format!("{}[{}]", table.name, column.name),
        None => format!("ObjectId({column:?})"),
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("     {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}