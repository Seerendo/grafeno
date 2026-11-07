#!/usr/bin/env bash
# Renders grafeno-mark.svg (the mark of the docs site, grafeno-docs
# static/img/grafeno-mark.svg) into grafeno.ico: the icon of grafeno.exe,
# the Windows setup, its uninstaller and its shortcuts. Sizes 16 to 256.
#
#   packaging/windows/make-icon.sh
#
# Needs Docker (librsvg and ImageMagick run in a container).
set -euo pipefail
export MSYS_NO_PATHCONV=1
here="$(cd "$(dirname "$0")" && pwd)"
if command -v cygpath >/dev/null 2>&1; then mount="$(cygpath -w "$here")"; else mount="$here"; fi
docker run --rm -v "$mount:/w" debian:trixie sh -c '
    set -e
    apt-get update -qq >/dev/null
    apt-get install -y -qq librsvg2-bin imagemagick >/dev/null 2>&1
    cd /tmp
    for size in 16 20 24 32 40 48 64 128 256; do
        rsvg-convert -w "$size" -h "$size" /w/grafeno-mark.svg -o "mark-$size.png"
    done
    convert mark-16.png mark-20.png mark-24.png mark-32.png mark-40.png mark-48.png \
        mark-64.png mark-128.png mark-256.png /w/grafeno.ico
    identify /w/grafeno.ico
    cp mark-256.png /w/grafeno-mark-256.png
'
