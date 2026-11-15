#!/usr/bin/env bash
# Two owner-authorized, single-use, 24-hour test admissions. No capability output.
set -euo pipefail
umask 077
[[ $# == 1 && $EUID == 0 && $(hostname -s) == komaphone-sync-1 && ${SUDO_UID:-} =~ ^[0-9]+$ ]] || exit 2
destination=$1
[[ "$destination" == /home/*/komaphone-k65/private-admissions-* && -d "$destination" && ! -L "$destination" ]] || exit 2
[[ $(stat -c %u "$destination") == "$SUDO_UID" && $(stat -c %a "$destination") == 700 ]] || exit 2
[[ ! -e "$destination/first.json" && ! -e "$destination/second.json" ]] || {
  printf '%s\n' 'Private admission output exists; do not mint replacements blindly.' >&2; exit 1;
}
was_public=false
if systemctl is-active --quiet caddy; then was_public=true; fi
resume() {
  result=$?
  trap - EXIT
  if systemctl start komaphone.service && python3 /opt/komaphone/current/hosted-probe.py >/dev/null; then
    if [[ "$was_public" == true ]]; then systemctl start caddy || result=1; fi
  else
    result=1
  fi
  exit "$result"
}
trap resume EXIT
systemctl stop caddy
systemctl stop komaphone.service
set -o noclobber
for name in first second; do
  runuser -u komaphone -- /opt/komaphone/current/photocraft-sync --data-dir /var/lib/komaphone --create-admission --admission-hours 24 > "$destination/$name.json"
  chmod 0600 "$destination/$name.json"
  chown "$SUDO_UID:$SUDO_GID" "$destination/$name.json"
done
printf '%s\n' 'Created two single-use 24-hour test admissions in private files. Values were not logged.'
