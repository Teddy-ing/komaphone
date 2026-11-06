# Komaphone technical plan

Status: K0 contracts implemented in [`photocraft-party`](../../crates/party/README.md) and baseline validation passed; see [status](status.md). Later components below remain planned. Baseline 1.1, 2026-11-04. Read the [product baseline](product-spec.md) first; it owns behavior and scope. This document chooses mechanisms for Windows desktop, a small private service, Animation and Exquisite Corpse first, and a later shared world with required tours, catch-up playback, and remix.

## 1. Preserve the editor; add product boundaries

Keep the Rust engine, finite layered documents, native egui/wgpu shell, and existing format compatibility. Add a focused workspace instead of removing advanced subsystems. Game assignments and shared-world ownership constrain command dispatch, including automation; hiding menu items alone is insufficient.

Existing code provides useful components, not the new product:

| Evidence | Reuse and limitation |
|---|---|
| [Document timeline](../../crates/doc/src/video.rs), [timeline commands](../../crates/engine/src/cmd/timeline_cmds.rs) | Frame/playhead and video surfaces can support playback. They do not implement game rotation or handoff privacy. |
| [Video commands](../../crates/engine/src/cmd/video_cmds.rs) | Existing animated GIF rendering is a candidate export path; verify timing, palette behavior, resource limits, and source preservation. |
| [Recovery store](../../crates/format/src/autosave.rs) | Reuse incremental document writing and completion reporting. Its temporary recovery lifecycle does not replace permanent game/gallery storage. |
| [Undo history](../../crates/ops/src/lib.rs) | Whole-document snapshots work for an isolated drawing. They must not rewind a shared world containing other authors' edits. |
| [Actions recording](../../crates/engine/src/actions_record.rs) | Existing command capture helps integration, but rewrites target IDs and coalesces edits. It is not the canonical collaboration/replay log. |
| [Geometry](../../crates/geom/src/lib.rs) | Current rectangles and tile coordinates use finite `i32` values. Sparse raster tiles do not create recursively finer drawing detail. |
| [UI services](../../crates/ui-egui/src/services.rs), [layer rules](../../xtask/src/layers.rs) | Keep platform effects injected and lower layers independent of UI libraries. Register any new crates before using them. |

## 2. Components and dependency direction

Use separate domain boundaries for finite drawings, games, and the shared scene. A drawing remains a normal editor document. A game references drawings, assignments, accepted submissions, notes, and reveal state. A world references a hierarchy of bounded drawing regions and author-owned layers; it is not one enormous editor document.

| Component | Planned responsibility and placement |
|---|---|
| Party domain | Pure serializable room/game rules, rotation, assignment permissions, and transitions; L2 or lower, without engine/UI dependencies. |
| Scene/sync model | Region topology, identities, ownership, event validation, dependencies, and merge decisions; L2 or lower. |
| Storage adapters | Durable metadata, assets, checkpoints, migrations, and archives; L4, reusing L3 native format where appropriate. |
| Engine integration | Command modules under the existing `crates/engine/src/cmd/` convention; L5 resolves editor operations and enforces editing context. |
| Product shell | Lobby, constrained drawing workspace, reveal, gallery, world navigation, and serialized view state; L6. |
| Desktop application | L7 wires filesystem, credentials, transport, and background jobs through service interfaces. |
| Private service | Separate Rust application, using domain/storage components; room authority, access checks, durable sync, and presence relay. |

These are dependency responsibilities, not permission to create every crate immediately. Choose module boundaries during the first implementation spike and document actual paths. No lower layer may depend upward; a storage adapter does not invoke the engine to replay arbitrary commands.

The core remains WASM-checkable. Native filesystem/network implementations are target-gated or injected; browser multiplayer and browser recovery remain deferred. No JavaScript UI, Tauri, Electron, or webview shell is introduced.

## 3. Persistent records and identities

Use schema versions from the first written record. Minimum logical entities:

- `Profile` and `Device`: stable opaque IDs, display labels, pairing credentials, and persistent sequence allocation. Display names confer no permissions.
- `Room` and `Game`: membership, frozen roster/order, mode/options, moderator, current phase, authoritative revision, deadlines/pause state, and transition history.
- `Assignment`: game, chain, round, player, editable drawing ID, permitted reference IDs, and submission state.
- `Submission`: assignment ID, immutable accepted revision, document/checkpoint/asset hashes, optional note reference, author, and acceptance revision.
- `Drawing`: editable document, durable revision, asset references, replay baseline and events; provenance identifies an import without inventing its history.
- `World`, `Region`, and `Layer`: stable IDs, parent/scale/local placement, ownership, ordering, content revisions, and referenced summaries/checkpoints.
- `GalleryEntry`: references originals and metadata; thumbnails and export files are regenerable derivatives.
- `RemixProvenance`: source artifact/revision, original authors, copy identity, and initial replay baseline. Keep credits even if the source is later unavailable.
- `Tour`: world ID, owned route revision, ordered stable camera targets, travel/hold durations, and export settings. An export job pins its route and world revisions plus required assets.
- `ReviewProgress`: viewer/world identity, last explicitly reviewed accepted revision, and resumable fixed review interval/playhead. Background sync acknowledgments are separate.

A durable event envelope contains `schema_version`, `event_id`, `actor_id`, `device_id`, `device_sequence`, `aggregate_id`, `base_revision`, dependency IDs, operation kind, resolved payload/asset hashes, and monotonic relative replay time. Service acceptance adds an aggregate revision. Wall-clock dates use `chrono` for presentation/audit, never conflict ordering.

Persist sequence allocation with the local event. A restored/cloned installation receives a new device identity before writing. An existing event ID with identical content is a retry; the same ID or device sequence with different content is rejected and retained for diagnosis/recovery.

Asset objects are immutable and content-addressed. Validate hash and length before making them reachable. A transaction may reference only complete objects. Metadata commits and event acceptance are atomic; interrupted uploads remain unreferenced staging data until retried or safely cleaned up.

## 4. Local durability and recovery

The user can draw without the service. Each completed edit enters a local durable transaction containing the event and the information needed to restore its result. Larger documents use checkpoints and immutable asset references rather than repeatedly copying all pixels.

The UI separately tracks unsaved input, confirmed local revision, pending synchronization, and last service-acknowledged revision. Enqueue success is not storage success. Submission acceptance is a further distinct state.

The storage spike must demonstrate transactional recovery, checksummed/bounded records, stable flush semantics, and atomic replacement on Windows. Choose the database/file library after that proof; do not build an untested custom database merely to avoid a dependency.

On launch, recover the last complete committed state and preserve an incomplete/corrupt tail for diagnosis. Never replace the last readable checkpoint with a failed update. Disk-full and quota failures keep the editor/draft recoverable and show an actionable save failure.

Game drafts have permanent IDs and are not deleted when upstream temporary autosave entries are discarded. Normal close flushes pending durable work where possible. Process-kill tests define the guarantee: every locally acknowledged edit survives; an unfinished or unacknowledged portion may not.

Checkpoints accelerate recovery but do not silently erase required drawing replay. Compaction preserves the canonical result and the retained replay stream. Unsynced events/assets are never pruned. Recoverable trash precedes permanent deletion; garbage collection follows references and retention rules.

## 5. Service authority, ordering, and interruptions

Choose service-mediated connections for the initial product. The service is the durable room authority and store-and-forward destination for worlds. It permits asynchronous handoff without either friend's desktop staying online. Direct P2P, automatic host migration, and provider-specific deployment are outside this baseline.

Each room/world has a serialized acceptance stream. Requests carry an idempotency key, expected revision when needed, actor identity, and explicit target. Validate membership, phase, ownership, bounds, dependencies, and assets before committing. Broadcast only after durable acceptance.

Retries return the original accepted result. Duplicate or reordered delivery cannot advance a round twice. A client detects missing revisions and requests a bounded delta or checkpoint; it never fills gaps with guesses. Incompatible protocol/schema versions receive a clear upgrade error.

The moderator is a role, not the server. On moderator disconnect, pause the game. Another connected participant may claim the role through a compare-and-set transition on the current room revision; competing claims produce one winner. The service restores the same committed room after restart.

Roster/order freeze at game start. Late arrivals wait. A disconnected player retains their assignment and draft; progression pauses by default. Explicit moderator skip/end produces a labeled incomplete result. Reconnect reuses stable identity and assignment, not a new player slot.

For timers, the service owns the deadline and pause state. A deadline freeze message identifies the phase epoch; each connected client freezes and reports its last locally durable revision and asset manifest, then submits them during a bounded transfer grace. The service cannot know an unsynced local revision in advance or retrospectively prove when offline input was drawn; timing is cooperative private-play behavior. Missing payloads become explicit missing submissions. Delayed messages from an old phase cannot alter a new assignment. Persist timer state and start paused after an uncertain restart rather than silently expiring work during downtime.

During service outage, existing local drawings and world edits continue; room advancement and authoritative submission acceptance wait. Local drafts survive rejected submissions. A service acknowledgment means persisted service state, not proof of a separately tested backup.

## 6. Assignment privacy and submission boundaries

Produce a participant-specific projection on the service. Clients receive only their editable assignment and currently allowed references. Do not download hidden originals and rely on masking them in the shell.

Animation supplies only the immediately previous submitted frame and its next-editor note. Onion skin and side-by-side images are read-only overlays. Copy previous frame creates new editable content with explicit provenance; it cannot mutate the accepted frame.

Exquisite Corpse supplies a derived bottom 24 px strip of the preceding segment, never its full image. The editable document contains only the current 1024 x 512 band. The overlay sits outside that band, so assembly concatenates three bands without duplicated reference rows.

Notes, author labels, cursors, and reference overlays are not document pixels. Ordinary image exports and replay exclude them. Reveal projections unlock the appropriate accepted artwork; notes remain separately requested metadata.

Apply the same scope checks to thumbnails, exports, automation inspection, command dispatch, and asset fetch endpoints. Advanced workspace operations cannot access other assignments. Previously permitted personal drafts/recovery files may already exist on a participant's device; preserve them without surfacing forbidden prior content in the party UI. No protocol can erase a player's prior knowledge. A service administrator can access stored content; this design does not claim end-to-end encryption.

Submission atomically binds the assignment to a locally durable final document revision, replay endpoint, and complete referenced assets. Accept it once. A later retry with a different revision returns an explicit conflict and preserves the local variant; it does not silently replace accepted art.

## 7. Drawing replay is separate from undo and animation

Record new Komaphone drawings from their first edit, before building the replay viewer. Capture canonical events before Actions portability/coalescing transforms lose exact targets. Events resolve brush parameters, random seeds, colors, selection/target context, assets, and operation version.

Strokes include ordered samples and relative timing for progressive playback. Erasing, fills, layer edits, transforms, and undo/redo are actual visible events. Imported images appear as a single insertion step; imported legacy documents begin at a saved baseline.

An operation without a stable replay implementation records a persisted result/checkpoint transition. It must still visibly appear in the correct sequence. Do not replay filesystem commands, arbitrary external effects, or depend on an asset's current local path.

Canonical checkpoints/patches retain exact accepted results when algorithm versions change. Playback may interpolate stroke progress, but its final result must match the recorded accepted pixels. Validate this for brush randomness, erasing, fills, transforms, imports, layer visibility, and undo/redo.

Undo history may be trimmed for interactive memory limits without deleting the independent retained replay. Reveal replays drawing creation; animation playback shows completed frames. Export keeps these two timelines distinct and excludes notes/presence.

Remix copies a permitted source revision into fresh project/object/layer IDs and editable ownership. Immutable blobs may be shared with reference tracking, but mutable state never is. Preserve animation frames/timing and attribution; explicit still-frame extraction is a separate choice. The imported result starts a new replay baseline with a provenance link, not a forged history. Validate source visibility before copying, preserve referenced assets when either original or copy is removed, and make retries of a world insertion idempotent. World regions must render and retain animated remixes as well as still artwork.

## 8. Shared world and author-local editing

The initial world model uses bounded local drawing regions in a hierarchy. Region placement records a parent ID, a scale level/relationship, and local coordinates; a viewport transforms only a bounded neighborhood. Scene traversal is acyclic and depth-bounded. Do not repeatedly multiply unbounded floating-point transforms across the entire hierarchy.

Regions use editable raster leaves and progressively cached summaries. Drawing a finer scene creates a deeper region; magnifying existing pixels alone is not completion. Neighboring regions provide lateral expansion. Navigation crosses boundaries without opening separate modal documents.

Each editable layer has one owning device actor in the initial model. Other participants see it but cannot destructively modify it. Copying another author's work creates a new owned layer with provenance. Multiple devices for one person use separate writer identities/layers until an explicit ownership transfer is supported.

Local operations on an owned layer form a sequential stream. Reconnect uploads dependencies and operations, retaining author order. The service applies independent layer streams in a canonical order and supplies aggregate revisions. Clients rebuild affected content from checkpoint plus accepted events when necessary.

Shared topology/order/metadata operations carry revision preconditions. Conflicts preserve a recoverable proposed variant and expose a deterministic accepted result; they are not resolved by last wall-clock timestamp. Offline region creation uses fresh IDs and immutable parent references. Invalid/deleted parents require recovery/reparenting, not silent data loss.

Personal undo targets an author's operation on their owned layer and appends a compensating/rebuild event. It never restores a whole-world snapshot. Recompute only the affected owned content as required; preserve other authors' layers and accepted events. Flattening, deleting another author's layer, and arbitrary shared destructive operations remain deferred.

Ghost cursors and unfinished stroke previews use a bounded ephemeral channel with expiry. They may be dropped or throttled; they never determine durable state. Confirmed strokes use the durable stream and replace their matching previews by stable stroke ID.

Catch-up playback derives from accepted world revisions, not wall-clock dates or presence. Keep a per-viewer watermark advanced only by an explicit reviewed/mark-seen action; first visit establishes a labeled baseline. Freeze each review's end revision so later arrivals remain unseen, and persist its playhead across restarts. Include offline uploads when accepted, plus erases, deletions, remixes, and undo. Reconstruct the canonical scene from checkpoint/events in a read-only playback context; author filters affect highlights/navigation, not causal event application. Index affected region IDs/bounds for deep-location jumps, retaining historical placement for deleted content. Replay retention/compaction must preserve this functionality rather than silently skipping old changes.

## 9. Deep-zoom proof before full-world implementation

Gate the world feature on a small native prototype with no production networking dependency:

1. Draw the hut -> pipe -> tiny house -> further scene example across six nested drawing regions, each at roughly 100x greater local detail, then extend it further within declared limits. Six levels are the required test fixture, not a product cap.
2. Pan into new neighboring regions, create finer content there, save, close, and reopen with stable placement and recoverable edits.
3. Demonstrate cursor-centered zoom, parent/home navigation, bookmarks, and a cancellable single-target automatic zoom path.
4. Verify no precision jumps, seams caused by duplicated content, or accidental edits to the wrong scale; test extreme/invalid transforms and cyclic input.
5. Measure viewport frame time, loaded regions/tiles, memory, disk growth, and summary regeneration under a declared representative fixture and reference machine.
6. Prove rendering/loading work follows visible regions and cache budgets, not total world size; reject excessive depth or exhausted storage gracefully.

Record the coordinate/scale scheme, supported depth/range, benchmark fixture, limits, and migration consequences before freezing its storage format. If the prototype fails, revise the mechanism rather than labeling ordinary raster zoom as the requested capability.

World exports include a selected viewport/region PNG, a portable full-world archive, and required guided-tour GIF/PNG-sequence exports. No full-world flat image promise is made for open-ended extent/depth.

K6 extends K5 camera targets into persisted multi-stop tours. Store region IDs, bounded local positions/zoom, durations, and route order rather than screen pixel coordinates. Use a common camera-path evaluator for preview and offscreen export, with bounded prefetch/caches and per-frame rendering. A tour export pins the scene/route revisions and animation time origin for consistent frames despite concurrent edits; validate that all referenced assets exist. Missing/deleted targets offer repair or explicit skip, never an unexplained jump. Cancellation or output failure leaves world/tour state intact. Sync authored routes with revision preconditions; keep viewer playheads local. Include tours and their references in archive/restore tests.

## 10. Private service, limits, and delivery proof

The initial transport/provider/library spike selects maintained Rust-compatible components after checking current primary documentation, licensing, Windows behavior, and required native/WASM boundaries. Compare the simplest authenticated service connection and storage setup; do not make P2P/NAT traversal a prerequisite.

Pair devices through an unguessable invitation capability with expiry/revocation, then store credentials outside repository files and ordinary logs. Use authenticated encrypted transport outside localhost. Scope every asset request to membership/assignment authorization; hash knowledge alone is not access permission.

Set and test bounds on messages, image dimensions, frame counts, notes, event batches, decompression, archive paths, recursive depth, upload duration, and per-device/per-world storage. Rate-limit presence separately. Reject malformed data without panics and preserve already committed state.

Back up service metadata and referenced assets consistently; include schema/version manifests and restore verification. Portable archives carry originals, events/checkpoints, assets, and provenance. Imports validate paths/hashes and create recoverable copies instead of overwriting newer live state.

Acceptance evidence includes two real app instances completing both modes, 2-/3-player rotation, lost/reordered/duplicate messages, process kill after save acknowledgment, service restart, disk-full handling, privacy projections, replay final-image checks, offline merge convergence, and personal undo preserving another author's drawing. Run repository-required checks for affected crates and inspect UI screenshots.

All implementation and local service tests can proceed under the staged plan. Purchasing hosting, opening public access, and production deployment remain a later concrete operational decision. This document does not authorize those actions or claim any planned component already exists.
