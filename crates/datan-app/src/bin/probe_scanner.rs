//! El scanner de Postgres devuelve menos filas que Postgres.
//!
//! `information_schema.key_column_usage` tiene 5 filas en `datan_bench`. A traves del scanner
//! de DuckDB llegan 3. Las dos que faltan son exactamente las claves foraneas.
//!
//! Un modelo construido con esa informacion no tiene relaciones: se ve como un informe que
//! cruza dimensiones y no cruza nada. Y no hay error, no hay excepcion, no hay codigo de
//! retorno distinto. Es la clase de fallo mas cara que existe en esta capa.
//!
//! Uso: cargo run --release -p datan-app --bin probe_scanner

use datan_storage::Engine;

fn main() {
    let home = std::env::var("HOME").expect("HOME");
    let root = std::path::Path::new(&home).join(".duckdb").join("extensions");
    let engine = Engine::open_with_bundled_extensions(":memory:", &root).expect("motor");
    engine.load_bundled_extension("postgres").expect("extension");
    engine
        .execute("ATTACH 'host=localhost dbname=datan_bench' AS pg (TYPE POSTGRES, READ_ONLY)")
        .expect("attach");

    println!("Datan: el scanner de Postgres devuelve todas las filas?\n");

    // El mismo catalogo, por dos caminos: `information_schema` (una vista hecha de UNION
    // y subconsultas) y `pg_catalog` (la tabla real). Si los dos caminos difieren, el
    // problema es la vista, no el scanner.
    let cases: [(&str, &str); 11] = [
        (
            "information_schema.key_column_usage",
            "SELECT constraint_name FROM information_schema.key_column_usage \
             WHERE table_schema = 'public' ORDER BY 1",
        ),
        (
            "pg_constraint (todas)",
            "SELECT conname FROM pg.pg_catalog.pg_constraint \
             WHERE connamespace = (SELECT oid FROM pg.pg_catalog.pg_namespace WHERE nspname = 'public') \
             ORDER BY 1",
        ),
        (
            "pg_constraint (solo foraneas)",
            "SELECT conname FROM pg.pg_catalog.pg_constraint \
             WHERE contype = 'f' \
             AND connamespace = (SELECT oid FROM pg.pg_catalog.pg_namespace WHERE nspname = 'public') \
             ORDER BY 1",
        ),
        (
            "information_schema sin filtro de esquema",
            "SELECT count(*) FROM information_schema.key_column_usage",
        ),
        (
            "information_schema.table_constraints",
            "SELECT constraint_name, constraint_type FROM information_schema.table_constraints \
             WHERE table_schema = 'public' ORDER BY 1",
        ),
        (
            "conteo de filas de la tabla de hechos",
            "SELECT count(*) FROM pg.public.product_boleta",
        ),
        (
            "duckdb_columns (catalogo nativo)",
            "SELECT schema_name, table_name, column_name, data_type \
             FROM duckdb_columns() WHERE database_name = 'pg' AND schema_name = 'public' ORDER BY 1, 2, 4",
        ),
        (
            "duckdb_tables",
            "SELECT schema_name, table_name, estimated_size \
             FROM duckdb_tables() WHERE database_name = 'pg' AND schema_name = 'public' ORDER BY 1, 2",
        ),
        (
            "constraintdef sin filtro",
            "SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE contype = 'f'",
        ),
        (
            "clases referenciadas por oid",
            "SELECT con.conname, src.relname, dst.relname
             FROM pg_constraint con
             JOIN pg_class src ON src.oid = con.conrelid
             JOIN pg_class dst ON dst.oid = con.confrelid
             WHERE con.contype = 'f'",
        ),
        (
            "duckdb_constraints",
            "SELECT table_name, constraint_type, constraint_text, \
                    constraint_column_names, referenced_table, referenced_column_names \
             FROM duckdb_constraints() WHERE database_name = 'pg' ORDER BY 1, 2",
        ),
    ];

    for (label, sql) in cases {
        print!("  {label:42} ");
        match engine.query_arrow(sql) {
            Err(error) => println!("ERROR {error}"),
            Ok(batches) => {
                let batch = batches.first();
                match batch {
                    None => println!("sin filas"),
                    Some(batch) => {
                        let mut out = Vec::new();
                        for row in 0..batch.num_rows() {
                            let cells: Vec<String> = (0..batch.num_columns())
                                .map(|c| text(batch, c, row))
                                .collect();
                            out.push(cells.join(" "));
                        }
                        println!("{}", out.join(" | "));
                    }
                }
            }
        }
    }

    println!();
    println!("  Comprobalo en psql:");
    println!("    psql -d datan_bench -c \"SELECT count(*) FROM information_schema.key_column_usage \\");
    println!("      WHERE table_schema = 'public'\"");
    println!();
    println!("  Si psql da 5 y el scanner da 3, la vista `information_schema` no sobrevive");
    println!("  al pushdown. La intrompecion tiene que leer `pg_catalog`, que es la tabla");
    println!("  real y no una vista de UNIONs.");
}

fn text(batch: &datan_storage::arrow::record_batch::RecordBatch, column: usize, row: usize) -> String {
    use datan_storage::arrow::array::{Array, Int64Array, StringArray};
    let array = batch.column(column);
    if array.null_count() > 0 && array.is_null(row) {
        return "NULL".to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        return values.value(row).to_string();
    }
    if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
        return values.value(row).to_string();
    }
    format!("{:?}", array.data_type())
}