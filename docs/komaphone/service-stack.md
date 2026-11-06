# K0 private-service decision

K2 implementation update: the selected adapters now exist in `apps/komaphone-service`
and `crates/room-client`. Use the [K2 runbook](k2-private-rooms.md) for actual commands,
limits, credentials and backup procedures, and [status](status.md) for verified gates.
The decision and proposed interface below remain the historical K0 record. The current
service intentionally binds only loopback; no TLS listener or external deployment is claimed.

Decision date: 2026-11-05. Scope: select components and adapter boundaries for K1/K2; no service executable, network deployment, production readiness, or crash-durability result is claimed here. The [product baseline](product-spec.md), [architecture](architecture.md), and [delivery gates](delivery-plan.md) remain authoritative.

## Selected mechanism

Use one Rust service process with axum/Tokio, bounded HTTP JSON requests, revision-based polling, and one serialized SQLite writer. Native clients use a reused reqwest client on a background worker. Keep the pure contracts independent of the networking and database crates. Two or three users do not need a separate database server, message broker, peer discovery, NAT traversal, or a general conflict-resolution framework.

Durable mutations return a committed revision and an idempotent receipt. Polling requests carry the last received revision and return bounded participant-specific updates; a gap triggers a permitted checkpoint rather than speculative replay. Begin with 250 ms active-room polls and back off while idle or disconnected. This is an operational starting point, not a measured latency claim or K6 presence implementation. K6 can add an independent bounded ephemeral transport after measuring its 250 ms presence target; durable ordering remains unchanged.

| Component | Selected release observed in primary documentation | License and boundary |
|---|---|---|
| HTTP routing | axum 0.8.9 | MIT; native service only. Its documented MSRV is 1.80. [Package documentation](https://docs.rs/crate/axum/0.8.9) |
| Runtime | Tokio 1.54.1, published 2026-10-21 | MIT; already in this checkout's lockfile. Windows is supported; networking/full features must not leak into the WASM core. [Package/release history](https://docs.rs/crate/tokio/1.54.1), [platform support](https://docs.rs/tokio/1.54.1/tokio/) |
| Native HTTP client | reqwest 0.13.5, published 2026-09-08 | MIT OR Apache-2.0; already in this checkout's lockfile. Use explicit JSON and Rustls features; disable redirects for authenticated service calls. [Package documentation](https://docs.rs/crate/reqwest/0.13.5), [feature/TLS/WASM documentation](https://docs.rs/reqwest/0.13.5/reqwest/) |
| Database adapter | rusqlite 0.41.1, published 2026-10-28; `default-features = false`, `bundled` | MIT; bundled SQLite is public domain. Bundling avoids requiring a separately installed SQLite DLL on Windows but does require a working native C compiler at build time. All application code remains Rust; SQLite is a third-party native dependency. [Package documentation](https://docs.rs/crate/rusqlite/0.41.1), [license](https://github.com/rusqlite/rusqlite/blob/master/LICENSE), [SQLite license](https://www.sqlite.org/copyright.html) |
| TLS listener | axum-server 0.8.0 with Rustls, when enabling non-loopback service access | MIT, documented MSRV 1.82. Keep certificates/keys out of the repository. Native service only. [Package documentation](https://docs.rs/crate/axum-server/0.8.0) |

These current releases and maintained documentation support the selection; they are not a security audit or proof that the combination builds here. Add exact resolved versions to Cargo.lock when the adapters are implemented, check advisories then, and retain dependency licenses in distribution. The selected service/database dependencies are not added in K0; the contract crate has its own minimal dependencies.

HTTP request/response and ordinary polling minimize initial protocol state. WebSockets may reduce later presence overhead but do not solve durable acceptance, privacy, or reconnect. SQLx adds asynchronous SQL machinery and multiple database backends unnecessary for one serialized embedded writer. redb 4.4.0 is a maintained, transactional pure-Rust alternative; selecting SQLite favors established inspection, migration, and backup tooling for the first local recovery proof. Reconsider only with measured build/recovery/performance evidence, while preserving the contracts. [SQLx capabilities](https://docs.rs/crate/sqlx/0.9.0), [redb capabilities](https://docs.rs/crate/redb/4.4.0)

## Storage and acknowledgment contract

Use one local database per application data root and one service database per isolated service data root. Data roots must be local application data, outside OneDrive/network shares and repository directories for real use. Tests use fresh temporary roots. Begin with `PRAGMA journal_mode=DELETE`, `PRAGMA synchronous=EXTRA`, `PRAGMA foreign_keys=ON`, a bounded busy timeout, and verify their returned settings. DELETE/EXTRA is a conservative starting point for a tiny serialized workload; adopting WAL later needs backup/sidecar and latency evidence. SQLite documents the additional DELETE-mode durability supplied by EXTRA and the weaker power-loss guarantee of NORMAL. [SQLite synchronous settings](https://www.sqlite.org/pragma.html#pragma_synchronous)

Store immutable content-addressed objects as bounded BLOB rows initially, alongside metadata and references. Upload chunks remain unreferenced staging rows; stream/hash and validate the completed object before marking it complete. A single transaction verifies all required objects, writes the event/submission, updates the room revision, and stores the deduplication receipt. Return an acknowledgment only after successful commit. This avoids making an untested Windows file rename plus unrelated metadata commit the durability boundary. Large objects must use bounded chunks/streaming, not a single remote-sized allocation. Native format checkpoints remain `.pcraft` bytes, not a replacement editor format.

The database worker owns its connection and serializes writes; handlers wait on bounded queues and receive typed success/failure. The UI thread never waits on SQLite or HTTP. Disk-full, quota, hash mismatch, queue saturation, or transaction failure returns an error while retaining the last readable version and local unsynced draft. Do not retry a permanent error in an unbounded loop. Checkpoints/events and stable device-sequence allocation commit together. Process-kill tests on Windows, immediately after acknowledgment and during a write, are required in K1/K2 before these settings become a durability claim.

Backups use SQLite's online backup API into a fresh destination, with a manifest of schema version and object hashes; restore into a fresh root and verify references/hashes before use. Do not copy just an actively written database file and call it a backup. Keep backup retention independent of service acceptance. [SQLite backup API](https://www.sqlite.org/backup.html)

## Authentication and authorization boundary

- Generate invitation capabilities and device credentials from the OS cryptographic random source; use at least 256 random bits for invitation secrets. Invitation redemption has explicit room scope, expiration, revocation, and use count. Store only a verifier for a high-entropy invitation; never write raw invitations or bearer tokens into ordinary logs or URLs.
- Pairing binds a credential to stable participant/device IDs. Request-supplied display names or actor IDs are not authentication. Store client secrets through the Windows credential adapter, outside repository/config dumps. K2 must select and test that concrete adapter.
- Only loopback tests may use HTTP. Reject non-loopback binds without TLS configuration. Clients verify certificate chains and names; private-network tests use an explicit trusted test CA, never disabled certificate checks. Deployment/provider/certificate operation is a later reviewable choice and remains unauthorized here.
- Asset requests identify an authorized room/game/assignment purpose. Check membership, phase, ownership and the permitted derivative before opening an object. Knowing its hash is insufficient. Corpse clients receive a separately hashed 24 px seam object, never the original segment before reveal. An animation reference permits only the previous submitted frame and separately authorized note.
- Apply body byte limits before JSON parsing, then domain validation before transition/storage. Bound declared and actual upload bytes, image dimensions and decoded bytes, references, event batches, staging storage, upload duration, and read responses. Disable request decompression initially to avoid a second resource envelope. Rate-limit invites, durable requests, uploads and later presence independently.
- HTTP handlers map typed conflict/phase/version/authorization errors to stable response codes. Responses and diagnostics must not echo credentials or unauthorized records. A version mismatch is an upgrade error, not best-effort interpretation.

## Room/protocol risks for K0 and K2

| Risk | Required invariant/test |
|---|---|
| Delayed old-round input | Explicit game/assignment/phase epoch; reject obsolete submissions even if sender still belongs to the room. |
| Duplicate/reordered clicks | Identical request ID and content returns the prior receipt; same ID or device sequence with different content conflicts. Persist receipts in the same transaction as state. |
| Conflicting final revision | One immutable accepted submission per assignment; preserve rejected local variants. Complete object/checkpoint/replay references are required. |
| Roster or moderation race | Freeze ordered roster at start. Membership does not give an in-progress assignment to a late arrival. Moderation claim uses expected room revision; simultaneous claims have one winner. |
| Clock/restart ambiguity | Transitions receive deterministic clock/deadline input. A persisted deadline freeze names its epoch; restart resumes paused until time is re-established. Missing transfer becomes an explicit placeholder. Never infer when offline pixels were drawn. |
| Projection leaks | Before reveal, serialize only the caller's current assignment and allowed reference. Apply the same permission set to asset fetch, thumbnails, exports and automation. Test serialized payloads, not merely hidden widgets. |
| Hidden metadata leaks | No source segment hash, full checkpoint, replay endpoint, or note in a seam-only response. Notes remain absent from ordinary artwork exports and opt-in at reveal. |
| Commit/response disconnect | Simulate lost response after commit; retry returns the accepted revision without a second advance. Restart before commit preserves previous accepted state. |
| Schema evolution | Separate protocol and persisted-record versions. Reject unknown future versions; migrate into a recoverable copy/transaction with original retained on failure. |

K0's in-memory adapter proves domain transitions and filtering only. It cannot prove credentials, HTTP middleware, database flushes, backup restoration, TLS, real reconnect, or Windows filesystem behavior. Those remain named K1/K2 acceptance checks.

## Local commands and future run interface

K0's executable local adapter check is:

```powershell
$env:CARGO_TARGET_DIR = 'target/agent-k0-coordinator'
cargo test -p photocraft-party
```

It runs the pure L2 contracts, room and in-memory adapter tests; it does not listen on a port. The coordinator's [status](status.md) records the environment and actual result. There is no service binary to run at K0. The following is a **proposed K2 CLI contract**, intentionally not a claim of an existing command:

```powershell
$env:CARGO_TARGET_DIR = 'target/agent-k2-coordinator'
cargo run -p photocraft-sync -- --bind 127.0.0.1:43127 --data-dir "$env:TEMP/komaphone-service-test"
```

K2 must implement `--bind` (loopback default; support port 0 in automated tests), `--data-dir` (explicit isolated root), graceful shutdown, and a readiness result containing the actual bound address without secrets. Use distinct roots and ports per concurrent harness. The real service runbook must replace this proposed recipe with a verified command and include actual initialization/pairing steps. Do not reuse or expose the editor's automation/control port. Certificate-file options for non-loopback binding must fail closed when absent or invalid.

K1/K2 ownership boundaries: one coordinator owns shared schema/version changes, workspace manifests and engine registrations. A storage implementer owns the native database adapter and crash harness; a service implementer owns HTTP/auth/room persistence wiring; a client implementer owns native background transport and UI status plumbing through the existing platform service seam. No adapter may silently weaken the pure validation/phase contract to make a request succeed.
