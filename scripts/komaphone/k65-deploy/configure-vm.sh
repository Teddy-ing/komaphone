#!/usr/bin/env bash
# Install public configuration and a pinned release on the approved dedicated VM.
set -euo pipefail
[[ $# == 2 && $EUID == 0 && $(hostname -s) == komaphone-sync-1 ]] || {
  printf '%s\n' 'Usage on approved VM: sudo bash configure-vm.sh /absolute/service/binary EXPECTED_SHA256' >&2; exit 2;
}
dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
mountpoint -q /var/lib/komaphone
systemctl stop caddy
install -d -o root -g root -m 0755 /etc/komaphone /etc/systemd/journald.conf.d
printf '%s\n' 'KOMAPHONE_GOOGLE_CLIENT_ID=981926394354-07f4s3dss4vgp7mteh0qe8e6dhk3s99o.apps.googleusercontent.com' > /etc/komaphone/service.env
printf '%s\n' 'KOMAPHONE_BACKUP_BUCKET=komaphone-k65-backups-20261112' > /etc/komaphone/backup.env
chown root:root /etc/komaphone/service.env /etc/komaphone/backup.env
chmod 0644 /etc/komaphone/service.env /etc/komaphone/backup.env
install -o root -g root -m 0644 "$dir/journald-komaphone.conf" /etc/systemd/journald.conf.d/komaphone.conf
systemctl restart systemd-journald
install -o root -g root -m 0644 "$dir/Caddyfile" /etc/caddy/Caddyfile
caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
bash "$dir/activate-release.sh" "$1" "$2"
systemd-analyze verify /etc/systemd/system/komaphone.service /etc/systemd/system/komaphone-backup.service /etc/systemd/system/komaphone-backup.timer
printf '%s\n' 'Hosted service is ready on loopback. Caddy remains stopped; public firewall remains a separate step.'
