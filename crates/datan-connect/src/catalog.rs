//! El catálogo de Postgres, leído por una conexión directa.
//!
//! # Por qué existe
//!
//! El scanner de Postgres de DuckDB lee **datos** muy bien: un millón de filas en dos
//! milisegundos. Lo que no hace es leer el **catálogo**, y lo hace mal de una forma que no
//! se ve.
//!
//! Medido en `probe_scanner`, contra una base con dos claves foráneas:
//!
//! ```text
//! SELECT conname FROM pg_constraint WHERE contype = 'f'   -> 2 filas   FUNCIONA
//! lo mismo unido a pg_class por oid                        -> 0 filas   FALLA
//! information_schema.key_column_usage                      -> 3 filas, faltan las 2
//! duckdb_constraints()                                     -> solo NOT NULL
//! ```
//!
//! Sin error, sin excepción, sin código de retorno distinto. Solo filas de menos.
//!
//! # El reparto
//!
//! DuckDB lee los datos. Postgres lee el catálogo. No es una()-> y la otra: es que cada
//! herramienta es buena en una cosa, y un modelo sin relaciones no sirve. Un informe que
//! cruza productos por categoría devolvería el total del informe, no el de la categoría.
//!
//! # Lo que se lee del catálogo
//!
//! Solo las claves primarias y foráneas. Las columnas y los tipos se leen de
//! `duckdb_columns()`, que es materializado y no pierde filas. Traer tambien las columnas
//! por aqui duplicaria la fuente de verdad sin ganar nada.

use std::collections::BTreeMap;

use datan_model::{
    Cardinality, FilterDirection, Model, ObjectId, Relationship, RelationshipRole, TableKind,
};

use crate::ConnectError;

/// Una clave foranea, tal y como la cuenta Postgres.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForeignKey {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub foreign_schema: String,
    pub foreign_table: String,
    pub foreign_column: String,
}

/// Una conexion directa a Postgres, para leer el catalogo.
pub struct PostgresCatalog {
    client: postgres::Client,
}

impl PostgresCatalog {
    /// Abre una conexion y comprueba que la base responde.
    ///
    /// `config` es un DSN de libpq: `host=localhost dbname=datan_bench`. El mismo formato
    /// que acepta el `ATTACH` de DuckDB, para que el usuario escriba la conexion una vez.
    pub fn connect(config: &str) -> Result<Self, ConnectError> {
        let mut client = postgres::Client::connect(config, postgres::NoTls).map_err(|error| {
            ConnectError::Catalog(format!("no se pudo conectar a Postgres: {error}"))
        })?;
        client
            .simple_query("SELECT 1")
            .map_err(|error| ConnectError::Catalog(format!("la conexion no responde: {error}")))?;
        Ok(Self { client })
    }

    /// Las claves foraneas de los esquemas de usuario.
    ///
    /// La consulta va contra `pg_constraint`, que es la TABLA del catalogo, no contra
    /// `information_schema`, que son VISTAS hechas de UNION y subconsultas. La diferencia no
    /// es academica: el scanner de DuckDB pierde las filas de clave foranea de esas vistas,
    /// y por la conexion directa no hay scanner en medio.
    pub fn foreign_keys(&mut self) -> Result<Vec<ForeignKey>, ConnectError> {
        const SQL: &str = "SELECT
                src_ns.nspname,
                src.relname,
                src_attr.attname,
                dst_ns.nspname,
                dst.relname,
                dst_attr.attname
            FROM pg_catalog.pg_constraint con
            JOIN pg_catalog.pg_class src ON src.oid = con.conrelid
            JOIN pg_catalog.pg_namespace src_ns ON src_ns.oid = src.relnamespace
            JOIN pg_catalog.pg_class dst ON dst.oid = con.confrelid
            JOIN pg_catalog.pg_namespace dst_ns ON dst_ns.oid = dst.relnamespace
            JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS key(attnum, position) ON true
            JOIN pg_catalog.pg_attribute src_attr
                ON src_attr.attrelid = src.oid AND src_attr.attnum = key.attnum
            JOIN pg_catalog.pg_attribute dst_attr
                ON dst_attr.attrelid = dst.oid AND dst_attr.attnum = con.confkey[key.position]
            WHERE con.contype = 'f'
              AND src_ns.nspname NOT IN ('pg_catalog', 'information_schema')
            ORDER BY src_ns.nspname, src.relname, src_attr.attname";

        let rows = self
            .client
            .query(SQL, &[])
            .map_err(|error| {
                ConnectError::Query {
                    sql: SQL.to_string(),
                    error: error.to_string(),
                }
            })?;

        Ok(rows
            .iter()
            .map(|row| ForeignKey {
                schema: row.get(0),
                table: row.get(1),
                column: row.get(2),
                foreign_schema: row.get(3),
                foreign_table: row.get(4),
                foreign_column: row.get(5),
            })
            .collect())
    }

    /// Las claves primarias, para marcar unicidad.
    ///
    /// Sin clave primaria declarada no se puede afirmar que una dimension tenga una clave, y
    /// un `DISTINCTCOUNT` sobre una columna cualquiera no es lo mismo que contar filas de una
    /// dimension. Sin esta informacion, el modelo no dice nada falso: no dice nada.
    pub fn primary_keys(&mut self) -> Result<BTreeMap<(String, String), Vec<String>>, ConnectError> {
        const SQL: &str = "SELECT
                ns.nspname,
                tbl.relname,
                attr.attname
            FROM pg_catalog.pg_constraint con
            JOIN pg_catalog.pg_class tbl ON tbl.oid = con.conrelid
            JOIN pg_catalog.pg_namespace ns ON ns.oid = tbl.relnamespace
            JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS key(attnum, position) ON true
            JOIN pg_catalog.pg_attribute attr
                ON attr.attrelid = tbl.oid AND attr.attnum = key.attnum
            WHERE con.contype = 'p'
              AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
            ORDER BY ns.nspname, tbl.relname, key.position";

        let rows = self.client.query(SQL, &[]).map_err(|error| ConnectError::Query {
            sql: SQL.to_string(),
            error: error.to_string(),
        })?;

        let mut out: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for row in &rows {
            out.entry((row.get(0), row.get(1))).or_default().push(row.get(2));
        }
        Ok(out)
    }

    /// Convierte el catalogo en relaciones del modelo.
    ///
    /// `from_column` es SIEMPRE la clave de la tabla referenciada (la dimension) y
    /// `to_column` la clave foranea de la tabla que referencia. Una relationship al reves
    /// filtra los hechos por la dimension, que es lo que hace el `GROUP BY` y lo que espera
    /// el fast path.
    pub fn relationships(
        &mut self,
        model: &Model,
        column_ids: &BTreeMap<(String, String, String), ObjectId>,
        foreign_keys: &[ForeignKey],
    ) -> Result<Vec<Relationship>, ConnectError> {
        let mut out = Vec::new();
        for key in foreign_keys {
            let from = column_ids.get(&(
                key.foreign_schema.clone(),
                key.foreign_table.clone(),
                key.foreign_column.clone(),
            ));
            let to = column_ids.get(&(
                key.schema.clone(),
                key.table.clone(),
                key.column.clone(),
            ));
            let (Some(from), Some(to)) = (from, to) else {
                // La columna referenciada no esta en el modelo. Pasa cuando la constraint
                // cruza un esquema que el filtro de `duckdb_columns` dejo fuera.
                continue;
            };

            // La direccion la decide el CATALOGO, no una heuristica de nombres: la tabla
            // referenciada es la dimension. Una tabla que referencia a otra puede ser hechos
            // (`product_boleta` -> `catalogo_productos`) o una tabla puente, y las dos
            // relaciones son unidireccionales desde la referenciada.
            let referrer_is_fact = model
                .tables
                .iter()
                .find(|table| {
                    table.name == key.table && matches!(table.kind, TableKind::Facts)
                })
                .is_some();

            out.push(Relationship {
                from_column: *from,
                to_column: *to,
                cardinality: if referrer_is_fact {
                    Cardinality::OneToMany
                } else {
                    Cardinality::ManyToOne
                },
                // Unidireccional. Una relationship bidireccional que nadie pidio es como
                // aparecen los ciclos, y un ciclo no se puede bajar a SQL.
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                // `false` a proposito. Con `true` el motor podria asumir que no hay
                // huerfanos y unifying joins, que es mas rapido y silenciosamente mas
                // erroneo si los hay. Un numero que falta se nota; uno que se infiere no.
                assume_referential_integrity: false,
            });
        }

        Ok(out)
    }
}
