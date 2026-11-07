if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi
# Removed (not upgraded): drop the SELinux module. Settings and data stay
# (`grafeno instances drop <name> --purge` removes an instance's data).
if [ "$1" -eq 0 ] && command -v selinuxenabled >/dev/null 2>&1 && selinuxenabled; then
    semodule -r grafeno >/dev/null 2>&1 || true
fi
