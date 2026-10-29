//! Prueba de la semana 1 de la Fase 3, tal y como la pide el plan.
//!
//! ANTES de escribir el optimizador: enumerar la propagacion de un grafo CICLICO y
//! comprobar si el resultado depende del orden. Si depende, el algebra tiene un bug y el
//! optimizador lo va a tapar.
//!
//! El grafo es el clasico: una dimension de producto y una de cliente unidas por un puente,
//! con filtro bidireccional. Con un ciclo, propagar A -> B -> A puede volver a A con mas
//! filtros de los que tenia, y el resultado puede depender de si se empieza por A o por B.
//!
//! La enumeracion es EXHAUSTIVA: todas las permutaciones del orden de las relationships, no
//! una muestra. Con 2 relaciones son 2 permutaciones; con 5 serian 120. Da para responder la
//! pregunta con certeza en vez de con ejemplos.
//!
//! Uso: cargo run --release -p datan-app --bin probe_cycle

use datan_dax_eval::{FilterContext, FilterValue, distinct_propagation_results};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

const PRODUCT: ObjectId = ObjectId(1);
const CUSTOMER: ObjectId = ObjectId(2);
const BRIDGE: ObjectId = ObjectId(3);

const PRODUCT_REGION: ObjectId = ObjectId(11);
const PRODUCT_KEY: ObjectId = ObjectId(12);
const CUSTOMER_TIER: ObjectId = ObjectId(21);
const CUSTOMER_KEY: ObjectId = ObjectId(22);
const BRIDGE_PRODUCT_KEY: ObjectId = ObjectId(31);
const BRIDGE_CUSTOMER_KEY: ObjectId = ObjectId(32);

fn text_column(id: ObjectId, name: &str) -> Column {
    Column::new(id, name, DataType::Text)
}

/// El grafo ciclico.
///
/// Product <-> Bridge <-> Customer, con filtro bidireccional en las dos relaciones del
/// puente. Un filtro en `Product[Region]` puede viajar a `Bridge`, de ahi a `Customer`, y
/// volver a `Product` por la otra relacion.
fn cyclic_model() -> Model {
    Model {
        tables: vec![
            Table {
                id: PRODUCT,
                name: "DimProduct".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    text_column(PRODUCT_REGION, "Region"),
                    text_column(PRODUCT_KEY, "ProductKey"),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
            Table {
                id: CUSTOMER,
                name: "DimCustomer".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    text_column(CUSTOMER_TIER, "Tier"),
                    text_column(CUSTOMER_KEY, "CustomerKey"),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
            Table {
                id: BRIDGE,
                name: "Bridge".to_string(),
                kind: TableKind::Dimension,
                // El puente tiene DOS claves, una por dimension. Sin las dos, la
                // relationship del cliente no tiene por donde volver.
                columns: vec![
                    text_column(BRIDGE_PRODUCT_KEY, "ProductKey"),
                    text_column(BRIDGE_CUSTOMER_KEY, "CustomerKey"),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
        ],
        measures: vec![],
        relationships: vec![
            Relationship {
                from_column: PRODUCT_KEY,
                to_column: BRIDGE_PRODUCT_KEY,
                cardinality: Cardinality::ManyToMany,
                direction: FilterDirection::Both,
                role: RelationshipRole::Active,
                assume_referential_integrity: true,
            },
            Relationship {
                from_column: CUSTOMER_KEY,
                to_column: BRIDGE_CUSTOMER_KEY,
                cardinality: Cardinality::ManyToMany,
                direction: FilterDirection::Both,
                role: RelationshipRole::Active,
                assume_referential_integrity: true,
            },
        ],
    }
}

fn main() {
    println!("Datan, Fase 3, prueba de semana 1: un grafo ciclico es ambiguo?\n");

    let model = cyclic_model();
    println!("Grafo:");
    println!("  DimProduct[ProductKey] <-Both- Bridge[ProductKey]");
    println!("  DimCustomer[CustomerKey] <-Both- Bridge[CustomerKey]");
    println!(
        "  El filtro puede dar la vuelta completa: Product -> Bridge -> Customer -> Bridge -> Product"
    );
    println!(
        "  {} relationships, filtro bidireccional en las dos",
        model.relationships.len()
    );

    let start =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));

    println!("\nFiltro inicial: DimProduct[Region] = 'Norte'");
    println!(
        "  columnas con filtro: {:?}",
        start.filtered_columns().collect::<Vec<_>>()
    );

    // 1. Enumeracion exhaustiva de ordenes.
    let results = distinct_propagation_results(&model, &start);
    println!("\n--- Enumeracion exhaustiva de ordenes ---");
    println!(
        "  {} resultados distintos entre todas las permutaciones",
        results.len()
    );

    for (order, context) in &results {
        println!(
            "  orden {:?} -> columnas filtradas {:?}",
            order,
            context.filtered_columns().collect::<Vec<_>>()
        );
    }
    if let Some((_, context)) = results.first() {
        println!("\n  detalle de filtros por columna:");
        for (col, fs) in context.entries() {
            println!(
                "    {col:?}: {} filtro(s), propagated={}",
                fs.len(),
                fs.iter().filter(|f| f.is_propagated()).count()
            );
        }
    }

    // 2. El veredicto.
    println!();
    if results.len() == 1 {
        println!("  CONVERGE. Todas las permutaciones dan lo mismo.");
        println!("  El punto fijo es unico y el orden de propagacion es irrelevante.");
        println!("  Eso NO significa que el ciclo se pueda expresar en SQL: significa que");
        println!("  el algebra de filtros si lo define, y el fast path sigue sin poder.");
    } else {
        println!("  DIVERGE. La propagacion depende del orden.");
        println!("  No hay un numero correcto: hay tantos como ordenes de ejecucion.");
        println!("  Un motor que elige uno en silencio cambia de resultado segun el camino.");
    }

    // 3. El caso que de verdad importa: el ciclo con filtro en el otro extremo.
    println!("\n--- El ciclo, empezando por el otro extremo ---");
    let mut model2 = cyclic_model();
    // Se invierte el grafo: el filtro entra por DimCustomer.
    model2.relationships.swap(0, 1);

    let from_customer =
        FilterContext::empty().with_equal(CUSTOMER_TIER, FilterValue::Text("Oro".to_string()));
    let results2 = distinct_propagation_results(&model2, &from_customer);
    println!("  filtro inicial: DimCustomer[Tier] = 'Oro'");
    println!(
        "  {} resultados distintos entre todas las permutaciones",
        results2.len()
    );
    for (order, context) in &results2 {
        println!(
            "  orden {:?} -> columnas filtradas {:?}",
            order,
            context.filtered_columns().collect::<Vec<_>>()
        );
    }

    println!("\n--- Que significa esto para Datan ---");
    println!("  Si CONVERGE: el interprete puede evaluar el ciclo, y hay que decidir si el");
    println!("  fast path lo rechaza con un mensaje claro en vez de fallar en silencio.");
    println!("  Si DIVERGE: hay que DETECTAR el ciclo al cargar el modelo y negarse a");
    println!("  evaluarlo, diciendo que hace falta una relationship unidireccional.");
}
