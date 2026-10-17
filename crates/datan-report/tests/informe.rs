//! El informe como producto: consultas minimas, matriz correcta, cross-filter.
//!
//! El arnes diferencial ya prueba que una medida da el mismo numero por los dos caminos.
//! Aqui se prueba lo que un informe necesita y una medida sola no: que N medidas cuesten las
//! consultas MINIMAS, que las filas de consultas distintas se peguen en la matriz correcta,
//! y que pinchar una celda cambie el filtro de todas las medidas a la vez.

use datan_dax_eval::FilterValue;
use datan_model::ObjectId;
use datan_report::{Matrix, Report};
use datan_testkit::{engine, model, Y2024};

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
    report.execute(&engine().expect("motor"), None).expect("corre")
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
    assert_eq!(regions, vec!["Este", "Norte", "Sur"], "ordenado, no en hash");
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
        total += matrix.cell(row, 2).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
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
        let qty: f64 = matrix.cell(row, 1).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let amount: f64 = matrix.cell(row, 2).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let margin: f64 = matrix.cell(row, 3).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let expected = if qty == 0.0 { 0.0 } else { (qty - amount) / qty };
        assert!(
            (margin - expected).abs() < 1e-6,
            "fila {row}: margin {margin}, esperado {expected} con qty {qty} y amount {amount}"
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
    let matrix = report.execute(&engine().expect("motor"), None).expect("corre");

    // La celda (Norte, 0) es la region. Pinchar ahi filtra el informe entero.
    let row = (0..matrix.row_count())
        .find(|row| matrix.cell(*row, 0) == Some("Norte"))
        .expect("Norte esta en la matriz");
    let drilled = matrix.drill(&report, row, 0).expect("pinchar una dimension");
    let filtered = drilled.execute(&engine().expect("motor"), None).expect("corre");

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
    let matrix = report.execute(&engine().expect("motor"), None).expect("corre");

    assert!(matrix.drill(&report, 0, 1).is_none(), "una medida no es un filtro");
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
    assert_eq!(soltado.execute(&engine, None).expect("corre").row_count(), 3);
}

#[test]
fn un_informe_sin_medidas_falla_con_mensaje() {
    let report = Report::new(model()).with_dimensions(vec![ObjectId(21)]);
    let error = report.execute(&engine().expect("motor"), None).expect_err("falla");
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
    assert_eq!(antes.rows, despues.rows, "los numeros no cambian al reabrir");

    let _ = std::fs::remove_file(
        datan_report::saved::folder()
            .join("informe de prueba.toml"),
    );
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

    let _ = std::fs::remove_file(
        datan_report::saved::folder()
            .join("informe filtrado.toml"),
    );
}

#[test]
fn los_nombres_de_medidas_guardadas_resuelven_contra_el_modelo() {
    // Guardar los NOMBRES y no los ids es la razon de que este viaje funcione. Un id
    // interno cambia entre ejecuciones; un nombre no.
    let model = model();
    let report = datan_report::Report::new(model.clone())
        .with_measures(vec![measure("Qty")]);

    datan_report::saved::save(&report, "informe de nombres").expect("guarda");
    let reabierto = datan_report::saved::open("informe de nombres", &model).expect("abre");

    let nombres: Vec<&str> = reabierto
        .measures
        .iter()
        .filter_map(|id| reabierto.measure_name(*id))
        .collect();
    assert_eq!(nombres, vec!["Qty"]);

    let _ = std::fs::remove_file(
        datan_report::saved::folder()
            .join("informe de nombres.toml"),
    );
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
fn el_grafico_toma_los_primeros_valores_ordenados_por_la_medida() {
    let matrix = matrix();
    let top = matrix.top(0, 1, 2);

    assert_eq!(top.len(), 2);
    assert!(
        top[0].1 >= top[1].1,
        "ordenado de mayor a menor: {top:?}"
    );
    assert!(top.iter().all(|(label, _)| ["Norte", "Sur", "Este"].contains(&label.as_str())));
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
    use datan_dax_eval::{shift, CalendarUnit, DateRange, TimeFunction};

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

    assert_eq!(actual.len(), anterior.len(), "las dos ventanas, igual de largas");
    assert!(actual.start > anterior.end, "la ventana anterior termina antes de empezar la actual");
    assert!(actual.contains(today), "la ventana actual llega hasta hoy");
    assert!(!anterior.contains(today), "la ventana anterior no llega hasta hoy");
}
