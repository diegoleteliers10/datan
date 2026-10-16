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

    println!(
        "{:<32} {:>14} {:>14}  {}",
        "caso", "SQL", "interprete", ""
    );
    println!("{}", "-".repeat(76));

    for result in &results {
        let sql = match &result.sql_value {
            Some(value) => value.clone(),
            None => {
                sin_sql += 1;
                "sin SQL".to_string()
            }
        };
        let interp = match &result.interp_value {
            Some(value) => format!("{value:?}"),
            None => "ninguno".to_string(),
        };
        let mark = if !result.agrees {
            "DIFIERE"
        } else if result.interpreter_only {
            "solo int."
        } else {
            "ok"
        };
        if !result.agrees {
            divergen += 1;
        }
        println!("{:<32} {:>14} {:>14}  {mark}", result.label, sql, interp);
    }

    let verificados = results.len() - sin_sql - divergen;
    println!(
        "\n{} casos: {verificados} por los dos caminos, {sin_sql} solo interprete, {divergen} divergen",
        results.len()
    );

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