#!/bin/sh
# Installs Grafeno on Linux from the release archives.
#
#   curl -fsSL https://raw.githubusercontent.com/Seerendo/grafeno/main/install.sh | sh
#   curl -fsSL …/install.sh | sudo sh -s -- --service     # also run it as a systemd service
#
# Options:
#   --version vX.Y.Z   a specific release (default: the latest)
#   --prefix DIR       where to put the binary (default: /usr/local/bin as root,
#                      ~/.local/bin otherwise)
#   --service          create the grafeno user, /etc/grafeno/grafeno.toml and the
#                      systemd service (needs root)
#
# GRAFENO_BASE_URL overrides where releases are downloaded from (mirrors, tests).

set -eu

REPO="Seerendo/grafeno"
BASE_URL="${GRAFENO_BASE_URL:-https://github.com/$REPO/releases}"
VERSION="latest"
PREFIX=""
SERVICE=0

say() { printf '%s\n' "$*"; }
fail() { printf 'grafeno install: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --prefix) PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --service) SERVICE=1; shift ;;
        -h|--help) sed -n '2,17p' "$0" 2>/dev/null || true; exit 0 ;;
        *) fail "unknown option $1" ;;
    esac
done

[ "$(uname -s)" = "Linux" ] || fail "this script installs on Linux; on Windows use the setup .exe"
case "$(uname -m)" in
    x86_64|amd64) TARGET="x86_64-unknown-linux-musl" ;;
    aarch64|arm64) TARGET="aarch64-unknown-linux-musl" ;;
    *) fail "no release for $(uname -m) yet" ;;
esac

IS_ROOT=0
[ "$(id -u)" = "0" ] && IS_ROOT=1
if [ "$SERVICE" = 1 ] && [ "$IS_ROOT" = 0 ]; then
    fail "--service needs root: run it with sudo"
fi
if [ "$SERVICE" = 1 ] && ! command -v systemctl >/dev/null 2>&1; then
    fail "--service needs systemd; without it, install without --service and run grafeno serve"
fi
if [ -z "$PREFIX" ]; then
    if [ "$IS_ROOT" = 1 ]; then PREFIX="/usr/local/bin"; else PREFIX="$HOME/.local/bin"; fi
fi

ARCHIVE="grafeno-$TARGET.tar.gz"
if [ "$VERSION" = "latest" ]; then
    URL="$BASE_URL/latest/download/$ARCHIVE"
else
    URL="$BASE_URL/download/$VERSION/$ARCHIVE"
fi

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        fail "curl or wget is needed"
    fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "Downloading $URL"
download "$URL" "$TMP/$ARCHIVE" || fail "cannot download $URL"
if download "$URL.sha256" "$TMP/$ARCHIVE.sha256" 2>/dev/null; then
    expected="$(cut -d ' ' -f 1 "$TMP/$ARCHIVE.sha256")"
    actual="$(sha256sum "$TMP/$ARCHIVE" | cut -d ' ' -f 1)"
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $ARCHIVE"
    say "Checksum verified."
fi

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
mkdir -p "$PREFIX"
install -m 0755 "$TMP/grafeno" "$PREFIX/grafeno"
say "Installed $("$PREFIX/grafeno" --version) to $PREFIX/grafeno"

if [ "$SERVICE" = 1 ]; then
    install -d /usr/share/grafeno
    install -m 0755 "$TMP/setup.sh" /usr/share/grafeno/setup.sh
    sed "s|/usr/bin/grafeno|$PREFIX/grafeno|" "$TMP/grafeno.service" > /etc/systemd/system/grafeno.service
    GRAFENO_BIN="$PREFIX/grafeno" /usr/share/grafeno/setup.sh
    say "Token: sudo $PREFIX/grafeno config token"
else
    case ":$PATH:" in
        *":$PREFIX:"*) ;;
        *) say "Add $PREFIX to your PATH to run grafeno from anywhere." ;;
    esac
    say "Try it: grafeno shell --memory"
fi
