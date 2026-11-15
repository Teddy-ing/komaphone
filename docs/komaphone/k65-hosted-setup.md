# K6.5 hosted setup and operations

Prepared 2026-11-12 and executed with owner authorization on2026-11-13 for [baseline 1.4](product-spec.md) and the [hosting contract](hosting-plan.md). **The hosted service is now live at `https://komaphone.tedsoftware.org` (`35.188.222.249`)**, with observed deployment, native play and recovery evidence below. Milestone disposition and remaining follow-ups are tracked in [status](status.md); the latest owner-approved independent-public-network scope is recorded at the end of this guide. Earlier pending statements are historical checkpoints. The [K6 setup record](k6-gcp-setup.md) preserves earlier Linux feasibility measurements.

**Current Linux deployment candidate:** `log/k65/deployment/linux-20261113T192233Z/bin/photocraft-sync`, SHA256 `d30065ddafc5d0b7b770692210031803b477e9259a227fb80f597554ba6ca998`. This supersedes the earlier96-test artifact retained in the historical checkpoints below. It includes the Google-only People directory and stroke-copy authorization fixes; its98-test/Linux runtime evidence is recorded at the end of this guide.

## Existing setup and the next owner steps

The existing project is **`komaphone`**, with billing linked, the domain **`komaphone.tedsoftware.org`** at Namecheap, and Google Desktop OAuth client **`981926394354-07f4s3dss4vgp7mteh0qe8e6dhk3s99o.apps.googleusercontent.com`**. Do not create replacements or change the unrelated default project in an installed gcloud configuration. Every command below names the project explicitly. The client ID is public; provider secrets, account tokens, invitation capabilities and payment details are not handoff material.

The owner confirmed the requested$20/$30 project budgets and credits are ready, enabled Compute Engine, and reported adding alternate Google test accounts. Root's read-only CLI verification subsequently confirmed Compute enabled, no instances/addresses/disks/buckets, and sufficient initial US-central1 E2/IP quota. The prior reported credit was $78.93 with expiry 2027-04-29; its *current amount, exact eligible products and expiry* have not been independently read. Budget setup is settled owner-reported evidence; detailed all-service/gross-credit filters and notification delivery remain independently uninspected. Do not ask the owner to repeat setup or reapprove the agreed thresholds. Resource creation was separately authorized as recorded below.

Use small interactive steps, with the agent continuing independent implementation between replies:

1. **Compute Engine is enabled.** Do not repeat this completed step. Creating an instance remains a separate, specifically authorized deployment step.
2. **Budget setup is owner-confirmed.** The requested$20/$30 project warnings and credits are already ready. Later independent inspection can document all-service/calendar-month scope, exclusion of promotional credits and notification delivery; it is not a request to repeat setup. One$20 budget with100%/150% thresholds or two budgets can supply the agreed warnings. A warning does not cap spending or stop the VM. [Google budget guidance](https://docs.cloud.google.com/billing/docs/how-to/budgets).
3. **The owner authorized this exact inventory/cost on2026-11-13; the coordinating agent confirmed real Google authentication gates passed and authorized execution.** The inventory below is now created. Do not repeat approval or rerun initial provisioning against it.
4. **The owner saved the Namecheap `komaphone` A record as `35.188.222.249`.** Authoritative and public DNS were verified. Preserve `@`, `www`, mail and all unrelated records; no AAAA record is part of this IPv4-only deployment.
5. **Independent-public-network acceptance scope is owner-approved.** Home broadband plus a laptop on a phone's cellular hotspot, with the phone's Wi-Fi uplink off, satisfies the K6.5 network gate. Same-LAN or VPN-only results cannot substitute. Fresh friend onboarding in another household remains K7 work; the current evidence does not claim that onboarding test occurred.

## Exact resource inventory

| Resource | Selected configuration |
|---|---|
| Project / region / zone | `komaphone` / `us-central1` / `us-central1-a` |
| VM | `komaphone-sync-1`, regular non-Spot `e2-small`, Debian 12, 2 GiB RAM, Shielded VM, deletion protection |
| Boot disk | 10 GiB `pd-standard`; no accepted art on the boot disk |
| Data disk | `komaphone-data-1`, 20 GiB `pd-standard`, device name `komaphone-data`, **auto-delete disabled** |
| Data mount / service quota | `/var/lib/komaphone` mounted by UUID; initial accepted-service growth quota 2 GiB |
| Network | Dedicated `komaphone-private` VPC; `komaphone-us-central1` subnet `10.42.0.0/24` |
| External address | One static regional Premium IPv4, `komaphone-ip` |
| Firewall | `komaphone-iap-ssh`: TCP22 from `35.235.240.0/20`; `komaphone-web`: TCP80/443, initially **disabled**, both target tag `komaphone-sync` |
| Admin access | IAP tunnel + OS Login; no public SSH, editor control or port43127 |
| VM identity | `komaphone-backup@komaphone.iam.gserviceaccount.com`; no project-wide role; bucket-only `roles/storage.objectCreator`; metadata credentials, no downloaded key |
| Backup bucket | `komaphone-k65-backups-20261112`, private Standard regional `us-central1`, uniform access, public access prevention, 7-day soft delete |
| Backup retention | Daily unique full verified snapshot; delete `backups/` objects after8 days; plan32 GiB including soft-deleted copies and lifecycle lag |
| HTTPS | Caddy on80/443; persistent certificate state under its packaged service account; upstream Rust on127.0.0.1:43127 |
| Runtime bounds | Service MemoryHigh768 MiB/MemoryMax1 GiB, backup MemoryMax512 MiB; bounded local journald, no Ops Agent/paid central logging by default |
| DNS | Existing Namecheap subdomain record; no Cloud DNS zone or domain purchase |

No load balancer, Cloud NAT, Kubernetes, managed database, paid identity platform or fleet is included. Invited-member4096 and presence256 defaults are operational limits from K6, not four-person canvas restrictions. Party rooms remain two/three players. E2 capacity must be measured on the VM; the local Linux result (service15.65 MiB, proxy48.87 MiB observed peak during700 writes) is a useful starting point, not a full-quota or long-running load guarantee.

The provisioning operator needs permission to enable services and create the listed Compute/network/IAM/bucket resources. OS Login/IAP administrator access can require `roles/compute.osAdminLogin`, `roles/iap.tunnelResourceAccessor` and permission to act as the attached VM service account; use the existing owner authorization where it suffices. Do not give the VM account Editor, project-wide Storage Admin or human billing privileges. Restore reads use a separately authorized administrator. [Google IAP guidance](https://docs.cloud.google.com/iap/docs/using-tcp-forwarding).

## Refreshed cost and limits

USD,730 hours/month, regular US-central1 Linux rates refreshed from official pricing pages on2026-11-13. Values charge ordinary usage **before credits and free allowances**; rounded totals are planning estimates. They exclude unknown jurisdiction-dependent tax. The existing domain's $1/month allocation remains provisional until its actual renewal cost/share is known.

| Item | Basis | Light use | Higher use |
|---|---|---:|---:|
| e2-small | $0.016752855/hour ×730 | $12.23 | $12.23 |
| In-use external IPv4 | $0.005/hour ×730 | $3.65 | $3.65 |
| Boot + data disk | 30 GiB ×$0.04/month | $1.20 | $1.20 |
| Standard GCS backups | 32 GiB ×$0.02/month | $0.64 | $0.64 |
| Upload/manifest/admin requests | Conservative small-request allowance | $0.01 | $0.01 |
| Premium egress to North America | 5/50 GiB ×$0.12 | $0.60 | $6.00 |
| Local logs and built-in platform metrics | No central agent or log export | $0.00 | $0.00 |
| DNS record | Existing provider, no Cloud DNS | $0.00 | $0.00 |
| **Incremental cloud** | Before tax/credits | **$18.33** | **$23.73** |
| Existing-domain allocation | Provisional $12/year share | $1.00 | $1.00 |
| **Total including provisional domain share** | Before tax/credits | **$19.33** | **$24.73** |

Sources: [E2 VM pricing](https://cloud.google.com/products/compute/pricing/general-purpose), [IPv4 and network](https://cloud.google.com/vpc/network-pricing), [persistent disk](https://cloud.google.com/compute/disks-image-pricing), [GCS storage and requests](https://cloud.google.com/storage/pricing). No new logging service is required; optional1 GiB central ingestion would add a conservative$0.50 before its free allowance under [Observability pricing](https://cloud.google.com/products/observability/pricing). Current pricing pages may display applicable free tiers; this table deliberately does not rely on unused account-wide allowances.

At100 GiB/month premium egress, the projected total is about**$30.73 before tax**, including provisional domain allocation: investigate traffic and reassess before growing there. Remote restore downloads also incur egress; same-region VM restore should be preferred. Soft-deleted objects remain billable, and lifecycle deletion is asynchronous; daily inventory must confirm actual retained bytes instead of assuming32 GiB is a hard ceiling. [Lifecycle](https://docs.cloud.google.com/storage/docs/lifecycle) and [soft-delete behavior](https://docs.cloud.google.com/storage/docs/soft-delete). Extra pre-update backups and interrupted local staging consume data-disk space and need an operator review; do not prune accepted art automatically.

Google sign-in uses the existing OAuth client, not paid Identity Platform. The credit offsets only eligible costs and expires; domain renewals, taxes and exclusions may still bill. Do not buy/renew a subscription for this deployment. Record an actual Billing report after the acceptance workload with the observation window, bytes and projected run rate; a few hours cannot establish a monthly bill.

## Review, provision and install

The [deployment directory](../../scripts/komaphone/k65-deploy/gcp-resources.sh) is self-contained. `gcp-resources.sh --plan` prints commands without calling gcloud or reading credentials. It always names the project and never changes gcloud defaults. In Cloud Shell, from the uploaded deployment directory:

```bash
bash gcp-resources.sh --plan
```

After specific deployment approval and API readiness:

```bash
KOMAPHONE_APPROVED_PROJECT=komaphone bash gcp-resources.sh --apply
```

This enables required APIs, checks for existing named Komaphone resources, and creates the exact inventory above. It leaves public80/443 **disabled** and installs no app. It refuses to adopt an existing resource; a failed partial run is inspected and resumed with the remaining printed commands. Do not delete/recreate resources to make a retry pass. A globally occupied bucket name fails first, before VM/IP allocation; select a documented unused suffix and update both configuration and inventory if needed.

On the current Windows workstation, [gcp-windows.ps1](../../scripts/komaphone/k65-deploy/gcp-windows.ps1) runs this same Bash script through installed Git for Windows, using the installed Windows Google Cloud SDK and its bundled Python. It does not copy credentials into WSL or change gcloud defaults. The wrapper restores its temporary environment in `finally` and passes arguments directly with Bash profiles disabled:

```powershell
# Print only; no gcloud calls.
& scripts/komaphone/k65-deploy/gcp-windows.ps1 -Mode Plan
# Only after the authentication gate passes; the owner already approved this inventory.
& scripts/komaphone/k65-deploy/gcp-windows.ps1 -Mode Apply
```

The route was checked on2026-11-13 with installed SDK574.0.0: Git Bash successfully invoked `gcloud --project=komaphone --quiet compute instances list --format=json` and returned an empty list. Wrapper syntax, printed plan and null-safe environment restoration checks pass; plan output is retained at `log/k65/deployment/resume-20261113/windows-resource-plan.txt`. The first environment-comparison test incorrectly passed null array values to `Compare-Object`; its result is not counted. The corrected comparison passed. The execution sandbox requires process/credential filesystem escalation for Git Bash/SDK, distinct from the owner's already granted resource authorization. **Apply was not run by these preparation checks.**

Connect through IAP and copy only the source-pinned Linux service, deployment files and expected SHA256:

```bash
gcloud --project=komaphone compute ssh komaphone-sync-1 --zone=us-central1-a --tunnel-through-iap
```

Install the official Debian Caddy package and Google Cloud CLI using their signed package repositories, plus `python3`, `curl` and `util-linux`. Follow [Caddy Debian installation](https://caddyserver.com/docs/install#debian-ubuntu-raspbian) and [Google Cloud CLI installation](https://docs.cloud.google.com/sdk/docs/install-sdk#deb); record package versions. Stop Caddy after installation until configuration is verified. Do not run `gcloud auth login` on the VM or copy administrator credentials there; the backup service uses the attached account's metadata identity. The release is built off-VM, not compiled within2 GiB RAM.

On the VM, the initialization script selects only `/dev/disk/by-id/google-komaphone-data`, checks the exact20 GiB whole-disk attachment, refuses partitions/foreign signatures or a mount elsewhere, and formats only a blank disk. An existing ext4 disk is mounted without formatting. It requires an empty mount directory, uses UUID in fstab, and never recursively changes ownership:

```bash
sudo bash prepare-vm.sh --initialize-empty-data-disk
```

Create `/etc/komaphone/service.env` with the following public configuration; make it root-owned, mode0644. Create `/etc/komaphone/backup.env` similarly. Neither contains credentials:

```text
KOMAPHONE_GOOGLE_CLIENT_ID=981926394354-07f4s3dss4vgp7mteh0qe8e6dhk3s99o.apps.googleusercontent.com
```

```text
KOMAPHONE_BACKUP_BUCKET=komaphone-k65-backups-20261112
```

Install the [bounded journal policy](../../scripts/komaphone/k6-deploy/journald-komaphone.conf) under `/etc/systemd/journald.conf.d/komaphone.conf` after confirming this is the dedicated VM. It bounds the entire VM journal to128 MiB persistent/32 MiB runtime and7 days. Retain the journal long enough to diagnose acceptance, without enabling request/header/body logs.

Activate the exact verified binary, replacing the two arguments with the retained build artifact path and hash:

```bash
sudo bash activate-release.sh /absolute/path/photocraft-sync EXPECTED_SHA256
sudo install -o root -g root -m 0644 Caddyfile /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
```

Activation checks the binary hash and hosted CLI, requires the data mount, installs an immutable release directory keyed by **binary hash plus runtime-helper/unit bundle hash**, stops the proxy, takes a verified backup using the previous release when updating, starts the new service and verifies the hosted config/client ID. This allows a backup-helper correction with the same service binary while preserving the original release. Any startup failure leaves the proxy stopped. It never automatically puts an older binary against a potentially migrated database. Record release/script/config hashes and the service's current schema/protocol in the evidence ledger.

**K7 local hardening, 2026-11-14 (not yet deployed):** activation now retains all
three systemd unit files alongside the binary/helpers and installs those pinned
copies. Before stopping services, it snapshots currently installed units plus
`service.env`/`backup.env` into the new release's root-only `pre-update-config`
directory, with checksums. This also captures units from older releases that did
not retain them. Treat these configuration snapshots as private; do not upload
them to client packages or diagnostic logs. A reviewed rollback must reconcile
the compatible database backup, prior binary, units and configuration, plus any
separately managed systemd drop-ins/Caddy state. Never automatically restore an
old database over newer acknowledged writes. Local failure/restore evidence is
tracked in the [K7 checklist](k7-release-checklist.md); production activation and
actual release-to-release migration are not claimed by those tests.

## Public activation, reboot and certificate checks

Before opening the public firewall, run actual service tests for invalid/replayed Google identity, wrong audience/expiry, legacy Pair and bearer denial, admission, revoked sessions and account switching. Readiness is an additional guard, not a replacement for those authorization tests. On the VM:

```bash
python3 hosted-probe.py
sudo systemctl status komaphone.service --no-pager
sudo ss -lntp
```

The Rust listener must be127.0.0.1:43127; no editor-control listener is deployed. After approved Namecheap DNS points to the static IP, enable only the prepared web rule from the administrator environment:

```bash
gcloud --project=komaphone compute firewall-rules update komaphone-web --no-disabled
```

Start/enable Caddy on the VM. Check with an ordinary trusted HTTPS client from outside the VM, without `-k`, custom roots or a VPN:

```bash
sudo systemctl enable --now caddy
python3 hosted-probe.py --base-url https://komaphone.tedsoftware.org
```

Record public chain/hostname, certificate issuer and expiry. Preserve `/var/lib/caddy` certificate state across releases. Caddy renews automatically, but configuration alone is not renewal evidence. Verify renewal using an isolated ACME staging exercise first; do not repeatedly erase production certificates or trigger production issuance rate limits. When the real renewal window arrives, retain its successful renewal observation; explicitly label a staging exercise separately. The [Caddy HTTPS documentation](https://caddyserver.com/docs/automatic-https) explains DNS/reachability/storage requirements.

Reboot the VM through the administrator's approved operational test. Then verify data UUID mount, hosted health, trusted HTTPS, both units, scheduled backup timer, room/canvas access and a prior acknowledged operation. Attempt external ports22/43127 and confirm they are closed; IAP administration still works. Do not infer a reboot pass from `systemctl enable`.

## Backups, fresh restore and revocation safety

`backup.sh` runs the service's verified SQLite snapshot, which includes immutable referenced assets, computes database and service hashes, then uploads the database followed by `manifest.json` to a unique object prefix with generation-match0. The manifest is the completion marker. Failed uploads leave bounded local staging for investigation; two leftover stages stop new attempts rather than accumulating indefinitely. The metadata account can create objects but cannot list/read/delete/overwrite backups. Run the timer once and inspect its journal:

Uploads now use `upload-object.py` and the [GCS JSON object-create API](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert). The initial `gcloud storage cp` attempted an object-read preflight and failed403 under the deliberately restricted account. The corrected uploader streams bounded file blocks over ordinary HTTPS, requests `ifGenerationMatch=0`, validates response size/generation and keeps metadata credentials only in memory. No extra IAM permission was granted. Administrator readback still uses the normal CLI below.

```bash
sudo systemctl start komaphone-backup.service
sudo systemctl status komaphone-backup.service --no-pager
sudo systemctl list-timers komaphone-backup.timer --no-pager
```

An authorized administrator lists the bucket and downloads a prefix containing both files using explicit project selection. Do this in a fresh private directory; substitute the observed exact prefix, never a broad bucket wildcard:

```bash
gcloud --project=komaphone storage cp gs://komaphone-k65-backups-20261112/backups/EXACT_PREFIX/manifest.json ./manifest.json
gcloud --project=komaphone storage cp gs://komaphone-k65-backups-20261112/backups/EXACT_PREFIX/service.sqlite3 ./service.sqlite3
```

Verify `database_bytes` and `database_sha256` against the downloaded file, and retain the immutable generation IDs. Copy only that backup and manifest via IAP into a fresh drill directory on the data disk or a separately authorized isolated restore host. With the matching service binary, restore into a **new empty** directory. The K6.5 `--restore-from` command now quarantines restored profiles/access automatically; the explicit invalidation command below is an idempotent additional check before starting a loopback test service:

```bash
photocraft-sync --data-dir /fresh/isolated/restored --restore-from /fresh/download/service.sqlite3
photocraft-sync --data-dir /fresh/isolated/restored --invalidate-restored-access
```

Use a separate loopback port and no Caddy route for the restored service. Verify room assignments, accepted artwork/assets, stable authorship, canvas ownership/members/revisions and canonical hashes. Revoked sessions and spent admission/room/canvas invitations must remain unusable. Invalidation also quarantines **all restored profiles**, so fresh Google login cannot silently revive a profile revoked after the snapshot. Before re-admitting any profile with the local `--admit-profile` command, compare with current authority/revocation records and reapply later membership removals or other denials. If the current authority is lost, hold the restore private and re-admit/reinvite only reviewed accounts; do not let an old membership list regain live access silently. Preserve the original service and database throughout the drill.

Administrative admission/revoke/readmit commands require stopping the service first: the binary holds an exclusive `service.lock` to preserve one-writer authority and clear transient presence on restart. Backup snapshots are the read-only exception. Stop Caddy, stop the service, execute the exact local administrator action as `komaphone`, then restart and verify hosted readiness before resuming Caddy. Admission capabilities are explicit private output, never stored in journals, tracked files, screenshots or this evidence ledger. Use ordinary invitation UI for friends; do not make friends copy developer bearer tokens.

Exercise interrupted backup upload and a corrupt download with synthetic test data. A missing completion manifest or mismatched size/hash must fail before restoration. Readiness, upload success and a SQLite integrity check alone are insufficient restore evidence.

## Update and rollback

Each release has an immutable binary/hash and compatible schema/protocol record. Before update, verify an off-VM backup and keep the previous release/configuration. `activate-release.sh` stops public proxy access and the service before its verified local pre-update snapshot, then performs startup/readiness with the proxy still stopped. Inspect the new service and repeat local authorization checks before starting Caddy.

If an update fails **before public access resumes and before new writes are accepted**, keep the failed database for diagnosis; restore the pre-update snapshot into a new directory, invalidate/reconcile access, and use the recorded compatible binary. Pointing the old binary at a newer schema is not rollback. If acknowledged new writes exist, preserve them and use a forward repair or an explicitly reviewed merge; a blind old-backup restore loses accepted art.

For infrastructure retreat, disable `komaphone-web` and stop Caddy first. Verify a restorable off-VM backup and retained local art. Preserve data disk and bucket. Deletion protection and data auto-delete=false prevent accidental instance deletion from becoming artwork deletion. Any final cleanup identifies the exact resources individually and needs the owner's destructive-change authorization; there is intentionally no blanket teardown script. Stopped VMs still bill for disks, addresses and backups, and a reserved unassigned address can cost more than one attached to a VM.

## Acceptance ledger to fill during execution

Record actual results, with a dated build/service hash and no credentials:

- Configured Windows sign-in through Google; admitted friend invite; nonmember/legacy enrollment denial; cancellation, expired/revoked sessions and isolated account switches.
- Owner-approved independent-public-network Animation and Exquisite Corpse completion, phase reconnects, service restart, close/reopen, reveal/replay/export; identify exact two-/three-player configurations tested. Fresh friend onboarding in another household is retained for K7.
- Shared-canvas simultaneous and asynchronous non-owner work, offline queued edits, author-local undo, isolation, tours/catch-up/remix and review-position recovery; preserve K6's larger membership fixtures.
- Off-VM completion manifest, fresh restore and post-backup revocation/membership reconciliation; original service preserved.
- Trusted HTTPS, renewal observation or explicitly limited staging evidence, reboot/mount/service/backup recovery, external port restrictions.
- Observed CPU/RSS/disk/traffic during representative acceptance, full-quota recovery headroom, billing period/gross-cost projection, budget recipients and actual domain/tax assumptions.

Preparation checks and their exact outcomes are added below. Nothing in this runbook alone closes K6.5.

### Preparation evidence — 2026-11-12

On the existing Ubuntu WSL environment, `python3 scripts/komaphone/k65-deploy/test_deploy.py` passes **9 tests** (zero failures) in2.455 seconds: printed plan performs no gcloud calls even when approval exists; missing/wrong approval prevents all calls; pre-existing resource and permission failures stop before resource creation; successful fake apply keeps explicit project scope, bucket-only IAM and disabled public ingress; real loopback readiness accepts only matching hosted configuration; untrusted HTTP and URL credentials are rejected; all shell scripts parse. These use fake gcloud, not the actual account.

`python3 scripts/komaphone/k65-deploy/validate-config.py --caddy log/k6/deployment/tools/caddy/caddy` passes Caddy2.11.9 validation and `systemd-analyze verify`. Only unavailable installed executable paths were substituted in temporary unit copies; no unit was installed or started. This validates configuration syntax, not actual VM supervision/reboot or certificate issuance.

The first combined test/syntax wrapper passed8 tests but its extra shell loop failed because PowerShell-to-WSL quoting removed the loop argument. The corrected Python-owned syntax test is included in the9-test pass above; the failed wrapper is not counted as success. WSL inspection initially needed filesystem/process escalation, which was approved; no cloud write occurred. New tests create only temporary local files and loopback listeners.

### Linux service and packaging checkpoint — 2026-11-12

The owner requested a pause before deployment or further login testing. **No GCP resources, DNS changes, installed VM services or distributed Windows package were created.** Resume from the tracked status; do not interpret this checkpoint as hosted acceptance.

`log/k65/deployment/linux-20261113T061005Z/` retains a 2,832-file frozen source snapshot manifest (`8fa0deb061b8a944c5c1ae07cea80cdaaa69608dc7a1ccd83172667116cddfe4`), exact commands and outputs. In existing isolated Ubuntu WSL storage, `cargo test --locked -p photocraft-sync --no-fail-fast` passes **96 tests, zero failures/ignored**, strict all-target Clippy passes, and `cargo build --locked -p photocraft-sync --bin photocraft-sync` passes. The service binary is `bin/photocraft-sync`, SHA256 **`d8e8628bc5ba2555cfb5031c23320272691b15dac2704bbebde5306aa942151e`**. The current service source differs only by a subsequently applied rustfmt brace expansion in `identity/hosted.rs`; that exact nonsemantic diff was independently inspected. The binary's source pin remains the retained snapshot, not an assertion of byte-identical current source.

The unchanged K6 Linux fixture passes against that binary: actual Caddy TLS proxy, rejection of an untrusted test CA and public plaintext, 700 operations across 2/4/8-writer groups, equal authorized event streams, SIGKILL/restart durability, and fresh verified backup restoration with denied nonmembers. Result: `runtime/run-014a5e7560c04798b20748f8b6384a9b/result.json`. The workload took58.22 seconds; service/proxy observed peak RSS16.63/50.65 MiB; combined CPU7.73 seconds; DB/backup/restore size2,514,944 bytes. HTTP p95 was1,028.14 ms while other builds contended for the host. This is functional evidence, **not a performance gate, native preview latency, Google login, public certificate trust or GCP measurement**. The earlier frozen attempt failed before compilation because `Cargo.lock` lacked a newly added client `chrono` dependency; its failed log is retained under `linux-20261113T060440Z/`. The shared lock was subsequently resolved by the coordinating agent.

The new [Windows packager](../../scripts/komaphone/package-k65-candidate.ps1), standalone [hosted launcher](../../scripts/komaphone/k65-launcher/src/main.rs) and [friend guide](k65-friends.md) are prepared. The launcher uses a stable `k65-hosted` data/config root, separate program directories per verified manifest, an exclusive profile lock, safe relative-export directory, and normal public certificate trust. Seven Windows helper tests pass, including update/data preservation, export/restart, hash/inventory checks, exclusive lock and junction rejection; strict Clippy and formatting pass. The initial sandbox run could not create required test hard links/junctions (4 passes/3 OS-access failures); the approved exact unsandboxed test rerun passed7/7. Packager syntax, no-write `-ValidateOnly`, wrong-hash rejection and existing-destination rejection pass. These checks used the pinned K6 app solely as a validation input and produced no hosted package. The new helper's final static-CRT release packaging, actual K6.5 app launch, fresh-profile login and the affected K4-LAUNCH-01 laptop retest remain pending.

`hosted-runtime.py` is prepared to exercise the real service binary's hosted readiness, login begin/cancel, invalid-token/legacy-pair denial and administrator writer lock in isolated local data. **It was not executed before the owner pause.** Its preparation does not add a pass to this ledger. It does not attempt real Google login even when executed.

### Resumed deployment verification — 2026-11-13

The prepared `hosted-runtime.py` now **passes** against the frozen Linux service SHA above. The actual production service reports matching hosted health/auth configuration, completes bounded login begin/cancel, rejects malformed Google tokens, legacy Pair, unknown bearer authority and cross-origin requests, and refuses a second administrator writer while running. The fixture uses isolated synthetic data and stops its child process. Exact result: `log/k65/deployment/resume-20261113/hosted-snn_9tez/result.json`. This does **not** test real Google sign-in, public certificate trust, GCP runtime or separate households.

Explicit `--project=komaphone` read-only inventory again found no VM instances, disks, addresses or buckets. US-central1 is UP, with E2_CPU quota24/usage0 and in-use/static address quotas8/usage0. The empty collections serialize as null in `log/k65/deployment/resume-20261113/gcp-readonly.json`; they are successful empty CLI results, not unperformed checks. The unrelated default gcloud project was not changed. Network/service-account names and bucket global availability are checked again by the initial provisioning script before its corresponding creates; this inventory does not reserve capacity or names.

Official VM, IPv4/egress, disk, storage/request and optional logging price sources above were refreshed. The totals remain unchanged. The VM page timed out when opened directly, so its indexed official price table supplied the E2 rate; the network, disk, storage and logging pages were readable. This is a public list-price estimate, not a billing-account quote. The current actual domain allocation, tax, credit eligibility and gross-budget alert configuration remain unverified.

The reviewable authorization scope is the exact inventory above: create the single e2-small VM with 10+20 GiB standard disks, one static IPv4, dedicated VPC/subnet, IAP-only SSH, initially disabled web rule, private regional backup bucket and its bucket-only uploader identity; then install the source-pinned service and local proxy/backup configuration for private validation. Expected cloud cost is $18.33/month at5 GiB or $23.73 at50 GiB egress, before tax/credits; adding the provisional domain share gives $19.33/$24.73. Public firewall activation and the existing Namecheap subdomain change follow successful authentication checks and explicit deployment authorization. Stop public access first if validation fails, preserve the data disk/bucket and verified snapshots, and investigate without automatic destructive teardown or database downgrade.

No resource creation, public firewall activation, DNS edit, Windows package distribution or cloud expense was initiated by these resumed checks. The source-pinned service artifact is ready for a controlled private deployment test; the complete hosted product remains subject to the pending Windows/Google/public/recovery/household gates.

### Updated Linux candidate — 2026-11-13, 19:28 UTC

`log/k65/deployment/linux-20261113T192233Z/` pins a fresh2,840-file snapshot with manifest SHA256 `85fc0faffcd92a989c7da719aa5273cdc02d75f492ef76426548f9a3a5dd0c0d`. All21 service source-file hashes match the service agent's final resume ledger and the current workspace at verification time (`service-source-comparison.json`). The candidate binary SHA256 is `d30065ddafc5d0b7b770692210031803b477e9259a227fb80f597554ba6ca998`.

The same service-only commands pass: `cargo test --locked -p photocraft-sync --no-fail-fast` (**98 passed, zero failures/ignored**), strict all-target Clippy and the service binary build. The new regressions exercise Google-only People pagination/revoked-profile hiding and hosted stroke-copy credit. No whole-app Linux build was started.

The unchanged local Caddy TLS/restart/backup fixture passes on retry:700 operations across2/4/8 writers, equal authorized event streams, SIGKILL durability and fresh verified restoration preserving events/nonmember denials. Result: `runtime/run-b99544f4c4624ec6ba1762df328a3766/result.json`. Workload17.06 seconds, observed service/proxy peak16,912/50,772 KiB, combined CPU4.46 seconds, DB/backup/restore2,514,944 bytes, HTTP p95~277 ms. These remain local functional observations, without host exclusivity or simulated E2 limits; they are not native-preview or GCP performance acceptance.

The first runtime attempt failed after trusted-health/untrusted-CA checks because the host clock moved backward: Caddy issued its test certificate at19:26:44.618 but later recorded shutdown at19:26:42.453, and Python rejected the now-future certificate. The failed result/log remains at `runtime/run-48cb863f8de146ed9e635fba9879cd87/`, with the failed command in `commands.jsonl`. The exact fixture was rerun without changing trust or service code; only that later complete run is counted as passing.

The actual hosted production-binary fixture also passes at `hosted-runtime/hosted-_ltmnsii/result.json`: matching hosted health/config, login begin/cancel, malformed-token/legacy-pair/unknown-bearer/cross-origin denial and administrator writer lock. Both fixture processes exited. Actual Google login, public trust, off-VM recovery and separate-household play still require their own evidence. No cloud resource was created.

### Windows desktop OAuth configuration packaging

The real Google exchange identified that this installed Desktop client also requires its `client_secret` field. This is the application's native public-client configuration, distinct from a user's bearer token, Google login token, admission capability or service-account key. Those user/server credentials are never package inputs. The service VM still receives only the public client ID.

The candidate packager now requires `-GoogleClientJson` pointing to the owner's original downloaded Desktop-client JSON. The packaging parser reads at most32 KiB, requires the exact compiled client ID and a1–256-byte ASCII-graphic secret, and emits only `installed.client_id` plus `installed.client_secret`. Extra downloaded fields, URLs and project metadata are discarded. Parse errors use fixed diagnostics without echoing the input. The raw file and sanitized content must remain outside tracked sources and logs; only the sanitized `candidate/google-desktop-client.json` belongs in the explicitly authorized friend package.

The sidecar is part of the launcher's exact19-file inventory and pinned manifest. Missing or modified configuration fails verification before launch. The launcher sets only the verified sidecar's absolute path in the child's `KOMAPHONE_GOOGLE_CLIENT_CONFIG`; it never passes the field value through arguments or environment variables. Friends use the normal launcher with no manual configuration. Rotating the desktop configuration requires a newly verified package/launcher manifest, but no application rebuild. Use `-ValidateOnly` for a no-write check before building the small launcher; final app hash, candidate ZIP hash and actual launch/login acceptance remain separate gates.

Synthetic checks on2026-11-13:12 sanitizer tests pass (strict bounds/shape/client/secret, malformed UTF-8/JSON, exclusion of unrelated fields);8 Windows launcher tests and strict Clippy pass, including missing/tampered sidecar and child-path checks. The initial launcher run had7 passes and one test-only Windows separator comparison mismatch; after correcting the expected path,8/8 pass. No-write packager validation with the old pinned K6 PE solely as a fixture and synthetic configuration passes, creating no candidate. Logs: `log/k65/deployment/resume-20261113/launcher-google-tests2.log` and `launcher-google-clippy.log`. These checks do not claim an actual Google login or a completed K6.5 package.

The first actual package attempt later reached the release launcher's static-CRT guard and failed safely. PowerShell7.6.9 turned the attempted null environment assignment into a present-but-empty `CARGO_ENCODED_RUSTFLAGS`, which Cargo prioritized over `RUSTFLAGS`. The packager now explicitly sets both plain and encoded static-CRT flags through `k65-launcher/build-environment.ps1`, then restores original presence and value after success or failure. Six child-process cases cover absent, empty and stale inherited flags. All8 launcher tests, strict Clippy and an actual release build pass; `dumpbin /dependents` reports only Windows system DLLs (`api-ms-win-core-synch-l1-2-0.dll`, `KERNEL32.dll`, `ntdll.dll`), with no dynamic CRT dependency. The guard remains intact. Logs: `launcher-build-environment-tests.log`, `launcher-static-crt.log`, `launcher-static-crt-dependents.log` under the resumed deployment evidence directory. This verification build uses a synthetic manifest pin and is not the distribution launcher; the packager must rebuild with the real candidate manifest.

### Actual GCP deployment and operations — 2026-11-13

After the owner's deployment approval and the coordinating agent's real-Google gate (`log/k65/root/live-google-probe3.log`), `gcp-windows.ps1 -Mode Apply` created the exact reviewed inventory successfully. No unrelated default project was changed. Evidence under `log/k65/deployment/resume-20261113/`: `gcp-apply.log`, `gcp-created-inventory.json`, `dns.json` and the `vm-*.log` files. The VM is regular e2-small inus-central1-a, with10 GiB boot/20 GiB retained data disk, deletion protection, OS Login/IAP-only administration, private Standard backup bucket,8-day lifecycle/7-day soft delete, and **zero project IAM bindings for the backup account**. The prepared web rule was enabled only after DNS and live hosted checks passed.

Official signed packages installed Caddy2.11.9; the Google Debian image already contained Cloud SDK592.0.0. Uploaded file hashes matched before installation. Data UUID `442a5eac-1be7-46f9-8820-a8c2d44b829a` is mounted at `/var/lib/komaphone` with `nodev,nosuid`. The Rust service listens only on127.0.0.1:43127, with the exact98-test binary pin above. The VM received only the public OAuth client ID; no desktop configuration, administrator cloud credentials or downloaded service-account key was copied there. Installed runtime bundle: `d30065ddafc5d0b7b770692210031803b477e9259a227fb80f597554ba6ca998-df220b590f2351014939c4b13d39c3c250780fc8fbac86920f5e0f7adac5161f`.

Ordinary workstation HTTPS validation passes with TLS1.3 and a Let's Encrypt YE2 certificate for the exact hostname, valid2026-11-13T19:18:11Z–2027-02-11T19:18:10Z. Certificate SHA256 `280c0e3ab0f78af6a3292f5927a0954f8df7eeea1c6100d36254cf1377c44477`. HTTP redirects308 to HTTPS; external ports22 and43127 time out closed while IAP administration succeeds. Both live loopback and public probes pass hosted config, login begin/cancel and malformed-token/legacy-Pair/unknown-bearer/cross-origin denial. These probes enroll no account. Files: `public-security.json`, `public-https-denials2.log`, `vm-live-denials.log`. An earlier workstation probe hit cached NXDOMAIN; one cache clear followed by ordinary validation resolved it, without custom roots, hosts-file changes or TLS bypass. Production ACME issuance is observed; **renewal is not yet observed**.

The first backup failed because CLI preflight required `storage.objects.get`; its log and `backup-staging.8jZCTNtJ` remain preserved. After the create-only uploader fix, backup completion at20:15:38UTC uploaded233,472 database bytes and366 manifest bytes to `backups/20261113T201537Z-234wssYH/`, generations `1794600937824427`/`1794600938115322`. Administrator readback matched database SHA256 `e3f14fb813e7f6c39444b59f70d37b726fd1f02c784a5c6593af5bf85d6b4dd7`. The fresh isolated restore rejects corrupt data and a missing completion manifest before restore, preserves compared content, quarantines access, and passes hosted readiness/denials. **This was an empty initial authority: zero profiles and zero room/canvas/art rows. It proves the pipeline structurally, not recovery of accepted hosted artwork. Repeat after substantive app play and post-backup revocation.** See `backup-readback/verification.json` and `vm-fresh-restore.log`. The original database remained in place.

An actual reboot changed boot ID from `7accb327-98b8-4709-ade7-84ae09ba355c` to `a591c06c-1042-4671-a916-38f3ccb7b4c1`. UUID mount, hosted service, Caddy and backup timer recovered active/enabled; trusted public health passed. Linux changed data device name fromsda tosdb, while the UUID mount continued correctly. The first IAP verification raced shutdown and disconnected; its failed log is retained and the post-boot retry passed. See `vm-after-reboot2.log` and `public-after-reboot.log`.

The unchanged K6 workload fixture also ran **on the actual e2-small**, under the service user in isolated `/var/lib/komaphone/e2-capacity-20261113/`, with its own loopback service/TLS ports and synthetic database. It passed700 operations with2/4/8 writers, equal event streams, SIGKILL durability and fresh restore/nonmember denial. Duration20.275s; service/proxy peak17,476/55,536 KiB; CPU9.06s; HTTP p95~351ms; DB/backup/restore2,514,944 bytes. `e2-capacity-evidence.json` identifies the VM execution; the retained unmodified fixture's `cloud_deployed:false` flag means that fixture itself never provisions or exercises public hosted authentication. This is a bounded infrastructure workload, **not household latency, Google-authenticated play or full2 GiB quota acceptance**.

At20:30UTC, the data filesystem used9,691,136 bytes including retained drills/staging, with20,930,977,792 bytes available. Since reboot, ens4 counters were654,610 bytes received/446,201 sent, including administration and system traffic; loopback fixture traffic is separate. These few minutes do not establish a monthly bill. The reviewed recurring estimate remains$18.33/$23.73 cloud for5/50 GiB monthly egress, plus provisional$1 domain allocation and unknown tax, before credits. Actual billed gross usage and credit treatment still need a representative Billing observation.

Two owner-authorized24-hour, single-use test admissions were generated during an orderly administrative stop/restart and delivered only through private0600 files and owner/SYSTEM-only Windows ACL files. No values appear in this guide, logs, argv, package or manifest; `admission-delivery-metadata.json` records paths/expiry only (2026-11-14T20:28:14Z). Ordinary public readiness passed after restart. Initial multi-source PuTTY download was unsupported; separate file transfers succeeded without reminting capabilities.

Preparation regressions after the deployment fix:9 deployment/shell checks and3 no-cloud uploader tests pass. Current service/proxy are active, backup timer is enabled, and the one failed staging directory is intentionally retained. Follow-up gates: actual native hosted accounts/play and account switching, accepted-art off-VM recovery with later revocations, certificate renewal/staging evidence, representative gross billing, and separate-household acceptance. Do not mark K6.5 complete from this infrastructure result.

### Managed staging certificate renewal — 2026-11-13

The installed Caddy2.11.9 completed one Let's Encrypt **staging** issuance and one managed renewal using the same private store, account and staging issuer. Phase A took6.35 seconds; phase B took7.37 seconds and recorded `tls.renew: certificate renewed successfully`. The certificate fingerprint changed from `e38e031d1d87c27c685bd74edaedd5a976c39366ec62e8639e059e5be669e22c` to `08063fda2e3ced7492bb131b0e59683962552ab331048bf62d083f0564c9f8c3`; both certificate/key pairs matched. This was actual ACME renewal of the retained certificate, not a second empty-store issuance.

The staging process had no HTTPS listener and could not access `/var/lib/caddy`. Only temporary HTTP challenge GETs reached its loopback solver. Each phase had a180-second systemd deadline and an independent8-minute rollback watchdog protected the original production configuration. Afterward, exact original Caddyfile/active JSON, production certificate/key/account-file hashes, production leaf and Caddy PID were unchanged. Both staging processes stopped, no solver listener remained, and the verified-restoration watchdog was canceled. Ordinary workstation HTTPS health/config, the original trusted certificate, HTTP308 and external22/43127 denials passed. No cloud resource, production certificate, clock, DNS record or trust store was changed.

The first final assertion used a socket bind and falsely rejected normal TCP `TIME_WAIT` left by the stopped solver. `ss` showed no listener and both unit PIDs were zero. The original failed result/log was preserved; supplemental verification checks listening sockets and connection refusal, and confirms exactly one success per phase without making another ACME request. Two real local Linux socket regressions pass for a live listener and closed TIME_WAIT. Evidence: `log/k65/deployment/resume-20261113/tls-staging-execution.log`, `tls-staging-post-inspection.log`, `tls-staging-verified.json`, `tls-listener-regression.log`, `public-after-staging-renewal.json` and `public-after-staging-health2.log`. Private staging keys remain only inside the caddy-owned0700 staging directory under `/var/lib/komaphone-tls-validation/e751fd51c757408baa794b150e833b44/`; evidence contains public metadata and hashes only.

This exercises accelerated staging renewal on startup with `renewal_window_ratio=1` and a1-hour maintenance interval, using the pinned [Caddy2.11.9 dependencies](https://raw.githubusercontent.com/caddyserver/caddy/v2.11.9/go.mod) and [CertMagic0.25.6 renewal implementation](https://raw.githubusercontent.com/caddyserver/certmagic/v0.25.6/config.go). It **does not prove natural production renewal timing/ARI or a production-CA renewal**. Observe the real production certificate changing during its normal renewal window; keep the [staging CA's untrusted certificates](https://letsencrypt.org/docs/staging-environment/) out of normal trust stores. Any repeat requires reviewing current production configuration and the reason for another issuance, not an unattended retry.

### Prepared populated-art recovery checkpoint

This sequence is prepared from a read-only source audit while real native hosted play is underway; it has **not run**. The coordinator must signal an acknowledged game/canvas checkpoint before execution. No service stop, profile revocation, restart or live-data replacement is part of preparation.

1. Record the clients' accepted room/game IDs and revisions, submission asset hashes, and canvas IDs/revisions. Save only an allowlisted receipt of IDs, counts, revisions and content hashes. Never dump room snapshots, canvas state, account subjects, names, invitation values, session verifiers or bearer tokens into the receipt. Heartbeats can increase room revision, so the captured backup must include at least the acknowledged revision and the exact acknowledged submissions; exact room revision equality is unnecessary. An archive row is expected only after the application's archive/next-game transaction, not merely because a round or reveal completed.
2. Run the existing consistent online backup through `komaphone-backup.service` at that checkpoint. The administrator backup path is exempt from the running-authority writer lock, so the game service need not stop. The pinned binary verifies source and copied database/assets, the uploader conditionally creates unique objects, and the completion manifest uploads last. Record object generations, database size/SHA and service SHA. Read the objects back through administrator access into a new private directory; run `verify-backup.py` before opening or restoring them. Do not broaden the VM's create-only bucket permission.
3. Extend the prepared restoration comparison before executing the populated drill: compare typed, deterministic row digests/counts for `rooms`, `archives`, `objects`, `worlds`, `world_events`, `world_members`, **`world_media_sources`, `world_blobs`, `world_trash`**, and `receipts`. The original empty drill covered only the first six. Full object/media byte hashes and lengths, complete-upload flags, stored world/event hashes, canvas revision and event revision range must match. Record room/game phase and revision, acknowledged-submission count, completed object count/bytes, and canvas stroke/media/tour counts without serializing the underlying art or labels. Verify the acknowledged client IDs/revisions/hash set against this backup receipt. SQL read-only mode and a bounded transaction keep each receipt coherent; run the substantive comparison against the immutable readback rather than a changing live WAL file.
4. Restore only into a fresh isolated directory with the exact pinned service binary. `--restore-from` automatically quarantines every hosted profile, session, admission and pending invitation. Before starting the isolated process, require identical content receipts and check zero unrevoked profiles/sessions/admissions/room invitations, zero pending canvas invitations and zero in-flight identity logins. Start only on a fresh loopback port, run hosted readiness/denials, and stop that owned process afterward. Startup can legitimately change active-room timing/revision state, so pre-start content equality and later service readability are distinct checks. The public service and original database stay in place.
5. For a substantive old-access HTTP check, use a previously valid session retained only in an owner-restricted private file or process memory by the coordinator. First establish that its digest matched an active session in this exact backup (emit a boolean only). After the owner safely signs out, establish revocation against live authority, then test the same old session against the isolated restored service and require HTTP401. Transport must remain private, through loopback/IAP, with no bearer in argv, logs, manifests, chat or package. A random unknown token401 does **not** prove restored-session revocation. If private access cannot be obtained without disturbing play, retain this as a pending check rather than substituting an unknown token.
6. Reconcile any profile/member revocations made after the backup before selectively readmitting a restored profile. Persisted all-profile quarantine already fails closed, but a live post-backup profile revocation and fresh Google relogin denial are separate evidence. Any live administrative profile revocation requires the coordinator's later safe checkpoint because its CLI requires an orderly service stop; do not perform it during play. Readmission changes only profile eligibility and cannot reactivate an old session. Preserve the verified snapshot, restore receipts and denial results; never replace production data merely to demonstrate recovery.

Acceptance must explicitly distinguish: populated game/canvas/media byte preservation; equality with acknowledged client work; old known-session denial; post-backup profile/member reconciliation; and actual client-readable recovered artwork. A structurally valid backup, synthetic authorization refusal or empty table hash alone does not close these gates.

The prepared verifier is now implemented in `content-receipt.py` and integrated with `restore-drill.py --expectations <private-json> --require-populated`. It hashes typed rows from all ten content tables, verifies completed object/media bytes against their stored hashes, requires contiguous canvas event revisions, and matches terminal-game image assignment/hash/size sets and author counts against allowlisted export receipts. Canvas `state_sha256` is the same SHA256 of serialized `WorldState` exposed by the native canonical hash, so a final accepted revision/count/hash can be matched without dumping art. Empty/missing game or canvas expectations cannot pass the populated gate. Restored profiles, sessions, admissions and invitations must all be quarantined; in-flight login transactions must be gone.

Nine local Linux synthetic regressions pass, including corrupted bytes, missing events, wrong accepted art, previously unexamined media/trash/receipt changes, output privacy and exact room restart validation. The expanded drill also passes against the existing isolated700-event fixture with the exact pinned98-test service binary; this is preparation evidence only. The initial strict comparison exposed that **the restore CLI itself opens Store and performs one room restart**, before the later isolated server start: room revision increases once, members disconnect, the clock resets, and active timers pause. The verifier now checks those exact permitted transformations and requires all other room fields, art and permissions unchanged, with all nine other content tables byte-equivalent by digest. It retains both original/restored receipts. The failed strict comparison remains `expanded-local-restore-initial-failure.json`; passing evidence is `expanded-local-restore.json` and `content-receipt-linux-tests2.log` under the resumed deployment directory. An initial Windows test attempt could not open SQLite files in the sandbox temporary directory; the Linux fixture run supplies the actual nine passes. No production backup/restore was performed by these preparation checks.

### Actual populated recovery — 2026-11-13, 22:24 UTC snapshot

The first read-only comparison found newer live work than the revision35/31-stroke client receipt: revision37/33 strokes. The mismatch was retained and no work was removed. An online backup preserved that earlier checkpoint at `backups/20261113T222303Z-7b45OCBf/`; independent readback verified3,645,440 bytes, SHA256 `1a1395bccc2c4875493c04da97b88c98dccc8d5547d2b94cec4a4e649e11943e`. It was not substituted for an exact acknowledged checkpoint.

After the owner stopped writes and the coordinator supplied an updated native export, the next online backup fixed **shared revision39/35 strokes**. Object prefix `backups/20261113T222426Z-V7fpJi7c/`; database generation `1794608667423065`, completion-manifest generation `1794608667652008`. Independent generation-pinned GCS readback into an owner/SYSTEM-only directory verified3,661,824 bytes and database SHA256 `ba24ae46fc8ed41530759896c1139ffebab08b23dc8d1664878586303782d154`. The native manifest file hash is `04f6b97b623a37e984eac29035c30fafe166f304ef8888710fa5ffa688488147`; its canvas canonical hash `4711a7443478133582cd9210a6cff9e34fa51a6174e7f54bceedb20aa0446142` matches the snapshot exactly.

A fresh private restore of that actual readback passed all ten content-table checks, the one exact room-restart transition, and automatic quarantine of both hosted profiles, sessions, admissions and invitations. Both completed games match their native export receipts: four Animation images and six Exquisite Corpse images, with exact assignment/hash/size sets and author counts. The snapshot contains51 object rows with2,862,327 completed-object bytes, three canvases,41 total canvas events, four membership rows, one media source, two media blobs and141 RPC receipts. Shared art includes one tour and one media item; each private canvas has one stroke/revision1. No trash rows existed, so actual trash recovery remains covered by the synthetic regression, not a populated production trash fixture. Evidence: `populated-backup-revision39.log`, `populated-readback-revision39-verification.json`, `populated-revision39-content.json`, `populated-restore39.log` and `populated-restore39-result.json` under `log/k65/deployment/resume-20261113/`.

The isolated restored authority was subsequently started only at127.0.0.1:49196 under a20-minute systemd deadline, with its own data directory and the unchanged pinned service binary. A temporary IAP SSH forward at localhost43195 (IPv4/IPv6 loopback only) reaches it; no public rule or production endpoint changed. The first local port49195 fell inside Windows' reserved49193–49292 range; that failed forward was stopped and the available port43195 was attested. A private-canvas display-name assertion also failed because the owner used a variation; owner ID, unique owned canvas and absent nonmember grant provide the correct identity check, without requiring a rename.

Automatic approval review twice rejected creating/downloading the verifier-only receipt, including after source evidence established that it contains SHA256 of a random bearer, never the bearer itself. The stated reason was missing explicit authorization for that authentication-related payload and exact local destination. No alternate transfer was attempted. The owner then explicitly authorized this receipt, and the reviewed action completed after fresh listener attestation: only private verifier metadata was delivered into the owner/SYSTEM-only readback directory; no raw bearer was read or exported. Receipt contents are excluded from logs; `restore39-private-receipt-pin.json` records its file hash/path.

The coordinator's reviewed probe subsequently **passed with the known current session**: production Identity200 → isolated restored Identity401 with fault `session` → production Identity200, proving denial was not an unknown token or expired live session. The hash matched the exact backup session; client scope stayed unchanged. The same B session received403 for A's independently verified private canvas. Evidence: `log/k65/root/restore-session-live-proof1.log` and `.json`, pinned executable SHA256 `900d39497960496ee1a9a05c3f54e62c4898e59a28074b46c25fc5ac9849d928`, receipt file SHA256 `3205ac6f09a60c316372597accffcbffe77542c7409e3cacecf7c33efe0fb4db`. Its archived ledger2 provenance remains authoritative; subsequent unrelated shutdown source edits were not attributed to this previously built helper. The exact owned local tunnel and isolated systemd service were stopped afterward; no listener remained (`restore39-cleanup.log`). No live session or membership was changed by this probe. Later production revisions remain intact; revision39 is immutable historical recovery evidence.

### Populated production restart — 2026-11-13, 22:49 UTC

After the coordinator confirmed both clients held saved, unsubmitted round0 drafts, the production Rust service alone was stopped/restarted under an independent90-second restart watchdog and guaranteed-start trap. Caddy stayed running. A verified snapshot taken while the Rust service was stopped fixed the exact current authority, including later canvas edits and removal/reinvitation; a second consistent online snapshot followed hosted readiness. No production restore, membership mutation or sign-out was performed.

The comparison passed: all nine nonroom content tables were identical; the room preserved every artwork/permission/assignment field with exactly one restart and two reconnect presence transitions (room revision38→41). Both checkpoints remained Drawing, round0, zero submissions, with the expected `restart` pause afterward. Eight profile/device/session/admission/account/invitation tables were identical, compared privately in memory without exporting authentication material. Shared canvas **revision42/38 strokes**, canonical hash `a3148dd26e461dfdb9059c2112d5ab26ed82747b3ed569fdfbbaba6f19e0b7ba`, and both private revision1 canvases were unchanged; this deliberately compared the latest authority rather than the old revision39 backup. The service PID changed1227→5139 at22:49:16UTC; Caddy PID1234 was unchanged. Loopback and ordinary public hosted readiness passed; the restart watchdog was inactive afterward. Evidence: `populated-production-restart.log`, `populated-production-restart-result.json` and `populated-production-restart-processes.log` under resumed deployment evidence. The underlying receipt/reconnect suite now has ten passing Linux tests.

The coordinator subsequently confirmed the desktop retained the same game/chain/round/project, local revision2 and visible saved pixels, with both clients reconnected. Normal moderator resume succeeded and the recovered desktop submission was accepted. The owner confirmed both laptop marks survived and continued to the next round; the second full Animation game reached its four-entry reveal and was saved to Gallery. These native/UI observations close the visible saved-draft follow-up, in addition to the server comparisons above; detailed client evidence belongs to the coordinator's desktop acceptance record.

### Read-only post-acceptance usage observation — 2026-11-14, 00:08–00:10 UTC

No resource, service, authority, certificate, object, retention or budget configuration changed during this observation. `post-acceptance-usage-audit.json` and its referenced raw aggregate JSON files under `log/k65/deployment/resume-20261113/` retain the exact measurements. This UTC observation occurred on the owner's local2026-11-13 date.

The30.021-second idle sample ran00:08:10.529–00:08:40.551UTC after native acceptance play. Both service/proxy remained active with unchanged PIDs5139/1234 and no automatic restarts; loopback and ordinary trusted public health returned200/hosted. Service RSS was70,676,480 bytes (67.40 MiB), proxy RSS59,125,760 bytes (56.39 MiB), and host available memory1,578,745,856 bytes (1.47 GiB). During that sample, total host nonidle CPU averaged0.534% across two visible CPUs (iowait separately1.518%); service/proxy consumed0.147729/0.005990 CPU seconds. Cumulative unit CPU31.847/137.538 seconds uses different windows: the service counter begins at its22:49 restart, proxy at20:28, so it is not total acceptance-workload CPU. The sample is an idle observation, not sustained capacity proof.

The data filesystem used32,309,248 bytes with20,908,359,680 available. The live database was4,354,048 bytes; retained recovery/restart evidence19,143,997 logical bytes and the isolated capacity fixture7,783,929 bytes account for most additional occupancy. One failed upload stage remains (233,838 logical bytes) as intentional evidence. Local journals occupied16.0 MiB. The backup timer was active, next scheduled05:07:31UTC. Low used bytes do not reduce billing for the provisioned30 GiB of disks.

At00:09:42UTC, the private bucket contained **six objects/three completed backup pairs totaling7,541,836 bytes (7.19 MiB)**, including all retained versions; the separate soft-delete listing found zero objects/bytes. Creation dates span2026-11-13T20:15:37Z–22:24:27Z. This is current retained storage, not the eventual daily-retention steady state; the conservative32 GiB backup allowance remains in the cost plan.

For the same boot, comparison with the recorded20:30:04UTC baseline spans **3h38m36.551s**. The primary NIC received104,873,453 additional bytes and transmitted309,609,190 bytes (0.28835 GiB). Those counters include IAP administration, uploads, readbacks/restores, backups and system traffic, and are **not a billable internet-egress meter**. The final30-second idle interval added only654 received/600 transmitted bytes. If the entire mixed acceptance/administration transmit rate persisted for730 hours and every byte were billed at the plan's North America egress rate, the diagnostic extension would be57.77 GiB and about$25.66/month including the provisional$1 domain share, before tax/credits. That conditional calculation is neither a normal-play forecast nor an observed bill. The existing5/50 GiB planning scenarios remain$19.33/$24.73; this short observation establishes no material cost overrun and cannot replace representative billing data.

Scoped metadata inspection found BigQuery enabled but **zero datasets in project `komaphone`**, hence no project-local billing export to read. Cloud Billing and Budget APIs were not enabled; none were enabled for this audit, no BigQuery query job ran, and exports in unrelated projects were not searched. Actual gross project spend remains unavailable from these scoped tools. The owner already confirmed the requested$20/$30 project budgets and credits are ready; this audit does not reopen that settled setup. Independently uninspected details are the all-service/gross-credit budget filters, notification delivery, and current credit eligibility/balance/expiry. Actual Billing gross spend, existing-domain renewal allocation and tax remain unavailable inputs. The observed upload403 and retained stage prove a real failed backup attempt; they do not substitute for the separately assigned deliberately interrupted-transfer fixture.

The coordinator also checked the supported browser inventory read-only: no existing authenticated GCP tab/session was exposed through the connected surfaces. No fresh login or unrelated-page inspection was attempted. This limits the available connection while the owner is away; it does not imply that Google's Billing UI does not exist.

### Owner-reported Billing and network clarification — 2026-11-14

The owner reported the Billing display in project `komaphone` context for **November1–13,2026**: **Cost$0.08, Savings$0.08, Total cost$0.00**. These are the reported display labels and amounts. The project-only report filter was not independently inspected, and the label **Savings** is not attributed to a particular promotional credit or discount. This owner observation supplements the earlier scoped-tool availability limitation; it is not an independently retrieved Billing export. The short reporting window and possible reporting lag prevent treating it as a mature monthly cost measurement or extrapolating a monthly bill from it. No account identifier or source URL is retained. Redacted local evidence: `log/k65/deployment/resume-20261113/owner-billing-observation-20261114.json`.

The pre-tax, pre-credit planning scenarios remain **$19.33/$24.73 per month**, including the provisional$1 existing-domain allocation. Actual domain renewal allocation and tax remain qualified inputs; the owner-reported Savings figure does not change those forecast assumptions. The requested$20/$30 project budgets and credits remain settled owner-confirmed setup.

The owner explicitly approved **home broadband plus cellular hotspot, with phone Wi-Fi uplink off**, as the K6.5 independent-public-network gate. The coordinator reports the candidate5 laptop update/reopen, offline submission followed by normal moderator resume, four-frame reveal/Gallery save and explicitly simultaneous drawing confirmed; the shared canvas reached revision44/40 strokes. This supplements the earlier native/server evidence without changing the historical revision39 backup or revision42 restart checkpoints. **Fresh friend onboarding in another household remains K7 work.** No new-household onboarding test is claimed, and earlier references to that pending gate should be read under this owner-approved clarification. This documentation update made no cloud call, configuration change or new test run.
