# Installing Grafeno

Every way of installing gives the same `grafeno` program: the HTTP server
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

## Windows

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

## Debian and Ubuntu

```bash
sudo apt install ./grafeno_0.1.0-1_amd64.deb
```

## Fedora, RHEL and derivatives

```bash
sudo dnf install ./grafeno-0.1.0-1.x86_64.rpm
```

Fedora 45 and later reject unsigned packages; until releases are signed,
add `--nogpgcheck` to `dnf` (or `--nosignature` to `rpm -i`).

### What the packages do

- Create the `grafeno` system user and `/var/lib/grafeno` (its data).
- Create `/etc/grafeno/grafeno.toml` with a new token, readable by root and
  the `grafeno` group only. Upgrades keep it.
- Install and start the `grafeno` systemd service, sandboxed so that it can
  write its data folder and nothing else (`systemd-analyze security
  grafeno` rates it 1.3, "OK").

```bash
systemctl status grafeno
```

```bash
journalctl -u grafeno -f
```

Removing the package stops the service and keeps `/var/lib/grafeno` and
`/etc/grafeno`, even on purge: they may be the only copy of someone's
memory.

## Any Linux: install.sh

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

## Docker

```bash
docker build -t grafeno .
```

```bash
docker run -d -p 7810:7810 -v grafeno-data:/data -e GRAFENO_TOKEN=change-me grafeno
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

[query]                     # limits for each GQ query over HTTP and MCP; 0 turns one off
timeout_secs = 30
max_reads = 50000000        # nodes, edges and index entries read
max_rows = 1000000          # rows held at once
```

Flags and environment variables (`GRAFENO_DB`, `GRAFENO_ADDR`,
`GRAFENO_TOKEN`) take precedence over the file. Expose the port beyond
`127.0.0.1` only behind TLS (a reverse proxy) and with a token.

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

`.github/workflows/release.yml` builds all of them and publishes a GitHub
release when a `v*` tag is pushed.
