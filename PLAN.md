# Datan: alternativa a Power BI en Rust

Aplicacion de escritorio multiplataforma para analisis de datos. Motor DuckDB, lenguaje de
medidas DAX-compatible, interfaz GPUI + gpui-kit.

Estado del plan: aprobado en conversacion. Ningun codigo escrito.

---

## 1. Decisiones tomadas

| Decision | Valor | Motivo |
| --- | --- | --- |
| Motor de consultas y cache | DuckDB 1.5.6 (crate `duckdb` 1.10506.0, MIT) | Storage incluido, mejor cobertura SQL para BI, ATTACH hace pushdown a Postgres/MySQL/SQLite |
| Lenguaje de medidas | DAX-compatible | El usuario de Power BI migra sin reescribir. Habilita import de PBIP/TMDL |
| Primera rebanada vertical | Conectar -> modelar -> ver tabla | Valida motor, medidas y GPUI antes de tocar el canvas |
| Producto | Desktop local primero, sin servidor | Distribucion simple, sin multi-tenancy, sin RLS |
| Lenguaje de medidas: alcance | Solo medidas, NO lenguaje de consulta | Barato y hace los modelos portables. Sin `EVALUATE`/`DEFINE` fiel |

---

## 2. Correcciones criticas al supuesto inicial

Estas dos verificaciones cambiaron la arquitectura. Aplicarlas antes de escribir codigo.

### 2.1 El scanner de Postgres de DuckDB no se enlaza desde crates.io — RESUELTO 14 Oct 2026

**Lo que se creo:** `libduckdb-sys` de crates.io no trae `CMakeLists.txt`. Su `duckdb.tar.gz` es una
amalgamacion de unidades de compilacion, no un arbol de fuentes de DuckDB. Su `manifest.json`
lista exactamente tres extensiones: `core_functions`, `json`, `parquet`. No hay
`BUILD_POSTGRES_EXTENSION` que activar, porque no hay sistema de build. `build_bundled_cc.rs`
filtra con `extension_enabled()`, que es codigo compilado en el crate, no configuracion.

**Lo que si funciona:** `INSTALL postgres` descarga la extension firmada en el primer uso y
`LOAD postgres` la activa. `postgres_scan`, `postgres_scan_pushdown`, `postgres_query` y
`postgres_execute` quedan disponibles.

**El problema que queda:** descarga en runtime. Falla en redes enterprise que la bloquean, y
rompe la apuesta 4 (offline, zero-install).

**La solucion, verificada:** precargar la extension y apuntar DuckDB ahi.

```sql
SET autoinstall_known_extensions = false;
SET autoinstall_extension_repository = '';  -- desactiva cualquier descarga
SET extension_directory = '<raiz precargada>';
LOAD postgres;
```

`extension_directory` es la **raiz**, no la carpeta de la plataforma. DuckDB le anade
`v<version>/<platform>/` por debajo. Apuntarlo a la carpeta equivocada produce una ruta
duplicada y un "not found".

**Decision de producto:** la app empaqueta `postgres_scanner.duckdb_extension` y lo carga
desde su propio directorio. Coste: **31 MB por plataforma** en el instalador. A cambio, la
apuesta 4 se sostiene entera y el primer uso no toca la red.

Probes: `crates/datan-app/src/bin/probe_connectors.rs` y `probe_offline.rs`.

### 2.2 `duckdb::Connection` es `Send` pero no `Sync`

`duckdb::inner_connection` envuelve un `RefCell`. Combinado con el lock de escritor unico de
DuckDB, el instinto natural de "refresh en un hilo, consultas en otro" son dos conexiones al
mismo archivo `.duckdb`.

**Guardrail:** una conexion, propiedad de un hilo de motor, alimentada por una cola de trabajo
serializada. El aislamiento por snapshot MVCC de DuckDB cubre el caso lectura durante escritura.
Eso ya es mejor que el intercambio de modelos de Power BI.

### 2.3 Trampa de versiones de arrow

`duckdb` 1.10506.0 depende de `arrow ^58`. El crate `arrow` actual es 60.0.0.
`duckdb::arrow` es un re-export de arrow 58.

Una sola dependencia directa de `arrow` produce dos tipos `RecordBatch` incompatibles y un error
de compilacion en el fondo de una funcion del motor que parece un bug de traits.

**Guardrail:** cero dependencia directa de `arrow` en el workspace, para siempre. Todo pasa por
`duckdb::arrow`.

**Criterio real de aceptacion, medido el 14 Oct 2026:** `cargo tree -d` debe mostrar una sola
version *major* de arrow. Hay 11 crates distintos del ecosystem arrow, todos en 58.4.0
(`arrow`, `arrow-array`, `arrow-arith`, `arrow-buffer`, `arrow-cast`, `arrow-data`,
`arrow-ord`, `arrow-row`, `arrow-schema`, `arrow-select`, `arrow-string`). Eso es correcto y no
duplica tipos. Lo que no se permite es que aparezca 60.x junto a 58.x.

Comando de verificacion:

```sh
cargo tree -d | grep -oE "arrow v[0-9.]+" | sort -u
```

Salida esperada: `arrow v58.4.0` y nada mas.

---

## 3. La decision que sostiene todo: Measure IR

Oracle rechaza "DAX compila a SQL" como capa principal.

Nada de DAX es estrictamente inexpresable en SQL. El problema es que algunos casos se vuelven
subconsultas correlacionadas que DuckDB no decorrelaciona, y el coste pasa a ser O(grupos)
escaneos completos de la tabla de hechos.

### 3.1 Casos que SQL no alcanza

1. **`CROSSFILTER(BOTH)` sobre un grafo ciclico.** SQL exige un arbol de joins aciclico. Power BI
   permite relaciones bidireccionales, que producen ciclos en cuanto encadenas Dim -> Fact ->
   Bridge -> Fact. Con ciclos el numero de alias crece de forma combinatoria. Aqui el backend SQL
   se rinde.
2. **Cambio de granularidad no acotado.**
   `SUMX(VALUES(Product[Subcategory]), CALCULATE([Revenue], REMOVEFILTERS(Product)))` agrupado por
   `[Category]`. SQL no tiene "agregar a una granularidad que no declaraste en GROUP BY".
3. **Agregado de agregado bajo filter context que varia por fila.**

### 3.2 Lo que SQL maneja limpio

| Constructo | Destino SQL |
| --- | --- |
| `SUM`, `COUNTROWS`, `MIN`, `MAX`, `DISTINCTCOUNT` | agregado nativo |
| `CALCULATE` con `ALL`, `ALLEXCEPT`, `KEEPFILTERS`, `REMOVEFILTERS` | reescritura del arbol de filtros en el WHERE |
| `RELATED`, `RELATEDTABLE` | join por el grafo de relaciones |
| `SUMX(Fact, Fact[Qty] * Dim[Price])` con context transition | join normal |
| `AVERAGEX`, `MAXX`, `MINX` | `SUM(expr)/COUNT(expr)` o `MAX(expr)` |
| `SUMMARIZE` con `ROLLUP` | `GROUPING SETS`, que DuckDB soporta |
| `VAR` | CTE o subexpresion inlineada |
| Toda la time intelligence | join a la tabla de fechas, si el modelo tiene fecha marcada contigua |
| `SELECTEDVALUE` | `MIN`/`MAX` mas `count == 1` |
| `ISBLANK`, `BLANK()` | `NULL` con coercion blank-a-cero explicita |

### 3.3 Por que no SQL como representacion principal

Empezar por el SQL cuesta cuatro cosas que no se recuperan:

1. **Sin inspeccionabilidad.** No puedes lintear una medida, diffear dos modelos ni responder que
   pide realmente esta medida. Un string es el peor IR posible justo para las features que
   diferencian el producto.
2. **Sin fallback a mitad de camino.** Te comprometes a generar SQL antes de saber si el plan va a
   ser bueno.
3. **Sin round-trip de plan.** El grid necesita "muestrame la query" y la cache necesita
   invalidacion por clave de plan. Ambos necesitan estructura, no texto.
4. **Seis meses en un optimizador y un pretty-printer**, que DuckDB ya tiene, mientras el problema
   real, el algebra de filter context, queda aguas arriba donde el optimizador no lo ve.

### 3.4 La estructura

```
AST DAX
  -> (1) Measure IR: normalizar la medida bajo (filter context, grouping columns)
         hasta un DAG de ATOMOS
         atomo := (fn agregada, expr argumento) bajo un conjunto de predicados
                 de filtro concreto y totalmente resuelto
  -> (2) Lowering a SQL:  si todos los atomos son combinaciones algebraicas y ninguno
                           necesita granularidad distinta a la del GROUP BY
                           -> una query DuckDB, una pasada, cero interprete
  -> (3) Interprete:      si no -> evaluar por CELDA DE GRUPO.
                         El SUM(X) del interprete es una busqueda en diccionario
                         Arrow sobre el batch del paso (1), no un escaneo.
```

El insight que lo hace tratable es el mismo que usa VertiPaq: **el fallback nunca corre sobre
filas. Corre sobre celdas de grupo, y cada agregacion hoja la sirve un escalar ya precalculado.**
Una matriz de 5.000 celdas con 6 atomos son 30.000 busquedas hash. Son milisegundos. Eso es lo
que hace aceptable tener un motor de formulas propio.

La regla del fast path, implementable en unas cientos de lineas:

> SQL es valido cuando todo atomo se expresa como WHERE sobre el GROUP BY declarado, y ninguna
> medida re-entra en una medida a otra granularidad.

`[Margin] = ([Revenue] - [COGS]) / [Revenue]` se queda en el fast path. Eso cubre la mayoria de
las medidas reales.

El limite lo dibuja VertiPaq en su propio xmSQL-vs-callback, porque Microsoft ya lo dibujo y es
el unico split empiricamente validado del mundo DAX.

### 3.5 Recorte de alcance a escribir esta semana

Datan se compromete a DAX como lenguaje de **medidas**, que es barato y hace los modelos
portables. **No** se compromete a la semantica del lenguaje de **consulta**.

Sin `EVALUATE`/`DEFINE` fiel, sin paridad de casos borde de `SUMMARIZECOLUMNS`, sin features
estilo MDX.

Hay que publicar que construcciones no soporta. Hay que shippear el camino "no puedo compilar
esta medida" en la Fase 2 de forma permanente. Un usuario golpea una medida no soportada el dia 1,
y un error duro lo hace irse.

---

## 4. Layout del workspace

Regla para crear un crate: **debe tener un contrato testeable de forma independiente, o un
consumidor reutilizable de forma independiente.** Ninguna otra justificacion.

La frontera GPUI se resuelve no resolviendola. Los tipos no-`Send` de GPUI (`Entity<T>`, `App`,
`Window`, `Context<T>`) nunca cruzan la linea porque la dependencia es estrictamente unidireccional
y el payload es `Send + Sync`. El payload es `arrow::RecordBatch` mas un `CancelHandle`. Eso es
todo el protocolo.

```
crates/
  datan-dax-syntax    # tokens, spans, lexer, parser, serde
  datan-model         # tablas, columnas, lineage, relaciones (incluidas bidireccionales
                      #   y cross-filter), jerarquias, medidas, RLS, codec TMDL in+out
  datan-dax-check     # resolucion de nombres, lineage tagging, typecheck, lint de DAX
  datan-dax-eval      # algebra de filter context, EXTRACCION DE ATOMOS, el interprete.
                      #   Puro. Depende solo de datan-model.
  datan-plan          # (filter ctx, grouping) -> DAG de Measure IR -> plan por backend. Puro.
  datan-dax-sql       # Measure IR -> texto SQL. Aislado de storage para poder diffearlo
                      #   y testearlo sin base de datos.
  datan-storage       # el UNICO crate que toca duckdb::Connection.
                      #   scan base, emision de queries de materializacion de atomos,
                      #   refresh transaccional, materializacion de columnas calculadas
  datan-connect       # UN crate, un modulo por fuente. Postgres, ODBC/SQL Server,
                      #   archivos, REST. El conteo de conectores es lo mas volatil;
                      #   los modulos cambian de forma, los crates no.
  datan-ui            # todo gpui/gpui-kit. shell, browser de workspace, editor de modelo,
                      #   EL GRID, canvas de reportes.
  datan-app           # binario thin, gpui::main, wiring.

  datan-testkit/      # solo dev. Suite de conformidad DAX + runner diferencial
                      #   (modelo + query -> ambos paths -> diff).
```

Propiedades de independencia que importan:

- `datan-dax-eval` **no** tiene duckdb, ni gpui, ni IO. `cargo test -p datan-dax-eval` corre en
  milisegundos, headless, sin GPU. Esto es lo que hace que la parte dificil del proyecto itere
  rapido, y es el argumento mas fuerte a favor del split.
- `datan-dax-sql` se testea con snapshots sin base de datos.
- `datan-storage` se testea contra DuckDB en memoria con fixtures generadas.

**Enforcement, no disciplina.** Un paso de CI que corre `cargo metadata` y falla el build si
cualquier crate del grupo motor declara `gpui-pre`, `gpui-kit` o `arrow` directamente. Doscientas
lineas de script, y convierte la frontera `Send` en un fallo de build en vez de un comentario de
code review.

---

## 5. Dependencias

Versiones verificadas el 14 Oct 2026.

| Capa | Crate | Version | Licencia |
| --- | --- | --- | --- |
| UI framework | `gpui-pre` (+ `-platform`, `-macros`) | 0.3.9 | Apache-2.0 |
| Biblioteca de componentes | `gpui-kit` | 0.7.3 | Apache-2.0 |
| Motor y cache | `duckdb` (+ `libduckdb-sys`) | 1.10506.0 | MIT |
| Parser SQL | `sqlparser` | 0.63.0 | Apache-2.0 |
| Transpilacion de dialectos | `polyglot-sql` | 0.13.3 | MIT |
| Import Power BI | `ripbi-core` (vendorizado) | 0.8.1 | MIT OR Apache-2.0 |
| Excel lectura | `calamine` | 0.36.1 | MIT |
| JSON incremental | `jiter` | 0.17.0 | MIT |
| JSON masivo | `simd-json` | 0.18.1 | MIT |
| CSV | `csv` | 1.4.0 | MIT |
| Postgres | `sqlx` | 0.9.0 | MIT OR Apache-2.0 |
| ODBC y legado | `odbc-api` | 29.2.0 | MIT |
| OAuth | `oauth2` | 5.0.0 | MIT |
| Metadata store | `rusqlite` | 0.40.2 | MIT |
| Cache en memoria | `moka` | 0.12.16 | Apache-2.0 |
| Empaquetado | `cargo-dist` (dist) | 0.32.0 | MIT OR Apache-2.0 |
| Auto-actualizacion | `self_update` | 1.3.0 | MIT OR Apache-2.0 |
| Testing dev | features `test-support` y `bench-support` de gpui-component | 0.7.3 | — |

Notas de configuracion:

- `gpui-kit` pinnea `gpui-pre = "=0.3.9"`. No subir `gpui-pre` sin subir `gpui-kit`.
- `odbc-api` con `default-features = false, features = ["odbc_version_3_80"]`. El feature `prompt`
  arrastra `winit`, que es una segunda dependencia de windowing en una app GPUI.
- `oauth2` con `timing-resistant-secret-traits`.
- Sin dependencia directa de `arrow`. Todo via `duckdb::arrow`.
- Perfil dev: `[profile.dev.package]` con `opt-level = 3` para `gpui-pre`, `gpui-component`,
  `gpui-kit`, `rustybuzz`, `taffy`, `ttf-parser`.

No adopted:

- **DataFusion.** Mejor story de extensibilidad, pero sin storage y con SQL mas estrecho. Revisitar
  si hacen falta kernels de medidas propios o un filesystem custom.
- **ADBC.** Pre-1.0, sin drivers Rust para PostgreSQL ni BigQuery, `adbc_snowflake` capea Arrow
  por debajo de 59. Year two.
- **Webview para charts.** No existe en gpui. PR #54433 cerrada sin mergear en abril 2026 con
  Wayland sin resolver.
- **vello.** Segundo stack GPU dentro de GPUI, acoplado a internals privados de `gpui_wgpu`.

---

## 6. Fases

### Fase 0, dias 1 a 4: el spike. P0 — COMPLETADA 14 Oct 2026

No es una fase del producto. Era una puerta de muerte. Las dos puertas pasaron.

**Dia 1: el grid.** Resultado: **23 us de media por frame** con 1M filas x 8 columnas
residentes en memoria, a 60 fps sostenidos durante 12 s de auto-scroll. `uniform_list` monta
~45 filas por frame y salta a un indice distinto cada vez, asi que el coste no viene de un
rango cacheado. Medido en la app real (`crates/datan-app/src/main.rs`), no en un bucle
sintetico.

Lo que valido:

- `ColumnarTable` cruza la frontera como `Send`. El hilo de UI no importa `duckdb` ni `arrow`.
- `cargo tree -d` muestra una sola major de arrow: 58.4.0.
- El costo por frame es independiente del tamano de la tabla.

**Dia 2: el fast path SQL.** Resultado: **PASA**. 10M de hechos, 500 productos, 5000 clientes.
`EXPLAIN ANALYZE` da un unico `HASH_GROUP_BY`, sin `DELIM_JOIN` ni subconsultas por celda,
**47 ms** contra un criterio de 500 ms. La forma de `Revenue = SUMX('Fact Sales', [Qty] * [Price])`
agrupado por region y quarter resuelve en una sola pasada. El backend SQL es viable.

Codigo: `crates/datan-app/src/bin/spike_day2.rs`.

**Dia 4: el lowering a SQL.** `datan-dax-sql` baja el `AtomSet` a una unica query de DuckDB.
Verificado contra DuckDB real en `verify_pipeline`: el SQL generado da **el mismo resultado**
que la consulta manual del spike, con `HASH_GROUP_BY` y 27 ms sobre 10M de filas.

Decisiones del lowering:

- Un filtro propagado se baja como `IN (SELECT ... FROM dim WHERE ...)`, **no** como JOIN. Un
  JOIN con la dimension multiplica filas y rompe `COUNT(DISTINCT)` y los `SUM` sobre
  columnas de la dimension. Un `IN` es semijoin.
- Una columna de agrupamiento que vive en otra tabla si requiere `JOIN`, con alias `t0`, `t1`
  porque `fact_sales` y `dim_product` comparten el nombre `product_id`.
- Un semijoin por tabla de dimension, no por columna. Repetirlo es correcto pero ilegible.
- Las columnas de agrupamiento NO se aliasean en el `SELECT`: DuckDB rechaza un alias dentro
  del `GROUP BY` incluso cuando el alias es identico al nombre de la columna.

**Dia 3, bonus: R2, los conectores.** Resultado: **RESUELTO**, con un metodo distinto al que
el plan asumia. El tarball de `libduckdb-sys` no trae `CMakeLists.txt`, asi que compilar la
extension desde fuente era imposible con crates.io. La via que si funciona es precargarla.
Ver seccion 2.1. Coste: 31 MB por plataforma en el instalador.

Codigo: `crates/datan-app/src/bin/probe_connectors.rs` y `probe_offline.rs`.

**Correcciones que salieron del spike:**

1. `uniform_list` en gpui 0.3.9 recibe `(range, &mut Window, &mut App)` y devuelve `Vec<R>`.
   No recibe callback de altura de fila ni un `entity`. Mide el primer elemento y lays out el
   resto en linea. El id se pasa como `ElementId::View(entity_id)`.
2. `EXPLAIN` de DuckDB devuelve **dos columnas**: la clave de cada renglon y el valor. El plan
   real esta en la segunda. Leer solo la columna 0 devuelve vacio. `Engine::explain` concatena
   todas las columnas de texto.
3. `Connection::interrupt_handle()` devuelve `Arc<InterruptHandle>` directo, sin `Result`. No
   existe `get_interrupt_handle`.
4. `Statement::query_arrow([])` devuelve un iterador **infalible**: si la ejecucion falla
   despues de empezar, hace panic. No hay forma de propagar el error con `collect`. Aceptable en
   v1 con cancelacion disponible via `interrupt_handle`.
5. `batch.column_by_name()` en arrow 58 devuelve `Option`, no `Result`.
6. La prediccion de Oracle de que una subconsulta correlacionada produce `DELIM_JOIN` es
   **incorrecta**. DuckDB la optimiza a `HASH_JOIN`. El criterio correcto es si la consulta
   introduce un join extra, no el nombre del nodo. El test
   `la_forma_simple_no_introduce_joins` mide eso.
7. `libduckdb-sys` no trae `CMakeLists.txt`. Su `duckdb.tar.gz` es una amalgamacion de
   unidades de compilacion con un `manifest.json` de tres extensiones. La prediccion de Oracle
   de compilar con `DUCKDB_EXTENSION_CONFIGS` era inviable desde crates.io. Ver seccion 2.1.

### Fase 1, siguiente: Postgres -> modelo -> medidas -> grid de 1M. P0

### Fase 1: Postgres -> modelo -> medidas -> grid de 1M. P0 — PENDIENTE

Entregable: modelo con cuatro medidas (`Revenue = SUM`, `COGS = SUM`, `Qty = SUM`,
`Margin = (Revenue - COGS)/Revenue`) mas columnas de dimension, sobre Postgres real. Grid de 1M
filas. Click en un header de dimension cross-filtra. El modelo hace round-trip por TMDL que
escribemos nosotros.

**Primer paso pendiente:** R2. Construir `libduckdb-sys` con `DUCKDB_EXTENSION_CONFIGS` para el
scanner de Postgres, o confirmar que hace falta. Es la dependencia oculta mas cara de esta fase.

Aceptacion dura:

- Grid de 1M filas a 60 fps de scroll.
- Query p95 bajo 300 ms.
- Cross-filter en un header re-ejecuta en menos de 300 ms.
- Todo sobrevive un round-trip TMDL write/read con resultados identicos.
- Build de `libduckdb-sys` con el scanner de Postgres funcionando en CI.

Supuesto mas riesgoso: que un conjunto **fijo** de medidas colapse en **una** query DuckDB con
todas las celdas de grupo en una pasada, sin subconsulta por celda.

Test de semana 1: escribir las cuatro medidas a mano en SQL, `EXPLAIN ANALYZE`, afirmar un solo
nodo hash aggregate y ningun `DELIM_JOIN`.

Ademas arrancar el grid custom el dia 1 de esta fase, no en la Fase 5. Ver R7.

**Informe, hecho antes del grid.** `datan-report` construye la matriz sin GPU, para que la
logica del producto sea testeable: dimensiones, medidas, filtros, y el cross-filter que
cambia el filtro de todas las medidas a la vez.

Las medidas se agrupan por conjunto de atomos y cada grupo es UNA consulta. Con dos medidas
que comparten hojas, el informe cuesta una consulta y no dos. Las filas de consultas
distintas se pegan por las columnas de dimension, no por posicion: es seguro porque todas
las medidas se evaluan bajo el mismo filtro y el mismo agrupamiento, y el `ORDER BY` del
lowering las ordena igual. Sin el `ORDER BY` seria suposicion, porque DuckDB devuelve los
grupos en el orden de su hash y dos consultas pueden diferir.

Hay un test que mira el TOTAL de cada columna de la matriz, no celdas sueltas. Si `Margin` se
pegara en la columna de `Qty`, la suma de la columna de `Margin` seria la de `Qty` y no se
notaria mirando una celda.

#### La app

`datan-app/src/main.rs` es la ventana: barra lateral con dimensiones y medidas, barra de
filtros activos, y el grid. Todo el trabajo pesado ocurre en `background_spawn`: al hilo de UI
solo llega la matriz ya calculada, como cadenas. `Engine` es `Send` pero no `Sync`, asi que
cada ejecucion abre su propia conexion y se cae al terminar.

`uniform_list` y no el componente `Table` de gpui-kit: el primero es el que la Fase 0 midio a
23 microsegundos por frame con un millon de filas, y el segundo trae su propia virtualizacion sin
medir. gpui-kit se usa para el tema, el hover y los estilos.

Dos decisiones de UI que parecen detalles y no lo son:

- **Solo las celdas de DIMENSION son pinchables.** Una celda de medida que parece pinchable y
  no hace nada hace pensar que la app se congelo.
- **El grid vuelve arriba tras filtrar.** Pinchar una fila del medio deja el resultado con tres
  filas, y sin volver arriba el usuario se queda mirando la posicion 400 de una lista de tres: el
  grid parece vacio.

Sobre gpui-kit: es una FACHADA. Reexporta los tipos de gpui pero no sus traits de interactividad,
y los metodos de cadena (`on_click`, `when`) no existen en `Div`. `on_click` va por
`interactivity()`, `Div` no tiene `when`, y el crate se llama `gpui-pre` con la libreria `gpui`.

#### La pantalla de conexion elige, no escribe

Escribir un DSN necesita un input con foco, teclado y estado. Todo eso no se puede verificar sin
pantalla, y un campo de texto roto no se ve como roto: se ve como un campo que no responde.

Elegir de una lista usa los mismos elementos clicables que el grid, que ya estan verificados.
Las conexiones viven en `~/.datan/connections.toml`, y la que funciona se sube arriba: la lista
ordena por USO, no por nombre, porque la base de ayer es la que se va a abrir hoy. Ordenar
alfabeticamente haria que la conexion de todos los dias quedara en medio de la lista para siempre.

El TOML no acepta un array en el raiz: `to_string` sobre un `Vec` falla en tiempo de ejecucion
con "UnsupportedType", no al compilar. Va dentro de una tabla.

Y la app arranca en la pantalla de conexion. No intenta conectar sola: una app que abre una base
sin preguntar es una app que escribe en la base equivocada.

**Test de semana 1, ejecutado: PASA.** Sobre 1.000.000 de lineas, `1 HASH_GROUP_BY`, `0
DELIM_JOIN`, p50 1 ms, p95 4 ms. El conjunto fijo de medidas SI colapsa en una pasada, y el
presupuesto de 300 ms no esta en riesgo.

Linea base, por categoria: Revenue 9.750.000.000 / Margin 0.6209 para Almacen. El generador tiene
que producir exactamente estos numeros.

#### El hallazgo que la linea base destapo

El IR no podia expresar las dos medidas mas simples de un informe real.

`Revenue = SUM(Qty * Price)` es una agregacion sobre una EXPRESION de dos columnas. El atomo
llevaba `column: Option<ObjectId>`, UNA columna. `first_fact_table` miraba solo ese campo, no
encontraba tabla de hechos, y la medida entera se rechazaba con "no se encontro la tabla de
hechos". Lo mismo con `COGS = SUM(Qty * Cost)`, que ademas cruza dos tablas.

Lo que faltaba no era un caso especial: era que el atomo fuera una hoja cuando deberia ser una
expresion. `Atom::referenced_columns()` y `Atom::anchor_table()` cierran la parte de las hojas, y
`render_expression` emite el arbol entero con sus tablas calificadas.

El segundo fallo era menos visible: la tabla de una COLUMNA de la agregacion no se unia al FROM.
`SUM(Qty * Cost)` no agrupa por la dimension ni filtra por ella, asi que el plan la ignoraba y el
`Cost` quedaba sin calificar. El SQL se generaba bien y DuckDB fallaba al ejecutarlo. Ahora
`required_tables` mete en el FROM toda tabla que un atomo toque, no solo la que agrupa.

Despues de los dos arreglos, las tres medidas hoja coinciden con la linea base a mano por `EXCEPT`
en las dos direcciones, sobre 1M de lineas.

#### `Margin` resuelto

`AtomSet` ahora tiene una capa de SALIDAS encima de la lista plana de hojas:

```rust
pub enum Shape {
    Atom(usize),           // indice en AtomSet.atoms
    Number(f64),
    Binary { op: BinaryOp, left: Box<Shape>, right: Box<Shape> },
}
```

Una hoja repetida es UN atomo: `Margin` referencia `Revenue` dos veces y son dos indices al
mismo atomo. Sin deduplicar eran tres atomos con dos identicos, y el informe tenia una columna
invisible en medio.

El SQL repite la sub-expresion del agregado, no la envuelve:

```sql
((SUM((t0.quantity * t0.precio_unitario)) - SUM((t0.quantity * t1.costo)))
 / NULLIF(SUM((t0.quantity * t0.precio_unitario)), 0)) AS "atom_0"
```

`1 HASH_GROUP_BY`, `0 DELIM_JOIN`, y coincide con la linea base a mano por `EXCEPT` en las dos
direcciones. Medido sobre 1M de lineas.

Los parentesis van en cada nivel, no solo alrededor de los operandos. Lo que decide la
precedencia es el ARBOL: `SUM(a + b * c)` no es `SUM(a) + SUM(b) * SUM(c)`.

`NULLIF` en el denominador porque DuckDB devuelve `inf` para `x / 0` y `NaN` para `0 / 0`, y DAX
devuelve BLANK.

#### El contador de profundidad confundia dos cosas

`depth > 0` significa "hay una agregacion dentro de otra", que rompe el fast path porque
recalcula por fila: `SUMX(VALUES(A), CALCULATE(...))`. `Margin = ([Revenue] - [COGS]) /
[Revenue]` no es eso: es aritmetica entre agregaciones al MISMO nivel. Con la profundidad
incrementada al inlinear una medida, las tres hojas salian con `Granularity::Finer` y caian al
interprete sin motivo.

Inlinear una medida reinicia la profundidad de agregacion. Los iteradores se rechazan antes, en
`shape_of`, porque un iterador SI cambia el grano de verdad.

El mapa de valores de la celda se indexa por ETIQUETA de atomo, y la forma guarda el INDICE. Sin
las dos cosas no hay forma de saber que hoja es: con dos hojas de la misma etiqueta y distinto
contexto se devolveria la equivocada.

La linea base a mano lo resuelve repitiendo la expresion en la lista de seleccion, sin
subconsulta y sin `DELIM_JOIN`, como demostro el test de semana 1. Y asi lo hace el generador.

**Medido:** repetir la sub-expresion de agregado NO cuesta nada. Con las cuatro medidas hay 6
`SUM(` en la lista de seleccion y 3 en el `HASH_GROUP_BY`: DuckDB hace CSE. El riesgo de
"computar el agregado dos veces" no existe, y la forma de repetir es la correcta.

**Division por cero:** DuckDB devuelve `inf` para `10 / 0` y `NaN` para `0 / 0`, no `NULL`. Sin
`NULLIF` el grid muestra un numero visiblemente falso. DAX devuelve BLANK, asi que el fast path
necesita `NULLIF` y el interprete necesita una division que devuelva BLANK. Los dos caminos tienen
que coincidir o la misma medida da `inf` en uno y un blanco en el otro.

#### El hole que el arnes diferencial no cubria

El interprete NO podia evaluar una agregacion sobre expresion:

```
SUM('fact_sales'[qty])                                 Integer(165)
SUM('fact_sales'[qty] * 'fact_sales'[amount])          ERROR: no se puede interpretar
```

`evaluate_atom` devolvia `Unsupported` porque el atomo no tiene columna unica. Y el arnes estaba
verde con 27 casos porque todos usaban una sola columna, y el fast path si bajaba: la comparacion
solo miraba SQL.

Ademas el interprete no AGRUPABA. Devolvia un total unico y el arnes comparaba ese total contra
`SUM(atom_0)` sobre las filas del `GROUP BY`. Eso solo valida `SUM`: para `MIN`, `MAX` y
`AVERAGE` es comparar cosas distintas. Los tres divergian, y la culpa era del arnes.

Arreglado: `evaluate_grouped` recorre el producto cartesiano de los valores distintos de las
columnas de agrupamiento, con un tope de celdas que se dice en voz alta en vez de truncar en
silencio. Y el arnes compara las dos MATRICES fila por fila, ordenadas.

#### Cuatro fallos de esta tanda, todos silenciosos

1. **La propagacion borraba filtros concretos.** `propagate_one` hacia `without_column` sobre la
   columna destino y luego metia el semijoin. Un filtro como `product_id = 3` desaparecia en
   cuanto un filtro de la dimension llegaba por la relationship. El informe devolvia el
   subtotal de la region entera. No es un error ni un vacio: es un total que parece un detalle.
   Ahora `without_propagated_column` quita solo los semijoins.
2. **El contexto de celda se descartaba al agrupar.** `evaluate_atom` usa el contexto del ATOMO y
   descarta el de la celda. Correcto para un `CALCULATE` con modificadores, al reves para la
   agrupacion. Todas las celdas devolvian el total: un informe entero con la misma cifra.
   Ahora el filtro de grupo se SUMA al contexto del atomo.
3. **Los grupos vacios no existen.** Un `GROUP BY` de SQL no los devuelve. El chequeo por valor
   no los veía porque `COUNT` sobre cero filas da 0, no BLANK, y un 0 no distingue "vacio" de
   "cero". Ahora se comprueba si hay filas.
4. **El orden de filas era el del hash de DuckDB.** El `GROUP BY` no garantiza orden. El usuario
   veia las filas reordenarse al tocar un filtro, y el arnés divergia en todas las medidas no
   aditivas. El lowering emite `ORDER BY` por las columnas de agrupamiento, y el arnés ordena
   antes de comparar para que un fallo de orden no se confunda con uno de numero.

#### Sobre el esquema real

`boleta`, `product_boleta` y `catalogo_productos` no tienen columna de costo, y `COGS` la
necesita. El costo pertenece al PRODUCTO, no a la linea: es la forma correcta y lo que permite
calcular margen a nivel de categoria.

Decidir con el usuario si se agrega `costo` a `catalogo_productos` en la base real. Mientras tanto
todo corre contra `datan_bench`, una base aparte con el mismo esquema mas esa columna, y
1.000.000 de lineas sembradas. La base real no se ha tocado.

### Fase 2: CALCULATE, context transition y el fallback. P0

Entregable: `CALCULATE` con todos sus modificadores, context transition, `RELATED`, iteradores, y
el interprete por celda de grupo corriendo para las medidas que el path SQL rechaza.

Aceptacion dura: suite de conformidad de **al menos 60 expresiones** tomadas de modelos PBIP
reales, todas pasando por el interprete, **y al menos 40 produciendo resultado identico por el path
SQL**. El conteo de casos que coinciden **es** el entregable. Es la metrica de cobertura.

Supuesto mas riesgoso: que el interprete y el path SQL se mantengan semanticamente alineados.

Test de semana 1: construir el **harness diferencial primero, antes de cualquier motor**. Un runner:
modelo + query -> corre ambos paths -> diff. Si lo construyes el dia 1, la divergencia aparece el
dia 2. Si lo construyes en la semana 4, pasaras un mes preguntandote por que Import y el fallback
discrepan en el manejo de blanks.

### Fase 3: time intelligence y relaciones ciclicas. P1

Entregable: `DATEADD`, `PARALLELPERIOD`, `DATESYTD`, `PREVIOUSYEAR`, mas un `CROSSFILTER(BOTH)`
sobre un ciclo.

Aceptacion dura: time intelligence coincide con valores calculados a mano sobre una tabla de
fechas contigua, todas las funciones, todos los cortes de mes; el ciclo bidireccional reproduce
exactamente un computation manual de dos saltos.

Supuesto mas riesgoso: que el manejo de ciclos en el algebra de filtros sea correcto, no rapido.

Test de semana 1: antes de escribir el optimizador, tomar un grafo ciclico y enumerar la
propagacion de filtros a mano. Si el orden de propagacion es ambiguo, el algebra tiene un bug y el
optimizador lo va a tapar.

**Resultado, ejecutado antes de escribir nada:** CONVERGE. Todas las permutaciones del orden de las
relationships dan el mismo punto fijo, y el filtro da la vuelta completa por el ciclo
(Product -> Bridge -> Customer -> Bridge -> Product). La propagacion es conmutativa.

Lo que esto **no** significa: que el ciclo se pueda bajar a SQL. Un `JOIN` exige arbol de joins
aciclico. Por eso `has_bidirectional_cycle` existe igual, para rechazar el fast path con un
mensaje y dejar el calculo al interprete.

**La ambiguedad de la que habla la documentacion de Power BI no esta en la propagacion de filtros.
Esta en la secuencia de medidas.** El algebra es conmutativo; `CALCULATE` encadenado no lo es.

La enumeracion encontro dos bugs antes de encontrar uno en el algebra:

1. El generador de permutaciones pasaba sufijos en vez de permutaciones, asi que reportaba una
   divergencia que no existia. Una enumeracion mal escrita produce un veredicto falso, y un
   veredicto falso sobre la ambiguedad del ciclo manda cambiar el diseno del motor por nada.
2. El modelo de prueba declaraba columnas que las relationships no referenciaban, asi que la
   propagacion nunca ocurria y el test pasaba sin probar nada.

Comprobacion del ciclo, dos: el detector no puede dar falsos positivos. Un star schema normal es
el caso comun y rechazarlo seria rechazar el 90% de los modelos.

#### Time intelligence

Toda la time intelligence de DAX es un `CALCULATE` con un filtro sobre una tabla de fechas. La
unica diferencia con `ALL` es que el filtro lo produce un CALCULO, no un literal. El rango son dos
filtros, `>=` y `<=`, y no un `BETWEEN`: los dos operadores ya existian en el algebra y por eso el
rango se propaga por las relationships sin un caso especial.

Un rango vacio se modela con un filtro imposible, no con la ausencia de filtros. La ausencia
significa "sin restriccion", que es lo contrario: la celda devolveria el total en vez de BLANK.

Semantica que hay que fijar con test porque no es intuitiva:

- `DATEADD` ancla en el inicio y conserva la longitud. Un rango que cruza el cambio de ano se
  comprime dentro de diciembre: 28 dic 2023 - 6 ene 2024, desplazado un ano, da
  28 dic 2022 - 10 dic 2022.
- `PARALLELPERIOD` recorta a los limites del periodo calendario y si conserva el dia.

Quien quiere "el mismo mes del ano pasado" usa `PARALLELPERIOD`. Quien quiere "la ventana
desplazada" usa `DATEADD`. Un acumulado mensual y uno anual NO pueden dar el mismo numero.

Trampa de calendario: la division de trimestres tiene que ser `div_euclid`. Con truncacion hacia
cero, `-1 / 4` es `0`, y "un trimestre antes de Q1" cae en Q4 del mismo ano. El error solo se ve al
cruzar el limite de ano, que es justo cuando un acumulado trimestral compara Q4 contra Q4 del ano
anterior.

#### Tres fallos que la fase destapo, todos de la misma clase

Todos devuelven numeros sin fallar. Ninguno lo detectan los tipos.

1. **`render_value` bajaba `Date` como `DATE '19723'`.** DuckDB espera `DATE '2024-01-01'` y
   responde con un error de conversion. **Todo filtro de fecha habria fallado en el fast path.**
   La funcion no tenia cobertura porque no existia ningun filtro de fecha hasta ahora.
2. **`run_suite` se comia los errores con `filter_map(..ok())`.** Ocho casos de time intelligence
   desaparecieron del informe sin dejar rastro, y el recuento bajo como si estuviera correcto. Un
   caso que error no es un caso que no cuenta.
3. **`NULL` de SQL se reportaba como "sin SQL".** Un `SUM` sobre cero filas es `NULL`, asi que
   todos los casos de rango sin ventas se contaban como "el interprete es el unico camino". Los
   tres estados son distintos: sin fast path, `NULL`, y divergencia.

Y un cuarto, en el checker, que llevaba meses oculto por el `filter_map`:

4. **`= 'Oeste'` fallaba con "`Oeste` no existe en el modelo".** En DAX un nombre entre comillas
   sin `[` detras es tabla (`COUNTROWS('Fact Sales')`) o literal (`= 'Oeste'`), y el parser no
   puede decidirlo. El checker lo resuelve contra el modelo: si el nombre existe, es tabla; si no,
   es texto. Solo aplica a nombres entre comillas. `= Oeste` sin comillas sigue siendo error de
   nombre, que es lo correcto.

### Fase 4: import de PBIP / TMDL. P1

Entregable: abrir un proyecto PBIP real y obtener numeros.

Aceptacion dura: importar AdventureWorks, y que **diez medidas coincidan exactamente con una linea
base de Power BI Desktop**, registrada por una persona corriendo Power BI Desktop.

**Dependencia oculta:** necesita una maquina Windows y un humano. No es un riesgo de codigo. Es un
riesgo de calendario que aparece el dia 1 de la fase y la bloquea entera. Reservarlo en la semana 1
de la Fase 4, no en la semana 3.

Riesgo: que `ripbi-core` muera. **Mitigacion, hacerlo desde ya:** vendorizarlo detras de un trait
propio y parsear hacia **AST propio**. Su DAG de dependencias con petgraph y su lexer de DAX son
genuinamente valiosos. Su esquema de AST no lo es, y adoptarlo hereda para siempre el esquema de
alguien de 6 semanas de vida.

### Fase 5: canvas de reportes y charts. P2

Aceptacion dura: matriz mas dos charts, cross-filter bidireccional, y un export PBIR que Power BI
Service acepta. La aceptacion de Service es el test que importa, porque la compatibilidad de import
era un objetivo declarado.

Charts nativos con primitivas de GPUI y `lyon`, que ya es dependencia de GPUI. Sin vello, sin
webview.

### Fase 6: refresh, carga incremental, columnas calculadas. P2

Las columnas calculadas son un **estadio de materializacion separado**, no parte de ninguno de los
dos backends. Se evaluan en refresh, en row context, sin filter context, y se escriben como arrays
Arrow dentro de DuckDB. No intentar enrutarlas por el path de query.

Guardrail de refresh: cargar a tabla de staging, luego swap dentro de una transaccion, con hash de
contenido del modelo para invalidacion de cache.

---

## 7. Las cinco apuestas del producto

Cada una tiene que ser una consecuencia del stack, no una feature.

**1. Archivos de modelo en disco, diffeables, commiteables.** El Import de Power BI Desktop acaba
en ~10 GB y ahi es una apuesta de RAM; la pagina de memoria miente sobre el swap. DuckDB da un
archivo unico que spillea a disco, se diffea en git y se mergea. Concretamente: **el modelo es un
archivo en el repo del usuario, revisable en un pull request, y los datos son un archivo aparte
que no lo es.** Es un modelo de objeto distinto al de Power BI y no se puede anadir despues,
porque cambia como funcionan refresh, locking y comparticion.

**2. Un modo con eleccion de plan, no dos modos.** Power BI obliga a elegir Import *o* DirectQuery,
con semanticas distintas y peor UX en el segundo. DuckDB habla Postgres, MySQL y SQLite
nativamente, asi que se elige **por query** si se va a la cache o se hace pushdown. Forzado por
tener un motor en vez de dos. Solo se sostiene si se mantiene Postgres como fuente de pushdown y
DuckDB como store local, que es exactamente la arquitectura decidida, asi que la apuesta ya esta
pagada.

**Medido 14 Oct 2026 contra Postgres 18 local, 10M de ventas** (`probe_pushdown --bi`):

| Consulta | Postgres directo | Cache DuckDB | Factor |
| --- | --- | --- | --- |
| `GROUP BY region, quarter` con join, `SUM(qty * price)` | 372 ms | 18.8 ms | **20x** |

La cache gana por mucho, lo que confirma el valor de la apuesta. Deja una duda abierta: el
plan muestra `TABLE_SCAN`, no `POSTGRES_SCAN`, y la agregacion no aparece como
`POSTGRES_SCAN_PUSHDOWN`. Puede que las filas enteras viajan y la agregacion se resuelva en
DuckDB. **Pendiente de investigar, no bloquea la Fase 1** porque la cache ya es 20x mas rapida.

Complicacion extra: `postgres_scan_pushdown()` acepta una **cadena de conexion**, no el alias
del `ATTACH`. La via manual exige la URL de nuevo.

**3. Un language server real para DAX, porque DAX es un programa real de Rust.** Rename,
find-references, go-to-definition a traves del modelo, y tests unitarios para medidas. La barra de
formulas de Power BI no tiene nada de esto. Esta apuesta depende **enteramente** del layout de la
seccion 4: un front-end de DAX que no se pueda compilar sin gpui nunca puede tener un LSP. El payoff
esta *aguas arriba* de la UI y es la apuesta con mejor ratio valor/hora de la lista.

**4. Offline total, multiplataforma, zero-install.** Un binario para macOS, Linux y Windows que
nunca llama a casa. Power BI Desktop es Windows-only y ahora exige conexion a Fabric. Forzado por
GPUI mas cargo-dist, y ahora verificado: la extension de Postgres se precarga en el bundle en vez
de descargarse en el primer uso. Coste: 31 MB por plataforma en el instalador. Es una apuesta de
segmento: compradores air-gapped, soberanos y regulados. Choca contra la apuesta 2, y por eso el
plan necesita que **ambas** sean verdad.

**5. Analisis con presupuesto de frames bajo una query viva.** Power BI bloquea el reporte durante
el refresh. GPUI tiene un contrato de frames duro y DuckDB corre en su propio hilo con un par
verificado `get_interrupt_handle()` / `interrupt()`. asi que: **se sigue clicking, se sigue
filtrando y se sigue editando mientras una query sigue corriendo, y cancelar es instantaneo.**
Para un producto local de escritorio eso es una diferencia sentida, no un benchmark.

---

## 8. Riesgos

### P0, bloqueantes o definidores de calendario

**R1. Split arrow ^58 / 60.** Cubierto en la seccion 2.3. Guardrail: cero dependencia directa de
`arrow`, verificado por CI sobre `cargo metadata`.

**R2. Descarga de la extension de Postgres.** Resuelto en la seccion 2.1. Guardrail: la app
precarga `postgres_scanner.duckdb_extension` y usa `extension_directory` con
`autoinstall_known_extensions = false`. Verificado en `probe_offline`.

**R3. Threading de la conexion DuckDB.** Cubierto en la seccion 2.2. Guardrail: una conexion, un
hilo de motor, cola serializada.

**R4. DAX no tiene especificacion.** `dax.guide` lo mantiene la comunidad. Los modelos de Power BI
en el mundo dependen de comportamiento de motor no documentado, y hay una cola larga: `INFO.*`,
`LOOKUPVALUE` dentro de `CALCULATE`, `PATHCONTAINS`, `USERNAME`, semantica de wildcards en
`CONTAINSSTRING`. **Este es el riesgo mas grande del proyecto y ninguna cantidad de ingenieria lo
elimina.** Guardrail: construir la suite de conformidad desde modelos PBIP reales, no desde la
especificacion imaginada.

**R5. `formula-dax` es la base equivocada.** No esta en crates.io; el repo tiene 109 stars,
Apache-2.0, ultimo push en 2026-03. Su `InMemoryTableBackend` es `Vec<Vec<Value>>` y su
`FilterContext` es un modelo de slice-and-dice de tabla unica. **La propagacion multi-tabla con
lineage tags no es una extension de eso, es una reescritura.** Leerlo por la estructura de
`CALCULATE` y context transition. No depender de el y no construir sobre su `FilterContext`.

### P1, significativos pero manejables

**R6. GPUI no tiene semver y hay que escribir un `Element` custom.** gpui-kit 0.7.3 pinnea
`gpui-pre = "=0.3.9"`, un snapshot de Zed. `use gpui_kit::*` es un glob sobre una biblioteca con
capa shell, asi que cada upgrade es una tormenta de renombres en todo el repo. Peor: un grid de
1M filas no se puede construir con `DataTable` ni `VirtualList` de gpui-kit, asi que **hay que
escribir un `Element` custom**, que ata a una API sin ruta de upgrade y sin garantia de semver. Es
el riesgo de plataforma de Zed y se acepto al elegir el stack. Unico mitigacion: saber que es un
impuesto de mantenimiento permanente sobre un fork. Mantener el Element lo mas thin posible.

**R7. El grid es el producto y nadie lo planifica.** Un grid real necesita virtualizacion de
columnas, paneles congelados, sort server-side sobre 1M filas, formateo por columna, clipboard sobre
una seleccion de 100k filas, y navegacion por teclado sin stutter. El editor de Zed es la
implementacion de referencia y su codigo no es lo que gpui-kit da. Presupuestar 3 a 6 semanas y
**arrancarlo el dia 1 de la Fase 1**.

**R8. Acceso Arrow a celda en el hilo de UI.** `RecordBatch` es columnar y `Send + Sync`; la UI
quiere la celda de la fila N, columna 3 en cada paint. Hacerlo con un lock y un downcast por celda
son 400 ms de frame que se presentan como bug de render y cuestan una semana. **Convertir a una
forma indexada apta para GPUI en el hilo de trabajo, antes de que el batch cruce la frontera.**

**R9. La pericia DAX de una sola persona.** Bus factor 1 en el dominio mas dificil del proyecto. El
harness diferencial y la suite de conformidad son la unica defensa real, porque ponen la semantica
en tests y no en una cabeza. Construirlos temprano y tratarlos como el entregable.

### P2, de plataforma

**R10. Vulkan en Linux.** wgpu/Vulkan falla en GPUs viejas, VMs y desktops remotos. Shippear un
fallback por software. No dejar que Linux bloquee macOS y Windows.

**R11. La semantica de refresh diverge.** El Import de Power BI siempre es consistente porque es
una recarga total mas un intercambio atomico. El de Datan es un archivo persistido con write lock, y
un refresh fallido puede dejarlo a medio escribir. Guardrail en la seccion 6, Fase 6.

---

## 9. La decision de la semana 1

**El contrato motor/UI es `Send + Sync` de datos Arrow mas un `CancelHandle`, demostrado con cero
DAX en los primeros dos dias.**

Todo lo demas en este diseño depende de eso. Si se sostiene, el motor DAX entero es una libreria
headless, testeable en milisegundos, sin display, y la UI es un consumidor thin. Si no se sostiene,
se descubre despues de escribir el motor, con un mes de costo hundido.

**El experimento:** un binario desechable.

1. Generar una tabla 1M x 12.
2. `query_arrow` a ella.
3. Pasar el `RecordBatch` a `cx.background_spawn`.
4. Renderizar con `uniform_list`.
5. Scrollear con `cx.spawn`.
6. Loggear tiempos de frame.

Condicion de paso: **el scroll mantiene 16 ms con la tabla entera residente en Arrow y ni un solo
tipo de duckdb en el crate de UI.** En la misma tarde, `cargo tree -d | grep arrow` confirma
exactamente una version.

Dos dias. Valida o mata la arquitectura, el modelo de threading, la eleccion de virtualizacion y R1
de una sola vez.

---

## 10. Cuando reconsiderar

- **R2 sale mal** (el build de DuckDB desde fuente duele en CI): volver a leer Postgres con el
  crate `postgres` y hacer `COPY` a DuckDB. Eso mata la apuesta 2 (pushdown) y cambia el modelo de
  refresh. Decide en la semana 1 de la Fase 1.
  **Estado: no aplica.** R2 se resolvio por precarga, no por build. Ver seccion 2.1. El fallback
  sigue vivo si el tamano de 31 MB por plataforma resulta inaceptable para el producto.
- **El fast path SQL cubre menos del 70%** de las medidas reales en la suite de conformidad al
  final de la Fase 2: el backend SQL no se esta ganando su lugar y deberia borrarse para quedarse
  nativo. El Measure IR se queda igual, y por eso construirlo primero es el orden correcto.
- **Un servicio resulta obligatorio**: todo el diseño disk-local tiene que revisarse. Anadir un
  servicio a una app local-first es otro proyecto, no un target de despliegue.
- **`gpui-pre` deja de moverse** o gpui-kit rompe contra un snapshot nuevo y se esta manteniendo un
  fork: en ese punto el `Element` custom del grid es lo que hace caro irse.

### Camino avanzado, solo si paga

Registrar funciones escalares de DuckDB que llamen de vuelta al evaluador DAX de Rust. Permite que
**una sola query** mezcle agregados nativos con fragmentos interpretados, asi que en vez de "fast
path o slow path" hay fast paths por atomo dentro de una query. `duckdb-loadable-macros` existe
para esto.

Solo vale la pena cuando la suite de conformidad muestre una poblacion real de medidas **mixtas**
donde la mayoria de los atomos son SQL-nativos y unos pocos no. Hasta que la suite lo muestre, el
split binario es mas simple y el IR carga la misma informacion. Revisar en Fase 3, y solo con datos.

---

## 11. Alcance descartado de forma explicita

Decisiones de producto, no de codigo:

- **Power BI Service, Fabric, workspaces, apps, gateway.** Otro producto, otro comprador.
- **Power Query M completo.** ~80 de las 700 funciones cubren la practica. El folding de query
  entre 40 dialectos es un proyecto entero. No esta en el alcance de v1.
- **DirectQuery, Dual, Hybrid, Direct Lake, Live Connection.** Modos enterprise o de nicho. Un
  solo modo con eleccion de plan por query.
- **Custom visual SDK.** Superficie grande, pasivo de seguridad, ROI bajo hasta que el set built-in
  sea excelente.
- **Paginated reports (RDL).** Motor y artefacto separados. Saltar salvo que la impresion sea el
  comprador.
- **Power BI Mobile.** Solo consumo, sin apalancamiento de autoria.
- **Dataflows, Datamarts, Eventhouse, Real-Time Intelligence.** Cargas de trabajo de Fabric
  adyacentes, no features de Power BI.
- **Sensitivity labels, BYOK, multi-geo, disaster recovery, private endpoints.** Superficie
  corporativa de TI, cero diferenciacion de producto.
- **Visual Q&A.** Microsoft lo esta retirando. No construir.

---

## 12. Trabajo de research pendiente

Ninguno bloquea el inicio de la Fase 0. Todo es para fases posteriores.

- Performance real de `DataTable` de gpui-kit a 1M filas. Documentado para "cientos de miles".
  Benchmark temprano con `bench-support`. Prepararse para el Element custom.
- Throughput real de los charts de gpui-kit a conteos de puntos realistas.
- Costo de text shaping en un grid denso. El comportamiento de la cache de anchos de `LineWrapper`
  bajo churn no esta documentado.
- Costo de construir `libduckdb-sys` con el scanner de Postgres en los tres sistemas objetivo.
- Firms con la semantica de Not Matching Blank en comparaciones de medidas DAX.