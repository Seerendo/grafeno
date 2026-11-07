# $1 is 0 when the package is removed (1 on upgrades).
if [ "$1" -eq 0 ]; then
    /usr/share/grafeno/stop-all.sh
fi
