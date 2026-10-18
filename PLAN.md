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

#### Guardar el informe, y exportarlo

Un informe guardado lleva la DEFINICION, no el resultado. Dimensiones, medidas, filtros y
conexion, en `~/.datan/reports/`. Al abrir se vuelve a correr contra la base.

Guardar el resultado en vez de la definicion produceria un archivo que se ve bien y miente: el
informe de ayer con los numeros de ayer, presentados como si fueran de hoy. Es el fallo que un
producto de datos no puede tener.

Los ids de columnas y medidas son internos del modelo y cambian entre ejecuciones. Lo que viaja
son los NOMBRES, y al abrir se resuelven contra el modelo de esa conexion. Guardar ids daria un
informe que se abre sin error y con la matriz vacia: el fallo mas caro de esta pieza, porque
parece un informe sin datos y no un informe roto.

El nombre del informe ES el nombre del archivo, y se rechaza si trae `/`, `..` o empieza por punto.
Es la unica barrera entre un nombre de informe y una ruta fuera de la carpeta.

El CSV es el escape de RFC 4180, y el escape es la parte que falla: un valor de dimension es
texto libre, y `Producto, 1` con una coma parte la fila en dos. Lo grave es que el informe
seguiria SUMANDO bien y mostrando filas mal: los totales correctos con filas rotas, que es lo
peor de los dos mundos.

Guardar y exportar viven en la barra de filtros y no en un menu, porque son las dos acciones que
existen sobre un informe, y un menu que no se abre no ensena sus opciones.

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
## Fase UI-1: el lienzo Lumen (rediseno hacia el mockup)

Objetivo visual: barra superior (Datan > conexion > informe, guardado, Actualizar), panel
Datos a la izquierda (busqueda, tablas expandibles con conteo de filas), lienzo central
(titulo, rango 3M/6M/12M, tarjetas KPI con delta vs ano anterior y sparkline, linea
temporal, dona por categoria, tablas), panel Visualizaciones a la derecha (tipos +
propiedades: titulo, ancho, eje, valores), pestañas de paginas abajo.

El motor YA soporta casi todo: DAX completo, time intelligence (DATEADD/PREVIOUSYEAR para
el delta), totales sin GROUP BY, cross-filter, guardar. Faltante de motor: sparkline
(serie mensual = GROUP BY mes, ya se hace), dona (agregado por categoria, ya se hace),
delta porcentual (medida DAX con DATEADD, ya se escribe).

Orden de construccion, cada paso verificable headless donde se pueda:
1. Esqueleto de 3 paneles + barra superior + pestañas de paginas (sin logica).
2. Panel Datos: busqueda de tablas/campos (Input), expandir/contraer, conteo de filas.
3. Tarjetas KPI con delta: medida DAX con DATEADD -1 ano + sparkline (GROUP BY mes).
4. Linea temporal: GROUP BY mes de la tabla de fechas; dona: agregado por categoria.
5. Panel Visualizaciones: elegir tipo del visual activo + propiedades (titulo, ancho 1/4
   a completo, eje, valores).
6. Estado Editar/Lectura: en lectura el lienzo no responde clics de edicion.

Decisiones ya tomadas: los visuales se pintan con primitivas GPUI (divs, flex_grow, y
lyon si hace falta curva), sin libreria de charts ni webview. Las paginas viven en el
mismo archivo .toml de informe. El rango temporal es un filtro de fecha en el contexto,
no un caso especial.

## Correccion 2026-10-17: DATEADD conserva el dia del mes

Decision: `add_months` ya no salta al primer dia del mes destino. `DATEADD(-6, MONTH)`
desde el 31 de diciembre es el 30 de junio, no el 1 de junio, y el dia se recorta al ultimo
que existe en el mes destino (31 de enero un mes adelante es 29 de febrero de 2024).

Por que: Power BI conserva el dia del mes, y el proyecto acepta ser compatible a nivel de
funcion. La version anterior solo se notaba desde el dia 29, 30 o 31, y fallaba en silencio:
la ventana de un informe se alargaba hasta treinta dias sin que ninguna consulta fallara.

Efecto secundario que se corrigio de paso: la barra de rango de la app (3M/6M/12M) usaba
`shift` esperando una ventana y recibia un rango del mismo tamano que el de entrada. Como
`DateRange::new(hoy, hoy)` tiene tamano 1, la ventana entera se reducia al dia de hoy hace
N meses. La app ahora toma el INICIO de `shift` y el FIN es hoy, que es lo que quiere
decir "ultimos tres meses".

Pruebas: `dateadd_conserva_el_dia_del_mes` (nueva), `dateadd_mueve_la_ventana_y_conserva_el_
dia_del_ano` (reescribe la que fijaba el comportamiento viejo),
`la_ventana_anterior_mide_lo_mismo_que_la_actual`. 179 tests, 38 casos de conformance con 0
divergencias.

## Delta automatico en las tarjetas

Cada tarjeta con rango de fechas activo muestra su variacion contra la MISMA ventana un ano
antes. Es una pasada extra de `totals`, no N consultas: las medidas comparten sus grupos de
lectura. Un total anterior de cero no da porcentaje y la tarjeta dice "sin periodo anterior"
en vez de un infinito.

Sin rango activo no hay delta: el historico completo contra el historico completo un ano
antes no es una variacion.

## Fase UI-1 pasos 5 a 7: el panel Datos por tablas, cabecera del lienzo y pie

Siguiendo el mockup de Lumen al detalle:

Panel Datos:
- Lista de TABLAS con su conteo de filas a la derecha, la de hechos primero. La tabla se
  abre con el chevron y dentro estan sus campos. Una lista plana obligaba a recordar de que
  tabla era cada columna.
- `COUNT(*)` por tabla al conectar, no en cada refresh: el numero cambia muy despacio y el
  panel se dibuja en cada frame. Una tabla que falla cuenta 0 y no tumba el panel entero.
- Con busqueda activa una tabla se abre sola si tiene un campo que coincide, y solo se
  muestran los campos que coinciden: buscar "region" tiene que ENSENAR el campo Region.
- El modelo no guarda el esquema de la tabla, asi que el SQL del conteo usa la misma forma
  que el motor: `pg."tabla"`. Sin comillas, una tabla con guion o palabra reservada da un
  error de sintaxis que parece del motor.

Cabecera del lienzo:
- Nombre de la base como etiqueta, "Actualizado hace 2 min" al lado, titulo de la pagina en
  grande y los botones de rango a la derecha. Con la base vacia dice "Contando filas" en vez
  de una fecha que no existe.

Pie:
- Pestana con el nombre de la pagina y "N visualizacion(es)" mas las estadisticas del
  informe. N es lo que se PINTA segun el visual activo, no los tipos disponibles.

179 tests.

## Fase UI-2: replicar el mockup de Lumen

El alcance es la capa de presentacion. Ni una consulta, ni una regla de DAX, ni el modelo de
datos cambian. Lo que Datan tiene y Lumen no (barra DAX, "Crear medida", CSV, barra de filtros
activos) se queda, reubicado o reestilado.

### Paso 1: tokens y componentes base (20312a0)

`tokens.rs` junta las medidas y la escala de letra en un solo sitio, y la paleta de SERIES.
La paleta del tema (`chart.1` a `chart.5`) es una rampa de azul: cinco tonos del mismo color
no distinguen cinco categorias. `components.rs` da los botones, el control segmentado, la
pastilla, la fila de dato, la tarjeta y la nota. El lienzo pasa a gris y las tarjetas a
blanco, que es como Lumen separa las superficies.

182 tests.

### Paso 2: barras superior e inferior (2953e33)

La barra superior dice que informe se mira (marca, base, pagina, estado de guardado) y que se
puede hacer con el (undo, redo, Editar/Lectura, Compartir, Publicar). `last_saved` guardaba la
palabra "Guardado"; ahora guarda el instante y la barra escribe "Guardado hace X" o "Sin
guardar". Sin dependencia de zona horaria no hay reloj local: la hora en UTC seria la hora
equivocada. Undo, redo, Compartir y Publicar no tienen backend y se dibujan apagados.
Editar/Lectura si hace algo: en Lectura el clic no filtra y Guardar y Crear medida se apagan.
La barra inferior lleva la pestana, un "+" apagado y las estadisticas con plural de verdad.

182 tests.

### Paso 3: panel de Datos

Encabezado con el numero de tablas, buscador con la lupa dentro del campo, tablas plegadas con
su conteo de filas en mono, campos escalonados debajo, medidas con su conteo y el pie con
"Obtener datos", que vuelve a la lista de conexiones: es el unico camino que existe para
cambiar de fuente.

Tres defectos de presentacion que salieron al mirar la ventana:

- `hit` daba coincidencia con la busqueda VACIA (`contains("")` es verdadero para cualquier
  texto), asi que "sin busqueda" significaba "abre las tres tablas y muestra los treinta
  campos" y las tablas nunca nacian plegadas.
- `.flex_col()` sin `.flex()` deja el div como bloque: los hijos se apilan pero `flex_1` no
  crece, asi que el pie del panel quedaba flotando a media altura con un hueco al final. El
  alto del panel sale de la ventana, porque el alto al 100 % no se resuelve contra un
  contenedor con `flex_1`.
- `flex_1` y `overflow_y_scrollbar` en el MISMO div no conviven: el envoltorio del scroll se
  dibuja `size_full()` y empuja a los hermanos fuera de la caja. El `flex_1` va en un div y el
  scroll en el de adentro.

184 tests.

### Paso 4: cabecera del lienzo y tarjetas KPI

La cabecera se parte en dos filas. Arriba, de que informe son estos numeros: la base, cuando se
contaron las filas, sobre que columna filtra el periodo, el control de rango y actualizar. Abajo,
el titulo y solo el titulo: compartir fila con las etiquetas lo convertia en una etiqueta mas. El
control de rango y "sobre <columna>" solo existen si hay columna de fecha conectada, porque un
rango que no filtra nada y no lo dice es la falla silenciosa de siempre. Actualizar se mueve al
lado del periodo: las dos cosas cambian QUE datos se ven.

La tarjeta KPI lleva el nombre de la medida, el total, el delta contra el periodo anterior y una
linea mensual. La linea es una consulta extra y no doce: se agrupa por la columna de fecha y los
dias se suman en el mes que les toca.

La linea se dibuja solo si sumar los meses da el total, y eso lo decide la FORMA de salida de la
medida (`extract_atoms`), no su texto. `AVERAGE`, `DISTINCTCOUNT` y `SUM(a)/SUM(b)` no llevan
linea: en las tres, la linea serie una mentira con forma de dato. Sin `outputs` (un `SUMX` con
contexto de fila) tampoco, porque no se puede saber.

Dos defectos de presentacion que salieron al mirar la ventana:

- Un `canvas` se mide por su contenido. El div de afuera tenia 96 x 28 px y el canvas heredaba un
  alto de CERO, asi que los catorce puntos caian en la misma `y` y la linea salia recta y
  perfecta. El tamano va en el propio canvas.
- `.flex_col()` sin `.flex()` otra vez, esta vez en la tarjeta, con el mismo sintoma: los hijos
  apilados y el `flex_1` sin crecer.

Hallazgo fuera de alcance: `Report::totals` pega sus resultados por POSICION mientras `execute`
los pega por conjunto de atomos. Con dos medidas que comparten atomos, la segunda tarjeta muestra
"-" y el grid muestra el numero. No se toca: es el motor.

Hallazgo fuera de alcance, y el de mas peso: un esquema con fechas guardadas en `timestamp` no
tiene control de periodo ni time intelligence. `datan-connect` mapea `timestamp*` a
`DataType::DateTime`, y las dos busquedas de columna de fecha (la de la app y la de
`datan-dax-eval::time`) piden `DataType::Date`. `datan_bench` cae justo en ese caso, asi que para
mirar la linea se le agrego una columna `fecha date` con DDL sobre el banco de pruebas. La columna
es dato de prueba, no codigo.

185 tests.

### Paso 5: los graficos y la tabla

Hasta este paso el visual elegido cambiaba el color de un boton y nada mas: los tres tipos
pintaban el mismo lienzo. Ahora el tipo decide QUE se dibuja, y el lienzo tiene tres zonas: las
tarjetas, el grafico y la tabla.

Los graficos salen de `gpui_kit::component::chart` y no de una dependencia nueva. Columnas es
`BarChart`, Lineas es `LineChart`, Area es `AreaChart` y Anillo es `PieChart` con
`inner_radius`. El eje de valores reserva su caja a partir del ancho de sus propias etiquetas,
que es lo que el mockup de referencia no hace: ahi "0 mil EUR" sale cortado por el borde. Los
tics pasan por `format_value`, la rejilla va solida y el tooltip dice el valor como se lee.

Decision: las Barras siguen dibujandose con divs. Un grafico del sistema no avisa QUE barra se
toco, y tocar una barra pone un filtro, que es una funcion que ya existia. Columnas es el
`BarChart` real, asi que el componente se ejercita igual. El anillo paga el mismo precio al
reves: sin etiquetas sobre las porciones (ocho lineas de guia alrededor de un circulo se pisan),
con una leyenda de dos columnas que dice lo mismo y donde cada fila filtra.

El titulo es el nombre de la medida y el de la dimension, sin traducir, y al lado dice cuantos
valores quedaron afuera cuando la matriz tiene mas filas que los ocho que se dibujan.

La tabla pasa a una tarjeta blanca con la cabecera redondeada, y la flecha de orden aparece solo
en las columnas de medidas: sobre una dimension no ordenaba nada, y una flecha que promete algo
que el clic no cumple es peor que ninguna.

Tres hallazgos de layout, los tres medidos en la ventana:

- `flex_1` y `min_h` no alcanzan para darle alto a una caja con `uniform_list` adentro: la caja
  mide su contenido. El alto de la tabla y el del grafico van explicitos, y se reparten por tipo
  de visual (con la tabla, el grafico mide 220; sin la tabla, 430; con la tabla sola, la tabla
  mide 430).
- `.flex_col()` sin `.flex()` otra vez, ahora en el div del lienzo: sin `flex` el lienzo crecia
  con su contenido y empujaba la tabla fuera de la ventana.
- `overflow_hidden` corta, no encoge. El contenido que no cabe no busca su scroll: desaparece.

185 tests.

### Paso 6: el panel de visualizaciones

Los ocho tipos de la referencia, cuatro por fila, cada uno con su icono. El tipo elegido se
pinta con el fondo del texto principal y el texto del fondo de la pagina: en el panel no hay
borde de color ni sombra que diga cual esta activo.

`Dispersion` se dibuja apagado y sin clic. No se esconde: un tipo que falta sin decirlo se busca
en el panel, y no se encuentra. Tampoco se dibuja como los demas, porque un mosaico que se ve
activo y no responde se lee como un cuelgue.

Hallazgo, y el de mas peso de este paso: la app registraba `assets::Assets`, el juego corto de
106 iconos del componente. Los siete que usa esta pantalla (`chart-area`, `chart-bar`,
`chart-column`, `chart-line`, `chart-scatter`, `square-stack`, `table`) no estan ahi: el logo de
la barra superior, los cuatro tipos de grafico, los mosaicos y la tabla salian como un hueco en
blanco, y el unico icono que se veia era el del anillo, que si esta en el juego corto. Se
registra `assets::AllAssets`, el catalogo completo. No es una dependencia nueva: es el mismo
crate con el otro conjunto de archivos embebidos.

Propiedades pasa a decir lo que se esta mirando: el titulo del visual, su ancho, el eje y los
valores, todos con los nombres del modelo. Antes decia "Titulo: Informe" y "Ancho: completo" con
el informe que fuera, dos renglones que no cambiaban nunca. El titulo sale de la MISMA funcion
que el titulo de la tarjeta del grafico y no de una copia, porque dos cadenas que dicen lo mismo
se separan el dia que una cambia.

Un visual por pagina: el lienzo compuesto (tarjetas, grafico y tabla) no es uno de los ocho
tipos. Lo representa el mosaico `Tarjetas`, que es su zona de arriba. Sin eso la grilla se queda
sin ningun mosaico encendido al abrir la app, que se lee como un error.

`.flex_col()` sin `.flex()` otra vez, ahora en el panel de la derecha, y esta vez el sintoma era
visible: los hijos apilados sin estiramiento.

185 tests.

### Paso 7: la pasada de consistencia

Cinco radios distintos en la misma pantalla (6, 12, 16 px) pasan a dos: 8 px en todo lo que se
toca y en todo lo que agrupa una fila (botones, pastillas, campos, filas de la barra lateral y
del grid, mosaicos) y 16 px en una tarjeta. Las marcas de un grafico bajan a 4 px, que es lo que
ya usaba la columna del grafico de columnas: una barra de 18 px con la esquina de un boton se lee
como un boton. Los literales `px(20.0)`, `px(13.0)`, `px(12.0)` y `px(11.0)` pasan a los tokens
que ya existian, y el 13 estrena nombre (`LETRA_CUERPO`), que era el tamano del texto corriente
escrito a mano en cuatro sitios.

Los dos paneles hermanos estrenan la misma cabecera: mismo alto (40 px), mismo relleno y la misma
linea de abajo. Antes la de Datos media 40 y la de Visualizaciones lo que midiera su texto. La
caja del control segmentado se queda en 28 px, el alto de un boton: el segmento media 22 y la
caja 30, y la barra superior tenia dos altos distintos a la vista.

Hallazgo de fondo, y el defecto mas visible de todo el trabajo: la tabla se dibujaba DEBAJO de la
barra de pestanas. El lienzo medía lo que medía su contenido (cabecera, barra DAX, tarjetas,
grafico y 210 px de tabla) y `h_full` no se resuelve contra un `flex_1`, asi que la columna se
salia de la ventana. A 1470 x 956 (la referencia) el contenido entraba con 32 px de sobra y el
defecto no se veia. En la pantalla real, que deja 888 px utiles despues de la barra de menus, la
ultima fila y el borde de la tarjeta caian detras de las pestanas. Se arregla con el mismo
recurso que el panel de Datos: un alto calculado desde la ventana. Con el alto definido, la tabla
ya no necesita su propio alto fijo: se queda con lo que sobra, que es distinto en cada ventana, y
`ALTO_GRID` desaparece. El grafico hace lo mismo cuando es lo unico del lienzo.

Tres `.flex_col()` sin `.flex()` mas, los tres en el mismo sitio: el estado vacio. Sin `.flex()`
la caja no era flexible y `items_center`/`justify_center` no centraban nada. Verificado forzando
la condicion del estado vacio y capturando la pantalla.

Fuera el ruido de consola de los eventos: quedaban cuatro `eprintln!` de depuracion (clic en una
conexion, medida creada, rango elegido, clic en una celda). El de un guardado fallido se queda:
es la unica senal de un error que la pantalla no dice.

185 tests.

## Fase UI-3: los bucles rotos. El modelo del usuario y el informe guardado

La app de la Fase UI-2 se veia bien y no podia cerrar dos ciclos. Los dos son de la misma clase:
algo se escribe, se guarda, y al volver no esta.

### Paso 1: el modelo del usuario vive en disco (80880fe)

`Crear medida` ponia la medida en la RAM del `Report`. `Guardar` guardaba su NOMBRE en el TOML.
Al reabrir, `saved::open` resuelve nombres contra el modelo, la medida no estaba en la base y el
informe moria con `UnknownMeasure`. O sea: `Crear medida` + `Guardar` producia un archivo que no
abre, y el usuario no tenia forma de saberlo hasta el dia siguiente.

El modelo del usuario se guarda ahora aparte, por base, en `~/.datan/models/<base>.toml`:
medidas propias y ajustes de columna. Se aplica ENCIMA de la introspeccion al conectar.

Gana la introspeccion. Una medida que ya existe en el modelo no se toca: aplicar el documento dos
veces no duplica, y un nombre que la base ya trae no se pisa. Una medida que referencia a otra del
mismo documento chequea contra el modelo en su estado actual, asi que las dos se aplican en orden.
Una medida que ya no chequea (la columna se renombro en la base) se SALTA con un aviso en la barra
de estado, y se queda en el documento: renombrar algo en la base no es razon para tirar trabajo.

Los ids de las medidas del usuario salen de `max + 1`, no de un literal: un literal se desincroniza
la primera vez que la base trae una medida con mas id.

`Crear medida` escribe el documento ANTES de tocar la vista. Si el archivo no se puede guardar, la
medida no se agrega y el usuario ve por que. Al reves quedaria una medida en pantalla que se pierde
al cerrar, que es el defecto que este paso arregla.

14 tests. 199 en total.

### Paso 2: el informe guardado se puede abrir

`saved::list` existia y nadie lo llamaba. El nombre del informe estaba escrito a mano en tres
sitios de `main.rs` ("Resumen de ventas"), asi que el archivo guardado y el titulo pintado no tenian
nada que ver.

El archivo gana dos campos que son de la PANTALLA y no de la consulta: el visual del lienzo y el
periodo activo. Reabrir un informe guardado en "Anillo" y que aparezca en "Tabla" es abrir otro
informe. Van en un `SavedView` aparte de `Report` a proposito: mezclarlos con los filtros haria que
cambiar de vista cambiara los datos.

Los filtros guardados pasan de un valor a un conjunto (`values: Vec<String>`), y el alias
`value = "..."` sigue leyendo un informe de la version anterior. Era el momento: hay UN informe
guardado en disco, y convertir el campo despues deja archivos que no abren.

La pantalla de abrir es nueva (`State::Opening`). Lista los informes y marca los que no son de esta
base. Se compara el QUALIFIER del informe (`pg`), que es lo que guarda el archivo, y no el nombre de
la base: comparar con `self.database` daba todos los informes como ajenos.

El nombre del informe se deriva de la PRIMERA DIMENSION DE LA SELECCION, no de `self.report`.
`self.report` es el informe base que acumula filtros y `refresh` le pone las dimensiones al correr,
asi que leer de ahi daba "informe" mientras la matriz mostraba `nombre`. El mismo error tenia el
`Guardar` de antes, que por eso guardaba todo bajo `informe.toml`.

Dos defectos del motor que salieron al ejercitar el camino nuevo, tapados porque nada construia un
`IN`:

- `lower_filter` bajaba `IN` con UN valor como `col IN 'x'`, que no es SQL. El parentesis no es
  opcional. Con varios valores si lo ponia.
- Un operador de comparacion con varios valores tomaba el primero en silencio. Ahora es el AND de
  todos, que es lo que DAX significa, y no un filtro que no se pidio.

`Report::filtered_to` es el primitivo de un slicer: un conjunto de valores para UNA columna. Un
conjunto vacio quita el filtro en vez de dejar un `IN ()`, que el SQL baja como `1 = 1` y el
interprete evalua como falso. Esa divergencia estaba viva y no era alcanzable. Con este paso deja de
poder construirse. `refresh` agrupa los filtros activos por columna y los aplica con `filtered_to`,
que es lo mismo para un valor y una union para varios.

204 tests. 38 casos de conformidad, 0 divergencias.

## Fase UI-4: una medida no es su primera hoja

El primer paso del plan de brechas con Power BI. Es un defecto del MOTOR y no de la pantalla, y
estaba en el camino mas comun: una medida de negocio escrita con `DIVIDE`.

### El defecto

`Shape` tenia tres casos: una hoja, un numero y una operacion binaria. El nivel superior de una
medida que NO es una agregacion ni una operacion aritmetica no tenia forma. `shape_of` devolvia
`None`, el conjunto de atomos quedaba con sus hojas y SIN salidas, el plan emitia una columna por
hoja, y `Report::execute` pegaba la columna de la hoja 0 en la columna de la medida.

`DIVIDE([Qty], [Amount])` daba el valor de `Qty`. Con el tipo correcto, sin error y sin aviso. Una
medida que se ve bien y miente es el peor resultado posible de esta pieza.

La misma clase de defecto alcanzaba a `VAR`/`RETURN` (el bloque entero quedaba sin forma) y a un
literal negado, que el parser produce como `Unary(Negate)`. `DIVIDE(a, b, -1)` no tenia forma por
el `-1`.

### La forma de una llamada

`Shape::Call { name, args: Vec<Shape> }`. Una lista blanca de cuatro funciones: `DIVIDE`, `IF`,
`COALESCE` y `SWITCH`. Cualquier otra devuelve `None`, y `None` es lo correcto: una forma a medias
seria peor que no tener forma, porque el plan la tomaria por valida y el numero saldria de la hoja
equivocada.

`Block` vale lo que su cuerpo y `Variable` lo que su valor: las dos son plegados, no evaluacion.
Las ligaduras de un `VAR` no tienen efecto y el chequeo ya resolvio las referencias.

Los dos caminos tienen que dar el mismo numero, asi que las cuatro funciones se escriben dos veces
a proposito:

- SQL, en `render_call`: `DIVIDE` -> `((a) / NULLIF((b), 0))`, con el valor alterno dentro de un
  `COALESCE`; `IF` -> `CASE WHEN`; `SWITCH` -> `CASE expr WHEN v THEN r ... END`. El `NULLIF` no es
  decorativo: DuckDB devuelve `inf` para `x / 0` y DAX devuelve BLANK.
- Interprete, en `call_scalar`: `DIVIDE` reusa `binary_numbers`, que ya tiene la regla del cero;
  `IF` con la condicion en BLANK toma la rama falsa; `COALESCE` devuelve el primer valor no vacio.

La condicion de un `IF` es una comparacion, y comparar no es aritmetica: `binary_numbers` devuelve
BLANK para todo lo que no sea `+`, `-`, `*` y `/`. Con eso, la condicion salia BLANK y la medida
tomaba SIEMPRE la rama falsa. Hay ahora una rama de comparaciones en el interprete
(`boolean_values`) y los mismos operadores en `render_shape`, que antes eran un error.

`forma_aditiva` responde `false` para una llamada: `DIVIDE` es un cociente y `IF` elige, asi que
sumar sus meses no da el total. Esa medida no lleva racha en la tarjeta, que es preferible a una
racha equivocada.

### La clave del plan no eran las hojas

La prueba de `IF` expuso un defecto vecino. Los planes se agrupaban por la FIRMA DE LOS ATOMOS, y
`Qty` (`SUM(qty)`) y `Condicional` (`IF(SUM(qty) > 50, ...)`) comparten hoja. Caian en el mismo
plan, la consulta producia una columna con la forma de la primera, y las dos medidas mostraban el
mismo numero.

Dos medidas comparten consulta solo si comparten las hojas Y la forma de salida. La agrupacion vive
ahora en `Report::plans`, y `execute`, `query_count` y `totals` la usan. Antes eran tres bucles
iguales, y no decian lo mismo: `query_count` contaba por firmas y `execute` agrupaba por firmas, asi
que el numero de consultas y la ejecucion podian discrepar sin que nada lo notara.

Dentro de un grupo, la consulta produce una columna por FORMA, no por medida. Cuando el grupo tiene
mas medidas que columnas, las de mas son identicas y leen la ultima. `execute` ya lo hacia;
`totals` leia por posicion y dejaba la segunda tarjeta de dos medidas iguales EN BLANCO. Un guion en
una tarjeta parece un dato que falta, no una medida que no se pudo leer.

### Resultado

Cuatro casos de conformidad nuevos con la forma de salida y su condicion, mas el denominador cero
con y sin valor alterno. Los dos caminos coinciden.

208 tests. 48 casos de conformidad, 44 por los dos caminos, 0 divergencias.

### Lo que sigue abierto, y por que

Una medida que NO tiene forma (un `CALCULATE`, un iterador, una funcion de texto) sigue sin poder
resolverse con el mapeo de columnas, que es "una columna por salida". Hoy no se nota porque la app
no le pasa ningun `DataSource` y esas medidas fallan con `NoSource` en vez de dar un numero. El
arreglo NO es una forma: el interprete tiene que producir UNA columna de medida para la expresion
entera, y eso va con el paso del `DataSource` sobre DuckDB, que es el que hace que esas medidas
corran en la app.

El campo `expected` de `Case` no lo comprueba nadie: `expecting(...)` documenta la intencion y
`run_suite` no lo mira. Los casos se validan por la coincidencia entre caminos, que es lo que
importa, pero un valor escrito a mano que nadie compara es una promesa sin comprobar.


## Fase UI-5: una medida interpretada da su valor

Las medidas con `CALCULATE`, iteradores o funciones sin forma caian al camino viejo: el plan
emertia una columna por atomo y el informe pegaba la primera en la columna de la medida.
`Variacion = [Qty] - CALCULATE([Qty], ALL('dim_product'))` daba CERO, sin error y sin aviso. Es la
forma en que se escribe una variacion contra el total, y era el numero mas comun que salia mal.

Cuatro defectos, todos del mismo tipo: una hoja que no es la que toca.

1. `CellResult` era un mapa por ETIQUETA. Las dos hojas de `Variacion` son las dos `SUM(qty)`, con
   distinto contexto, y la segunda pisaba a la primera. Es una lista en el orden de la tanda de
   atomos: `eval_shape` lee por INDICE, que es lo que la forma guarda.
2. `extract_iterator` esperaba dos argumentos y el chequeo deja el iterador con UNO, el cuerpo: un
   `SUMX` no extraia NINGUN atomo. Nadie lo habia notado porque ningun caso de la suite usa
   iteradores.
3. Los modificadores de `CALCULATE` viajan ahora con el atomo y se RE-APLICAN sobre el contexto de
   cada celda. La union atomo+celda le devolvia al `ALL` el filtro que acababa de quitar: agrupado
   por region, el total daba el valor de la region. La dedup comparacion incluye los modificadores,
   porque en un modelo sin filtros las dos hojas de `Variacion` tienen el mismo contexto efectivo.
4. `Shape::Atom` tambien para CALCULATE e iteradores: su cuerpo bajo el contexto modificado es una
   hoja. Un CALCULATE dentro de otro apila los modificadores en orden de aplicacion.

Una medida sin forma (`ABS([Qty])`) es un ERROR del informe, `NoShape`, con el nombre de la medida.
Antes mostraba el valor de `Qty`. Un numero equivocado sin aviso es peor que un error que dice lo
que falta.

### El origen del interprete

`datan_report::source::DuckSource`: un `DataSource` sobre el motor DuckDB del informe. El mapa
fisico es el `SourceMap` del modelo con el mismo calificador del lowering, asi que los dos caminos
leen las mismas tablas. La columna se materializa UNA vez y queda en cache: el interprete la visita
por cada filtro de cada celda. `table_len` es un `COUNT(*)`. Los tipos: enteros, decimales,
`DECIMAL(38,s)`, texto, fecha y booleano; lo demas se relee como texto, con la misma limitacion de
`timestamp` que ya tenia el fast path. `DataSource::column` devuelve `Arc`, que es lo unico que
sobrevive al candado del cache.

`run_report` y `monthly_series` se lo pasan a `execute` y `totals`: la app corre medidas
interpretadas contra los datos reales. Sin el, `NoSource`; ese mensaje era el que la app mostraba
siempre que una medida usaba time intelligence.

### Resultado

Tres medidas nuevas en el modelo de prueba: `Variacion` (CALCULATE junto a la hoja pelada),
`QtyDoble` (SUMX) y `Cruda` (ABS, sin forma). Cuatro tests de informe, uno de interprete y dos del
origen. 215 tests. 48 casos de conformidad, 44 por los dos caminos, 0 divergencias. La app arranca
con el origen puesto y su consumo de memoria no cambia.

### Queda abierto

`RANKX` no tiene forma: su valor no sale en la matriz y cae en `NoShape`. El chequeo acepta funciones
de texto y `ABS`, y todas caen en el mismo error nombrado. Los iteradores dentro de un CALCULATE
anidado (`SUMX(VALUES(a), CALCULATE(...))`) conservan el camino del interprete, que ya resolvia ese
grano; no hay caso de conformidad que lo fije.

## Fase UI-6: los cuatro campos del modelo llegan a la pantalla

`datan-model` ya traia `format_string`, `description`, `is_hidden` y `sort_by`, y el documento del
usuario ya los aplicaba al modelo. Nadie los leia: un `sort_by` escrito en el TOML no cambiaba el
orden de la matriz, y un formato de moneda no llegaba a la tarjeta.

### sort_by

El orden de las filas de una dimension es SU columna de orden, no el alfabetico: los meses salen
Enero, Diciembre, Febrero si se ordenan por su nombre. Los dos caminos:

- Interprete: `distinct_values` ordena por la clave de la PRIMERA aparicion en la columna de orden,
  con clave numerica primero y texto despues (el orden alfabetico de claves numericas pone "10"
  antes que "9").
- SQL: `ORDER BY MIN(columna_de_orden)`. No esta en el `GROUP BY`; como es funcionalmente
  dependiente de la columna agrupada, el minimo es EL valor. La tabla de la columna de orden entra
  al FROM por el mismo camino que las de agrupamiento.

La fixture gana `dim_product.region_orden` (Norte=1, Sur=2, Este=3), distinto del alfabetico, para
que el test pueda distinguir quien ordena bien. El test corre el informe por los DOS caminos y
compara el orden.

### format_string

Subconjunto honesto del lenguaje de Power BI y .NET: `#,##0`, `#,##0.00`, `0.0%`, `Currency`,
`C0..C6`, `N0..N6`, `P0..P6`, y patron con simbolo de moneda (`EUR #,##0`). Lo que no se reconoce
deja el numero como esta. La salida es en espanol: punto de miles y coma decimal. Solo formatea lo
que se MUESTRA (el grid y la tarjeta): el texto crudo sigue siendo el que viaja al filtro cuando se
pincha una celda. Una medida sin formato sigue abreviada (`1,3M`), que es lo que cabe en una
tarjeta.

### is_hidden

Una columna oculta no se ofrece en el panel Datos. Es lo que hace que una clave tecnica no estorbe.
Las medidas no tienen el campo.

### description

Se guarda y se aplica al modelo (ya lo hacia `ModelDoc::apply`). Sin UI: gpui-kit no tiene tooltip
sobre un div, y una linea de texto bajo cada campo del panel ensucia la lista. Queda para la
etapa de panel de propiedades.

## Arreglos de la misma pasada

- Una columna `TIMESTAMP` (lo que Postgres llama fecha) servia para nada: `date_column` exigia
  `Date` puro en los dos lados, asi que la base se quedaba sin periodo, sin filtro de fecha y sin
  time intelligence. Ahora `DateTime` tambien vale, y `DuckSource` lee un timestamp como su DIA
  (`div_euclid`, para que un instante anterior a 1970 trunque hacia su dia). El fast path ya
  comparaba bien: DuckDB coerciona fecha contra timestamp.
- La pastilla de una fecha se reconstruia como TEXTO y el filtro nunca coincidia. Ahora se lee el
  dia desde 1970, que es lo que `valor_legible` escribe.
- El pantallazo de error decia "No se pudo conectar" tambien cuando la conexion habia funcionado y
  lo que fallo fue el informe. Hay un estado `Broken` con su titulo: manda a buscar el error al
  lugar correcto.
- `count_text` reusa `formato::con_puntos`: una sola implementacion de los puntos de miles.

### Resultado

221 tests. 48 casos de conformidad, 44 por los dos caminos, 0 divergen. Solo `arrow v58.4.0`. La app
arranca y su consumo de memoria no cambia.

## Exportar a CSV, Excel y PDF

El CSV existia (`1bd01af`): coma, `\\n`, sin BOM, escape RFC 4180, y un boton que escribia
`~/Desktop/datan.csv`, siempre el mismo nombre. Se conservan las decisiones del archivo y se
agregan los dos formatos que faltaban.

- Excel via `rust_xlsxwriter`, la UNICA dependencia nueva (el usuario la pidio al pedir el
  formato). Numeros como NUMEROS: un "74000" de texto no suma ni ordena en el Excel y queda
  marcado con el triangulo verde. Cabecera en negrita, fila congelada, ancho por columna con
  techo. El nombre de hoja se recorta a 31 y suelta `[ ] : * ? / \\`.
- PDF escrito a mano con las fuentes base del formato (Courier y Helvetica, cero embebido y
  cero dependencia): una tabla de informe es texto en columnas. Horizontal a partir de siete
  columnas, paginado con la cabecera repetida, lineas bajo cada renglon, texto recortado al
  ancho de su columna y escape WinAnsi (el euro incluido; lo que no tiene byte cae en `?`).
  El test valida la estructura completa: `%PDF`, `startxref` apuntando de verdad a la tabla
  xref y conteo de paginas.
- El boton unico "CSV" pasa a tres: CSV, Excel y PDF. El archivo se llama
  `<titulo>-<dia>.<ext>` y va a `~/Desktop`; dos exportaciones del mismo dia ya no se pisan, y
  la barra de estado dice donde quedo. El dia sale del reloj convertido con el calendario del
  motor (`format_date`): sin dependencia de tiempo nueva.

Los tres salen de la MISMA matriz: es el DATO de la consulta, no el formato de pantalla. Un
`format_string` de moneda no viaja en el archivo; el que abre el Excel quiere el numero.

Verificado con `file(1)`: `CSV text`, `Microsoft Excel 2007+`, `PDF document, version 1.4`.

### Resultado

220 tests. 48 casos de conformidad, 44 por los dos caminos, 0 divergen. Solo `arrow v58.4.0`;
`rust_xlsxwriter` es el segundo crate nuevo del arbol y no arrastra ningun otro del workspace.

## La columna de periodo cuando la tabla trae timestamp y fecha

`boleta` trae las dos: `created_at` (timestamp de insercion, todo el mismo dia) y `fecha` (la
fecha de negocio, doce meses). `date_column` tomaba la PRIMERA columna de fecha declarada, y la
introspeccion entrega las columnas por orden de declaracion: el periodo caia en el timestamp y la
ventana de doce meses daba un solo dia.

Ahora la columna de la tabla de hechos va primero, y dentro de cada tabla una `Date` pura antes
que un `DateTime`. El timestamp de auditoria sigue sirviendo cuando es lo unico que hay.

Y la decision pendiente de la base: `fecha` SE QUEDA. Es la fecha de negocio, llena al cien por
ciento (cero nulos sobre cien mil filas), repartida entre 2025-09 y 2026-10. Borrarla dejaria el
periodo en manos de `created_at`, que es un solo dia. `ALTER TABLE boleta DROP COLUMN fecha` no se
corre.
