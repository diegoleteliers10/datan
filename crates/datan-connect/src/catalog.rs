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
    Cardinality, FilterDirection, ObjectId, Relationship, RelationshipRole,
};

use crate::ConnectError;

/// Una clave foranea, tal y como la cuenta Postgres.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForeignKey {
    pub constraint: String,
    pub column_count: i32,
    pub source_is_unique: bool,
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
        let tls = native_tls::TlsConnector::new().map_err(|error| {
            ConnectError::Catalog(format!("no se pudo preparar TLS para Postgres: {error}"))
        })?;
        let connector = postgres_native_tls::MakeTlsConnector::new(tls);
        let mut client = postgres::Client::connect(config, connector).map_err(|error| {
            ConnectError::Catalog(format!("no se pudo conectar a Postgres: {error}"))
        })?;
        client
            .simple_query("SELECT 1")
            .map_err(|error| ConnectError::Catalog(format!("la conexion no responde: {error}")))?;
        Ok(Self { client })
    }

    /// Abre el catálogo con el modo SSL/TLS elegido para la conexión de datos.
    pub fn connect_with_sslmode(config: &str, sslmode: &str) -> Result<Self, ConnectError> {
        let mut client = match sslmode.to_ascii_lowercase().as_str() {
            "disable" => postgres::Client::connect(config, postgres::NoTls),
            "prefer" | "require" => {
                let mut builder = native_tls::TlsConnector::builder();
                // `prefer` intenta cifrar y puede volver a texto plano. `require` exige TLS.
                // Ninguno valida el certificado ni el nombre del servidor.
                builder
                    .danger_accept_invalid_certs(true)
                    .danger_accept_invalid_hostnames(true);
                let tls = builder.build().map_err(|error| {
                    ConnectError::Catalog(format!("no se pudo preparar TLS para Postgres: {error}"))
                })?;
                postgres::Client::connect(config, postgres_native_tls::MakeTlsConnector::new(tls))
            }
            _ => {
                return Err(ConnectError::Catalog(format!(
                    "modo SSL/TLS no válido: {sslmode}"
                )));
            }
        }
        .map_err(|error| {
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
                con.conname,
                cardinality(con.conkey),
                src_ns.nspname,
                src.relname,
                src_attr.attname,
                dst_ns.nspname,
                dst.relname,
                dst_attr.attname,
                CASE WHEN cardinality(con.conkey) = 1 THEN EXISTS (
                    SELECT 1
                    FROM pg_catalog.pg_index unique_idx
                    WHERE unique_idx.indrelid = src.oid
                      AND unique_idx.indisunique
                      AND unique_idx.indisvalid
                      AND unique_idx.indpred IS NULL
                      AND unique_idx.indnkeyatts = 1
                      AND unique_idx.indkey[0] = key.attnum
                ) ELSE false END
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
            .map_err(|error| ConnectError::Query {
                sql: SQL.to_string(),
                error: error.to_string(),
            })?;

        Ok(rows
            .iter()
            .map(|row| ForeignKey {
                constraint: row.get(0),
                column_count: row.get(1),
                schema: row.get(2),
                table: row.get(3),
                column: row.get(4),
                foreign_schema: row.get(5),
                foreign_table: row.get(6),
                foreign_column: row.get(7),
                source_is_unique: row.get(8),
            })
            .collect())
    }

    /// Las claves primarias, para marcar unicidad.
    ///
    /// Sin clave primaria declarada no se puede afirmar que una dimension tenga una clave, y
    /// un `DISTINCTCOUNT` sobre una columna cualquiera no es lo mismo que contar filas de una
    /// dimension. Sin esta informacion, el modelo no dice nada falso: no dice nada.
    pub fn primary_keys(
        &mut self,
    ) -> Result<BTreeMap<(String, String), Vec<String>>, ConnectError> {
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

        let rows = self
            .client
            .query(SQL, &[])
            .map_err(|error| ConnectError::Query {
                sql: SQL.to_string(),
                error: error.to_string(),
            })?;

        let mut out: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for row in &rows {
            out.entry((row.get(0), row.get(1)))
                .or_default()
                .push(row.get(2));
        }
        Ok(out)
    }

}

/// Convierte relaciones de una sola columna del catalogo al modelo.
///
/// La columna referenciada queda en `from` y la FK en `to`. Las FKs compuestas se
/// omiten porque el modelo actual no puede guardar pares de columnas como una relacion.
pub fn relationships_from_catalog(
    column_ids: &BTreeMap<(String, String, String), ObjectId>,
    foreign_keys: &[ForeignKey],
) -> Vec<Relationship> {
    foreign_keys
        .iter()
        .filter(|key| key.column_count == 1)
        .filter_map(|key| {
            let from = column_ids.get(&(
                key.foreign_schema.clone(),
                key.foreign_table.clone(),
                key.foreign_column.clone(),
            ));
            let to = column_ids.get(&(key.schema.clone(), key.table.clone(), key.column.clone()));
            let (Some(from), Some(to)) = (from, to) else {
                return None;
            };
            Some(Relationship {
                from_column: *from,
                to_column: *to,
                cardinality: if key.source_is_unique {
                    Cardinality::OneToOne
                } else {
                    Cardinality::OneToMany
                },
                direction: FilterDirection::Single,
                role: RelationshipRole::Active,
                assume_referential_integrity: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn foreign_key(column_count: i32, source_is_unique: bool) -> ForeignKey {
        ForeignKey {
            constraint: "fk_orders_customer".to_string(),
            column_count,
            source_is_unique,
            schema: "public".to_string(),
            table: "orders".to_string(),
            column: "customer_id".to_string(),
            foreign_schema: "public".to_string(),
            foreign_table: "customers".to_string(),
            foreign_column: "id".to_string(),
        }
    }

    fn column_ids() -> BTreeMap<(String, String, String), ObjectId> {
        BTreeMap::from([
            (
                ("public".to_string(), "customers".to_string(), "id".to_string()),
                ObjectId(10),
            ),
            (
                (
                    "public".to_string(),
                    "orders".to_string(),
                    "customer_id".to_string(),
                ),
                ObjectId(20),
            ),
        ])
    }

    #[test]
    fn foreign_key_uses_referenced_to_foreign_orientation_and_cardinality() {
        let relationships = relationships_from_catalog(&column_ids(), &[foreign_key(1, false)]);

        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].from_column, ObjectId(10));
        assert_eq!(relationships[0].to_column, ObjectId(20));
        assert_eq!(relationships[0].cardinality, Cardinality::OneToMany);
    }

    #[test]
    fn unique_foreign_key_is_one_to_one() {
        let relationships = relationships_from_catalog(&column_ids(), &[foreign_key(1, true)]);

        assert_eq!(relationships[0].cardinality, Cardinality::OneToOne);
    }

    #[test]
    fn composite_foreign_key_does_not_create_false_single_column_joins() {
        let relationships = relationships_from_catalog(&column_ids(), &[foreign_key(2, false)]);

        assert!(relationships.is_empty());
    }
}
