# Permanent local Komaphone drafts

`photocraft-local-store` is an L4 library. Portable request/result types remain
WASM-checkable; the SQLite implementation is native-only. The engine owns the
bounded background worker and must keep the exact `SaveRequest` until its matching
`SaveAck`. Scheduling a request is not a saved revision.

`Store::open` takes the database **file** path plus `StoreOptions`. The desktop app
chooses its local application-data root. Tests use independent temporary roots;
real data must stay outside OneDrive, network shares and repository directories.
The API does not delete documents, history or unsynced work.

## Transaction and replay contract

Every `save` transaction commits the native `.pcraft` result checkpoint, canonical
replay payload, validated K0 event envelope, stable device sequence and project
head. A cryptographically random actor/device identity is persisted when the
database is initialized. Caller-provided event IDs are idempotency keys: identical
retries return the original receipt, while changed data or a stale base revision
conflicts. `SaveAck` is returned only after SQLite commit succeeds.

SQLite runs in verified `journal_mode=DELETE`, `synchronous=EXTRA`, and foreign-key
mode with a two-second busy timeout. Checksummed immutable objects use at most
64 KiB per BLOB. Native ZIP entry boundaries determine chunk boundaries, so a
changed manifest or tile does not shift every later tile and defeat deduplication.
`PcraftWriter` retains its bounded compression cache. All checkpoints and replay
records remain reachable; this is permanent storage, separate from upstream
temporary autosave cleanup.

`list` returns draft metadata. `load` validates event/checkpoint associations and
checksums, then loads the newest readable native checkpoint. A corrupt tail is
preserved. Recovery returns both the recovered revision and original head plus
an actionable warning; writing against the recovered revision conflicts. The
shell saves subsequent edits as an independent project. `load_revision` reads a
retained checkpoint; `replay_page(project, after_revision, limit)` reads canonical
records and rejects gaps. No earlier history is invented for imported baselines.

## Bounds and failures

| Resource | Native limit |
|---|---|
| Database | Default 2 GiB, configurable 64 KiB–64 GiB; SQLite page limit |
| Drafts | 1,024; no automatic pruning |
| One native checkpoint | 64 MiB compressed; 512 MiB aggregate decoded data; 64 MiB per decoded blob and 8 MiB manifest |
| Drawing | K0 bounds: 8,192 px side, 16,777,216 pixels; 4,096 layers and existing native-format group-depth limit |
| Replay payload | 8 MiB, nesting 64; envelope remains within K0's 256 KiB bound |
| Replay page | Up to 128 records and 16 MiB serialized payloads |
| Replay duration/events | Existing K0 limits: 7 days / 1,000,000 events per project |
| Recovery scan | Latest 256 revisions; older originals remain retained for manual recovery |

The database quota excludes the temporary rollback journal: SQLite may need extra
free disk space while writing. Disk-full (`SQLITE_FULL`), exhausted quota, corrupt
objects and incompatible schemas return errors without a success acknowledgment.
Before committing, the worker validates every generated checkpoint through the same
bounded native loader used to reopen it. Compressible metadata cannot receive an
acknowledgment for a checkpoint that those reopen limits would reject.
The current in-memory drawing stays the caller's responsibility until retry or an
external save succeeds. Unsupported/future schemas and unrelated databases are
left intact. There is no preexisting local-store schema to migrate in K1; a future
migration must preserve the source and activate only a verified new transaction.

The Windows crash tests exercise process termination, not power removal or a
physical failing drive. Local recovery does not protect against disk loss. Full
portable archives, explicit backup/restore with new device identity after cloning,
and recoverable deletion remain their assigned later gates.

## Runnable storage evidence

From a normal Rust/MSVC environment:

```powershell
$env:CARGO_TARGET_DIR = 'target/agent-local-store'
cargo test -p photocraft-local-store
cargo clippy -p photocraft-local-store --all-targets -- -D warnings
cargo check -p photocraft-local-store --target wasm32-unknown-unknown
```

`tests/process_crash.rs` launches the real test executable as a child and kills it
immediately after a returned save acknowledgment, then separately during an
uncommitted transaction after asserting a real rollback journal and more than
2 MiB of spilled database pages exist. Reopening checks revision, layers and
replay; retry after rollback also checks the device sequence.

`tests/store.rs` exercises 8/16/32f round trips, undo/redo payload retention,
idempotency, competing writers, monotonic replay time, native-entry deduplication,
real SQLite page-exhaustion (`SQLITE_FULL`), corrupt payload fallback, envelope
checksum/schema/association validation, malformed input and preserved future or
foreign databases. The coordinator records executed results and app-level
acceptance separately in [status](../../docs/komaphone/status.md).

SQLite is a bundled third-party dependency, not application C code. rusqlite is
MIT licensed and SQLite is public domain; retain dependency notices when packaging.
See the [selected service/storage mechanism](../../docs/komaphone/service-stack.md).
