#!/usr/bin/env bash
# Install an exact tested binary. Public firewall must remain disabled for first use.
# Updates stop the proxy and retain a verified pre-update backup before any migration.
set -euo pipefail
umask 077
[[ $# == 2 && $EUID == 0 ]] || {
  printf '%s\n' 'Usage: sudo bash activate-release.sh /absolute/path/photocraft-sync EXPECTED_SHA256' >&2; exit 2;
}
source_binary=$(realpath -e -- "$1")
expected=${2,,}
[[ "$expected" =~ ^[0-9a-f]{64}$ && -f "$source_binary" ]] || exit 2
[[ $(sha256sum -- "$source_binary" | cut -d ' ' -f1) == "$expected" ]] || {
  printf '%s\n' 'Binary hash mismatch; no service changes made.' >&2; exit 1;
}
mountpoint -q /var/lib/komaphone || { printf '%s\n' 'Data disk is not mounted.' >&2; exit 1; }
dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=$(cd -- "$dir" && sha256sum hosted-probe.py backup.sh upload-object.py komaphone.service komaphone-backup.service komaphone-backup.timer | sha256sum | cut -d ' ' -f1)
release=/opt/komaphone/releases/$expected-$bundle
[[ ! -e "$release" ]] || { printf '%s\n' 'Release directory already exists; inspect it instead of overwriting.' >&2; exit 1; }
[[ -f /etc/komaphone/service.env && -f /etc/komaphone/backup.env ]] || {
  printf '%s\n' 'Prepare service.env and backup.env from the runbook first.' >&2; exit 1;
}
"$source_binary" --help | grep -q -- '--google-client-id' || {
  printf '%s\n' 'Binary lacks hosted authentication; refusing activation.' >&2; exit 1;
}
install -d -o root -g root -m 0755 "$release"
install -o root -g root -m 0755 "$source_binary" "$release/photocraft-sync"
install -o root -g root -m 0755 "$dir/hosted-probe.py" "$dir/backup.sh" "$dir/upload-object.py" "$release/"
install -o root -g root -m 0644 "$dir/komaphone.service" "$dir/komaphone-backup.service" "$dir/komaphone-backup.timer" "$release/"
sha256sum "$release/photocraft-sync" "$release/hosted-probe.py" "$release/backup.sh" "$release/upload-object.py" \
  "$release/komaphone.service" "$release/komaphone-backup.service" "$release/komaphone-backup.timer" > "$release/SHA256SUMS"
# Keep the exact installed files before replacing units. Older releases did not
# retain their units, and operator configuration may differ from source templates.
# Environment files can contain credentials: this snapshot is root-only and is
# never printed, uploaded, or automatically restored against a migrated database.
rollback="$release/pre-update-config"
install -d -o root -g root -m 0700 "$rollback"
for config in /etc/komaphone/service.env /etc/komaphone/backup.env \
  /etc/systemd/system/komaphone.service /etc/systemd/system/komaphone-backup.service \
  /etc/systemd/system/komaphone-backup.timer; do
  if [[ -e "$config" || -L "$config" ]]; then
    [[ -f "$config" && ! -L "$config" ]] || {
      printf '%s\n' 'Rollback configuration must be a regular file; no service changes made.' >&2; exit 1;
    }
    install -o root -g root -m 0600 "$config" "$rollback/$(basename -- "$config")"
  fi
done
(cd -- "$rollback" && sha256sum -- ./* > SHA256SUMS)
systemctl stop caddy
systemctl stop komaphone.service 2>/dev/null || {
  systemctl list-unit-files komaphone.service --no-legend | grep -q komaphone && exit 1
  true
}
if [[ -L /opt/komaphone/current ]]; then
  prior=$(readlink -f /opt/komaphone/current)
  [[ "$prior" == /opt/komaphone/releases/* && -x "$prior/photocraft-sync" ]] || exit 1
  printf '%s\n' "$prior" > "$release/previous-release.txt"
  backup_dir=$(runuser -u komaphone -- mktemp -d /var/lib/komaphone/pre-update.XXXXXXXX)
  runuser -u komaphone -- "$prior/photocraft-sync" --data-dir /var/lib/komaphone --backup-to "$backup_dir/service.sqlite3"
  printf '%s\n' "$backup_dir" > "$release/pre-update-backup.txt"
fi
ln -s "$release" /opt/komaphone/current.next
mv -T /opt/komaphone/current.next /opt/komaphone/current
install -o root -g root -m 0644 "$release/komaphone.service" /etc/systemd/system/komaphone.service
install -o root -g root -m 0644 "$release/komaphone-backup.service" "$release/komaphone-backup.timer" /etc/systemd/system/
systemctl daemon-reload
if ! systemctl start komaphone.service; then
  systemctl stop komaphone.service
  printf '%s\n' 'Hosted startup failed. Proxy stays stopped. Preserve the database and pre-update backup; do not run an older binary on a migrated database.' >&2
  exit 1
fi
systemctl enable komaphone.service komaphone-backup.timer
systemctl start komaphone-backup.timer
printf '%s\n' 'Hosted readiness passed locally. Proxy remains stopped pending Caddy validation and public activation.'
