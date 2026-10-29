//! Tests de los modificadores de `CALCULATE`.
//!
//! El caso central: `CALCULATE([Revenue], ALL('Dim Product'))` tiene que devolver el total
//! sin el filtro de la dimension. Antes el atomo conservaba el contexto original y el
//! filtro se aplicaba igual, asi que los dos caminos divergian sin que nada lo dijera.

use datan_dax_eval::{
    FilterContext, FilterValue, IntColumn, Interpreter, MemorySource, Scalar, TextColumn,
    extract_atoms,
};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

const FACT: ObjectId = ObjectId(1);
const PRODUCT: ObjectId = ObjectId(2);

const FACT_QTY: ObjectId = ObjectId(11);
const FACT_PRODUCT_ID: ObjectId = ObjectId(12);
const PRODUCT_ID: ObjectId = ObjectId(20);
const PRODUCT_REGION: ObjectId = ObjectId(21);
const PRODUCT_CATEGORY: ObjectId = ObjectId(22);

fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: FACT,
                name: "fact_sales".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(FACT_QTY, "qty", DataType::Integer),
                    Column::new(FACT_PRODUCT_ID, "product_id", DataType::Integer),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
            Table {
                id: PRODUCT,
                name: "dim_product".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(PRODUCT_ID, "product_id", DataType::Integer),
                    Column::new(PRODUCT_REGION, "region", DataType::Text),
                    Column::new(PRODUCT_CATEGORY, "category", DataType::Text),
                ],
                key_columns: vec![PRODUCT_ID],
                is_date_table: false,
            },
        ],
        // `Revenue` depende de `Qty`, para que `CALCULATE([Revenue], ...)` tenga que
        // atravesar la referencia a otra medida.
        measures: vec![
            Measure::new(ObjectId(100), "Qty", "SUM('fact_sales'[qty])"),
            Measure::new(ObjectId(101), "Revenue", "[Qty]"),
        ],
        relationships: vec![Relationship {
            from_column: PRODUCT_ID,
            to_column: FACT_PRODUCT_ID,
            cardinality: Cardinality::OneToMany,
            direction: FilterDirection::Single,
            role: RelationshipRole::Active,
            assume_referential_integrity: false,
        }],
    }
}

/// Productos 1 y 3 son Norte, 2 y 5 son Sur, 4 es Este.
///
/// Valores de qty: producto 1 -> 10, 2 -> 20, 3 -> 30, 4 -> 40, 5 -> 50, 6 -> 60.
/// Total 210. Norte 40, Sur 70, Este 40.
fn source() -> MemorySource {
    MemorySource::new()
        .with_column(
            FACT_QTY,
            IntColumn {
                values: vec![Some(10), Some(20), Some(30), Some(40), Some(50), Some(60)],
            },
        )
        .with_column(
            FACT_PRODUCT_ID,
            IntColumn {
                values: vec![Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)],
            },
        )
        .with_column(
            PRODUCT_ID,
            IntColumn {
                values: vec![Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)],
            },
        )
        .with_column(
            PRODUCT_REGION,
            TextColumn {
                values: vec![
                    Some("Norte".to_string()),
                    Some("Sur".to_string()),
                    Some("Norte".to_string()),
                    Some("Este".to_string()),
                    Some("Sur".to_string()),
                    Some("Este".to_string()),
                ],
            },
        )
        .with_column(
            PRODUCT_CATEGORY,
            TextColumn {
                values: vec![
                    Some("A".to_string()),
                    Some("A".to_string()),
                    Some("B".to_string()),
                    Some("B".to_string()),
                    Some("A".to_string()),
                    Some("B".to_string()),
                ],
            },
        )
        .with_table_length(FACT, 6)
        .with_table_length(PRODUCT, 6)
}

fn run(expression: &str, filters: &FilterContext) -> (Vec<(String, Scalar)>, bool) {
    let model = model();
    let grouping = [PRODUCT_REGION];
    let atoms = extract_atoms(expression, &model, filters, &grouping).expect("extrae");
    let needs_interpreter = atoms.needs_interpreter();
    let source = source();
    let interpreter = Interpreter::new(&model, &source);
    let result = interpreter
        .evaluate_cell(&atoms.atoms, filters)
        .expect("interpreta");
    let mut values: Vec<(String, Scalar)> = atoms
        .atoms
        .iter()
        .zip(&result.values)
        .map(|(atom, value)| (atom.label.clone(), value.clone()))
        .collect();
    values.sort_by(|a, b| a.0.cmp(&b.0));
    (values, needs_interpreter)
}

#[test]
fn sin_modificadores_el_filtro_sigue_aplicando() {
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let (values, needs_interpreter) = run("[Qty]", &filters);
    // Productos 1 y 3: 10 + 30 = 40.
    assert_eq!(values[0].1, Scalar::Integer(40));
    let _ = needs_interpreter;
}

#[test]
fn all_de_la_dimension_quita_el_filtro() {
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let (values, needs_interpreter) = run("CALCULATE([Qty], ALL('dim_product'))", &filters);

    assert!(
        needs_interpreter,
        "un CALCULATE con modificadores cae al interprete"
    );
    // El filtro de la dimension desaparece: 210 es el total.
    assert_eq!(
        values[0].1,
        Scalar::Integer(210),
        "ALL de la dimension tiene que quitar el filtro"
    );
}

#[test]
fn all_sin_argumento_quita_todos_los_filtros() {
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let (values, _) = run("CALCULATE([Qty], ALL())", &filters);
    assert_eq!(values[0].1, Scalar::Integer(210));
}

#[test]
fn all_de_la_dimension_quita_el_valor_de_grupo() {
    // Agrupado por region, `CALCULATE([Qty], ALL('dim_product'))` vale el TOTAL en cada
    // fila. El valor de grupo solo vive en el contexto de CELDA, y un atomo con
    // modificadores parte de la celda y aplica sus modificadores ENCIMA. Uniendo los dos
    // contextos, la union le devolvia al `ALL` el filtro que acaba de quitar: la fila de
    // Norte mostraba 40 en vez de 210.
    let model = model();
    let atoms = extract_atoms(
        "CALCULATE([Qty], ALL('dim_product'))",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    )
    .expect("extrae");
    assert!(!atoms.outputs.is_empty(), "un CALCULATE tiene forma");

    let src = source();
    let interpreter = Interpreter::new(&model, &src);
    let grouped = interpreter
        .evaluate_grouped(
            &atoms.atoms,
            &atoms.outputs,
            &FilterContext::empty(),
            &[PRODUCT_REGION],
            100,
        )
        .expect("agrupa");

    assert_eq!(grouped.rows.len(), 3, "Este, Norte y Sur");
    for row in &grouped.rows {
        assert_eq!(
            row[1],
            Scalar::Integer(210),
            "el ALL quita tambien el valor de grupo de la fila"
        );
    }
}

#[test]
fn removefilters_quita_la_tabla_pero_no_lo_que_ya_llego_a_los_hechos() {
    // La diferencia real entre `ALL` y `REMOVEFILTERS`, y es una distincion importante:
    //
    // - `ALL(Tabla)` quita los filtros de la tabla Y los que se propagaron a otras.
    //   Por eso `CALCULATE([Qty], ALL('dim_product'))` da 210.
    // - `REMOVEFILTERS(Tabla)` quita SOLO los de esa tabla. El filtro que ya llego a
    //   `fact_sales` por la propagacion se queda.
    //
    // Con un filtro puesto en la dimension, `REMOVEFILTERS` no lo quita: el semijoin ya
    // esta escrito sobre `fact_sales`. Este test fija esa diferencia para que nadie la
    // "arregle" al revés.
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));

    let (all, _) = run("CALCULATE([Qty], ALL('dim_product'))", &filters);
    assert_eq!(
        all[0].1,
        Scalar::Integer(210),
        "ALL quita tambien lo propagado"
    );

    let (remove, _) = run("CALCULATE([Qty], REMOVEFILTERS('dim_product'))", &filters);
    assert_eq!(
        remove[0].1,
        Scalar::Integer(40),
        "REMOVEFILTERS solo quita los de la tabla: el semijoin ya llego a los hechos"
    );
}

#[test]
fn un_filtro_directo_dentro_de_calculate_aprieta() {
    // Un booleano como modificador anade un filtro. `Norte` Y `Norte` es lo mismo.
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let (values, _) = run(
        "CALCULATE([Qty], 'dim_product'[region] = \"Norte\")",
        &filters,
    );
    assert_eq!(values[0].1, Scalar::Integer(40));
}

#[test]
fn un_filtro_contradictorio_da_blank() {
    // `Norte` en el contexto y `Sur` como modificador. Contradiccion: BLANK.
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let (values, _) = run(
        "CALCULATE([Qty], 'dim_product'[region] = \"Sur\")",
        &filters,
    );
    assert_eq!(
        values[0].1,
        Scalar::Blank,
        "un filtro contradictorio no puede dar un numero"
    );
}

#[test]
fn allexcept_conserva_las_columnas_marcadas() {
    // Con filtro en region y category, `ALLEXCEPT(Dim Product, [Region])` quita el de
    // category y deja el de region.
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()))
        .with_equal(PRODUCT_CATEGORY, FilterValue::Text("A".to_string()));

    let (values, _) = run(
        "CALCULATE([Qty], ALLEXCEPT('dim_product', 'dim_product'[region]))",
        &filters,
    );
    // Solo el filtro de region queda: Norte son los productos 1 (10) y 3 (30) = 40.
    // El de category (A) se quita.
    assert_eq!(
        values[0].1,
        Scalar::Integer(40),
        "ALLEXCEPT tiene que quitar el filtro de category y dejar el de region"
    );
}

#[test]
fn dos_modificadores_se_aplican_en_orden() {
    // `ALL(Dim Product)` quita el filtro de region, y luego el booleano pone `Sur`. El
    // orden importa: al reves, el `ALL` se llevaria por delante el filtro nuevo.
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let (values, _) = run(
        "CALCULATE([Qty], ALL('dim_product'), 'dim_product'[region] = \"Sur\")",
        &filters,
    );
    assert_eq!(values[0].1, Scalar::Integer(70), "Sur son 20 + 50");
}

#[test]
fn calculate_atravesa_una_referencia_a_otra_medida() {
    // `Revenue` es `[Qty]`. `CALCULATE([Revenue], ALL(...))` tiene que llegar a la suma
    // que hay dentro de `Qty`, no quedarse en la referencia.
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let (values, _) = run("CALCULATE([Revenue], ALL('dim_product'))", &filters);
    assert_eq!(values[0].1, Scalar::Integer(210));
}

#[test]
fn un_modificador_desconocido_no_se_pierde_en_silencio() {
    // Si un modificador no se sabe interpretar, el atomo cae al interprete y el error se
    // reporta. Perder el filtro en silencio daria numeros incorrectos sin avisar.
    let model = model();
    let filters =
        FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let result = extract_atoms(
        "CALCULATE([Qty], UNA_FUNCION_QUE_NO_EXISTE('dim_product'))",
        &model,
        &filters,
        &[PRODUCT_REGION],
    );
    assert!(
        result.is_err(),
        "un modificador desconocido tiene que ser un error explicito, no un filtro perdido"
    );
}
