//! Un informe de punta a punta contra Postgres.
//!
//! Conectar, construir el modelo, elegir dimensiones y medidas, ejecutar, y pinchar una
//! celda para ver como cambia el informe entero. Sin GPU: es la logica del producto, y tiene
//! que poder verificarse sin pantalla.
//!
//! Uso: cargo run --release -p datan-app --bin informe_postgres -- [base]

use std::path::Path;

// El documento del usuario vive en el bin de la app. Se incluye por ruta para poder
// verificar las jerarquias contra la base, sin pantalla.
#[path = "../model_doc.rs"]
#[allow(dead_code)] // El bin solo lee el documento; el resto de la API es de la app.
mod model_doc;

use datan_connect::{PostgresCatalog, Source, introspect};
use datan_model::{DataType, ObjectId, TableKind};
use datan_report::Report;
use datan_storage::Engine;

fn main() {
    let database = std::env::args().nth(1).unwrap_or_else(|| "datan_bench".into());
    let home = std::env::var("HOME").expect("HOME");
    let root = Path::new(&home).join(".duckdb").join("extensions");

    let engine = Engine::open_with_bundled_extensions(":memory:", &root).expect("motor");
    engine
        .load_bundled_extension("postgres")
        .expect("extension de Postgres");
    engine
        .execute(&format!(
            "ATTACH 'host=localhost dbname={database}' AS pg (TYPE POSTGRES, READ_ONLY)"
        ))
        .expect("conecta");
    println!("Datan: informe contra `{database}`\n");

    let mut catalog = PostgresCatalog::connect(&format!("host=localhost dbname={database}"))
        .expect("catalogo");
    let introspection =
        introspect(&engine, &Source::new("pg"), Some(&mut catalog), Some("product_boleta"))
            .expect("modelo");
    let mut model = introspection.model;

    // El documento del usuario se aplica ENCIMA, igual que en la app. Sin esto, una jerarquia
    // o una medida declaradas en el archivo no existen para este bin, y una jerarquia sobre
    // una columna calculada no resolveria: el chequeo diria que la columna no esta, que es
    // justo lo contrario de lo que pasa en la app.
    let user_doc = model_doc::ModelDoc::load(&database).expect("el modelo del usuario");
    let doc_avisos = user_doc.apply(&mut model);
    println!("--- modelo del usuario ---");
    println!(
        "  {} medidas, {} columnas, {} jerarquias",
        user_doc.measures.len(),
        user_doc.columns.len(),
        user_doc.jerarquias.len()
    );
    for aviso in &doc_avisos {
        println!("  AVISO: {aviso}");
    }
    if doc_avisos.is_empty() {
        println!("  sin avisos");
    }

    // El interprete lee de DuckDB las medidas que no se bajan a SQL. Se pasa en TODAS las
    // consultas: una columna calculada obliga a ese camino, y un informe sin origen falla en
    // vez de dar un numero. Es lo mismo que hace la app en `run_report`.
    let source = datan_report::source::DuckSource::new(&engine, &model, "pg");

    println!("--- relaciones ---");
    for relating in &model.relationships {
        let (Some((from_table, from_column)), Some((to_table, to_column))) = (
            model.column(relating.from_column),
            model.column(relating.to_column),
        ) else {
            continue;
        };
        println!(
            "  {}[{}] -> {}[{}] ({:?})",
            from_table.name, from_column.name, to_table.name, to_column.name, relating.role
        );
    }
    if model.relationships.is_empty() {
        println!("  ninguna: una base sin foreign keys no trae relaciones");
    }

    // Las dimensiones son las columnas de texto de las dimensiones que no son clave. Las
    // medidas, las de suma sobre los hechos.
    let dimensions: Vec<ObjectId> = model
        .tables
        .iter()
        .filter(|t| matches!(t.kind, TableKind::Dimension))
        .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
        .filter(|(t, c)| matches!(c.data_type, DataType::Text) && !t.key_columns.contains(&c.id))
        .map(|(_, c)| c.id)
        .collect();
    let measures: Vec<ObjectId> = model
        .measures
        .iter()
        .filter(|m| m.expression.starts_with("SUM"))
        .map(|m| m.id)
        .collect();

    println!("Dimensiones:");
    for dimension in &dimensions {
        if let Some((table, column)) = model.column(*dimension) {
            println!("  {}[{}]", table.name, column.name);
        }
    }
    println!("Medidas:");
    for measure in &measures {
        if let Some(m) = model.measures.iter().find(|m| m.id == *measure) {
            println!("  {} = {}", m.name, m.expression);
        }
    }

    let report = Report::new(model.clone())
        .attached_to("pg")
        .with_dimensions(dimensions.clone())
        .with_measures(measures.clone());

    println!("\nConsultas necesarias: {}", report.query_count().unwrap_or(0));

    // 1. El informe entero.
    match report.execute(&engine, Some(&source)) {
        Ok(matrix) => {
            println!("\n--- informe completo ---");
            matrix.print(12);

            // Cross-filter: se pincha la celda de la primera dimension de la primera fila.
            if let Some(drilled) = matrix.drill(&report, 0, 0) {
                let name = model
                    .column(dimensions[0])
                    .map(|(_, c)| c.name.clone())
                    .unwrap_or_default();
                let value = matrix.cell(0, 0).unwrap_or_default().to_string();
                let filtered = drilled.execute(&engine, Some(&source)).expect("filtra");
                println!("\n--- pinchando {} = {value} ---", name);
                filtered.print(12);
            }
        }
        Err(error) => println!("\nERROR: {error}"),
    }

    // El drill, de punta a punta: cada nivel que el usuario declaro como jerarquia, como
    // dimension unica de su propio informe. Es lo que hacen los botones de bajar y subir.
    let doc = &user_doc;
    if doc.jerarquias.is_empty() {
        println!("\nSin jerarquias declaradas en el modelo del usuario.");
        return;
    }
    let medida: Vec<ObjectId> = measures.iter().copied().take(1).collect();
    for jerarquia in &doc.jerarquias {
        let niveles = match jerarquia.resolver(&model) {
            Ok(niveles) => niveles,
            Err(aviso) => {
                println!("\nAVISO: {aviso}");
                continue;
            }
        };
        println!("\n--- jerarquia {} ---", jerarquia.name);
        for (posicion, nivel) in niveles.iter().enumerate() {
            let nombre = model
                .column(*nivel)
                .map(|(_, c)| c.name.clone())
                .unwrap_or_default();
            println!("\nnivel {}: {nombre}", posicion + 1);
            let informe = Report::new(model.clone())
                .attached_to("pg")
                .with_dimensions(vec![*nivel])
                .with_measures(medida.clone());
            match informe.execute(&engine, Some(&source)) {
                Ok(matrix) => matrix.print(4),
                Err(error) => println!("  ERROR: {error}"),
            }
        }
    }

    // El Top N: el primer nivel de la primera jerarquia, acotado a un valor por la primera
    // medida. El recorte es un filtro, asi que el gran total sigue saliendo de la base y la
    // matriz trae exactamente los valores elegidos.
    //
    // Para `categoria` la base dice: Ferreteria 1588000, Oficina 1516000, Limpieza 1472000,
    // Almacen 1424000. El mayor y el menor tienen que salir con esos numeros.
    if let (Some(jerarquia), Some(medida)) = (doc.jerarquias.first(), measures.first())
        && let Ok(niveles) = jerarquia.resolver(&model)
        && let Some(nivel) = niveles.first()
    {
        let nombre = model
            .column(*nivel)
            .map(|(_, c)| c.name.clone())
            .unwrap_or_default();
        for abajo in [false, true] {
            println!(
                "\n--- {} de {nombre} por {} ---",
                if abajo { "el menor" } else { "el mayor" },
                model
                    .measures
                    .iter()
                    .find(|m| m.id == *medida)
                    .map(|m| m.name.clone())
                    .unwrap_or_default()
            );
            let informe = Report::new(model.clone())
                .attached_to("pg")
                .with_dimensions(vec![*nivel])
                .with_measures(vec![*medida])
                .with_top_n(datan_report::TopN {
                    measure: *medida,
                    count: 1,
                    abajo,
                });
            match informe.execute(&engine, Some(&source)) {
                Ok(matrix) => matrix.print(10),
                Err(error) => println!("  ERROR: {error}"),
            }
        }
    }

    // --- Columnas calculadas ---
    //
    // Una columna calculada no esta en la base: la declara el modelo del usuario y el motor
    // la evalua FILA A FILA. Se verifica contra la base por SQL crudo, que es un camino
    // independiente: el mismo numero tiene que salir de los dos lados.
    //
    // Las columnas de esta seccion se declaran EN MEMORIA y no en el archivo del usuario: es
    // una verificacion, y el archivo es del usuario.
    let mut con_calculadas = model.clone();
    let demo = model_doc::ModelDoc {
        measures: vec![
            model_doc::MeasureDoc::new("Cantidad", "SUM('product_boleta'[quantity])"),
            model_doc::MeasureDoc::new("Importe", "SUM('product_boleta'[importe])"),
        ],
        columns: vec![
            model_doc::ColumnDoc::calculada(
                "product_boleta",
                "importe",
                "'product_boleta'[quantity] * 'product_boleta'[precio_unitario]",
                "entero",
            ),
            model_doc::ColumnDoc::calculada(
                "product_boleta",
                "tamano",
                "IF('product_boleta'[quantity] > 2, \"Grande\", \"Chico\")",
                "texto",
            ),
            // La jerarquia de fecha que la base no declara: el ano y el mes de la boleta.
            // Es el caso de uso que motivo las columnas calculadas.
            model_doc::ColumnDoc::calculada(
                "boleta",
                "anio_demo",
                "YEAR('boleta'[fecha])",
                "entero",
            ),
            model_doc::ColumnDoc::calculada(
                "boleta",
                "mes_demo",
                "MONTH('boleta'[fecha])",
                "entero",
            ),
        ],
        jerarquias: vec![],
        relaciones: vec![],
    };
    let avisos = demo.apply(&mut con_calculadas);
    for aviso in &avisos {
        println!("\nAVISO: {aviso}");
    }

    let demo_source = datan_report::source::DuckSource::new(&engine, &con_calculadas, "pg");
    let cantidad = con_calculadas
        .measures
        .iter()
        .find(|m| m.name == "Cantidad")
        .map(|m| m.id);
    let importe = con_calculadas
        .measures
        .iter()
        .find(|m| m.name == "Importe")
        .map(|m| m.id);
    let tamano = con_calculadas
        .table_by_name("product_boleta")
        .and_then(|table| table.column_by_name("tamano"))
        .map(|column| column.id);

    // El total de una medida sobre una columna calculada, contra el mismo calculo en SQL.
    if let Some(importe) = importe {
        let informe = Report::new(con_calculadas.clone())
            .attached_to("pg")
            .with_measures(vec![importe]);
        let obtenido = informe
            .totals(&engine, Some(&demo_source))
            .map(|values| values.first().cloned().unwrap_or_default());
        let esperado = engine
            .scalar(
                "SELECT SUM(quantity * precio_unitario)::text FROM pg.product_boleta",
                0,
            )
            .ok()
            .flatten();
        println!("\n--- total de `Importe` = SUM(quantity * precio_unitario) ---");
        println!("  motor: {obtenido:?}");
        println!("  SQL:   {esperado:?}");
    }

    // Agrupar por una calculada de texto, contra un `CASE` en SQL. Es el chequeo fuerte: no
    // mira un total sino la particion entera de las filas.
    if let (Some(tamano), Some(cantidad)) = (tamano, cantidad) {
        let informe = Report::new(con_calculadas.clone())
            .attached_to("pg")
            .with_dimensions(vec![tamano])
            .with_measures(vec![cantidad]);
        println!("\n--- por `tamano` = IF(quantity > 2, Grande, Chico) ---");
        match informe.execute(&engine, Some(&demo_source)) {
            Ok(matrix) => {
                matrix.print(10);
                let esperado = engine
                    .render_rows(
                        "SELECT CASE WHEN quantity > 2 THEN 'Grande' ELSE 'Chico' END, \
                         SUM(quantity)::text FROM pg.product_boleta GROUP BY 1 ORDER BY 1",
                    )
                    .unwrap_or_default();
                for row in esperado {
                    println!("  SQL: {} {}", row[0], row[1]);
                }
            }
            Err(error) => println!("  ERROR: {error}"),
        }
    }

    // Apagar una relacion tiene que notarse. Si el informe sigue dando lo mismo, el rol no
    // gobierna nada y el interruptor de la app seria decorativo.
    {
        let mut sin_producto = user_doc.clone();
        sin_producto.upsert_relationship(
            model_doc::RelationshipDoc::new(
                "catalogo_productos",
                "id",
                "product_boleta",
                "product_id",
            )
            .con_rol("inactiva"),
        );
        let mut modelo_sin = model.clone();
        let avisos = sin_producto.apply(&mut modelo_sin);
        for aviso in &avisos {
            println!("\nAVISO: {aviso}");
        }
        let sin_source = datan_report::source::DuckSource::new(&engine, &modelo_sin, "pg");
        let categoria = modelo_sin
            .table_by_name("catalogo_productos")
            .and_then(|table| table.column_by_name("categoria"))
            .map(|column| column.id);
        let cantidad = modelo_sin
            .measures
            .iter()
            .find(|m| m.expression.contains("quantity"))
            .map(|m| m.id);
        if let (Some(categoria), Some(cantidad)) = (categoria, cantidad) {
            let informe = Report::new(modelo_sin.clone())
                .attached_to("pg")
                .with_dimensions(vec![categoria])
                .with_measures(vec![cantidad]);
            println!("\n--- por categoria con la relacion de productos APAGADA ---");
            match informe.execute(&engine, Some(&sin_source)) {
                Ok(matrix) => matrix.print(6),
                Err(error) => println!("  ERROR: {error}"),
            }
        }
    }

    // La jerarquia de fecha, de punta a punta: una calculada sobre la tabla de la BOLETA,
    // agrupada desde los hechos por el camino de la relacion. Contra `EXTRACT` en SQL, que es
    // el mismo calculo por otro camino.
    if let Some(cantidad) = cantidad
        && let Some(anio) = con_calculadas
            .table_by_name("boleta")
            .and_then(|table| table.column_by_name("anio_demo"))
            .map(|column| column.id)
    {
        let informe = Report::new(con_calculadas.clone())
            .attached_to("pg")
            .with_dimensions(vec![anio])
            .with_measures(vec![cantidad]);
        println!("\n--- por `boleta[anio_demo]` = YEAR(fecha) ---");
        match informe.execute(&engine, Some(&demo_source)) {
            Ok(matrix) => {
                matrix.print(10);
                let esperado = engine
                    .render_rows(
                        "SELECT EXTRACT(YEAR FROM b.fecha)::text, SUM(pb.quantity)::text \
                         FROM pg.product_boleta pb JOIN pg.boleta b ON b.id = pb.boleta_id \
                         GROUP BY 1 ORDER BY 1",
                    )
                    .unwrap_or_default();
                for row in esperado {
                    println!("  SQL: {} {}", row[0], row[1]);
                }
            }
            Err(error) => println!("  ERROR: {error}"),
        }
    }
}