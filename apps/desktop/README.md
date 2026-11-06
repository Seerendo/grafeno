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

The test client keeps a persistent HTTP connection. It retries only reads,
with a fixed limit, when the driver closes the connection; it never retries
clicks, typing or commands that may modify data. Its five transport tests run
without Tauri:

```bash
python3 -m unittest discover -s apps/desktop/tests -p 'test_*.py'
```

## Backups and recovery

To create a local backup, open **Administration → Back up the database** and
choose a new filename. It includes every namespace and access key, using a
consistent snapshot while writes continue. If you switch databases while the
save dialog is open, the operation fails instead of backing up another database.

To recover, close the database and choose **Recuperar una copia…** on the start
screen. Select a completed backup and a new destination. The app copies it to
its own temporary file, verifies checksums and checks every namespace before
publishing the recovered database. It then opens that database. The original
backup is read without modification; failed validation leaves the session alone.
If the subsequent open fails, the recovered file remains available.

Backup and recovery never replace existing files, directories or links, even
if the system dialog offers to replace them: choose another name. Each operation
owns a unique temporary file beside its destination and cleans it up on ordinary
errors. Existing `.partial` files are left alone. A process or system interruption
may leave a temporary file requiring separate inspection. Use completed backups,
not live database files being written by another process. If verification reports
that it repaired the copy, publication is rejected.

In Rust, `Grafeno::restore_backup(source, destination)` provides the same recovery
without opening or replacing a session. The engine backup API retains its
no-overwrite policy and now protects against temporary-name collisions and
destinations created while the backup is being written.

```bash
cargo test --locked -p grafeno --test backup
cargo test --locked -p grafeno --lib backup::tests
cargo test --locked -p grafeno-desktop local_database::tests
```

The window test also creates a backup, refuses existing destinations and an
expired session, rejects an invalid recovery and opens a valid recovered database
with its data intact. Native file dialogs still need manual testing.

## HTTP bridge contract

`src/bridge.rs` contains local and remote transport without a Tauri dependency.
`studio/src/desktop-fetch.ts` adapts Studio requests to that contract. UTF-8 text
travels in `body`; replies containing other bytes include `bodyBase64` and an
empty `body`. Requests accept either `body` or `bodyBase64`, never both. This
preserves binary backups without replacing invalid UTF-8 bytes with characters.
Existing text callers remain compatible.

The adapter normalizes `Request`, URL and `RequestInit`, preserving the method,
headers and body bytes. HTTP statuses, including errors, remain responses;
HEAD, 204, 205 and 304 have no response body.

`AbortSignal` stops waiting and prevents dispatch when aborted beforehand. An
already dispatched command may continue and commit: aborting neither rolls back
a write nor stops an engine query. The adapter does not automatically resend a
failed command. Bodies are fully buffered in memory; this change does not add
streaming or implement every browser `fetch` option.

```bash
npm test --prefix studio
cargo test --locked -p grafeno-desktop bridge::tests
```

Tests cover Unicode, byte values 0 through 255, real remote HTTP transport,
error statuses, Request objects, cancellation and the JSON wire format. The
window test obtains a binary backup through the Tauri command, restores it
and checks the resulting database contents.

The [batched read API](../../docs/read-many.md) also works through the local
bridge. The window test checks nodes, edges, duplicates and missing records
in one `read-many` request.
