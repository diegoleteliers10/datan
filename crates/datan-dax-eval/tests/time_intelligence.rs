//! Time intelligence sobre el pipeline real, no sobre el calendario aislado.
//!
//! `src/time.rs` prueba que las funciones de calendario dan el rango correcto. Esto prueba
//! que el rango llega a ser filtro, que el filtro viaja por las relationships, y que un
//! modelo sin tabla de fechas falla en vez de devolver un total.

use datan_dax_eval::{
    CalendarUnit, DateRange, FilterOperator, TimeError, TimeFunction, date_column, filters,
    has_bidirectional_cycle,
};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

const DATE_TABLE: ObjectId = ObjectId(1);
const FACT: ObjectId = ObjectId(2);

const DATE: ObjectId = ObjectId(11);
const DATE_YEAR: ObjectId = ObjectId(12);
const FACT_DATE: ObjectId = ObjectId(21);
const FACT_AMOUNT: ObjectId = ObjectId(22);

/// 2024-01-01 es el dia 19723 desde 1970-01-01.
const Y2024: i32 = 19723;

fn at(offset: i32) -> i32 {
    Y2024 + offset
}

fn column(id: ObjectId, name: &str, data_type: DataType) -> Column {
    Column::new(id, name, data_type)
}

/// `Dim Date` marcada, relacionada con los hechos. El modelo minimo que hace falta para que
/// `PREVIOUSYEAR` signifique algo.
fn model_with_date_table() -> Model {
    Model {
        tables: vec![
            Table {
                id: DATE_TABLE,
                name: "Dim Date".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    column(DATE, "Date", DataType::Date),
                    column(DATE_YEAR, "Year", DataType::Integer),
                ],
                key_columns: vec![DATE],
                is_date_table: true,
            },
            Table {
                id: FACT,
                name: "Fact Sales".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    column(FACT_DATE, "Date", DataType::Date),
                    column(FACT_AMOUNT, "Amount", DataType::Integer),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
        ],
        measures: vec![],
        relationships: vec![Relationship {
            from_column: DATE,
            to_column: FACT_DATE,
            cardinality: Cardinality::OneToMany,
            direction: FilterDirection::Single,
            role: RelationshipRole::Active,
            assume_referential_integrity: false,
        }],
    }
}

fn model_without_date_table() -> Model {
    let mut model = model_with_date_table();
    model.tables[0].is_date_table = false;
    model
}

/// El rango actual: marzo de 2024, del 1 al 31.
fn march_2024() -> DateRange {
    DateRange::new(at(60), at(90))
}

fn previous_year() -> TimeFunction {
    TimeFunction::PreviousYear
}

#[test]
fn previousyear_produce_dos_filtros_de_rango_sobre_la_columna_de_fecha() {
    let model = model_with_date_table();
    // "Hoy" es 15 de marzo de 2024, asi que PREVIOUSYEAR es todo 2023.
    let today = at(74);

    let context = filters(&model, previous_year(), march_2024(), today)
        .expect("el modelo tiene tabla de fechas");

    let bounds = context.filters_on(DATE);
    assert_eq!(bounds.len(), 2, "un rango son dos limites: >= y <=");

    let operators: Vec<FilterOperator> = bounds.iter().map(|f| f.operator).collect();
    assert!(
        operators.contains(&FilterOperator::GreaterOrEqual),
        "falta el limite inferior: {operators:?}"
    );
    assert!(
        operators.contains(&FilterOperator::LessOrEqual),
        "falta el limite superior: {operators:?}"
    );
}

#[test]
fn el_rango_de_previousyear_cubre_el_ano_entero_anterior() {
    let model = model_with_date_table();
    let today = at(74);
    let context = filters(&model, previous_year(), march_2024(), today).expect("ok");

    let rendered: Vec<i32> = context
        .filters_on(DATE)
        .iter()
        .filter_map(|f| match f.values().first() {
            Some(datan_dax_eval::FilterValue::Date(day)) => Some(*day),
            _ => None,
        })
        .collect();

    assert_eq!(rendered.len(), 2);
    // 2023 tiene 365 dias, asi que 2023-01-01 es `at(-365)` y no `at(-366)`: 2024 es el
    // ano bisiesto. Confundir los dos desplaza todo el rango un dia.
    assert_eq!(at(-365), 19358, "2023-01-01");
    assert!(rendered.contains(&at(-365)), "falta el 1 de enero de 2023");
    assert!(rendered.contains(&at(-1)), "falta el 31 de diciembre de 2023");
}

#[test]
fn el_rango_de_fechas_viaja_a_los_hechos_por_la_relationship() {
    let model = model_with_date_table();
    let today = at(74);
    let context = filters(&model, previous_year(), march_2024(), today).expect("ok");

    let propagated = datan_dax_eval::propagate_ordered(&model, &context, &[0]);

    assert!(
        !propagated.filters_on(FACT_DATE).is_empty(),
        "el rango de fechas tiene que llegar a los hechos; si no, PREVIOUSYEAR"
    );
    assert!(
        propagated.filters_on(FACT_DATE).iter().all(|f| f.is_propagated()),
        "los filtros de los hechos deben venir de la propagacion, no del rango directo"
    );
}

#[test]
fn dateadd_mes_conserva_la_longitud_del_rango() {
    // Este es el contraste con `PREVIOUSYEAR`: `DATEADD` mueve la ventana, `PREVIOUSYEAR`
    // recorta al ano. Sin los dos, un acumulado mensual y uno anual darian el mismo numero.
    let model = model_with_date_table();
    let today = at(74);
    let march = march_2024();

    let dateadd = filters(
        &model,
        TimeFunction::DateAdd {
            units: -1,
            unit: CalendarUnit::Month,
        },
        march,
        today,
    )
    .expect("ok");
    let previous = filters(&model, previous_year(), march, today).expect("ok");

    let width = |context: &datan_dax_eval::FilterContext| {
        context
            .filters_on(DATE)
            .iter()
            .filter_map(|f| match (f.operator, f.values().first()) {
                (FilterOperator::GreaterOrEqual, Some(datan_dax_eval::FilterValue::Date(d))) => {
                    Some(*d)
                }
                _ => None,
            })
            .next()
            .expect("limite inferior")
    };

    // El 1 de marzo menos un mes es el 1 de febrero, no el 29 de febrero: `DATEADD`
    // desplaza la ventana mes a mes, no retrocede un numero de dias.
    assert_eq!(
        width(&dateadd),
        at(31),
        "el 1 de marzo menos un mes es el 1 de febrero de 2024"
    );
    assert!(
        width(&previous) < width(&dateadd),
        "PREVIOUSYEAR tiene que empezar antes: recorre el ano entero"
    );
}

#[test]
fn un_rango_vacio_filtra_en_vez_de_no_filtrar() {
    // El fallo silencioso clasico: si un rango vacio se modela como "sin filtro", la
    // celda devuelve el total en vez de BLANK, y nadie se entera hasta que alguien mira el
    // informe y pregunta por que enero de 2023 tiene la misma venta que diciembre.
    let model = model_with_date_table();
    // "Hoy" en 1970: PREVIOUSYEAR cae en 1969, antes de que la columna tenga filas.
    let today = 10;

    let context = filters(&model, previous_year(), DateRange::new(today, today), today)
        .expect("el modelo tiene tabla de fechas");

    assert!(
        !context.filters_on(DATE).is_empty(),
        "un rango vacio debe llevar un filtro imposible, no ninguno"
    );
}

#[test]
fn un_modelo_sin_tabla_de_fechas_falla_con_mensaje() {
    // DAX no exige la tabla de fechas y da numeros silenciosos sin ella. Datan la exige: el
    // error tiene que decir que falta y por que, no devolver un cero que parece un dato.
    let model = model_without_date_table();
    let today = at(74);

    let error = filters(&model, previous_year(), march_2024(), today)
        .expect_err("sin tabla de fechas no hay time intelligence");

    assert_eq!(error, TimeError::NoDateTable);
    let message = error.to_string();
    assert!(
        message.contains("tabla de fechas"),
        "el mensaje debe nombrar lo que falta: {message}"
    );
}

#[test]
fn la_columna_de_fechas_es_la_clave_no_cualquier_columna_de_fecha() {
    // Si se eligiera `Year` o `Month` como columna del rango, dos dias del mismo mes
    // quedarian indistinguibles y el filtro devolveria filas que el rango dice excluir.
    let model = model_with_date_table();
    assert_eq!(date_column(&model).expect("el modelo tiene tabla de fechas"), DATE);
}

#[test]
fn una_columna_timestamp_tambien_sirve_de_fecha() {
    // Postgres devuelve `timestamp` para sus fechas y datan-connect lo mapea a `DateTime`.
    // Exigir `Date` puro dejaba a una base de timestamps sin periodo, sin filtro de fecha y
    // sin time intelligence: el motor trabaja en dias, y un `timestamp` es un dia con hora.
    let mut model = model_with_date_table();
    model.tables[0].columns[0].data_type = DataType::DateTime;
    assert_eq!(
        date_column(&model).expect("un timestamp es una fecha para el motor"),
        DATE
    );
}

#[test]
fn el_time_intelligence_no_exige_un_ciclo() {
    // Contraste con `tests/cycles.rs`: el rechazo del fast path es por el ciclo, no por la
    // tabla de fechas. Un star schema con tabla de fechas tiene que pasar.
    let model = model_with_date_table();
    assert!(
        !has_bidirectional_cycle(&model),
        "el star schema de las pruebas de tiempo intelligence no tiene ciclo"
    );
}