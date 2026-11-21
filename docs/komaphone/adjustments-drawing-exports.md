# Drawing, exports and replay adjustments

**Current workflow (owner update 2026-11-21):** Antigravity verifies each authorized item, records Complete (worker-verified), and continues to the next ready item without Codex review or handoff approval. This supersedes older review-stop wording below, including copied prompts and Awaiting review states. Preserve historical evidence; reconcile pending items against current source and finish missing checks before closing them. Codex review is optional on owner request. Testing, external acceptance and live-action authorization requirements remain. See orchestration-workflow.md (under docs/komaphone from the repository root).

Owner intake, 2026-11-18. Queued requirements under the [backlog](adjustments-backlog.md) and [orchestration workflow](orchestration-workflow.md); no runtime investigation or implementation is claimed. User screenshots show the current export path field and four format buttons. The earlier inability to find gallery replay was resolved by the owner, so no replay-discovery feature is requested.

## Priority override

**Later owner clarification — 2026-11-18:** QOL-014 keyframed party submission replay is already running in Antigravity (owner-reported, not independently verified). Reliable gameplay takes priority over stroke-by-stroke uploaded replay fidelity; bounded sampled party timelapse is approved, preserving exact final editable artwork/pixels/seam, local undo/recovery and old archive playback. This supersedes the fidelity restriction in the original intake below. Complete/review the active checkpoint; QOL-009 remains the next requested implementation priority and QOL-010 follows it. Do not allocate a new ID after QOL-008: QOL-009 through QOL-014 already exist. Separate investigating the reported cause from claiming a tested fix.

The submitted worker prompt additionally covers preparation errors being erased by polling; keep errors visible until retry/success/dismissal/room exit. Sample metadata before loading expensive checkpoints where possible; bounded output alone does not prove bounded preparation work. A few MiB is a measured target, not a promise for an arbitrarily large lossless final drawing. Count final assets, replay, manifest and transfer operations against all limits; fail clearly without losing the draft if the mandatory final payload cannot fit. Preserve deterministic retries/deadline pins and truthful sampled-replay timing; do not invent intermediate strokes.

The owner explicitly puts **QOL-009 shared-canvas brushes/opacity first**, then **QOL-010 simple exports second**. After those reviews, do QOL-011 Workroom navigation, QOL-012 hide collaborator names, QOL-013 catch-up flicker, and the QOL-014 replay-size investigation. Then resume unfinished earlier adjustments in their recorded order. Preserve any already active implementation checkpoint before switching; no competing writers or cancellation of unrecorded work. QOL-008 admission remains deferred. One implementation item/evidence packet/Codex disposition at a time.

QOL-009 covers the current party brush set. When QOL-006 adds the new airbrush later, it must also extend shared-canvas parity; do not delay priority one for that future feature or mark the airbrush implemented through this task.

## QOL-009 — Shared-canvas brush and opacity parity

**State:** Ready; priority 1. Shared canvases should offer the same brush tools and opacity behavior as Animation/Exquisite Corpse. This is the owner's highest-priority request, repeated explicitly. Worker inventories the actual party brush controls, presets and meaningful settings and maps every one to world support before changing code; do not infer a tiny subset from the word brush. Non-brush editing features are not automatically added by this request.

Match visible results and interaction, including opacity differences and overlapping marks, at normal and deep zoom. Reuse existing engine primitives where compatible. Preserve the distinction between stroke opacity and layer opacity rather than approximating one with the other. Confirm that previews, accepted strokes, other clients, reopen, catch-up, tour rendering and exports agree on the selected tool/settings. Preserve author ownership, author-local undo, offline synchronization and old artwork. Version any needed event/schema changes and document old-client handling; no silent downgrade to opaque strokes or full-scene snapshots per pointer event.

**Acceptance:** a tool/settings parity matrix with actual rendered examples, including low/medium/full opacity and repeated overlap; stationary/moving brush behavior where applicable; two clients, offline/reconnect and reopen; undo/redo, deep scale, export/replay fidelity and representative size/latency measurements. Explain any unsupported behavior rather than declaring parity from matching toolbar labels. Refresh affected K7 evidence and supply exact tested candidate identities.

## QOL-010 — Name-only exports to a dedicated application folder

**State:** Ready after QOL-009 review; priority 2. Replace the free-form output path with a filename/name field prefilled from the selected gallery item's name. Users choose **Editable archive**, **PNG image**, **PNG frames**, or **Animated GIF**; the selected action determines the extension/output kind. Users cannot choose or edit the export directory in this normal flow. Retain appropriate availability rules for formats that do not apply to the selected item.

Use a dedicated app-managed exports folder under the existing per-user writable application-data root, isolated from program binaries, configuration, databases and recovery files. Resolve the actual existing storage convention during implementation; do not assume ZIP extraction creates a writable install directory. The folder is created as needed and works after installing/extracting the ZIP anywhere, without administrator access or a working-directory dependency.

The name field is a basename only: no directories, absolute/UNC/device paths or traversal. Normalize trailing recognized export extensions so typing .png or .gif does not duplicate or override the chosen format; retain meaningful internal dots. Handle blank names, invalid Windows characters, reserved device names, trailing dots/spaces and length limits with a clear safe result. Enforce output containment in the filesystem adapter as well as the UI, including redirected/reparse-point paths; do not rely solely on text filtering. Preserve lower-layer format APIs needed by other legitimate flows.

Use non-destructive collision handling (default to a numbered suffix) so existing exports are never silently overwritten. PNG frames use a uniquely named subfolder in Exports. Stage outputs within the approved export location and publish only on success; cancellation, permissions errors, disk-full or interrupted multi-file exports leave originals and app state usable. Never delete unrelated files while cleaning staging outputs.

On success say **Saved to [resolved location]**, with **Show in folder** (or Open folder for sequences). No path typing, extension typing or manual directory setup is required. Export errors are actionable and must not prevent closing/reopening the app or returning to the drawing.

**Acceptance:** all four applicable formats from named gallery items; default/custom names, typed extensions, duplicate names, invalid/path-like input; ZIP launched from another directory/read-only installation location; cancellation/failure and clean restart with the same profile. Inspect actual UI, generated files and Explorer action. Confirm saved artwork/configuration remain unchanged and exported filenames/format agree. Retest relevant startup/export regressions even though an older K7 subcheck was waived; do not rewrite historical results.

## QOL-011 — Persistent Workroom navigation

**State:** Ready after QOL-010 review. Add a clearly labeled **Workroom** button to the top-level navigation, available from Gallery, Canvases and Party. It returns to the existing drawing workspace with the current assigned drawing and other open drawing tabs/documents. **New drawing** remains a separate creation action; switching navigation must not create a new draft.

Owner clarification: Gallery already has a Draw action; the requirement remains a clearer top-level Workroom route. Do not treat the earlier report as proof the existing action is always broken. Reproduce any disabled/stuck navigation state and fix a demonstrated defect as part of this item.

**Acceptance:** enter Gallery during an active assignment, return directly through Workroom without cycling Canvases/Party, and recover the same document/selection and unsaved work. Test multiple open drawings, no open drawing (clear empty-state/create action), lobby/active/reveal phases and reconnect. Maintain phase/assignment restrictions; navigation does not grant editing of hidden or submitted content. Test New drawing separately where allowed.

## QOL-012 — Hide collaborator name labels

**State:** Ready after QOL-011 review. Add a simple local view toggle for names above multiplayer cursors/previews in shared canvases. Hide labels while retaining cursors/live stroke previews. Default to current visible behavior; persist the viewer's preference through reopen. This does not anonymize stored authorship, affect access, remove credits or hide names for other people.

**Acceptance:** two clients with different preferences, existing and newly arriving peers, reconnect and switching canvases. Ghost positions/strokes remain correct; artwork and exports are unchanged. Ensure control discoverability when the bookmark pane is collapsed.

## QOL-013 — Stable catch-up playback without scene/UI flashing

**State:** Ready after QOL-012 review. Owner reports severe flicker during shared-canvas Since your last review playback; intended experience is a stable scene with new marks appearing progressively. Party Exquisite Corpse replay appears to add strokes correctly, although some surrounding UI may flicker. Reuse applicable playback/presentation techniques only after inspecting their actual differences; do not copy the party storage system into worlds.

Reproduce and record the visual problem; distinguish scene clearing/reloading, camera movement, preview/checkpoint changes and UI invalidation before selecting a fix. Keep a stable presented scene while preparing replay changes; avoid showing transient blank/partially rebuilt frames. Preserve legitimate erases, undo/redo, deletions and transforms: the requested incremental appearance does not mean all events are append-only. Explicit seek/author filters must still reconstruct canonical state correctly. Pause/resume/scrub and new edits arriving during review retain fixed review intervals and review-watermark semantics. Do not mark changes reviewed merely to suppress playback.

**Acceptance:** inspected capture of playback/pause/scrub with a dense scene, deep locations, additions/erasures/undo and reconnect/offline uploads; no unintended scene or surrounding UI flashing. Compare final canonical content and save/review state; measure frame time, loaded memory and event/storage volume. Do not introduce per-frame whole-world persistence, discard events or hide genuine edits to achieve smoothness. If party UI flicker is independently reproduced, report it as a narrowly scoped follow-up rather than expanding this task silently.

## QOL-014 — Investigate oversized party replay blocking submission

**State:** Proposed / bounded evidence investigation first. Owner reports a friend could not submit a heavily drawn corpse segment because replay data grew too large. Supplied prior explanation claims that local/party/gallery replay stores full .pcraft snapshots per edit while worlds store compact operations. These are unverified supplied claims, not code findings from this intake. Preserve the friend's recoverable drawing and do not clear history or raise limits blindly.

First worker packet: reproduce with synthetic heavy drawing; show exact serialization/storage/upload paths, relevant excerpts, measured draft/replay/final-artifact sizes, submission limit/error and logs; distinguish client packaging, server validation/storage and transport failures. Include what the replay actually needs and whether unused manifest fields exist. Check the current build before assuming the pasted diagnosis still applies.

The pasted proposal of 40–60 sampled PNG/WebP frames and a 150+ MB to 1–3 MB reduction is an unmeasured suggestion. The owner values visible stroke-by-stroke replay; sampling is not approved as a replacement for the canonical retained history. Prefer a measured compact event/delta/checkpoint approach that preserves supported replay and full final editable artwork. A sampled preview may be evaluated separately but must not silently replace required history. Do not remove raw params before proving they are unnecessary for replay/compatibility. Do not convert shared canvases to the reported snapshot-per-edit path.

After the evidence packet, Codex defines a bounded fix from demonstrated facts. Acceptance for that fix must include successful heavy two-/three-player submission, preserved final artwork/replay/remix/archives, measured payload growth, retries/reconnect/service restart, old-data compatibility and graceful oversized-input errors without losing drafts. Any intentional replay-fidelity reduction needs an explicit owner decision. Do not claim a size target achieved without a reproducible workload and before/after measurements.

## First handoff

Use the backlog worker prompt for **QOL-009 only**, referencing this document. Return a compact parity/evidence packet for Codex review before QOL-010 starts. Existing queued items are retained; the owner can add further requests at any time.
