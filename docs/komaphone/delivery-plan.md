# Komaphone delivery plan

Baseline 1.4, 2026-11-08. Implement [the product specification](product-spec.md) in bounded milestones, with [architecture](architecture.md) as the technical starting point and [status](status.md) as the live handoff. A phase is complete only when its acceptance behavior is demonstrated; existing code is a reuse candidate until exercised in the Komaphone flow. Tours, catch-up, remix, continuous at-scale drawing, and multiple named canvases with arbitrary independently selected member lists are required. Example group sizes are test fixtures, not product limits. GCP hosting and Google sign-in are required under H1-H4 and the [hosting plan](hosting-plan.md): K6 foundations, K6.5 hosted access, K7 hardening. Target recurring USD $20/month or less; approximately $30 is acceptable.

## Working model

Use one coordinating agent per milestone. It owns integration, shared registration/schema changes, validation, and status updates. Delegate independent implementations and an independent review to subagents. Assign files before editing; serialize shared-file changes. In separate checkouts, integrate prerequisites before dependent work. In a shared checkout, reread files and preserve others' edits. Do not run all milestone prompts simultaneously.

The user should need to judge the experience at useful points: a playable party build, a deep-zoom demonstration, and hosted play on independent public internet connections, followed by fresh friend onboarding in K7. These are feedback opportunities, not automatic pauses if the agreed behavior is already testable. Routine design, coding, testing, and bug fixing are delegated. GCP, Google sign-in and the stated cost range are chosen; ask only for genuinely missing project/access/deployment decisions or a material cost/scope change after preparing a concrete reviewable result. Keep completing independent local work.

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

Baseline 1.4 clarification: this gate proves private-network play only. Hosted internet readiness requires K6.5 on independent public internet connections under the owner-approved H4 clarification, without VPN/manual certificates; do not generalize the K4 LAN result to that experience.

**Done when:** one complete game of each mode can be closed, reopened from the gallery, replayed, exported, remixed, and restored from an archive on a fresh data directory. Verify remix independence, credits, animation timing, and hidden-source rejection. Compare final replay pixels to canonical CPU-rendered results (exact where deterministic, explicit tolerance otherwise). Verify GIF frame order, loop, approximate timing, and color/transparency limits. Review visible UI and independent failure tests. Record the separate-machine smoke result before claiming a friend-ready party release. A locally verified candidate with that gate pending is partial, not complete; the shared world also remains outstanding.

**Owner acceptance decision — 2026-11-07:** the owner accepts the export-path/startup limitation as non-blocking for K4 after the successful physical two-PC games, reconnect, Gallery, export and remix checks. Close K4 with known issue **K4-LAUNCH-01**: use full output paths outside the verified program folder; the corrected launcher's relative-export/restart fix passed locally, but the latest laptop no-start report remains unresolved. This is an explicit acceptance exception for that report, not a passing retest or a general safety guarantee. Preserve the failed observation and carry diagnosis/retest into K7 hardening. Baseline 1.1, required features, and all other mandatory checks remain unchanged. This accepts a private party build; it does not claim a deployed service for friends on other networks.

### K5 Deep zoom proof

**Depends on:** K4 local implementation/acceptance; K0/K1 storage contracts reused. An unavailable external-machine smoke test may remain explicitly pending while this independent prototype proceeds, without closing K4. Preserve any recorded owner acceptance exception and unresolved issue. **Covers:** W2/W3/W6 prototype and W9 named local canvases/identity foundations.

- Prototype a hierarchical scene with bounded local drawing coordinates, stable canvas/region IDs/transforms, reversible navigation, progressive previews, and viewport-only loading. Compare retained strokes/raster/hybrid rendering against the Endless Paper-style experience in the product spec; do not require a vector-authoring UI.
- Add local create/name/rename/switch for independent canvases, with an owner identity and a future membership boundary. Verify at least four canvases, including identical display names with distinct IDs, separate camera/bookmark/history state, save/reopen, and crash recovery during switching. This is single-user storage and UX, not completed network sharing.
- Keep party documents unchanged. Define world format/versioning only after the prototype proves the nested-detail use case.
- Demonstrate six nested drawing regions, each at roughly 100x greater local detail, plus lateral expansion far beyond one party document. Six levels are a test fixture, not a six-level product cap; choose and document bounded depth limits with evidence.
- Draw recognizable shapes directly at every scale and across storage-region boundaries, save/reopen, pan/zoom both ways, and return via bookmarks without coordinate jumps or selecting/reopening separate files. Automatic region/detail selection and predictable brush size are required; a manual Create detail step does not close the gate. Inspect existing stroke rendering as well as new detail and document raster-image resolution limits.
- Implement single-target automatic zoom with pause/cancel and stable region/local-coordinate camera targets that K6 tours can reuse. Measure memory/frame time and show summaries rather than loading all offscreen detail.

**Done when:** a screen capture or inspected screenshots plus a reproducible test prove direct drawing at successive scales survives deep nesting and reopen. Multiple named canvases switch/recover without mixed artwork, history, or bookmarks. Prototype tests hit configured depth/coordinate/size limits safely. Record precision/render/storage tradeoffs. If this fails, report the specific failing property and continue the prototype; do not call bitmap magnification equivalent or silently drop deep zoom.

### K6 Persistent shared world

**Depends on:** K5, K2, and K4 remix/replay. **Covers:** W1-W9, G2 shared-world remix, and H1/H2/H3 foundations.

- Read the [hosting plan](hosting-plan.md). Before freezing shared-canvas identity schemas, implement stable account/profile/device separation and a versioned, recoverable linking migration that proves existing ownership, preserves IDs/authorship/permissions/unsynced work, and never links by email/name alone. Test mixed devices, wrong-account linking and isolated queues on account switching. Local verifier adapters are test-only; actual Google sign-in remains a K6.5 acceptance requirement.
- Prove an isolated Linux service build/start, persistent restart and the proposed trusted-HTTPS termination arrangement locally. Measure representative memory/CPU/storage/traffic, prepare repeatable GCP deployment assets and an updated full cost estimate. External project/OAuth access is not required to complete this local feasibility gate; record exactly what K6.5 still needs. Existing local canvas/party flows must remain usable without Google login.

- Integrate multiscale regions with local durability and service sync. Use author-owned layers, stable operation IDs, immutable blobs, deterministic merge/rebase, and explicit metadata conflict handling.
- Add dynamic per-canvas owner/editor membership and invite/accept/remove/leave controls, default-private creation, and scoped service listing/read/write/presence subscriptions. Test arbitrary overlapping/non-overlapping member sets, different canvas owners, repeated groups, and canvases excluding the primary test user. Include a nonmember denial for each canvas and concurrent groups of 2, 4, and 8 as coverage fixtures, not ceilings. Test larger membership lists with bounded pagination/chunking and document measured load/capacity; no fixed four-person cap. Do not raise or reuse the party-room limit.
- Migrate K5's local-only identity assumptions before sharing: distinguish canvas owner from authenticated stroke author/local viewer, permit authorized canvases owned by other users, author-filter undo, and keep navigation/review state per viewer/canvas. Preserve existing local IDs/artwork through migration; test mixed authors and at least one non-owner editor after restart. Do not remove ownership checks without replacing them with actual authorization.
- Test guessed IDs/hashes, swapped canvas IDs, same-name canvases, invitations used for the wrong canvas, removal during an active session, offline edits uploaded after removal, reconnect, and owner-only archive restore. Cross-canvas remix requires explicit source/destination authorization and must not leak private source metadata. Membership changes and rejected drafts survive restart without corrupting other canvases.
- Add ghost cursors/live stroke previews with expiry; their loss must never lose committed artwork. Include node/scale/camera context so cursors are positioned correctly.
- Add offline editing, reconnect catch-up, acknowledgments, retry/deduplication, bounded caches, bookmarks/home, and selected-view/full-archive exports.
- Personal undo reverses only the author's operations. Reference/clone somebody else's layer before editing it. Defer unrestricted destructive cross-author edits.
- Add saved multi-stop guided tours with stop ordering/timing, play/pause/scrub/cancel, and GIF/PNG-sequence export at a frozen scene revision. Verify nested and lateral paths, save/reopen/sync/archive, missing destinations, concurrent edits, cancellation, output timing/framing, and bounded export resources.
- Add per-viewer reviewed-revision tracking, author highlights, navigation, and replay for Since you were away. Test offline arrivals, erases/deletions/undo, author filters with canonical replay, restart mid-review, first visits, and changes arriving while a fixed interval is being reviewed. Background sync must not mark changes seen.
- Extend Remix to owned world regions, preserving animation frames/timing, provenance, editable ownership, and the source. Test placement at multiple scales, offline retry without duplicate insertion, authorization, and independence after source/clone deletion.

**Done when:** variable-size client groups, including more than four, draw concurrently on permitted canvases; A syncs and exits before B comes online; both separately edit offline then reconnect; authorized replicas converge to the same canonical scene and artifact hashes without losing a contribution. Repeat with duplicate/out-of-order messages, dropped previews, service restart, client kill, and interrupted uploads. Undo by A leaves B unchanged. Lateral/deep navigation and archive restoration pass on the shared result. B can review A's new work without clearing later unseen edits, replay/export a saved multi-stop tour, and edit a remixed creature/animation without changing its original. Generated W9 membership matrices and revocation tests prove isolation regardless of member count, combination, or canvas owner. All G2/W7/W8/W9 acceptance cases above pass.

K6 also requires tested account/linking contracts and a reproducible local Linux deployment feasibility result as described above. A local identity adapter does not prove real Google login or hosted access; those gates remain K6.5.

### K6.5 Hosted friend access

**Depends on:** K6 and K4 party/recovery evidence. **Covers:** H1-H4 end to end, with hosted regression coverage for party and world features.

- Follow the [hosting plan](hosting-plan.md): finish system-browser Google sign-in, trusted token verification, private service admission, expiring/revocable Komaphone sessions and safe legacy-profile linking. Preserve local-only use and isolate account switches/queues. Never let legacy pairing bypass public-service admission or canvas permissions.
- Package the existing Rust service for one small Linux GCP Compute Engine VM with persistent disk, trusted HTTPS/renewal, supervised boot/restart, bounded logs/resources and consistent off-VM backups. Deliver repeatable setup/update/rollback, a configured client endpoint, and simple administrator/friend runbooks.
- Re-price the complete deployment before credits: target <=USD $20/month, approximately $30 acceptable, including VM/address/storage/backups/egress/logs/domain and applicable taxes. Show measured capacity and low/higher-use forecasts, credit eligibility/expiry and budget alerts. Budget alerts are not hard caps; materially exceeding approximately $30 requires an owner decision before added spend.
- Prepare the full resource/configuration/cost/rollback result before requesting genuinely missing project/access/deployment decisions. GCP, Google sign-in and the stated cost preference are already selected. Deploy using applicable authorization when available; do not provision from this planning update alone.
- Exercise packaged Windows apps on independent public internet connections with actual Google accounts, ordinary invitations and no VPN/router changes/manual certificates. Under the owner-approved H4 clarification, home broadband plus a cellular hotspot with phone Wi-Fi off qualifies for K6.5 even in one home; retain fresh friend onboarding from another household in K7. Finish both party modes, gallery/replay/export, reconnect during phases and service restart. If K4-LAUNCH-01 prevents a participant launching, fix that prerequisite now and retain the K7 regression test.
- Verify simultaneous and non-overlapping canvas use, non-owner editing, denied nonmembers, account-switch isolation, offline recovery, tours/catch-up/remix, fresh off-VM backup restoration and HTTPS renewal/reboot. Retain K6's variable-membership and K3's two-/three-player coverage. Record exact externally tested configurations; do not infer results for unavailable participants.

**Done when:** all [K6.5 hosted acceptance cases](hosting-plan.md#k65-hosted-friend-access) pass with recorded real Google/GCP/independent-network evidence, tested runbooks and an itemized sustainable cost estimate. Mock sign-in, two LAN PCs, a VPN workaround, or deployment scripts alone cannot close this milestone. Missing external access/participants leaves that gate pending while independent preparation continues; no claim of hosted friend readiness until exercised.

### K7 Friend-ready hardening

**Depends on:** K6 and accepted K6.5 hosted access. **Covers:** all required specification IDs, including H1-H4.

- Run a requirements-to-evidence review and fix missing behavior. Inspect the actual Windows UI and packaged launch path, not only unit tests.
- Resolve and retest K4-LAUNCH-01 on the affected laptop: establish the actual launcher/log/inventory, preserve extra exports and saved work, then verify full-path and relative-path exports plus clean same-profile restart. K4's owner-accepted limitation is not evidence that this later hardening check passed.
- Test fresh install/update with isolated old-format data, Google sign-in/linking and invitations on distinct household networks through the K6.5 hosted route, recovery, protocol mismatch, quotas, corruption, and backup restore. Upgrade failure must retain an original usable backup. Record missing external test access as pending rather than optional success.
- Run relevant performance tests, record hardware and workload, and repair regressions. Produce exact quick-start instructions, release notes, known limits, and a recoverable service administration/backup path.
- Harden the proven K6.5 GCP deployment, credential/certificate renewal, backup restore and rollback. Recheck real monthly cost projections and operational limits; preserve the agreed budget preference. If external access is unavailable, finish independent checks and leave the affected release gate unverified. Do not first discover remote play or defer incomplete K6.5 evidence into a claim of final completion.

**Done when:** every required behavior has evidence; both party modes, multiple independently shared canvases with variable member sets, asynchronous use, guided tour exports, catch-up playback, and local/world remix pass end to end through the hosted Google-sign-in path; a fresh Windows user can follow the runbook from another household. Features outside baseline 1.4 remain deferred.

**Owner waiver decision — 2026-11-17:** following Checkpoint C acceptance based on supplied evidence, the owner explicitly waived the remaining **K4-LAUNCH-01** relative-export/reopen subcheck on the affected laptop for K7. Attempt 1 (`OWNER-SUPPLIED` on `Windows NT 10.0.22621.0`) verified `19` intact Candidate 3 program files (`9E519980…`, app `1146466D…`), a `57,148`-byte full-path PNG export (`72AE060D…`) outside `candidate/`, preserved `gallery.sqlite3` and `drafts.sqlite3`, and two `exit 0` same-profile launches, while `relExport` (`exports\k7-laptop-rel.png`) returned `exists: false` (`pid 3956` still open) and the owner declined investigating an unspecified follow-up issue. Retain full-path export guidance outside `candidate/` (new output filename or sequence subfolder), do not relabel the unverified relative-export/reopen subcheck as passed, and do not investigate further unless the owner requests it. **Fresh friend onboarding from another household (`H1`, `H2`, `H4`) is the sole remaining K7 acceptance gate**; billing (`OWNER-OBSERVED` Nov 1–13 Billing UI + pre-credit/pre-tax forecasts; live Cloud Billing export, post-Nov-13 invoices, taxes, exact domain amortization and credit details `NOT VERIFIED`) and natural production certificate renewal (`NOT VERIFIED` until the January 2027 Let's Encrypt ARI window) remain explicit operational qualifications.

## Dependency and delegation map

K0 -> K1 -> K2 -> K3 -> K4 -> K5 -> K6 -> K6.5 -> K7 is the default delivery sequence. K1 drawing and K2 service internals may overlap after K0 contracts, but K2 closes only after local-storage integration. Within K3 the two mode implementations can run in parallel. Within K4 gallery and replay/export can run in parallel. K6 establishes Google-compatible identity/linking and local Linux hosting feasibility before shared schemas freeze; K6.5 completes actual hosted access before K7 hardening. Independent review/fault tests can accompany every milestone. K5 follows the accepted private-network party release; only K6.5 proves the normal remote-friend experience.

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
| Presence | Ghost cursor/stroke preview visible within 250 ms p95 under simulated 100 ms RTT for a four-user benchmark fixture; also measure a group larger than four and document scaling/backpressure. Four is not a member cap. Durable correctness cannot depend on this target. |
| Scene rendering | p95 frame time <= 33 ms for the recorded K5 scene on the reference machine; render cost governed by visible detail, not total region count. |
| Resource bounds | Choose explicit per-message, asset, document, replay, cache, and world limits in K0/K5 and test limit failures. No unbounded allocation based on a remote size field. |

Do not promise a calendar completion date or infer Komaphone effort from upstream Photoshop-parity agent-hour estimates. Record actual milestone duration and blockers, then revise delivery expectations from evidence.
