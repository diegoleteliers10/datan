//! Modelo semantico de Datan: tablas, columnas, lineage, relaciones, jerarquias, medidas, RLS.
//!
//! Puro. Sin duckdb, sin gpui, sin IO. `cargo test` corre en milisegundos.
//!
//! Este crate arranca con lo que el motor DAX necesita y nada mas. Cada tipo existe porque
//! el algebra de filter context lo exige.

/// Identificador estable de un objeto del modelo.
///
/// Power BI usa lineage tags (UUID) para que un filtro solo se propague a columnas del mismo
/// linaje. Datan necesita lo mismo desde el dia 1: sin tags, propagar filtros entre tablas es
/// ambiguo y el algebra de la Fase 2 no se puede escribir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub u64);

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Tipo de dato de una columna.
///
/// `Moneda` existe porque el dominio real del usuario guarda los montos en enteros de pesos
/// sin centimos. Un `NUMERIC(10,2)` de Power BI lo destruye. Ver la sesion de Oct 2026.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Text,
    Integer,
    Decimal,
    Date,
    DateTime,
    Boolean,
    /// Entero de moneda, sin decimales.
    Moneda,
}

impl DataType {
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            DataType::Integer | DataType::Decimal | DataType::Moneda
        )
    }

    /// Tipo que corresponde en SQL de DuckDB.
    pub fn to_sql(self) -> &'static str {
        match self {
            DataType::Text => "VARCHAR",
            DataType::Integer => "BIGINT",
            DataType::Decimal => "DOUBLE",
            DataType::Date => "DATE",
            DataType::DateTime => "TIMESTAMP",
            DataType::Boolean => "BOOLEAN",
            DataType::Moneda => "BIGINT",
        }
    }
}

/// Cardinalidad de una relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    /// Uno a uno.
    OneToOne,
    /// Varios hechos a una dimension. El caso normal de un star schema.
    ManyToOne,
    /// Una dimension a varios hechos.
    OneToMany,
    /// Varios a varios. Power BI necesita `assume_referential_integrity` para propagar.
    ManyToMany,
}

/// Direccion en que una relationship filtra.
///
/// Bidireccional produce ciclos cuando el grafo se encadena Dim -> Fact -> Bridge -> Fact.
/// El backend SQL no puede expresar eso, y por eso cae al interprete. Ver PLAN.md seccion 3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterDirection {
    /// Solo la dimension filtra los hechos. Es el default de Power BI.
    Single,
    /// Ambos lados filtran.
    Both,
    /// Ningun lado filtra. Se usa con `USERELATIONSHIP`.
    None,
}

/// Rol de una relationship en la propagacion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationshipRole {
    /// Filtra con el contexto actual.
    Active,
    /// Existe pero no filtra hasta que `USERELATIONSHIP` la activa.
    Inactive,
}

/// Una relationship entre la columna de una dimension y la de un hecho.
///
/// `from` es el lado de la dimension, `to` el lado de los hechos. Esa convencion es la de
/// Power BI y hace que el algebra sea mas simple de leer.
#[derive(Debug, Clone)]
pub struct Relationship {
    pub from_column: ObjectId,
    pub to_column: ObjectId,
    pub cardinality: Cardinality,
    pub direction: FilterDirection,
    pub role: RelationshipRole,
    /// Power BI exige `AssumeReferentialIntegrity` para propagar por una relationship
    /// many-to-many en vez de resolver por materializacion.
    pub assume_referential_integrity: bool,
}

/// Una columna de una tabla del modelo.
#[derive(Debug, Clone)]
pub struct Column {
    pub id: ObjectId,
    pub name: String,
    pub data_type: DataType,
    /// Columna por la que se ordena, si esta no es la clave natural. Es la `sortByColumn`
    /// de Power BI: rompe la afinidad de tipo.
    pub sort_by: Option<ObjectId>,
    pub is_hidden: bool,
}

impl Column {
    pub fn new(id: ObjectId, name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            id,
            name: name.into(),
            data_type,
            sort_by: None,
            is_hidden: false,
        }
    }
}

/// Una tabla del modelo. Una tabla de hechos o una dimension.
#[derive(Debug, Clone)]
pub enum TableKind {
    Facts,
    Dimension,
    /// Tabla calculada por `SUMMARIZECOLUMNS`. No tiene filas propias hasta el refresh.
    Calculated,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub id: ObjectId,
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<Column>,
    /// Columnas que forman la clave de la tabla.
    pub key_columns: Vec<ObjectId>,
    /// Marca la tabla de fechas para las medidas de time intelligence.
    pub is_date_table: bool,
}

impl Table {
    pub fn column(&self, id: ObjectId) -> Option<&Column> {
        self.columns.iter().find(|c| c.id == id)
    }

    pub fn column_by_name(&self, name: &str) -> Option<&Column> {
        self.columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }
}

/// Una medida. Es una expresion DAX evaluada por celda de grupo, no por fila.
#[derive(Debug, Clone)]
pub struct Measure {
    pub id: ObjectId,
    pub name: String,
    /// Texto DAX sin parsear. El AST vive en `datan-dax-syntax`; aqui queda la fuente para
    /// el linter y el diff.
    pub expression: String,
    /// Formato de presentacion, como el `Format String` de Power BI.
    pub format_string: Option<String>,
    pub description: Option<String>,
}

impl Measure {
    pub fn new(id: ObjectId, name: impl Into<String>, expression: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            expression: expression.into(),
            format_string: None,
            description: None,
        }
    }
}

/// El modelo completo.
///
/// Es inmutable una vez construido. `datan-dax-eval` lo toma por referencia y produce el
/// Measure IR sin tocarlo, lo que permitecachear el plan por combinacion de modelo y filtro.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub tables: Vec<Table>,
    pub measures: Vec<Measure>,
    pub relationships: Vec<Relationship>,
}

impl Model {
    pub fn table(&self, id: ObjectId) -> Option<&Table> {
        self.tables.iter().find(|t| t.id == id)
    }

    pub fn table_by_name(&self, name: &str) -> Option<&Table> {
        self.tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    pub fn measure_by_name(&self, name: &str) -> Option<&Measure> {
        self.measures
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
    }

    pub fn column(&self, id: ObjectId) -> Option<(&Table, &Column)> {
        self.tables.iter().find_map(|table| {
            table.column(id).map(|column| (table, column))
        })
    }

    pub fn column_by_name(&self, table: &str, column: &str) -> Option<(&Table, &Column)> {
        self.table_by_name(table)
            .and_then(|t| t.column_by_name(column).map(|c| (t, c)))
    }

    /// La tabla de fechas marcada, si el modelo tiene una. Toda la time intelligence
    /// depende de que exista: DAX no la exige, pero sin ella no hay periodos comparables.
    pub fn date_table(&self) -> Option<&Table> {
        self.tables.iter().find(|t| t.is_date_table)
    }

    /// Relationships que propagan en el contexto actual.
    pub fn active_relationships(&self) -> impl Iterator<Item = &Relationship> {
        self.relationships
            .iter()
            .filter(|r| r.role == RelationshipRole::Active && r.direction != FilterDirection::None)
    }

    /// El camino mas corto entre dos tablas por el grafo de relaciones.
    ///
    /// BFS sobre las relaciones activas. Las aristas vienen en orden, desde `from` hacia
    /// `to`. Un camino vacio significa que `from` ES `to`.
    ///
    /// Vive aqui y no en el lowering a proposito: el interprete hace la misma pregunta para
    /// resolver una agregacion que cruza tablas, y dos BFS divergen en cuanto uno filtra una
    /// relationship y el otro no. Es el tipo de divergencia que no se ve hasta que un informe
    /// da un numero distinto en cada camino.
    ///
    /// `None` significa que el modelo esta desconectado. No es un error de sintaxis: es un
    /// modelo mal formado, y quien llama tiene que decirselo al usuario.
    pub fn join_path(&self, from: ObjectId, to: ObjectId) -> Option<Vec<JoinEdge>> {
        if from == to {
            return Some(Vec::new());
        }

        let mut queue: std::collections::VecDeque<(ObjectId, Vec<JoinEdge>)> =
            std::collections::VecDeque::new();
        let mut visited = vec![from];
        queue.push_back((from, Vec::new()));

        while let Some((table, path)) = queue.pop_front() {
            for relationship in &self.relationships {
                if relationship.role != RelationshipRole::Active {
                    continue;
                }
                let from_table = self.column(relationship.from_column).map(|(t, _)| t.id);
                let to_table = self.column(relationship.to_column).map(|(t, _)| t.id);

                // La relationship se recorre en los dos sentidos: un JOIN no distingue
                // dimension de hecho. El Sentido lo codifica el ORDEN de las columnas, no
                // la relationship, asi que cada direccion es una arista distinta.
                let directions = [
                    (
                        from_table,
                        JoinEdge {
                            from_column: relationship.from_column,
                            to_column: relationship.to_column,
                        },
                    ),
                    (
                        to_table,
                        JoinEdge {
                            from_column: relationship.to_column,
                            to_column: relationship.from_column,
                        },
                    ),
                ];

                for (source, edge) in directions {
                    let Some(source) = source else { continue };
                    if source != table {
                        continue;
                    }
                    let Some(destination) = self.column(edge.to_column).map(|(t, _)| t.id) else {
                        continue;
                    };
                    if destination == to {
                        let mut full = path.clone();
                        full.push(edge);
                        return Some(full);
                    }
                    if visited.contains(&destination) {
                        continue;
                    }
                    visited.push(destination);
                    let mut next = path.clone();
                    next.push(edge);
                    queue.push_back((destination, next));
                }
            }
        }

        None
    }
}

/// Un arista del camino de join: la relationship que conecta dos columnas.
///
/// `from_column` es la del lado desde el que se parte y `to_column` la del lado al que se
/// llega. El orden ES la direccion.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinEdge {
    pub from_column: ObjectId,
    pub to_column: ObjectId,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El star schema de prueba. 10M de ventas, 500 productos, 5000 clientes.
    fn modelo() -> Model {
        let fact = ObjectId(1);
        let product = ObjectId(2);
        let customer = ObjectId(3);
        let date = ObjectId(4);

        Model {
            tables: vec![
                Table {
                    id: fact,
                    name: "Fact Sales".to_string(),
                    kind: TableKind::Facts,
                    columns: vec![
                        Column::new(ObjectId(10), "SaleId", DataType::Integer),
                        Column::new(ObjectId(11), "Qty", DataType::Integer),
                        Column::new(ObjectId(12), "ProductId", DataType::Integer),
                        Column::new(ObjectId(13), "CustomerId", DataType::Integer),
                        Column::new(ObjectId(14), "SaleDate", DataType::Date),
                    ],
                    key_columns: vec![ObjectId(10)],
                    is_date_table: false,
                },
                Table {
                    id: product,
                    name: "Dim Product".to_string(),
                    kind: TableKind::Dimension,
                    columns: vec![
                        Column::new(ObjectId(20), "ProductId", DataType::Integer),
                        Column::new(ObjectId(21), "Product", DataType::Text),
                        Column::new(ObjectId(22), "Category", DataType::Text),
                        Column::new(ObjectId(23), "Region", DataType::Text),
                        Column::new(ObjectId(24), "Price", DataType::Moneda),
                    ],
                    key_columns: vec![ObjectId(20)],
                    is_date_table: false,
                },
                Table {
                    id: customer,
                    name: "Dim Customer".to_string(),
                    kind: TableKind::Dimension,
                    columns: vec![
                        Column::new(ObjectId(30), "CustomerId", DataType::Integer),
                        Column::new(ObjectId(31), "Customer", DataType::Text),
                    ],
                    key_columns: vec![ObjectId(30)],
                    is_date_table: false,
                },
                Table {
                    id: date,
                    name: "Dim Date".to_string(),
                    kind: TableKind::Dimension,
                    columns: vec![
                        Column::new(ObjectId(40), "Date", DataType::Date),
                        Column::new(ObjectId(41), "Year", DataType::Integer),
                        Column::new(ObjectId(42), "Month", DataType::Integer),
                        Column::new(ObjectId(43), "Quarter", DataType::Text),
                    ],
                    key_columns: vec![ObjectId(40)],
                    is_date_table: true,
                },
            ],
            measures: vec![
                Measure::new(ObjectId(100), "Revenue", "SUM(Fact Sales[Qty])"),
                Measure::new(ObjectId(101), "Qty", "SUM('Fact Sales'[Qty])"),
            ],
            relationships: vec![
                Relationship {
                    from_column: ObjectId(20),
                    to_column: ObjectId(12),
                    cardinality: Cardinality::OneToMany,
                    direction: FilterDirection::Single,
                    role: RelationshipRole::Active,
                    assume_referential_integrity: false,
                },
                Relationship {
                    from_column: ObjectId(30),
                    to_column: ObjectId(13),
                    cardinality: Cardinality::OneToMany,
                    direction: FilterDirection::Single,
                    role: RelationshipRole::Active,
                    assume_referential_integrity: false,
                },
                Relationship {
                    from_column: ObjectId(40),
                    to_column: ObjectId(14),
                    cardinality: Cardinality::OneToMany,
                    direction: FilterDirection::Single,
                    role: RelationshipRole::Active,
                    assume_referential_integrity: false,
                },
            ],
        }
    }

    #[test]
    fn busca_por_nombre_sin_distinguir_mayusculas() {
        let model = modelo();
        assert!(model.table_by_name("fact sales").is_some());
        assert!(model.measure_by_name("revenue").is_some());
        assert!(model.table_by_name("Dim Time").is_none());
    }

    #[test]
    fn resuelve_una_columna_por_tabla_y_nombre() {
        let model = modelo();
        let (table, column) = model.column_by_name("Dim Product", "Price").expect("columna");
        assert_eq!(table.id, ObjectId(2));
        assert_eq!(column.data_type, DataType::Moneda);
        assert!(column.data_type.is_numeric());
    }

    #[test]
    fn encuentra_la_tabla_de_fechas() {
        let model = modelo();
        let date = model.date_table().expect("tabla de fechas");
        assert_eq!(date.name, "Dim Date");
    }

    #[test]
    fn solo_propagan_las_relationships_activas() {
        let model = modelo();
        assert_eq!(model.active_relationships().count(), 3);

        let mut inactive = model.clone();
        inactive.relationships[0].role = RelationshipRole::Inactive;
        assert_eq!(inactive.active_relationships().count(), 2);
    }

    #[test]
    fn la_moneda_chilena_es_un_entero() {
        // El dominio real guarda los montos en enteros de pesos. Verificar que el tipo no
        // se degrada a decimal es un test de producto, no de implementacion.
        assert_eq!(DataType::Moneda.to_sql(), "BIGINT");
        assert!(DataType::Moneda.is_numeric());
        assert!(!DataType::Date.is_numeric());
    }
}