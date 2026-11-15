#!/usr/bin/env bash
# Consistent database/assets backup, uploaded with the VM's bucket-scoped identity.
# Run as komaphone after reviewing the configured bucket and testing fresh restore.
set -euo pipefail
umask 077
: "${KOMAPHONE_BACKUP_BUCKET:?Set the private GCS bucket name, without gs://}"
if [[ ! "$KOMAPHONE_BACKUP_BUCKET" =~ ^[a-z0-9][a-z0-9._-]{1,220}[a-z0-9]$ ]]; then
  printf '%s\n' 'Invalid backup bucket name' >&2
  exit 2
fi
exec 9>/var/lib/komaphone/backup.lock
flock -n 9 || { printf '%s\n' 'Another backup is running' >&2; exit 75; }
stages=$(find /var/lib/komaphone -maxdepth 1 -type d -name 'backup-staging.*' -printf '. ' | wc -w)
if (( stages >= 2 )); then
  printf '%s\n' 'Two interrupted backup staging directories remain; inspect/recover them before retrying. Live artwork is unchanged.' >&2
  exit 1
fi
stage=$(mktemp -d /var/lib/komaphone/backup-staging.XXXXXXXX)
cleanup() {
  # These exact generated files are our temporary copies; the live DB is untouched.
  rm -f -- "$stage/service.sqlite3" "$stage/service.sqlite3.sha256"
  rmdir -- "$stage" || true
}
trap cleanup EXIT
/opt/komaphone/current/photocraft-sync --data-dir /var/lib/komaphone --backup-to "$stage/service.sqlite3"
(cd "$stage" && sha256sum service.sqlite3 > service.sqlite3.sha256)
object="backups/$(date -u +%Y%m%dT%H%M%SZ)-${stage##*.}"
# Enforce creation atomically; --no-clobber can issue GETs unavailable to Object Creator.
export CLOUDSDK_STORAGE_PARALLEL_COMPOSITE_UPLOAD_ENABLED=false
gcloud storage cp --if-generation-match=0 "$stage/service.sqlite3" "gs://$KOMAPHONE_BACKUP_BUCKET/$object/service.sqlite3"
gcloud storage cp --if-generation-match=0 "$stage/service.sqlite3.sha256" "gs://$KOMAPHONE_BACKUP_BUCKET/$object/service.sqlite3.sha256"
printf '%s\n' 'Verified local backup uploaded; scheduled fresh restore remains required.'
