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
    let introspection = match introspect(&engine, &datan_connect::Source::new("pg")) {
        Ok(result) => result,
        Err(error) => {
            println!("  FALLO introspeccionar: {error}");
            std::process::exit(1);
        }
    };
    let model = &introspection.model;

    println!("\n  3. El modelo\n");
    for line in &introspection.report {
        println!("{line}");
    }

    // 4. Las relaciones, que es donde se decide si el modelo es utilizable.
    println!("\n  4. Relationships");
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
    println!("\n  5. Medidas");
    let facts: Vec<ObjectId> = model
        .tables
        .iter()
        .filter(|t| matches!(t.kind, TableKind::Facts))
        .flat_map(|t| t.columns.iter().map(|c| c.id))
        .filter(|id| {
            matches!(model.column(*id).map(|(_, c)| c.data_type), Some(DataType::Text))
        })
        .collect();
    let grouping: Vec<ObjectId> = facts.iter().take(1).copied().collect();

    let sources = SourceMap::from_model(model);
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

    // 6. Una consulta de verdad, de punta a punta. Un modelo que se imprime bien y una
    //    consulta que no corre son dos mitades, y aqui solo se prueba una.
    println!("\n  6. Consulta de punta a punta");
    match measure_by(model, "product_boleta quantity") {
        Some(measure) => {
            let atoms =
                extract_atoms(&measure.expression, model, &filters, &grouping).expect("extrae");
            match lower(model, &sources, &atoms, &filters) {
                Ok(plan) => {
                    println!("{}\n", indent(&plan.sql));
                    let sql = format!(
                        "SELECT CAST({} AS VARCHAR) FROM ({})",
                        plan.columns.last().map(|c| c.name.as_str()).unwrap_or("atom_0"),
                        plan.sql
                    );
                    match engine.timed(&sql) {
                        Ok(ms) => {
                            let total = engine.scalar(&sql, 0).expect("consulta");
                            println!("     total = {:?} en {ms} ms", total);
                        }
                        Err(error) => println!("     ERROR al ejecutar: {error}"),
                    }
                }
                Err(error) => println!("     NO baja: {error}"),
            }
        }
        None => println!("     no se encontro la medida de ejemplo"),
    }
}

fn measure_by(model: &datan_model::Model, name: &str) -> Option<datan_model::Measure> {
    model.measures.iter().find(|m| m.name == name).cloned()
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