//! Tests del lowering a SQL.
//!
//! El SQL se compara como texto. Eso es deliberado: si la forma cambia, alguien tiene que
//! mirar el diff y decidir si el cambio es intencional. Es una de las razones por las que el
//! Measure IR es un IR y no "DAX compila a SQL".

use datan_dax_eval::{FilterContext, FilterValue, extract_atoms};
use datan_dax_sql::{SourceMap, lower, quote_identifier};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

const FACT: ObjectId = ObjectId(1);
const PRODUCT: ObjectId = ObjectId(2);
const DATE: ObjectId = ObjectId(4);

const FACT_QTY: ObjectId = ObjectId(11);
const FACT_PRODUCT_ID: ObjectId = ObjectId(13);
const PRODUCT_ID: ObjectId = ObjectId(20);
const PRODUCT_REGION: ObjectId = ObjectId(23);
const PRODUCT_CATEGORY: ObjectId = ObjectId(22);
const DATE_YEAR: ObjectId = ObjectId(41);
const FACT_SALE_DATE: ObjectId = ObjectId(17);

fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: FACT,
                name: "Fact Sales".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(ObjectId(10), "SaleId", DataType::Integer),
                    Column::new(FACT_QTY, "Qty", DataType::Integer),
                    Column::new(FACT_PRODUCT_ID, "ProductId", DataType::Integer),
                    Column::new(FACT_SALE_DATE, "SaleDate", DataType::Date),
                ],
                key_columns: vec![ObjectId(10)],
                is_date_table: false,
            },
            Table {
                id: PRODUCT,
                name: "Dim Product".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(PRODUCT_ID, "ProductId", DataType::Integer),
                    Column::new(PRODUCT_CATEGORY, "Category", DataType::Text),
                    Column::new(PRODUCT_REGION, "Region", DataType::Text),
                ],
                key_columns: vec![PRODUCT_ID],
                is_date_table: false,
            },
            Table {
                id: DATE,
                name: "Dim Date".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(ObjectId(40), "Date", DataType::Date),
                    Column::new(DATE_YEAR, "Year", DataType::Integer),
                ],
                key_columns: vec![ObjectId(40)],
                is_date_table: true,
            },
        ],
        measures: vec![Measure::new(
            ObjectId(100),
            "Qty",
            "SUM('Fact Sales'[Qty])",
        )],
        relationships: vec![
            Relationship {
                from_column: PRODUCT_ID,
                to_column: FACT_PRODUCT_ID,
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
            Relationship {
                from_column: ObjectId(40),
                to_column: FACT_SALE_DATE,
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
        ],
    }
}

fn sources(model: &Model) -> SourceMap {
    SourceMap::from_model(model)
}

#[test]
fn baja_una_agregacion_simple_a_un_solo_select() {
    let model = model();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty(),
        &[FACT_QTY],
    )
    .expect("extrae");

    let plan = lower(
        &model,
        &sources(&model),
        &atoms,
        &FilterContext::empty(),
    )
    .expect("baja");

    assert_eq!(
        plan.sql,
        "SELECT\n  t0.\"Qty\",\n  SUM(t0.\"Qty\") AS \"atom_0\"\nFROM \"Fact Sales\" t0\n\nGROUP BY t0.\"Qty\""
    );
}

#[test]
fn el_filtro_de_la_dimension_se_vuelve_un_in_no_un_join() {
    // El detalle que importa: un JOIN con la dimension duplica filas y rompe
    // `COUNT(DISTINCT)` y los SUM sobre columnas de la dimension. Un IN es semijoin.
    let model = model();
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));

    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &filters,
        &[FACT_QTY],
    )
    .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");

    assert!(
        plan.sql.contains("IN (SELECT"),
        "deberia usar un semijoin:\n{}",
        plan.sql
    );
    assert!(
        !plan.sql.contains("JOIN"),
        "no deberia hacer JOIN:\n{}",
        plan.sql
    );
    assert!(plan.sql.contains("\"ProductId\" IN (SELECT \"ProductId\" FROM \"Dim Product\" WHERE \"Region\" = 'Norte')"));
}

#[test]
fn el_sql_generado_no_tiene_subconsulta_por_celda() {
    // El criterio del dia 2 del spike: un HASH_GROUP_BY y ninguna subconsulta correlacionada
    // por celda de grupo. El `IN` es un semijoin sobre la dimension, no por celda.
    let model = model();
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));

    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &filters,
        &[FACT_QTY, DATE_YEAR],
    )
    .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");

    // Un semijoin por TABLA de dimension, no uno por columna de grupo. Agrupar por dos
    // columnas de tablas distintas no puede duplicar el semijoin de la dimension filtrada.
    assert_eq!(
        plan.sql.matches("IN (SELECT").count(),
        1,
        "un semijoin por tabla de dimension:\n{}",
        plan.sql
    );
    assert!(
        plan.sql.contains("JOIN \"Dim Date\""),
        "la tabla de la columna de agrupamiento tiene que estar en el FROM:\n{}",
        plan.sql
    );
    assert!(
        plan.sql.contains("GROUP BY t0.\"Qty\", t1.\"Year\"")
            || plan.sql.contains("GROUP BY t0.\"Qty\", t2.\"Year\""),
        "el GROUP BY debe listar ambas columnas de grupo:\n{}",
        plan.sql
    );
}

#[test]
fn varios_atomos_salen_en_la_misma_pasada() {
    let model = model();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty]) + COUNT('Fact Sales'[SaleId])",
        &model,
        &FilterContext::empty(),
        &[FACT_QTY],
    )
    .expect("extrae");

    assert_eq!(atoms.atoms.len(), 2);
    let plan = lower(
        &model,
        &sources(&model),
        &atoms,
        &FilterContext::empty(),
    )
    .expect("baja");

    assert_eq!(
        plan.sql.matches("SELECT").count(),
        1,
        "todo en un solo SELECT:\n{}",
        plan.sql
    );
    assert!(plan.sql.contains("SUM(t0.\"Qty\") AS \"atom_0\""));
    assert!(plan.sql.contains("COUNT(t0.\"SaleId\") AS \"atom_1\""));
}

#[test]
fn las_columnas_de_salida_tienen_nombre_estable() {
    // `atom_0`, `atom_1`, por indice. El orden tiene que ser estable para que el diff de
    // dos consultas generadas sea legible.
    let model = model();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty]) + COUNT('Fact Sales'[SaleId])",
        &model,
        &FilterContext::empty(),
        &[FACT_QTY],
    )
    .expect("extrae");

    let plan = lower(
        &model,
        &sources(&model),
        &atoms,
        &FilterContext::empty(),
    )
    .expect("baja");

    assert_eq!(plan.columns[0].name, "Qty");
    assert_eq!(plan.columns[1].name, "atom_0");
    assert_eq!(plan.columns[2].name, "atom_1");
    assert_eq!(plan.columns[1].atom.as_deref(), Some("SUM(Qty)"));
}

#[test]
fn un_calculate_con_modificadores_no_se_baja() {
    // El corte: si un atomo necesita el interprete, el lowering falla en vez de generar SQL
    // que significa otra cosa.
    let model = model();
    let atoms = extract_atoms(
        "CALCULATE([Qty], ALL('Dim Product'))",
        &model,
        &FilterContext::empty(),
        &[FACT_QTY],
    )
    .expect("extrae");

    assert!(atoms.needs_interpreter());
    let error = lower(
        &model,
        &sources(&model),
        &atoms,
        &FilterContext::empty(),
    )
    .expect_err("deberia fallar");
    assert!(matches!(error, datan_dax_sql::LowerError::Unsupported(_)));
}

#[test]
fn un_valor_con_comilla_simple_no_rompe_el_sql() {
    // El filtro viene de datos, no de codigo. Un nombre con apostrofe tiene que quedar
    // escapado.
    let model = model();
    let filters = FilterContext::empty().with_equal(
        PRODUCT_REGION,
        FilterValue::Text("Robert'); DROP TABLE fact; --".to_string()),
    );
    let atoms = extract_atoms("SUM('Fact Sales'[Qty])", &model, &filters, &[FACT_QTY])
        .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");

    assert!(plan.sql.contains("'Robert''); DROP TABLE fact; --'"));
    // El payload contiene la palabra "Robert" dos veces, asi que 3 apariciones es lo
    // correcto. Lo que no debe aparecer es el predicado directo de la dimension ademas
    // del semijoin: eso es un WHERE redundante que el ojo no puede revisar.
    let where_clause = plan.sql.split("WHERE").nth(1).unwrap_or("");
    assert_eq!(
        where_clause.matches("IN (SELECT").count(),
        1,
        "un solo semijoin:\n{}",
        plan.sql
    );
    assert!(
        !where_clause.contains("\"Region\" ="),
        "el filtro de la dimension no deberia repetirse fuera del semijoin:\n{}",
        plan.sql
    );
}

#[test]
fn el_mapeo_fisico_sepuede_sobrescribir() {
    // El modelo guarda `Dim Product`, la base puede llamarlo `dim_product`. El mapeo lo
    // resuelve sin tocar el modelo.
    let model = model();
    let sources = SourceMap::from_model(&model).with_table(PRODUCT, "dim_product");
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let atoms = extract_atoms("SUM('Fact Sales'[Qty])", &model, &filters, &[FACT_QTY])
        .expect("extrae");

    let plan = lower(&model, &sources, &atoms, &filters).expect("baja");
    assert!(plan.sql.contains("FROM \"dim_product\""));
    assert!(!plan.sql.contains("\"Dim Product\""));
}

#[test]
fn una_columna_de_agrupamiento_no_aparece_en_el_where() {
    // Si la columna agrupa y tambien filtra con el mismo valor, el filtro es redundante. Y
    // si el valor cambia, es una contradiccion que no se puede escribir en SQL.
    let model = model();
    let filters = FilterContext::empty()
        .with_equal(FACT_QTY, FilterValue::Integer(5));
    let atoms = extract_atoms("SUM('Fact Sales'[Qty])", &model, &filters, &[FACT_QTY])
        .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    assert!(
        !plan.sql.contains("WHERE"),
        "el filtro sobre la columna de agrupamiento es redundante:\n{}",
        plan.sql
    );
}

#[test]
fn escapa_identificadores() {
    assert_eq!(quote_identifier("Dim Product"), "\"Dim Product\"");
}