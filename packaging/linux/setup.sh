#!/bin/sh
# Shared post-install steps of the .deb and .rpm packages (and of
# install.sh --service): the system user and folders, the AppArmor profile
# or SELinux module, then on a first install the default instance
# (`grafeno setup`: settings with a fresh token, the service enabled and
# started), and on an upgrade a restart of every running instance (each
# keeps a copy of its database if the new version upgrades its layout).
# Safe to run again.
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

# Confinement, where the system has it loaded.
if [ -f /etc/apparmor.d/grafeno ] && command -v apparmor_parser >/dev/null 2>&1 \
    && [ -d /sys/kernel/security/apparmor ]; then
    apparmor_parser -r -W /etc/apparmor.d/grafeno >/dev/null 2>&1 \
        || echo "Grafeno: could not load the AppArmor profile; the service runs unconfined."
fi
if [ -f /usr/share/selinux/packages/grafeno.pp ] && command -v selinuxenabled >/dev/null 2>&1 \
    && selinuxenabled; then
    semodule -i /usr/share/selinux/packages/grafeno.pp >/dev/null 2>&1 || true
    if command -v semanage >/dev/null 2>&1; then
        semanage port -a -t grafeno_port_t -p tcp 7810-7819 >/dev/null 2>&1 || true
    fi
    restorecon -R "$GRAFENO_BIN" /etc/grafeno /var/lib/grafeno >/dev/null 2>&1 || true
fi

systemd=0
[ -d /run/systemd/system ] && systemd=1
[ "$systemd" = 1 ] && systemctl daemon-reload

if [ ! -f "$CONFIG" ]; then
    # A first install: the default instance, with the token (and no
    # administrator yet: `grafeno users create` makes one).
    start=""
    [ "${GRAFENO_NO_START:-0}" = 1 ] && start="--no-start"
    "$GRAFENO_BIN" setup --instance default --unattended --no-admin --quiet $start
    chown root:grafeno "$CONFIG" && chmod 0640 "$CONFIG"
    echo "Grafeno: created the default instance ($CONFIG, a new token: sudo grafeno config token)."
    if [ "$systemd" = 1 ]; then
        if [ -n "$start" ]; then
            echo "Grafeno: the service is enabled; start it with: systemctl start grafeno"
        else
            echo "Grafeno: the service is running on 127.0.0.1:7810 (grafeno doctor checks it)."
        fi
    else
        echo "Grafeno: systemd is not running; start the server with:"
        echo "  sudo -u grafeno $GRAFENO_BIN serve --config $CONFIG"
    fi
    echo "Grafeno: more instances with: sudo grafeno setup --instance <name>"
elif [ "$systemd" = 1 ]; then
    # An upgrade: the running instances move to the new version.
    for config in /etc/grafeno/*.toml; do
        [ -f "$config" ] || continue
        name="$(basename "$config" .toml)"
        if [ "$name" = grafeno ]; then unit=grafeno.service; else unit="grafeno@$name.service"; fi
        systemctl try-restart "$unit" >/dev/null 2>&1 || true
    done
    echo "Grafeno: restarted the running instances (grafeno doctor checks them)."
fi
