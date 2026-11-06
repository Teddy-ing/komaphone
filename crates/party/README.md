# Komaphone party contracts

`photocraft-party` is an L2, headless, Rust/WASM-compatible domain crate. It has no editor,
UI, filesystem, socket, credential, or implicit clock dependency. It does not implement a
production service. The native editor and its format stay unchanged.
K2 adds editable lobby options/leave, invited late-arrival waiting rooms, next-game
transitions, the bounded HTTP wire schema, and verified replay containers. The service
archives terminal games transactionally before resetting the lobby. Runtime evidence
and incomplete integration gates are tracked in the status document below.

Run `cargo test -p photocraft-party` and `cargo clippy -p photocraft-party --all-targets -- -D warnings`.
The tests complete two/three-player schedules for both modes, including all animation
rotation options, and exercise invalid input, privacy projections, deadlines, reconnect,
immutable retries, missing submissions, bounded snapshots, and failed memory commits.

## Interfaces

- `contracts`: schema-1 typed IDs, project/event envelopes, immutable submission,
  checkpoint/replay/image references, validation and byte/allocation bounds.
- `room::Room`: deterministic authority. `new`, `apply`, `tick`, `set_connected`,
  `restart`, `snapshot_bytes`, `from_snapshot`, `view`, `notes`, and `can_fetch`.
  Clock values are injected monotonic milliseconds. They are never calendar dates.
- `adapter`: bounded in-memory byte transport, SHA-256 object store with actor upload
  grants, atomic compare-and-set room/receipt store, and fault injection. This is a test
  adapter, not a durability or authentication implementation.
- `protocol`: versioned bounded requests/responses; capability-bearing requests omit Debug.
- `replay_artifact`: checkpoint-backed replay metadata and checksum-verified intermediate
  checkpoint packs, preserving complete history without one top-level object per edit.
- `canonical_color`: converts document-profile composite floats to sRGB through the
  shared CMS before client/service PNG encoding; native checkpoint profiles and bit
  depth remain intact. This keeps profile-free shared artwork and seam pixels consistent.

Client requests carry schema version, request ID, authenticated participant, expected
room revision and a typed action. The adapter verifies the principal separately from the
body. Reusing a request ID with any changed field conflicts; an identical retry returns
the stored original outcome even after restart. Submission identity also binds the full
immutable payload, so a new request ID cannot replace accepted art. Mutation uses a candidate
room; response/visible state changes follow a successful atomic store commit.

The service must authorize a join with an invitation **before** invoking `Join`; this crate
does not authenticate invitations. `set_connected`, `tick`, and `restart` are trusted service
inputs. Every persisted restore must validate with `from_snapshot`, then call `restart`
before serving (the memory adapter does this). Restart disconnects members and pauses active
countdowns; reconnect preserves their assignment, then the moderator explicitly resumes.
Only connected members may claim a disconnected moderator, with a revision precondition.

`Start` receives fresh project IDs for every assignment, ordered round-major then chain.
The native adapter allocates cryptographically random 128-bit IDs and persists sequence
allocation with each local event. IDs are lowercase nonzero hex, opaque labels rather than
credentials; their distinct Rust types and containing schema field establish their kind.
Use a new device ID after cloning/restoring an installation before it becomes a writer.

An animation reference contains only the flattened prior frame and its next-editor note.
A corpse reference contains only a service-derived 1024 by 24 strip. Native checkpoints,
replay assets, other bands and earlier references are withheld. Notes are excluded from
ordinary reveal payloads and available via an explicit notes request. All thumbnail,
export, replay and blob endpoints must check the same current projection; knowing a hash
does not authorize fetching or attaching it to a submission. Upload grants prove actual
possession of supplied bytes, not knowledge of a digest.

The fixture seam registration method is a **trusted renderer hook**, never a client
endpoint. The native service decodes bounded checkpoints and independently checks the
flattened image and bottom strip. Synthetic fixture bytes in this crate's tests do not
demonstrate image cropping, pixel correctness, or service privacy by themselves.

Timed play transitions Drawing -> Transfer on an authoritative tick. In Transfer,
`FreezeReport` fixes a participant's last locally durable submission manifest; byte upload
may follow, and `Submit` must match exactly. The 30-second grace labels absent submissions
explicitly. It is a cooperative timing policy, not proof of when offline drawing occurred.
Disconnect pauses progression/countdown. Moderator skip/end creates labeled missing entries;
it never fabricates artwork. Pending draft preservation belongs to K1/K2.

## Versions and limits

`EventEnvelope`, `ProjectRecord`, request, and room snapshots each own schema version 1.
Submission/checkpoint/replay records are nested under those versioned aggregates. Ingress
must use bounded decoding plus semantic validation, rather than raw serde alone. Unknown
versions/fields fail closed. No legacy Komaphone format exists to migrate yet: future readers
must preserve original bytes, migrate into a separate transaction/copy, validate before
activation, and retain the original if migration fails. Do not silently reinterpret a newer
record. This crate neither changes nor replaces `.pcraft` compatibility.

| Bound | Schema-1 value / rationale |
|---|---|
| Players / chains | 2–3 during a game; one member allowed only in lobby |
| Animation | 1–4 rotations; 2 by product default; up to 12 frames per chain / 36 assignments |
| Corpse | Exactly 3 rounds; 2 players intentionally repeat head/legs author |
| Request | 256 KiB before JSON allocation; transport must enforce during streaming |
| Stored room snapshot | 4 MiB; tested at 36 submissions × 256 references; K2 paginates large reveal responses |
| Asset / project manifest | 64 MiB per object; 256 unique refs; 512 MiB total referenced bytes |
| Image | Side at most 8192; 16,777,216 pixels; decoded bytes at most 512 MiB with checked runtime bytes/pixel |
| Names / notes / missing reason | 96 / 1024 / 256 UTF-8 bytes |
| Replay | 1,000,000 events; relative duration at most 7 days; 64 unique dependencies per event |
| Round timer / transfer grace | At most 1 hour / 30 seconds; untimed is the product default |
| Memory harness | 64 queued requests; 4096 retained receipts; 512 MiB asset bytes; at most 9216 objects and 3 grants/object |

These are bounded operational choices for finite party contracts, not edits to the product
baseline or world limits. K1 must add streaming checkpoint/replay storage and tested quotas;
it must not silently discard history at a limit. K5 measures world limits separately.

Resolved replay steps identify versioned canonical payload assets and exact result
checkpoints. Unsupported operations use a checkpoint transition. These envelopes alone are
not a replay recorder/player. K1 records resolved stroke settings/seeds/targets/timing,
undo/redo and every completed edit before Actions coalescing; K4 validates final pixels.

## Next implementation ownership

The coordinator owns shared contracts, manifests, engine registration and UI serialized
state. K1 persistence owns the native durable adapter, device allocation, project records,
replay capture and recovery acknowledgments. K1 workspace owns curated panels/tool access
using existing engine commands. K2 service owns authentication, invitations, durable
room transactions, timers, scoped assets and bounded pagination; native client work owns
transport/background jobs and revision-gap recovery. Shared schema changes go through the
coordinator before either module consumes them.

K2 registers `komaphone.room.*` through the engine seam, including `claim`, `openDraft`
and `submit`. Freeze reports are a worker-internal protocol action bound to the saved
assignment revision. The earlier K0 `komaphone.assignment.*` names were reservations,
never registered public commands; no existing command ID was replaced. See the
[control protocol](../../docs/control-protocol.md) for the implemented interface.

See [service decision](../../docs/komaphone/service-stack.md),
[reuse audit](../../docs/komaphone/k0-reuse-audit.md), and
[durable status](../../docs/komaphone/status.md) for runtime evidence and remaining gates.
