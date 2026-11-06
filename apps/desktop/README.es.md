# Grafeno Desktop

**Español** | [English](README.md)

Grafeno Studio en una ventana nativa con [Tauri](https://tauri.app).
Permite trabajar de dos formas:

- **Base local:** crea o abre un archivo `.redb` con el motor dentro de la
  aplicación. No necesita un servicio ni abre un puerto.
- **Servidor Grafeno:** conecta con una instancia local o remota mediante su
  dirección y token. Las solicitudes salen desde la aplicación y no requieren
  configurar CORS para la ventana.

El inicio muestra las bases recientes. El menú permite crear, abrir y cerrar
bases, conectar a un servidor e importar o exportar información. Un archivo
solo puede estar abierto en un proceso a la vez.

## Compilar y ejecutar

Prepara los [requisitos de desarrollo](../../CONTRIBUTING.es.md#requisitos).
Desde la raíz del repositorio:

```bash
npm ci --prefix studio
npm run build --prefix studio
cargo run --locked -p grafeno-desktop
```

Para abrir un archivo al iniciar:

```bash
cargo run --locked -p grafeno-desktop -- "mis datos/memoria.redb"
```

La interfaz compilada se carga desde `studio/dist`. Los instaladores se generan
con el CLI de Tauri; consulta la [guía de contribución](../../CONTRIBUTING.es.md).

## Comportamiento de las sesiones locales

- Una nueva apertura reemplaza la sesión actual cuando la base y su router
  están preparados.
- Si falla, la base anterior sigue disponible y la interfaz muestra el error
  sin desconectarla.
- Reabrir la misma ruta canónica reutiliza la sesión y su token propietario.
  Esto también reconoce rutas equivalentes y enlaces simbólicos.
- Cerrar una sesión libera sus referencias al archivo. Las solicitudes que ya
  se están ejecutando pueden conservar referencias hasta terminar.
- Las rutas recientes y el título solo se actualizan tras una apertura exitosa.
- Cada sesión tiene una dirección interna propia. Las solicitudes tardías de
  otra sesión se rechazan; las que ya capturaron su router terminan sobre la
  base original. Cerrar y reabrir un archivo invalida su dirección anterior.

## Pruebas

El módulo `src/local_database.rs` separa las sesiones de la ventana. Comprueba
persistencia tras cerrar, cambio entre archivos, rutas con espacios y Unicode,
reaperturas, archivos inválidos o truncados, directorios ausentes, permisos Unix y bloqueo
por un proceso hijo:

```bash
cargo test --locked -p grafeno-desktop local_database::tests
```

La prueba auxiliar marcada como ignorada se ejecuta desde la prueba de bloqueo.
Las comprobaciones de permisos necesitan un usuario sin privilegios: la prueba
informa cuando su usuario puede escribir pese a un permiso de solo lectura.

Las pruebas incluyen solicitudes tardías y una escritura despachada antes de
cambiar de base. No abren la ventana ni sustituyen la comprobación completa de
checksums de una base potencialmente dañada. La interacción visual necesita
validaciones adicionales.

## Organización

- `src/main.rs`: ventana, menús, comandos Tauri y solicitudes locales o remotas.
- `src/local_database.rs`: creación del router y reemplazo de la sesión local.
- `../../studio/src/desktop.ts`: comunicación de la interfaz con los comandos.
- `recent.json`: historial en el directorio de configuración de la aplicación
  (`~/.config/dev.grafeno.desktop` en Linux, `%APPDATA%\\dev.grafeno.desktop` en Windows).

## Prueba automatizada de la ventana en Linux

La prueba ejecuta el binario Tauri real con WebKitWebDriver en una pantalla
virtual, siguiendo la [guía oficial de Tauri](https://v2.tauri.app/develop/tests/webdriver/ci/).
No modifica la configuración del usuario: utiliza directorios temporales para
preferencias, historial y datos del WebView.

Después de preparar las bibliotecas de escritorio y compilar Studio:

```bash
sudo apt-get install -y webkit2gtk-driver xvfb xauth dbus-x11
cargo install tauri-driver --locked
cargo build --locked -p grafeno-desktop
dbus-run-session -- xvfb-run -a -s "-screen 0 1600x1000x24" python3 apps/desktop/tests/ui_smoke.py
```

Comprueba la pantalla inicial, creación de una base, guardado de un nodo desde
su formulario, error de apertura sin desconexión, separación entre bases,
rechazo de una escritura de la sesión anterior, cierre de borradores al cambiar
de base y reapertura desde recientes.
Las aperturas se solicitan mediante el evento que usa el menú; no se simulan
ni se consideran probados los selectores nativos de archivos.

Las capturas y el diagnóstico se escriben en `reports/desktop-e2e/` (ignorado
por Git) y CI los conserva durante siete días como artefacto
`pruebas-escritorio-linux`. La prueba no sustituye la revisión manual en los
sistemas operativos soportados ni verifica instaladores.

El cliente de pruebas mantiene una conexión HTTP persistente. Reintenta de
forma acotada solo lecturas si el controlador corta la conexión; no repite
clics, escritura de texto ni órdenes que puedan modificar datos. Sus cinco
pruebas de transporte pueden ejecutarse sin Tauri:

```bash
python3 -m unittest discover -s apps/desktop/tests -p 'test_*.py'
```

## Copias de seguridad y recuperación

Para crear una copia local, entra en **Administration → Back up the database**
y elige un nombre nuevo. La copia incluye todos los namespaces y las claves de
acceso, y usa una instantánea consistente mientras continúan las escrituras.
Si cambias de base durante el diálogo, la operación se rechaza en lugar de
copiar otra base.

Para recuperar, cierra la base y pulsa **Recuperar una copia…** en el inicio.
Selecciona un respaldo terminado y un destino nuevo. La aplicación copia el
respaldo a un temporal propio, verifica checksums y comprueba la integridad de
cada namespace antes de publicar el archivo. Después abre la base recuperada.
La copia original se lee sin modificarla; una validación fallida no cambia la
sesión. Si la apertura posterior falla, el archivo recuperado permanece disponible.

Ni copiar ni recuperar sobrescribe archivos existentes, directorios o enlaces,
incluso si el diálogo del sistema ofrece reemplazarlos: elige otro nombre.
Los temporales tienen nombres únicos junto al destino y se limpian ante errores
normales. No se eliminan antiguos archivos `.partial`. Una interrupción del
proceso o del sistema puede dejar un temporal que debe revisarse por separado.
Utiliza respaldos terminados, no archivos de bases que otro proceso esté escribiendo.
Si la verificación informa que necesitó reparar la copia, se rechaza su publicación.

En Rust, `Grafeno::restore_backup(source, destination)` realiza la misma
recuperación sin abrir ni reemplazar una sesión. La API de respaldo del motor
conserva su política de no sobrescribir; ahora también evita colisiones entre
temporales y protege destinos creados durante la copia.

```bash
cargo test --locked -p grafeno --test backup
cargo test --locked -p grafeno --lib backup::tests
cargo test --locked -p grafeno-desktop local_database::tests
```

La prueba de ventana también crea una copia, rechaza destinos existentes y una
sesión caducada, intenta recuperar un archivo inválido y abre una recuperación
válida conservando los datos. Los selectores nativos siguen pendientes de prueba
manual.

## Contrato del puente HTTP

`src/bridge.rs` contiene el transporte local y remoto sin dependencia de Tauri.
`studio/src/desktop-fetch.ts` adapta las solicitudes de Studio a ese contrato.
El texto UTF-8 viaja en `body`; las respuestas con otros bytes incluyen
`bodyBase64` y un `body` vacío. En las solicitudes se admite `body` o
`bodyBase64`, nunca ambos. Así se conservan respaldos binarios sin convertir
bytes inválidos en caracteres de reemplazo. Las llamadas textuales existentes
siguen siendo compatibles.

El adaptador normaliza `Request`, URL y `RequestInit`, conservando método,
cabeceras y bytes del cuerpo. Mantiene los estados HTTP —incluidos los errores—
y deja sin cuerpo las respuestas HEAD, 204, 205 y 304.

`AbortSignal` permite dejar de esperar y evita el envío si se cancela antes.
Una orden ya enviada puede continuar y confirmar cambios: cancelar no revierte
una escritura ni detiene una consulta del motor. El adaptador no vuelve a
enviar automáticamente una orden fallida. Los cuerpos se almacenan completos
en memoria; esta entrega no implementa streaming ni un reemplazo general de
todas las opciones de `fetch` del navegador.

```bash
npm test --prefix studio
cargo test --locked -p grafeno-desktop bridge::tests
```

Las pruebas verifican texto Unicode, bytes de 0 a 255, transporte remoto real
por HTTP, estados de error, objetos Request, cancelación y el formato JSON del
contrato. La prueba de ventana obtiene un respaldo binario por el comando
Tauri, lo recupera y comprueba los datos en la base resultante.

La API de [lecturas por lotes](../../docs/lecturas-por-lotes.md) también funciona
por el puente local. La prueba de ventana comprueba nodos, aristas, duplicados
y ausentes en una llamada `read-many`.

La [expansión visual](../../docs/expansion-grafo.md) agrupa ocho vecindarios por
solicitud y descarta cargas antiguas. La prueba nativa cuenta las llamadas al
abrir 18 raíces, aplica una fecha y verifica que limpiar no repueble el lienzo
con respuestas pendientes, incluida la lista inicial de “Show all”.
