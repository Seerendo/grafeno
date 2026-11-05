# Grafeno desktop

[Español](README.es.md) | **English**

Grafeno as a desktop application: Grafeno Studio in a native window, built
with [Tauri](https://tauri.app). It works two ways:

- **On this computer**: open or create a database file (`.redb`). The
  engine runs inside the app; the Studio's API calls reach it in the same
  process, so no service is needed and no port is opened. A file can be
  open in one program at a time: close it in the app before a `grafeno
  serve` or `grafeno shell` uses it, and the other way around.
- **A Grafeno server**: the installed service, or one on another machine,
  with its URL and token. The app sends the requests itself, so the server
  needs no CORS setting for it.

The start screen lists recent databases. The File menu creates, opens and
closes databases, connects to a server, and imports and exports JSON
Lines with save and open dialogs; the View menu switches views
(Ctrl+1 to Ctrl+7) and themes. Launching the app with a database path
(`grafeno-desktop notes.redb`) opens it; launching it again while it runs
brings the window forward and opens the new path there. The window keeps
its size and position.

## Building

The window shows the Studio's build, so build it first:

```bash
cd studio && npm ci && npm run build
```

```bash
cargo run -p grafeno-desktop
```

```bash
cargo run -p grafeno-desktop -- path/to/notes.redb
```

On Windows the window uses WebView2, part of Windows 10 and 11. On Linux,
Tauri needs WebKitGTK and its development files, for example on Debian or
Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev
```

Installers (`.exe`, `.msi`, `.deb`, `.rpm`, `.AppImage`) are built with the
Tauri bundler (phase 9.5).

## How it works

- `src/main.rs`: the window, the menu and the commands the Studio calls
  (`session`, `open_database`, `close_database`, `recent`,
  `pick_database`, `pick_save`, `pick_open`, `read_text`, `write_text`,
  `open_external`, `request`).
- `request` is the Studio's `fetch`: for `http://grafeno.local/…` it calls
  the open database's HTTP router (`grafeno_server::http`) in-process; for
  other addresses it sends the request from the app.
- The Studio notices the app (`window.__TAURI__`, see
  `studio/src/desktop.ts`): it shows the start screen instead of the
  connect screen, saves files with dialogs, opens web links in the system
  browser and follows the menu.
- Recent databases are kept in `recent.json` in the app's configuration
  folder (`%APPDATA%\dev.grafeno.desktop` on Windows,
  `~/.config/dev.grafeno.desktop` on Linux).

## Local database sessions

Opening another file only replaces the current session after the new database
and its router are ready. If opening fails, the current database remains usable
and the interface reports the error without disconnecting. Reopening the same
canonical path (including a symbolic link) reuses its session and owner token.

`src/local_database.rs` keeps this lifecycle separate from the window. Its tests
cover persistence after closing, switching between files, equivalent paths,
invalid and truncated files, missing directories, Unix file permissions and a lock held by a
child process:

```bash
cargo test --locked -p grafeno-desktop local_database::tests
```

The ignored helper test is executed by the lock test in a child process. Unix
permission checks require an unprivileged user; the test reports when its user
can bypass read-only permissions. These tests do not launch the window and do
not replace a full checksum scan of a potentially damaged database.
