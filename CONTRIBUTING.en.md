# Contributing to Grafeno

[Español](CONTRIBUTING.md) | **English**

This guide prepares the engine, TypeScript client and Tauri desktop application
for development and verification. Run commands from the repository root unless
a different directory is specified.

## Requirements

- Stable Rust through rustup. The workspace declares Rust 1.88 as its minimum;
  CI uses stable Rust and does not yet verify the minimum supported version.
- Node.js 24 and npm, matching CI and release builds.
- On Windows: Visual Studio Build Tools with the C++ tools and the MSVC Rust
  toolchain.
- For Tauri on Linux: WebKitGTK and GTK development libraries.
  On Ubuntu 22.04, used by CI:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev
```

Check the tools before starting:

```bash
rustc --version
cargo --version
node --version
npm --version
```

## Prepare JavaScript dependencies

```bash
npm ci --prefix studio
npm ci --prefix clients/typescript
npm ci --prefix apps/desktop
```

Keep the lockfiles. Use `npm ci` and Cargo commands with `--locked` to detect
unexpected dependency changes. The first Rust build also downloads Cargo
dependencies.

## Check the engine without the desktop application

This path supports engine development without installing WebKitGTK:

```bash
cargo test --workspace --locked --exclude grafeno-desktop
cargo build --locked -p grafeno-server
```

## Check the interface and client

```bash
npm run build --prefix studio
npm run build --prefix clients/typescript
npm test --prefix clients/typescript
```

The Studio build also checks types. Client tests start the real server from
`target/debug/grafeno` (`grafeno.exe` on Windows), so they require the server
build above. Set `GRAFENO_BIN` to use a different binary.

## Build and test the desktop application

Build Studio first: Tauri loads `studio/dist` inside its window.

```bash
npm run build --prefix studio
cargo test --locked -p grafeno-desktop
cargo build --locked -p grafeno-desktop
```

To open the application in a graphical session:

```bash
cargo run --locked -p grafeno-desktop
```

To generate installers with the Tauri CLI:

```bash
cd apps/desktop
npm run build -- --ci
```

Packaging requires additional tools depending on the format and operating
system; the release workflow contains the configuration used for installers.
Contribution CI builds the application and runs its bridge tests, but does not
open the window, automate clicks or generate installers.

## Contribution checks

The [Verificación de contribuciones](.github/workflows/ci.yml) workflow runs
on pull requests, pushes to `main` and `f-slg/**`, and manual dispatch.
It checks separately:

- Studio and TypeScript client types and builds.
- Rust engine and server tests, plus the client against the real server.
- Tauri bridge tests and the desktop build on Linux.

These checks do not validate the visual interface, performance, installers or
Windows. Those still require specific tests.

Work on a branch and open a focused PR. Describe the problem, resulting behavior
and checks performed, including any you could not run. For these contributions,
explanations, commits and PRs use Spanish; this guide provides an equivalent
English version. Preserve the project's technical names and commands.
