//! La prueba que decide si el proyecto tiene dos caminos o uno.
//!
//! Corre la suite de conformidad por el fast path SQL y por el interprete, y compara.
//! Si divergen, la divergencia tiene que estar en la suite, no en un archivo de resultados.

use datan_testkit::{engine, run_suite, suite};

fn main() {
    println!("Datan: suite de conformidad diferencial\n");

    let engine = engine().expect("carga fixtures");
    let cases = suite();
    let results = run_suite(&engine, &cases);

    let mut divergen = 0usize;
    let mut sin_sql = 0usize;
    let mut fallidos = 0usize;
    let mut nulls = 0usize;

    println!(
        "{:<32} {:>14} {:>14}  {}",
        "caso", "SQL", "interprete", ""
    );
    println!("{}", "-".repeat(76));

    for result in &results {
        if let Some(error) = &result.error {
            fallidos += 1;
            println!("{:<32} {}", result.label, format!("ERROR: {error}"));
            continue;
        }
        // `NULL` y `sin SQL` son estados distintos y la version anterior los confundia:
        // una consulta que devuelve NULL se contaba como "sin fast path". Un `SUM` sobre
        // cero filas es NULL, asi que todos los casos de rango sin ventas se reportaban
        // como si el interprete fuera el unico camino, sin que nadie lo notara.
        let sql = match (&result.sql, &result.sql_value) {
            (Some(_), Some(value)) => value.clone(),
            (Some(_), None) => {
                nulls += 1;
                "NULL".to_string()
            }
            (None, _) => {
                sin_sql += 1;
                "sin SQL".to_string()
            }
        };
        let interp = match &result.interp_value {
            Some(value) => format!("{value:?}"),
            None => "ninguno".to_string(),
        };
        let mark = if !result.agrees {
            divergen += 1;
            "DIFIERE"
        } else if result.interpreter_only {
            "solo int."
        } else if result.sql_value.is_none() {
            "NULL"
        } else {
            "ok"
        };
        println!("{:<32} {:>14} {:>14}  {mark}", result.label, sql, interp);
    }

    let verificados = results.len() - sin_sql - divergen - fallidos;
    println!(
        "\n{} casos: {verificados} por los dos caminos, {sin_sql} solo interprete, \
         {nulls} con NULL en SQL, {divergen} divergen, {fallidos} con error",
        results.len()
    );

    if fallidos > 0 {
        println!("\n  FALLA. {fallidos} caso(s) no se pudieron correr.");
        println!("  Un caso que error no es un caso que no cuenta: el recuento de arriba");
        println!("  lo excluye, y sin el error a la vista el suite parece mas corto.");
        std::process::exit(1);
    }

    if divergen == 0 {
        println!("\n  PASA. Donde hay fast path, los dos caminos dicen lo mismo.");
        println!("  Los casos de CALCULATE van solo por interprete, que es lo que deben hacer.");
    } else {
        println!("\n  FALLA. Los dos caminos NO coinciden.");
        println!("  Cada divergencia es una medida que daria un numero distinto segun");
        println!("  que camino tome. Hay que arreglar el motor antes de seguir.");
        std::process::exit(1);
    }
}