#!/usr/bin/env bash
# Tests the one-liner and the Linux packages in clean containers, as a user
# would run them, against the files of `dist/linux` served as a release:
#
#   docker build -f packaging/linux/Dockerfile --output dist/linux .
#   packaging/linux/test-install.sh [image ...]
#
# For each image it runs `curl -fsSL …/install.sh | sh` as root (the
# package: the user, the settings, the service's unit), checks the version,
# that the server answers with those settings, and that nothing graphical
# came with it (no .desktop entry, no icons, no GTK or WebKit); then, as a
# user without root, the binary into ~/.local/bin. With `--systemd` it also
# boots systemd in an AlmaLinux container and checks the service runs.
#
# Needs Docker and Python 3 (to serve the files).
set -euo pipefail
# Git Bash on Windows would rewrite the paths given to docker.
export MSYS_NO_PATHCONV=1

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIST="$ROOT/dist/linux"
PORT="${GRAFENO_TEST_PORT:-8799}"
SYSTEMD=0
IMAGES=()
for arg in "$@"; do
    case "$arg" in
        --systemd) SYSTEMD=1 ;;
        *) IMAGES+=("$arg") ;;
    esac
done
if [ ${#IMAGES[@]} -eq 0 ]; then
    IMAGES=(almalinux:9 almalinux:8 rockylinux:9 fedora:42 debian:12 ubuntu:24.04)
fi
[ -f "$DIST/grafeno-linux-x86_64.rpm" ] || { echo "build dist/linux first" >&2; exit 1; }

# The files laid out as GitHub serves a release, with install.sh beside them.
SITE="$(mktemp -d)"
trap 'kill "$SERVER" 2>/dev/null || true; wait "$SERVER" 2>/dev/null || true; rm -rf "$SITE" 2>/dev/null || true' EXIT
mkdir -p "$SITE/releases/latest/download"
cp "$DIST"/* "$SITE/releases/latest/download/"
cp "$ROOT/install.sh" "$SITE/install.sh"
PYTHON="$(command -v python3 || command -v python)"
(cd "$SITE" && exec "$PYTHON" -m http.server "$PORT" --bind 0.0.0.0 >/dev/null 2>&1) &
SERVER=$!
sleep 1
URL="http://host.docker.internal:$PORT"

# What runs inside each container, as root.
read -r -d '' CHECK <<'EOF' || true
set -eu
if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq >/dev/null && apt-get install -y -qq curl ca-certificates >/dev/null
fi
command -v curl >/dev/null || { dnf install -y -q curl >/dev/null 2>&1 || yum install -y -q curl >/dev/null; }
# Minimal images (Fedora's) come without su; a real system has it.
command -v su >/dev/null || { dnf install -y -q util-linux >/dev/null 2>&1 || yum install -y -q util-linux >/dev/null; }
curl -fsSL "$URL/install.sh" | GRAFENO_BASE_URL="$URL/releases" sh
grafeno --version
test -f /etc/grafeno/grafeno.toml
id grafeno >/dev/null
test -d /var/lib/grafeno
if command -v rpm >/dev/null 2>&1 && rpm -q grafeno >/dev/null 2>&1; then
    files="$(rpm -ql grafeno)"; needs="$(rpm -qR grafeno)"
else
    files="$(dpkg -L grafeno)"; needs="$(dpkg-query -W -f '${Depends}' grafeno)"
fi
echo "$files" | grep -Eq 'applications/|icons/|\.desktop' && { echo "graphical files installed:"; echo "$files"; exit 1; }
echo "$needs" | grep -Eqi 'gtk|webkit|x11|wayland' && { echo "graphical dependencies: $needs"; exit 1; }
su -s /bin/sh grafeno -c "grafeno serve --config /etc/grafeno/grafeno.toml" >/tmp/serve.log 2>&1 &
for _ in $(seq 50); do curl -fs http://127.0.0.1:7810/health >/dev/null 2>&1 && break; sleep 0.2; done
curl -fs http://127.0.0.1:7810/health
token="$(grafeno config token --path /etc/grafeno/grafeno.toml)"
curl -fs -H "Authorization: Bearer $token" http://127.0.0.1:7810/v1/namespaces >/dev/null
curl -fs http://127.0.0.1:7810/studio/ | grep -q 'installed on its own'
# A user without root gets the binary in ~/.local/bin.
useradd -m tester >/dev/null 2>&1 || adduser --disabled-password --gecos '' tester >/dev/null
su - tester -c "curl -fsSL '$URL/install.sh' | GRAFENO_BASE_URL='$URL/releases' sh && ~/.local/bin/grafeno --version"
echo "OK"
EOF

failed=0
for image in "${IMAGES[@]}"; do
    echo "== $image"
    if docker run --rm --add-host host.docker.internal:host-gateway -e URL="$URL" "$image" \
        sh -c "$CHECK" >"$SITE/$(echo "$image" | tr ':/' '__').log" 2>&1; then
        echo "   passed ($(grep -m1 '^grafeno ' "$SITE/$(echo "$image" | tr ':/' '__').log"))"
    else
        echo "   FAILED:"
        tail -20 "$SITE/$(echo "$image" | tr ':/' '__').log" | sed 's/^/   /'
        failed=1
    fi
done

if [ "$SYSTEMD" = 1 ]; then
    echo "== almalinux/9-init (systemd)"
    name="grafeno-systemd-test"
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker run -d --name "$name" --privileged --cgroupns=host \
        -v /sys/fs/cgroup:/sys/fs/cgroup:rw --add-host host.docker.internal:host-gateway \
        almalinux/9-init >/dev/null
    sleep 3
    if docker exec -e URL="$URL" "$name" sh -c '
        set -eu
        curl -fsSL "$URL/install.sh" | GRAFENO_BASE_URL="$URL/releases" sh
        for _ in $(seq 50); do curl -fs http://127.0.0.1:7810/health >/dev/null 2>&1 && break; sleep 0.2; done
        systemctl is-active grafeno
        systemctl is-enabled grafeno
        curl -fs http://127.0.0.1:7810/health
        dnf remove -y -q grafeno >/dev/null
        test -f /etc/grafeno/grafeno.toml && test -d /var/lib/grafeno
        echo "OK: the service ran, and removing the package kept the data"
    ' >"$SITE/systemd.log" 2>&1; then
        echo "   passed"
    else
        echo "   FAILED:"; tail -20 "$SITE/systemd.log" | sed 's/^/   /'; failed=1
    fi
    docker rm -f "$name" >/dev/null 2>&1 || true
fi
exit "$failed"
