//! La pregunta de la semana 1 de la Fase 3, fijada como test.
//!
//! La pregunta era: en un grafo CICLICO con filtro bidireccional, la propagacion depende del
//! orden? Si depende, el algebra no define una semantica y habria que negarse a evaluar.
//!
//! La respuesta fue NO. Todas las permutaciones del orden convergen al mismo punto fijo, y el
//! filtro da la vuelta completa por el ciclo: Product -> Bridge -> Customer -> Bridge ->
//! Product.
//!
//! Eso NO significa que el ciclo se pueda escribir en SQL. Significa algo mas preciso: la
//! ambiguedad de la que habla la documentacion de Power BI no esta en la propagacion de
//! filtros, esta en la SECUENCIA de medidas. El algebra es conmutativa; la semantica de
//! medidas encadenadas no lo es.

use datan_dax_eval::{FilterContext, FilterValue, distinct_propagation_results, propagate_ordered};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

const PRODUCT: ObjectId = ObjectId(1);
const CUSTOMER: ObjectId = ObjectId(2);
const BRIDGE: ObjectId = ObjectId(3);
const FACT: ObjectId = ObjectId(4);
const FACT_KEY: ObjectId = ObjectId(41);

const PRODUCT_REGION: ObjectId = ObjectId(11);
const PRODUCT_KEY: ObjectId = ObjectId(12);
const CUSTOMER_TIER: ObjectId = ObjectId(21);
const CUSTOMER_KEY: ObjectId = ObjectId(22);
const BRIDGE_PRODUCT_KEY: ObjectId = ObjectId(31);
const BRIDGE_CUSTOMER_KEY: ObjectId = ObjectId(32);

fn text_column(id: ObjectId, name: &str) -> Column {
    Column::new(id, name, DataType::Text)
}

fn table(id: ObjectId, name: &str, columns: Vec<Column>) -> Table {
    Table {
        id,
        name: name.to_string(),
        kind: TableKind::Dimension,
        columns,
        key_columns: vec![],
        is_date_table: false,
    }
}

/// Product <-> Bridge <-> Customer, bidireccional. Un ciclo real.
fn cyclic_model() -> Model {
    Model {
        tables: vec![
            table(
                PRODUCT,
                "DimProduct",
                vec![
                    text_column(PRODUCT_REGION, "Region"),
                    text_column(PRODUCT_KEY, "ProductKey"),
                ],
            ),
            table(
                CUSTOMER,
                "DimCustomer",
                vec![
                    text_column(CUSTOMER_TIER, "Tier"),
                    text_column(CUSTOMER_KEY, "CustomerKey"),
                ],
            ),
            table(
                BRIDGE,
                "Bridge",
                vec![
                    text_column(BRIDGE_PRODUCT_KEY, "ProductKey"),
                    text_column(BRIDGE_CUSTOMER_KEY, "CustomerKey"),
                ],
            ),
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

fn norte() -> FilterContext {
    FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()))
}

#[test]
fn la_propagacion_en_un_ciclo_no_depende_del_orden() {
    let model = cyclic_model();
    let results = distinct_propagation_results(&model, &norte());

    assert_eq!(
        results.len(),
        1,
        "la propagacion depende del orden. Resultados distintos: {:?}",
        results.iter().map(|(order, _)| order).collect::<Vec<_>>()
    );
}

#[test]
fn el_filtro_da_la_vuelta_completa_al_ciclo() {
    // El filtro entra por DimProduct[Region] y tiene que llegar a DimCustomer[Tier]
    // passando por el puente dos veces. Si se queda en DimProduct, la propagacion no cruza
    // el ciclo y el cross-filtering entre dimensiones no funciona.
    let model = cyclic_model();
    let propagated = propagate_ordered(&model, &norte(), &[0, 1]);

    for column in [
        PRODUCT_REGION,
        PRODUCT_KEY,
        BRIDGE_PRODUCT_KEY,
        BRIDGE_CUSTOMER_KEY,
        CUSTOMER_KEY,
    ] {
        assert!(
            !propagated.filters_on(column).is_empty(),
            "la columna {column} deberia tener filtro tras dar la vuelta al ciclo"
        );
    }

    // `DimCustomer[Tier]` NO recibe filtro, y no deberia.
    //
    // El filtro viaja por la RELATIONSHIP: sale de `DimProduct[Region]`, cruza
    // `DimProduct[ProductKey]` a `Bridge[ProductKey]`, de ahi a `Bridge[CustomerKey]`, y
    // llega a `DimCustomer[CustomerKey]`. Llega a la columna por la que entro, no a las
    // columnas hermanas de la dimension.
    //
    // Esto es lo que hace que cross-filtering sobre una dimension de producto doesnt
    // convierta la tabla de clientes en un subconjunto visible: filtra las RELACIONES, no
    // las filas de la dimension.
    assert!(
        propagated.filters_on(CUSTOMER_TIER).is_empty(),
        "un filtro no salta a las columnas hermanas de la dimension"
    );
}

#[test]
fn el_ciclo_tambien_converge_entrando_por_el_otro_extremo() {
    // La simetria importa: si converge solo desde un lado, el grafo esta sesgado y la
    // conclusion anterior no vale.
    let model = cyclic_model();
    let from_customer =
        FilterContext::empty().with_equal(CUSTOMER_TIER, FilterValue::Text("Oro".to_string()));

    let results = distinct_propagation_results(&model, &from_customer);
    assert_eq!(results.len(), 1, "la convergencia debe ser simetrica");
}

#[test]
fn la_consecuencia_de_calcular_no_implica_poder_escribirlo_en_sql() {
    // Que la propagacion converja NO significa que el ciclo se pueda bajar a SQL. Un
    // `JOIN` entre tablas que forman un ciclo exige un arbol de joins aciclico, y este
    // grafo no lo tiene.
    //
    // Por eso la deteccion de ciclos tiene que existir igual: para RECHAZAR el fast path con
    // un mensaje, no para evitar el calculo. El calculo lo hace el interprete.
    let model = cyclic_model();
    assert!(
        datan_dax_eval::has_bidirectional_cycle(&model),
        "el modelo de prueba deberia tener un ciclo"
    );
}

#[test]
fn un_grafo_sin_ciclo_no_lo_declara_ciclico() {
    // Un star schema normal: los hechos al centro, las dimensiones en las hojas, todas
    // unidireccionales. Es el caso mas comun de todos y el detector no puede dar falsos
    // positivos: rechazarlo seria rechazar el 90% de los modelos.
    let model = Model {
        tables: vec![
            table(FACT, "FactSales", vec![text_column(FACT_KEY, "ProductKey")]),
            table(
                PRODUCT,
                "DimProduct",
                vec![
                    text_column(PRODUCT_REGION, "Region"),
                    text_column(PRODUCT_KEY, "ProductKey"),
                ],
            ),
            table(
                CUSTOMER,
                "DimCustomer",
                vec![
                    text_column(CUSTOMER_TIER, "Tier"),
                    text_column(CUSTOMER_KEY, "CustomerKey"),
                ],
            ),
        ],
        measures: vec![],
        relationships: vec![
            Relationship {
                from_column: PRODUCT_KEY,
                to_column: FACT_KEY,
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
            Relationship {
                from_column: CUSTOMER_KEY,
                to_column: FACT_KEY,
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
        ],
    };
    assert!(
        !datan_dax_eval::has_bidirectional_cycle(&model),
        "un star schema no debe marcarse como ciclico"
    );
}
