//! La prueba de CI: los dos caminos tienen que dar los mismos numeros.
//!
//! Si esto falla, el proyecto tiene dos motores que dicen cosas distintas sobre los mismos
//! datos. Eso no se arregla en la UI: se arregla aqui, antes de que llegue a un usuario.

use datan_testkit::{engine, run_case, run_suite, suite};

#[test]
fn el_fast_path_y_el_interprete_coinciden() {
    let engine = engine().expect("carga fixtures");
    let results = run_suite(&engine, &suite());

    let divergentes: Vec<String> = results
        .iter()
        .filter(|result| !result.agrees)
        .map(|result| {
            format!(
                "{}: SQL={:?} interprete={:?}",
                result.label, result.sql_value, result.interp_value
            )
        })
        .collect();

    assert!(
        divergentes.is_empty(),
        "el fast path SQL y el interprete NO coinciden:\n  {}",
        divergentes.join("\n  ")
    );
}

#[test]
fn la_suite_cubre_las_agregaciones_de_dax() {
    // Si la suite se reduce sin querer, esta prueba no lo detecta. El conteo es una
    //prueba de cobertura, no una prueba de que algo funciona.
    let engine = engine().expect("carga fixtures");
    let results = run_suite(&engine, &suite());
    assert!(
        results.len() >= 15,
        "la suite se redujo a {} casos. La cobertura baja en silencio.",
        results.len()
    );
}

#[test]
fn cada_caso_sin_filtro_compara_un_total_no_una_celda() {
    // Sin filtro la consulta agrupa por region y devuelve una fila por celda. Comparar
    // solo la primera celda contra el total del interprete daria un falso positivo cuando
    // la primera celda coincide por casualidad.
    let engine = engine().expect("carga fixtures");
    let cases = vec![
        datan_testkit::Case::new("sum sin filtro", "SUM('fact_sales'[qty])"),
        datan_testkit::Case::new("amount sin filtro", "SUM('fact_sales'[amount])"),
    ];
    for case in &cases {
        let result = run_case(&engine, case).expect("corre el caso");
        assert!(
            result.agrees,
            "{}: SQL={:?} interprete={:?}",
            case.name, result.sql_value, result.interp_value
        );
    }
}

#[test]
fn un_filtro_sin_resultados_da_blank_en_los_dos_caminos() {
    // El caso limite: una region que no existe. `SUM` sobre cero filas es BLANK en DAX,
    // no 0. Si un camino devuelve 0 y el otro BLANK, divergen en la ultima fila de una
    // tabla, que es donde el usuario mira primero.
    let engine = engine().expect("carga fixtures");
    let case = datan_testkit::Case::new("sin resultados", "SUM('fact_sales'[qty])")
        .filter("region", "Oeste");
    let result = run_case(&engine, &case).expect("corre el caso");
    assert!(result.agrees, "un filtro sin resultados debe coincidir");
}