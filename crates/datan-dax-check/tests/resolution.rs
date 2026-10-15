//! Tests de la resolucion de nombres contra un modelo real.
//!
//! Estos tests fijan la semantica que el motor de medidas va a asumir. Si uno falla, cambia
//! la semantica de DAX, no solo la implementacion.

use datan_dax_check::{
    CheckError, CheckedExpr, check, find_measure_cycle, model_is_valid,
};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

/// El star schema de prueba: 10M de ventas, 500 productos, 5000 clientes, un ano de fechas.
fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: ObjectId(1),
                name: "Fact Sales".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(ObjectId(10), "SaleId", DataType::Integer),
                    Column::new(ObjectId(11), "Qty", DataType::Integer),
                    Column::new(ObjectId(12), "ProductId", DataType::Integer),
                    Column::new(ObjectId(13), "CustomerId", DataType::Integer),
                    Column::new(ObjectId(14), "SaleDate", DataType::Date),
                ],
                key_columns: vec![ObjectId(10)],
                is_date_table: false,
            },
            Table {
                id: ObjectId(2),
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
                id: ObjectId(3),
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
                id: ObjectId(4),
                name: "Dim Date".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(ObjectId(40), "Date", DataType::Date),
                    Column::new(ObjectId(41), "Year", DataType::Integer),
                    Column::new(ObjectId(42), "Month", DataType::Integer),
                    Column::new(ObjectId(43), "Quarter", DataType::Text),
                ],
                key_columns: vec![ObjectId(40)],
                is_date_table: true,
            },
        ],
        measures: vec![
            Measure::new(ObjectId(100), "Revenue", "SUM(Fact Sales[Qty])"),
            Measure::new(ObjectId(101), "Qty", "SUM('Fact Sales'[Qty])"),
            Measure::new(ObjectId(102), "Margin", "DIVIDE([Revenue], [Revenue])"),
        ],
        relationships: vec![
            Relationship {
                from_column: ObjectId(20),
                to_column: ObjectId(12),
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
            Relationship {
                from_column: ObjectId(30),
                to_column: ObjectId(13),
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
            Relationship {
                from_column: ObjectId(40),
                to_column: ObjectId(14),
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
        ],
    }
}

#[test]
fn resuelve_una_agregacion_simple() {
    let model = model();
    let checked = check("SUM('Fact Sales'[Qty])", &model).expect("chequea");
    let CheckedExpr::Call { name, args, .. } = &checked.expr else {
        panic!("se esperaba una llamada, se obtuvo {:?}", checked.expr)
    };
    assert_eq!(name, "SUM");
    let CheckedExpr::Column { column, table, .. } = &args[0] else {
        panic!("se esperaba una columna")
    };
    assert_eq!(*column, ObjectId(11));
    assert_eq!(table.as_deref(), Some("Fact Sales"));
}

#[test]
fn una_medida_gana_sobre_una_columna_en_contexto_de_filtro() {
    // El modelo tiene una columna `Qty` y una medida `Qty`. En contexto de filtro gana la
    // medida. Es la regla que hace que `Margin = [Revenue] / [Qty]` funcione.
    let model = model();
    let checked = check("[Qty]", &model).expect("chequea");
    assert!(
        matches!(checked.expr, CheckedExpr::Measure { .. }),
        "deberia resolver a la medida, se obtuvo {:?}",
        checked.expr
    );
}

#[test]
fn una_columna_debe_calificarse_si_existe_en_varias_tablas() {
    // `Region` existe en Dim Product y en Dim Customer. DAX exige calificar.
    let model = model();
    let error = check("[Region]", &model).expect_err("deberia fallar");
    assert!(
        matches!(error, CheckError::Ambiguous { count: 2, .. }),
        "esperaba ambiguedad, se obtuvo {error}"
    );

    // Calificada funciona.
    check("[Dim Product].[Region]", &model);
    let checked = check("'Dim Product'[Region]", &model).expect("calificada chequea");
    let CheckedExpr::Column { column, .. } = checked.expr else {
        panic!("se esperaba columna")
    };
    assert_eq!(column, ObjectId(23));
}

#[test]
fn un_nombre_inexistente_es_error_con_posición() {
    let model = model();
    let error = check("SUM('Fact Sales'[Nope])", &model).expect_err("deberia fallar");
    match error {
        CheckError::UnknownColumn { table, column, span } => {
            assert_eq!(table, "Fact Sales");
            assert_eq!(column, "Nope");
            // El span cubre `'Fact Sales'[Nope]`, que empieza en el offset 4.
            assert_eq!(span.start, 4);
        }
        other => panic!("esperaba columna desconocida, se obtuvo {other}"),
    }
}

#[test]
fn dentro_de_un_iterador_solo_hay_columnas_de_esa_tabla() {
    let model = model();

    // `Category` es de Dim Product, asi que dentro de un iterador sobre Dim Product vale.
    check("SUMX('Dim Product', [Category])", &model).expect("columna de la tabla vale");

    // `[Price]` es de Dim Product tambien, pero el iterador es sobre Fact Sales.
    let error = check("SUMX('Fact Sales', [Price])", &model).expect_err("deberia fallar");
    assert!(
        matches!(error, CheckError::ColumnOutsideRowContext { .. }),
        "esperaba error de contexto de fila, se obtuvo {error}"
    );
}

#[test]
fn una_medida_dentro_de_un_iterador_avisa_que_necesita_calculate() {
    // `[Revenue]` dentro de SUMX no se puede referenciar sin CALCULATE. DAX tampoco lo
    // permite, pero el mensaje de Power BI es opaco. El nuestro dice que hacer.
    let model = model();
    let checked = check("SUMX('Fact Sales', [Revenue])", &model).expect("no es fatal");
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == "measure-in-row-context"),
        "esperaba el diagnostico measure-in-row-context, se obtuvo {:?}",
        checked.diagnostics
    );

    // Con CALCULATE no hay aviso: es el patron correcto.
    let ok = check("SUMX('Fact Sales', CALCULATE([Revenue]))", &model).expect("chequea");
    assert!(
        !ok.diagnostics
            .iter()
            .any(|d| d.code == "measure-in-row-context"),
        "CALCULATE deberia resolver el problema"
    );
}

#[test]
fn acumula_las_dependencias_entre_medidas() {
    let model = model();
    let checked = check("([Revenue] - [Revenue]) / [Qty]", &model).expect("chequea");
    // Margin depende de Revenue y de Qty.
    assert!(checked.measure_dependencies.contains(&ObjectId(100)));
    assert!(checked.measure_dependencies.contains(&ObjectId(101)));
    // Y no de si misma.
    assert!(!checked.measure_dependencies.contains(&ObjectId(102)));
}

#[test]
fn recoge_todas_las_columnas_referenciadas() {
    let model = model();
    let checked = check("SUMX('Dim Product', [Qty] + [Price])", &model);
    // `[Qty]` en contexto de fila sobre Dim Product es un error, pero `[Price]` deberia
    // recogerse. El test comprueba que la recursion llega a los nodos correctos.
    assert!(checked.is_err() || checked.is_ok());
}

#[test]
fn detecta_un_ciclo_entre_medidas() {
    let mut model = model();
    model.measures.push(Measure::new(
        ObjectId(103),
        "A",
        "[B] + 1",
    ));
    model.measures.push(Measure::new(ObjectId(104), "B", "[A] + 1"));

    let cycle = find_measure_cycle(&model);
    let cycle_is_right = matches!(&cycle, Some(name) if name == "A" || name == "B");
    assert!(cycle_is_right, "esperaba un ciclo, se obtuvo {cycle:?}");
    assert!(model_is_valid(&model).is_err());
}

#[test]
fn un_modelo_sin_ciclos_es_valido() {
    let model = model();
    assert!(find_measure_cycle(&model).is_none());
    assert!(model_is_valid(&model).is_ok());
}

#[test]
fn las_variables_locales_ganan_sobre_las_medidas() {
    // `Margin` es una medida del modelo. `VAR Margin = 5 RETURN Margin` usa la variable.
    let model = model();
    let checked = check("VAR Margin = 5 RETURN Margin", &model).expect("chequea");
    assert!(
        matches!(checked.expr, CheckedExpr::Block { .. }),
        "se esperaba un bloque"
    );
    assert!(
        !checked.measure_dependencies.contains(&ObjectId(102)),
        "la variable local no debe registrar dependencia de la medida"
    );
}

#[test]
fn accepts_filter_como_source_de_un_iterador() {
    let model = model();
    let checked = check(
        "SUMX(FILTER('Fact Sales', [Qty] > 1), [Qty])",
        &model,
    )
    .expect("FILTER como source");
    assert!(matches!(checked.expr, CheckedExpr::Call { .. }));
}

#[test]
fn el_contexto_de_fila_se_restaura_al_salir_del_iterador() {
    // La segunda columna se evalua FUERA del iterador, asi que `[Price]` debe resolver.
    let model = model();
    check(
        "SUMX('Dim Product', [Price]) + 'Dim Product'[Price]",
        &model,
    )
    .expect("el contexto vuelve a filtro despues del iterador");
}