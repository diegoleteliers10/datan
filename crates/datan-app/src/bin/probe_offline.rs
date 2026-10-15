//! R2, parte 2: la extension de Postgres se descarga en runtime. Eso rompe la apuesta de
//! funcionar offline y falla en redes enterprise que bloquean descargas.
//!
//! Este binario comprueba la ruta que si es viable para una app distribuida: precargar la
//! extension dentro del bundle y apuntar DuckDB ahi con `extension_directory`, sin red.
//!
//! Uso: cargo run --release -p datan-app --bin probe_offline

use datan_storage::Engine;

fn main() {
    println!("Datan: R2, la extension precargada\n");

    // Directorio donde se guardan las extensiones de DuckDB.
    let home = std::env::var("HOME").expect("HOME");
    let root = format!("{home}/.duckdb/extensions");
    let dir = format!("{root}/v1.5.6/osx_arm64");
    println!("Directorio por defecto: {dir}");

    if !std::path::Path::new(&dir).exists() {
        println!("No existe. Ejecuta probe_connectors primero para descargarla.");
        return;
    }

    // Apunta DuckDB al directorio precargado y prohibe la descarga. Si esto funciona, la app
    // puede empaquetar la extension y no depender de la red en el primer uso.
    let engine = Engine::open(":memory:").expect("abre motor");

    println!("\n1. Desactivar la descarga automatica");
    match engine.execute("SET autoinstall_known_extensions = false") {
        Ok(()) => println!("   OK."),
        Err(error) => println!("   FALLA: {error}"),
    }

    // `extension_directory` es la raiz, no la carpeta de la plataforma. DuckDB le anade
    // `v<version>/<platform>/` por debajo. Apuntarlo a la carpeta de la plataforma produce
    // una ruta duplicada y un "not found".
    println!("\n2. Apuntar a la raiz de extensiones");
    match engine.execute(&format!("SET extension_directory = '{root}'")) {
        Ok(()) => println!("   OK. DuckDB anade v1.5.6/osx_arm64 por debajo."),
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n3. Cargar sin red (LOAD, no INSTALL)");
    match engine.execute("LOAD postgres") {
        Ok(()) => println!("   OK. La extension se carga desde disco."),
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\n4. Funciones disponibles");
    match engine.query_arrow(
        "SELECT function_name FROM duckdb_functions() WHERE function_name LIKE 'postgres%' ORDER BY 1 LIMIT 3",
    ) {
        Ok(batches) => {
            let text = datan_storage::text_of_first_column(&batches);
            println!("   {text}");
        }
        Err(error) => println!("   FALLA: {error}"),
    }

    println!("\nConclusion:");
    println!("  Si los 4 pasos pasan, la app empaqueta postgres_scanner.duckdb_extension");
    println!("  con un tamaño de ~31 MB y lo carga desde el bundle. Sin red en el primer uso.");
    println!("  El costo es 31 MB por plataforma en el instalador.");
}