#!/bin/sh
# Nightly backup of every hub, plus gateway.db and platform.db when present, to R2.
# Install from cron (root) or a systemd timer, see deploy/README.md:
#   17 3 * * * /srv/chefslegacy/deploy/backup.sh >> /var/log/chefs-backup.log 2>&1
set -eu
ROOT="${HUBCTL_ROOT:-/srv/chefslegacy}"
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) backup start"
python3 -I "$ROOT/deploy/hubctl" --root "$ROOT" backup all "$@"
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) backup done"
