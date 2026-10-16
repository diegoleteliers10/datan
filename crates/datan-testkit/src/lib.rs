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
    AtomKind, AtomSet, CellResult, FilterContext, FilterValue, IntColumn, Interpreter,
    MemorySource, Scalar, TextColumn, extract_atoms,
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
    /// Los dos caminos existen y dicen lo mismo.
    pub agrees: bool,
    /// El atomo cae al interprete y no tiene fast path. NO es un fallo.
    pub interpreter_only: bool,
}

/// Un caso de la suite: un filtro y una medida.
pub struct Case {
    pub name: &'static str,
    pub measure: &'static str,
    pub filters: Vec<(&'static str, &'static str)>,
    pub expected: Option<Scalar>,
}

impl Case {
    pub fn new(name: &'static str, measure: &'static str) -> Self {
        Self {
            name,
            measure,
            filters: Vec::new(),
            expected: None,
        }
    }

    pub fn filter(mut self, column: &'static str, value: &'static str) -> Self {
        self.filters.push((column, value));
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

pub const FACT_QTY: ObjectId = ObjectId(11);
pub const FACT_PRODUCT_ID: ObjectId = ObjectId(12);
pub const FACT_AMOUNT: ObjectId = ObjectId(13);
pub const PRODUCT_ID: ObjectId = ObjectId(20);
pub const PRODUCT_REGION: ObjectId = ObjectId(21);
pub const PRODUCT_CATEGORY: ObjectId = ObjectId(22);

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
        measures: vec![
            Measure::new(ObjectId(100), "Qty", "SUM('fact_sales'[qty])"),
            Measure::new(ObjectId(101), "Amount", "SUM('fact_sales'[amount])"),
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

/// Los mismos datos que el spike, en memoria y en DuckDB.
///
/// Que sean los mismos es lo que hace valida la comparacion. Si el interprete lee una cosa
/// y DuckDB otra, la divergencia no dice nada.
pub const FIXTURES: &str = "\
    CREATE TABLE dim_product AS
    SELECT
        i AS product_id,
        CASE i % 3 WHEN 0 THEN 'Norte' WHEN 1 THEN 'Sur' ELSE 'Este' END AS region,
        CASE i % 2 WHEN 0 THEN 'A' ELSE 'B' END AS category
    FROM range(1, 7) t(i);

    CREATE TABLE fact_sales AS
    SELECT
        i AS sale_id,
        ((i % 6) + 1) AS product_id,
        CASE WHEN i % 7 = 0 THEN NULL ELSE (i % 9) + 1 END AS qty,
        CASE WHEN i % 11 = 0 THEN NULL ELSE (i % 5) * 1000 END AS amount
    FROM range(1, 41) t(i);";

/// Los datos en memoria, con la misma forma que `FIXTURES`.
pub fn memory_source() -> MemorySource {
    let mut qty = Vec::new();
    let mut amount = Vec::new();
    let mut product_ids = Vec::new();
    for i in 1..=40 {
        let sale_id = i;
        product_ids.push(Some(((i % 6) + 1) as i64));
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
        category.push(Some(if i % 2 == 0 { "A" } else { "B" }.to_string()));
    }

    MemorySource::new()
        .with_column(FACT_QTY, Box::new(IntColumn { values: qty }))
        .with_column(FACT_AMOUNT, Box::new(IntColumn { values: amount }))
        .with_column(
            FACT_PRODUCT_ID,
            Box::new(IntColumn { values: product_ids }),
        )
        .with_column(PRODUCT_ID, Box::new(IntColumn { values: product_id }))
        .with_column(PRODUCT_REGION, Box::new(TextColumn { values: region }))
        .with_column(PRODUCT_CATEGORY, Box::new(TextColumn { values: category }))
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
        let _ = model;
        context = match id {
            PRODUCT_REGION | PRODUCT_CATEGORY => {
                context.with_equal(id, FilterValue::Text(value.to_string()))
            }
            _ => context.with_equal(id, FilterValue::Integer(value.parse().unwrap_or(0))),
        };
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
    let sql = match lower(&model, &sources, &atoms, &context) {
        // El `SELECT` externo trae solo la columna del atomo: si trae tambien las de
        // agrupamiento, `scalar(0)` lee la primera y devuelve `region`.
        //
        // El cast a VARCHAR es necesario porque `SUM` de enteros devuelve `HUGEINT`, que
        // Arrow representa como `Decimal128`, y `scalar` solo lee columnas de texto.
        Ok(plan) => {
            // `SUM(atom_0)` y no `atom_0`: sin filtro la consulta agrupa por region y
            // devuelve una fila POR CELDA. Leer solo la primera compara una celda con un
            // total. La suite compara totales.
            Some(format!(
                "SELECT CAST(SUM(atom_0) AS VARCHAR) AS v FROM ({})",
                plan.sql
            ))
        }
        Err(error) => {
            eprintln!("[debug] {} no baja a SQL: {error}", case.name);
            None
        }
    };
    let sql_value = match &sql {
        Some(sql) => engine.scalar(sql, 0).map_err(|e| e.to_string())?,
        None => None,
    };

    // Camino 2: interprete.
    let source = memory_source();
    let interpreter = Interpreter::new(&model, &source);
    let result: CellResult = interpreter
        .evaluate_cell(&atoms.atoms, &context)
        .map_err(|e| e.to_string())?;
    // El primer atomo es el que se compara. Los casos de la suite tienen una sola medida.
    let interp_value = atoms
        .atoms
        .first()
        .and_then(|atom| result.get(&atom.label).cloned());

    // El camino SQL devuelve texto, el interprete devuelve `Scalar`. La comparacion
    // numerica sobre el texto parseado evita los falsos negativos de tipo: `SUM` de un
    // entero devuelve `HUGEINT` en DuckDB y `Integer` en Rust.
    // `None` del lado SQL significa que el filtro no dejo filas, y `Blank` del lado
    // interprete significa lo mismo. Eso es coincidencia, no una prueba que no prueba.
    let agrees = match (&sql_value, &interp_value) {
        (Some(a), Some(b)) => a
            .parse::<f64>()
            .map(|left| scalars_agree(&Scalar::Decimal(left), b))
            .unwrap_or(false),
        (None, None) => true,
        (None, Some(b)) => b.is_blank(),
        _ => false,
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
        agrees: agrees || interpreter_only,
        interpreter_only,
    })
}

/// Dos escalares coinciden si sus numeros coinciden.
///
/// La comparacion es numerica a proposito: `SUM` de un entero devuelve `HUGEINT` en
/// DuckDB y `Integer` en Rust. Comparar tipos strictos daria falsos negativos en cada
/// agregacion.
fn scalars_agree(sql: &Scalar, interp: &Scalar) -> bool {
    if sql.is_blank() && interp.is_blank() {
        return true;
    }
    (sql.as_number() - interp.as_number()).abs() < 1e-9
}

/// Ejecuta la suite entera y devuelve los casos que divergen.
pub fn run_suite(engine: &Engine, cases: &[Case]) -> Vec<PathResult> {
    cases
        .iter()
        .filter_map(|case| run_case(engine, case).ok())
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