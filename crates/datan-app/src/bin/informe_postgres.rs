//! Un informe de punta a punta contra Postgres.
//!
//! Conectar, construir el modelo, elegir dimensiones y medidas, ejecutar, y pinchar una
//! celda para ver como cambia el informe entero. Sin GPU: es la logica del producto, y tiene
//! que poder verificarse sin pantalla.
//!
//! Uso: cargo run --release -p datan-app --bin informe_postgres -- [base]

use std::path::Path;

use datan_connect::{PostgresCatalog, Source, introspect};
use datan_model::{DataType, ObjectId, TableKind};
use datan_report::Report;
use datan_storage::Engine;

fn main() {
    let database = std::env::args().nth(1).unwrap_or_else(|| "datan_bench".into());
    let home = std::env::var("HOME").expect("HOME");
    let root = Path::new(&home).join(".duckdb").join("extensions");

    let engine = Engine::open_with_bundled_extensions(":memory:", &root).expect("motor");
    engine
        .load_bundled_extension("postgres")
        .expect("extension de Postgres");
    engine
        .execute(&format!(
            "ATTACH 'host=localhost dbname={database}' AS pg (TYPE POSTGRES, READ_ONLY)"
        ))
        .expect("conecta");
    println!("Datan: informe contra `{database}`\n");

    let mut catalog = PostgresCatalog::connect(&format!("host=localhost dbname={database}"))
        .expect("catalogo");
    let introspection =
        introspect(&engine, &Source::new("pg"), Some(&mut catalog), Some("product_boleta"))
            .expect("modelo");
    let model = introspection.model;

    // Las dimensiones son las columnas de texto de las dimensiones que no son clave. Las
    // medidas, las de suma sobre los hechos.
    let dimensions: Vec<ObjectId> = model
        .tables
        .iter()
        .filter(|t| matches!(t.kind, TableKind::Dimension))
        .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
        .filter(|(t, c)| matches!(c.data_type, DataType::Text) && !t.key_columns.contains(&c.id))
        .map(|(_, c)| c.id)
        .collect();
    let measures: Vec<ObjectId> = model
        .measures
        .iter()
        .filter(|m| m.expression.starts_with("SUM"))
        .map(|m| m.id)
        .collect();

    println!("Dimensiones:");
    for dimension in &dimensions {
        if let Some((table, column)) = model.column(*dimension) {
            println!("  {}[{}]", table.name, column.name);
        }
    }
    println!("Medidas:");
    for measure in &measures {
        if let Some(m) = model.measures.iter().find(|m| m.id == *measure) {
            println!("  {} = {}", m.name, m.expression);
        }
    }

    let report = Report::new(model.clone())
        .attached_to("pg")
        .with_dimensions(dimensions.clone())
        .with_measures(measures.clone());

    println!("\nConsultas necesarias: {}", report.query_count().unwrap_or(0));

    // 1. El informe entero.
    match report.execute(&engine, None) {
        Ok(matrix) => {
            println!("\n--- informe completo ---");
            matrix.print(12);

            // Cross-filter: se pincha la celda de la primera dimension de la primera fila.
            if let Some(drilled) = matrix.drill(&report, 0, 0) {
                let name = model
                    .column(dimensions[0])
                    .map(|(_, c)| c.name.clone())
                    .unwrap_or_default();
                let value = matrix.cell(0, 0).unwrap_or_default().to_string();
                let filtered = drilled.execute(&engine, None).expect("filtra");
                println!("\n--- pinchando {} = {value} ---", name);
                filtered.print(12);
            }
        }
        Err(error) => println!("\nERROR: {error}"),
    }
}