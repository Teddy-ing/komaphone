# Komaphone delivery plan

Baseline 1.1, 2026-11-04. Implement [the product specification](product-spec.md) in bounded milestones, with [architecture](architecture.md) as the technical starting point and [status](status.md) as the live handoff. A phase is complete only when its acceptance behavior is demonstrated; existing code is a reuse candidate until exercised in the Komaphone flow. Guided tours, catch-up playback, and remix are required in the milestones below.

## Working model

Use one coordinating agent per milestone. It owns integration, shared registration/schema changes, validation, and status updates. Delegate independent implementations and an independent review to subagents. Assign files before editing; serialize shared-file changes. In separate checkouts, integrate prerequisites before dependent work. In a shared checkout, reread files and preserve others' edits. Do not run all milestone prompts simultaneously.

The user should need to judge the experience at two useful points: a playable party build and a deep-zoom demonstration. These are feedback opportunities, not automatic pauses if the agreed behavior is already testable. Routine design, coding, testing, and bug fixing are delegated. External hosting credentials/cost or a material change to the product may require a concrete decision; keep completing independent local work.

## Milestones and acceptance gates

### K0 Baseline and contracts

**Depends on:** this documentation. **Purpose:** make subsequent work buildable and prevent incompatible game/storage/network implementations.

- Inspect the actual checkout/toolchain and repository rules. Record commands, versions, baseline failures, and supported test/snapshot routes; do not trust historical parity counts as current measurements.
- Verify existing mouse drawing, native autosave, frame persistence, and GIF export with focused tests or a reproducible local run. Reuse what works.
- Establish the minimal typed, versioned IDs and envelopes for projects, participants, games, assignments, immutable submissions, assets, and replay/checkpoint references. Define schema migration and resource bounds. Add shared contracts at valid dependency layers; avoid a speculative framework.
- Define the deterministic room state machine and phase-filtered payloads in a headless module, with an in-memory transport/storage test adapter. Separate clock/deadline input from transitions.
- Select a small Rust service/storage/transport stack after a focused maintenance/license/WASM-boundary check. Record the decision and local run commands; no provider purchase or production deployment.
- Agree file ownership and interface boundaries before delegating K1/K2 implementations.

**Done when:** baseline results are recorded; contract tests reject malformed/oversized inputs; two/three-player schedules and legal/illegal transitions are tested; two implementers could use the contracts without guessing. No mock service is labeled production-ready.

### K1 Reliable local drawing

**Depends on:** K0. **Covers:** E1-E4, R1-R3 foundations.

- Build the curated Komaphone workspace while retaining the editor's underlying capabilities and advanced workspace.
- Add stable local project storage, automatic durable edit boundaries, recovery, and accurate local/sync state. Designate a Windows application-data location; keep test data isolated and original files intact.
- Record durable replay information from the first edit, including asset/settings references, undo/redo, and checkpoint fallbacks. The polished replay viewer comes in K4.
- Provide local draft listing/reopen and error handling for disk-full/corrupt writes; final gallery comes in K4.

**Done when:** brush/eraser/fill/selection/transform/text/shapes/import/layers and pan/zoom are usable with a mouse; app restart restores acknowledged saves, including layers and undo-related final pixels. Kill the test app immediately after a save acknowledgment and during a write. Failed writes never falsely report success. Inspect offscreen workspace screenshots at 1280 x 720 and 1920 x 1080, including a high-DPI case.

### K2 Private service and rooms

**Depends on:** K0 and K1 storage integration. **Covers:** L1-L5 and R2/R4 networking foundations.

- Run a local Rust sync/room service with durable metadata/assets, authenticated invitations, membership checks, protocol versions, and bounded payloads. Make a reusable test harness that launches isolated clients/service with independent storage.
- Build lobby/create/join/ready/mode/options flow, service-authoritative transitions, immutable/idempotent submissions, reconnect, and moderator claim after host loss.
- Queue offline draft edits, but do not accept a game submission until the authority commits it. Separate presence from durable data. Persist enough state to resume after either endpoint restarts.
- Implement a deterministic test clock, optional timer/grace policy, paused missing-player state, explicit skip/end, and clear service-unavailable UI.

**Done when:** two and three independent clients join/leave/rejoin a real service; unauthorized clients cannot fetch room assets; duplicate/reordered submissions cannot advance twice; a service restart and host loss restore the correct phase; pending local work survives. The existing automation/control port is never exposed as the multiplayer protocol.

### K3 Both party modes

**Depends on:** K1 and K2. **Covers:** E5, A1-A4, C1-C4 except polished replay/export.

- Delegate Animation and Exquisite Corpse as separate modules using K0 contracts. The coordinator owns lobby/reveal integration and shared UI state.
- Animation: N chains, cyclic assignments, configured rotations, blank/copy-previous choice, adjustable reference/onion skin, metadata notes, immutable accepted frames, and full-chain reveal.
- Corpse: exactly three bands, intentional repeat author for two players, locked 24 px seam strip, correctly assembled reveal, and no premature delivery of hidden segments.
- Persist drafts, assignment ownership, deadlines, notes, and reveal state through restarts. Handle submit races, missing players, and incomplete archives.

**Done when:** full two-player and three-player games finish in each mode through the actual UI and headless integration harness. Inspect requests/payloads/caches and engine command access, not only screenshots, for hidden content. Test seam alignment with colored boundary rows; notes/reference/cursors never enter final artwork. Reconnect during editing, transfer, and reveal.

### K4 Replay gallery and first playable release

**Depends on:** K3 and K1 replay recording. **Covers:** A4-A5, C4, R1-R4, G1, G2 local-project remix.

- Add per-frame/per-segment timelapse scrub/playback and complete-animation playback as separate controls. Include erase, undo/redo, fills, layer edits, transforms, imported assets, and checkpoint steps. Replay finishes at the accepted final image.
- Add gallery navigation/filter/favorites, durable editable archives, thumbnails, incomplete sessions, and recovery status.
- Add Remix into a new local drawing/animation from revealed creatures, animations, or selected frames. Allocate new editable IDs, preserve source revision/credits, and use the copied result as a new replay baseline. Verify edits and deletion of either project cannot corrupt the other; full animations preserve frames/timing and still extraction is explicit. Round-trip provenance and content through archives; deny unrevealed sources.
- Reuse GIF encoding where suitable; add frame-sequence PNG, creature PNG, and portable archive export/import. Export contains no UI/notes unless explicitly selected as a separate metadata view.
- Prepare a friend-usable Windows build and local service runbook. Document backup/restore and service limits. A deployment can be prepared, but provision paid/public infrastructure only under the user's applicable authorization.
- Run a private-network smoke test with two separate Windows machines and the intended service route before labeling the party build friend-ready. Test invitation, one game of each mode, reconnect, and gallery/export. If machines/network access are unavailable, label the build a local-only candidate and retain this release gate as pending; do not defer discovering all network usability issues until K7.

**Done when:** one complete game of each mode can be closed, reopened from the gallery, replayed, exported, remixed, and restored from an archive on a fresh data directory. Verify remix independence, credits, animation timing, and hidden-source rejection. Compare final replay pixels to canonical CPU-rendered results (exact where deterministic, explicit tolerance otherwise). Verify GIF frame order, loop, approximate timing, and color/transparency limits. Review visible UI and independent failure tests. Record the separate-machine smoke result before claiming a friend-ready party release. A locally verified candidate with that gate pending is partial, not complete; the shared world also remains outstanding.

### K5 Deep zoom proof

**Depends on:** K4 local implementation/acceptance; K0/K1 storage contracts reused. An unavailable external-machine smoke test may remain explicitly pending while this independent prototype proceeds, without closing K4. **Covers:** W2/W3/W6 prototype.

- Prototype a hierarchical scene with bounded local drawing coordinates, stable region IDs/transforms, reversible navigation, progressive previews, and viewport-only loading.
- Keep party documents unchanged. Define world format/versioning only after the prototype proves the nested-detail use case.
- Demonstrate six nested drawing regions, each at roughly 100x greater local detail, plus lateral expansion far beyond one party document. Six levels are a test fixture, not a six-level product cap; choose and document bounded depth limits with evidence.
- Draw recognizable shapes at every scale, save/reopen, pan/zoom both ways, and return via bookmarks without coordinate jumps or selecting/reopening separate files.
- Implement single-target automatic zoom with pause/cancel and stable region/local-coordinate camera targets that K6 tours can reuse. Measure memory/frame time and show summaries rather than loading all offscreen detail.

**Done when:** a screen capture or inspected screenshots plus a reproducible test prove new detail survives deep nesting and reopen. Prototype tests hit configured depth/coordinate/size limits safely. Record precision/render/storage tradeoffs. If this fails, report the specific failing property and continue the prototype; do not call bitmap magnification equivalent or silently drop deep zoom.

### K6 Persistent shared world

**Depends on:** K5, K2, and K4 remix/replay. **Covers:** W1-W8 and G2 shared-world remix.

- Integrate multiscale regions with local durability and service sync. Use author-owned layers, stable operation IDs, immutable blobs, deterministic merge/rebase, and explicit metadata conflict handling.
- Add ghost cursors/live stroke previews with expiry; their loss must never lose committed artwork. Include node/scale/camera context so cursors are positioned correctly.
- Add offline editing, reconnect catch-up, acknowledgments, retry/deduplication, bounded caches, bookmarks/home, and selected-view/full-archive exports.
- Personal undo reverses only the author's operations. Reference/clone somebody else's layer before editing it. Defer unrestricted destructive cross-author edits.
- Add saved multi-stop guided tours with stop ordering/timing, play/pause/scrub/cancel, and GIF/PNG-sequence export at a frozen scene revision. Verify nested and lateral paths, save/reopen/sync/archive, missing destinations, concurrent edits, cancellation, output timing/framing, and bounded export resources.
- Add per-viewer reviewed-revision tracking, author highlights, navigation, and replay for Since you were away. Test offline arrivals, erases/deletions/undo, author filters with canonical replay, restart mid-review, first visits, and changes arriving while a fixed interval is being reviewed. Background sync must not mark changes seen.
- Extend Remix to owned world regions, preserving animation frames/timing, provenance, editable ownership, and the source. Test placement at multiple scales, offline retry without duplicate insertion, authorization, and independence after source/clone deletion.

**Done when:** two/three clients draw concurrently; A syncs and exits before B comes online; both separately edit offline then reconnect; all converge to the same canonical scene and artifact hashes without losing a contribution. Repeat with duplicate/out-of-order messages, dropped previews, service restart, client kill, and interrupted uploads. Undo by A leaves B unchanged. Lateral/deep navigation and archive restoration pass on the shared result. B can review A's new work without clearing later unseen edits, replay/export a saved multi-stop tour, and edit a remixed creature/animation in the world without changing its original. All G2/W7/W8 acceptance cases above pass.

### K7 Friend-ready hardening

**Depends on:** K6. **Covers:** all required specification IDs.

- Run a requirements-to-evidence review and fix missing behavior. Inspect the actual Windows UI and packaged launch path, not only unit tests.
- Test fresh install/update with isolated old-format data, invitations on distinct machines/networks when available, recovery, protocol mismatch, quotas, corruption, and backup restore. Upgrade failure must retain an original usable backup.
- Run relevant performance tests, record hardware and workload, and repair regressions. Produce exact quick-start instructions, release notes, known limits, and a recoverable service administration/backup path.
- Prepare the intended private service deployment. If access or a paid provider choice is unavailable, finish packaging/local verification and explicitly label the external-network/deployment gate unverified. Do not declare the deployed product complete.

**Done when:** every required behavior has evidence; both party modes, asynchronous shared-world use, guided tour exports, catch-up playback, and local/world remix pass end to end; a fresh Windows user can follow the runbook. Features outside baseline 1.1 remain deferred.

## Dependency and delegation map

K0 -> K1 -> K2 -> K3 -> K4 -> K5 -> K6 -> K7 is the default delivery sequence. K1 drawing and K2 service internals may overlap after K0 contracts, but K2 closes only after local-storage integration. Within K3 the two mode implementations can run in parallel. Within K4 gallery and replay/export can run in parallel. Independent review/fault tests can accompany every milestone. K5 is intentionally after a playable party release so the hardest feature does not delay game night.

## Verification policy

Follow root `AGENTS.md` final gates: touched-crate tests and clippy, layers, WASM for L0-L6, parity generation and adversarial `panic_hunt` for command changes, corpus checks for affected format/render/I/O crates, and performance checks for hot paths. Add meaningful tests around game transitions, persistence, filtering, network retries, and merge/replay correctness. Do not modify CI as part of feature work.

Documentation-only work uses diff, link, scope, and consistency checks; it does not claim compiled runtime validation. Toolchain/environment failures must include the exact command/error and their effect on confidence. Never convert a blocked or skipped mandatory check into a pass. Screenshots must actually be inspected.

Use isolated test directories and ports. Existing single-instance launch behavior needs `--new-instance`, separate config roots, or the documented control-mode launch route for multi-client tests. Keep automation loopback-only. Never test destructive recovery on personal originals.

## Provisional performance targets

These are acceptance targets to calibrate in K0/K5, not measured claims or permission to weaken existing performance gates. Record hardware, build profile, workload, p50/p95 and limits. Any material target change needs an explained status entry.

| Scenario | Initial target |
|---|---|
| Local drawing | p95 input-to-visible-stroke <= 50 ms on the recorded Windows reference machine, a 1536 x 1024 canvas and 10 ordinary paint layers. |
| Local persistence | Completed ordinary stroke durably acknowledged within 1 second under normal local disk conditions; UI remains responsive during longer checkpoints. |
| Presence | Ghost cursor/stroke preview visible within 250 ms p95 under simulated 100 ms RTT with three active users. Durable correctness cannot depend on this target. |
| Scene rendering | p95 frame time <= 33 ms for the recorded K5 scene on the reference machine; render cost governed by visible detail, not total region count. |
| Resource bounds | Choose explicit per-message, asset, document, replay, cache, and world limits in K0/K5 and test limit failures. No unbounded allocation based on a remote size field. |

Do not promise a calendar completion date or infer Komaphone effort from upstream Photoshop-parity agent-hour estimates. Record actual milestone duration and blockers, then revise delivery expectations from evidence.
