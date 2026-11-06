# Installing Grafeno

There are two families of installers:

- **Grafeno Desktop**, the desktop application: Grafeno Studio in its own
  window, which opens database files with the engine inside the app (no
  service, no port) or connects to a Grafeno server. For using Grafeno on
  your own computer.
- **The Grafeno server**: the `grafeno` program as a service, for agents
  and applications that need Grafeno running all the time (and for
  machines without a desktop).

Both can be installed side by side. A database file can be open in one of
them at a time.

## Grafeno Desktop

| System | File |
| ------ | ---- |
| Windows 10/11 (x64) | `grafeno-desktop-<version>-windows-x64-setup.exe` (for your user, no administrator) or `grafeno-desktop-<version>-windows-x64.msi` |
| Debian, Ubuntu (x86_64) | `grafeno-desktop_<version>_amd64.deb` |
| Fedora, RHEL, openSUSE (x86_64) | `grafeno-desktop-<version>-1.x86_64.rpm` |
| Any Linux (x86_64) | `grafeno-desktop-<version>-x86_64.AppImage` (`chmod +x`, then run it) |

The installers add **Grafeno Desktop** to the Start menu or the application
menu, with the Grafeno icon, and make `.redb` files open with it. On
Windows the window uses WebView2, already part of Windows 10 and 11 (the
setup installs it if it is missing); on Linux the packages depend on
WebKitGTK, which the package manager installs.

On first start it shows the start screen: **New database…** creates a
file, **Open…** opens one, recent databases are listed, and **A Grafeno
server** connects to the installed service (`http://127.0.0.1:7810` with
the token from `grafeno config token`) or another machine. More in
[apps/desktop/README.md](../apps/desktop/README.md).

## The Grafeno server

Every way of installing the server gives the same `grafeno` program: the HTTP server
with Grafeno Studio (the web interface) inside, the MCP server, the shell
and the administration commands. The installers also run it as a service
that starts with the system.

| System | File | What it sets up |
| ------ | ---- | --------------- |
| Windows 10/11, Server 2019+ (x64) | `grafeno-<version>-windows-x64-setup.exe` | `C:\Program Files\Grafeno`, the `Grafeno` service, PATH, Start menu shortcuts |
| Debian, Ubuntu and derivatives (x86_64) | `grafeno_<version>-1_amd64.deb` | `/usr/bin/grafeno`, the `grafeno` systemd service and user |
| Fedora, RHEL, Rocky, Alma, openSUSE (x86_64) | `grafeno-<version>-1.x86_64.rpm` | the same |
| Any Linux (x86_64) | `grafeno-x86_64-unknown-linux-musl.tar.gz`, or `install.sh` | the binary; the service with `--service` |
| Containers | `Dockerfile` | an image serving on port 7810, data in `/data` |

The Linux binary is static (musl): the same file runs on any distribution,
whatever its glibc version.

After installing, open **Grafeno Studio** at <http://127.0.0.1:7810/studio/>.
It asks for the server's token, which the installer generated:

```bash
grafeno config token
```

(On Linux, as root: `sudo grafeno config token`.)

### Windows

Run `grafeno-<version>-windows-x64-setup.exe` (it asks for administrator
rights). It lets you choose to:

- **Run Grafeno as a Windows service**: the `Grafeno` service starts with
  Windows and serves on `127.0.0.1:7810`.
- **Add grafeno to the system PATH**: to run `grafeno` from any terminal
  (open a new one after installing).

It creates `C:\ProgramData\Grafeno\grafeno.toml` (address, token, Studio)
and keeps the database in `C:\ProgramData\Grafeno\data`. The Start menu gets
*Grafeno Studio*, *Grafeno Shell* (connected to the service) and *Grafeno
settings*. The service logs to `C:\ProgramData\Grafeno\grafeno.log`.

Manage the service like any other (`services.msc`, or PowerShell as
administrator):

```powershell
Restart-Service Grafeno
```

Upgrading runs the new installer over the old one: it stops the service,
replaces the program, and keeps the settings and data. Uninstalling
(Settings → Apps) removes the program, the service and the PATH entry, and
keeps `C:\ProgramData\Grafeno`.

Without the installer, `grafeno-x86_64-pc-windows-msvc.zip` has the bare
`grafeno.exe`; as administrator, the service can then be set up by hand:

```powershell
grafeno config init
```

```powershell
grafeno service install
```

### Debian and Ubuntu

```bash
sudo apt install ./grafeno_0.1.0-1_amd64.deb
```

### Fedora, RHEL and derivatives

```bash
sudo dnf install ./grafeno-0.1.0-1.x86_64.rpm
```

Fedora 45 and later reject unsigned packages; until releases are signed,
add `--nogpgcheck` to `dnf` (or `--nosignature` to `rpm -i`).

#### What the packages do

- Create the `grafeno` system user and `/var/lib/grafeno` (its data).
- Create `/etc/grafeno/grafeno.toml` with a new token, readable by root and
  the `grafeno` group only. Upgrades keep it.
- Install and start the `grafeno` systemd service, sandboxed so that it can
  write its data folder and nothing else (`systemd-analyze security
  grafeno` rates it 1.3, "OK").
- Add **Grafeno Studio** to the application menu, with the Grafeno icon:
  it opens the service's Studio in the browser.

```bash
systemctl status grafeno
```

```bash
journalctl -u grafeno -f
```

Removing the package stops the service and keeps `/var/lib/grafeno` and
`/etc/grafeno`, even on purge: they may be the only copy of someone's
memory.

### Any Linux: install.sh

```bash
curl -fsSL https://raw.githubusercontent.com/Seerendo/grafeno/main/install.sh | sh
```

It downloads the latest release for your architecture, checks its SHA-256,
and installs `grafeno` to `~/.local/bin` (or `/usr/local/bin` as root). With
`--service` (as root, with systemd) it also sets up the service as the
packages do:

```bash
curl -fsSL https://raw.githubusercontent.com/Seerendo/grafeno/main/install.sh | sudo sh -s -- --service
```

Other options: `--version v0.1.0`, `--prefix DIR`. `GRAFENO_BASE_URL`
points it at a mirror.

### Docker

```bash
docker build -t grafeno .
```

```bash
docker run -d -p 7810:7810 -v grafeno-data:/data -e GRAFENO_TOKEN=change-me grafeno
```

## A cluster of servers

Three (or five) servers can hold the same data and keep serving when one
fails (phases 13.2 and 13.3). Each gets the same `[cluster]` section in its
`grafeno.toml`, with its own `id` and `addr`:

```toml
[cluster]
id = 1                         # 2 and 3 on the others
addr = "10.0.0.1:7811"         # where this member listens for the others
secret = "a long random string, the same on every member"
voters = [1, 2, 3]             # the first members; [] on a member that joins later
peers = [                      # addr: the members' port; api: the HTTP API, for forwarding
  { id = 1, addr = "10.0.0.1:7811", api = "10.0.0.1:7810" },
  { id = 2, addr = "10.0.0.2:7811", api = "10.0.0.2:7810" },
  { id = 3, addr = "10.0.0.3:7811", api = "10.0.0.3:7810" },
]
```

Start all three: they elect a leader within a second or so. Send requests
to any member: a follower forwards writes to the leader (with `api` in
`peers`; without it, it answers 421 `not_leader` naming the leader), and
reads are served by the member itself. A read can ask for more with the
header `x-grafeno-consistency`: `bounded` goes to the leader when the
member lags far behind, `linearizable` sees every write acknowledged
before it. Every response carries `x-grafeno-position`; a request with
`x-grafeno-after: N` waits until its member has applied N (read your
writes). A GQ write sent with an `Idempotency-Key` header runs once,
however many times it is retried. The shell (`--url` with several
addresses, comma-separated) and the TypeScript client (`url: [...]`)
move to another member when one fails and read their own writes. When
the leader dies, writes resume in about half a second.

A costly read (estimated cost from `split_cost` in `[cluster]`, 200 000
by default, as `EXPLAIN` shows it) is split across the members: each
runs a share at the same position, and the member asked merges the
parts into the same rows one server returns. `x-grafeno-split: on`
splits a read whenever its shape allows, `off` never. Members that
may take parts need `api` in `peers`.

Writes of one group go through one leader at a time. To write more, run
more groups, each with its own leader and database file
(`<db>.group-<id>.redb`) on its voters:

```toml
[cluster]
# … as above, then:
groups = [{ id = 1, voters = [1, 2, 3] }, { id = 2, voters = [1, 2, 3] }]
```

Namespaces live in group 0 until placed elsewhere:

```bash
curl -X POST http://10.0.0.1:7810/v1/cluster/placement -H "Authorization: Bearer …" -H 'content-type: application/json' -d '{"namespace": "vault", "group": 1}'
```

A namespace that already has data moves while it keeps serving: its
writes pause for tens of milliseconds (they answer busy; clients retry)
and none is lost. Any member takes requests for any namespace. Back up
each group's file (`GET /v1/backup?group=1`; backups can be taken on a
follower). `grafeno cluster status --url http://10.0.0.1:7810 --token …`
(`--group 1` for another group) shows a member's view, with elections,
leader changes, snapshots, the entry format the group writes, what each
member runs and, on the leader, each member's lag; `/v1/metrics` has the
same as `grafeno_cluster_*` series, and Grafeno Studio a Cluster view; `grafeno cluster voters 1 2 3 4` (on the leader)
changes the members, the new one listed in every member's `peers` and
started with `voters = []`; `grafeno cluster compact` snapshots a member's
database and trims its log. Each member keeps its Raft state in
`<db>.raft` and snapshots in `<db>.snapshots/`.

A three-member compose cluster, systemd and Windows service setups, and
the runbooks for losing a member, losing a majority, replacing a disk and
upgrading are in [cluster-runbooks.md](cluster-runbooks.md).

For now: traffic between members is authenticated by the secret but not
encrypted (keep them on a private network); imports go into a single server
before it starts a cluster. Webhooks and background work (extraction,
summaries) run on the leader, and a new leader resumes them.

## Accounts

A new server is open to anyone who reaches it (fine on your own
machine) until it has accounts, or a token in `grafeno.toml`. Make the
first account, a superuser, with the server stopped or through it:

```bash
grafeno users create admin --superuser --config /etc/grafeno/grafeno.toml
```

(On an open server, Grafeno Studio also offers to create it.) Then make
the people who sign in, with grants per namespace, and service accounts
for agents and applications, which use API keys:

```bash
grafeno users create ana --grant notes:write --config /etc/grafeno/grafeno.toml
```

```bash
grafeno users create indexer --service --grant "team-*:write" --config /etc/grafeno/grafeno.toml
```

Users sign in from the Studio, the shell (`grafeno shell --url … --user
ana`) or the API (`POST /v1/auth/signin`), and can make their own API
keys, scoped down to some namespaces or to reading. `grafeno users
password`, `disable`, `enable` and `delete` manage them. Keys from before
14.1 kept working: each became a service account of its name. The token
in `grafeno.toml`, when set, still does everything: keep it for
emergencies. `[auth]` (below) sets how long sessions last and when
failed sign-ins lock an account.

## Backups

A backup is a consistent copy of the whole database file (every
namespace, and the API keys), taken while the service keeps running:

```bash
grafeno backup grafeno-backup.redb --config /etc/grafeno/grafeno.toml
```

(On Windows, the settings are in `C:\ProgramData\Grafeno\grafeno.toml`;
`grafeno config path` prints where.) To restore, stop the service, put the
copy where `db` in `grafeno.toml` points, and start it again. Before using
a file that may be damaged (a failing disk, a bad copy), check it with the
service stopped:

```bash
grafeno check --db grafeno-backup.redb
```

It verifies every page against its checksum, then the indexes. Exports
(`grafeno export`) are the other kind of copy: JSON Lines, readable and
portable between versions, but without the keys.

To move a namespace to or from other tools (spreadsheets, Gephi, yEd,
NetworkX, APOC), use CSV or GraphML. They hold current knowledge, without
retracted facts or the history behind them, so they do not replace a
backup:

```bash
grafeno export --format csv -n notes -o notes-csv
```

```bash
grafeno import --format csv -n notes notes-csv/nodes.csv --edges notes-csv/edges.csv
```

```bash
grafeno export --format graphml -n notes -o notes.graphml
```

```bash
grafeno import --format graphml -n notes notes.graphml
```

## Settings

`grafeno.toml` is read by the service, and by `grafeno serve --config` and
`grafeno shell --config`. Restart the service after changing it.

```toml
db = "/var/lib/grafeno/grafeno.redb"
addr = "127.0.0.1:7810"     # "0.0.0.0:7810" listens on every network interface
token = "…"                 # every /v1 request needs `Authorization: Bearer <token>`
studio = true               # serve Grafeno Studio at /studio/
cors_origins = []           # origins allowed to call the API from a browser ("*": any)
durability = "immediate"    # or "eventual": faster writes, a crash loses at most 0.5 s
max_requests = 512          # served at once; more get 503 with Retry-After

[query]                     # limits for each GQ query over HTTP and MCP; 0 turns one off
timeout_secs = 30
max_reads = 50000000        # nodes, edges and index entries read
max_rows = 1000000          # rows held at once
max_memory_mb = 512         # one value a query builds, or the rows it holds at once
max_cost = 0                # refuse queries EXPLAIN estimates above this (0: off)
scan_threads = 0            # threads large read scans share, over every query (0: one per CPU; 1: off)

[writer]                    # the group commit (see "Monitoring" below)
max_batch = 512             # writes per transaction at most
queue = 4096                # writes that may wait; more get 503
flush_ms = 500              # with eventual durability, how often commits reach the disk
import_chunk = 2000         # records per transaction of an import with ?bulk=true

[memory]                    # caches, in MiB (see "Memory" below)
cache_mb = 256              # pages of the database file
projection_cache_mb = 256   # graphs kept for graph algorithms
vector_cache_mb = 128       # vectors decoded while inserting embeddings

[changes]                   # the change log that streams and webhooks follow
enabled = true
max_entries = 1000000       # per namespace; 0 for no limit
max_age_days = 7            # 0 for no limit

[auth]                      # accounts and sign-in
session_minutes = 15        # an access token's life (sessions renew it)
refresh_days = 30           # a session's life
lockout_attempts = 5        # failed sign-ins that lock an account for a while
address_attempts = 20       # failed sign-ins that refuse an address for a while
lockout_minutes = 15

[background]                # work that follows the change log
extract = ["notes"]         # link what new and changed texts mention
communities = ["notes"]     # keep community summaries fresh
communities_every_secs = 300
communities_min_changes = 50
```

Flags and environment variables (`GRAFENO_DB`, `GRAFENO_ADDR`,
`GRAFENO_TOKEN`) take precedence over the file. Expose the port beyond
`127.0.0.1` only behind TLS (a reverse proxy) and with a token.

### Memory

Grafeno's caches are the main memory it keeps: pages of the database file
(`cache_mb`), graphs projected for algorithms (`projection_cache_mb`) and
vectors decoded while inserting (`vector_cache_mb`). Everything else is
per request and bounded by the `[query]` limits. With 10 million edges a
server stays near 300 MiB with the defaults; on a 512 MiB machine set
`cache_mb = 128` and `projection_cache_mb = 64` (graph algorithms on such
a graph then build their projection, about 140 MiB, each time, and may
need a longer `timeout_secs`). Loading many embeddings goes about twice as
fast with `vector_cache_mb = 1024` and large write transactions, if the
memory is there. Numbers in [benchmarks](benchmarks.md).

### Monitoring

`GET /v1/metrics` answers in the Prometheus text format: requests and
their time, GQ queries, the limits they hit, the writer's queue, groups,
commit time and write latency per lane. It needs a secret, like the rest
of `/v1`; a key with any grant will do:

```yaml
scrape_configs:
  - job_name: grafeno
    metrics_path: /v1/metrics
    authorization:
      credentials: gfk_…
    static_configs:
      - targets: ["127.0.0.1:7810"]
```

Watch `grafeno_write_queue` and `grafeno_write_seconds`: a queue that
stays up means writes arrive faster than one writer commits them (about
11 000 a second on an SSD with immediate durability, more with
`durability = "eventual"`); `grafeno_write_rejected_total` counts the 503s
past the queue's limit. Load large exports with `?bulk=true`: they then go
in chunks between the agents' writes instead of before them.

## Grafeno Studio on its own

The Studio is a static web app. Besides the copy inside `grafeno`, its build
(`studio/dist`) can be hosted anywhere and pointed at a server, which must
then allow that origin:

```bash
grafeno serve --config grafeno.toml --cors-origin https://studio.example.com
```

`grafeno serve --no-studio` turns the built-in copy off.

## Building the installers

| Files | Command | Needs |
| ----- | ------- | ----- |
| Linux `.tar.gz`, `.deb`, `.rpm` | `docker build -f packaging/linux/Dockerfile --output dist/linux .` | Docker |
| Windows setup `.exe` | `cd studio && npm ci && npm run build`, then `cargo build --release -p grafeno-server`, then `iscc /DAppVersion=0.1.0 packaging\windows\grafeno.iss` | Node 22+, Rust (MSVC), Inno Setup 6 |
| Grafeno Desktop (the installers of the system it runs on: `.exe` and `.msi`, or `.deb`, `.rpm` and `.AppImage`) | `npm ci --prefix studio`, `npm ci --prefix apps/desktop`, then `cd apps/desktop && npx tauri build` | Node 22+, Rust; on Linux WebKitGTK (see [apps/desktop/README.md](../apps/desktop/README.md)) |

Every icon comes from `docs/assets/logo.svg`: `npm run icons` in
`apps/desktop` renders the desktop app's icons, `packaging/windows/grafeno.ico`
(the server's executable, installer and shortcuts) and
`packaging/linux/icons/` (the server packages' menu entry).
`.github/workflows/release.yml` builds all of them and publishes a GitHub
release when a `v*` tag is pushed.
