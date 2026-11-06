# Komaphone local private service

`photocraft-sync` is a separate Rust service. It never uses the editor automation port.
K2 supports **loopback HTTP only**; all non-loopback addresses fail closed. TLS and the
separate-Windows-machine private-network route must be implemented and verified before
the K4 friend-ready claim. This is not a public or paid deployment.

From the workspace with the Rust/MSVC environment loaded:

```powershell
$env:CARGO_TARGET_DIR = 'target/agent-service'
cargo run -p photocraft-sync --offline -- --bind 127.0.0.1:43127 --data-dir "$env:LOCALAPPDATA/Komaphone/service"
```

Use a fresh directory under `$env:TEMP` for tests. Real data belongs on a local disk,
outside OneDrive/network shares and the source checkout. The service prints a single
readiness JSON record with the actual bound address; `--bind 127.0.0.1:0` selects an
isolated ephemeral port. Ctrl+C closes the listener and joins its database worker.
`--quota-mib` defaults to 2048; exhausted storage returns a failure without deleting data.

The native Komaphone client pairs a generated 256-bit credential with its stable local
participant/device IDs, then creates a room or redeems a privately supplied invitation.
Display names confer no authority. Invitations are room scoped, expire within 24 hours,
allow one or two redemptions, and can be revoked by the moderator. Secrets travel only in
the bounded POST body or Authorization header; the database stores SHA-256 verifiers,
never raw credentials/invitations. Request diagnostics do not echo secrets.

## Wire and durable boundaries

The shared contract is `photocraft-party::protocol`: `POST /v1/rpc`, version 1,
`Authorization: Bearer <credential>` except the initial Pair operation. The native
client polls the current participant-specific room projection. HTTP bodies are capped
at 256 KiB before JSON parsing; compressed and browser-origin requests are rejected.
Every authenticated read checks membership and phase. Asset hashes are identifiers,
not capabilities. Authentication, invite, upload, and general request rate buckets are
bounded separately. The HTTP/worker queue and active request body reads are capped at 32.

Room mutations, invitation uses, immutable asset completion, and idempotency receipts
commit through one SQLite writer using DELETE journaling, synchronous EXTRA, foreign
keys, a checked page quota, and a 2-second busy timeout. No acknowledgment precedes a
successful commit. A lost reply retries the same request ID and content. A changed body
with that ID conflicts; retries retain the original acceptance outcome while returning
the **current authorized projection**, so old receipts cannot reintroduce old references.

Uploads use 32 KiB chunks into bounded SQLite incremental BLOBs. An entire declared
object is reserved against the quota; completion streams SHA-256 verification. Objects
are inaccessible until complete. Matching overlaps can retry; changed overlaps, gaps,
incorrect hashes, and lengths fail. Interrupted uploads survive restart and long
offline intervals. Staging data remains quota-bounded until retried; K2 never prunes
incomplete uploads behind a retained client retry. Each HTTP body has a 10-second deadline.

Submission validation loads bounded native checkpoints (8 MiB manifest, matching the
local draft store; 64 MiB per decoded blob and 512 MiB total), renders with the canonical CPU
compositor, converts its document-profile colors to sRGB through the shared party/CMS
helper, and independently encodes the flattened PNG and corpse bottom 24 rows. Native
checkpoint depth, profile and pixels remain intact. Submitted image and seam bytes must
equal this metadata-free, sRGB canonical encoding; an unconverted linear-RGB preview is
rejected. If a conversion cannot be constructed, the local original remains recoverable. This
both proves pixel correspondence and rejects ancillary/trailing data smuggling. The
decoder never processes a client-supplied PNG. Nonzero replay histories require the
complete versioned replay artifact, with all checkpoint objects, sequential revisions,
IDs, timing, dimensions and endpoint validated. Intermediate checkpoints can share bounded
64 MiB packs; each member's hash, length and native dimensions is checked before acceptance.
Missing, duplicate and unrelated members fail. This preserves long drawing histories without
consuming one top-level asset slot per edit. Recorded command parameters are data;
the service never executes them.

Connected participants refresh a 5-second heartbeat through successful room operations.
Heartbeat observations are ephemeral; disconnection pauses and moderator changes are
durable. Deadlines and grace use the deterministic room clock. Production time starts
from `chrono` UTC milliseconds and advances with `Instant`; opening existing state
calls `Room::restart` and pauses countdowns. Invitation timestamps retain their UTC
origin across service restarts. Tests inject clock values without a network clock route.

Operational limits: 256 paired identities, 128 rooms, 128 invitations per room,
65,536 retained mutation receipts, 256 archived games per room, 64 MiB per object and
512 MiB total stored object bytes per room, within the configured database quota.
Limits reject growth and preserve existing data; no automatic destructive cleanup is
provided. Prior game snapshots remain in the archive table, with original game/project
IDs protected against reuse. Gallery access and portable editable archives are K4 work.

`Store::backup` uses SQLite's online backup API into a new destination, then validates
room snapshots and hashes of complete objects. It refuses to overwrite an existing
destination. `Store::open` validates schema, database integrity, bounded snapshots, and
complete object hashes before serving or updating restored rooms. Keep a separate copy
of the original database when diagnosing corruption; failures do not replace it.
The K4 runbook will add portable archive/administration interfaces and retention policy.

## Acceptance harness

```powershell
cargo test -p photocraft-sync --offline --no-fail-fast
cargo clippy -p photocraft-sync --all-targets --offline -- -D warnings
```

`tests/http.rs` launches real isolated TCP listeners and two/three independent HTTP
clients for both modes, with native U16 checkpoints and real assets. It exercises
privacy, stale parallel submissions, exact retries, host heartbeat loss, moderator
races, and restart. `tests/independent.rs` kills actual child processes after committed
acknowledgment and while a flushed SQLite transaction has an on-disk rollback journal.
`tests/assets.rs` covers complete games, forged strips, hashes, partial uploads, replay,
receipt privacy and backup. `tests/storage_failures.rs` forces actual SQLite page-quota
failures, future schemas, malformed/oversized snapshots and corrupted complete objects.
Run these tests with local TCP permission; a sandbox socket refusal is not a pass.

These tests establish K2 service behavior. They do not claim K3 game UI completion,
K4 gallery/export, private-network TLS usability, operating-system power-loss guarantees,
or off-device backup retention. The milestone ledger contains exact accepted runs.

## Dependency review — 2026-11-05

Reviewed the [official RustSec advisory database](https://github.com/RustSec/advisory-db)
at commit `26fd2d43f7280a1fb925a3743ec463017afe7119` (repository commit date
2026-11-05), downloaded with `git clone --depth 1` into the ignored K2 evidence directory.
Scope was the seven selected packages (axum 0.8.9, axum-core 0.5.6, matchit 0.8.4,
keyring 3.6.3, reqwest 0.13.5, Tokio 1.54.1 and rusqlite 0.41.1), plus every registry
package/version newly present relative to checkout commit `69a9aacb566e036264a93ed6da925cf96eeda0bf`.
This conservatively includes K1 additions: **24 exact package versions** in total.

All 11 matching RustSec records were parsed and manually compared to their published
patched ranges; all selected versions are within those ranges. This includes
[Tokio RUSTSEC-2026-0384](https://rustsec.org/advisories/RUSTSEC-2026-0384.html),
patched at the selected 1.54.1, and
[RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html), patched
from 1.53.3. Other matches concern historical axum-core, rusqlite and libsqlite3-sys
versions. There were no affected matches in this scope, and no unparsed matching files.
This is a scoped source review, **not cargo-audit, a whole-lockfile audit, a code/security
audit, or proof of no vulnerabilities**. Unchanged transitive packages and later advisory
updates require a separate release review.

License evidence came from the exact downloaded crate manifests and neighboring license
files, retaining SHA-256 hashes in `log/k2/dependency-licenses.json`. axum/axum-core,
Tokio and rusqlite use MIT; keyring and reqwest use MIT OR Apache-2.0.
**matchit 0.8.4 is MIT AND BSD-3-Clause**: retain both `LICENSE` and
`LICENSE.httprouter` in distribution. Remaining scoped packages use MIT/Apache-2.0
alternatives, except byteorder's Unlicense OR MIT. libsqlite3-sys bindings use MIT;
its bundled SQLite 3.54.1 source is [public domain](https://www.sqlite.org/copyright.html).
No new asset, font or upstream brand mark was added. This review does not replace the
complete third-party notices required for a packaged release.

Evidence: `log/k2/dependency-review-input.json` records versions, original advisory
metadata and the database ref; `dependency-review-result.json` records all 11 dispositions
and no-record package names; `dependency-licenses.json` records exact package license files.
The ignored database checkout makes the review reproducible without a moving web search.
