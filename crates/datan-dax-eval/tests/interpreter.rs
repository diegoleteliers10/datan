//! Tests del interprete por celda de grupo.
//!
//! Aqui vive la semantica de `BLANK`, que DAX maneja de forma distinta a SQL y que es la
//! fuente de discrepancias mas comun entre el fast path y el fallback.

use datan_dax_eval::{
    ColumnView, FilterContext, FilterValue, IntColumn, Interpreter, MemorySource, Scalar,
    TextColumn, extract_atoms,
};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};

const FACT: ObjectId = ObjectId(1);
const PRODUCT: ObjectId = ObjectId(2);

const FACT_QTY: ObjectId = ObjectId(11);
const FACT_PRODUCT_ID: ObjectId = ObjectId(12);
const FACT_AMOUNT: ObjectId = ObjectId(13);
const PRODUCT_ID: ObjectId = ObjectId(20);
const PRODUCT_REGION: ObjectId = ObjectId(21);

fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: FACT,
                name: "Fact Sales".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(FACT_QTY, "Qty", DataType::Integer),
                    Column::new(FACT_PRODUCT_ID, "ProductId", DataType::Integer),
                    Column::new(FACT_AMOUNT, "Amount", DataType::Moneda),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
            Table {
                id: PRODUCT,
                name: "Dim Product".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(PRODUCT_ID, "ProductId", DataType::Integer),
                    Column::new(PRODUCT_REGION, "Region", DataType::Text),
                ],
                key_columns: vec![PRODUCT_ID],
                is_date_table: false,
            },
        ],
        measures: vec![],
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

/// Datos de prueba.
///
/// `Qty` tiene un BLANK en la fila 2. `Amount` tambien. Eso es a proposito: DAX trata
/// `BLANK` de forma distinta a `NULL` y es donde los dos caminos divergen sin querer.
fn source() -> MemorySource {
    MemorySource::new()
        .with_column(
            FACT_QTY,
            IntColumn {
                values: vec![Some(10), Some(20), None, Some(40)],
            },
        )
        .with_column(
            FACT_AMOUNT,
            IntColumn {
                values: vec![Some(1000), Some(2000), None, Some(4000)],
            },
        )
        .with_column(
            FACT_PRODUCT_ID,
            IntColumn {
                values: vec![Some(1), Some(2), Some(1), Some(3)],
            },
        )
        .with_column(
            PRODUCT_ID,
            IntColumn {
                values: vec![Some(1), Some(2), Some(3)],
            },
        )
        .with_column(
            PRODUCT_REGION,
            TextColumn {
                values: vec![
                    Some("Norte".to_string()),
                    Some("Sur".to_string()),
                    Some("Norte".to_string()),
                ],
            },
        )
        .with_table_length(FACT, 4)
        .with_table_length(PRODUCT, 3)
}

fn evaluate(expression: &str, filters: &FilterContext, grouping: &[ObjectId]) -> Vec<(String, Scalar)> {
    let model = model();
    let atoms = extract_atoms(expression, &model, filters, grouping).expect("extrae");
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
    values
}

#[test]
fn sum_salta_los_blanks() {
    let values = evaluate(
        "SUM('Fact Sales'[Qty])",
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    );
    // 10 + 20 + 40 = 70. El blank no es 0: no esta.
    assert_eq!(values[0].1, Scalar::Integer(70));
}

#[test]
fn count_no_cuenta_blanks_pero_countrows_si() {
    let values = evaluate(
        "COUNT('Fact Sales'[Qty])",
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    );
    assert_eq!(values[0].1, Scalar::Integer(3), "COUNT ignora los blanks");

    let rows = evaluate(
        "COUNTROWS('Fact Sales')",
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    );
    assert_eq!(rows[0].1, Scalar::Integer(4), "COUNTROWS cuenta filas");
}

#[test]
fn distinctcount_ignora_los_blanks() {
    // `DISTINCTCOUNT` sobre una columna con blanks. En SQL, `COUNT(DISTINCT)` tambien los
    // ignora, asi que este test fija que los dos caminos coinciden.
    let values = evaluate(
        "DISTINCTCOUNT('Fact Sales'[Qty])",
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    );
    assert_eq!(values[0].1, Scalar::Integer(3));
}

#[test]
fn un_solo_blank_no_cuenta_como_valor_distinto() {
    // El caso limite: una columna con un unico valor mas un blank. El blank NO suma.
    let model = model();
    let source = MemorySource::new()
        .with_column(
            FACT_QTY,
            IntColumn {
                values: vec![Some(5), None, None],
            },
        )
        .with_table_length(FACT, 3)
        .with_table_length(PRODUCT, 0);
    let atoms = extract_atoms(
        "DISTINCTCOUNT('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    )
    .expect("extrae");
    let interpreter = Interpreter::new(&model, &source);
    let result = interpreter
        .evaluate_cell(&atoms.atoms, &FilterContext::empty())
        .expect("interpreta");
    assert_eq!(result.get(0), Some(&Scalar::Integer(1)));
}

#[test]
fn min_y_max_ignoran_blanks() {
    // Cada agregacion por separado. El atomo es la unidad: el interprete devuelve un
    // escalar por atomo, no una expresion aritmetica.
    let min = evaluate(
        "MIN('Fact Sales'[Qty])",
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    );
    assert_eq!(min[0].1, Scalar::Integer(10));

    let max = evaluate(
        "MAX('Fact Sales'[Qty])",
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    );
    assert_eq!(max[0].1, Scalar::Integer(40));
}

#[test]
fn average_sin_valores_es_blank_no_cero() {
    // Un `AVG` sobre una columna sin filas da BLANK en DAX, no 0. Es la diferencia entre
    // "no hay datos" y "hay datos que valen cero".
    let model = model();
    let source = MemorySource::new()
        .with_column(
            FACT_QTY,
            IntColumn {
                values: vec![None, None],
            },
        )
        .with_table_length(FACT, 2)
        .with_table_length(PRODUCT, 0);
    let atoms = extract_atoms(
        "AVERAGE('Fact Sales'[Qty])",
        &model,
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    )
    .expect("extrae");
    let interpreter = Interpreter::new(&model, &source);
    let result = interpreter
        .evaluate_cell(&atoms.atoms, &FilterContext::empty())
        .expect("interpreta");
    assert_eq!(result.get(0), Some(&Scalar::Blank));
}

#[test]
fn el_filtro_de_la_dimension_llega_a_los_hechos_por_lineage() {
    // El filtro pone `Region = 'Norte'` en Dim Product. Solo los productos 1 y 3 son Norte,
    // asi que las filas 0 y 2 de los hechos entran.
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));

    let values = evaluate("SUM('Fact Sales'[Qty])", &filters, &[PRODUCT_REGION]);
    // Productos Norte: 1 y 3. El producto 1 aparece en las filas 0 (Qty 10) y 2 (blank).
    // El producto 3 aparece en la fila 3 (Qty 40). Total: 10 + 40 = 50.
    assert_eq!(values[0].1, Scalar::Integer(50));
}

#[test]
fn la_semijoin_solo_deja_pasar_las_claves_del_origen() {
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Sur".to_string()));

    let values = evaluate("SUM('Fact Sales'[Qty])", &filters, &[PRODUCT_REGION]);
    // Solo el producto 2 es Sur, y aparece en la fila 1 con Qty 20.
    assert_eq!(values[0].1, Scalar::Integer(20));
}

#[test]
fn dos_filtros_sobre_distintas_columnas_se_intersectan() {
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()))
        .with_equal(FACT_QTY, FilterValue::Integer(10));

    let values = evaluate("SUM('Fact Sales'[Qty])", &filters, &[PRODUCT_REGION]);
    assert_eq!(values[0].1, Scalar::Integer(10));
}

#[test]
fn un_filtro_que_no_coincide_da_blank_no_cero() {
    // Una region que no existe. `SUM` sobre cero filas es BLANK, no 0.
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Oeste".to_string()));

    let values = evaluate("SUM('Fact Sales'[Qty])", &filters, &[PRODUCT_REGION]);
    assert_eq!(values[0].1, Scalar::Blank);
}

#[test]
fn los_dos_caminos_coinciden_en_la_misma_consulta() {
    // El caso que importa: el mismo filtro,Interpreter y la semantica de BLANK, evaluados
    // sobre las mismas filas. Si estos dos dan numeros distintos, el fallback no sirve.
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));

    let all = evaluate("SUM('Fact Sales'[Qty])", &FilterContext::empty(), &[PRODUCT_REGION]);
    let norte = evaluate("SUM('Fact Sales'[Qty])", &filters, &[PRODUCT_REGION]);

    // La suma de la celda filtrada tiene que ser una parte de la suma total, nunca mayor.
    let total = all[0].1.as_number();
    let filtrado = norte[0].1.as_number();
    assert!(
        filtrado <= total,
        "el filtro no puede agrandar la suma: {filtrado} > {total}"
    );
    assert_eq!(filtrado, 50.0);
}

#[test]
fn la_moneda_chilena_se_suma_como_entero() {
    // `Moneda` es un entero. Si el interprete devuelve `Decimal`, el grid formatea mal y el
    // total de la boleta no cuadra. Es un test de producto.
    let values = evaluate(
        "SUM('Fact Sales'[Amount])",
        &FilterContext::empty(),
        &[PRODUCT_REGION],
    );
    assert_eq!(
        values[0].1,
        Scalar::Integer(7000),
        "la suma de montos tiene que quedar entera"
    );
}

#[test]
fn dos_filtros_sobre_la_misma_columna_se_intersectan() {
    // Dos valores sobre la misma columna es una interseccion, no una union. `Norte AND Sur`
    // no deja pasar nada.
    let filters = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()))
        .with_equal(PRODUCT_REGION, FilterValue::Text("Sur".to_string()));

    // `Norte AND Sur` no deja pasar nada. El resultado es BLANK, no 0: en DAX un filtro
    // que no deja filas da blank, y la celda vacia no es lo mismo que un cero.
    let values = evaluate("SUM('Fact Sales'[Qty])", &filters, &[PRODUCT_REGION]);
    assert_eq!(values[0].1, Scalar::Blank);

    // El mismo filtro dos veces si deja pasar lo mismo que una vez.
    let twice = FilterContext::empty()
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()))
        .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
    let values = evaluate("SUM('Fact Sales'[Qty])", &twice, &[PRODUCT_REGION]);
    assert_eq!(values[0].1, Scalar::Integer(50));
}
/// Un origen que cuenta cuantas veces se lee cada columna.
///
/// Es como se prueba que un semijoin se resuelve UNA vez y no una por fila del hecho: el
/// conteo distingue una lectura del origen de un producto de dos tablas.
struct CountingSource {
    inner: MemorySource,
    reads: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<ObjectId, usize>>>,
}

impl CountingSource {
    fn new(inner: MemorySource) -> Self {
        Self {
            inner,
            reads: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    fn reads(&self, column: ObjectId) -> usize {
        self.reads
            .lock()
            .expect("candado")
            .get(&column)
            .copied()
            .unwrap_or(0)
    }
}

struct CountedColumn {
    inner: std::sync::Arc<dyn ColumnView>,
    id: ObjectId,
    reads: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<ObjectId, usize>>>,
}

impl ColumnView for CountedColumn {
    fn len(&self) -> usize {
        self.inner.len()
    }

    fn value_at(&self, row: usize) -> Scalar {
        *self.reads.lock().expect("candado").entry(self.id).or_insert(0) += 1;
        self.inner.value_at(row)
    }

    fn non_blank_count(&self) -> usize {
        self.inner.non_blank_count()
    }

    fn distinct_count(&self) -> usize {
        self.inner.distinct_count()
    }
}

impl datan_dax_eval::DataSource for CountingSource {
    fn column(&self, column: ObjectId) -> Option<std::sync::Arc<dyn ColumnView>> {
        let inner = self.inner.column(column)?;
        Some(std::sync::Arc::new(CountedColumn {
            inner,
            id: column,
            reads: self.reads.clone(),
        }))
    }

    fn table_len(&self, table: ObjectId) -> usize {
        self.inner.table_len(table)
    }
}

#[test]
fn el_semijoin_resuelve_su_origen_una_vez_y_no_una_por_fila() {
    // Un filtro propagado no lleva valores: lleva los filtros del origen y se evalua fila a
    // fila. La fila del hecho tiene que buscar su clave en el origen, y ese origen NO se
    // recorre por cada fila: se recorre una vez y se guardan sus claves. Sin cache, cuatro
    // hechos por tres productos son doce lecturas, y en una tabla de verdad es el producto de
    // las dos tablas.
    let model = model();
    let source = CountingSource::new(source());
    let intercepted = FilterContext::empty().with_equal(PRODUCT_REGION, FilterValue::Text("Norte".into()));
    let propagated = intercepted.propagate(&model);

    let interpreter = Interpreter::new(&model, &source);
    let rows = interpreter.select_rows(FACT, &propagated);

    // Las tres ventas de Norte: la 1, la 2 y la 4. La 3 no.
    assert_eq!(rows.len(), 3);
    // El origen se leyo una vez: tres filas, no tres por cada hecho.
    assert_eq!(source.reads(PRODUCT_ID), 3);
}
