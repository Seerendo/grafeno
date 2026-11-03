# $1 is 0 when the package is removed (1 on upgrades).
if [ "$1" -eq 0 ] && [ -d /run/systemd/system ]; then
    systemctl disable --now grafeno.service >/dev/null 2>&1 || true
fi
