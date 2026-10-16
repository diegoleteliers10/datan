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
        "SELECT\n  t0.\"Qty\",\n  SUM(t0.\"Qty\") AS \"atom_0\"\nFROM \"Fact Sales\" t0\n\nGROUP BY t0.\"Qty\"\nORDER BY t0.\"Qty\""
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

    assert_eq!(atoms.atoms.len(), 2, "dos hojas");
    assert_eq!(atoms.outputs.len(), 1, "pero UNA columna: la suma es una medida");
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
    // Una columna, no dos. `SUM(a) + COUNT(b)` es una medida, y una medida es una celda.
    // Antes eran `atom_0` y `atom_1` y el informe tenia una columna invisible en medio.
    assert!(
        plan.sql.contains("(SUM(t0.\"Qty\") + COUNT(t0.\"SaleId\")) AS \"atom_0\""),
        "las dos hojas van en una sola columna:\n{}",
        plan.sql
    );
    assert!(
        !plan.sql.contains("atom_1"),
        "no debe haber una segunda columna de salida:\n{}",
        plan.sql
    );
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

    assert_eq!(plan.columns.len(), 2, "la agrupacion y la medida");
    assert_eq!(plan.columns[0].name, "Qty");
    assert_eq!(plan.columns[1].name, "atom_0");
    assert_eq!(
        plan.columns[1].measure.as_deref(),
        Some("(SUM(Qty) + COUNT(SaleId))"),
        "la salida guarda que MEDIDA es, no solo el slot"
    );
    // El slot se llama `atom_N` por indice porque el orden tiene que ser estable entre
    // consultas. La medida es lo que el grid pinta en la cabecera.
    assert_eq!(
        plan.columns[1].atom.as_deref(),
        Some("(SUM(Qty) + COUNT(SaleId))"),
        "la etiqueta de la salida es la medida, no la hoja"
    );
}

#[test]
fn una_medida_sin_forma_sigue_emitiendo_una_columna_por_atomo() {
    // La ruta plana no se tira. Una medida con `CALCULATE` no se puede expresar como
    // combinacion aritmetica de hojas, y el lowering tiene que negarse en vez de emitir
    // algo equivocado; este test fija que el camino de una columna por atomo sigue vivo
    // para las medidas que si bajan sin forma.
    let model = model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty])",
        &model,
        &filters,
        &[FACT_QTY],
    )
    .expect("extrae");

    assert_eq!(atoms.outputs.len(), 1);
    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    assert_eq!(plan.columns.len(), 2);
    assert_eq!(plan.columns[1].measure.as_deref(), Some("SUM(Qty)"));
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

// ---------------------------------------------------------------------------------------------
// Agregaciones sobre expresion
// ---------------------------------------------------------------------------------------------

/// Un modelo con las columnas de negocio del plan: la linea de venta trae cantidad y
/// precio, el producto trae el costo.
///
/// `Revenue = SUM(Qty * Price)` y `COGS = SUM(Qty * Cost)` son las medidas mas simples de
/// cualquier informe real, y ninguna baja si el atomo solo puede llevar una columna.
fn business_model() -> Model {
    let mut model = model();
    model.tables[0].columns.push(Column::new(
        ObjectId(14),
        "Price",
        DataType::Moneda,
    ));
    model.tables[1].columns.push(Column::new(
        ObjectId(24),
        "Cost",
        DataType::Moneda,
    ));
    model.measures = vec![
        Measure::new(ObjectId(100), "Revenue", "SUM('Fact Sales'[Qty] * 'Fact Sales'[Price])"),
        Measure::new(ObjectId(101), "COGS", "SUM('Fact Sales'[Qty] * 'Dim Product'[Cost])"),
        Measure::new(ObjectId(102), "Qty", "SUM('Fact Sales'[Qty])"),
    ];
    model
}

#[test]
fn una_agregacion_sobre_expresion_de_una_tabla_baja() {
    let model = business_model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty] * 'Fact Sales'[Price])",
        &model,
        &filters,
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    assert!(
        plan.sql.contains("SUM((t0.\"Qty\" * t0.\"Price\"))"),
        "esperaba la expresion entera dentro del agregado:\n{}",
        plan.sql
    );
}

#[test]
fn una_agregacion_sobre_expresion_de_dos_tablas_trae_la_dimensjon_al_from() {
    // El caso que mas se escondia: la dimension no agrupa y no filtra, asi que el plan
    // antiguo no la unia, y el `Cost` quedaba sin calificar. El SQL se generaba bien y
    // DuckDB fallaba al ejecutarlo.
    let model = business_model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty] * 'Dim Product'[Cost])",
        &model,
        &filters,
        &[PRODUCT_CATEGORY],
    )
    .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    assert!(
        plan.sql.contains("JOIN \"Dim Product\""),
        "la dimension del costo tiene que estar en el FROM:\n{}",
        plan.sql
    );
    assert!(
        plan.sql.contains("t1.\"Cost\""),
        "el costo tiene que salir de la dimension, no de los hechos:\n{}",
        plan.sql
    );
}

#[test]
fn una_agregacion_sobre_expresion_declara_las_dos_columnas() {
    // El campo `column` del atomo es `None` para una expresion. El FROM se armaba mirando
    // solo ese campo, asi que no encontraba tabla de hechos y rechazaba la medida entera.
    let model = business_model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty] * 'Fact Sales'[Price])",
        &model,
        &filters,
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    let atom = &atoms.atoms[0];
    assert_eq!(atom.column, None, "una expresion no es una columna");
    assert_eq!(
        atom.referenced_columns(),
        vec![FACT_QTY, ObjectId(14)],
        "tiene que declarar las columnas que toca"
    );
    assert_eq!(
        atom.anchor_table(&model),
        Some(FACT),
        "y con ellas se puede encontrar la tabla de hechos"
    );
}

#[test]
fn los_parentesis_de_la_expresion_no_se_pueden_sacar() {
    // `SUM(a + b * c)` no es `SUM(a) + SUM(b) * SUM(c)`. Los parentesis vienen del arbol,
    // no del texto: quitarlos cambia el resultado.
    let model = business_model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms(
        "SUM('Fact Sales'[Qty] * 'Fact Sales'[Price] + 1)",
        &model,
        &filters,
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    assert!(
        plan.sql.contains("(t0.\"Qty\" * t0.\"Price\") + 1"),
        "la precedencia tiene que venir del arbol:\n{}",
        plan.sql
    );
}

#[test]
fn margin_baja_a_sql_repitiendo_la_expresion_del_agregado() {
    // `Margin = ([Revenue] - [COGS]) / [Revenue]` es aritmetica ENTRE agregaciones. Un atomo
    // es una hoja y `Margin` es una expresion sobre dos hojas distintas.
    //
    // La forma correcta es repetir la expresion del agregado en la lista de seleccion, sin
    // subconsulta y sin `DELIM_JOIN`: es exactamente lo que hace el SQL a mano que se midio
    // en `probe_measures`, con un solo `HASH_GROUP_BY`.
    let model = business_model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms("([Revenue] - [COGS]) / [Revenue]", &model, &filters, &[PRODUCT_REGION])
        .expect("extrae");

    assert_eq!(atoms.atoms.len(), 2, "dos hojas, no tres: Revenue sale dos veces");
    assert_eq!(atoms.outputs.len(), 1, "una columna de salida");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    assert!(
        plan.sql.contains("NULLIF"),
        "la division por cero tiene que dar NULL, no inf ni NaN:\n{}",
        plan.sql
    );
    assert!(
        !plan.sql.contains("FROM ( SELECT") && !plan.sql.contains("LATERAL"),
        "no puede haber subconsulta por celda:\n{}",
        plan.sql
    );
}

#[test]
fn la_division_por_cero_produce_null_y_no_un_numero() {
    // DuckDB devuelve `inf` para `10 / 0` y `NaN` para `0 / 0`. Sin `NULLIF`, el grid
    // muestra un numero visiblemente falso donde DAX muestra BLANK. Es el mismo bug en dos
    // caminos, y por eso los dos tienen que convertir.
    let model = business_model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms("([Revenue] - [COGS]) / [Revenue]", &model, &filters, &[PRODUCT_REGION])
        .expect("extrae");
    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");

    // `NULLIF(denominador, 0)`: el denominador va entre parentesis y luego el `, 0`.
    let rendered = plan.sql.replace(' ', "");
    assert!(
        rendered.contains("NULLIF("),
        "el denominador de la division va envuelto en NULLIF:\n{}",
        plan.sql
    );
}

#[test]
fn una_forma_binaria_por_nivel_mantiene_la_precedencia() {
    // `SUM(a + b * c)` no es `SUM(a) + SUM(b) * SUM(c)`. Los parentesis vienen del arbol.
    let model = business_model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms(
        "[Qty] + [Qty] * [Qty]",
        &model,
        &filters,
        &[PRODUCT_REGION],
    )
    .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    let rendered = plan.sql.replace(' ', "");
    assert!(
        rendered.contains("(SUM(t0.\"Qty\")+(SUM(t0.\"Qty\")*SUM(t0.\"Qty\")))"),
        "la precedencia tiene que venir del arbol, no del texto:\n{}",
        plan.sql
    );
}

#[test]
fn el_sql_ordena_por_las_columnas_de_agrupamiento() {
    // Sin `ORDER BY`, DuckDB devuelve los grupos en el orden de su hash, que es estable
    // dentro de una consulta y distinto entre consultas segun el plan. El usuario ve las
    // filas reordenarse al tocar un filtro de otra dimension, y el arnes diferencial
    // diverge en todas las medidas no aditivas solo por el orden.
    let model = model();
    let filters = FilterContext::empty();
    let atoms = extract_atoms("SUM('Fact Sales'[Qty])", &model, &filters, &[PRODUCT_REGION])
        .expect("extrae");

    let plan = lower(&model, &sources(&model), &atoms, &filters).expect("baja");
    assert!(
        plan.sql.contains("ORDER BY t1.\"Region\""),
        "el ORDER BY tiene que ir por las columnas de agrupamiento:\n{}",
        plan.sql
    );
    assert!(
        !plan.sql.contains("ORDER BY \"atom_0\""),
        "ordenar por una medida haria que las filas cambiaran de lugar al recalcular:\n{}",
        plan.sql
    );
}

#[test]
fn un_filtro_concreto_no_lo_tira_la_propagacion() {
    // `FACT_PRODUCT_ID = 3` y un filtro de `Region` llegan los dos a la MISMA columna del
    // hecho. La propagacion reemplazaba el filtro concreto por el semijoin, y el informe
    // devolvia el subtotal de la region entera en vez del detalle de la fila.
    //
    // El sintoma no es un error ni un vacio: es un total que parece un detalle. Por eso el
    // test mira que los DOS filtros sigan en el SQL.
    let model = model();
    let filters = FilterContext::empty()
        .with_equal(FACT_PRODUCT_ID, FilterValue::Integer(3))
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let atoms = extract_atoms("SUM('Fact Sales'[Qty])", &model, &filters, &[PRODUCT_REGION])
        .expect("extrae");

    let on_fact = atoms.atoms[0].context.filters_on(FACT_PRODUCT_ID);
    assert!(
        on_fact
            .iter()
            .any(|filter| !filter.is_propagated() && filter.values() == [FilterValue::Integer(3)]),
        "el filtro concreto se perdio en la propagacion: {:?}",
        on_fact
    );
    assert!(
        on_fact.iter().any(|filter| filter.is_propagated()),
        "y el semijoin de la region tambien tiene que estar: {:?}",
        on_fact
    );
}
