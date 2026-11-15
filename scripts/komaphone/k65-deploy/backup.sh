#!/usr/bin/env bash
# Verified DB+asset snapshot; immutable private GCS objects; manifest uploaded last.
set -euo pipefail
umask 077
: "${KOMAPHONE_BACKUP_BUCKET:?Set private bucket name in /etc/komaphone/backup.env}"
[[ "$KOMAPHONE_BACKUP_BUCKET" =~ ^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$ ]] || exit 2
mountpoint -q /var/lib/komaphone || exit 1
exec 9>/var/lib/komaphone/backup.lock
flock -n 9 || { printf '%s\n' 'Another backup is running.' >&2; exit 75; }
stages=$(find /var/lib/komaphone -maxdepth 1 -type d -name 'backup-staging.*' -printf '. ' | wc -w)
(( stages < 2 )) || { printf '%s\n' 'Two interrupted staging copies remain; inspect them before retrying. Live artwork is unchanged.' >&2; exit 1; }
stage=$(mktemp -d /var/lib/komaphone/backup-staging.XXXXXXXX)
/opt/komaphone/current/photocraft-sync --data-dir /var/lib/komaphone --backup-to "$stage/service.sqlite3"
python3 - "$stage" <<'PY'
import datetime, hashlib, json, pathlib, sys
stage = pathlib.Path(sys.argv[1])
def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()
database = stage / 'service.sqlite3'
manifest = {'format': 1, 'created_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'database_sha256': digest(database), 'database_bytes': database.stat().st_size,
            'service_sha256': digest(pathlib.Path('/opt/komaphone/current/photocraft-sync')),
            'access_restore_policy': 'invalidate-restored-access-and-reconcile-live-revocations-before-exposure'}
(stage / 'manifest.json').write_text(json.dumps(manifest, sort_keys=True) + '\n', encoding='utf-8')
PY
object="backups/$(date -u +%Y%m%dT%H%M%SZ)-${stage##*.}"
# Direct conditional creates avoid gcloud cp's object-read preflight. No IAM expansion.
python3 /opt/komaphone/current/upload-object.py "$stage/service.sqlite3" "$KOMAPHONE_BACKUP_BUCKET" "$object/service.sqlite3"
python3 /opt/komaphone/current/upload-object.py "$stage/manifest.json" "$KOMAPHONE_BACKUP_BUCKET" "$object/manifest.json"
# Only these exact generated temporary files are removed, and only after both uploads.
rm -- "$stage/service.sqlite3" "$stage/manifest.json"
rmdir -- "$stage"
printf 'Backup uploaded: gs://%s/%s/manifest.json; fresh restore verification remains required.\n' "$KOMAPHONE_BACKUP_BUCKET" "$object"
