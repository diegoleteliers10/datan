//! Fase 1, segundo paso: el IR genera el mismo SQL que la linea base a mano.
//!
//! El test de semana 1 (`probe_measures`) contesto que un conjunto fijo de medidas cabe en
//! una pasada. Este probe comprueba que el GENERADOR produce ese SQL, comparando con
//! `EXCEPT` contra la consulta escrita a mano.
//!
//! La medida que decide es `Margin = (Revenue - COGS) / Revenue`: es aritmetica ENTRE
//! agregaciones. Un modelo de atomos plano no la puede expresar, porque un atomo es una hoja
//! y `Margin` necesita combinar tres. Este probe mide si el IR llega ahi.
//!
//! Uso: cargo run --release -p datan-app --bin probe_margin

use datan_dax_eval::extract_atoms;
use datan_dax_sql::{SourceMap, lower};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};
use datan_storage::Engine;

const FACT: ObjectId = ObjectId(1);
const PRODUCT: ObjectId = ObjectId(2);

const FACT_QUANTITY: ObjectId = ObjectId(11);
const FACT_PRICE: ObjectId = ObjectId(12);
const FACT_PRODUCT: ObjectId = ObjectId(13);
const PRODUCT_KEY: ObjectId = ObjectId(20);
const PRODUCT_CATEGORY: ObjectId = ObjectId(21);
const PRODUCT_COST: ObjectId = ObjectId(22);

/// Las medidas del plan. `Margin` es la que estira el IR.
const REVENUE: &str = "SUM('fact_line'[quantity] * 'fact_line'[precio_unitario])";
const COGS: &str = "SUM('fact_line'[quantity] * 'dim_product'[costo])";
const QTY: &str = "SUM('fact_line'[quantity])";
const MARGIN: &str = "([Revenue] - [COGS]) / [Revenue]";

fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: FACT,
                name: "fact_line".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(FACT_QUANTITY, "quantity", DataType::Integer),
                    Column::new(FACT_PRICE, "precio_unitario", DataType::Moneda),
                    Column::new(FACT_PRODUCT, "product_key", DataType::Integer),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
            Table {
                id: PRODUCT,
                name: "dim_product".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(PRODUCT_KEY, "product_key", DataType::Integer),
                    Column::new(PRODUCT_CATEGORY, "categoria", DataType::Text),
                    Column::new(PRODUCT_COST, "costo", DataType::Moneda),
                ],
                key_columns: vec![PRODUCT_KEY],
                is_date_table: false,
            },
        ],
        measures: vec![
            Measure::new(ObjectId(100), "Revenue", REVENUE),
            Measure::new(ObjectId(101), "COGS", COGS),
            Measure::new(ObjectId(102), "Qty", QTY),
            Measure::new(ObjectId(103), "Margin", MARGIN),
        ],
        relationships: vec![Relationship {
            from_column: PRODUCT_KEY,
            to_column: FACT_PRODUCT,
            cardinality: Cardinality::OneToMany,
            direction: FilterDirection::Single,
            role: RelationshipRole::Active,
            assume_referential_integrity: false,
        }],
    }
}

fn fixtures() -> Engine {
    let engine = Engine::open(":memory:").expect("motor");
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

/// Una medida y su linea base, con la misma forma.
///
/// El `EXCEPT` exige el mismo numero de columnas en los dos lados, asi que la comparacion
/// es por medida: la generada tiene `categoria` mas el agregado, y la base tiene lo mismo.
/// Comparar el conjunto entero contra una base de tres medidas no compila, y un `EXCEPT`
/// mal armado no dice nada.
struct Leaf {
    name: &'static str,
    dax: &'static str,
    manual: &'static str,
}

const LEAVES: [Leaf; 3] = [
    Leaf {
        name: "Revenue",
        dax: REVENUE,
        manual: "SUM(pb.quantity * pb.precio_unitario)",
    },
    Leaf {
        name: "COGS",
        dax: COGS,
        manual: "SUM(pb.quantity * cp.costo)",
    },
    Leaf {
        name: "Qty",
        dax: QTY,
        manual: "SUM(pb.quantity)",
    },
];

fn baseline_for(manual: &str) -> String {
    format!(
        "SELECT cp.categoria, {manual} AS atom_0 \
         FROM fact_line pb \
         JOIN dim_product cp ON cp.product_key = pb.product_key \
         GROUP BY cp.categoria"
    )
}

fn main() {
    println!("Datan, Fase 1: el IR contra la linea base a mano\n");

    let model = model();
    let engine = fixtures();
    let sources = SourceMap::from_model(&model);
    let context = datan_dax_eval::FilterContext::empty();

    let mut same = 0usize;
    let mut different = 0usize;

    for leaf in &LEAVES {
        println!("--- {} ---", leaf.name);

        let atoms = match extract_atoms(leaf.dax, &model, &context, &[PRODUCT_CATEGORY]) {
            Err(error) => {
                println!("  ERROR al extraer: {error}");
                different += 1;
                continue;
            }
            Ok(set) => set,
        };

        println!("  {} atomo(s)", atoms.atoms.len());
        for atom in &atoms.atoms {
            println!(
                "    {} {:?} columnas={:?}",
                atom.label,
                atom.kind,
                atom.referenced_columns()
            );
        }

        let generated = match lower(&model, &sources, &atoms, &context) {
            Err(error) => {
                println!("  ERROR al bajar: {error}");
                different += 1;
                continue;
            }
            Ok(plan) => plan.sql,
        };
        println!("{}", indent(&generated));

        let baseline = baseline_for(leaf.manual);
        let mut agreed = true;
        for (label, sql) in [
            (
                "solo en la generada",
                format!("SELECT * FROM ({generated}) EXCEPT SELECT * FROM ({baseline})"),
            ),
            (
                "solo en la linea base",
                format!("SELECT * FROM ({baseline}) EXCEPT SELECT * FROM ({generated})"),
            ),
        ] {
            // `EXCEPT` sin diferencias no devuelve filas, y `query_arrow` marca eso como
            // error. Un `EXCEPT` que coincide ES cero filas: tratarlo como fallo
            // invertiria el veredicto del test.
            let batches = match engine.query_arrow(&sql) {
                Err(datan_storage::StorageError::EmptyResult) => continue,
                Err(error) => {
                    println!("  ERROR al comparar: {error}");
                    agreed = false;
                    continue;
                }
                Ok(batches) => batches,
            };
            let count = batches.first().map(|batch| batch.num_rows()).unwrap_or(0);
            if count > 0 {
                agreed = false;
                println!("  {count} fila(s) {label}:");
                if let Some(batch) = batches.first() {
                    for row in 0..count {
                        let cells: Vec<String> = (0..batch.num_columns())
                            .map(|column| cell(batch, column, row))
                            .collect();
                        println!("    {}", cells.join("  |  "));
                    }
                }
            }
        }

        if agreed {
            same += 1;
            println!("  COINCIDE con la linea base, en las dos direcciones");
        } else {
            different += 1;
        }
        println!();
    }

    println!("--- Margin: aritmetica entre medidas ---");
    println!("  definicion: {MARGIN}");
    match extract_atoms(MARGIN, &model, &context, &[PRODUCT_CATEGORY]) {
        Err(error) => println!("  ERROR al extraer: {error}"),
        Ok(set) => {
            println!("  {} atomo(s):", set.atoms.len());
            for atom in &set.atoms {
                println!("    {} {:?}", atom.label, atom.kind);
            }
            match lower(&model, &sources, &set, &context) {
                Err(error) => {
                    println!("  NO baja a SQL: {error}");
                    println!();
                    println!("  Este es el hallazgo. El IR extrae las hojas de `Margin` y con");
                    println!("  ellas se puede calcular cada hoja, pero NO se puede emitir la");
                    println!("  division entre tres agregaciones: un atomo es una hoja, y");
                    println!("  `Margin` es una EXPRESION sobre tres hojas.");
                    println!();
                    println!("  La linea base a mano SI lo resuelve repitiendo la expresion en");
                    println!("  la lista de seleccion, sin subconsulta y sin DELIM_JOIN. El plan");
                    println!("  del test de semana 1 lo confirma: 1 HASH_GROUP_BY, 0 DELIM_JOIN.");
                    println!();
                    println!("  El generador tiene que saber emitir una expresion sobre atomos,");
                    println!("  no solo una lista plana. Ese es el trabajo que queda.");
                }
                Ok(plan) => {
                    println!("  BAJA a SQL:\n{}", indent(&plan.sql));
                    let text = engine.explain(&plan.sql);
                    println!(
                        "  HASH_GROUP_BY: {}, DELIM_JOIN: {}",
                        text.matches("HASH_GROUP_BY").count(),
                        text.matches("DELIM_JOIN").count()
                    );

                    // Y contra la linea base a mano, que es la unica prueba que dice si el
                    // numero es el correcto y no solo si el SQL tiene la forma correcta.
                    let manual = format!(
                        "SELECT cp.categoria,
                            (SUM(pb.quantity * pb.precio_unitario) - SUM(pb.quantity * cp.costo))
                                / NULLIF(SUM(pb.quantity * pb.precio_unitario), 0) AS atom_0
                         FROM fact_line pb
                         JOIN dim_product cp ON cp.product_key = pb.product_key
                         GROUP BY cp.categoria"
                    );
                    let mut agreed = true;
                    for (label, sql) in [
                        (
                            "solo en la generada",
                            format!(
                                "SELECT * FROM ({}) EXCEPT SELECT * FROM ({manual})",
                                plan.sql
                            ),
                        ),
                        (
                            "solo en la linea base",
                            format!(
                                "SELECT * FROM ({manual}) EXCEPT SELECT * FROM ({})",
                                plan.sql
                            ),
                        ),
                    ] {
                        match engine.query_arrow(&sql) {
                            Err(datan_storage::StorageError::EmptyResult) => {}
                            Err(error) => {
                                println!("  ERROR al comparar: {error}");
                                agreed = false;
                            }
                            Ok(batches) => {
                                let count = batches.first().map(|b| b.num_rows()).unwrap_or(0);
                                if count > 0 {
                                    agreed = false;
                                    println!("  {count} fila(s) {label}");
                                }
                            }
                        }
                    }
                    if agreed {
                        println!("  COINCIDE con la linea base a mano, en las dos direcciones");
                    }
                }
            }
        }
    }

    println!("\n{same} medidas coinciden con la linea base, {different} no.");
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn cell(
    batch: &datan_storage::arrow::record_batch::RecordBatch,
    column: usize,
    row: usize,
) -> String {
    use datan_storage::arrow::array::{
        Array, Decimal128Array, Float64Array, Int32Array, Int64Array, StringArray,
    };
    let array = batch.column(column);
    if array.null_count() > 0 && array.is_null(row) {
        return "NULL".to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        return values.value(row).to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        return values.value(row).to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<Int32Array>() {
        return values.value(row).to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        return format!("{:.4}", values.value(row));
    }
    if let Some(values) = array.as_any().downcast_ref::<Decimal128Array>() {
        return values.value(row).to_string();
    }
    format!("{:?}", array.data_type())
}
