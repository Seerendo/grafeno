#!/bin/sh
# Stops and disables every instance's service: the packages run it when
# Grafeno is removed (not on upgrades). Settings and data stay.
[ -d /run/systemd/system ] || exit 0
for config in /etc/grafeno/*.toml; do
    [ -f "$config" ] || continue
    name="$(basename "$config" .toml)"
    if [ "$name" = grafeno ]; then unit=grafeno.service; else unit="grafeno@$name.service"; fi
    systemctl disable --now "$unit" >/dev/null 2>&1 || true
done
