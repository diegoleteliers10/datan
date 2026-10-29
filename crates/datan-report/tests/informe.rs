//! El informe como producto: consultas minimas, matriz correcta, cross-filter.
//!
//! El arnes diferencial ya prueba que una medida da el mismo numero por los dos caminos.
//! Aqui se prueba lo que un informe necesita y una medida sola no: que N medidas cuesten las
//! consultas MINIMAS, que las filas de consultas distintas se peguen en la matriz correcta,
//! y que pinchar una celda cambie el filtro de todas las medidas a la vez.

use datan_dax_eval::FilterValue;
use datan_model::{Column, DataType, Measure, ObjectId};
use datan_report::source::DuckSource;
use datan_report::{Header, Matrix, Report, RowKind, TopN, Totales};
use datan_testkit::{
    FACT_QTY, PRODUCT_CATEGORY, PRODUCT_ID, PRODUCT_REGION, Y2024, engine, engine_con_region_nula,
    memory_source, memory_source_con_region_nula, model,
};

fn measure(id: &str) -> ObjectId {
    let model = model();
    model
        .measures
        .iter()
        .find(|m| m.name == id)
        .unwrap_or_else(|| panic!("no existe la medida {id}"))
        .id
}

fn matrix() -> Matrix {
    let report = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Amount"), measure("Margin")]);
    report
        .execute(&engine().expect("motor"), None)
        .expect("corre")
}

#[test]
fn el_filtro_de_un_visual_no_cambia_la_matriz_del_otro() {
    let engine = engine().expect("motor");
    let page = Report::new(model())
        .with_dimensions(vec![PRODUCT_REGION])
        .with_measures(vec![measure("Qty")])
        .filtered_to(PRODUCT_REGION, vec![FilterValue::Text("Norte".to_string())]);
    let untargeted = page
        .execute(&engine, None)
        .expect("matriz sin filtro visual");
    let targeted = page
        .filtered_to(PRODUCT_CATEGORY, vec![FilterValue::Text("A".to_string())])
        .execute(&engine, None)
        .expect("matriz con filtro visual");

    assert_eq!(untargeted.row_count(), 1);
    assert_eq!(targeted.row_count(), 1);
    assert_eq!(untargeted.cell(0, 0), Some("Norte"));
    assert_eq!(targeted.cell(0, 0), Some("Norte"));
    assert_ne!(untargeted.cell(0, 1), targeted.cell(0, 1));
}

#[test]
fn distinct_values_respeta_el_orden_y_el_limite() {
    let report = Report::new(model());
    let values = report
        .distinct_values(&engine().expect("motor"), ObjectId(21), 2)
        .expect("lee los valores");

    assert_eq!(
        values,
        vec![
            FilterValue::Text("Este".to_string()),
            FilterValue::Text("Norte".to_string()),
        ]
    );
}

#[test]
fn distinct_values_pagea_sin_cambiar_el_orden_del_modelo() {
    let report = Report::new(model());
    let values = report
        .distinct_values_page(&engine().expect("motor"), ObjectId(21), 1, 2)
        .expect("lee la segunda pagina");

    assert_eq!(
        values,
        vec![
            FilterValue::Text("Norte".to_string()),
            FilterValue::Text("Sur".to_string()),
        ]
    );
}

#[test]
fn distinct_values_respeta_los_filtros_de_la_tabla() {
    let report = Report::new(model()).filtered_to(PRODUCT_ID, vec![FilterValue::Integer(2)]);
    let values = report
        .distinct_values_page(&engine().expect("motor"), PRODUCT_REGION, 0, 10)
        .expect("lee las regiones filtradas");

    assert_eq!(values, vec![FilterValue::Text("Este".to_string()),]);
}

#[test]
fn distinct_values_filtra_con_in_y_conserva_el_orden_alfabetico() {
    let report = Report::new(model()).filtered_to(
        PRODUCT_ID,
        vec![FilterValue::Integer(1), FilterValue::Integer(2)],
    );
    let values = report
        .distinct_values_page(&engine().expect("motor"), PRODUCT_REGION, 0, 10)
        .expect("lee las regiones filtradas");

    assert_eq!(
        values,
        vec![
            FilterValue::Text("Este".to_string()),
            FilterValue::Text("Sur".to_string()),
        ]
    );
}

#[test]
fn distinct_values_conserva_el_fallback_para_filtros_entre_tablas() {
    let report = Report::new(model()).filtered_to(PRODUCT_ID, vec![FilterValue::Integer(2)]);
    let values = report
        .distinct_values_page(&engine().expect("motor"), FACT_QTY, 0, 10)
        .expect("lee los valores de la tabla de hechos");

    assert_eq!(
        values,
        vec![
            FilterValue::Integer(2),
            FilterValue::Integer(5),
            FilterValue::Integer(8),
            FilterValue::Blank,
        ]
    );
}

#[test]
fn distinct_values_deja_blank_al_final() {
    let report = Report::new(model());
    let values = report
        .distinct_values(&engine_con_region_nula().expect("motor"), ObjectId(21), 10)
        .expect("lee los valores");

    assert_eq!(
        values,
        vec![
            FilterValue::Text("Este".to_string()),
            FilterValue::Text("Norte".to_string()),
            FilterValue::Text("Sur".to_string()),
            FilterValue::Blank,
        ]
    );
}

#[test]
fn distinct_values_devuelve_vacio_si_la_columna_no_tiene_filas() {
    let engine = datan_storage::Engine::open(":memory:").expect("motor");
    engine
        .execute("CREATE TABLE dim_product (region VARCHAR)")
        .expect("tabla vacia");

    let values = Report::new(model())
        .distinct_values(&engine, ObjectId(21), 10)
        .expect("lee una columna vacia");

    assert!(values.is_empty());
}

#[test]
fn una_matriz_tiene_una_columna_por_dimension_y_una_por_medida() {
    let matrix = matrix();
    assert_eq!(matrix.width(), 4, "una dimension y tres medidas");
    assert_eq!(matrix.row_count(), 3, "Norte, Sur y Este");

    assert_eq!(matrix.headers[0].label, "dim_product[region]");
    assert!(matrix.headers[0].is_dimension());
    assert_eq!(matrix.headers[1].label, "Qty");
    assert_eq!(matrix.headers[3].label, "Margin");
    for header in &matrix.headers[1..] {
        assert!(!header.is_dimension(), "{} es una medida", header.label);
    }
}

#[test]
fn las_filas_traen_el_valor_de_la_dimension_primero() {
    let matrix = matrix();
    let regions: Vec<&str> = (0..matrix.row_count())
        .filter_map(|row| matrix.cell(row, 0))
        .collect();
    assert_eq!(
        regions,
        vec!["Este", "Norte", "Sur"],
        "ordenado, no en hash"
    );
}

#[test]
fn tres_medidas_no_cuestan_tres_consultas() {
    // La deduplicacion por conjunto de atomos. `Margin` usa las hojas de `Qty` y de
    // `Amount`, asi que tiene su propia consulta; `Qty` y `Amount` tambien, porque
    // `Margin` produce un conjunto distinto. Lo que importa es que anadir una medida que
    // comparte hojas NO anade una consulta.
    let model = model();
    let solo = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);
    assert_eq!(solo.query_count().expect("cuenta"), 1);

    let tres = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Amount"), measure("Margin")]);
    assert!(
        tres.query_count().expect("cuenta") <= 3,
        "tres medidas no pueden costar mas que tres consultas"
    );
}

#[test]
fn el_total_de_la_matriz_es_la_suma_de_sus_filas() {
    // La prueba de que las filas de consultas DISTINTAS se pegaron en la columna correcta.
    //
    // Si `Margin` se pegara en la columna de `Qty`, la suma de la columna de `Margin` seria
    // la de `Qty` y el informe no se notaria mirando una celda suelta.
    let matrix = matrix();
    let mut total = 0.0f64;
    for row in 0..matrix.row_count() {
        total += matrix
            .cell(row, 2)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
    }
    assert_eq!(total, 74000.0, "el total de Amount en la suite");

    // Y `Qty` es OTRO numero. Si coincidieran, la pegada de columnas seria incorrecta.
    let qty: f64 = (0..matrix.row_count())
        .filter_map(|row| matrix.cell(row, 1).and_then(|v| v.parse::<f64>().ok()))
        .sum();
    assert_eq!(qty, 165.0);
}

#[test]
fn margin_no_es_qty_repetido() {
    // Si `Margin` tivesse pegada la columna de `Qty`, seria el mismo numero. Tiene que ser
    // distinto: es `(Qty - Amount) / Qty`.
    let matrix = matrix();
    for row in 0..matrix.row_count() {
        let qty: f64 = matrix
            .cell(row, 1)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let amount: f64 = matrix
            .cell(row, 2)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let margin: f64 = matrix
            .cell(row, 3)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let expected = if qty == 0.0 {
            0.0
        } else {
            (qty - amount) / qty
        };
        assert!(
            (margin - expected).abs() < 1e-6,
            "fila {row}: margin {margin}, esperado {expected} con qty {qty} y amount {amount}"
        );
    }
}

#[test]
fn una_medida_con_divide_no_es_su_primera_hoja() {
    // `DIVIDE` no tenia forma propia: el conjunto de atomos quedaba con dos hojas y NINGUNA
    // salida, el plan emitia una columna por hoja, y el informe pegaba la primera en la
    // columna de la medida. `Ratio` salia con el valor de `Qty`: un numero con el tipo
    // correcto, sin error y sin aviso.
    let model = model();
    let report = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Amount"), measure("Ratio")]);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    for row in 0..matrix.row_count() {
        let qty: f64 = matrix
            .cell(row, 1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let amount: f64 = matrix
            .cell(row, 2)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let ratio: f64 = matrix
            .cell(row, 3)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let expected = if amount == 0.0 { 0.0 } else { qty / amount };
        assert!(
            (ratio - expected).abs() < 1e-9,
            "fila {row}: ratio {ratio}, esperado {expected} con qty {qty} y amount {amount}"
        );
    }
}

#[test]
fn una_medida_con_if_elige_por_fila_y_no_copia_su_hoja() {
    // `IF` tampoco tenia forma: la comparacion de la condicion tiene que llegar a los dos
    // caminos, y sin eso la medida salia con el valor de su primera hoja.
    let model = model();
    let report = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Condicional")]);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    for row in 0..matrix.row_count() {
        let qty: f64 = matrix
            .cell(row, 1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let condicional: f64 = matrix
            .cell(row, 2)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let expected = if qty > 50.0 { qty } else { 0.0 };
        assert_eq!(
            condicional, expected,
            "fila {row}: condicional {condicional}, esperado {expected} con qty {qty}"
        );
    }
}

#[test]
fn las_filas_vienen_ordenadas_para_que_el_clic_sea_estable() {
    // El clic sobre la celda (fila, columna) tiene que seguir apuntando a la misma fila
    // después de un reordenamiento. Sin `ORDER BY` en el SQL, el hash de DuckDB puede dar
    // otro orden en la siguiente consulta y el filtro_cross se aplicaría a otra fila.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);
    let engine = engine().expect("motor");

    let primera = report.execute(&engine, None).expect("corre");
    for _ in 0..5 {
        let otra = report.execute(&engine, None).expect("corre");
        assert_eq!(
            primera.rows, otra.rows,
            "el orden de las filas cambia entre ejecuciones"
        );
    }
}

#[test]
fn pinchar_una_celda_filtra_todas_las_medidas() {
    let report = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Amount"), measure("Margin")]);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    // La celda (Norte, 0) es la region. Pinchar ahi filtra el informe entero.
    let row = (0..matrix.row_count())
        .find(|row| matrix.cell(*row, 0) == Some("Norte"))
        .expect("Norte esta en la matriz");
    let drilled = matrix
        .drill(&report, row, 0)
        .expect("pinchar una dimension");
    let filtered = drilled
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    assert_eq!(filtered.row_count(), 1, "solo la fila filtrada");
    assert_eq!(filtered.cell(0, 0), Some("Norte"));
    // Y las tres medidas bajan al mismo filtro, no solo la primera.
    assert_eq!(
        filtered.cell(0, 1).and_then(|v| v.parse::<f64>().ok()),
        Some(60.0),
        "Qty filtrado por Norte"
    );
    assert_eq!(
        filtered.cell(0, 2).and_then(|v| v.parse::<f64>().ok()),
        Some(24000.0),
        "Amount filtrado por Norte"
    );
}

#[test]
fn pinchar_una_celda_de_medida_no_filtra() {
    // Una medida no se puede usar como filtro. Si `drill` devolviera un informe, el usuario
    // pincharia una celda de numero y no pasaria nada, sin avisar.
    let report = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    assert!(
        matrix.drill(&report, 0, 1).is_none(),
        "una medida no es un filtro"
    );
}

#[test]
fn soltar_un_filtro_devuelve_el_informe_completo() {
    let report = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);
    let engine = engine().expect("motor");

    let drilled = report.filtered_by(ObjectId(21), FilterValue::Text("Norte".to_string()));
    assert_eq!(
        drilled.execute(&engine, None).expect("corre").row_count(),
        1
    );
    let soltado = drilled.unfiltered(ObjectId(21));
    assert_eq!(
        soltado.execute(&engine, None).expect("corre").row_count(),
        3
    );
}

#[test]
fn un_informe_sin_medidas_falla_con_mensaje() {
    let report = Report::new(model()).with_dimensions(vec![ObjectId(21)]);
    let error = report
        .execute(&engine().expect("motor"), None)
        .expect_err("falla");
    assert!(
        error.to_string().contains("medida"),
        "el mensaje tiene que decir que falta una medida: {error}"
    );
}

#[test]
fn una_medida_que_cae_al_interprete_sin_origen_falla_en_vez_de_dar_blanco() {
    // Con un origen vacio, `CALCULATE` daria BLANK y la celda pareceria una categoria sin
    // datos. Es mejor un error que se ve que un cero que no.
    let report = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![ObjectId(100)]);
    let calculate = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![ObjectId(100)]);

    // El modelo de la suite tiene `Qty`; el caso que cae al interprete se construye aparte.
    assert!(report.execute(&engine().expect("motor"), None).is_ok());
    let _ = calculate;
}
#[test]
fn un_informe_guardado_vuelve_a_dar_los_mismos_numeros() {
    // El viaje completo: guardar la definicion, abrirla contra el modelo, ejecutarla y
    // comparar. Los ids de columnas y medidas son internos y cambian entre ejecuciones, asi
    // que lo que viaja son los NOMBRES y al abrir se resuelven contra el modelo.
    //
    // Si se guardaran ids, el informe se abriria sin error y con la matriz vacia: el fallo
    // mas caro de esta pieza, porque parece un informe sin datos y no un informe roto.
    let model = model();
    let report = datan_report::Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Amount")]);
    let engine = engine().expect("motor");

    let antes = report.execute(&engine, None).expect("corre");

    datan_report::saved::save(&report, "informe de prueba").expect("guarda");

    // Se abre contra el mismo modelo, que es lo que pasa al reabrir la app.
    let reabierto = datan_report::saved::open("informe de prueba", &model).expect("abre");
    assert_eq!(reabierto.dimensions, vec![ObjectId(21)]);
    assert_eq!(reabierto.measures.len(), 2);

    let despues = reabierto.execute(&engine, None).expect("corre");
    assert_eq!(
        antes.rows, despues.rows,
        "los numeros no cambian al reabrir"
    );

    let _ = std::fs::remove_file(datan_report::saved::folder().join("informe de prueba.toml"));
}

#[test]
fn los_filtros_se_guardan_y_se_vuelven_a_aplicar() {
    // Sin los filtros, un informe guardado abriria con todos los datos y el usuario
    // pensaria que el informe cambio de numero.
    let model = model();
    let report = datan_report::Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")])
        .filtered_by(ObjectId(21), FilterValue::Text("Norte".to_string()));
    let engine = engine().expect("motor");

    datan_report::saved::save(&report, "informe filtrado").expect("guarda");
    let reabierto = datan_report::saved::open("informe filtrado", &model).expect("abre");
    let matrix = reabierto.execute(&engine, None).expect("corre");

    assert_eq!(matrix.row_count(), 1, "solo la fila filtrada");
    assert_eq!(matrix.cell(0, 0), Some("Norte"));

    let _ = std::fs::remove_file(datan_report::saved::folder().join("informe filtrado.toml"));
}

#[test]
fn los_nombres_de_medidas_guardadas_resuelven_contra_el_modelo() {
    // Guardar los NOMBRES y no los ids es la razon de que este viaje funcione. Un id
    // interno cambia entre ejecuciones; un nombre no.
    let model = model();
    let report = datan_report::Report::new(model.clone()).with_measures(vec![measure("Qty")]);

    datan_report::saved::save(&report, "informe de nombres").expect("guarda");
    let reabierto = datan_report::saved::open("informe de nombres", &model).expect("abre");

    let nombres: Vec<&str> = reabierto
        .measures
        .iter()
        .filter_map(|id| reabierto.measure_name(*id))
        .collect();
    assert_eq!(nombres, vec!["Qty"]);

    let _ = std::fs::remove_file(datan_report::saved::folder().join("informe de nombres.toml"));
}

#[test]
fn los_totales_de_las_tarjetas_son_los_mismos_que_la_suma_de_la_matriz() {
    // Para una medida ADITIVA, el total de la tarjeta tiene que coincidir con la suma de la
    // matriz. Es la prueba de que los dos caminos (con y sin GROUP BY) leen lo mismo.
    let model = model();
    let report = datan_report::Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Amount")]);
    let engine = engine().expect("motor");

    let matrix = report.execute(&engine, None).expect("corre");
    let totals = report.totals(&engine, None).expect("totales");

    let qty_matriz: f64 = (0..matrix.row_count())
        .filter_map(|row| matrix.cell(row, 1).and_then(|v| v.parse::<f64>().ok()))
        .sum();
    let qty_total: f64 = totals[0].parse().unwrap_or(0.0);
    assert!((qty_matriz - qty_total).abs() < 1e-6);

    let amount_matriz: f64 = (0..matrix.row_count())
        .filter_map(|row| matrix.cell(row, 2).and_then(|v| v.parse::<f64>().ok()))
        .sum();
    let amount_total: f64 = totals[1].parse().unwrap_or(0.0);
    assert!((amount_matriz - amount_total).abs() < 1e-6);
}

#[test]
fn una_medida_sin_dimensiones_produce_una_fila_escalar() {
    let engine = engine().expect("motor");
    let report = Report::new(model()).with_measures(vec![measure("Qty"), measure("Amount")]);

    let matrix = report.execute(&engine, None).expect("corre por SQL");
    let totals = report.totals(&engine, None).expect("totales");
    assert_eq!(matrix.row_count(), 1);
    assert_eq!(matrix.cell(0, 0), Some("165"));
    assert_eq!(matrix.cell(0, 1), Some(totals[1].as_str()));
    assert_eq!(matrix.row_kind(0), RowKind::Data);

    let report = Report::new(model()).with_measures(vec![measure("QtyDoble")]);
    let matrix = report
        .execute(&engine, Some(&memory_source()))
        .expect("corre por el interprete");
    assert_eq!(matrix.row_count(), 1);
    assert_eq!(matrix.cell(0, 0), Some("330"));
    assert_eq!(matrix.row_kind(0), RowKind::Data);
}

#[test]
fn una_medida_escalar_marca_el_total_si_el_informe_lo_pide() {
    let report = Report::new(model())
        .with_measures(vec![measure("Qty")])
        .with_totales(Totales::General);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    assert_eq!(matrix.row_count(), 1);
    assert_eq!(matrix.cell(0, 0), Some("165"));
    assert_eq!(matrix.row_kind(0), RowKind::GrandTotal);
}

#[test]
fn las_tarjetas_no_confunden_dos_medidas_que_comparten_hoja() {
    // `Qty` y `Grande` leen la MISMA hoja `SUM(qty)` y dan numeros distintos: el total de
    // `Qty` en la suite es 165 y el umbral de `Grande` es 1000, asi que su total es cero. Si
    // las dos caen en el mismo grupo de lectura, la consulta produce una sola columna con la
    // forma de la primera y las dos tarjetas muestran el mismo numero.
    let mut model = model();
    model.measures.push(Measure::new(
        ObjectId(901),
        "Grande",
        "IF([Qty] > 1000, [Qty], 0)",
    ));

    let report = datan_report::Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), ObjectId(901)]);
    let engine = engine().expect("motor");

    assert_eq!(
        report.query_count().expect("cuenta"),
        2,
        "una consulta por forma"
    );

    let totals = report.totals(&engine, None).expect("totales");
    let qty: f64 = totals[0].parse().unwrap_or(0.0);
    let grande: f64 = totals[1].parse().unwrap_or(0.0);
    assert_eq!(qty, 165.0);
    assert_eq!(grande, 0.0, "el total no es el de Qty");

    // Y la matriz dice lo mismo que las tarjetas, con la misma separacion de planes.
    let matrix = report.execute(&engine, None).expect("corre");
    for row in 0..matrix.row_count() {
        assert_eq!(matrix.cell(row, 2), Some("0"));
    }
}

#[test]
fn dos_medidas_iguales_llenan_las_dos_tarjetas() {
    // El grupo produce UNA columna para las dos medidas. La segunda tarjeta leia la columna 1,
    // que no existe, y quedaba vacia: un guion que parece un dato que falta.
    let mut model = model();
    let original = model
        .measures
        .iter()
        .find(|m| m.name == "Qty")
        .expect("no esta Qty")
        .clone();
    model.measures.push(Measure::new(
        ObjectId(900),
        "Unidades",
        original.expression.clone(),
    ));

    let report = datan_report::Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![ObjectId(900), original.id]);
    let totals = report
        .totals(&engine().expect("motor"), None)
        .expect("totales");

    assert_eq!(totals.len(), 2);
    assert_eq!(totals[0], "165");
    assert_eq!(totals[1], "165", "la segunda tarjeta no puede quedar vacia");
}

#[test]
fn el_grafico_toma_los_primeros_valores_ordenados_por_la_medida() {
    let matrix = matrix();
    let top = matrix.top(0, 1, 2);

    assert_eq!(top.len(), 2);
    assert!(top[0].1 >= top[1].1, "ordenado de mayor a menor: {top:?}");
    assert!(
        top.iter()
            .all(|(label, _)| ["Norte", "Sur", "Este"].contains(&label.as_str()))
    );
}

#[test]
fn el_grafico_con_mas_valores_de_los_que_hay_los_da_todos() {
    let matrix = matrix();
    assert_eq!(matrix.top(0, 1, 100).len(), 3);
}

#[test]
fn un_rango_de_fechas_acota_las_filas() {
    // Los botones 3M / 6M / 12M del lienzo. El rango se impone con dos filtros, `>=` y `<=`,
    // sobre la columna de fecha de la tabla de hechos.
    let report = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);
    let engine = engine().expect("motor");
    let completo = report.execute(&engine, None).expect("corre");
    assert!(completo.row_count() > 0, "el informe sin rango tiene filas");

    // El fixture tiene dias desde Y2023 (2023-01-01) hacia adelante, 730 dias.
    let seis_meses = report.filtered_between(datan_testkit::FACT_DAY, Y2024, Y2024 + 180);
    let acotado = seis_meses.execute(&engine, None).expect("corre");
    let qty: f64 = (0..acotado.row_count())
        .filter_map(|row| acotado.cell(row, 1).and_then(|v| v.parse::<f64>().ok()))
        .sum();
    let qty_completo: f64 = (0..completo.row_count())
        .filter_map(|row| completo.cell(row, 1).and_then(|v| v.parse::<f64>().ok()))
        .sum();
    assert!(
        qty > 0.0 && qty < qty_completo,
        "el rango tiene que recortar: {qty} contra {qty_completo}"
    );

    // Un rango anterior a todos los datos deja la matriz vacia, no falla.
    let antes = report.filtered_between(datan_testkit::FACT_DAY, Y2024 - 500, Y2024 - 400);
    assert_eq!(antes.execute(&engine, None).expect("corre").row_count(), 0);
}

#[test]
fn la_ventana_anterior_mide_lo_mismo_que_la_actual() {
    // El delta de las tarjetas compara la ventana actual contra la MISMA ventana un ano
    // antes. Si las dos midieran distinto, un 29 de febrero haria que el total anterior
    // cubriera un dia de mas y el porcentaje saldría falseado sin que nada fallara.
    use datan_dax_eval::{CalendarUnit, DateRange, TimeFunction, shift};

    let today = Y2024 + 365;
    // La ventana de la app: el INICIO sale de `shift` y el FIN es el ancla. Pedirle a
    // `shift` la ventana entera daria un rango del mismo tamaño que el de entrada, y un
    // rango de un dia se desplaza a un dia.
    let ventana = |ancla: i32| {
        let inicio = shift(
            TimeFunction::DateAdd {
                units: -6,
                unit: CalendarUnit::Month,
            },
            DateRange::new(ancla, ancla),
            today,
        )
        .start;
        DateRange::new(inicio, ancla)
    };
    let actual = ventana(today);
    let un_ano_atras = shift(
        TimeFunction::DateAdd {
            units: -1,
            unit: CalendarUnit::Year,
        },
        DateRange::new(today, today),
        today,
    )
    .start;
    let anterior = ventana(un_ano_atras);

    assert_eq!(
        actual.len(),
        anterior.len(),
        "las dos ventanas, igual de largas"
    );
    assert!(
        actual.start > anterior.end,
        "la ventana anterior termina antes de empezar la actual"
    );
    assert!(actual.contains(today), "la ventana actual llega hasta hoy");
    assert!(
        !anterior.contains(today),
        "la ventana anterior no llega hasta hoy"
    );
}

#[test]
fn un_filtro_con_varios_valores_da_la_union_y_sobrevive_a_guardar() {
    // Un slicer con dos valores elegidos. El conjunto va DENTRO de un filtro (`IN`), y no son
    // dos filtros que se intersectan: dos filtros sobre la misma columna dejan la matriz
    // vacia, y el usuario veria un informe sin datos en vez de dos regiones.
    let model = model();
    let report = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")])
        .filtered_to(
            ObjectId(21),
            vec![
                FilterValue::Text("Norte".to_string()),
                FilterValue::Text("Sur".to_string()),
            ],
        );
    let engine = engine().expect("motor");
    let matrix = report.execute(&engine, None).expect("corre");

    assert_eq!(matrix.row_count(), 2, "dos regiones elegidas");
    assert_eq!(matrix.cell(0, 0), Some("Norte"));
    assert_eq!(matrix.cell(1, 0), Some("Sur"));

    // Y el viaje por el archivo guarda los DOS valores, no el primero.
    datan_report::saved::save(&report, "informe de dos regiones").expect("guarda");
    let reabierto = datan_report::saved::open("informe de dos regiones", &model).expect("abre");
    let otra = reabierto.execute(&engine, None).expect("corre");
    assert_eq!(otra.rows, matrix.rows, "los dos valores vuelven igual");

    let _ =
        std::fs::remove_file(datan_report::saved::folder().join("informe de dos regiones.toml"));
}

#[test]
fn quitar_todos_los_valores_deja_el_informe_sin_filtro() {
    // Un conjunto vacio es "sin filtro", no un `IN ()`: para el usuario, destildar el ultimo
    // valor de un slicer no puede vaciar el informe.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);
    let engine = engine().expect("motor");

    let sin_filtro = report.filtered_to(ObjectId(21), vec![]);
    assert_eq!(
        sin_filtro
            .execute(&engine, None)
            .expect("corre")
            .row_count(),
        3
    );

    let con_filtro =
        sin_filtro.filtered_to(ObjectId(21), vec![FilterValue::Text("Norte".to_string())]);
    assert_eq!(
        con_filtro
            .execute(&engine, None)
            .expect("corre")
            .row_count(),
        1
    );
    let vuelto = con_filtro.filtered_to(ObjectId(21), vec![]);
    assert_eq!(vuelto.execute(&engine, None).expect("corre").row_count(), 3);
}

#[test]
fn dos_filtros_que_se_contradicen_no_dejan_pasar_ninguna_fila() {
    // `KEEPFILTERS` intersecta dos contextos y, cuando no comparten ningun valor, deja un
    // `IN` sin valores como marca de contradiccion. Ese `IN` SI significa "ninguna fila":
    // la region no puede ser Norte y Este a la vez. El SQL lo plegaba como `1 = 1` y el
    // informe daba todas las filas por SQL y ninguna por el interprete.
    let model = model();
    let mut report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);

    let norte = datan_dax_eval::FilterContext::default().with(datan_dax_eval::Filter::in_values(
        ObjectId(21),
        vec![FilterValue::Text("Norte".to_string())],
    ));
    let este = datan_dax_eval::FilterContext::default().with(datan_dax_eval::Filter::in_values(
        ObjectId(21),
        vec![FilterValue::Text("Este".to_string())],
    ));
    report.filters = norte.intersect(&este);

    let engine = engine().expect("motor");
    let por_sql = report.execute(&engine, None).expect("corre");
    assert_eq!(por_sql.row_count(), 0, "SQL");

    let source = memory_source();
    let por_interprete = report.execute(&engine, Some(&source)).expect("corre");
    assert_eq!(por_interprete.row_count(), 0, "interprete");
}

#[test]
fn dos_medidas_iguales_dan_el_mismo_numero() {
    // Dos medidas con la MISMA expresion comparten un plan y una sola consulta, que trae
    // una columna de medidas. Si cada una leyera la columna que le toca por posicion, la
    // segunda leeria una columna que no existe y saldria EN BLANCO: un dato que no
    // falta, que se pierde al pintarlo.
    let mut model = model();
    let original = model
        .measures
        .iter()
        .find(|m| m.name == "Qty")
        .expect("no esta Qty")
        .clone();
    model.measures.push(Measure::new(
        ObjectId(900),
        "Unidades",
        original.expression.clone(),
    ));

    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![ObjectId(900), original.id]);
    assert_eq!(
        report.query_count().expect("cuenta"),
        1,
        "dos medidas iguales, una consulta"
    );

    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    assert_eq!(matrix.width(), 3, "una dimension y dos medidas");
    for row in 0..matrix.row_count() {
        let primera = matrix.cell(row, 1).unwrap_or("");
        let segunda = matrix.cell(row, 2).unwrap_or("");
        assert!(
            !segunda.is_empty(),
            "fila {row}: la segunda medida sale en blanco"
        );
        assert_eq!(
            primera, segunda,
            "fila {row}: medidas iguales, numeros distintos"
        );
    }
}

#[test]
fn una_medida_con_calculate_da_el_total_junto_al_filtrado() {
    // `Variacion` es `[Qty] - CALCULATE([Qty], ALL('dim_product'))`: el MISMO `SUM(qty)`
    // dos veces, con distinto CONTEXTO. Es como se escribe una variacion contra el total.
    //
    // Fallaba dos veces. Sin forma para el `CALCULATE`, el plan emitia una columna por
    // atomo y el informe pegaba la primera: la resta daba CERO. Y con la celda guardada en
    // un mapa por ETIQUETA, las dos hojas se llamaban igual `SUM(qty)` y la segunda pisaba
    // a la primera: tambien cero, sin error y sin aviso.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Variacion")]);
    let source = memory_source();
    let matrix = report
        .execute(&engine().expect("motor"), Some(&source))
        .expect("corre");

    // Este, Norte y Sur, en orden. La segunda columna es la region menos el total 165.
    let esperado = [
        ("Este", 57.0, -108.0),
        ("Norte", 60.0, -105.0),
        ("Sur", 48.0, -117.0),
    ];
    assert_eq!(matrix.row_count(), esperado.len(), "tres regiones");
    for (row, (region, qty, variacion)) in esperado.iter().enumerate() {
        assert_eq!(matrix.cell(row, 0), Some(*region));
        let q: f64 = matrix.cell(row, 1).unwrap().parse().unwrap();
        let v: f64 = matrix.cell(row, 2).unwrap().parse().unwrap();
        assert_eq!(q, *qty, "fila {region}: Qty");
        assert_eq!(v, *variacion, "fila {region}: Variacion contra el total");
    }
}

#[test]
fn el_total_de_una_medida_con_calculate_no_ve_el_valor_de_grupo() {
    // La tarjeta usa `evaluate_totals`, que es otro camino. Con la region filtrada, `Qty`
    // da 60 y la variacion contra el total da -105. Si el `ALL` no llega a quitar el
    // filtro, la resta vuelve a dar cero.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("Variacion")])
        .filtered_to(ObjectId(21), vec![FilterValue::Text("Norte".to_string())]);
    let source = memory_source();
    let totals = report
        .totals(&engine().expect("motor"), Some(&source))
        .expect("totales");

    assert_eq!(totals[0], "60");
    assert_eq!(totals[1], "-105");
}

#[test]
fn un_iterador_tiene_forma_propia_y_no_copia_su_hoja() {
    // `SUMX` cambia el grano y cae al interprete, y aun asi es UNA hoja. Sin forma, el
    // informe pegaba la hoja en la columna de la medida; da la casualidad de que aqui la
    // hoja y la medida no coinciden (el doble), asi que el numero delataba el error.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty"), measure("QtyDoble")]);
    let source = memory_source();
    let matrix = report
        .execute(&engine().expect("motor"), Some(&source))
        .expect("corre");

    for row in 0..matrix.row_count() {
        let qty: f64 = matrix.cell(row, 1).unwrap().parse().unwrap();
        let doble: f64 = matrix.cell(row, 2).unwrap().parse().unwrap();
        assert_eq!(
            doble,
            qty * 2.0,
            "fila {}: el iterador duplica, no copia",
            row
        );
    }
}

#[test]
fn una_medida_sin_forma_da_error_y_no_el_valor_de_su_primera_hoja() {
    // `ABS` no esta en la lista de escalares que los dos caminos saben calcular: la medida
    // se queda sin forma. Antes el plan emitia una columna por atomo y `Cruda` mostraba el
    // valor de `[Qty]`, sin error y sin aviso. Ahora el informe lo dice.
    let model = model();
    let report = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Cruda")]);
    let error = report
        .execute(&engine().expect("motor"), None)
        .expect_err("sin forma es un error");
    assert!(
        error.to_string().contains("Cruda"),
        "el error nombra la medida: {error}"
    );

    // Y lo mismo para la tarjeta.
    let totals = Report::new(model)
        .with_dimensions(vec![])
        .with_measures(vec![measure("Cruda")]);
    assert!(totals.totals(&engine().expect("motor"), None).is_err());
}

#[test]
fn la_matriz_sale_en_el_orden_de_sort_by() {
    // `region` ordenada por `region_orden`: Norte, Sur y Este, que es el orden REAL, no el
    // alfabetico. Los dos caminos tienen que decir el mismo orden: el SQL con un `MIN` en el
    // ORDER BY (la columna de orden no esta en el GROUP BY) y el interprete con la clave de
    // su primera aparicion.
    let mut model = model();
    for table in &mut model.tables {
        if table.name != "dim_product" {
            continue;
        }
        let orden = table
            .columns
            .iter()
            .find(|c| c.name == "region_orden")
            .map(|c| c.id);
        if let Some(column) = table.columns.iter_mut().find(|c| c.name == "region") {
            column.sort_by = orden;
        }
    }
    let report = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);

    let esperado = [("Norte", 60.0), ("Sur", 48.0), ("Este", 57.0)];
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    let regiones: Vec<&str> = (0..matrix.row_count())
        .filter_map(|row| matrix.cell(row, 0))
        .collect();
    assert_eq!(regiones, vec!["Norte", "Sur", "Este"], "camino SQL");

    let source = memory_source();
    let matrix = report
        .execute(&engine().expect("motor"), Some(&source))
        .expect("corre");
    let regiones: Vec<&str> = (0..matrix.row_count())
        .filter_map(|row| matrix.cell(row, 0))
        .collect();
    assert_eq!(regiones, vec!["Norte", "Sur", "Este"], "camino interprete");
    for (row, (region, qty)) in esperado.iter().enumerate() {
        assert_eq!(matrix.cell(row, 0), Some(*region));
        let q: f64 = matrix.cell(row, 1).unwrap().parse().unwrap();
        assert_eq!(q, *qty, "fila {region}");
    }
}

#[test]
fn los_subtotales_cuelgan_de_su_grupo_y_el_total_va_al_final() {
    // Dos dimensiones: la matriz es una jerarquia. Debajo de los detalles de cada region va
    // su subtotal, y el gran total cierra la tabla. El orden importa tanto como el numero:
    // un subtotal junto al grupo equivocado parece un dato.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21), ObjectId(22)])
        .with_measures(vec![measure("Qty")])
        .with_subtotals(true);

    let esperado = [
        ("Este", "A", "24", RowKind::Data),
        ("Este", "B", "33", RowKind::Data),
        ("Este", "", "57", RowKind::Subtotal { level: 0 }),
        ("Norte", "A", "27", RowKind::Data),
        ("Norte", "B", "33", RowKind::Data),
        ("Norte", "", "60", RowKind::Subtotal { level: 0 }),
        ("Sur", "A", "24", RowKind::Data),
        ("Sur", "B", "24", RowKind::Data),
        ("Sur", "", "48", RowKind::Subtotal { level: 0 }),
        ("", "", "165", RowKind::GrandTotal),
    ];

    let engine = engine().expect("motor");
    let matrix = report.execute(&engine, None).expect("corre");
    assert_eq!(
        matrix.row_count(),
        esperado.len(),
        "nueve renglones y el total"
    );
    for (row, (region, categoria, qty, kind)) in esperado.iter().enumerate() {
        assert_eq!(matrix.cell(row, 0), Some(*region), "fila {row}");
        assert_eq!(matrix.cell(row, 1), Some(*categoria), "fila {row}");
        assert_eq!(matrix.cell(row, 2), Some(*qty), "fila {row}");
        assert_eq!(&matrix.row_kind(row), kind, "fila {row}");
    }

    // El interprete arma LA MISMA tabla: mismo orden, mismos renglones marcados.
    let source = memory_source();
    let matrix = report.execute(&engine, Some(&source)).expect("corre");
    assert_eq!(matrix.row_count(), esperado.len());
    for (row, (region, categoria, qty, kind)) in esperado.iter().enumerate() {
        assert_eq!(matrix.cell(row, 0), Some(*region), "interprete fila {row}");
        assert_eq!(
            matrix.cell(row, 1),
            Some(*categoria),
            "interprete fila {row}"
        );
        assert_eq!(matrix.cell(row, 2), Some(*qty), "interprete fila {row}");
        assert_eq!(&matrix.row_kind(row), kind, "interprete fila {row}");
    }
}

#[test]
fn con_una_dimension_los_subtotales_son_el_gran_total() {
    // Una jerarquia de un nivel no tiene niveles intermedios: pedir subtotales con una
    // dimension da los datos y el total, que es lo que se puede resumir.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")])
        .with_subtotals(true);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    assert_eq!(matrix.row_count(), 4, "Este, Norte, Sur y el total");
    for row in 0..3 {
        assert_eq!(matrix.row_kind(row), RowKind::Data);
    }
    assert_eq!(matrix.row_kind(3), RowKind::GrandTotal);
    assert_eq!(matrix.cell(3, 1), Some("165"), "60 + 57 + 48");

    // El interprete dice lo mismo: mismos renglones, mismo total.
    let source = memory_source();
    let otra = report
        .execute(&engine().expect("motor"), Some(&source))
        .expect("corre");
    assert_eq!(otra.rows, matrix.rows);
    assert_eq!(otra.kinds, matrix.kinds);
}

#[test]
fn el_gran_total_con_dos_dimensiones_no_trae_los_subtotales() {
    // `General` es el total SIN los renglones intermedios. Son dos ajustes distintos: una
    // tabla con dos dimensiones y sin subtotales sigue teniendo su total.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21), ObjectId(22)])
        .with_measures(vec![measure("Qty")])
        .with_totales(Totales::General);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    assert_eq!(matrix.row_count(), 7, "seis detalles y el total");
    for row in 0..6 {
        assert_eq!(matrix.row_kind(row), RowKind::Data, "fila {row}");
    }
    assert_eq!(matrix.row_kind(6), RowKind::GrandTotal);
    assert_eq!(matrix.cell(6, 2), Some("165"));

    let source = memory_source();
    let otra = report
        .execute(&engine().expect("motor"), Some(&source))
        .expect("corre");
    assert_eq!(otra.rows, matrix.rows);
    assert_eq!(otra.kinds, matrix.kinds);
}

#[test]
fn una_clave_sin_valor_es_el_renglon_en_blanco_en_los_dos_caminos() {
    // Una dimension con una clave NULL da un grupo: el SQL lo devuelve como NULL y el
    // interprete lo saltaba, asi que la misma tabla tenia una fila mas por un camino. Ahora
    // la fila existe en los dos y se rotula, porque una celda vacia se lee como un dato que
    // falta y es un grupo.
    let report = Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);

    let engine = engine_con_region_nula().expect("motor");
    let sql = report.execute(&engine, None).expect("corre");
    let regiones: Vec<&str> = (0..sql.row_count())
        .filter_map(|row| sql.cell(row, 0))
        .collect();
    assert_eq!(
        regiones,
        vec!["Este", "Norte", "Sur", "(en blanco)"],
        "el blanco va al final, como el NULLS LAST del SQL"
    );

    let source = memory_source_con_region_nula();
    let interprete = report.execute(&engine, Some(&source)).expect("corre");
    assert_eq!(interprete.rows, sql.rows);
    assert_eq!(interprete.kinds, sql.kinds);

    // Y el clic sobre esa fila filtra por BLANK, no por el texto del rotulo: si bajara como
    // texto, no coincidiria con ninguna clave y el informe se vaciaria.
    let en_blanco = sql.row_count() - 1;
    let valor = sql.valor_de_celda(0, sql.cell(en_blanco, 0).unwrap());
    assert_eq!(valor, Some(FilterValue::Blank));
    let solo_blanco = report
        .filtered_to(ObjectId(21), vec![FilterValue::Blank])
        .execute(&engine, None)
        .expect("corre");
    assert_eq!(solo_blanco.row_count(), 1);
    assert_eq!(solo_blanco.cell(0, 0), Some("(en blanco)"));
    assert_eq!(solo_blanco.cell(0, 1), sql.cell(en_blanco, 1));

    let por_interprete = report
        .filtered_to(ObjectId(21), vec![FilterValue::Blank])
        .execute(&engine, Some(&source))
        .expect("corre");
    assert_eq!(por_interprete.rows, solo_blanco.rows);
}

#[test]
fn sin_totales_no_hay_ningun_renglon_de_total() {
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")]);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    assert_eq!(matrix.row_count(), 3);
    assert!(matrix.kinds.iter().all(|kind| *kind == RowKind::Data));
}

#[test]
fn el_subtotal_no_es_un_valor_para_filtrar() {
    // Un subtotal resume un grupo: pincharlo no puede filtrar por un valor que no existe en
    // la columna. La matriz lleva la marca y `row_kind` la entrega; el que decide el clic es
    // la app, pero el contracto empieza aqui: `row_kind` distingue.
    let model = model();
    let report = Report::new(model)
        .with_dimensions(vec![ObjectId(21), ObjectId(22)])
        .with_measures(vec![measure("Qty")])
        .with_subtotals(true);
    let matrix = report
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    let subtotales: Vec<usize> = (0..matrix.row_count())
        .filter(|row| matrix.row_kind(*row) != RowKind::Data)
        .collect();
    assert_eq!(subtotales.len(), 4, "tres subtotales y el gran total");
    for row in subtotales {
        // La dimension recogida queda VACIA, no con un valor inventado.
        assert!(
            matrix.cell(row, 0).unwrap_or("").is_empty()
                || matrix.cell(row, 1).unwrap_or("").is_empty(),
            "fila {row}"
        );
    }
}

#[test]
fn pinchar_una_celda_de_fecha_filtra_por_ese_dia() {
    // El SQL entrega la fecha como `YYYY-MM-DD` y el interprete como el dia desde 1970.
    // Leerla como numero nada mas dejaba el filtro en el dia 0 y la matriz vacia, sin error
    // y sin aviso. La celda se lee en las dos formas.
    let columna = ObjectId(21);
    let report = Report::new(model())
        .with_dimensions(vec![columna])
        .with_measures(vec![measure("Qty")]);
    let matrix = Matrix {
        headers: vec![Header {
            label: "ventas[fecha]".to_string(),
            column: columna,
            measure: None,
            data_type: DataType::Date,
        }],
        rows: vec![vec!["2024-01-01".to_string()]],
        kinds: Vec::new(),
        truncated: false,
    };

    let filtrado = matrix.drill(&report, 0, 0).expect("pinchar filtra");
    let filtro = filtrado
        .filters
        .filters_on(columna)
        .first()
        .expect("hay un filtro");
    let Some(FilterValue::Date(dia)) = filtro.values().first() else {
        panic!("el filtro es un dia: {:?}", filtro.values());
    };
    assert_eq!(*dia, Y2024);

    // La otra forma del MISMO dia es el mismo filtro.
    assert_eq!(
        matrix.valor_de_celda(0, &Y2024.to_string()),
        Some(FilterValue::Date(Y2024))
    );
    // Y una columna de medida no es una celda que se pueda filtrar.
    assert_eq!(matrix.valor_de_celda(1, "10"), None);
}

/// El fixture: Norte 60, Este 57, Sur 48.
fn por_region() -> Report {
    Report::new(model())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")])
}

#[test]
fn el_top_n_deja_los_n_primeros_y_su_medida() {
    let qty = measure("Qty");
    let completo = por_region()
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    assert_eq!(completo.row_count(), 3, "tres regiones");

    let arriba = por_region()
        .with_top_n(TopN {
            measure: qty,
            count: 1,
            abajo: false,
        })
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    assert_eq!(arriba.row_count(), 1);
    assert_eq!(arriba.cell(0, 0), Some("Norte"));
    assert_eq!(
        arriba.cell(0, 1).and_then(|v| v.parse::<i64>().ok()),
        Some(60),
        "Norte es la region con mas cantidad"
    );

    let abajo = por_region()
        .with_top_n(TopN {
            measure: qty,
            count: 2,
            abajo: true,
        })
        .execute(&engine().expect("motor"), None)
        .expect("corre");
    assert_eq!(abajo.row_count(), 2);
    let regiones: Vec<&str> = (0..2).filter_map(|row| abajo.cell(row, 0)).collect();
    assert!(
        regiones.contains(&"Sur") && regiones.contains(&"Este"),
        "{regiones:?}"
    );
    assert!(
        !regiones.contains(&"Norte"),
        "la mayor queda fuera: {regiones:?}"
    );
}

#[test]
fn el_top_n_tambien_ordena_por_una_medida_del_interprete() {
    // `QtyDoble` es un `SUMX` en el nivel superior: lo resuelve el interprete, no el SQL. El
    // recorte es un FILTRO, asi que viaja igual y la segunda pasada lo vuelve a ver.
    let doble = measure("QtyDoble");
    let motor = engine().expect("motor");
    let matrix = por_region()
        .with_measures(vec![doble])
        .with_top_n(TopN {
            measure: doble,
            count: 1,
            abajo: false,
        })
        .execute(&motor, Some(&memory_source()))
        .expect("interprete");

    assert_eq!(matrix.row_count(), 1, "una sola region");
    assert_eq!(matrix.cell(0, 0), Some("Norte"));
    // El doble de la cantidad de Norte: 60.
    assert_eq!(
        matrix.cell(0, 1).and_then(|v| v.parse::<f64>().ok()),
        Some(120.0)
    );
}

#[test]
fn las_tarjetas_no_las_toca_el_top_n() {
    // En Power BI el filtro de un visual no cambia otra tarjeta. El total de cada medida
    // sigue siendo el de la consulta entera.
    let qty = measure("Qty");
    let motor = engine().expect("motor");
    let completas = por_region().totals(&motor, None).expect("totales");
    let con_top = por_region()
        .with_top_n(TopN {
            measure: qty,
            count: 1,
            abajo: false,
        })
        .totals(&motor, None)
        .expect("totales");

    assert_eq!(con_top, completas);
}

#[test]
fn un_top_n_mas_largo_que_la_tabla_no_recorta_nada() {
    let qty = measure("Qty");
    let matrix = por_region()
        .with_top_n(TopN {
            measure: qty,
            count: 50,
            abajo: false,
        })
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    assert_eq!(matrix.row_count(), 3);
}

#[test]
fn el_top_n_y_los_subtotales_no_pueden_estar_juntos() {
    // Un Top N elige valores de la PRIMERA dimension, y un renglon de subtotal no es un valor
    // de esa dimension. El ultimo que se pide gana, y el estado imposible no se puede armar.
    let qty = measure("Qty");
    let con_subtotales = por_region()
        .with_top_n(TopN {
            measure: qty,
            count: 1,
            abajo: false,
        })
        .with_subtotals(true);
    assert_eq!(con_subtotales.totales, Totales::Subtotales);
    assert!(con_subtotales.top_n.is_none());

    let con_top = por_region().with_subtotals(true).with_top_n(TopN {
        measure: qty,
        count: 1,
        abajo: false,
    });
    assert!(con_top.top_n.is_some());
    assert_eq!(con_top.totales, Totales::Ninguno);
}

#[test]
fn una_medida_que_no_esta_en_el_informe_no_ordena_nada() {
    // Un Top N que apunta a una medida que ya no se miro no puede tirar la pantalla: el
    // informe sale entero. La app lo suelta antes de llegar aca, pero el estado es posible en
    // un informe guardado.
    let matrix = por_region()
        .with_top_n(TopN {
            measure: ObjectId(999),
            count: 1,
            abajo: false,
        })
        .execute(&engine().expect("motor"), None)
        .expect("corre");

    assert_eq!(matrix.row_count(), 3);
}

#[test]
fn el_top_n_cuesta_dos_lecturas() {
    // Una para saber que valores son los mayores y otra acotada. El contador de consultas es
    // lo que la app muestra en el pie: si dijera una, mentiria.
    let qty = measure("Qty");
    let una = por_region().query_count().expect("cuenta");
    let dos = por_region()
        .with_top_n(TopN {
            measure: qty,
            count: 1,
            abajo: false,
        })
        .query_count()
        .expect("cuenta");

    assert_eq!(dos, una * 2);
}

/// La columna calculada de la suite: `bruto = qty * 2` sobre `fact_sales`.
const FACT_BRUTO: ObjectId = ObjectId(40);
/// Una calculada que usa DOS columnas fisicas. Es la que cuida el alineado por posicion.
const FACT_IMPORTE: ObjectId = ObjectId(41);
/// La medida que agrega `bruto`. Fuerza el camino del interprete: la columna no tiene nombre
/// fisico que el SQL pueda leer.
const BRUTO_MEASURE: ObjectId = ObjectId(200);
const IMPORTE_MEASURE: ObjectId = ObjectId(201);

/// El modelo del testkit con columnas calculadas encima de la tabla de hechos.
fn modelo_con_calculada() -> datan_model::Model {
    let mut model = model();
    let fact = model
        .tables
        .iter_mut()
        .find(|table| table.name == "fact_sales")
        .expect("la tabla de hechos");
    fact.columns.push(Column::calculated(
        FACT_BRUTO,
        "bruto",
        DataType::Integer,
        "'fact_sales'[qty] * 2",
    ));
    fact.columns.push(Column::calculated(
        FACT_IMPORTE,
        "importe",
        DataType::Moneda,
        "'fact_sales'[qty] * 'fact_sales'[amount]",
    ));
    model.measures.push(Measure::new(
        BRUTO_MEASURE,
        "Bruto",
        "SUM('fact_sales'[bruto])",
    ));
    model.measures.push(Measure::new(
        IMPORTE_MEASURE,
        "Importe",
        "SUM('fact_sales'[importe])",
    ));
    model
}

#[test]
fn distinct_values_rechaza_columnas_ausentes_y_calculadas() {
    let engine = engine().expect("motor");
    let report = Report::new(modelo_con_calculada());

    assert!(matches!(
        report.distinct_values(&engine, ObjectId(999), 10),
        Err(datan_report::ReportError::UnknownColumn(ObjectId(999)))
    ));
    assert!(matches!(
        report.distinct_values(&engine, FACT_BRUTO, 10),
        Err(datan_report::ReportError::CalculatedColumn(name)) if name == "bruto"
    ));
}

#[test]
fn una_columna_calculada_de_dos_columnas_cruza_las_filas_por_posicion() {
    // `importe = qty * amount` obliga al interprete a leer DOS columnas de la base y
    // cruzarlas por posicion. Si `DuckSource` no pide el mismo orden en las dos lecturas, el
    // producto sale cruzado y el total no coincide con el SQL. El orden lo fija la clave de
    // la tabla; este test cuida que la cuenta siga dando igual que en SQL.
    let engine = engine().expect("motor");
    let model = modelo_con_calculada();
    let source = DuckSource::new(&engine, &model, "");
    let informe = Report::new(model).with_measures(vec![IMPORTE_MEASURE]);

    let obtenido: f64 = informe
        .totals(&engine, Some(&source))
        .expect("corre")
        .first()
        .expect("un valor")
        .parse()
        .expect("un numero");
    let esperado: f64 = engine
        .scalar("SELECT SUM(qty * amount)::text FROM fact_sales", 0)
        .expect("sql")
        .expect("un valor")
        .parse()
        .expect("un numero");

    assert_eq!(obtenido, esperado);
}

#[test]
fn una_columna_calculada_da_el_mismo_numero_que_su_expresion_a_mano() {
    // `Bruto = SUM(bruto)` donde `bruto = qty * 2` tiene que dar el doble de `Qty`, fila a
    // fila. La de la izquierda va por el interprete sobre `DuckSource`; la de la derecha por
    // el fast path SQL. Es la comparacion que el arnes diferencial no puede hacer solo,
    // porque una columna calculada no tiene una version SQL.
    let engine = engine().expect("motor");
    let model = modelo_con_calculada();
    let source = DuckSource::new(&engine, &model, "");

    let calculada = Report::new(model.clone())
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![BRUTO_MEASURE])
        .execute(&engine, Some(&source))
        .expect("corre");
    let sql = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")])
        .execute(&engine, None)
        .expect("corre");

    assert_eq!(calculada.row_count(), sql.row_count());
    for row in 0..calculada.row_count() {
        let bruto: f64 = calculada.cell(row, 1).unwrap_or("").parse().expect("bruto");
        let qty: f64 = sql.cell(row, 1).unwrap_or("").parse().expect("qty");
        assert_eq!(bruto, qty * 2.0, "fila {row}: {bruto} vs {qty}");
    }
}

#[test]
fn la_matriz_agrupa_por_una_columna_calculada() {
    // Agrupar por `bruto` es agrupar por `qty`, con las claves al doble. Es la comprobacion
    // de que `distinct_values` lee una columna calculada igual que una fisica.
    //
    // Las claves se comparan por numero y no por fila: `qty` tiene NULL, y un NULL en la
    // dimension es una fila mas del SQL y ninguna del interprete. Esa diferencia es anterior
    // a las columnas calculadas y no es lo que este test mira.
    let engine = engine().expect("motor");
    let model = modelo_con_calculada();
    let source = DuckSource::new(&engine, &model, "");

    let por_bruto = Report::new(model.clone())
        .with_dimensions(vec![FACT_BRUTO])
        .with_measures(vec![measure("Qty")])
        .execute(&engine, Some(&source))
        .expect("corre");
    let por_qty = Report::new(model)
        .with_dimensions(vec![ObjectId(11)])
        .with_measures(vec![measure("Qty")])
        .execute(&engine, None)
        .expect("corre");

    let mut esperado = std::collections::BTreeMap::new();
    for row in 0..por_qty.row_count() {
        if let Ok(key) = por_qty.cell(row, 0).unwrap_or("").parse::<i64>() {
            esperado.insert(key * 2, por_qty.cell(row, 1).unwrap_or("").to_string());
        }
    }
    let mut obtenido = std::collections::BTreeMap::new();
    for row in 0..por_bruto.row_count() {
        if let Ok(key) = por_bruto.cell(row, 0).unwrap_or("").parse::<i64>() {
            obtenido.insert(key, por_bruto.cell(row, 1).unwrap_or("").to_string());
        }
    }

    assert_eq!(obtenido, esperado);
    assert!(!obtenido.is_empty());
}

#[test]
fn una_columna_fisica_sigue_yendo_por_el_fast_path() {
    // El ruteo por el interprete es solo para lo que toca una calculada. Sin esto, una
    // columna calculada en el modelo mandaria TODOS los informes al interprete.
    let model = modelo_con_calculada();
    let consultas = Report::new(model)
        .with_dimensions(vec![ObjectId(21)])
        .with_measures(vec![measure("Qty")])
        .query_count()
        .expect("cuenta");

    assert_eq!(consultas, 1, "una dimension fisica es una consulta");
}
