#!/bin/sh
# Shared post-install steps of the .deb and .rpm packages (and of
# install.sh --service): a system user, a data folder, a settings file with
# a fresh token, and the systemd service. Safe to run again on upgrades.
set -e

GRAFENO_BIN="${GRAFENO_BIN:-/usr/bin/grafeno}"
CONFIG=/etc/grafeno/grafeno.toml

if ! getent passwd grafeno >/dev/null 2>&1; then
    nologin=/usr/sbin/nologin
    [ -x "$nologin" ] || nologin=/sbin/nologin
    useradd --system --home-dir /var/lib/grafeno --no-create-home --shell "$nologin" \
        --user-group grafeno
fi
install -d -o grafeno -g grafeno -m 0750 /var/lib/grafeno
install -d -o root -g grafeno -m 0750 /etc/grafeno

if [ ! -f "$CONFIG" ]; then
    "$GRAFENO_BIN" config init --path "$CONFIG" --db /var/lib/grafeno/grafeno.redb >/dev/null
    chown root:grafeno "$CONFIG"
    chmod 0640 "$CONFIG"
    echo "Grafeno: created $CONFIG with a new token (sudo grafeno config token)."
fi

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    systemctl enable grafeno.service >/dev/null 2>&1 || true
    systemctl restart grafeno.service || true
    echo "Grafeno: the service is running. Studio: http://127.0.0.1:7810/studio/"
else
    echo "Grafeno: systemd is not running; start the server with:"
    echo "  sudo -u grafeno $GRAFENO_BIN serve --config $CONFIG"
fi
