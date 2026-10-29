//! Las medidas de negocio sobre TU esquema real (boleta / product_boleta / catalogo_productos).
use datan_connect::{PostgresCatalog, Source, introspect};
use datan_dax_eval::{FilterContext, extract_atoms};
use datan_dax_sql::{SourceMap, lower};
use datan_model::{DataType, TableKind};

fn main() {
    let engine = datan_storage::Engine::open(":memory:").expect("motor");
    engine
        .execute(&format!(
            "ATTACH 'host=localhost dbname=postgres' AS pg (TYPE POSTGRES, READ_ONLY)"
        ))
        .expect("conecta");

    let mut catalog = PostgresCatalog::connect("host=localhost dbname=postgres").expect("catalogo");
    let introspection = introspect(
        &engine,
        &Source::new("pg"),
        Some(&mut catalog),
        Some("product_boleta"),
    )
    .expect("modelo");
    let model = introspection.model;

    println!("Tu esquema retail:\n");
    let fact = model.fact_table().expect("hay hechos");
    let fact_table = model.table(fact).expect("tabla");
    println!("  hechos: {}", fact_table.name);
    for relationship in &model.relationships {
        let from = model
            .column(relationship.from_column)
            .map(|(t, c)| format!("{}[{}]", t.name, c.name));
        let to = model
            .column(relationship.to_column)
            .map(|(t, c)| format!("{}[{}]", t.name, c.name));
        if let (Some(from), Some(to)) = (from, to) {
            println!("  relacion: {from} <- {to}");
        }
    }

    println!("\nMedidas generadas:");
    for measure in &model.measures {
        println!("  {} = {}", measure.name, measure.expression);
    }

    // Y las cuatro, ejecutadas de verdad. Con 0 boletas el resultado es BLANK, que es lo
    // correcto: un informe sobre cero ventas no da cero, da nada.
    println!("\nEjecutadas (agrupadas por producto):");
    let sources = SourceMap::from_model(&model).with_qualifier("pg");
    let filters = FilterContext::empty();
    let grouping: Vec<datan_model::ObjectId> = model
        .tables
        .iter()
        .filter(|t| matches!(t.kind, TableKind::Dimension) && model.join_path(fact, t.id).is_some())
        .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
        .filter(|(t, c)| matches!(c.data_type, DataType::Text) && !t.key_columns.contains(&c.id))
        .map(|(_, c)| c.id)
        .take(1)
        .collect();

    for measure in model
        .measures
        .iter()
        .filter(|m| matches!(m.name.as_str(), "Unidades" | "Ventas" | "Costo" | "Margen"))
    {
        let atoms =
            extract_atoms(&measure.expression, &model, &filters, &grouping).expect("extrae");
        match lower(&model, &sources, &atoms, &filters) {
            Ok(plan) => match engine.render_rows(&plan.sql) {
                Ok(rows) => println!(
                    "  {:10} {} fila(s): {:?}",
                    measure.name,
                    rows.len(),
                    rows.first()
                ),
                Err(e) => println!(
                    "  {:10} ERROR al ejecutar: {}",
                    measure.name,
                    e.to_string().lines().next().unwrap_or("")
                ),
            },
            Err(e) => println!("  {:10} NO baja: {}", measure.name, e),
        }
    }
}
