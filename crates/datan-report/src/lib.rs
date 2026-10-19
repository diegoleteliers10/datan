//! Un informe: dimensiones, medidas y filtros. Y la consulta que lo llena.
//!
//! Un informe es una matriz. Las filas son combinaciones de dimensiones, las columnas son
//! medidas, y las celdas son numeros.
//!
//! # Por que el arnes diferencial no basta
//!
//! El arnes prueba que UNA medida da el mismo numero por los dos caminos. Un informe prueba
//! algo distinto y mas fuerte: que N medidas en M celdas se leen con las consultas
//! MINIMAS, y que pinchar una celda cambia el filtro de todas a la vez.
//!
//! # Una consulta por conjunto de atomos, y como se vuelven a pegar
//!
//! `Revenue`, `Qty` y `Margin` comparten hojas. Si cada medida fuera su propia consulta,
//! una matriz de 6 columnas leeria la tabla de hechos 6 veces.
//!
//! Las medidas que producen el MISMO `AtomSet` comparten la lectura. Pero dos conjuntos
//! distintos dan dos consultas con columnas de medida DIFERENTES, y hay que volver a unirlas
//! en una sola matriz.
//!
//! Se pegan por las columnas de dimension, no por posicion a ciegas. Es seguro porque todas
//! las medidas se evaluan bajo el MISMO filtro y el MISMO agrupamiento, y el `ORDER BY` del
//! lowering ordena por las columnas de agrupamiento. Las filas salen en el mismo orden y con
//! las mismas claves. Sin el `ORDER BY` seria suposicion: DuckDB devuelve los grupos en el
//! orden de su hash, y dos consultas distintas pueden darryptos en orden distinto.

pub mod export;
pub mod saved;
pub mod source;

use datan_dax_eval::{AtomSet, Filter, FilterContext, FilterValue, Interpreter, Scalar};
use datan_dax_sql::{SourceMap, lower, lower_scalar};
use datan_model::{DataType, Measure, Model, ObjectId};
use datan_storage::Engine;

/// Un error de ejecucion de un informe.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("la columna {0} no existe en el modelo")]
    UnknownColumn(ObjectId),
    #[error("la medida {0} no existe en el modelo")]
    UnknownMeasure(ObjectId),
    #[error("un informe sin medidas no muestra nada: se necesita al menos una")]
    NoMeasures,
    #[error("el modelo no tiene una tabla de hechos. Un informe agrupa una tabla de hechos, \\
             no una lista de tablas sueltas")]
    NoFactTable,
    #[error("`{measure}` necesita el interprete y no hay origen de datos en el informe")]
    NoSource { measure: String },
    #[error("la medida `{measure}` usa una funcion que el motor no sabe calcular todavia")]
    NoShape { measure: String },
    #[error("fallo la consulta del informe: {0}")]
    Query(String),
    #[error("el interprete fallo: {0}")]
    Interp(String),
    #[error("el motor devolvio {got} columnas y el informe esperaba {expected}")]
    ColumnCount { expected: usize, got: usize },
}

/// Tope de celdas de la matriz.
///
/// El producto cartesiano de las dimensiones es el limite real del informe. El corte se dice
/// con el numero en vez de truncar en silencio: un informe que sale con menos filas de las
/// que tiene parece uno que funciona.
pub const MAX_CELLS: usize = 500_000;

/// Un informe.
#[derive(Debug, Clone)]
pub struct Report {
    pub model: Model,
    /// Las columnas de la izquierda, en orden.
    pub dimensions: Vec<ObjectId>,
    /// Las medidas, en orden de columna.
    pub measures: Vec<ObjectId>,
    /// El filtro actual. Lo cambia el cross-filter.
    pub filters: FilterContext,
    /// El alias de la base adjunta a DuckDB, si la hay.
    ///
    /// Una base de Postgres adjunta vive bajo el alias del `ATTACH`, y sus tablas se llaman
    /// `pg.product_boleta`. Sin esto, el SQL se genera bien y DuckDB responde "table does not
    /// exist" con una pista que es justamente el nombre con el prefijo que falta.
    pub qualifier: String,
    /// Subtotales por nivel de dimension y gran total al final.
    ///
    /// Apagado por defecto: una matriz de UNA dimension no tiene subtotales que mostrar, y
    /// encenderlo de mas cambia el numero de filas que ven todos los consumidores.
    pub subtotales: bool,
}

impl Report {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            dimensions: Vec::new(),
            measures: Vec::new(),
            filters: FilterContext::empty(),
            qualifier: String::new(),
            subtotales: false,
        }
    }

    /// Enciende o apaga los subtotales. Con menos de dos dimensiones no hacen nada: una
    /// jerarquia de un nivel no tiene niveles intermedios.
    pub fn with_subtotals(mut self, subtotales: bool) -> Self {
        self.subtotales = subtotales;
        self
    }

    /// El informe corre contra la base adjunta con este alias.
    pub fn attached_to(mut self, alias: &str) -> Self {
        self.qualifier = alias.to_string();
        self
    }

    pub fn with_dimensions(mut self, dimensions: Vec<ObjectId>) -> Self {
        self.dimensions = dimensions;
        self
    }

    pub fn with_measures(mut self, measures: Vec<ObjectId>) -> Self {
        self.measures = measures;
        self
    }

    pub fn measure_definitions(&self) -> Result<Vec<&Measure>, ReportError> {
        self.measures
            .iter()
            .map(|id| {
                self.model
                    .measures
                    .iter()
                    .find(|measure| measure.id == *id)
                    .ok_or(ReportError::UnknownMeasure(*id))
            })
            .collect()
    }

    /// El nombre de una medida, si existe.
    pub fn measure_name(&self, measure: ObjectId) -> Option<&str> {
        self.model
            .measures
            .iter()
            .find(|m| m.id == measure)
            .map(|m| m.name.as_str())
    }

    /// La tabla de hechos: la que contiene los atoms.
    pub fn fact_table(&self) -> Result<ObjectId, ReportError> {
        self.model
            .tables
            .iter()
            .find(|table| matches!(table.kind, datan_model::TableKind::Facts))
            .map(|table| table.id)
            .ok_or(ReportError::NoFactTable)
    }

    /// Anade un filtro de igualdad, como el que produce pinchar una celda.
    pub fn filtered_by(&self, column: ObjectId, value: FilterValue) -> Self {
        let mut next = self.clone();
        next.filters = next.filters.with_equal(column, value);
        next
    }

    /// Filtra la columna a un CONJUNTO de valores, como un slicer con varios elegidos.
    ///
    /// Reemplaza y no acumula: un slicer es UN filtro sobre la columna, y elegir dos valores
    /// no es pedir dos filtros que se intersectan, que es lo que daria el conjunto vacio. La
    /// union vive dentro del filtro (`IN`), y el `AND` entre columnas distintas sigue igual.
    ///
    /// Un conjunto VACIO quita el filtro en vez de dejar un `IN` sin valores. Los dos caminos
    /// no coinciden con ese `IN`: el SQL lo baja como `1 = 1`, que pasa todo, y el interprete
    /// lo evalua como falso, que no pasa nada. Un informe que da todas las filas por SQL y
    /// ninguna por el interprete es la peor clase de divergencia, asi que el estado no se
    /// puede construir.
    pub fn filtered_to(&self, column: ObjectId, values: Vec<FilterValue>) -> Self {
        let mut next = self.clone();
        next.filters = next.filters.without_column(column);
        if values.is_empty() {
            return next;
        }
        next.filters = next.filters.with(Filter::in_values(column, values));
        next
    }

    /// Acota la columna de fecha a un rango, como los botones 3M / 6M / 12M.
    ///
    /// Dos filtros, `>=` y `<=`, y no un `BETWEEN`: los dos operadores ya viajan por el
    /// grafo de relaciones sin un caso especial.
    pub fn filtered_between(&self, column: ObjectId, from: i32, to: i32) -> Self {
        let mut next = self.clone();
        next.filters = next
            .filters
            .with_greater_equal(column, FilterValue::Date(from))
            .with_less_equal(column, FilterValue::Date(to));
        next
    }

    /// Suelta un filtro de una columna, como el que produce pinchar la cabecera.
    pub fn unfiltered(&self, column: ObjectId) -> Self {
        let mut next = self.clone();
        next.filters = next.filters.without_column(column);
        next
    }

    /// El total de cada medida, para las tarjetas.
    ///
    /// Sin agrupamiento. Sumar las filas de la matriz solo vale para medidas aditivas: el
    /// promedio de promedios no es el promedio, y el conteo distinto de conteos tampoco.
    ///
    /// Devuelve en el MISMO orden que `self.measures`, para que la tarjeta i es la medida i.
    pub fn totals(
        &self,
        engine: &Engine,
        source: Option<&dyn datan_dax_eval::DataSource>,
    ) -> Result<Vec<String>, ReportError> {
        let sources = SourceMap::from_model(&self.model).with_qualifier(&self.qualifier);

        // Los mismos grupos de lectura que `execute`. La agrupacion vive en un solo sitio:
        // mientras fueron dos bucles, `execute` separaba dos medidas que comparten hoja y
        // distinta forma, y las tarjetas no: las dos daban el numero de la primera.
        let plans = self.plans()?;

        let mut out = vec![String::new(); self.measures.len()];
        for (atoms, members) in &plans {
            if atoms.outputs.is_empty() {
                return Err(self.no_shape(members));
            }
            let values: Vec<String> = if atoms.needs_interpreter() {
                let src = source.ok_or_else(|| ReportError::NoSource {
                    measure: members
                        .first()
                        .and_then(|i| self.measures.get(*i))
                        .and_then(|id| self.measure_name(*id))
                        .unwrap_or("?")
                        .to_string(),
                })?;
                let interpreter = Interpreter::new(&self.model, src);
                interpreter
                    .evaluate_totals(&atoms.atoms, &atoms.outputs, &self.filters)
                    .map_err(|e| ReportError::Interp(e.to_string()))?
                    .iter()
                    .map(scalar_to_text)
                    .collect()
            } else {
                let plan = lower_scalar(&self.model, &sources, atoms, &self.filters)
                    .map_err(|e| ReportError::Interp(e.to_string()))?;
                let rows = engine
                    .render_rows(&plan.sql)
                    .map_err(|e| ReportError::Query(e.to_string()))?;
                rows.first().cloned().unwrap_or_default()
            };

            for (offset, member) in members.iter().enumerate() {
                // Una consulta produce una columna por FORMA, no por medida. Cuando el grupo
                // tiene mas medidas que columnas, las de mas son identicas a la ultima y leen
                // su valor. Leer por posicion dejaba la segunda en blanco: un guion en una
                // tarjeta que parece un dato que falta.
                let index = offset.min(values.len().saturating_sub(1));
                out[*member] = values.get(index).cloned().unwrap_or_default();
            }
        }

        Ok(out)
    }

    /// Las cabeceras: primero las dimensiones, luego las medidas.
    pub fn headers(&self) -> Result<Vec<Header>, ReportError> {
        let mut out = Vec::with_capacity(self.dimensions.len() + self.measures.len());
        for column in &self.dimensions {
            let (table, definition) = self
                .model
                .column(*column)
                .ok_or(ReportError::UnknownColumn(*column))?;
            out.push(Header {
                label: format!("{}[{}]", table.name, definition.name),
                column: *column,
                measure: None,
                data_type: definition.data_type,
            });
        }
        for measure in self.measure_definitions()? {
            out.push(Header {
                label: measure.name.clone(),
                column: ObjectId(0),
                measure: Some(measure.id),
                data_type: DataType::Decimal,
            });
        }
        Ok(out)
    }

    /// Cuantas consultas hacen falta para este informe.
    ///
    /// Es el numero que importa para la latencia: no las medidas, sino las CONSULTAS. Dos
    /// medidas que comparten hojas cuestan una.
    pub fn query_count(&self) -> Result<usize, ReportError> {
        Ok(self.plans()?.len())
    }

    /// El error de una medida sin forma, con el nombre de la primera del grupo.
    fn no_shape(&self, members: &[usize]) -> ReportError {
        ReportError::NoShape {
            measure: members
                .first()
                .and_then(|index| self.measures.get(*index))
                .and_then(|id| self.measure_name(*id))
                .unwrap_or("?")
                .to_string(),
        }
    }

    /// Agrupa las medidas por consulta.
    ///
    /// Dos medidas comparten consulta solo si comparten las hojas Y la forma de salida. Las
    /// hojas solas no alcanzan: `Qty` es `SUM(qty)` y `Condicional` es `IF(SUM(qty) > 50, ...)`
    /// sobre la MISMA hoja, y dan numeros distintos. Compartiendo plan, la segunda leia la
    /// columna de la primera y salia con el valor de `Qty`.
    ///
    /// `query_count` y `execute` llaman a esto para no divergir: mientras fueron dos bucles
    /// separados, la cuenta decia una consulta y la ejecucion hacia otra.
    fn plans(&self) -> Result<Vec<(AtomSet, Vec<usize>)>, ReportError> {
        let mut plans: Vec<(AtomSet, Vec<usize>)> = Vec::new();
        for (index, measure) in self.measure_definitions()?.into_iter().enumerate() {
            let atoms = datan_dax_eval::extract_atoms(
                &measure.expression,
                &self.model,
                &self.filters,
                &self.dimensions,
            )
            .map_err(|error| ReportError::Interp(error.to_string()))?;
            match plans.iter_mut().find(|(set, _)| {
                set.signature() == atoms.signature() && set.outputs == atoms.outputs
            }) {
                Some((_, members)) => members.push(index),
                None => plans.push((atoms, vec![index])),
            }
        }
        Ok(plans)
    }

    /// Corre el informe.
    ///
    /// `source` solo se usa para las medidas que caen al interprete. Sin el, un informe con
    /// un `CALCULATE` falla en vez de dar un numero: es preferible a calcularlo con un origen
    /// vacio, que sale BLANK y parece una celda sin datos.
    pub fn execute(
        &self,
        engine: &Engine,
        source: Option<&dyn datan_dax_eval::DataSource>,
    ) -> Result<Matrix, ReportError> {
        if self.measures.is_empty() {
            return Err(ReportError::NoMeasures);
        }
        let sources = SourceMap::from_model(&self.model).with_qualifier(&self.qualifier);
        let headers = self.headers()?;

        // Un plan por conjunto de atomos. Cada plan lleva la lista de medidas que lee, en
        // el orden en que deben salir en la matriz.
        let plans = self.plans()?;

        let width = headers.len();
        let con_subtotales = self.subtotales && self.dimensions.len() >= 2;
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut kinds: Vec<RowKind> = Vec::new();

        for (atoms, members) in &plans {
            // Sin forma no hay valor que pegar. El camino viejo emitia una columna por atomo
            // y pegaba la primera en la columna de la medida: `ABS([Qty])` mostraba el valor
            // de `SUM(qty)`, sin error y sin aviso. Con forma, `CALCULATE`, los iteradores y
            // las escalares registradas si tienen la suya; sin ella, el informe dice que no
            // sabe calcular esa medida.
            if atoms.outputs.is_empty() {
                return Err(self.no_shape(members));
            }
            // Cada plan trae las dimensiones primero y despues SUS PROPIAS medidas. El
            // numero de columnas de medida es el de `members`, que puede ser uno o varios.
            let measure_columns = atoms.outputs.len();
            let produced = self.dimensions.len() + measure_columns;

            let (cells, kinds_del_plan) = if atoms.needs_interpreter() {
                let source = source.ok_or_else(|| ReportError::NoSource {
                    measure: members
                        .first()
                        .and_then(|index| self.measures.get(*index))
                        .map(|id| self.measure_name(*id).unwrap_or("?").to_string())
                        .unwrap_or_else(|| "?".to_string()),
                })?;
                let interpreter = Interpreter::new(&self.model, source);
                if con_subtotales {
                    subtotales_del_interprete(&interpreter, atoms, &self.filters, &self.dimensions)?
                } else {
                    let grouped = interpreter
                        .evaluate_grouped(
                            &atoms.atoms,
                            &atoms.outputs,
                            &self.filters,
                            &self.dimensions,
                            MAX_CELLS,
                        )
                        .map_err(|error| ReportError::Interp(error.to_string()))?;
                    let filas = grouped
                        .rows
                        .iter()
                        .map(|row| row.iter().map(scalar_to_text).collect())
                        .collect::<Vec<Vec<String>>>();
                    let tipos = vec![RowKind::Data; filas.len()];
                    (filas, tipos)
                }
            } else {
                let plan = if con_subtotales {
                    datan_dax_sql::lower_with_subtotals(&self.model, &sources, atoms, &self.filters)
                } else {
                    lower(&self.model, &sources, atoms, &self.filters)
                }
                .map_err(|error| ReportError::Interp(error.to_string()))?;
                let crudas = engine
                    .render_rows(&plan.sql)
                    .map_err(|error| ReportError::Query(error.to_string()))?;
                if con_subtotales {
                    separar_banderas(crudas, self.dimensions.len(), produced)?
                } else {
                    let tipos = vec![RowKind::Data; crudas.len()];
                    (crudas, tipos)
                }
            };

            for row in &cells {
                if row.len() != produced {
                    return Err(ReportError::ColumnCount {
                        expected: produced,
                        got: row.len(),
                    });
                }
            }

            if kinds.is_empty() {
                kinds = kinds_del_plan;
            } else if kinds != kinds_del_plan {
                return Err(ReportError::Query(
                    "dos medidas del informe produjeron renglones de tipo distinto. Con el \
                     mismo filtro y agrupamiento tienen que ser las mismas"
                        .to_string(),
                ));
            }

            if rows.is_empty() {
                // La primera consulta aporta las DIMENSIONES y deja las medidas en su
                // sitio vacias. Pegarlas despues, con el mismo bucle que las demas
                // consultas, es lo que hace que una medida que comparte consulta con otra
                // no salga en blanco.
                rows = cells
                    .iter()
                    .map(|cell| {
                        let mut full: Vec<String> =
                            cell.iter().take(self.dimensions.len()).cloned().collect();
                        full.resize(width, String::new());
                        full
                    })
                    .collect();
            } else if cells.len() != rows.len() {
                // Dos consultas con el mismo filtro y agrupamiento tienen las mismas filas.
                // Si no, el informe esta mal formado y pegar por posicion daria una matriz
                // con las medidas de una columna mezcladas con las de otra.
                return Err(ReportError::Query(format!(
                    "dos medidas del informe produjeron {} y {} filas. Con el mismo filtro y \\
                     agrupamiento tienen que ser las mismas",
                    rows.len(),
                    cells.len()
                )));
            }

            // Pegar las medidas SIEMPRE, tambien las de la primera consulta. Antes solo se
            // pegaban a partir de la segunda, y la matriz salia con la primera tanda de
            // medidas en blanco: cuatro columnas a la vista y ningun numero.
            let produced_measures = produced.saturating_sub(self.dimensions.len());
            for (row, cell) in rows.iter_mut().zip(&cells) {
                for (offset, member) in members.iter().enumerate() {
                    let target = self.dimensions.len() + member;
                    // Varias medidas con el MISMO conjunto de atomos comparten consulta y
                    // dan el MISMO numero. Se leen de la ultima columna de medidas que el
                    // plan produjo, no de la que les tocaria por posicion: sin esto, la
                    // segunda medida del grupo se queda en blanco y parece un dato que no
                    // existe.
                    let source = self.dimensions.len() + offset.min(produced_measures - 1);
                    if target < row.len() && source < cell.len() {
                        row[target] = cell[source].clone();
                    }
                }
            }
        }

        Ok(Matrix {
            headers,
            rows,
            kinds,
            truncated: false,
        })
    }
}

/// Que renglon dicen las banderas de `GROUPING`: una por dimension, al final de la fila.
///
/// Las recogidas son las dimensiones que el renglon NO desglosa. Todas: gran total. Ninguna:
/// dato. Las demas: el subtotal del nivel mas profundo que conserva, que es `dims - recogidas
/// - 1`.
fn separar_banderas(
    crudas: Vec<Vec<String>>,
    dims: usize,
    produced: usize,
) -> Result<(Vec<Vec<String>>, Vec<RowKind>), ReportError> {
    let mut filas = Vec::with_capacity(crudas.len());
    let mut kinds = Vec::with_capacity(crudas.len());
    for mut fila in crudas {
        if fila.len() != produced + dims {
            return Err(ReportError::ColumnCount {
                expected: produced + dims,
                got: fila.len(),
            });
        }
        let recogidas = fila.drain(produced..).filter(|c| c == "1").count();
        let kind = match recogidas {
            0 => RowKind::Data,
            n if n == dims => RowKind::GrandTotal,
            n => RowKind::Subtotal {
                level: dims - n - 1,
            },
        };
        // La dimension recogida sale como texto "NULL" y queda VACIA: un subtotal no
        // desglosa esa dimension, y un "NULL" pintado parece un dato que no existe.
        let desde = match kind {
            RowKind::Data => usize::MAX,
            RowKind::Subtotal { level } => level + 1,
            RowKind::GrandTotal => 0,
        };
        for celda in fila.iter_mut().take(dims).skip(desde) {
            *celda = String::new();
        }
        kinds.push(kind);
        filas.push(fila);
    }
    Ok((filas, kinds))
}

/// Los renglones de una matriz con subtotales por el camino del interprete.
///
/// Un nivel por prefijo de dimensiones: el nivel `l` agrupa por las primeras `l+1`. El
/// recorrido es el de la matriz: por cada grupo, su subarbol y DESPUES su subtotal; al final,
/// el gran total sin dimensiones. Dos lecturas por nivel, no una por grupo.
fn subtotales_del_interprete(
    interpreter: &Interpreter,
    atoms: &datan_dax_eval::AtomSet,
    filters: &FilterContext,
    dims: &[ObjectId],
) -> Result<(Vec<Vec<String>>, Vec<RowKind>), ReportError> {
    let niveles: Vec<Vec<Vec<String>>> = (0..dims.len())
        .map(|l| {
            interpreter
                .evaluate_grouped(&atoms.atoms, &atoms.outputs, filters, &dims[..=l], MAX_CELLS)
                .map(|grouped| {
                    grouped
                        .rows
                        .iter()
                        .map(|row| row.iter().map(scalar_to_text).collect::<Vec<String>>())
                        .collect()
                })
                .map_err(|error| ReportError::Interp(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    fn rec(
        nivel: usize,
        prefijo: &[String],
        niveles: &[Vec<Vec<String>>],
        filas: &mut Vec<Vec<String>>,
        kinds: &mut Vec<RowKind>,
    ) {
        for fila in niveles[nivel]
            .iter()
            .filter(|f| f.len() >= prefijo.len() && f[..prefijo.len()] == *prefijo)
        {
            if nivel + 1 < niveles.len() {
                rec(nivel + 1, &fila[..=nivel], niveles, filas, kinds);
                filas.push(fila.clone());
                kinds.push(RowKind::Subtotal { level: nivel });
            } else {
                filas.push(fila.clone());
                kinds.push(RowKind::Data);
            }
        }
    }

    let mut filas = Vec::new();
    let mut kinds = Vec::new();
    rec(0, &[], &niveles, &mut filas, &mut kinds);

    // El gran total es OTRA consulta: sin agrupamiento, una fila de medidas.
    let totales = interpreter
        .evaluate_totals(&atoms.atoms, &atoms.outputs, filters)
        .map_err(|error| ReportError::Interp(error.to_string()))?;
    let mut fila = vec![String::new(); dims.len()];
    fila.extend(totales.iter().map(scalar_to_text));
    filas.push(fila);
    kinds.push(RowKind::GrandTotal);

    Ok((filas, kinds))
}

/// Una cabecera de columna.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub label: String,
    /// La columna del modelo. Para una medida es `ObjectId(0)`: una medida no es una columna.
    pub column: ObjectId,
    /// `Some` si la columna es una medida, `None` si es una dimension.
    pub measure: Option<ObjectId>,
    pub data_type: DataType,
}

impl Header {
    pub fn is_dimension(&self) -> bool {
        self.measure.is_none()
    }
}

/// Que renglon es cada fila de la matriz.
///
/// Con subtotales, una matriz no es una lista plana de datos: un renglon puede resumir un
/// grupo o el informe entero. El que pinta lo distingue (fondo, peso, sin clic) y el que
/// exporta decide si lo lleva; sin esta marca, un subtotal es una fila mas y pincharlo
/// filtraria por un valor que no existe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Un renglon del detalle.
    Data,
    /// El renglon que resume las dimensiones `0..=level` de su grupo.
    Subtotal { level: usize },
    /// El total del informe, sin dimensiones.
    GrandTotal,
}

/// La matriz del informe.
#[derive(Debug, Clone)]
pub struct Matrix {
    pub headers: Vec<Header>,
    /// Cada fila: primero los valores de dimension, luego los de medida.
    pub rows: Vec<Vec<String>>,
    /// Que renglon es cada fila, en el mismo orden. Mas corto que `rows` solo en una matriz
    /// construida a mano sin subtotales: todo lo que falte es `Data`.
    pub kinds: Vec<RowKind>,
    /// `true` si el informe tiene mas celdas de las que se muestran.
    pub truncated: bool,
}

impl Matrix {
    pub fn width(&self) -> usize {
        self.headers.len()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// El valor de una celda.
    pub fn cell(&self, row: usize, column: usize) -> Option<&str> {
        self.rows.get(row)?.get(column).map(String::as_str)
    }

    /// Que renglon es la fila. Las matrices sin subtotales ni siquiera llevan la lista.
    pub fn row_kind(&self, row: usize) -> RowKind {
        self.kinds.get(row).copied().unwrap_or(RowKind::Data)
    }

    /// Los primeros `n` valores de una columna de dimension, ordenados por una de medidas.
    ///
    /// Es lo que alimenta un grafico de barras. Las celdas que no parsean a numero se
    /// descartan: una dimension agrupada junto a una medida no numerica no es un grafico, es
    /// un error de seleccion que la UI debio evitar.
    pub fn top(&self, dimension: usize, measure: usize, n: usize) -> Vec<(String, f64)> {
        let mut values: Vec<(String, f64)> = self
            .rows
            .iter()
            .filter_map(|row| {
                let label = row.get(dimension)?.clone();
                let value = row.get(measure)?.parse::<f64>().ok()?;
                Some((label, value))
            })
            .collect();
        values.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        values.truncate(n);
        values
    }

    /// La cabecera de una columna.
    pub fn header(&self, column: usize) -> Option<&Header> {
        self.headers.get(column)
    }

    /// El indice de la dimension de una columna, si es una dimension.
    ///
    /// Es lo que convierte un clic en un filtro: la celda `(fila, columna)` es un valor de
    /// la dimension `dimensions[columna]`.
    pub fn dimension_index(&self, column: usize) -> Option<usize> {
        let header = self.headers.get(column)?;
        if !header.is_dimension() {
            return None;
        }
        Some(
            self.headers
                .iter()
                .take(column)
                .filter(|h| h.is_dimension())
                .count(),
        )
    }

    /// La matriz como tabla de texto.
    ///
    /// Imprime las primeras filas, no todas: un informe con 5000 celdas llenaria la terminal
    /// y el numero que interesa se ve en las primeras.
    pub fn print(&self, limit: usize) {
        let width = self.width();
        let pad = |value: String| -> String {
            let mut out: String = value.chars().take(44).collect();
            while out.chars().count() < width {
                out.push(' ');
            }
            out
        };

        let header: Vec<String> = self
            .headers
            .iter()
            .map(|h| pad(h.label.clone()))
            .collect();
        println!("  {}", header.join(" | "));

        for row in self.rows.iter().take(limit) {
            let cells: Vec<String> = (0..width)
                .map(|column| pad(row.get(column).cloned().unwrap_or_default()))
                .collect();
            println!("  {}", cells.join(" | "));
        }
        if self.rows.len() > limit {
            println!("  ... y {} filas mas", self.rows.len() - limit);
        }
    }

    /// El filtro que produce pinchar `(fila, columna)`, o `None` si la celda no es de una
    /// dimension.
    pub fn drill(&self, report: &Report, row: usize, column: usize) -> Option<Report> {
        let dimension = self.dimension_index(column)?;
        let column_id = *report.dimensions.get(dimension)?;
        let value = self.cell(row, column)?.to_string();
        let filter = match self.headers[column].data_type {
            DataType::Text => FilterValue::Text(value),
            DataType::Date => FilterValue::Date(value.parse().ok()?),
            _ => FilterValue::Integer(value.parse().ok()?),
        };
        Some(report.filtered_by(column_id, filter))
    }
}

/// Un escalar del interprete como texto, con la misma convencion que el SQL.
///
/// La comparacion entre caminos es de cadenas. Si los dos usan redondeos distintos, todos
/// los promedios divergen y el arnes se queja de un motor que esta bien.
pub fn scalar_to_text(value: &Scalar) -> String {
    match value {
        Scalar::Blank => String::new(),
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