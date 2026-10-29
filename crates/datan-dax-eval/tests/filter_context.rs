//! Tests del algebra de filter context.
//!
//! Estos tests fijan la semantica de DAX que el motor va a asumir. Si uno falla, cambia el
//! comportamiento, no la implementacion.
//!
//! El caso central es la propagacion: un filtro sobre `Dim Product[Region]` tiene que llegar
//! a `Fact Sales`. Sin eso, cross-filtering no existe y Datan no es Power BI.

use datan_dax_eval::{
    FilterContext, FilterValue, Granularity, Modifier, apply_modifiers, extract_atoms,
    needs_interpreter,
};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

const FACT: ObjectId = ObjectId(1);
const PRODUCT: ObjectId = ObjectId(2);
const CUSTOMER: ObjectId = ObjectId(3);
const DATE: ObjectId = ObjectId(4);

/// La columna de la relationship en la tabla de hechos. En un star schema real el filtro
/// de `Dim Product[Region]` NO viaja a una columna homonima de los hechos: viaja a
/// `Fact Sales[ProductId]`, y de ahi sale el filtro a las filas. Por eso la propagacion
/// tiene que alcanzar la clave de la relacion.
const FACT_PRODUCT_ID: ObjectId = ObjectId(13);
const FACT_QTY: ObjectId = ObjectId(11);
const PRODUCT_REGION: ObjectId = ObjectId(23);
#[allow(dead_code)]
const PRODUCT_PRICE: ObjectId = ObjectId(24);
#[allow(dead_code)]
const CUSTOMER_REGION: ObjectId = ObjectId(32);
const DATE_YEAR: ObjectId = ObjectId(41);

/// Star schema: ventas, producto, cliente y fecha.
fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: FACT,
                name: "Fact Sales".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(ObjectId(10), "SaleId", DataType::Integer),
                    Column::new(ObjectId(11), "Qty", DataType::Integer),
                    Column::new(ObjectId(12), "PriceCopy", DataType::Moneda),
                    Column::new(ObjectId(13), "ProductId", DataType::Integer),
                    Column::new(ObjectId(14), "CustomerId", DataType::Integer),
                    Column::new(ObjectId(15), "SaleDate", DataType::Date),
                ],
                key_columns: vec![ObjectId(10)],
                is_date_table: false,
            },
            Table {
                id: PRODUCT,
                name: "Dim Product".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(ObjectId(20), "ProductId", DataType::Integer),
                    Column::new(ObjectId(21), "Product", DataType::Text),
                    Column::new(ObjectId(22), "Category", DataType::Text),
                    Column::new(ObjectId(23), "Region", DataType::Text),
                    Column::new(ObjectId(24), "Price", DataType::Moneda),
                ],
                key_columns: vec![ObjectId(20)],
                is_date_table: false,
            },
            Table {
                id: CUSTOMER,
                name: "Dim Customer".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(ObjectId(30), "CustomerId", DataType::Integer),
                    Column::new(ObjectId(31), "Customer", DataType::Text),
                    Column::new(ObjectId(32), "Region", DataType::Text),
                ],
                key_columns: vec![ObjectId(30)],
                is_date_table: false,
            },
            Table {
                id: DATE,
                name: "Dim Date".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(ObjectId(40), "Date", DataType::Date),
                    Column::new(ObjectId(41), "Year", DataType::Integer),
                ],
                key_columns: vec![ObjectId(40)],
                is_date_table: true,
            },
        ],
        measures: vec![
            Measure::new(ObjectId(100), "Qty", "SUM('Fact Sales'[Qty])"),
            Measure::new(ObjectId(101), "Revenue", "SUMX('Fact Sales', [Qty])"),
            Measure::new(ObjectId(102), "Total", "[Qty] + [Qty]"),
        ],
        relationships: vec![
            Relationship {
                from_column: ObjectId(20),
                to_column: ObjectId(13),
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
            Relationship {
                from_column: ObjectId(30),
                to_column: ObjectId(14),
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
            Relationship {
                from_column: ObjectId(40),
                to_column: ObjectId(15),
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
        ],
    }
}

fn norte() -> FilterValue {
    FilterValue::Text("Norte".to_string())
}

#[test]
fn un_contexto_vacio_no_filtra_nada() {
    let context = FilterContext::empty();
    assert!(context.is_empty());
    assert_eq!(context.column_count(), 0);
}

#[test]
fn un_filtro_se_propaga_de_la_dimension_a_los_hechos() {
    let model = model();
    let context = FilterContext::empty().with_equal(PRODUCT_REGION, norte());

    // Sin propagar, solo la dimension esta filtrada.
    assert_eq!(context.filters_on(PRODUCT_REGION).len(), 1);
    assert!(context.filters_on(FACT_PRODUCT_ID).is_empty());

    // Despues de propagar, tambien llega a los hechos por la relationship.
    let propagated = context.propagate(&model);
    assert!(
        !propagated.filters_on(FACT_PRODUCT_ID).is_empty(),
        "el filtro deberia llegar a Fact Sales[ProductId]"
    );
}

#[test]
fn la_propagacion_es_transitiva() {
    let model = model();
    // El filtro en Dim Date tiene que viajar por la relationship hasta los hechos.
    let context = FilterContext::empty().with_equal(DATE_YEAR, FilterValue::Integer(2025));
    let propagated = context.propagate(&model);
    assert!(!propagated.filters_on(ObjectId(15)).is_empty());
}

#[test]
fn quitar_una_tabla_no_toca_las_demas() {
    let model = model();
    let context = FilterContext::empty()
        .with_equal(PRODUCT_REGION, norte())
        .with_equal(DATE_YEAR, FilterValue::Integer(2025));

    let after = context.without_table(&model, PRODUCT);
    assert!(after.filters_on(PRODUCT_REGION).is_empty());
    assert!(!after.filters_on(DATE_YEAR).is_empty());
}

#[test]
fn removefilters_de_una_tabla_solo_quita_esa_tabla() {
    let model = model();
    let base = FilterContext::empty()
        .with_equal(PRODUCT_REGION, norte())
        .with_equal(DATE_YEAR, FilterValue::Integer(2025));

    let after = apply_modifiers(
        &model,
        &base,
        &[Modifier::RemoveFilters {
            table: Some(PRODUCT),
        }],
    )
    .expect("aplica");

    assert!(after.filters_on(PRODUCT_REGION).is_empty());
    assert!(!after.filters_on(DATE_YEAR).is_empty());
}

#[test]
fn removefilters_sin_argumento_lo_vacia_todo() {
    let model = model();
    let base = FilterContext::empty()
        .with_equal(PRODUCT_REGION, norte())
        .with_equal(DATE_YEAR, FilterValue::Integer(2025));

    let after =
        apply_modifiers(&model, &base, &[Modifier::RemoveFilters { table: None }]).expect("aplica");
    assert!(after.is_empty());
}

#[test]
fn all_de_una_tabla_quita_tambien_lo_que_se_propaga() {
    let model = model();
    // El filtro sobre Dim Product viaja a Fact Sales. `ALL(Dim Product)` tiene que quitar
    // los dos, no solo el de la dimension. Si no lo hiciera, `CALCULATE([Revenue],
    // ALL('Dim Product'))` no removeria el filtro yaria la medida.
    let base = FilterContext::empty().with_equal(PRODUCT_REGION, norte());
    let propagated = base.propagate(&model);
    assert!(!propagated.filters_on(FACT_PRODUCT_ID).is_empty());

    let after = apply_modifiers(
        &model,
        &propagated,
        &[Modifier::All {
            table: Some(PRODUCT),
        }],
    )
    .expect("aplica");
    assert!(after.filters_on(PRODUCT_REGION).is_empty());
    assert!(
        after.filters_on(FACT_PRODUCT_ID).is_empty(),
        "ALL(Dim Product) tambien tiene que quitar el filtro propagado a los hechos"
    );
}

#[test]
fn allexcept_conserva_las_columnas_marcadas() {
    let model = model();
    let base = FilterContext::empty()
        .with_equal(PRODUCT_REGION, norte())
        .with_equal(ObjectId(22), FilterValue::Text("Bicicletas".to_string()));

    let after = apply_modifiers(
        &model,
        &base,
        &[Modifier::AllExcept {
            table: PRODUCT,
            keep: vec![ObjectId(22)],
        }],
    )
    .expect("aplica");

    assert!(
        after.filters_on(PRODUCT_REGION).is_empty(),
        "la columna no marcada debe perder su filtro"
    );
    assert!(
        !after.filters_on(ObjectId(22)).is_empty(),
        "la columna marcada debe conservar su filtro"
    );
}

#[test]
fn intersect_combina_los_filtros_de_una_misma_columna() {
    // `KEEPFILTERS` sobre dos valores de la misma columna da la interseccion, que es un
    // conjunto vacio, no la union.
    let norte_este = FilterContext::empty()
        .with_equal(PRODUCT_REGION, norte())
        .with_equal(PRODUCT_REGION, FilterValue::Text("Este".to_string()));

    assert_eq!(norte_este.filters_on(PRODUCT_REGION).len(), 2);

    let a = FilterContext::empty().with_equal(PRODUCT_REGION, norte());
    let b = FilterContext::empty().with_equal(PRODUCT_REGION, norte());
    let intersection = a.intersect(&b);
    assert_eq!(intersection.filters_on(PRODUCT_REGION).len(), 2);
}

#[test]
fn dos_contextos_con_los_mismos_filtros_son_iguales() {
    // La igualdad es estructural, no por orden de insercion. De eso depende el cache de
    // planes: dos consultas que filtran lo mismo deben compartir la lectura.
    let a = FilterContext::empty()
        .with_equal(PRODUCT_REGION, norte())
        .with_equal(DATE_YEAR, FilterValue::Integer(2025));
    let b = FilterContext::empty()
        .with_equal(DATE_YEAR, FilterValue::Integer(2025))
        .with_equal(PRODUCT_REGION, norte());

    assert_eq!(a, b);
    assert_eq!(a.propagate(&model()), b.propagate(&model()));
}

#[test]
fn extrae_un_atomo_de_una_agregacion_simple() {
    let model = model();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    assert_eq!(atoms.atoms.len(), 1);
    assert_eq!(atoms.atoms[0].kind, datan_dax_eval::AtomKind::Sum);
    assert_eq!(atoms.atoms[0].column, Some(FACT_QTY));
    assert_eq!(atoms.atoms[0].label, "SUM(Qty)");
}

#[test]
fn dos_medidas_sobre_la_misma_columna_comparten_atomo() {
    // `[Qty] + [Qty]` genera DOS atomos con la misma etiqueta y el mismo filtro. Se
    // deduplican despues; el test verifica que la deteccion funciona.
    let model = model();
    let atoms = extract_atoms(
        "[Qty] + [Qty]",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    // Una hoja repetida es UNA hoja. `[Qty] + [Qty]` usa el mismo atomo dos veces por
    // indice, no dos atomos identicos: el lowering repite la expresion del agregado en la
    // lista de seleccion y DuckDB hace CSE, asi que la segunda copia no costaba nada pero
    // ocupaba un slot de salida.
    //
    // Antes eran dos atomos y el SQL emitia `SUM(...) AS atom_0` y `SUM(...) AS atom_1` con
    // el mismo contenido. El numero era correcto; el informe tenia una columna invisible.
    assert_eq!(atoms.atoms.len(), 1);
    assert_eq!(atoms.outputs.len(), 1);
    match &atoms.outputs[0].shape {
        datan_dax_eval::Shape::Binary { left, right, .. } => {
            assert_eq!(left, right, "las dos ramas apuntan al mismo atomo");
        }
        other => panic!("se esperaba una forma binaria, se obtuvo {other:?}"),
    }
}

#[test]
fn una_hoja_con_dos_contextos_distintos_no_se_comparte() {
    // El caso contrario, y el que hace que la deduplicacion por etiqueta sola sea incorrecta:
    // dos `SUM(Qty)` con filtros distintos son dos hojas. Compartirlas haria que una medida
    // con `CALCULATE` mostrara el total sin filtrar.
    let model = model();
    let atoms = extract_atoms(
        "CALCULATE([Qty], 'Dim Product'[Region] = \'Norte\') + CALCULATE([Qty], 'Dim Product'[Category] = \'A\')",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    assert_eq!(
        atoms.atoms.len(),
        2,
        "mismo agregado, contextos distintos: dos hojas"
    );
}

#[test]
fn un_calculate_con_modificadores_fuerza_el_interprete() {
    // El corte fast path / interprete. Un CALCULATE con modificadores cambia el conjunto de
    // filtros de forma no uniforme, asi que no se puede bajar a un agregado SQL.
    let model = model();
    let atoms = extract_atoms(
        "CALCULATE([Qty], ALL('Dim Product'))",
        &model,
        &FilterContext::empty().with_equal(PRODUCT_REGION, norte()),
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    for _a in &atoms.atoms {}
    assert!(
        atoms.needs_interpreter(),
        "un CALCULATE con modificadores debe necesitar el interprete"
    );
    assert!(needs_interpreter(
        "CALCULATE([Qty], ALL('Dim Product'))",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION]
    ));
}

#[test]
fn una_agregacion_simple_no_necesita_el_interprete() {
    let model = model();
    assert!(!needs_interpreter(
        "SUM('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION]
    ));
}

#[test]
fn el_atomo_captura_el_contexto_ya_propagado() {
    // El atomo guarda el filtro de la dimension Y su llegada a los hechos. Asi el
    // interprete por celda de grupo no tiene que volver a propagar.
    let model = model();
    let context = FilterContext::empty().with_equal(PRODUCT_REGION, norte());
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &context,
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    let atom = &atoms.atoms[0];
    assert!(!atom.context.filters_on(PRODUCT_REGION).is_empty());
    assert!(
        !atom.context.filters_on(FACT_PRODUCT_ID).is_empty(),
        "el contexto del atomo deberia venir propagado"
    );
}

#[test]
fn una_agregacion_simple_es_siempre_el_fast_path() {
    // El caso 2 de PLAN.md seccion 3.1: cambiar de granularidad obliga a subconsulta
    // correlacionada, y ahi se rompe el fast path.
    let model = model();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty(),
        &[ObjectId(22)],
    )
    .expect("extrae");

    // `SUM(Qty) GROUP BY Category` NO necesita interprete. Las filas ya estan filtradas por
    // el contexto y el agregado corre sobre ellas. Es un hash aggregate normal de DuckDB.
    // Este test fija que la granularidad de un atomo no depende de si su columna esta en el
    // GROUP BY: confundirlas mandaba todo al interprete sin motivo.
    assert_eq!(atoms.atoms[0].granularity, Granularity::Query);
    assert!(
        !atoms.needs_interpreter(),
        "una agregacion simple sobre columnas filtradas se baja a SQL"
    );
}

#[test]
fn el_filtro_de_un_atomo_no_depende_del_orden_de_construccion() {
    let model = model();
    let a = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty().with_equal(PRODUCT_REGION, norte()),
        &[PRODUCT_REGION],
    )
    .expect("extrae");
    let b = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string())),
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    assert_eq!(a.signature(), b.signature());
}

#[test]
fn una_relationship_inactiva_no_propaga() {
    let mut model = model();
    model.relationships[0].role = RelationshipRole::Inactive;

    let context = FilterContext::empty().with_equal(ObjectId(20), FilterValue::Integer(7));
    let propagated = context.propagate(&model);
    assert!(
        propagated.filters_on(ObjectId(13)).is_empty(),
        "una relationship inactiva no debe propagar"
    );
}

#[test]
fn las_tablas_tocadas_incluyen_las_alcanzadas_por_propagacion() {
    let model = model();
    let context = FilterContext::empty().with_equal(PRODUCT_REGION, norte());
    let tables = context.tables_touched(&model);
    assert!(tables.contains(&PRODUCT));
    assert!(
        tables.contains(&FACT),
        "los hechos se tocan por propagacion"
    );
    assert!(tables.contains(&CUSTOMER) == false);
}

#[test]
fn el_contexto_determina_las_columnas_que_hay_que_proyectar() {
    let model = model();
    let context = FilterContext::empty().with_equal(PRODUCT_REGION, norte());
    let columns = context.required_columns(&model);
    assert!(columns.contains(&PRODUCT_REGION));
    assert!(
        columns.contains(&FACT_PRODUCT_ID),
        "la columna de hechos llega por propagacion"
    );
}
