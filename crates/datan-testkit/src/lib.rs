//! Suite de conformidad y runner diferencial de DAX.
//!
//! Dev-only. Este crate responde una sola pregunta: **¿el fast path SQL y el interprete por
//! celda de grupo dan los mismos numeros?**
//!
//! # Por que compararlos con el motor y no entre ellos
//!
//! Si los dos Paths devuelven `None` porque no saben leer el tipo, los dos "coinciden" y la
//! suite pasa sin haber comprobado nada. Por eso los dos resultados se vuelven a meter en
//! DuckDB y se comparan con `EXCEPT`, que usa la semantica del motor. Un resultado vacio
//! no es un resultado.
//!
//! # Por que el harness va antes que el motor
//!
//! El plan lo dice: si el harness se construye en la semana 4, se pasa un mes
//! preguntandose por que Import y el fallback discrepan en el manejo de blanks. Si se
//! construye el dia 1, la divergencia aparece el dia 2.

use datan_dax_eval::{
    AtomKind, AtomSet, CalendarUnit, DateColumn, DateRange, FilterContext,
    FilterValue, IntColumn, Interpreter, MemorySource, Scalar, TextColumn, TimeFunction,
    extract_atoms, filters as time_filters,
};
use datan_dax_sql::{SourceMap, lower};
use datan_model::{
    Cardinality, Column, DataType, FilterDirection, Measure, Model, ObjectId, Relationship,
    RelationshipRole, Table, TableKind,
};
use datan_storage::Engine;

/// El resultado de correr una consulta por los dos caminos.
#[derive(Debug)]
pub struct PathResult {
    pub label: String,
    pub sql: Option<String>,
    pub sql_value: Option<String>,
    pub interp_value: Option<Scalar>,
    /// La matriz completa por cada camino: agrupamiento primero, medidas despues.
    ///
    /// Es la comparacion que vale. La de una sola linea solo dice algo para `SUM`.
    pub sql_rows: Option<Vec<Vec<String>>>,
    pub interp_rows: Vec<Vec<String>>,
    /// Los dos caminos existen y dicen lo mismo.
    pub agrees: bool,
    /// El atomo cae al interprete y no tiene fast path. NO es un fallo.
    pub interpreter_only: bool,
    /// El caso no se pudo correr. Un caso que falla NO es un caso que no cuenta.
    pub error: Option<String>,
}

/// Un caso de la suite: un filtro y una medida.
pub struct Case {
    pub name: &'static str,
    pub measure: &'static str,
    pub filters: Vec<(&'static str, &'static str)>,
    /// Una funcion de time intelligence y el rango sobre el que se aplica.
    ///
    /// Los tres campos van juntos porque no significan nada separados: `PREVIOUSYEAR` sin
    /// un "hoy" es ambiguo, y un rango sin una funcion es un filtro comun.
    pub time: Option<(TimeFunction, DateRange, i32)>,
    pub expected: Option<Scalar>,
}

impl Case {
    pub fn new(name: &'static str, measure: &'static str) -> Self {
        Self {
            name,
            measure,
            filters: Vec::new(),
            time: None,
            expected: None,
        }
    }

    pub fn filter(mut self, column: &'static str, value: &'static str) -> Self {
        self.filters.push((column, value));
        self
    }

    /// Aplica una funcion de time intelligence sobre `current`, con "hoy" en `today`.
    pub fn time(mut self, function: TimeFunction, current: DateRange, today: i32) -> Self {
        self.time = Some((function, current, today));
        self
    }

    pub fn expecting(mut self, value: Scalar) -> Self {
        self.expected = Some(value);
        self
    }
}

// ---------------------------------------------------------------------------------------------
// El modelo de prueba
// ---------------------------------------------------------------------------------------------

pub const FACT: ObjectId = ObjectId(1);
pub const PRODUCT: ObjectId = ObjectId(2);
pub const DATE_TABLE: ObjectId = ObjectId(3);

pub const FACT_QTY: ObjectId = ObjectId(11);
pub const FACT_PRODUCT_ID: ObjectId = ObjectId(12);
pub const FACT_AMOUNT: ObjectId = ObjectId(13);
pub const FACT_DAY: ObjectId = ObjectId(14);
pub const PRODUCT_ID: ObjectId = ObjectId(20);
pub const PRODUCT_REGION: ObjectId = ObjectId(21);
pub const PRODUCT_CATEGORY: ObjectId = ObjectId(22);
/// La columna por la que `sort_by` ordena `region`. No es la clave de nada: es un dato.
pub const PRODUCT_REGION_ORDER: ObjectId = ObjectId(23);
pub const DATE_DAY: ObjectId = ObjectId(30);

/// 2023-01-01 y 2024-01-01, en dias desde 1970. Los dos extremos que la suite usa de
/// "hoy" para que `PREVIOUSYEAR` y `DATESYTD` tengan algo que comparar.
pub const Y2023: i32 = 19358;
pub const Y2024: i32 = 19723;

/// El modelo de la suite. Nombres sin espacios, para que el mapeo fisico sea el del modelo.
pub fn model() -> Model {
    Model {
        tables: vec![
            Table {
                id: FACT,
                name: "fact_sales".to_string(),
                kind: TableKind::Facts,
                columns: vec![
                    Column::new(FACT_QTY, "qty", DataType::Integer),
                    Column::new(FACT_PRODUCT_ID, "product_id", DataType::Integer),
                    Column::new(FACT_AMOUNT, "amount", DataType::Moneda),
                    Column::new(FACT_DAY, "day", DataType::Date),
                ],
                key_columns: vec![],
                is_date_table: false,
            },
            Table {
                id: DATE_TABLE,
                name: "dim_date".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(DATE_DAY, "day", DataType::Date),
                    // Una columna de fecha que NO es la clave. Si el time intelligence la
                    // eligiera por tipo en vez de por clave, el rango devolveria filas que
                    // dice excluir.
                    Column::new(ObjectId(31), "day_of_week", DataType::Date),
                ],
                key_columns: vec![DATE_DAY],
                is_date_table: true,
            },
            Table {
                id: PRODUCT,
                name: "dim_product".to_string(),
                kind: TableKind::Dimension,
                columns: vec![
                    Column::new(PRODUCT_ID, "product_id", DataType::Integer),
                    Column::new(PRODUCT_REGION, "region", DataType::Text),
                    Column::new(PRODUCT_REGION_ORDER, "region_orden", DataType::Integer),
                    Column::new(PRODUCT_CATEGORY, "category", DataType::Text),
                ],
                key_columns: vec![PRODUCT_ID],
                is_date_table: false,
            },
        ],
        measures: vec![
            Measure::new(ObjectId(100), "Qty", "SUM('fact_sales'[qty])"),
            Measure::new(ObjectId(101), "Amount", "SUM('fact_sales'[amount])"),
            // Arimetrica ENTRE medidas. Referencia `[Qty]` dos veces a proposito: una hoja
            // repetida tiene que ser un solo atomo, y la division tiene que salir en el
            // mismo SELECT.
            Measure::new(
                ObjectId(103),
                "Margin",
                "([Qty] - [Amount]) / [Qty]",
            ),
            // Una funcion escalar en el nivel SUPERIOR de la medida. `DIVIDE` es la forma en
            // que se escriben las medidas de negocio, y es la que decide si una fila sin
            // datos da BLANK o una division por cero. Sin forma propia, el conjunto quedaba
            // con dos hojas y ninguna salida, y el informe pegaba la primera en la columna de
            // la medida: el valor de `[Qty]`, sin error y sin aviso.
            Measure::new(ObjectId(104), "Ratio", "DIVIDE([Qty], [Amount])"),
            Measure::new(ObjectId(105), "Condicional", "IF([Qty] > 50, [Qty], 0)"),
            // Un `CALCULATE` en el nivel superior, junto a la medida pelada. Es la forma
            // exacta de una variacion contra el total: el mismo `SUM(qty)` dos veces, con
            // distinto CONTEXTO. Sin forma propia y con la celda indexada por etiqueta, la
            // segunda hoja pisaba a la primera y la resta daba CERO.
            Measure::new(
                ObjectId(106),
                "Variacion",
                "[Qty] - CALCULATE([Qty], ALL('dim_product'))",
            ),
            // Un iterador en el nivel superior. Cambia el grano y cae al interprete, y aun
            // asi es UNA hoja: la agregacion sobre la expresion.
            Measure::new(
                ObjectId(107),
                "QtyDoble",
                "SUMX('fact_sales', 'fact_sales'[qty] * 2)",
            ),
            // Una funcion que NINGUNO de los dos caminos sabe calcular. No tiene forma, y
            // el informe tiene que decirlo en vez de pegar la primera hoja en su columna.
            Measure::new(ObjectId(108), "Cruda", "ABS([Qty])"),
        ],
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
                from_column: DATE_DAY,
                to_column: FACT_DAY,
                cardinality: Cardinality::OneToMany,
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            },
        ],
    }
}

/// Los mismos datos que el spike, en memoria y en DuckDB.
///
/// Que sean los mismos es lo que hace valida la comparacion. Si el interprete lee una cosa
/// y DuckDB otra, la divergencia no dice nada.
pub const FIXTURES: &str = "\
    CREATE TABLE dim_product AS
    SELECT
        i AS product_id,
        CASE i % 3 WHEN 0 THEN 'Norte' WHEN 1 THEN 'Sur' ELSE 'Este' END AS region,
        -- El orden REAL de las regiones, distinto del alfabetico. Es la columna por la que
        -- `sort_by` ordena `region`: Norte, Sur y Este en vez de Este, Norte y Sur.
        CASE i % 3 WHEN 0 THEN 1 WHEN 1 THEN 2 ELSE 3 END AS region_orden,
        CASE i % 2 WHEN 0 THEN 'A' ELSE 'B' END AS category
    FROM range(1, 7) t(i);

    CREATE TABLE fact_sales AS
    SELECT
        i AS sale_id,
        ((i % 6) + 1) AS product_id,
        CASE WHEN i % 7 = 0 THEN NULL ELSE (i % 9) + 1 END AS qty,
        CASE WHEN i % 11 = 0 THEN NULL ELSE (i % 5) * 1000 END AS amount,
        -- Las ventas se reparten entre 2023 y 2024. Un reparto dentro de un solo ano
        -- haria que PREVIOUSYEAR y DATESYTD dieran siempre la misma respuesta, y un test
        -- que no puede fallar no prueba nada.
        DATE '2023-01-01' + CAST((i * 37) % 730 AS INTEGER) AS day
    FROM range(1, 41) t(i);

    CREATE TABLE dim_date AS
    SELECT DATE '2023-01-01' + CAST(i AS INTEGER) AS day,
           DATE '2023-01-01' + CAST(i AS INTEGER) AS day_of_week
    FROM range(0, 730) t(i);";

/// Los datos en memoria, con la misma forma que `FIXTURES`.
pub fn memory_source() -> MemorySource {
    let mut qty = Vec::new();
    let mut amount = Vec::new();
    let mut product_ids = Vec::new();
    let mut day = Vec::new();
    for i in 1..=40 {
        let sale_id = i;
        product_ids.push(Some(((i % 6) + 1) as i64));
        // El MISMO reparto que el SQL: `(i * 37) % 730` dias desde 2023-01-01. Si los dos
        // caminos leen distinto, la divergencia no dice nada.
        day.push(Some(Y2023 + (((i * 37) % 730) as i32)));
        qty.push(if i % 7 == 0 {
            None
        } else {
            Some((i % 9) + 1)
        });
        amount.push(if i % 11 == 0 {
            None
        } else {
            Some(((i % 5) * 1000) as i64)
        });
        let _ = sale_id;
    }

    let mut product_id = Vec::new();
    let mut region = Vec::new();
    let mut region_orden = Vec::new();
    let mut category = Vec::new();
    for i in 1..=6 {
        product_id.push(Some(i as i64));
        region.push(Some(
            match i % 3 {
                0 => "Norte",
                1 => "Sur",
                _ => "Este",
            }
            .to_string(),
        ));
        // El MISMO orden que el SQL. Si difirieran, `sort_by` ordenaria distinto en cada
        // camino y el arnes no podria decir quien miente.
        region_orden.push(Some(match i % 3 {
            0 => 1,
            1 => 2,
            _ => 3,
        }));
        category.push(Some(if i % 2 == 0 { "A" } else { "B" }.to_string()));
    }

    let mut date_days = Vec::new();
    let mut date_weekdays = Vec::new();
    for i in 0..730i32 {
        date_days.push(Some(Y2023 + i));
        date_weekdays.push(Some(Y2023 + i));
    }

    MemorySource::new()
        .with_column(FACT_QTY, IntColumn { values: qty })
        .with_column(FACT_DAY, DateColumn { values: day })
        .with_column(DATE_DAY, DateColumn { values: date_days })
        .with_column(
            ObjectId(31),
            DateColumn {
                values: date_weekdays,
            },
        )
        .with_column(FACT_AMOUNT, IntColumn { values: amount })
        .with_column(
            FACT_PRODUCT_ID,
            IntColumn { values: product_ids },
        )
        .with_column(PRODUCT_ID, IntColumn { values: product_id })
        .with_column(PRODUCT_REGION, TextColumn { values: region })
        .with_column(
            PRODUCT_REGION_ORDER,
            IntColumn {
                values: region_orden,
            },
        )
        .with_column(PRODUCT_CATEGORY, TextColumn { values: category })
        .with_table_length(FACT, 40)
        .with_table_length(PRODUCT, 6)
}

/// Crea un motor con los fixtures cargados.
pub fn engine() -> datan_storage::Result<Engine> {
    let engine = Engine::open(":memory:")?;
    for statement in FIXTURES.split(';') {
        let trimmed = statement.trim();
        if !trimmed.is_empty() {
            engine.execute(trimmed)?;
        }
    }
    Ok(engine)
}

/// Construye el contexto de filtro de un caso.
pub fn context_for(model: &Model, case: &Case) -> FilterContext {
    let mut context = FilterContext::empty();
    for (column, value) in &case.filters {
        let id = match *column {
            "region" => PRODUCT_REGION,
            "category" => PRODUCT_CATEGORY,
            "product_id" => FACT_PRODUCT_ID,
            "qty" => FACT_QTY,
            other => panic!("columna de filtro desconocida: {other}"),
        };
        context = match id {
            PRODUCT_REGION | PRODUCT_CATEGORY => {
                context.with_equal(id, FilterValue::Text(value.to_string()))
            }
            _ => context.with_equal(id, FilterValue::Integer(value.parse().unwrap_or(0))),
        };
    }

    // El time intelligence se aplica DESPUES de los filtros de columna, igual que en DAX:
    // el `CALCULATE` de `PREVIOUSYEAR` descarta el filtro de fechas del contexto y pone el
    // suyo. Aplicarlo antes haria que `previousyear` se combinara con un filtro de fecha
    // preexistente y el rango se intersectara en vez de reemplazarse.
    if let Some((function, current, today)) = &case.time {
        context = time_filters(model, *function, *current, *today)
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
    }
    context
}

/// Corre un caso por los dos caminos.
pub fn run_case(engine: &Engine, case: &Case) -> Result<PathResult, String> {
    let model = model();
    let context = context_for(&model, case);
    let grouping = [PRODUCT_REGION];

    let atoms = extract_atoms(case.measure, &model, &context, &grouping)
        .map_err(|e| e.to_string())?;
    let sources = SourceMap::from_model(&model);

    // Camino 1: SQL. El atomo tiene que ser bajable; si no, este caso no prueba nada.
    //
    // El SQL se envuelve en un `SELECT` que castea TODAS las filas a texto, no en un
    // `SUM(atom_0)`.
    //
    // La version anterior sumaba la columna del atomo sobre las filas de grupo. Para `SUM`
    // eso es inocuo, porque la suma de sumas es el total. Para `MIN`, `MAX` y `AVERAGE` no
    // lo es: `SUM(MIN(qty * pid))` sobre tres regiones es la SUMA de los minimos, y
    // `AVG(AVG(x))` no es el promedio. Los tres casos divergian y la culpa era del arnes,
    // no del motor. Comparar el conjunto de filas no tiene ese problema.
    let sql = match lower(&model, &sources, &atoms, &context) {
        Ok(plan) => Some(plan.sql),
        Err(error) => {
            eprintln!("[debug] {} no baja a SQL: {error}", case.name);
            None
        }
    };
    let (sql_rows, sql_value) = match &sql {
        Some(sql) => {
            // Se castea a VARCHAR porque `SUM` de enteros devuelve `HUGEINT`, que Arrow
            // representa como `Decimal128`, y la lectura de celdas no lo mira.
            let rendered = engine.render_rows(sql).map_err(|e| e.to_string())?;
            let value = rendered.first().map(|row| row.first().cloned().unwrap_or_default());
            (Some(rendered), value)
        }
        None => (None, None),
    };

    // Camino 2: interprete, celda de grupo a celda de grupo.
    //
    // El agrupamiento se evalua entero, no celda a celda con `evaluate_cell`: comparar un
    // total del fast path contra un total del interprete solo valida `SUM`. Con `MIN` o
    // `AVERAGE` hace falta la matriz, porque una suma de minimos no es el minimo.
    let source = memory_source();
    let interpreter = Interpreter::new(&model, &source);
    let grouped = interpreter
        .evaluate_grouped(&atoms.atoms, &atoms.outputs, &context, &grouping, MAX_CELLS)
        .map_err(|e| e.to_string())?;
    let interp_rows: Vec<Vec<String>> = grouped
        .rows
        .iter()
        .map(|row| row.iter().map(scalar_text).collect())
        .collect();

    // El primer atomo es el que se compara en la vista de una linea. Los casos de la suite
    // tienen una sola medida.
    let interp_value = interpreter
        .evaluate_cell(&atoms.atoms, &context)
        .ok()
        .and_then(|result| result.get(0).cloned());

    // El camino SQL devuelve texto, el interprete devuelve `Scalar`. La comparacion
    // numerica sobre el texto parseado evita los falsos negativos de tipo: `SUM` de un
    // entero devuelve `HUGEINT` en DuckDB y `Integer` en Rust.
    // `None` del lado SQL significa que el filtro no dejo filas, y `Blank` del lado
    // interprete significa lo mismo. Eso es coincidencia, no una prueba que no prueba.
    // La comparacion de UNA linea queda para cuando no hay fast path. Con fast path manda
    // la matriz: comparar el primer renglon de dos cosas distintas es comparar celdas que
    // no se corresponden.
    let agrees = if sql.is_some() {
        match (&sql_rows, &interp_rows) {
            (Some(sql), interpreter) => matrices_agree(sql, interpreter),
            _ => false,
        }
    } else {
        true
    };

    // Un caso sin fast path no es un fallo: es el motivo de existir del interprete. Lo que
    // no vale es que un caso CON fast path diverja, ni que uno sin fast path se cuente como
    // verificado por los dos caminos.
    let interpreter_only = sql.is_none();

    Ok(PathResult {
        label: case.name.to_string(),
        sql,
        sql_value,
        interp_value,
        sql_rows,
        interp_rows,
        agrees: agrees || interpreter_only,
        interpreter_only,
        error: None,
    })
}

/// Tope de celdas de una matriz.
///
/// Un producto cartesiano de columnas de alta cardinalidad no es un informe. El corte se
/// produce con un error que dice el numero, no truncando la matriz en silencio: un informe
/// que sale con menos celdas de las que tiene parece uno que funciona.
const MAX_CELLS: usize = 100_000;

/// Un escalar como texto, con la misma convencion que `Engine::render_rows`.
///
/// La comparacion es de cadenas, asi que las dos tienen que usar el mismo redondeo. Sin el,
/// `27009.090909` en DuckDB y `27009.09090909091` en Rust divergen siempre.
fn scalar_text(value: &Scalar) -> String {
    match value {
        Scalar::Blank => "NULL".to_string(),
        Scalar::Boolean(b) => b.to_string(),
        Scalar::Integer(n) => n.to_string(),
        Scalar::Decimal(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n:.6}")
            }
        }
        Scalar::Text(t) => t.clone(),
        Scalar::Date(day) => day.to_string(),
    }
}

/// Dos matrices coinciden si tienen las mismas filas, en el mismo orden.
///
/// El orden importa y por eso `evaluate_grouped` ordena por las columnas de agrupamiento con
/// el mismo criterio que el `GROUP BY` de DuckDB. Comparar sin ordenar daria divergencias que
/// son de orden, no de valor, y taparian las que si son de valor.
fn matrices_agree(sql: &[Vec<String>], interpreter: &[Vec<String>]) -> bool {
    // Se ordena ANTES de comparar. El orden de filas no es parte de la semantica que el
    // arnes verifica, y comparar en el orden de salida convierte cada diferencia de hash en
    // una divergencia, que tapa las de valor.
    //
    // El `ORDER BY` del lowering da orden estable al grid; esto es lo mismo aplicado a la
    // comparacion, y hace que un fallo de orden no se confunda con un fallo de numero.
    let mut left: Vec<&Vec<String>> = sql.iter().collect();
    let mut right: Vec<&Vec<String>> = interpreter.iter().collect();
    left.sort();
    right.sort();
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(&right).all(|(a, b)| a == b)
}

/// Ejecuta la suite entera.
///
/// Cada caso produce un resultado, termasuk los que fallan. La version anterior usaba
/// `filter_map(..ok())` y se comia los errores: un caso que revienta desaparece del
/// informe sin dejar rastro, y el recuento baja como si estuviera correcto. Ocho casos de
/// time intelligence estuvieron invisibles hasta que esto se arreglo.
pub fn run_suite(engine: &Engine, cases: &[Case]) -> Vec<PathResult> {
    cases
        .iter()
        .map(|case| match run_case(engine, case) {
            Ok(result) => result,
            Err(error) => PathResult {
                label: case.name.to_string(),
                sql: None,
                sql_value: None,
                interp_value: None,
                sql_rows: None,
                interp_rows: Vec::new(),
                agrees: false,
                interpreter_only: false,
                error: Some(error),
            },
        })
        .collect()
}

/// La suite base. Cada caso tiene un `expected` calculado a mano cuando se puede.
pub fn suite() -> Vec<Case> {
    vec![
        Case::new("sum sin filtro", "SUM('fact_sales'[qty])").expecting(Scalar::Integer(0)),
        Case::new("sum por region Norte", "SUM('fact_sales'[qty])")
            .filter("region", "Norte"),
        Case::new("sum por region Sur", "SUM('fact_sales'[qty])")
            .filter("region", "Sur"),
        Case::new("sum por region Este", "SUM('fact_sales'[qty])")
            .filter("region", "Este"),
        Case::new("sum por region que no existe", "SUM('fact_sales'[qty])")
            .filter("region", "Oeste"),
        Case::new("amount sin filtro", "SUM('fact_sales'[amount])"),
        Case::new("amount por Norte", "SUM('fact_sales'[amount])")
            .filter("region", "Norte"),
        Case::new("count por Norte", "COUNT('fact_sales'[qty])")
            .filter("region", "Norte"),
        Case::new("min por Norte", "MIN('fact_sales'[qty])")
            .filter("region", "Norte"),
        Case::new("max por Sur", "MAX('fact_sales'[qty])")
            .filter("region", "Sur"),
        Case::new("average por Este", "AVERAGE('fact_sales'[qty])")
            .filter("region", "Este"),
        Case::new("distinctcount por Norte", "DISTINCTCOUNT('fact_sales'[qty])")
            .filter("region", "Norte"),
        Case::new("countrows por Norte", "COUNTROWS('fact_sales')")
            .filter("region", "Norte"),
        Case::new("dos filtros", "SUM('fact_sales'[qty])")
            .filter("region", "Norte")
            .filter("category", "A"),
        Case::new("filtro por clave", "SUM('fact_sales'[qty])")
            .filter("product_id", "3"),

        // Agregaciones sobre EXPRESION. Son las medidas de negocio mas comunes y el
        // fixture no tenia ninguna: los 27 casos anteriores usaban todas una sola columna, y
        // el interprete devolvia "no se puede interpretar" sin que el arnés lo notara,
        // porque el fast path si bajaba y la comparacion solo miraba SQL.
        Case::new("suma de un producto de columnas", "SUM('fact_sales'[qty] * 'fact_sales'[amount])"),
        Case::new("suma de una expresion que cruza tablas", "SUM('fact_sales'[qty] * 'dim_product'[product_id])"),
        Case::new("suma de una expresion por region", "SUM('fact_sales'[qty] * 'fact_sales'[amount])")
            .filter("region", "Norte"),
        Case::new("min de una expresion", "MIN('fact_sales'[qty] * 'dim_product'[product_id])"),
        Case::new("max de una expresion", "MAX('fact_sales'[qty] * 'dim_product'[product_id])"),
        Case::new("average de una expresion", "AVERAGE('fact_sales'[qty] * 'fact_sales'[amount])"),
        // Una suma de ceros da 0, y un BLANK solo cuando no hay filas o todas son blank. La
        // distincion importa: `0` es un dato y BLANK es "no hay".
        Case::new("suma de una expresion que vale cero", "SUM('fact_sales'[qty] * 0)")
            .expecting(Scalar::Integer(0)),
        Case::new("suma de una expresion sobre filtro vacio", "SUM('fact_sales'[qty] * 'fact_sales'[amount])")
            .filter("region", "Oeste")
            .expecting(Scalar::Blank),

        // Una medida que es ARITMETICA ENTRE medidas. Referencia `[Qty]` dos veces, asi que
        // una hoja repetida tiene que ser un solo atomo y la division tiene que salir en el
        // mismo SELECT, sin subconsulta por celda.
        Case::new("margin: division entre medidas", "[Margin]"),
        Case::new("margin con filtro de region", "[Margin]").filter("region", "Norte"),
        Case::new("margin de una hoja repetida", "([Qty] - [Qty]) / [Qty]"),

        // Funciones escalares en el nivel superior. Los dos caminos tienen que decir lo
        // mismo: el `NULLIF` del SQL y el BLANK del interprete son la misma regla escrita dos
        // veces, y una medida de negocio se escribe con `DIVIDE`, no con una barra.
        Case::new("divide entre medidas", "[Ratio]"),
        Case::new("divide con filtro de region", "[Ratio]").filter("region", "Norte"),
        // Denominador cero. DuckDB devuelve `inf` sin el `NULLIF` y DAX devuelve BLANK, asi
        // que este caso es el que distingue una regla de una casualidad.
        Case::new("divide con denominador cero", "DIVIDE([Qty], [Qty] - [Qty])")
            .expecting(Scalar::Blank),
        Case::new(
            "divide con denominador cero y valor alterno",
            "DIVIDE([Qty], [Qty] - [Qty], -1)",
        )
        .expecting(Scalar::Integer(-1)),
        Case::new("if con una comparacion", "[Condicional]"),
        Case::new("if con una comparacion por region", "[Condicional]").filter("region", "Sur"),
        Case::new("coalesce de una medida", "COALESCE([Qty], 0)"),
        Case::new("switch por el valor de la medida", "SWITCH([Qty], 57, [Amount], 0)"),
        Case::new("menos unario", "-[Qty]").expecting(Scalar::Integer(0)),
        Case::new(
            "variable de una medida",
            "VAR total = [Qty] RETURN total * 2",
        ),

        // Casos de `CALCULATE`. No tienen fast path: el atomo se marca para el interprete
        // porque el conjunto de filtros cambia de forma no uniforme. El harness tiene que
        // aceptar eso, no fallar por no tener las dos columnas.
        Case::new("calculate con ALL de la dimension", "CALCULATE([Qty], ALL('dim_product'))")
            .filter("region", "Norte"),
        Case::new("calculate con ALL global", "CALCULATE([Qty], ALL())")
            .filter("region", "Norte"),
        Case::new(
            "calculate con allexcept",
            "CALCULATE([Qty], ALLEXCEPT('dim_product', 'dim_product'[region]))",
        )
        .filter("region", "Norte")
        .filter("category", "A"),
        Case::new(
            "calculate con filtro contradictorio",
            "CALCULATE([Qty], 'dim_product'[region] = 'Oeste')",
        )
        .filter("region", "Norte"),

        // Casos de time intelligence. Estos SI tienen fast path: el rango de fechas son dos
        // filtros `>=` y `<=` sobre la columna de fecha, y los dos operadores ya bajan a SQL.
        // Por eso son los que atrapan el fallo de los literales de fecha, que no se ve al
        // compilar: DuckDB responde con un error de conversion en tiempo de ejecucion.
        //
        // "Hoy" es 15 de marzo de 2024 en todos ellos, asi que `PREVIOUSYEAR` es 2023 y
        // `DATESYTD` es desde el 1 de enero de 2024.
        Case::new("previousyear sobre la ventana actual", "SUM('fact_sales'[amount])")
            .time(
                TimeFunction::PreviousYear,
                DateRange::new(Y2024 + 74, Y2024 + 74),
                Y2024 + 74,
            ),
        Case::new("dateadd de un ano hacia atras", "SUM('fact_sales'[amount])")
            .time(
                TimeFunction::DateAdd {
                    units: -1,
                    unit: CalendarUnit::Year,
                },
                DateRange::new(Y2024 + 74, Y2024 + 74),
                Y2024 + 74,
            ),
        Case::new("dateadd de un mes hacia atras", "SUM('fact_sales'[amount])")
            .time(
                TimeFunction::DateAdd {
                    units: -1,
                    unit: CalendarUnit::Month,
                },
                DateRange::new(Y2024 + 74, Y2024 + 74),
                Y2024 + 74,
            ),
        Case::new("datesytd sobre un rango ancho", "SUM('fact_sales'[amount])")
            .time(
                TimeFunction::YearToDate,
                DateRange::new(Y2023, Y2024 + 74),
                Y2024 + 74,
            ),
        Case::new("parallelperiod de un ano", "SUM('fact_sales'[amount])")
            .time(
                TimeFunction::ParallelPeriod {
                    amount: -1,
                    unit: CalendarUnit::Year,
                },
                DateRange::new(Y2024 + 74, Y2024 + 74),
                Y2024 + 74,
            ),
        // Un rango que cae antes de las ventas: tiene que dar BLANK, no el total. Un filtro
        // de fecha que no filtra devuelve el total en silencio, y ese es el fallo que mas
        // caro sale en un informe.
        Case::new("previousyear anterior al modelo", "SUM('fact_sales'[amount])")
            .time(
                TimeFunction::PreviousYear,
                DateRange::new(Y2023 - 365, Y2023 - 365),
                Y2023 - 365,
            )
            .expecting(Scalar::Blank),
        // Time intelligence combinado con un filtro de columna: los dos filtros travelan.
        Case::new("previousyear con filtro de region", "SUM('fact_sales'[amount])")
            .filter("region", "Norte")
            .time(
                TimeFunction::PreviousYear,
                DateRange::new(Y2024 + 74, Y2024 + 74),
                Y2024 + 74,
            ),
        Case::new("datesytd con filtro de region", "SUM('fact_sales'[amount])")
            .filter("region", "Sur")
            .time(
                TimeFunction::YearToDate,
                DateRange::new(Y2023, Y2024 + 74),
                Y2024 + 74,
            ),
    ]
}

/// El conteo de atomos de un conjunto, para diagnostico.
pub fn atom_summary(atoms: &AtomSet) -> Vec<(String, AtomKind)> {
    atoms
        .atoms
        .iter()
        .map(|atom| (atom.label.clone(), atom.kind))
        .collect()
}

#[cfg(test)]
mod fixtures {
    use super::*;

    #[test]
    fn los_fixtures_cargan() {
        let engine = engine().expect("carga fixtures");
        let batches = engine.query_arrow("SELECT COUNT(*) FROM fact_sales").expect("cuenta");
        assert_eq!(batches[0].num_rows(), 1);
    }
}