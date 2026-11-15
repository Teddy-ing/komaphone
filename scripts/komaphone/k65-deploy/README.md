# K6.5 deployment assets

Start with the [setup/cost/recovery plan and observed deployment ledger](../../../docs/komaphone/k65-hosted-setup.md). The reviewed initial resources were deployed with owner authorization on2026-11-13; do not rerun initial provisioning against the existing resources.

- `gcp-resources.sh`: pure printed plan by default; explicitly approved initial provisioning only. Names project `komaphone` on every gcloud call, never changes account defaults, refuses existing named resources and leaves public web access disabled.
- `gcp-windows.ps1`: invokes that same script through Git for Windows Bash and the installed Windows SDK/bundled Python, with profiles disabled and temporary environment restored. Defaults to `-Mode Plan`; `-Mode Apply` is reserved for the authorized deployment after authentication checks pass.
- `prepare-vm.sh`: checks the separately named20 GiB data disk before initialization/mount, rejects foreign signatures/partitions, creates a dedicated user. Does not launch services.
- `install-vm-packages.sh`, `configure-vm.sh`, `journald-komaphone.conf`: signed vendor packages, public client-ID configuration, bounded logs and private release activation on the named dedicated VM.
- `activate-release.sh`: verifies exact binary hash, retains binary/helpers and all three hashed systemd units in an immutable release, captures prior installed units/environment files in root-only `pre-update-config`, stops public proxy, preserves pre-update database backup, then starts the hosted service. Failure leaves public proxy stopped. No blind old-schema rollback. Configuration snapshots can contain credentials; never distribute them with a client package or ordinary logs.
- `komaphone.service`, `hosted-probe.py`: hosted-only configuration and bounded health/client-ID check. Readiness does not prove authorization tests or actual Google login.
- `Caddyfile`: public HTTPS termination and bounded proxy. Caddy certificate data survives application updates; no access logging.
- `backup.sh`, backup unit/timer, lifecycle JSON: verified snapshot, generation-conditional unique uploads, completion manifest last, bounded retained staging. Operator must test off-VM fresh restore and quarantine/reconcile restored access.
- `upload-object.py`, `test-upload.py`: bounded streaming conditional object creation using in-memory metadata credentials; avoids the CLI's object-read preflight without broadening uploader IAM.
- `verify-live.py`, `inspect-public.py`: non-enrolling live authorization denials, ordinary public certificate/redirect/port observations.
- `verify-backup.py`, `restore-drill.py`: completion/hash refusal and fresh isolated restore/content/access verification. Empty snapshots do not prove recovery of accepted art.
- `content-receipt.py`, `test-content-receipt.py`: read-only ten-table artwork/media/replay receipts, accepted export-image and canonical canvas-hash matching, plus exact restore-time room restart validation. The populated drill requires `--expectations /private/acknowledged.json --require-populated`; no authority/token values are emitted.
- `issue-test-admissions.sh`: explicitly authorized two-account test enrollment helper; only private24-hour one-use output files, orderly service restart and no capability logging.
- `tls-staging-renewal.py` and staging templates: explicitly authorized, bounded staging issuance plus same-store managed renewal, isolated from production certificate data, with exact-config rollback. Requires the reviewed current production configuration; never schedule unattended or rerun an unexplained failure. `tls-staging-cleanup.py` verifies an existing completed run without another issuance. `test_tls_listener.py` distinguishes a listener from closed TCP TIME_WAIT sockets.
- `test_deploy.py`: local safety tests with fake gcloud and loopback HTTP; no cloud account access.
- `test-rollback-preservation.py`: temporary-filesystem activation checks for prior configuration preservation, failed startup/backup and symlink refusal; service controls and backup content are synthetic.
- `rollback-drill.py`: pinned real Linux service and synthetic loopback drawings; verifies incompatible-schema refusal, original/backup preservation, fresh SQLite restoration and session/invitation quarantine. No Google login, systemd, cloud access or real release-to-release migration is claimed.
- `validate-config.py`: validates Caddy and copied systemd unit syntax with temporary executable substitutions; does not install/start services.

Run preparation checks in Linux:

```bash
python3 scripts/komaphone/k65-deploy/test_deploy.py
python3 scripts/komaphone/k65-deploy/validate-config.py --caddy /absolute/path/to/verified/caddy
bash scripts/komaphone/k65-deploy/gcp-resources.sh --plan
```

Provisioning, DNS changes, installation/reboot, public certificate issuance/renewal and off-VM restore remain separately authorized/executed operations. Preserve logs and exact artifact hashes for acceptance.
