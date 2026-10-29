# Datan contra Power BI: hoja de ruta

Inventario de lo que falta para paridad con Power BI Desktop, ordenado por dependencia. El
estado real de cada pieza se verifico contra el codigo, no contra Power BI.

`PLAN.md` es el registro de fases cerradas. Este archivo es el trabajo que queda.
Las secciones de fase documentan trabajo integrado y brechas pendientes. La auditoria inicial
y las brechas confirmadas describen el estado actual.

## Estado de partida

Hay: paginas, varios visuales por pagina, lienzo con arrastre, ocultar y cambiar tamaño de
visuales, paneles laterales que se pueden ocultar, filtros por visual/pagina/informe,
cross-filter y cross-highlight, bookmarks con renombrar y eliminar, drillthrough, undo/redo,
export CSV/Excel/PDF tambien desde Lectura, y guardar/abrir. Hay atajos de teclado: Cmd+S
guarda, Cmd+Z deshace, Shift+Cmd+Z o Cmd+Y rehacen, Supr o Cmd+Supr borran el visual
seleccionado, Cmd+/Cmd- y Cmd+0 acercan, alejan y restablecen el zoom del lienzo, y Cmd+Q
cierra la app con el mismo flujo del boton de cierre. Los atajos despachan desde el foco raiz
de la ventana: sin foco, GPUI no entrega acciones. Cerrar Datan (boton rojo o Cmd+Q) apaga el
proceso; con cambios sin guardar abre "¿Guardar los cambios?" con Guardar y cerrar, Cerrar sin
guardar y Cancelar, y el guardado pedido desde el dialogo cierra la app al terminar bien. La
app ofrece Tarjetas, Barras, Columnas, Lineas, Area, Anillo, Dispersion, Radar, Embudo, Medidor,
Treemap, Cascada, KPI, Sankey, Candlestick, Combinado, Tabla, Matriz y Segmentador.

Verificado 26 Oct 2026, segunda pasada: el rol "Información sobre herramientas" ya se usa.
Sus medidas entran en la consulta del visual como columnas extra y el tooltip de cada punto
las muestra como "Nombre: valor" (Lineas, Area, Columnas, Barras, Anillo, Combinado,
Candlestick). Los campos de tipo columna del rol se ignoran: cambiar el agrupamiento no es
aceptable. La Tabla ordena por cualquier columna: medidas por numero, dimensiones por texto,
con el renglón en blanco primero. Las celdas de la Matriz tienen tooltip "fila · columna ·
valor", util con celdas recortadas. El ribbon perdio los botones sin accion (Cuadro de texto,
Imagen, Más visuales, Nueva tabla, Medida rápida, Nueva columna) y la pestaña Ver, que no
tenia ninguna accion viva: las líneas de cuadrícula ya viven en Propiedades y la selección de
visuales en la lista Elementos.

Tercera pasada, mismo dia: el panel de propiedades replica la organizacion de Power BI con
los controles de Datan. Pestañas Datos/Formato/Análisis con icono y subrayado activo. Datos
muestra cada rol como "Nombre — N campos", tarjetas de campo con tirador, chevron con menu
"Mover a {rol}" y X, y la zona punteada de drop siempre visible al pie de cada seccion.
Formato organiza "Objeto visual" en grupos plegables (Eje X, Eje Y, Líneas de cuadrícula,
Líneas/Series con swatches de la paleta, Leyenda) y "General" en Texto y tamaños, Fondo y
borde, Serie y Color condicional; el reset vive centrado al pie del panel. Análisis enumera
promedio, minimo, maximo, constante (con su Valor al abrir), tendencia y pronostico en filas
con linea fina. Quedan fuera del panel —hasta que el motor de graficos los soporte— las
unidades de visualizacion, la posicion de la leyenda, la interpolacion, el grosor y los
marcadores: un interruptor que no dibuja no entra.

El marcador de visual vacio ("Faltan campos" / "Sin datos") escala con el tamano de la
tarjeta: 420x300 es el tamano completo, y abajo de eso el icono, los espacios y las letras se
achican; en tarjetas muy bajas caen primero la descripcion y despues el icono, para que nunca
se recorte. Los estados vacios del lienzo entero y de la vista de un solo visual no escalan.

Los visuales del lienzo llenan su tarjeta sin marcos internos, como las tarjetas de Power BI:
el borde, el radio y el fondo que se personalizan en Propiedades son los de la tarjeta
general, no los de un recuadro anidado. Las mini-tarjetas de "Tarjetas" van sin borde propio,
el Combinado sin su caja, la Matriz sin marco alrededor de la tabla, y la linea "N filas · N
filtros visuales" salio del visual. La tarjeta muestra la etiqueta de la medida en su forma
corta ("Recuento de id", sin el calificador de tabla) y el valor; la variacion contra el año
anterior solo aparece con Comparacion encendida en Propiedades y con un periodo contra quien
comparar. El KPI sin objetivo asignado no muestra texto de ayuda: el objetivo se asigna desde
Propiedades.

## Auditoria de vistas y lienzo

El inventario se verifico en `crates/datan-app/src/main.rs` y `crates/datan-app/src/visuales.rs`.
El lienzo ya admite varios visuales, movimiento, cambio de tamaño, orden de capas, zoom, ajuste
   al espacio disponible y dos paneles laterales que se pueden ocultar. La visibilidad de los
paneles se guarda con el informe. El lienzo central crece cuando se oculta un panel. Esta auditoria confirma:

1. La matriz permite repartir las dimensiones entre filas y columnas. Guarda la division por
   visual, conserva el diseño anterior en informes existentes, y alinea celdas y totales con la
   tupla completa de columnas. Falta revisar el resultado en una ventana real.
2. El segmentador ya vive en el lienzo. Su seleccion se guarda en el visual y queda separada de
   los filtros que llegan por interacciones de otros visuales.
3. Combinado ya muestra columnas y línea por categoría, con ejes Y separados.
4. Lectura oculta la barra de edición y los paneles laterales. La cabecera del canvas muestra
   informe, página, hora de la última actualización correcta, periodo y acciones. El periodo
   se aplica a todos los visuales de la página. Los gráficos y segmentadores siguen activos.
   Las pestañas quedan al pie. Falta validar la composición en una ventana real.

### Inventario de visuales

La app tiene 19 tipos. El selector muestra ocho tipos de uso frecuente y deja los otros once
en el panel avanzado, que se abre desde su encabezado. Esto confirma que el tipo existe en el código. No confirma que cada
visual tenga el mismo nivel de formato o que se vea bien en una ventana real.

| Estado | Visuales | Brecha conocida |
| --- | --- | --- |
| Disponibles | Tarjetas, Barras, Columnas, Lineas, Area, Anillo, Dispersion, Radar, Embudo, Medidor, Treemap, Cascada, KPI, Combinado, Tabla, Matriz, Segmentador | Los visuales vacíos muestran un marcador con icono por tipo y una guía para los campos que faltan. Falta revisar su apariencia, tamaños, tooltips y zoom en la app. |
| Disponibles con requisitos | Sankey, Candlestick | Sankey requiere dos dimensiones y una medida. Candlestick requiere cuatro medidas. La app muestra un mensaje cuando faltan campos. |
| Fuera del alcance actual | Mapa | No existe visual de mapa. El alcance del roadmap lo excluye. |

Faltan visuales de mapa, árbol de descomposición, influenciadores clave y preguntas en lenguaje
natural. Estos no son errores del selector: no forman parte del alcance actual. La matriz ya
permite varios niveles en el eje de columnas y conserva su división por visual.

### Canvas y paneles laterales

Los botones para mostrar u ocultar Datos y Visualizaciones están en la barra secundaria, que sigue
visible aunque se oculte cualquiera de los paneles. La visibilidad se guarda en el informe. El
canvas calcula su ancho disponible con los paneles visibles y deja más área libre al ocultarlos.
El canvas y "Ajustar" usan el mismo cálculo del ancho disponible. Una prueba cubre ambos paneles,
cada panel por separado, ambos ocultos y un viewport demasiado angosto. Ocultar un panel no activa
"Ajustar" por sí solo. Las pestañas de página se desplazan en horizontal y recortan nombres
largos. La tira ocupa el ancho flexible y el estado del lienzo se recorta a 220 px para
reservar espacio a las pestañas. El botón de detalle conserva su ancho. El botón "+" queda
fuera del área con desplazamiento, siempre visible durante la edición.
La tira lleva la página activa al área visible al cambiar, crear, borrar, mover o abrir páginas.
El clic derecho sobre una pestaña abre el menú de página con cambiar nombre, duplicar, mover y
eliminar, según el patrón de páginas de Power BI. Falta validar el comportamiento en una ventana
real con muchas páginas y poco ancho. Una captura en vivo confirma que el clic derecho abre el
menú con Cambiar nombre, Duplicar página, Mover y Eliminar. Esta revisión usó una sola página.
La tira usa desplazamiento horizontal y lleva la página activa a la vista. Las pruebas del
binario pasan. Falta probar muchas páginas en una ventana estrecha dentro de la app.
Una captura a 1.173 × 800 px mostró que la barra de acciones recortaba Zoom, Abrir, Guardar y
Exportar. La barra ahora ocupa el ancho disponible y envuelve sus controles. Falta verificar el
resultado en la app. La compilación actual y las 156 pruebas del binario pasan. El binario actual
abre el selector de conexión. La vista de un informe queda pendiente de revisión visual.
En edición, una página sin visuales indica cómo añadir el primero y muestra el panel
Visualizaciones si está oculto. En Lectura, el canvas informa que no hay visuales sin sugerir
acciones de edición. Si todos los visuales están ocultos, el canvas explica cómo volver a mostrarlos.

El panel Datos muestra una búsqueda para tablas y campos, tablas plegables con conteos de filas,
medidas debajo de las tablas y el botón "Obtener datos" fijo al pie. Las relaciones se editan en la
vista Modelo, junto al diagrama, no en el panel Datos. "Conectar dos columnas" activa la selección
de campos en las tarjetas del modelo. Las tarjetas muestran filas, tipos de campo, claves y
columnas relacionadas. Al conectar, el diagrama muestra todas las columnas. El panel
Visualizaciones mantiene el selector arriba, muestra un estado vacío sin visual seleccionado y un
inspector con título, ancho, ejes y valores al seleccionar uno. Si faltan campos, el inspector indica
que se agregan al marcar campos en Datos. KPI, Combinado, Dispersión y Candlestick muestran el orden
de sus medidas y permiten cambiarlos con flechas.

### Barra superior

La barra comparte marca, vistas Informe y Datos, ruta y controles globales. Datos abre el diagrama
relacional. El panel Datos muestra campos y medidas dentro de Informe. El estado de guardado,
historial y modo de lectura solo aparecen en Informe. Los controles para ocultar los paneles quedan
en la barra secundaria. Compartir y Publicar siguen deshabilitados y no tienen acción conectada. La
captura confirma su ubicación, no su funcionamiento. Una captura de escritorio confirmó el diseño
en Informe. En Lectura no se muestra la barra de edición: el encabezado de página, periodo,
actualizar y Editar quedan dentro del canvas. La vista Datos aún requiere una comprobación visual.

Una captura en vivo confirmó la vista Informe en edición: barra superior, panel Datos, selector,
inspector con un visual seleccionado, lienzo y pestañas. La captura no verifica las acciones.
Lectura oculta la barra de edición, los editores de renombre, zoom, abrir, guardar y exportar,
según el código y sus pruebas. Falta revisar Lectura y los 19 tipos en una ventana real. También
falta revisar "Ajustar" al alternar los paneles, el recorte de etiquetas y tooltips en lienzos
estrechos, además de los mensajes de requisitos de Sankey y Candlestick.
La vista Datos perfila hasta 1.000 filas con recuento, distintos, únicos, vacíos y, para columnas
numéricas, mínimo, máximo, media y distribución. Los pasos "Sin vacíos", ordenar ascendente o
descendente y quitar filas duplicadas se guardan por conexión, se pueden quitar antes de aplicar
y afectan tanto la vista previa como los informes. "Tipo de datos" convierte a número, texto o
fecha, actualiza el tipo del modelo y usa `TRY_CAST`; los valores que no convierten quedan vacíos.
Faltan renombrar o quitar columnas.

### Estado de uso confirmado

1. **Filtros en el lienzo.** El segmentador admite lista, desplegable o rango de fechas,
   conserva la seleccion y aplica el filtro a la pagina.
2. **Composicion del lienzo.** Se puede mover, redimensionar, cambiar el orden de capas, hacer
   zoom y ajustar el contenido. El zoom escala texto, espacios, tablas, listas de segmentadores,
   dibujos propios y controles del selector de fechas. El selector limita su escala de 0,75 a 1,5
   para mantener un tamaño de clic útil.
3. **Espacio de trabajo.** Los dos paneles laterales se pueden ocultar desde la barra. El lienzo
   central usa el ancho disponible y sigue el alto de los visuales visibles, con un minimo de
   480 px.
4. **Comparacion de series.** Combinado asigna la primera medida a columnas y la segunda a
   línea. Usa dos ejes Y y conserva el orden del informe. Propiedades permite elegir la paleta.
   Los otros visuales de serie ofrecen leyenda y cinco colores para Columnas, Lineas, Area y
   Radar. El color cambia una serie completa, no cada categoria.
5. **Formato de visuales.** Propiedades permite cambiar títulos, subtítulos, familia y tamaño
   de fuente, colores de texto y serie, fondo, borde y radio. El color condicional puede usar
   otra medida y funciona en KPI y marcas compatibles. La regla KPI tiene dos resultados,
   favorable y desfavorable. El color por medida admite reglas por rangos (`<`, `Entre`, `≥`)
   con presets de color, reordenar y quitar.
6. **Detalle visual.** Los tooltips incluyen las medidas de la fila. Las lineas de referencia,
   tendencia y pronostico tienen controles en Propiedades. Dispersión muestra ambas medidas en
   el tooltip de cada punto, filtra por la categoría completa al hacer clic y respeta el
   cross-highlight. Barras, Anillo, Combinado y las leyendas de Lineas, Area, Columnas, Radar,
   Embudo, Treemap, Cascada, Sankey y Candlestick filtran por categoría. Embudo, Treemap,
   Cascada, Sankey y Candlestick también filtran al hacer clic directo en sus marcas. Sankey
   filtra la dimensión del nodo. Candlestick conserva la clave completa aunque descarte filas
   con OHLC inválido. Las marcas de Columnas, Lineas, Area y Radar filtran al hacer clic directo.
   Area acepta clics dentro de su zona rellena. Las barras finas tienen un objetivo de clic
   mínimo sin cambiar el dibujo. Los hit tests de Area y barras finas tienen pruebas unitarias.
   Falta verificar estos gestos en la app. Medidor permite asignar Valor, Mínimo, Máximo y
   Objetivo desde Propiedades. Sin límites asignados, calcula un rango automático desde cero
   e incluye el cero para valores negativos. Si los límites no forman un rango válido, usa el
   rango automático. Una prueba confirma que los cuatro campos asignados llegan al medidor.

### Prioridad de pulido

1. **Hecho.** El zoom escala las etiquetas y los espacios de los ejes en Lineas, Area, Barras,
   Columnas y Candlestick. También escala las etiquetas de Radar, los dibujos propios de Datan y
   el campo y calendario del selector de fechas. El selector limita su escala entre 0,75 y 1,5.
2. **Hecho.** Las opciones de cada segmentador respetan los filtros de informe, pagina,
   visual, otros segmentadores y rangos de fecha. Cada segmentador excluye su propia seleccion.
3. Revisar tooltips y analitica durante la prueba visual de cada tipo de grafico. El formato de
   moneda y porcentaje ya llega a los tooltips y ejes de Barras, Columnas, Lineas, Area,
   Combinado y a los ejes de Dispersión. Dispersión también tiene tooltip por punto, filtro por
   categoría y cross-highlight. Falta validar estos datos en la app y revisar los tooltips de
   cada visual en una ventana real.
4. **Hecho.** La Matriz admite varios niveles en el eje de columnas y alinea las celdas y los
   totales con la tupla completa de columnas.

Cada segmentador guarda su seleccion por separado. Las selecciones visibles se aplican a los
demas visuales junto con los filtros de pagina. Limpiar un segmentador conserva los filtros de pagina
y los demas segmentadores. Los informes anteriores migran su seleccion guardada. Las opciones
responden a los otros filtros y la lista muestra 50 valores por pagina.

**Estado de rendimiento:** filtros concretos de igualdad o `IN` sobre columnas de la misma
tabla se aplican en DuckDB antes de paginar los valores distintos. La consulta conserva el
orden y la página solicitada. Los filtros entre tablas, propagados, de rango o con valores que
el camino SQL no admite usan el intérprete. En esos casos, un campo de alta cardinalidad puede
aumentar la latencia y el uso de memoria. Las pruebas cubren el camino SQL y el fallback.

La versión local de `gpui-component` ahora expone tamaño de texto para los ejes de Lineas, Area,
Barras, Columnas y Candlestick. Radar y las etiquetas del anillo también escalan con el zoom.
Combinado y Sankey escalan sus etiquetas desde Datan. Barras y Columnas usan orientaciones
distintas. El tamaño base de GPUI Component es 10 px. El selector de fechas escala el campo y
calendario entre 0,75 y 1,5. Falta validar estos cambios con una pasada visual en la app.

El mapa queda fuera del alcance acordado. Candlestick y Sankey ya existen, pero necesitan cuatro
medidas y dos dimensiones mas una medida, respectivamente. La Matriz calcula los totales por
categoria de columna con una consulta agrupada por esa dimension. Admite varios niveles en el eje
de columnas y alinea los totales con la tupla completa. Su escala de color se puede activar desde
Propiedades.

Los paneles se pueden ocultar y su visibilidad se guarda con el informe. Los informes viejos
abren con ambos paneles visibles.
Cada visual tiene un tirador abajo a la derecha para cambiar ancho y alto. Los botones del panel
siguen disponibles. Candlestick requiere cuatro medidas y Sankey requiere dos dimensiones y una
medida.

## Regla de orden

Un filtro con alcance no significa nada mientras haya una pagina y un visual: "informe",
"pagina" y "visual" filtrarian lo mismo. Por eso el alcance llega con las paginas y el
multi-visual, no antes. Construir el panel de alcance primero seria un interruptor decorativo.

Las fases D y E no dependen de nada y se pueden adelantar si conviene mostrar avance.

## Fase A (hecha): filtros sin pinchar

**Problema.** Hoy un filtro nace de un clic en un grafico. No se puede filtrar un campo que no
esta dibujado, ni ver los valores de una columna, ni quitar un valor sin quitar el filtro
entero.

**Que.**

1. `Report::distinct_values(engine, column, limit)`: los valores distintos de una columna, en
   el orden del SQL, con el renglon en blanco al final. Es la unica pieza de motor de la fase.
2. Panel "Filtros" en la barra derecha, con los filtros activos y su boton de quitar.
3. Alta de filtro: "Agregar filtro" arma un modo de un clic sobre el panel de campos; al elegir
   la columna se leen sus valores y se eligen con casillas. Tildar varios es una union (`IN`).
4. El estado intermedio se dice: mientras carga, "leyendo valores"; si no hay, un aviso.

**Hecho cuando.** Se puede filtrar `categoria` sin tocar el grafico, con dos valores tildados,
y la pastilla de filtro se quita desde el panel. Test del helper en `datan-report`; la UI se
verifica por compilacion y arranque (la captura de pantalla no funciona en esta maquina).

## Fase B (hecha): paginas

**Problema.** Una sola pagina. El "+" esta dibujado y apagado. Un informe con secciones
(Resumen, Detalle, Producto) no se puede armar.

**Que.**

1. `Pagina { nombre, vista }`, donde `Vista` es la que ya usa el historial (seleccion, filtros,
   visual, orden, Top N, rango, formatos). Una pagina ES una vista guardada.
2. Pestañas de pagina con agregar, renombrar, borrar y reordenar. Borrar la ultima no se puede.
3. Cambiar de pagina guarda la actual y carga la destino, con refresh. El historial es por
   pagina: deshacer no cruza de pagina.
4. Filtros de INFORME aparte de los de pagina. El informe se aplica encima de cada pagina.
5. El archivo guardado gana `[[pagina]]`. Un archivo viejo (sin paginas) abre como una pagina.

**Hecho cuando.** Dos paginas con selecciones distintas se guardan, se reabren y se alternan sin
perder nada; un filtro de informe recorta las dos.

## Fase C (hecha): mas de un visual por pagina

**Problema.** El limite estructural: un visual por pagina. Bloquea el alcance por visual, el
segmentador en el lienzo, los tooltips, el panel de seleccion y los bookmarks.

**Que.**

1. `Pagina { visuales: Vec<Visual> }` con posicion y tamaño (x, y, ancho, alto) en una grilla.
2. Cada visual tiene su propia seleccion, filtros, Top N, orden y formato. La matriz se corre
   por visual: una consulta por conjunto de atomos, agrupadas como ya hace `Report::plans`.
3. Lienzo con arrastrar y redimensionar. Sin grilla imantada en la primera version.
4. Alcance de filtro completo: visual, pagina, informe.
5. Panel de seleccion (elegir, mostrar, ocultar, reordenar, redimensionar y borrar visuales) y
   bookmarks (guardar una vista de la pagina).

**Hecho cuando.** Una pagina con un grafico y una tabla, con un filtro que toca solo el
grafico, se guarda y se reabre igual.

## Fase D (hecha): vista Modelo

**Problema.** Las relaciones solo se ven como una lista. No hay diagrama, que es la vista que
mas identifica a Power BI.

**Que.** Pantalla con las tablas como cajas (nombre, columnas clave, marca de tabla de hechos),
las relaciones como lineas con cardinalidad y flecha de direccion, arrastre para mover, y el
clic sobre una relacion abre rol, direccion y cardinalidad. Es dibujo puro del modelo: no
consulta datos.

**Hecho cuando.** El diagrama de `datan_bench` se ve con sus dos relaciones y sus cardinalidades
reales.

## Fase E (hecha): propiedades de campo y relaciones

**Problema.** `is_hidden`, `sort_by`, `format_string` y `description` existen en el modelo y en
el TOML, sin UI. Las relaciones solo dejan tocar el rol desde la app: `direction` y
cardinalidad viven en el TOML, y no hay forma de borrar una relacion.

**Que.**

1. Propiedades de columna en el panel, al elegir un campo: oculta, ordenar por, formato,
   descripcion.
2. En Relaciones: direccion, cardinalidad y borrar, con las mismas validaciones de P8.

**Hecho cuando.** Todo lo que hoy se escribe a mano en `~/.datan/models/<db>.toml` se puede
hacer desde la app.

Verificado 26 Oct 2026: el panel de propiedades de columna estaba implementado pero nunca se
invocaba. Ahora vive al pie del panel Datos: clic en un campo abre "Propiedades · tabla[campo]"
con Ocultar/Mostrar, Ordenar por, Formato, Descripcion y un boton de cierre. Un campo oculto se
puede volver a mostrar desde ahi, que era imposible antes. El gesto "Conectar dos columnas" se
cancela al salir de la vista Modelo o al entrar a Lectura, que dejaba "Restablecer filtros" y
"Agregar filtro" apagados sin explicacion.

## Fase F: visuales

**Hecho.** La app ofrece Tarjetas, Barras, Columnas, Lineas, Area, Anillo, Dispersion, Radar,
Embudo, Medidor, Treemap, Cascada, KPI, Sankey, Candlestick, Combinado, Tabla, Matriz y
Segmentador. Los visuales personalizados tienen pruebas con matrices conocidas.

**Hecho.** Combinado usa la primera medida para columnas y la segunda para linea. Las series
comparten categorias y usan ejes Y separados.

**Hecho.** Matriz ofrece totales por categoria de columna y escala de color condicional. Los
totales se calculan agrupando la medida por la dimension del eje, con los filtros activos.

**Hecho.** Propiedades permite elegir la paleta de Combinado, y leyenda y color de serie en
Columnas, Lineas, Area y Radar.

## Fase G (hecha): interaccion

**Que.**

1. Tooltips al pasar el mouse: valor formateado y las demas medidas de la fila.
2. Drillthrough: pagina destino que se filtra con el valor pinchado.
3. Cross-highlight: resaltar en vez de filtrar, con "Editar interacciones" por par.
4. Analitica: linea de referencia, linea de tendencia, pronostico.

**Hecho cuando.** Un clic en un grafico resalta el otro visual de la pagina sin cambiar sus
totales, y una pagina de detalle abre filtrada.

## Fase H (hecha): medidas

**Que.** Editar y borrar una medida desde la barra DAX (hoy solo crea), medallas rapidas
(promedio, variacion, % del total), carpetas de medidas y tabla de medidas.

**Hecho cuando.** Una medida creada por error se puede corregir sin editar el TOML.

## Fuera de alcance

Decidido y no se hace: refresh programado e incremental, RLS, workspaces, publicar, gateway,
suscripciones, alertas, Q&A, layout movil y visuales de mapa. La paridad completa con Power
Query queda fuera de alcance. Datan lee Postgres con DuckDB y aplica los pasos compatibles
mediante vistas locales de DuckDB.

## Orden de trabajo

A, B, C, D, E, F, G, H. D y E se pueden adelantar porque no dependen de A, B ni C.
