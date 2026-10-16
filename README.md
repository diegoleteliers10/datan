# Datan

Alternativa a Power BI en Rust. Motor DuckDB, lenguaje de medidas DAX-compatible, interfaz
GPUI + gpui-kit.

App de escritorio multiplataforma para analisis de datos. Local first, sin servidor.

## Estado

| Fase | Estado |
| --- | --- |
| 0, el spike | Completada 14 Oct 2026. Ambas puertas pasaron. |
| R2, conectores | Resuelto 14 Oct 2026. La extension se precarga, no se compila. |
| Apuesta 2, pushdown | Medida 14 Oct 2026. La cache local es 20x mas rapida. |
| Measure IR, front-end | Lexer, parser y chequeo de nombres listos. |
| Measure IR, algebra de filtro | Completa. Propagacion conmutativa, incluso en ciclos. |
| 2, CALCULATE y modifiers | Completa. `ALL`, `ALLEXCEPT`, `KEEPFILTERS`, `CROSSFILTER`, `USERELATIONSHIP`. |
| 3, ciclos y time intelligence | Completa. `DATEADD`, `DATESYTD`, `PARALLELPERIOD`, `PREVIOUSYEAR`. |
| 1, aritmetica entre medidas | Completa. `Margin` baja a SQL con `NULLIF`. |
| 1, Postgres -> modelo -> medidas -> grid de 1M | La app abre, elige conexion, muestra el informe, cruza datos, guarda y exporta CSV. |
| 4, import de PBIP / TMDL | Pendiente. Reservar Windows + humano en la semana 1. |

El plan completo esta en [PLAN.md](PLAN.md). Las decisiones de arquitectura, los riesgos y las
correcciones criticas estan ahi. Este archivo es el punto de entrada corto.

## Verificacion rapida

```sh
# todo el motor, headless, sin GPU ni base de datos externa
cargo test --workspace

# la pregunta que decide si el proyecto tiene dos caminos o uno
cargo run --release -p datan-testkit --bin conformance

# el lowering produce la misma query que el spike, verificado por EXCEPT
cargo run --release -p datan-app --bin verify_pipeline

# un informe de punta a punta contra Postgres, con cross-filter
cargo run --release -p datan-app --bin informe_postgres

# tests de exportacion a CSV y de informes guardados
cargo test -p datan-report

# pregunta que decide la arquitectura: una query o N subconsultas?
cargo run --release -p datan-app --bin spike_day2

# R2: como se llega a Postgres sin red en el primer uso
cargo run --release -p datan-app --bin probe_connectors
cargo run --release -p datan-app --bin probe_offline

# la app. Requiere pantalla. Elige una conexion de la lista y pincha celdas.
cargo run --release -p datan-app
```

## Workspace

| Crate | Responsabilidad |
| --- | --- |
| `datan-storage` | El unico que toca `duckdb::Connection`. Sin gpui, sin arrow directo. |
| `datan-model` | Tablas, columnas, lineage, relaciones, medidas, RLS. |
| `datan-dax-syntax` | Lexer, AST y parser de DAX. Cada nodo lleva `Span`. |
| `datan-dax-check` | Resolucion de nombres y lint contra el modelo. |
| `datan-dax-eval` | Algebra de filtros, modificadores, atomos, interprete por celda. |
| `datan-dax-sql` | Bajo de un `AtomSet` a una unica query de DuckDB. |
| `datan-connect` | De una base conectada a un modelo. DuckDB lee datos, Postgres lee el catalogo. |
| `datan-report` | Un informe: dimensiones, medidas, filtros y la consulta que lo llena. Sin gpui. |
| `datan-testkit` | Suite de conformidad diferencial entre los dos caminos. |
| `datan-app` | Binario thin. Todo gpui vive aqui por ahora. |

Los crates que faltan (`datan-plan`, `datan-ui`) llegan con las fases 4 a 6.
Ver seccion 4 de PLAN.md.

Los crates del grupo motor son headless: `cargo test --workspace` corre sin GPU ni base de datos
externa. Esa es la propiedad que hace que la parte dificil del proyecto itere rapido.

## Reglas que no se negocian

1. **Cero dependencia directa de `arrow`.** Todo pasa por `duckdb::arrow`. `duckdb` pinnea
   `arrow ^58` y el crate suelto esta en 60. Dos majors en el arbol dan dos tipos
   `RecordBatch` incompatibles. Verificar con `cargo tree -d | grep -oE "arrow v[0-9.]+" | sort -u`.
2. **Ningun crate del grupo motor declara `gpui-pre` o `gpui-kit`.** La dependencia es
   unidireccional. El payload que cruza al hilo de UI es `Send + Sync`.
3. **Una conexion DuckDB, un hilo, cola serializada.** `Connection` es `Send` pero no `Sync`, y
   DuckDB tiene lock de escritor unico.
4. **El motor no se importa en el hilo de UI.** La extraccion de Arrow a `ColumnarTable` ocurre
   en `datan-storage`, en el hilo de trabajo.
5. **Las extensiones van precargadas, no descargadas.** `autoinstall_known_extensions = false`
   y `extension_directory` apuntando al bundle. La extension de Postgres pesa 31 MB.