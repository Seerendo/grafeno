#!/bin/sh
# Installs the Grafeno engine on Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/Seerendo/grafeno/main/install.sh | sudo sh
#
# As root, on a distribution whose package manager it knows, it installs the
# release's package, which creates the `grafeno` user, /etc/grafeno/grafeno.toml
# (with a new token) and the systemd service, and starts it:
#
#   RHEL, AlmaLinux, Rocky, Oracle Linux, CentOS Stream, Fedora, Amazon Linux
#     (dnf or yum)                                                  the .rpm
#   Debian, Ubuntu and their derivatives (apt)                      the .deb
#
# Elsewhere, without root, or with --binary, it installs the static binary
# (to /usr/local/bin as root, ~/.local/bin otherwise); with --service it also
# sets up the service. Every download is checked against its SHA-256.
#
# Options (or environment variables):
#   --version X.Y.Z    a specific release (GRAFENO_VERSION; default: the latest)
#   --binary           the binary, not the package (GRAFENO_METHOD=binary)
#   --prefix DIR       where the binary goes (GRAFENO_PREFIX)
#   --service          with --binary: the grafeno user, the settings and the
#                      systemd service (needs root)
#   --no-start         install, configure, but do not start the service
#
# GRAFENO_BASE_URL overrides where releases are downloaded from (mirrors, tests).

set -eu

REPO="Seerendo/grafeno"
BASE_URL="${GRAFENO_BASE_URL:-https://github.com/$REPO/releases}"
VERSION="${GRAFENO_VERSION:-latest}"
METHOD="${GRAFENO_METHOD:-auto}"
PREFIX="${GRAFENO_PREFIX:-}"
SERVICE=0
START=1

say() { printf '%s\n' "$*"; }
fail() { printf 'grafeno install: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --binary) METHOD="binary"; shift ;;
        --prefix) PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --service) SERVICE=1; shift ;;
        --no-start) START=0; shift ;;
        -h|--help) sed -n '2,27p' "$0" 2>/dev/null || true; exit 0 ;;
        *) fail "unknown option $1" ;;
    esac
done

[ "$(uname -s)" = "Linux" ] || fail "this script installs on Linux; on Windows use the setup .exe"
case "$(uname -m)" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) fail "no aarch64 release yet: build from source (cargo build --release -p grafeno-server)" ;;
    *) fail "no release for $(uname -m)" ;;
esac
case "$VERSION" in
    latest|v*) TAG="$VERSION" ;;
    *) TAG="v$VERSION" ;;
esac

IS_ROOT=0
[ "$(id -u)" = "0" ] && IS_ROOT=1

# The distribution's family, from /etc/os-release.
FAMILY=""
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    for id in ${ID:-} ${ID_LIKE:-}; do
        case "$id" in
            rhel|centos|fedora|almalinux|rocky|ol|amzn) FAMILY="rpm"; break ;;
            debian|ubuntu) FAMILY="deb"; break ;;
        esac
    done
fi

if [ "$METHOD" = "auto" ]; then
    if [ "$IS_ROOT" = 1 ] && [ -n "$FAMILY" ] && [ "$SERVICE" = 0 ] && [ -z "$PREFIX" ]; then
        METHOD="package"
    else
        METHOD="binary"
    fi
fi
if [ "$METHOD" = "package" ] && [ "$IS_ROOT" = 0 ]; then
    fail "installing the package needs root: run it with sudo, or add --binary"
fi
if [ "$METHOD" = "package" ] && [ -z "$FAMILY" ]; then
    fail "no package for this distribution (${PRETTY_NAME:-unknown}): add --binary"
fi

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        fail "curl or wget is needed"
    fi
}

url_of() {
    if [ "$TAG" = "latest" ]; then
        printf '%s/latest/download/%s' "$BASE_URL" "$1"
    else
        printf '%s/download/%s/%s' "$BASE_URL" "$TAG" "$1"
    fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Downloads a release file and checks it against its published SHA-256.
fetch() {
    url="$(url_of "$1")"
    say "Downloading $url"
    download "$url" "$TMP/$1" || fail "cannot download $url"
    download "$url.sha256" "$TMP/$1.sha256" || fail "cannot download the checksum of $1"
    expected="$(cut -d ' ' -f 1 "$TMP/$1.sha256")"
    actual="$(sha256sum "$TMP/$1" | cut -d ' ' -f 1)"
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $1: the download is damaged or altered"
    say "Checksum verified."
}

if [ "$METHOD" = "package" ]; then
    if [ "$FAMILY" = "rpm" ]; then
        PACKAGE="grafeno-linux-$ARCH.rpm"
        fetch "$PACKAGE"
        if command -v dnf >/dev/null 2>&1; then
            # The checksum above vouches for the file: releases are not yet
            # signed with a key the system knows.
            dnf install -y --nogpgcheck "$TMP/$PACKAGE"
        elif command -v yum >/dev/null 2>&1; then
            yum install -y --nogpgcheck "$TMP/$PACKAGE"
        else
            rpm -U --replacepkgs --nosignature "$TMP/$PACKAGE"
        fi
    else
        PACKAGE="grafeno-linux-amd64.deb"
        fetch "$PACKAGE"
        chmod 0644 "$TMP/$PACKAGE"
        DEBIAN_FRONTEND=noninteractive apt-get install -y "$TMP/$PACKAGE"
    fi
    # The package starts the service; --no-start stops it again.
    if [ "$START" = 0 ] && [ -d /run/systemd/system ]; then
        systemctl stop grafeno.service || true
    fi
    say ""
    say "Installed $(grafeno --version) from the package."
    say "Settings: /etc/grafeno/grafeno.toml   Data: /var/lib/grafeno"
    say "Token:    sudo grafeno config token"
    if [ -d /run/systemd/system ]; then
        say "Service:  systemctl status grafeno"
    fi
    exit 0
fi

if [ "$SERVICE" = 1 ] && [ "$IS_ROOT" = 0 ]; then
    fail "--service needs root: run it with sudo"
fi
if [ "$SERVICE" = 1 ] && ! command -v systemctl >/dev/null 2>&1; then
    fail "--service needs systemd; without it, install without --service and run grafeno serve"
fi
if [ -z "$PREFIX" ]; then
    if [ "$IS_ROOT" = 1 ]; then PREFIX="/usr/local/bin"; else PREFIX="$HOME/.local/bin"; fi
fi

ARCHIVE="grafeno-$ARCH-unknown-linux-musl.tar.gz"
fetch "$ARCHIVE"
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
mkdir -p "$PREFIX"
install -m 0755 "$TMP/grafeno" "$PREFIX/grafeno"
# On SELinux systems (RHEL and its family), a file moved from /tmp keeps a
# label systemd may not run: give it its place's.
if command -v restorecon >/dev/null 2>&1; then
    restorecon "$PREFIX/grafeno" 2>/dev/null || true
fi
say "Installed $("$PREFIX/grafeno" --version) to $PREFIX/grafeno"

if [ "$SERVICE" = 1 ]; then
    install -d /usr/share/grafeno
    install -m 0755 "$TMP/setup.sh" /usr/share/grafeno/setup.sh
    sed "s|/usr/bin/grafeno|$PREFIX/grafeno|" "$TMP/grafeno.service" > /etc/systemd/system/grafeno.service
    if command -v restorecon >/dev/null 2>&1; then
        restorecon /etc/systemd/system/grafeno.service 2>/dev/null || true
    fi
    if [ "$START" = 0 ]; then export GRAFENO_NO_START=1; fi
    GRAFENO_BIN="$PREFIX/grafeno" /usr/share/grafeno/setup.sh
    say "Token: sudo $PREFIX/grafeno config token"
else
    case ":$PATH:" in
        *":$PREFIX:"*) ;;
        *) say "Add $PREFIX to your PATH to run grafeno from anywhere." ;;
    esac
    say "Try it: grafeno shell --memory"
fi
