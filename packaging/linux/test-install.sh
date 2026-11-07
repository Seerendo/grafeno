#!/usr/bin/env bash
# Tests the one-liner, the Linux packages and the repositories in clean
# containers, as a user would run them, against the files of `dist/linux`
# served as a release:
#
#   docker build -f packaging/linux/Dockerfile --output dist/linux .
#   packaging/linux/test-install.sh [--repo] [--systemd] [--arm64] [image ...]
#
# Always, in each image: the release's files are signed with a throwaway
# minisign key, and `curl -fsSL …/install.sh | sh` runs as root (the package:
# the user, the default instance, the service's unit) and must verify the
# checksum (and the signature where minisign is installed); the version,
# the settings, the server answering with them, `grafeno doctor`, and that
# nothing graphical came with it. Then, as a user without root, the binary
# into ~/.local/bin. On Debian's family, `apt-get purge` must remove the
# data.
#
#   --repo     also builds the APT and DNF repositories (packaging/repo,
#              signed with a throwaway GnuPG key) and installs from them,
#              with the package managers checking the signatures.
#   --systemd  boots systemd in an AlmaLinux container: the service runs;
#              `grafeno setup` makes a second instance with an
#              administrator; an old database is copied before its layout
#              is upgraded and `grafeno instances rollback` puts it back; an
#              upgrade restarts the instances; removing keeps the data.
#   --arm64    runs the image checks on aarch64 too (emulated).
#
# Needs Docker and Python 3 (to serve the files).
set -euo pipefail
# Git Bash on Windows would rewrite the paths given to docker.
export MSYS_NO_PATHCONV=1

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIST="$ROOT/dist/linux"
PORT="${GRAFENO_TEST_PORT:-8799}"
SYSTEMD=0
REPO=0
ARM64=0
IMAGES=()
for arg in "$@"; do
    case "$arg" in
        --systemd) SYSTEMD=1 ;;
        --repo) REPO=1 ;;
        --arm64) ARM64=1 ;;
        *) IMAGES+=("$arg") ;;
    esac
done
if [ ${#IMAGES[@]} -eq 0 ]; then
    IMAGES=(almalinux:9 almalinux:8 rockylinux:9 fedora:42 debian:12 ubuntu:24.04)
fi
[ -f "$DIST/grafeno-linux-x86_64.rpm" ] || { echo "build dist/linux first" >&2; exit 1; }

# The files laid out as GitHub serves a release, with install.sh beside
# them, the repositories under repo/, and the fixtures of old layouts.
SITE="$(mktemp -d)"
SERVER=""
trap 'kill "$SERVER" 2>/dev/null || true; wait "$SERVER" 2>/dev/null || true; rm -rf "$SITE" 2>/dev/null || true' EXIT
mkdir -p "$SITE/releases/latest/download" "$SITE/fixtures"
cp "$DIST"/* "$SITE/releases/latest/download/"
cp "$ROOT/install.sh" "$SITE/install.sh"
cp "$ROOT/crates/grafeno/tests/fixtures/layout-v16.redb.gz" "$SITE/fixtures/"
URL="http://host.docker.internal:$PORT"
# A folder as Docker sees it (Windows paths under Git Bash).
host_path() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

# Throwaway keys: minisign for the release's files, GnuPG for the
# repositories.
echo "== signing (throwaway keys)"
docker run --rm -v "$(host_path "$SITE"):/site" -v "$(host_path "$ROOT"):/src:ro" -e REPO="$REPO" -e URL="$URL" debian:trixie sh -c '
    set -eu
    apt-get update -qq >/dev/null
    apt-get install -y -qq minisign gnupg rpm createrepo-c apt-utils >/dev/null 2>&1
    minisign -G -W -f -p /site/minisign.pub -s /tmp/minisign.key >/dev/null
    for f in /site/releases/latest/download/*; do
        case "$f" in *.sha256) continue ;; esac
        minisign -S -s /tmp/minisign.key -m "$f" >/dev/null
    done
    if [ "$REPO" = 1 ]; then
        gpg --batch --pinentry-mode loopback --passphrase "" \
            --quick-gen-key "Grafeno test <test@invalid>" rsa3072 sign never 2>/dev/null
        key="$(gpg --list-keys --with-colons | awk -F: "/^fpr/ {print \$10; exit}")"
        GRAFENO_GPG_KEY="$key" sh /src/packaging/repo/build-repo.sh \
            /site/releases/latest/download /site/repo "$URL/repo" >/dev/null
    fi
    chmod -R a+rX /site
'
MINISIGN_KEY="$(tail -1 "$SITE/minisign.pub")"
echo "   minisign key $MINISIGN_KEY"

PYTHON="$(command -v python3 || command -v python)"
(cd "$SITE" && exec "$PYTHON" -m http.server "$PORT" --bind 0.0.0.0 >/dev/null 2>&1) &
SERVER=$!
sleep 1

# What runs inside each container, as root. MODE is "file" (the release's
# package) or "repo".
read -r -d '' CHECK <<'EOF' || true
set -eu
if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq >/dev/null && apt-get install -y -qq curl ca-certificates minisign >/dev/null
fi
command -v curl >/dev/null || { dnf install -y -q curl >/dev/null 2>&1 || yum install -y -q curl >/dev/null; }
# Minimal images (Fedora's) come without su; a real system has it.
command -v su >/dev/null || { dnf install -y -q util-linux >/dev/null 2>&1 || yum install -y -q util-linux >/dev/null; }
if [ "$MODE" = repo ]; then
    curl -fsSL "$URL/install.sh" | GRAFENO_BASE_URL="$URL/releases" GRAFENO_REPO_URL="$URL/repo" sh | tee /tmp/install.log
    grep -q "from Grafeno's repository" /tmp/install.log
else
    curl -fsSL "$URL/install.sh" | GRAFENO_BASE_URL="$URL/releases" GRAFENO_REPO=0 \
        GRAFENO_MINISIGN_KEY="$KEY" sh | tee /tmp/install.log
    grep -q "Checksum verified" /tmp/install.log
    if command -v minisign >/dev/null 2>&1; then
        grep -q "Signature verified" /tmp/install.log
        # A key that did not sign the files is refused.
        if curl -fsSL "$URL/install.sh" | GRAFENO_BASE_URL="$URL/releases" GRAFENO_REPO=0 \
            GRAFENO_MINISIGN_KEY="RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3" sh >/tmp/bad.log 2>&1; then
            echo "a wrong signing key was accepted"; exit 1
        fi
        grep -q "does not match the release key" /tmp/bad.log
    fi
fi
grafeno --version
test -f /etc/grafeno/grafeno.toml
id grafeno >/dev/null
test -d /var/lib/grafeno
test -f /lib/systemd/system/grafeno@.service || test -f /usr/lib/systemd/system/grafeno@.service
if command -v rpm >/dev/null 2>&1 && rpm -q grafeno >/dev/null 2>&1; then
    files="$(rpm -ql grafeno)"; needs="$(rpm -qR grafeno)"
    echo "$files" | grep -q /usr/share/selinux/packages/grafeno.pp
    if [ "$MODE" = repo ]; then rpm -qi grafeno | grep -Eqi '^Signature.*key id'; fi
else
    files="$(dpkg -L grafeno)"; needs="$(dpkg-query -W -f '${Depends}' grafeno)"
    echo "$files" | grep -q /etc/apparmor.d/grafeno
fi
echo "$files" | grep -Eq 'applications/|icons/|\.desktop' && { echo "graphical files installed:"; echo "$files"; exit 1; }
echo "$needs" | grep -Eqi 'gtk|webkit|x11|wayland' && { echo "graphical dependencies: $needs"; exit 1; }
su -s /bin/sh grafeno -c "exec grafeno serve --config /etc/grafeno/grafeno.toml" >/tmp/serve.log 2>&1 &
serve_pid=$!
for _ in $(seq 50); do curl -fs http://127.0.0.1:7810/health >/dev/null 2>&1 && break; sleep 0.2; done
curl -fs http://127.0.0.1:7810/health
token="$(grafeno config token --path /etc/grafeno/grafeno.toml)"
curl -fs -H "Authorization: Bearer $token" http://127.0.0.1:7810/v1/namespaces >/dev/null
curl -fs http://127.0.0.1:7810/studio/ | grep -q 'installed on its own'
grafeno instances | grep -q '^default'
grafeno doctor
# A user without root gets the binary in ~/.local/bin.
useradd -m tester >/dev/null 2>&1 || adduser --disabled-password --gecos '' tester >/dev/null
su - tester -c "curl -fsSL '$URL/install.sh' | GRAFENO_BASE_URL='$URL/releases' sh && ~/.local/bin/grafeno --version"
# Purging the .deb removes the settings and the data (removing keeps them).
if command -v dpkg >/dev/null 2>&1 && dpkg -s grafeno >/dev/null 2>&1; then
    kill "$serve_pid" 2>/dev/null || true; sleep 1
    apt-get remove -y -qq grafeno >/dev/null
    test -f /etc/grafeno/grafeno.toml && test -d /var/lib/grafeno
    apt-get purge -y -qq grafeno >/dev/null
    test ! -e /etc/grafeno && test ! -e /var/lib/grafeno
fi
echo "OK"
EOF

failed=0
run_check() {
    local image="$1" mode="$2" platform="$3"
    local log="$SITE/$(echo "$image-$mode-$platform" | tr ':/' '__').log"
    echo "== $image ($mode${platform:+, $platform})"
    if docker run --rm ${platform:+--platform "$platform"} --add-host host.docker.internal:host-gateway \
        -e URL="$URL" -e MODE="$mode" -e KEY="$MINISIGN_KEY" "$image" sh -c "$CHECK" >"$log" 2>&1; then
        echo "   passed ($(grep -m1 '^grafeno ' "$log"))"
    else
        echo "   FAILED:"
        tail -25 "$log" | sed 's/^/   /'
        failed=1
    fi
}

for image in "${IMAGES[@]}"; do
    run_check "$image" file ""
    if [ "$REPO" = 1 ]; then
        run_check "$image" repo ""
    fi
done
if [ "$ARM64" = 1 ]; then
    for image in almalinux:9 debian:12; do
        run_check "$image" file linux/arm64
        if [ "$REPO" = 1 ]; then
            run_check "$image" repo linux/arm64
        fi
    done
fi

if [ "$SYSTEMD" = 1 ]; then
    mode=file
    [ "$REPO" = 1 ] && mode=repo
    echo "== almalinux/9-init (systemd, $mode)"
    name="grafeno-systemd-test"
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker run -d --name "$name" --privileged --cgroupns=host \
        -v /sys/fs/cgroup:/sys/fs/cgroup:rw --add-host host.docker.internal:host-gateway \
        almalinux/9-init >/dev/null
    sleep 3
    if docker exec -e URL="$URL" -e MODE="$mode" "$name" sh -c '
        set -eu
        wait_for() { for _ in $(seq 75); do curl -fs "$1/health" >/dev/null 2>&1 && return 0; sleep 0.2; done; return 1; }
        not() { if "$@"; then echo "unexpectedly true: $*"; exit 1; fi; }
        if [ "$MODE" = repo ]; then
            curl -fsSL "$URL/install.sh" | GRAFENO_BASE_URL="$URL/releases" GRAFENO_REPO_URL="$URL/repo" sh
        else
            curl -fsSL "$URL/install.sh" | GRAFENO_BASE_URL="$URL/releases" GRAFENO_REPO=0 sh
        fi
        wait_for http://127.0.0.1:7810
        systemctl is-active grafeno
        systemctl is-enabled grafeno
        grafeno doctor

        echo "-- a second instance, with an administrator"
        printf "s3cret-Passw0rd!\n" > /tmp/pw
        grafeno setup --instance second --unattended --admin root --admin-password-file /tmp/pw
        wait_for http://127.0.0.1:7811
        systemctl is-active grafeno@second
        curl -fs -X POST -H "content-type: application/json" \
            -d "{\"name\":\"root\",\"password\":\"s3cret-Passw0rd!\"}" \
            http://127.0.0.1:7811/v1/auth/signin | grep -q access_token
        stat -c "%U:%G %a" /var/lib/grafeno/second | grep -q "grafeno:grafeno 750"
        stat -c "%U:%G %a" /etc/grafeno/second.toml | grep -q "root:grafeno 640"
        grafeno instances | tee /tmp/instances
        grep -q "^second .*7811 .*running" /tmp/instances
        grafeno doctor --instance second

        echo "-- an instance with its data elsewhere"
        grafeno setup --instance elsewhere --unattended --no-admin --data /srv/grafeno-elsewhere --port 7815
        wait_for http://127.0.0.1:7815
        test -f /srv/grafeno-elsewhere/grafeno.redb
        grep -q "ReadWritePaths=/srv/grafeno-elsewhere" /etc/systemd/system/grafeno@elsewhere.service.d/data.conf
        grafeno instances drop elsewhere --purge --yes
        test ! -e /srv/grafeno-elsewhere && test ! -e /etc/grafeno/elsewhere.toml
        not systemctl is-active -q grafeno@elsewhere

        echo "-- an older database: copied before its layout is upgraded, then rolled back"
        systemctl stop grafeno
        curl -fsSL "$URL/fixtures/layout-v16.redb.gz" | gunzip > /var/lib/grafeno/grafeno.redb
        chown grafeno:grafeno /var/lib/grafeno/grafeno.redb
        grafeno doctor --instance default | grep -q "has layout 16: the next start upgrades it"
        systemctl start grafeno
        wait_for http://127.0.0.1:7810
        test -f /var/lib/grafeno/grafeno.layout-16.redb
        grafeno doctor --instance default | grep -q "grafeno.layout-16.redb"
        grafeno instances rollback default
        not systemctl is-active -q grafeno
        grafeno doctor --instance default | grep -q "has layout 16"
        ls /var/lib/grafeno/grafeno.before-rollback-*.redb
        test ! -e /var/lib/grafeno/grafeno.layout-16.redb
        systemctl start grafeno && wait_for http://127.0.0.1:7810

        echo "-- an upgrade restarts the running instances"
        before="$(systemctl show -p MainPID --value grafeno@second)"
        if [ "$MODE" = repo ]; then dnf reinstall -y -q grafeno >/dev/null
        else curl -fsSL "$URL/releases/latest/download/grafeno-linux-x86_64.rpm" -o /tmp/g.rpm && dnf reinstall -y -q --nogpgcheck /tmp/g.rpm >/dev/null; fi
        wait_for http://127.0.0.1:7811
        after="$(systemctl show -p MainPID --value grafeno@second)"
        [ "$before" != "$after" ]
        systemctl is-active grafeno grafeno@second

        echo "-- removing the package keeps the data"
        dnf remove -y -q grafeno >/dev/null
        not systemctl is-active -q grafeno@second
        test -f /etc/grafeno/grafeno.toml && test -d /var/lib/grafeno/second
        echo "OK"
    ' >"$SITE/systemd.log" 2>&1; then
        echo "   passed"
    else
        echo "   FAILED:"; tail -30 "$SITE/systemd.log" | sed 's/^/   /'; failed=1
    fi
    docker rm -f "$name" >/dev/null 2>&1 || true
fi
exit "$failed"
