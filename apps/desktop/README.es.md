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

Prepara los [requisitos de desarrollo](../../CONTRIBUTING.md#requisitos).
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
con el CLI de Tauri; consulta la [guía de contribución](../../CONTRIBUTING.md).

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
