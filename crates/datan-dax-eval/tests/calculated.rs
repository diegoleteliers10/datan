//! Columnas calculadas: una expresion DAX evaluada fila a fila sobre su tabla.
//!
//! El `CalculatedSource` es lo que hace que una columna calculada se comporte como una
//! fisica en todo el interprete. Estos tests fijan los tres usos que decide una matriz:
//! agrupar por ella, agregarla y filtrarla. Y el cuarto, que es una columna calculada que
//! referencia a otra.

use datan_dax_eval::{
    CalculatedSource, DataSource, DateColumn, FilterContext, FilterValue, IntColumn, Interpreter,
    MemorySource, Scalar, TextColumn, extract_atoms,
};
use datan_model::{Column, DataType, Model, ObjectId, Table, TableKind};

const FACT: ObjectId = ObjectId(1);
const QTY: ObjectId = ObjectId(10);
const PRICE: ObjectId = ObjectId(11);
const DAY: ObjectId = ObjectId(12);
/// `qty * price`.
const TOTAL: ObjectId = ObjectId(13);
/// `total * 2`, una calculada sobre otra calculada.
const DOBLE: ObjectId = ObjectId(14);
/// `YEAR(day)`: la jerarquia de fecha mas comun.
const ANIO: ObjectId = ObjectId(15);
/// `IF(qty > 2, 'G', 'P')`: una calculada de texto.
const TAMANO: ObjectId = ObjectId(16);

const QTY_MEASURE: ObjectId = ObjectId(100);
const TOTAL_MEASURE: ObjectId = ObjectId(101);

/// Los dos extremos, en dias desde 1970: 2023-01-15 y 2024-11-20.
const D2023: i32 = 19372;
const D2024: i32 = 20046;

fn model() -> Model {
    Model {
        tables: vec![Table {
            id: FACT,
            name: "fact_sales".to_string(),
            kind: TableKind::Facts,
            columns: vec![
                Column::new(QTY, "qty", DataType::Integer),
                Column::new(PRICE, "price", DataType::Integer),
                Column::new(DAY, "day", DataType::Date),
                Column::calculated(TOTAL, "total", DataType::Integer, "qty * price"),
                Column::calculated(DOBLE, "doble", DataType::Integer, "total * 2"),
                Column::calculated(ANIO, "anio", DataType::Integer, "YEAR(day)"),
                Column::calculated(TAMANO, "tamano", DataType::Text, "IF(qty > 2, 'G', 'P')"),
            ],
            key_columns: vec![],
            is_date_table: false,
        }],
        measures: vec![
            datan_model::Measure::new(QTY_MEASURE, "Ventas", "SUM('fact_sales'[qty])"),
            datan_model::Measure::new(TOTAL_MEASURE, "TotalBruto", "SUM('fact_sales'[total])"),
        ],
        relationships: vec![],
    }
}

/// Cinco filas. `qty` de 1 a 5, `price` constante, y dias repartidos entre 2023 y 2024.
fn source() -> MemorySource {
    MemorySource::new()
        .with_column(
            QTY,
            IntColumn {
                values: vec![Some(1), Some(2), Some(3), Some(4), Some(5)],
            },
        )
        .with_column(
            PRICE,
            IntColumn {
                values: vec![Some(10); 5],
            },
        )
        .with_column(
            DAY,
            DateColumn {
                values: vec![Some(D2023), Some(19358), Some(19723), Some(D2024), Some(19722)],
            },
        )
        .with_table_length(FACT, 5)
}

/// El texto de un escalar, la misma forma que el informe.
fn text(value: &Scalar) -> String {
    match value {
        Scalar::Blank => String::new(),
        Scalar::Integer(n) => n.to_string(),
        Scalar::Decimal(n) => n.to_string(),
        Scalar::Text(t) => t.clone(),
        Scalar::Boolean(b) => b.to_string(),
        Scalar::Date(d) => d.to_string(),
    }
}

/// Los valores de una columna calculada, materializada.
fn columna(model: &Model, source: &MemorySource, column: ObjectId) -> Vec<String> {
    let calculated = CalculatedSource::new(model, source);
    let view = calculated.column(column).expect("la columna calculada");
    let _ = calculated;
    (0..view.len()).map(|row| text(&view.value_at(row))).collect()
}

#[test]
fn una_columna_calculada_se_evalua_fila_a_fila() {
    let model = model();
    let source = source();
    assert_eq!(
        columna(&model, &source, TOTAL),
        vec!["10", "20", "30", "40", "50"]
    );
}

#[test]
fn una_columna_calculada_puede_usar_otra() {
    // `doble = total * 2` obliga a calcular `total` primero, por la misma capa.
    let model = model();
    let source = source();
    assert_eq!(
        columna(&model, &source, DOBLE),
        vec!["20", "40", "60", "80", "100"]
    );
}

#[test]
fn una_parte_de_fecha_se_deriva_en_el_contexto_de_fila() {
    let model = model();
    let source = source();
    assert_eq!(
        columna(&model, &source, ANIO),
        vec!["2023", "2023", "2024", "2024", "2023"]
    );
}

#[test]
fn una_condicional_se_evalua_fila_a_fila() {
    let model = model();
    let source = source();
    assert_eq!(
        columna(&model, &source, TAMANO),
        vec!["P", "P", "G", "G", "G"]
    );
}

#[test]
fn el_interprete_agrega_sobre_una_columna_calculada() {
    // `SUM('fact_sales'[total])` con `total` calculada. Sin el `CalculatedSource` la columna
    // no existe como fisica y la medida falla.
    let model = model();
    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    let interpreter = Interpreter::new(&model, &calculated);

    assert_eq!(columna_por_medida(&interpreter, &model, TOTAL_MEASURE), "150");
    assert_eq!(columna_por_medida(&interpreter, &model, QTY_MEASURE), "15");
}

#[test]
fn la_matriz_agrupa_por_una_columna_calculada() {
    let model = model();
    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    let interpreter = Interpreter::new(&model, &calculated);

    let atoms = extract_atoms("[Ventas]", &model, &FilterContext::empty(), &[ANIO]).expect("extrae");
    let grouped = interpreter
        .evaluate_grouped(
            &atoms.atoms,
            &atoms.outputs,
            &FilterContext::empty(),
            &[ANIO],
            1000,
        )
        .expect("agrupa");

    let filas: Vec<Vec<String>> = grouped
        .rows
        .iter()
        .map(|row| row.iter().map(text).collect())
        .collect();
    // 2023: 1 + 2 + 5. 2024: 3 + 4.
    assert_eq!(filas, vec![vec!["2023", "8"], vec!["2024", "7"]]);
}

#[test]
fn la_matriz_agrupa_por_una_calculada_de_texto() {
    let model = model();
    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    let interpreter = Interpreter::new(&model, &calculated);

    let atoms =
        extract_atoms("[Ventas]", &model, &FilterContext::empty(), &[TAMANO]).expect("extrae");
    let grouped = interpreter
        .evaluate_grouped(
            &atoms.atoms,
            &atoms.outputs,
            &FilterContext::empty(),
            &[TAMANO],
            1000,
        )
        .expect("agrupa");

    let filas: Vec<Vec<String>> = grouped
        .rows
        .iter()
        .map(|row| row.iter().map(text).collect())
        .collect();
    // G: 3 + 4 + 5. P: 1 + 2.
    assert_eq!(filas, vec![vec!["G", "12"], vec!["P", "3"]]);
}

#[test]
fn un_filtro_sobre_una_columna_calculada_recorta_las_filas() {
    let model = model();
    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    let interpreter = Interpreter::new(&model, &calculated);

    // `total = 30` es la fila de `qty = 3`.
    let filters = FilterContext::empty().with_equal(TOTAL, FilterValue::Integer(30));
    let atoms = extract_atoms("[Ventas]", &model, &filters, &[TAMANO]).expect("extrae");
    let grouped = interpreter
        .evaluate_grouped(&atoms.atoms, &atoms.outputs, &filters, &[TAMANO], 1000)
        .expect("agrupa");

    let filas: Vec<Vec<String>> = grouped
        .rows
        .iter()
        .map(|row| row.iter().map(text).collect())
        .collect();
    assert_eq!(filas, vec![vec!["G", "3"]]);
}

#[test]
fn un_ciclo_entre_calculadas_no_cuelga() {
    // `a` vale `b` y `b` vale `a`. Sin el guardia de reentrada esto desborda la pila. Lo
    // correcto es no tener valor: `None`, no un numero inventado.
    let mut model = model();
    model.tables[0].columns.push(Column::calculated(
        ObjectId(17),
        "a",
        DataType::Integer,
        "b",
    ));
    model.tables[0].columns.push(Column::calculated(
        ObjectId(18),
        "b",
        DataType::Integer,
        "a",
    ));

    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    assert!(calculated.column(ObjectId(17)).is_none());
}

#[test]
fn una_calculada_que_usa_una_medida_no_da_valor() {
    // Una medida no tiene fila actual. El chequeo lo avisa, y el valor no se puede calcular:
    // `None`, no una lista en blanco que parece un dato que falta.
    let mut model = model();
    model.tables[0].columns.push(Column::calculated(
        ObjectId(19),
        "rota",
        DataType::Integer,
        "[Ventas] + 1",
    ));

    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    assert!(calculated.column(ObjectId(19)).is_none());
}

#[test]
fn una_columna_fisica_pasa_derecho() {
    let model = model();
    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    let view = calculated.column(QTY).expect("la columna fisica");
    assert_eq!(view.value_at(0), Scalar::Integer(1));
    assert_eq!(calculated.table_len(FACT), 5);
}

/// El valor de una medida del modelo, por el interprete.
fn columna_por_medida(interpreter: &Interpreter<'_>, model: &Model, measure: ObjectId) -> String {
    let definition = model
        .measures
        .iter()
        .find(|m| m.id == measure)
        .expect("la medida");
    let atoms =
        extract_atoms(&definition.expression, model, &FilterContext::empty(), &[]).expect("extrae");
    let values = interpreter
        .evaluate_totals(&atoms.atoms, &atoms.outputs, &FilterContext::empty())
        .expect("evalua");
    values.first().map(text).unwrap_or_default()
}

#[test]
fn el_texto_de_una_fecha_es_el_dia() {
    // Guarda contra un cambio de `Scalar::Date`: el texto de una fecha calculada es su dia
    // desde 1970, que es lo que lee el grid. Una fecha que se formateara distinto rompe el
    // filtro de fecha al pincharla.
    let model = model();
    let source = source();
    let calculated = CalculatedSource::new(&model, &source);
    let view = calculated.column(DAY).expect("la columna");
    assert_eq!(view.value_at(0), Scalar::Date(D2023));
    let _ = TextColumn {
        values: vec![Some("x".to_string())],
    };
}
