# Komaphone adjustments backlog

**Current workflow (owner update 2026-11-21):** Antigravity verifies each authorized item, records Complete (worker-verified), and continues to the next ready item without Codex review or handoff approval. This supersedes older review-stop wording below, including copied prompts and Awaiting review states. Preserve historical evidence; reconcile pending items against current source and finish missing checks before closing them. Codex review is optional on owner request. Testing, external acceptance and live-action authorization requirements remain. See orchestration-workflow.md (under docs/komaphone from the repository root).

Created 2026-11-17 at the owner's request. A living queue for owner-requested quality-of-life changes, fine-tuning and later additions. Active queue items are recorded below; examples used to explain this workflow are not requests. Follow the [orchestration workflow](orchestration-workflow.md): Antigravity implements and gathers evidence; Codex reviews the packet; the owner relays messages.

## Scheduling and scope

These items may proceed before the sole remaining K7 acceptance gate — fresh friend onboarding from another household (`H1`, `H2`, `H4`) — while that tester awaits availability (the remaining `K4-LAUNCH-01` laptop relative-export/reopen subcheck was explicitly waived by the owner for K7 on 2026-11-17 with full-path export guidance retained). This does not mark unexecuted tests passed, waive external friend onboarding, or close K7. Read the current [status](status.md) before selecting work; do not assume every remaining gate is only a scheduling issue. Preserve pending reviews and the last tested release candidate.

Run one bounded item at a time. Do not overlap implementation with another worker modifying the same checkout or live service. Finish an item's evidence packet and Codex review before starting the next implementation item. New ideas can be added to the queue anytime without starting them. User priority overrides arrival order; dependencies determine readiness.

An accepted adjustment is not proof of K7 completion. Each change must identify which previous acceptance results remain applicable and which need rerunning against the new candidate. Keep old evidence intact; update the final package/source hashes and required release regressions. Changes to game rules, storage, protocol, auth or permissions require explicit compatibility and migration analysis. Record owner-approved changes to frozen product behavior in the specification/revision history; a backlog entry does not silently override the baseline.

## Current priority — 2026-11-18 follow-up

[Drawing/export follow-up requirements](adjustments-drawing-exports.md) override the earlier sequence: QOL-009 first, QOL-010 second, then QOL-011/012/013 and QOL-014 evidence investigation; resume unfinished earlier items afterward. Preserve any active checkpoint before changing tasks. QOL-008 remains deferred. No replay-discovery task is needed; the owner found that control.

## Queue

| ID | Requested outcome | Priority/dependencies | State | Handoff/evidence/review |
|---|---|---|---|---|
| `QOL-001` | Replace inherited `"PhotoCraft"` product wording in the Safe GPU / CPU-compositor fallback notice modal with **Komaphone** while preserving fallback controls and upstream attribution | Normal; independent (do not modify or rebuild Candidate 3 until owner requests implementation) | Proposed | Queued 2026-11-17 from Checkpoint C isolated Safe-GPU screenshot (`log/k7/launcher-candidate3/screenshots/cycle-3-safegpu.png`); Candidate 3 (`9A54729E…` / app `1146466D…`) intentionally unchanged |
| `QOL-002` | Restart sequential corpse reveal for each creature | Batch A / first | Ready | [Requirements](adjustments-2026-11-18.md#qol-002--restart-sequential-corpse-reveal-per-creature) |
| `QOL-003` | Collapse/restore shared-canvas bookmark sidebar | Batch A / after 002 review | Ready | [Requirements](adjustments-2026-11-18.md) |
| `QOL-004` | Bottom-left dimensions and zoom | Batch A / after 003 review | Ready | [Requirements](adjustments-2026-11-18.md) |
| `QOL-005` | Reuse PhotoCraft layer UI, drag reordering and options | Batch B / after A | Ready | [Requirements](adjustments-2026-11-18.md) |
| `QOL-006` | MS Paint Airbrush with continuous stationary spray | Batch C / after B | Ready | [Requirements](adjustments-2026-11-18.md) |
| `QOL-007` | Per-player corpse segment height changes during drawing | Batch D / after C | Ready | [Requirements](adjustments-2026-11-18.md) |
| `QOL-008` | Audit/enforce invite-only hosted admission | Later / owner deferred | Deferred | [Requirements](adjustments-2026-11-18.md) |
| `QOL-009` | Shared-canvas party brush and opacity parity | Priority 1 | Awaiting review | [Requirements](adjustments-drawing-exports.md); evidence `log/k7/qol-009-world-brushes-20261119/` |
| `QOL-010` | Name-only exports into dedicated app Exports folder | Priority 2 / after 009 review | Ready | [Requirements](adjustments-drawing-exports.md) |
| `QOL-011` | Persistent top-level Workroom navigation | After 010 | Ready | [Requirements](adjustments-drawing-exports.md) |
| `QOL-012` | Local toggle for collaborator name labels | After 011 | Ready | [Requirements](adjustments-drawing-exports.md) |
| `QOL-013` | Stable shared-canvas catch-up playback | After 012 | Ready | [Requirements](adjustments-drawing-exports.md) |
| `QOL-014` | Bounded keyframed party submission replay and persistent preparation errors | Checkpoint accepted; QOL-009 next | Accepted (supplied evidence with focused receipt/hash checks) | Codex 2026-11-19: implementation + verification addendum reviewed; 13 current Rust hashes match, full engine/UI result totals checked. New package/launcher/hosted regression and K7 external gates remain pending; not release certification. |
| `QOL-015` | Readable timestamps in the viewer's PC time zone | Small UI follow-up after QOL-011 review; preserve active work and priorities 009/010 | Ready | Requirements below |

Assign stable IDs `QOL-001`, `QOL-002`, and so on as actual requests arrive; do not reuse IDs. Use **Proposed -> Ready -> Implementing -> Awaiting review -> Accepted**. Review can return an item to **Changes required**; use **Blocked** or **Deferred** with a specific reason when appropriate. Only Codex's recorded disposition moves an item to Accepted in this workflow; identify whether it is based on supplied evidence or direct checks.

### QOL-009 — Shared-canvas brush and opacity parity

```text
ID / title: QOL-009 / Shared-canvas brush and opacity parity
Owner request and desired outcome: Give shared canvases (World) the same brush-family tools, presets, settings, and opacity/overlap/eraser/airbrush behavior as Animation and Exquisite Corpse at normal and deep zoom, with exact agreement across previews, accepted strokes, other clients, reopen, catch-up playback, tours, exports, and thumbnails, while preserving author-layer isolation, author-local undo/redo, offline sync, and legacy artwork.
Current behavior -> desired behavior: Shared canvases previously offered only opaque uniform-width round strokes (Brush/Eraser/Pan, single 1..=128 px width, opaque sRGB picker) -> Shared canvases now expose Brush (B), Pencil, Eraser (E), Pan, Size (1..=5000 px), Opacity (0..=100%), Flow (0..=100%), Hardness (0..=100%), Smoothing (0..=100% + live smoothing string), Presets (all 36 built-in presets + imported .abr sampled tips via shared brush_picker and right-click), Settings (floating Brush Settings F5 window), FG/Swap(X)/BG/Colors popover (Party's exact Hue Cube + Palette swatches), Lock thickness, and Undo/Redo/Home inside 1280x720 with zero horizontal clipping. A unified multi-scale brush rasterizer (crates/engine/src/world_brush.rs) renders StrokeBrush strokes identically across Party reference, World CPU exports/tours/catch-up/thumbnails, incremental live preview (LiveWorldBrush), peer previews, and WorldGpu wgpu hardware/CPU-blend compositing, preserving stroke opacity vs device-layer isolation, single-stroke no-doubling vs separate-stroke compounding, stationary Airbrush Soft build-up, Behind/Multiply/all 33 blend modes, and 106-bit Double deep-zoom precision (depths 0..=12).
In scope / explicit exclusions: In scope: L1 scene StrokeBrush/StrokeSample/PreviewStyle/world_protocol v3/WorldState schema 1->2/ArchiveManifest schema 1|2, L4/client WorldOutbox schema 2->3 with pre-migration backup and K5 local-canvas explicit rejection of v2 brush strokes, service SQLite schema 4->5 with .before-world-v5-* backup and HTTP 426 rejection of old v2 clients, L5 unified world_brush rasterizer + world_media/world_thumbnails/world_cmds integration, L6 WorldGpu brush pipelines/BrushCache/CPU-blend fallback + komaphone_world toolbar/input/render/shortcuts/inspect + visual parity & UI tests. Explicit exclusions / unsupported behaviors: non-brush Party tools (Fill, Eyedropper, Selection/Lasso, Move, Shapes, Text, Layers panel, Mixer Brush, Clone/Retouch, Symmetry), Pencil Auto Erase, Eraser Block mode, cross-author blending/erasing (by W4 rules, Erase/Behind/blend modes stay isolated to the author's device layer), future QOL-006 MS Paint dot-spray Airbrush, K5 local canvases (canvases.sqlite3 unchanged), live VM deploy, Candidate 3 ZIP (9A54729E... untouched), and hardware pen/live internet two-PC verification.
Priority / dependencies / state: Priority 1 / after QOL-014 checkpoint acceptance / Awaiting review.
Acceptance examples (observable outcomes, including relevant failure behavior): (1) Side-by-side Party vs World CPU vs World GPU visual parity sheets (01-opacity-overlap-matrix.png, 02-preset-parity-matrix.png, 03-deep-zoom-and-export-parity.png) verified 0/255 Party vs World CPU on 17/19 rows (<=1/255 on Calligraphy Flat & custom 32x32 tip), <=2/255 World CPU vs GPU on 1-2 layers and 3/255 across 3 stacked depth-6 translucent layers (mean 0.00094), covering 25/50/100% opacity, figure-8 no-doubling vs 2-stroke 75% compounding, partial-opacity Soft Eraser preserving Author 1 untouched, Airbrush Soft tap vs 400ms stationary hold vs sweep, Behind & Multiply modes, 14 presets across all 4 groups + custom 32x32 diamond PNG tip, depth-0 ancestor + depth-6 detail, PNG export, Tour GIF, thumbnail, and bit-identical schema-2 .komworld archive reopen; (2) real HTTP 2-client test (two_clients_http_brush_sync_presence_undo_offline_restart_and_v2_rejection) verified live brush Presence preview round-trip, multi-author brush sync, author-local undo/redo + cross-author undo refusal, server shutdown/offline queue/restart/reopen convergence, v4->v5 automatic backup+migration, v5 backup/restore, and explicit HTTP 426 rejection of old v2 clients; (3) payload sizes measured 1,042..1,489 B for all symbolic built-in presets (vs 987 B legacy, 20 pts) and 1,415/2,149 B for 32x32/128x128 custom PNG tips; 800x600 latencies measured 0.58 ms single-stroke GPU cached, 5.69 ms 20-stroke GPU cached, and 5.97 ms avg / 9.05 ms max per incremental 200-pt live drag step; (4) all 4 wgpu 1280x720 UI screenshots visually inspected (toolbar, Presets picker, Colors popover, Brush Settings F5).
Affected areas (worker confirms exact files before editing): 59 Rust files (54 modified + 5 new: crates/scene/src/brush.rs, crates/scene/tests/brush_schema.rs, crates/engine/src/world_brush.rs, crates/engine/tests/qol009_world_brush_parity.rs, apps/komaphone-service/tests/qol009_world_brushes.rs, plus crates/ui-egui/src/komaphone_world/qol009_evidence.rs — full SHA256 list in log/k7/qol-009-world-brushes-20261119/end-state-hashes.json).
Existing data / party rules / protocol / permissions impact: Coordinated breaking world upgrade: world_protocol::VERSION 2->3 (old v2 clients receive HTTP 426 Upgrade Required on /v1/canvas; party /v1/rpc remains v1), service.sqlite3 schema 4->5 (automatic .before-world-v5-<rand>.sqlite3 backup), WorldOutbox schema 2->3 (automatic .pre-brush-schema-v2.sqlite3 backup), WorldState schema 1->2 on brush strokes (existing v1 worlds keep schema 1 and exact canonical_hash), ArchiveManifest schema 1|2. K5 local canvases explicitly reject brush-v2 strokes. Author-layer isolation and membership authorization remain unchanged.
Specification changes needed, if any: None beyond the queued QOL-009 specification already recorded in docs/komaphone/adjustments-drawing-exports.md and product-spec.md (2026-11-18).
Relevant tests / visual checks / K7 evidence that must be refreshed: Executed on frozen final source hashes (59 Rust files, rustc 1.95.0): photocraft-scene (79 passed, 0 failed), photocraft-local-store (62 passed, 0 failed), photocraft-room-client (157 passed, 0 failed), photocraft-sync (113 passed, 0 failed), photocraft-engine full unfiltered --lib --tests (49 binaries: 3,653 passed, 0 failed, 22 pre-existing ignored) + --doc (0 failed) + ignored panic_hunt (1 passed), photocraft-ui-egui full unfiltered --lib --tests (5,010 unit + 200 integration = 5,210 passed, 0 failed, 22 pre-existing ignored) + --doc (0 failed) — 9,275 passed / 0 failed total — plus strict clippy -D warnings across all 7 crates (incl. apps/photocraft), xtask layers, xtask wasm, xtask parity (660/660 live, docs/parity-checklist.md unchanged CE382360...), cargo fmt --check, git diff --check, and visual inspection of all 7 PNG renders. Retained all failed intermediate logs (panic_hunt attempt1, ui-egui attempt1 deep_max=3 quantization, ui-egui attempt2 stack overflow resolved by boxing WorldGpu/LiveWorldBrush/Draft). K7 refresh required before release: coordinated VM schema 4->5 / protocol v3 closed-proxy upgrade + v5 off-VM backup/restore drill, Candidate 4 desktop ZIP build/hashes/isolated launcher smoke, hosted two-PC shared-canvas brush regression over independent internet connections, and fresh other-household friend onboarding (H1, H2, H4).
Worker handoff: Implemented and verified locally on 2026-11-19 without modifying Candidate 3 (9A54729E...), live VM (f4d77aee...), or user profiles. Stopped at QOL-009 for Codex review; QOL-010 not started.
Evidence packet path and tested candidate/source identities: log/k7/qol-009-world-brushes-20261119/ (parity-matrix.md, measurements.json 8146E937..., end-state-hashes.json, task-relative.patch, renders/01..07.png, logs/*.log; base HEAD 0d33e9b243be9218a9084af8e90e300c1b9a49a0; Candidate 3 ZIP 9A54729E2035499FE4339E208B80DD210B3D8922218248552E827E1C0B2C90F9 verified untouched).
Codex disposition / evidence basis / corrections / remaining limits: Awaiting Codex review.
```

### QOL-015 — Readable local dates and times

Owner request: replace cryptic raw timestamps such as the gallery screenshot's `2026-11-19T04:43:34.946Z` with a readable date/time in the viewing user's PC time zone. For a PC configured to Pacific time, that example should display approximately **Nov 18, 2026, 8:43 PM PST**. Use the actual system zone and the offset applicable to the timestamp, including daylight saving; do not hardcode PST or a fixed minus-eight-hour conversion for everyone.

Apply consistently to ordinary user-facing gallery, drawing/game and shared-canvas date/time labels. Prefer the PC's locale and 12-/24-hour convention where practical; omit fractional seconds and unnecessary seconds in compact cards. An exact timestamp may remain in a tooltip/details view. Use a clear zone label or UTC offset where ambiguity matters; never label a UTC fallback as local time. Invalid/missing values show an honest unavailable state without crashing.

This is display formatting only. Keep stored/synced instants, ordering, replay elapsed times, deadlines and machine-readable logs/archives intact. Use chrono and existing platform services per AGENTS.md; preserve WASM compilation and an explicit fallback when system timezone information is unavailable. Reuse one formatter rather than separate ad hoc conversions.

Acceptance: inspect an actual gallery card with the supplied example; verify two different system-zone fixtures, a UTC/local date rollover, winter/summer Pacific offsets, 12-/24-hour formatting where supported, and invalid/missing data. Confirm original stored timestamps remain unchanged and displayed time follows the viewer rather than the creator/server. Use the usual bounded Antigravity handoff and Codex evidence review; no new time-zone settings panel is required.

### QOL-014 — Bounded keyframed party submission replay and persistent preparation errors

```text
ID / title: QOL-014 / Bounded keyframed party submission replay and persistent preparation errors
Owner request and desired outcome: Prevent detailed Animation and Exquisite Corpse drawings from blowing past upload/outbox limits or taking minutes to upload, and keep submission-preparation/queue errors visible instead of letting the 250ms room poll wipe them, while staying 100% compatible with the unchanged live K7 service (f4d77aee...) and existing Gallery archives.
Current behavior -> desired behavior: (1) rooms::drafts::prepare loaded and serialized a full .pcraft checkpoint for every local revision 1..=N and embedded raw brush command params into ReplayArtifactStep, causing a ~115-stroke Corpse drawing to upload ~18.8 MB (~3.5 min) and a friend's more detailed drawing to exceed limits -> prepare inspects RevisionSummary metadata first, preserves local drafts.sqlite3 undo/recovery history untouched, retains exact baseline (rev 1) and unflattened editable final checkpoint (rev N) + canonical sRGB PNG + 24 px Corpse seam, samples <= 34 evenly spaced intermediate keyframes (<= 36 steps total, <= 4 MiB intermediate pack target), flattens intermediate-only keyframes into deterministic single-layer 8-bit sRGB .pcraft checkpoints, strips ReplayArtifactStep.params to {}, counts mandatory + replay + manifest + JSON outbox transfer bytes against all limits (64 MiB/object, 256 objects, 256 KiB RPC message, 512 MiB project raw bytes, 8,190 upload chunk ops, 384 MiB JSON pending outbox), and preserves Transfer deadline FreezePin matching via Prepared.local_revision / ClientCommand::Submit.local_revision. (2) Response::Prepared errors wrote only into self.rooms.status.error, which the 250ms Poll overwrote with None and komaphone_game_view::reference_panel never rendered -> Rooms latches scoped submission_error across 250ms Poll updates (and worker pre-outbox Submit rejections), renders the error + Dismiss (komaphone.room.dismissError) in both the left assignment reference panel and the Private room modal, shows "Submission failed · local draft retained" in the status bar, and clears cleanly on dismiss, retry, accepted submission, or room leave.
In scope / explicit exclusions: In scope: crates/local-store (RevisionSummary, revision_summary, load_revision_bytes), crates/engine (rooms/drafts.rs keyframing + limit accounting, rooms.rs + local_projects + komaphone_room_cmds.rs persistent submission_error + dismissError), crates/room-client (worker.rs ClientCommand::Submit.local_revision freeze-pin check), and crates/ui-egui (komaphone_game_view.rs, komaphone_rooms.rs, control/inspect.rs, komaphone_game_view_tests.rs) plus engine/client/UI regression tests. Explicit exclusions: zero edits to apps/komaphone-service or crates/party wire/SQLite contracts, zero live VM deployments, zero changes to Candidate 3 (komaphone-k7-candidate-3.zip 9A54729E...), zero shared-canvas (worlds) changes, and zero implementation of QOL-001..QOL-013 or QOL-015.
Priority / dependencies / state: Active checkpoint (preceding QOL-009) / owner-approved sampled party replay / Awaiting review.
Acceptance examples (observable outcomes, including relevant failure behavior): (1) Measured 1,020-revision multi-layer Exquisite Corpse drawing (smart object + semi-transparent shading + ink + erase + undo/redo + 4 KB stroke params/rev; 36,765,696 B local drafts.sqlite3) prepares in 929 ms into 3,375,256 B (3.22 MiB) raw upload (10,507 B manifest, 36 steps, 1 pack, 108 upload ops, 11.42 MiB JSON outbox), passes exact live K7 service validation, plays back step 0/17/35 in Gallery, Remixes with full unflattened layers/opacity/smart object intact, and leaves all 1,020 revisions in drafts.sqlite3; (2) real 2-player Exquisite Corpse game (80+ multi-layer strokes) completes over HTTP against unchanged photocraft-sync.exe with Gallery download and unflattened Remix; (3) 4-revision small drawings retain all 4 steps and 300-step historical full-history Gallery bundles with raw params still validate and play back; (4) Transfer deadline freeze retries produce byte-identical Submission and assets with local_revision=90 vs submission.revision=36; (5) preparation/limit errors (e.g. wrong canvas width or >384 MiB 7-layer F32 mandatory pair) preserve the saved local draft, persist across >= 3 live 250ms HTTP Polls, render with Dismiss in both UI surfaces, and clear on dismiss, valid retry, or Leave room.
Affected areas (worker confirms exact files before editing): crates/local-store/src/{types,native}.rs, crates/room-client/src/worker.rs, crates/engine/src/{rooms.rs,rooms/drafts.rs,local_projects.rs,local_projects/native.rs,cmd/komaphone_room_cmds.rs}, crates/engine/tests/replay_keyframes.rs, crates/ui-egui/src/{komaphone_rooms.rs,komaphone_game_view.rs,komaphone_game_view_tests.rs,control/inspect.rs}.
Existing data / party rules / protocol / permissions impact: Zero wire-protocol, SQLite schema, auth/permission, or live-server changes. Sampled party submissions emit contiguous 1..=K steps (submission.revision = K, event_count = K - 1) matching unchanged ReplayArtifact::validate and komaphone-service validate_submission/validate_replay; existing local drafts.sqlite3 and historical full-history Gallery bundles remain 100% readable.
Specification changes needed, if any: Already recorded in product-spec.md and adjustments-drawing-exports.md on 2026-11-18 (owner-approved sampled party replay preserving final editable checkpoint, canonical PNG/seam, local undo/recovery, and historical archive compatibility).
Relevant tests / visual checks / K7 evidence that must be refreshed: Executed on identical source hashes: full photocraft-local-store (62 passed), full photocraft-room-client (157 passed), full photocraft-party (90 passed), full unfiltered photocraft-engine --lib --tests (48 binaries: 3,648 passed; 0 failed; 22 ignored, incl. 2,944 unit + 434 it + 6 replay_keyframes against unchanged photocraft-sync.exe) + --doc (0 failed) + ignored panic_hunt (1 passed), full unfiltered photocraft-ui-egui --lib --tests (5,005 unit + 200 integration = 5,205 passed; 0 failed; 22 ignored) + --doc (0 failed) + 3 visually inspected wgpu PNGs, cargo xtask parity (660/660 live, docs/parity-checklist.md unchanged CE382360...), cargo xtask perf --quick (9 quick benches ok / 65 tracked scenarios / exit 0 in 2,692.47s; results.json C89785B0..., summary.md C62C6FE4...; separate from the 929 ms 1,020-rev replay_keyframes preparation fixture), clippy -D warnings, xtask layers (36 crates), xtask wasm (28 crates + heif), cargo fmt --check, and scoped git diff --check. Unchanged server/backup/restore/domain evidence (Checkpoints B1, B2a, B2b server side) remains valid; when a future desktop candidate (e.g. Candidate 4) is packaged, refresh candidate binary/ZIP hashes, isolated launcher smoke, and a brief hosted party submission smoke before/with fresh other-household friend onboarding (H1, H2, H4).
Worker handoff: Implemented on 2026-11-18 and completed 2026-11-19 verification addendum (full engine/UI suites, xtask parity, xtask perf --quick, and exact source-hash match) without modifying source, Candidate 3, or the live VM.
Evidence packet path and tested candidate/source identities: log/k7/replay-keyframes-20261118/ (HEAD 44ba5ef4d6d9036891a90b94724ee34936a3f14c + task-relative.patch + addendum-verification-20261119.json; screenshots/01..03.png; perf-quick-results/; Candidate 3 ZIP 9A54729E2035499FE4339E208B80DD210B3D8922218248552E827E1C0B2C90F9 verified untouched).
Codex disposition / evidence basis / corrections / remaining limits: Initial Codex review requested 4-point verification addendum (full engine/UI crate tests, xtask parity, xtask perf --quick, final source-hash confirmation); addendum completed 2026-11-19, awaiting Codex final disposition.
```

### QOL-001 — Safe GPU CPU-fallback modal product naming

```text
ID / title: QOL-001 / Safe GPU CPU-fallback modal product naming
Owner request and desired outcome: Replace the inherited "PhotoCraft" product name in the Safe GPU / CPU-compositor notice modal with "Komaphone" (queued 2026-11-17 without changing or rebuilding Candidate 3 now).
Current behavior -> desired behavior: Launching with --safe-gpu (Start-Komaphone-Safe-GPU.exe) displays a CPU-rendering notice modal whose body reads "PhotoCraft is using the CPU image compositor." (observed in log/k7/launcher-candidate3/screenshots/cycle-3-safegpu.png) -> user-facing modal copy names "Komaphone" (for example, "Komaphone is using the CPU image compositor.") while keeping Keep Using CPU / Retry GPU behavior identical.
In scope / explicit exclusions: In scope: user-facing Safe GPU / CPU-fallback notice string(s) and any matching unit/snapshot assertions for that notice. Explicit exclusions: do not remove or alter upstream copyright, license, or contributor attribution (About / ATTRIBUTION.md / bundled notices), do not rename machine identifiers (photocraft.exe, photocraft-* crates), do not change GPU/CPU fallback logic, and do not modify or rebuild Candidate 3 until the owner schedules this item.
Priority / dependencies / state: Normal / independent / Proposed (deferred from Candidate 3; no implementation or rebuild requested yet).
Acceptance examples (observable outcomes, including relevant failure behavior): (1) Launching with --safe-gpu renders the CPU-fallback notice naming Komaphone with no "PhotoCraft" user-facing branding in the modal; (2) Keep Using CPU dismisses the modal and retains normal workspace interaction, and Retry GPU retains existing behavior; (3) Upstream attribution files and About credits remain intact.
Affected areas (worker confirms exact files before editing): Safe GPU / CPU-compositor fallback notice UI in crates/ui-egui and/or apps/photocraft (worker confirms exact symbol and file path before editing).
Existing data / party rules / protocol / permissions impact: None (display-string-only UI change; zero local SQLite, archive, party/world wire protocol, or auth/permission impact).
Specification changes needed, if any: None (aligns with AGENTS.md and product-spec.md baseline 1.4 rule that user-facing product text says Komaphone while preserving upstream attribution and machine names).
Relevant tests / visual checks / K7 evidence that must be refreshed: Touched-crate unit/integration tests and strict Clippy, cargo xtask layers, cargo xtask wasm (if L0-L6 touched), visual inspection of a --safe-gpu screenshot, and—when a new release candidate is packaged—candidate binary/ZIP hashes plus isolated package launcher smoke (server/schema4 backup/restore, domain/archive suites, and non-UI benchmarks remain unaffected if untouched).
Worker handoff: Not started (queued only per 2026-11-17 owner instruction).
Evidence packet path and tested candidate/source identities: Baseline observation at log/k7/launcher-candidate3/screenshots/cycle-3-safegpu.png (SHA256 4A7D929190D0F63DC14785993FFDBD75444C6E9DDB80A8C564F0FFC08A6DABFC) on Candidate 3 (ZIP 9A54729E..., app 1146466D...). Implementation evidence pending.
Codex disposition / evidence basis / corrections / remaining limits: Pending implementation when scheduled.
```

## Item template

Copy this section once per actual request. Keep routine UI details practical; ask the owner only when a missing choice materially changes the intended experience.

```text
ID / title:
Owner request and desired outcome:
Current behavior -> desired behavior:
In scope / explicit exclusions:
Priority / dependencies / state:
Acceptance examples (observable outcomes, including relevant failure behavior):
Affected areas (worker confirms exact files before editing):
Existing data / party rules / protocol / permissions impact:
Specification changes needed, if any:
Relevant tests / visual checks / K7 evidence that must be refreshed:
Worker handoff:
Evidence packet path and tested candidate/source identities:
Codex disposition / evidence basis / corrections / remaining limits:
```

## Antigravity implementation prompt

```text
Implement only [QOL-ID] from docs/komaphone/adjustments-backlog.md.
Read AGENTS.md, the item, orchestration-workflow.md, current status and relevant
product/architecture/delivery requirements. Confirm no competing writer is
active. Preserve the dirty tree, user data, existing candidate and prior receipts.
Do not pick other queue items or broaden into unrelated cleanup.

Confirm the affected files and baseline behavior. Implement the smallest
complete change satisfying the item's observable acceptance cases. Check data,
protocol, permissions and existing-game compatibility where relevant. Record
necessary specification changes accurately; do not invent product decisions.
Run repository-required checks for the touched areas and actual visual checks
for UI changes. Reuse valid unchanged evidence; explicitly identify which K7
checks the change invalidates and refresh the relevant candidate evidence.
Do not waive unavailable laptop/friend tests or claim K7 complete. Do not deploy
live changes or incur expenses beyond applicable authorization.

After required checks pass, mark Complete (worker-verified) and update status/devlog. Retain the
orchestration workflow's compact evidence packet: task-relative diff, exact
commands/results, failures/skips, screenshot paths where relevant, source and
artifact identities, remaining limits and affected K7 gates. Continue to the next ready authorized item without waiting for Codex review.
```

## Optional Codex review prompt (owner request only)

```text
Review [QOL-ID] and the attached Antigravity evidence packet under
docs/komaphone/orchestration-workflow.md. Conserve quota: use the item and supplied
evidence first; no broad audit, builds, implementation or subagents by default.
Check the requested behavior, scope, regressions, data/authorization impact,
candidate provenance and implications for remaining K7 tests.
Give one concise disposition or a consolidated evidence/correction prompt.
Distinguish supplied evidence from independently performed checks. Record the
disposition on the item; keep it Awaiting review or Changes required if evidence
is insufficient. Accept only this item when supported, not the entire release.
Packet: [paste the worker's compact report]
```
