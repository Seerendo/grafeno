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

Each session has its own internal URL. Late requests from another session are
rejected; requests that already captured their router finish on the original
database. Closing and reopening a file invalidates its previous URL. Tests cover
both delayed requests and a write dispatched before switching databases.

## Automated window test on Linux

The test runs the real Tauri binary with WebKitWebDriver on a virtual display,
following the [official Tauri guide](https://v2.tauri.app/develop/tests/webdriver/ci/).
Temporary configuration, data and cache directories isolate the user's settings
and WebView storage. After preparing desktop libraries and building Studio:

```bash
sudo apt-get install -y webkit2gtk-driver xvfb xauth dbus-x11
cargo install tauri-driver --locked
cargo build --locked -p grafeno-desktop
dbus-run-session -- xvfb-run -a -s "-screen 0 1600x1000x24" python3 apps/desktop/tests/ui_smoke.py
```

It checks startup, database creation, saving a node through its form, failed
opening without disconnection, separation between databases, rejection of a
write from the previous session, closing drafts when switching databases, and
reopening through the recent-files list.
File opens use the same event as the menu; native file pickers are neither
mocked nor considered tested.

Screenshots and diagnostics go to `reports/desktop-e2e/` (ignored by Git).
CI retains them for seven days in the `pruebas-escritorio-linux` artifact.
These tests do not replace manual checks on supported operating systems or
validate installers.
