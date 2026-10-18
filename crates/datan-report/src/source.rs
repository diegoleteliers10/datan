//! Un origen de datos para el interprete, servido desde DuckDB.
//!
//! El fast path SQL no lo necesita: baja la medida entera a una consulta. Lo necesita el
//! interprete, que corre cuando un atomo no se baja (`CALCULATE`, iteradores, time
//! intelligence) y lee COLUMNA POR COLUMNA. Sin este origen, un informe con una medida
//! interpretada fallaba con `NoSource`: el analisis no podia correr contra los datos reales.
//!
//! La columna se materializa UNA vez y queda en un cache: el interprete la visita por cada
//! filtro de cada celda, y releerla de DuckDB en cada visita son miles de consultas iguales.
//! Es el mismo modelo de VertiPaq, a escala de una app de escritorio.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use datan_dax_eval::{ColumnView, DataSource, Scalar};
use datan_dax_sql::{quote_identifier, quote_qualified, SourceMap};
use datan_model::{Model, ObjectId};
use datan_storage::{
    arrow,
    arrow::array::{
        Array, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int32Array, Int64Array,
        StringArray,
    },
    Engine,
};

/// Una columna leida de DuckDB una vez y servida desde memoria.
struct DuckColumn {
    values: Vec<Scalar>,
    non_blank: usize,
    distinct: usize,
}

impl ColumnView for DuckColumn {
    fn len(&self) -> usize {
        self.values.len()
    }

    /// El valor de la fila. Devuelve `Blank` si la fila no existe o es nula.
    fn value_at(&self, row: usize) -> Scalar {
        self.values.get(row).cloned().unwrap_or(Scalar::Blank)
    }

    fn non_blank_count(&self) -> usize {
        self.non_blank
    }

    fn distinct_count(&self) -> usize {
        self.distinct
    }
}

/// Un origen sobre el motor DuckDB del informe.
pub struct DuckSource<'a> {
    engine: &'a Engine,
    sources: SourceMap,
    columns: Mutex<HashMap<ObjectId, Arc<DuckColumn>>>,
    lengths: Mutex<HashMap<ObjectId, usize>>,
}

impl<'a> DuckSource<'a> {
    /// El mapa fisico es el del modelo, con el calificador de la base adjunta. Es la MISMA
    /// cuenta que hace el lowering, y tiene que ser la misma: los dos caminos leen las mismas
    /// tablas.
    pub fn new(engine: &'a Engine, model: &Model, qualifier: &str) -> Self {
        Self {
            engine,
            sources: SourceMap::from_model(model).with_qualifier(qualifier),
            columns: Mutex::new(HashMap::new()),
            lengths: Mutex::new(HashMap::new()),
        }
    }

    /// Lee la columna entera y la convierte a escalares, preservando los nulos.
    ///
    /// Un tipo que no se sabe convertir se relee como texto: el valor llega y la medida de
    /// texto funciona, aunque los filtros de fecha no apliquen. Es la misma limitacion que
    /// tiene el fast path con las columnas `timestamp`.
    fn read_column(&self, column: ObjectId) -> Option<DuckColumn> {
        let (physical, table) = self.sources.column(column).ok()?;
        let (qualified, _) = self.sources.qualified(table).ok()?;
        let sql = format!(
            "SELECT {} FROM {}",
            quote_identifier(physical),
            quote_qualified(&qualified)
        );

        let batches = self.engine.query_arrow(&sql).ok()?;
        let mut values: Vec<Scalar> = Vec::new();
        for batch in &batches {
            if batch.num_columns() != 1 {
                return None;
            }
            read_array(batch.column(0).as_ref(), &mut values);
        }

        // Un tipo que quedo vacio es una columna que no se supo leer. Se reintenta como
        // texto antes de devolver nada.
        if batches.iter().all(|batch| batch.num_rows() == 0) {
            let sql = format!(
                "SELECT CAST({} AS VARCHAR) FROM {}",
                quote_identifier(physical),
                quote_qualified(&qualified)
            );
            values.clear();
            for batch in &self.engine.query_arrow(&sql).ok()? {
                read_array(batch.column(0).as_ref(), &mut values);
            }
        }

        let non_blank = values.iter().filter(|value| !value.is_blank()).count();
        let distinct: BTreeSet<String> = values
            .iter()
            .filter(|value| !value.is_blank())
            .map(|value| format!("{value:?}"))
            .collect();
        Some(DuckColumn {
            values,
            non_blank,
            distinct: distinct.len(),
        })
    }

    fn columns(&self) -> MutexGuard<'_, HashMap<ObjectId, Arc<DuckColumn>>> {
        self.columns.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Un candado emponzonado no tiene que tumbar el informe: el cache sigue siendo util.
fn lengths(map: &Mutex<HashMap<ObjectId, usize>>) -> MutexGuard<'_, HashMap<ObjectId, usize>> {
    map.lock().unwrap_or_else(|error| error.into_inner())
}

impl DataSource for DuckSource<'_> {
    fn column(&self, column: ObjectId) -> Option<Arc<dyn ColumnView>> {
        if let Some(hit) = self.columns().get(&column) {
            return Some(hit.clone());
        }
        let built = Arc::new(self.read_column(column)?);
        self.columns().insert(column, built.clone());
        Some(built)
    }

    fn table_len(&self, table: ObjectId) -> usize {
        if let Some(hit) = lengths(&self.lengths).get(&table) {
            return *hit;
        }
        let len = self
            .sources
            .qualified(table)
            .ok()
            .and_then(|(qualified, _)| {
                let sql = format!("SELECT COUNT(*) FROM {}", quote_qualified(&qualified));
                self.engine.query_arrow(&sql).ok().and_then(|batches| {
                    let first = batches.first()?;
                    let counts = first.column(0).as_any().downcast_ref::<Int64Array>()?;
                    Some(counts.value(0) as usize)
                })
            })
            .unwrap_or(0);
        lengths(&self.lengths).insert(table, len);
        len
    }
}

/// Vuelca un array de Arrow a escalares. Los nulos son `Blank`, que es como DAX los lee.
fn read_array(array: &dyn Array, out: &mut Vec<Scalar>) {
    let data_type = array.data_type();
    if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        for row in 0..values.len() {
            out.push(if values.is_null(row) {
                Scalar::Blank
            } else {
                Scalar::Integer(values.value(row))
            });
        }
        return;
    }
    if let Some(values) = array.as_any().downcast_ref::<Int32Array>() {
        for row in 0..values.len() {
            out.push(if values.is_null(row) {
                Scalar::Blank
            } else {
                Scalar::Integer(values.value(row) as i64)
            });
        }
        return;
    }
    if let Some(values) = array.as_any().downcast_ref::<Float64Array>() {
        for row in 0..values.len() {
            out.push(if values.is_null(row) {
                Scalar::Blank
            } else {
                Scalar::Decimal(values.value(row))
            });
        }
        return;
    }
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        for row in 0..values.len() {
            out.push(if values.is_null(row) {
                Scalar::Blank
            } else {
                Scalar::Text(values.value(row).to_string())
            });
        }
        return;
    }
    if let Some(values) = array.as_any().downcast_ref::<Date32Array>() {
        for row in 0..values.len() {
            out.push(if values.is_null(row) {
                Scalar::Blank
            } else {
                Scalar::Date(values.value(row))
            });
        }
        return;
    }
    // Un `timestamp` se lee como su DIA: el motor trabaja en dias, y la hora dentro del dia
    // no cambia ningun informe. Es la misma decision que toma el fast path cuando baja un
    // filtro de fecha contra una columna `timestamp`.
    macro_rules! timestamp {
        ($tipo:ty, $escala:expr) => {
            if let Some(values) = array.as_any().downcast_ref::<$tipo>() {
                for row in 0..values.len() {
                    out.push(if values.is_null(row) {
                        Scalar::Blank
                    } else {
                        // `div_euclid` y no `/`: un instante anterior a 1970 trunca hacia
                        // su dia, no hacia cero.
                        Scalar::Date(values.value(row).div_euclid($escala) as i32)
                    });
                }
                return;
            }
        };
    }
    timestamp!(arrow::array::TimestampSecondArray, 86_400);
    timestamp!(arrow::array::TimestampMillisecondArray, 86_400_000);
    timestamp!(arrow::array::TimestampMicrosecondArray, 86_400_000_000);
    timestamp!(arrow::array::TimestampNanosecondArray, 86_400_000_000_000);
    if let Some(values) = array.as_any().downcast_ref::<BooleanArray>() {
        for row in 0..values.len() {
            out.push(if values.is_null(row) {
                Scalar::Blank
            } else {
                Scalar::Boolean(values.value(row))
            });
        }
        return;
    }
    if let arrow::datatypes::DataType::Decimal128(_, scale) = data_type {
        let values = array
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .expect("decimal");
        let scale = 10f64.powi(*scale as i32);
        for row in 0..values.len() {
            out.push(if values.is_null(row) {
                Scalar::Blank
            } else {
                Scalar::Decimal(values.value(row) as f64 / scale)
            });
        }
    }
    // Tipo desconocido: sin valores. El que llama reintenta la columna como texto.
}

#[cfg(test)]
mod tests {
    use super::*;
    use datan_dax_eval::{FilterContext, FilterValue, Interpreter, extract_atoms};
    use datan_testkit::{engine, model, PRODUCT_REGION};

    /// El interprete, con DuckDB detras, ve la misma columna que la memoria materializa:
    /// seis productos, tres regiones, cuarenta ventas con cinco nulos.
    #[test]
    fn el_origen_de_duckdb_lee_lo_mismo_que_la_memoria() {
        let engine = engine().expect("motor");
        let model = model();
        let source = DuckSource::new(&engine, &model, "");

        let view = source.column(PRODUCT_REGION).expect("columna");
        assert_eq!(view.len(), 6);
        assert_eq!(view.non_blank_count(), 6);
        assert_eq!(view.distinct_count(), 3, "Norte, Sur y Este");

        // `fact_sales` deja 5 nulos en blanco: i = 7, 14, 21, 28, 35.
        let qty = ObjectId(11);
        let view = source.column(qty).expect("columna");
        assert_eq!(view.len(), 40);
        assert_eq!(view.non_blank_count(), 35);

        let table = model.column(qty).map(|(table, _)| table.id).expect("tabla");
        assert_eq!(source.table_len(table), 40);
    }

    /// Una medida con CALCULATE corre contra DuckDB y da el numero de la suite. Es la
    /// prueba de que la app puede correr medidas interpretadas contra los datos reales.
    #[test]
    fn una_medida_con_calculate_corre_contra_duckdb() {
        let engine = engine().expect("motor");
        let model = model();
        let source = DuckSource::new(&engine, &model, "");

        let filters = FilterContext::empty()
            .with_equal(PRODUCT_REGION, FilterValue::Text("Norte".to_string()));
        let atoms = extract_atoms(
            "[Qty] - CALCULATE([Qty], ALL('dim_product'))",
            &model,
            &filters,
            &[ObjectId(21)],
        )
        .expect("extrae");
        let interpreter = Interpreter::new(&model, &source);
        let values = interpreter
            .evaluate_totals(&atoms.atoms, &atoms.outputs, &filters)
            .expect("evalua");
        // La variacion contra el total, con Norte puesto: 60 menos 165.
        assert_eq!(values, vec![Scalar::Integer(-105)]);
    }

    /// Una columna `TIMESTAMP` se lee como su DIA. Postgres devuelve timestamps para sus
    /// fechas, y una columna que el origen no supiera leer caeria a texto: sin periodo, sin
    /// filtro de fecha y sin time intelligence en toda la base.
    #[test]
    fn un_timestamp_se_lee_como_su_dia() {
        use datan_model::{Column, DataType, Table, TableKind};

        let engine = Engine::open(":memory:").expect("motor");
        engine
            .execute("CREATE TABLE boleta (creado TIMESTAMP)")
            .expect("tabla");
        engine
            .execute(
                "INSERT INTO boleta VALUES \
                 ('2023-01-01 23:59:59'), ('2024-06-15 06:30:00'), (NULL)",
            )
            .expect("datos");

        // 19358 es el dia de 2023-01-01 en la fixture de la suite.
        let model = Model {
            tables: vec![Table {
                id: ObjectId(1),
                name: "boleta".to_string(),
                kind: TableKind::Facts,
                columns: vec![Column::new(ObjectId(2), "creado", DataType::DateTime)],
                key_columns: vec![],
                is_date_table: false,
            }],
            measures: vec![],
            relationships: vec![],
        };
        let source = DuckSource::new(&engine, &model, "");
        let view = source.column(ObjectId(2)).expect("columna");
        assert_eq!(view.len(), 3);
        assert_eq!(view.value_at(0), Scalar::Date(19358));
        assert_eq!(view.value_at(1), Scalar::Date(19358 + 531));
        assert_eq!(view.value_at(2), Scalar::Blank);
        assert_eq!(view.non_blank_count(), 2);
    }
}
