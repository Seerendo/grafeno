# Contribuir a Grafeno

[English](CONTRIBUTING.md) | **Español**

Esta guía prepara el motor, el cliente TypeScript y la aplicación de escritorio
Tauri para desarrollar y verificar cambios. Ejecuta los comandos desde la raíz
del repositorio, salvo que se indique otra ubicación.

## Requisitos

- Rust estable mediante rustup. El workspace declara Rust 1.88 como mínimo;
  CI utiliza la versión estable, no verifica todavía la versión mínima.
- Node.js 24 y npm, como en CI y en la publicación de versiones.
- En Windows: Visual Studio Build Tools con las herramientas de C++ y el
  toolchain Rust MSVC.
- Para Tauri en Linux: bibliotecas de desarrollo de WebKitGTK y GTK.
  En Ubuntu 22.04, utilizado por CI:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev
```

Comprueba las herramientas antes de empezar:

```bash
rustc --version
cargo --version
node --version
npm --version
```

## Preparar las dependencias JavaScript

```bash
npm ci --prefix studio
npm ci --prefix clients/typescript
npm ci --prefix apps/desktop
```

Se conservan los archivos de bloqueo. Usa `npm ci` y los comandos Cargo con
`--locked` para detectar cambios de dependencias inesperados. La primera
compilación de Rust también descarga las dependencias de Cargo.

## Verificar el motor sin el escritorio

Esta ruta permite trabajar en el motor sin instalar WebKitGTK:

```bash
cargo test --workspace --locked --exclude grafeno-desktop
cargo build --locked -p grafeno-server
```

Si tienes poco espacio temporal, en Linux puedes reducir los artefactos de
compilación con esta variante (también utilizada por CI):

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --workspace --locked --exclude grafeno-desktop -j 2
```

Desactiva la información de depuración y la caché incremental; mantiene las
pruebas y las aserciones de depuración. No requiere cambiar `Cargo.toml`.

## Verificar la interfaz y el cliente

```bash
npm run build --prefix studio
npm test --prefix studio
npm run build --prefix clients/typescript
npm test --prefix clients/typescript
```

La compilación de Studio también comprueba los tipos. Las pruebas del cliente
inician el servidor real desde `target/debug/grafeno` (`grafeno.exe` en Windows),
por lo que requieren haber ejecutado la compilación del servidor anterior.
Puedes elegir otro binario mediante la variable `GRAFENO_BIN`.

## Compilar y probar el escritorio

Compila primero Studio: Tauri carga `studio/dist` dentro de su ventana.

```bash
npm run build --prefix studio
cargo test --locked -p grafeno-desktop
cargo build --locked -p grafeno-desktop
```

Para abrir la aplicación en una sesión gráfica:

```bash
cargo run --locked -p grafeno-desktop
```

Para generar los instaladores con el CLI de Tauri:

```bash
cd apps/desktop
npm run build -- --ci
```

El empaquetado requiere herramientas adicionales según el formato y el sistema;
el workflow de publicación contiene la configuración utilizada para los
instaladores. CI de contribuciones compila la aplicación, prueba el puente y
verifica flujos de la ventana en una pantalla virtual de Linux. Guarda capturas
y diagnóstico; no prueba los selectores nativos de archivos ni genera instaladores.

## Comprobaciones de cada contribución

El workflow [Verificación de contribuciones](.github/workflows/ci.yml) se ejecuta
en los pull requests, en los pushes a `main`, y manualmente. Así los pushes a
una rama con un PR abierto no duplican las ejecuciones.
Comprueba por separado:

- Tipos y compilación de Studio y del cliente TypeScript.
- Pruebas Rust del motor y servidor, más el cliente contra el servidor real.
- Pruebas del puente Tauri, compilación y flujos de la ventana en Linux.

No equivale a una revisión visual manual ni valida rendimiento, instaladores
o Windows. Esas comprobaciones siguen requiriendo pruebas específicas. Consulta
la [guía de escritorio](apps/desktop/README.es.md#prueba-automatizada-de-la-ventana-en-linux)
para repetir la prueba de ventana y revisar las capturas.

Trabaja en una rama y abre un PR acotado. Describe el problema, el comportamiento
resultante y las comprobaciones ejecutadas, incluyendo las que no pudiste
realizar. Para estas contribuciones utilizamos español en las explicaciones,
commits y PR; esta guía ofrece una versión equivalente en inglés. Conserva los
nombres técnicos y comandos del proyecto.
