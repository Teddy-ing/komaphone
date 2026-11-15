# K6 deployment preparation

Use the [GCP setup and cost plan](../../../docs/komaphone/k6-gcp-setup.md). These files do not provision anything or establish hosted readiness.

- `resource-plan.json`: explicit project, resources, hostname, OAuth client, cost assumptions and remaining inputs. The OAuth client ID is public; credentials never belong here.
- `Caddyfile`: public trusted-HTTPS proxy to the existing Rust loopback service. Requires an authorized DNS hostname and deployed IP. No access logging or public admin port.
- `komaphone.service`: dedicated-user systemd supervision with the current service's SIGINT shutdown, explicit data mount dependency and resource bounds.
- `journald-komaphone.conf`: bounded **whole-VM** journald policy; review other services before installation.
- `backup.sh`, `komaphone-backup.service`, `komaphone-backup.timer`, `backup-lifecycle.json`: consistent verified SQLite/art backup, unique private GCS uploads, daily scheduling and bounded retention. Supply `KOMAPHONE_BACKUP_BUCKET` in `/etc/komaphone/backup.env`; it contains the bucket name, not credentials. The metadata-based VM identity gets bucket-only object creation. No GCS upload has been tested in K6.
- `linux-build.sh`: service-only Linux build and tests, Rust1.95 and a native C toolchain required; honors an isolated `CARGO_TARGET_DIR`.
- `linux-smoke.py`: real isolated Linux service + Caddy TLS + concurrent canvas workload, crash restart and fresh backup restoration. Python orchestrates the test; product code remains Rust.

Run the local fixture in Linux with actual built binaries:

```sh
python3 scripts/komaphone/k6-deploy/linux-smoke.py \
  --service /absolute/path/to/photocraft-sync \
  --caddy /absolute/path/to/caddy \
  --output /absolute/path/to/isolated-evidence
```

The fixture creates a new UUID directory, picks loopback ports, uses a private test CA only in that client's SSL context, and cleans up only its child processes. It never changes system trust or reads personal profiles. It checks the service's non-loopback plaintext refusal, explicit certificate trust, 2/4/8 writer fixtures, equal authorized event streams, post-acknowledgment SIGKILL recovery, restored membership denials, CPU/RSS and JSON payload bytes. It retains all test databases and logs for inspection. Local CA success is not public-certificate issuance, renewal, real Google login or cross-household play.

K6.5 must verify the public admission policy before exposing the existing RPC interface, attach the data disk with auto-delete disabled, verify startup after reboot, deploy the exact tested artifacts, inspect real budget alerts, exercise off-VM fresh restore/revocation reconciliation, and record actual billing/traffic. Never run the backup-retention or rollback steps against accepted art without the documented recovery checks.
