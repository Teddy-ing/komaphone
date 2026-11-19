# Owner adjustments — 2026-11-18

Approved requested behavior, queued for implementation under the [backlog](adjustments-backlog.md) and [orchestration workflow](orchestration-workflow.md). No implementation or runtime verification is claimed. The owner supplied two screenshots: the compact checkbox/button layer list, and the PhotoCraft bottom-left zoom/dimensions status display. Their requirements are transcribed below so implementation does not depend on temporary attachment paths. Names transcribed as Camophone/Comophone refer to Komaphone; the editor reference is PhotoCraft.

## Sequence and review boundaries

**Superseded scheduling:** the owner's later [drawing/export intake](adjustments-drawing-exports.md) puts QOL-009 first and QOL-010 second. Follow the backlog's current priority section before using the earlier batch order or first-worker instruction below. All earlier feature requirements remain queued.

Batch A: QOL-002 reveal reset, QOL-003 collapsible canvas sidebar, QOL-004 dimensions/zoom. Batch B: QOL-005 PhotoCraft layer controls. Batch C: QOL-006 airbrush. Batch D: QOL-007 variable corpse segment height. QOL-008 private admission stays deferred. These batches organize related work; execute and review one item at a time using the backlog worker prompt. Do not silently implement every item in one run. Existing QOL-001 remains separately proposed under its recorded authorization boundary.

The height change comes later because it changes saved game geometry and submission/assembly behavior. The UI/reveal fixes can be checked independently first. Changes affecting the final candidate require relevant K7 regression evidence; fresh friend onboarding remains pending until actually exercised. Codex reviews supplied evidence, with focused queries as necessary, before the next item starts.

## QOL-002 — Restart sequential corpse reveal per creature

**State:** Ready; Batch A, first. **Reported bug:** the first creature reveals head, then body, then legs correctly; selecting the next creature leaves it fully revealed.

Reset reveal progress and playback timing whenever the selected creature changes. Start the new creature at the head stage and reveal body/legs sequentially using the existing interaction/timing. Switching backward, selecting another gallery game and switching during playback must not reuse another creature's progress or old timer callbacks. Preserve any existing explicit full-reveal control.

**Acceptance:** demonstrate at least two creatures in each two-/three-player game; finish the first reveal then select the next and observe head-first progression. Test rapid next/previous and switching mid-reveal. Artwork, notes privacy, exported composition and saved drawings do not change. Provide before/after UI evidence and a regression test for the state reset.

## QOL-003 — Collapse and restore shared-canvas navigation sidebar

**State:** Ready after QOL-002 review; Batch A. Add a small conventional chevron at the top of the left bookmark/navigation pane. Clicking collapses the pane and expands drawing space; leave an accessible edge button to restore it. Direction should indicate collapse/expand; include a tooltip and accessible label. Retain the minimalist appearance.

**Acceptance:** collapse/restore preserves camera target, selected bookmark, artwork and navigation contents; painting does not jump or create a stroke when the control is clicked. Test narrow windows/high DPI and reopening the pane after navigating. Keep this a local view preference, not a shared canvas mutation. Scope is the shared-canvas left pane only; no general layout redesign.

## QOL-004 — Bottom-left dimensions and zoom

**State:** Ready after QOL-003 review; Batch A. Reuse PhotoCraft's existing zoom and document-dimensions display/control in Komaphone's bottom-left area. The screenshot shows a zoom percentage followed by `1536 px x 1024 px (72 ppi)`. Required information is current dimensions and zoom; preserve existing control behavior where reused. Keep the rest of the minimalist layout.

**Acceptance:** finite drawings show actual active width/height and current zoom, updating after switching drawings, zooming and resizing. Reuse the existing zoom control semantics; avoid a second inconsistent zoom state. Do not invent finite full-world dimensions or misleading percentages for the deep-zoom canvas: retain accurate existing scale semantics and use an explicit unbounded-canvas label where document dimensions are not meaningful. For party editing, dimensions describe the editable frame/segment rather than reference strips. Test high DPI and narrow layouts without overlapping tools/canvas. PPI is not a new editing feature requested here.

## QOL-005 — Reuse PhotoCraft's full layer interface

**State:** Ready after Batch A reviews; Batch B. Replace the clunky Komaphone layer list with the existing PhotoCraft layer interface, including drag reordering and the same layer options. Reuse repository code and engine commands; do not build a second approximate layer implementation or expand the whole workspace into Advanced mode.

The worker first inventories the actual PhotoCraft layer UI controls/context menus and returns a mapping of each option to its reused command and enabled condition. This inventory is implementation discovery, not a broad Codex code audit. Preserve all compatible options; document any option constrained by game assignments or shared author ownership rather than silently omitting it. Disabled actions should explain the relevant restriction. Existing locked-background rules and hidden assignment protections must hold through drag/drop and context menus.

**Acceptance:** demonstrate drag reordering with correct compositing and undo/redo; visibility, selection, rename, opacity, lock, add/duplicate/delete and every additional option found in the inventory behave as in PhotoCraft where authorized. Exercise empty/locked/multiple-layer states, save/reopen and party assignment boundaries. Identify finite-editor versus shared-world support explicitly; do not expose finite-document layer actions that bypass world author permissions. Inspect the resulting minimalist workspace at normal and high DPI. Reuse rendering/input conventions and theme tokens.

## QOL-006 — MS Paint Airbrush

**State:** Ready after QOL-005 review; Batch C. Add a tool named **MS Paint Airbrush** with the user-described spray behavior: tiny discrete dots scattered within a circular area controlled by brush radius; holding the mouse button still keeps depositing more dots, increasing coverage over time. Moving while held sprays along the path. This is a visual behavior reference, not permission to copy Microsoft code/assets or imply endorsement.

Use existing paint/command/replay architecture. Dot density should follow elapsed drawing time rather than frame rate or pointer-move event count. Bound generation/work for long holds or delayed frames. Stop on release, focus loss or cancellation; group a press/drag/release as one undoable stroke. Persist resolved randomness/events or an equivalent deterministic replay representation so replay and reopen reproduce accepted marks. Honor current color, brush radius, zoom coordinates, masks/locks and assigned drawing boundaries. Keep controls simple; extra professional brush settings are outside this request.

**Acceptance:** a stationary hold visibly accumulates dots; radius changes the spray footprint; movement produces discrete spray rather than a solid/soft brush. Test short/long holds, fast/slow frames, focus loss, undo/redo, recovery and replay final pixels. Exercise party tools and shared canvases wherever brush tools are offered, preserving author-local undo and sync/reconnect. Confirm compatibility across existing pixel-depth/color paths. Supply a short visual demonstration for owner comparison: no exact-match claim to an unspecified MS Paint version based solely on the description.

## QOL-007 — Resize only the current corpse segment's height during drawing

**State:** Ready after Batch C review; Batch D. Each player can make their own current segment longer or shorter after receiving it and while editing. This is not a lobby preset, host decision, or resize of the entire creature. Width stays fixed. Prefer a bottom-edge drag handle with a small contextual hint; reuse an accessible numeric height control if useful. Keep the top/reference seam anchored. Initial default height stays compatible with existing games; each submitted segment can have a different height.

Resizing changes canvas bounds, not artwork scale. Expansion adds blank space below. For shrinking, prevent silent clipping/loss: initially constrain the edge to retain existing marks; if content-removing crop is offered, make it explicit and undoable. Explain any enforced bound. Keep ordinary height changes and strokes recoverable and undoable without affecting another player's segment. Choose measured min/max heights and total output limits; preserve a valid reference strip and reject absurd dimensions safely.

On submission, commit dimensions and pixels together as immutable accepted data. Handoff derives the permitted bottom seam from the actual submitted height; expose no hidden prior artwork. Reveal/export assemble head/body/legs using each accepted height, with total height equal to their sum and no stretching, gaps or duplicated strips. Reconnect/retry must not mix old dimensions with new pixels. Once submitted, a player cannot resize that accepted segment.

**Acceptance:** two-/three-player games mixing short/default/tall segments, resizing blank and already drawn segments, undo/redo, crash/reopen, disconnect/retry, seam alignment, variable-height sequential reveal, PNG/archive export/import and existing-game restoration. Recheck QOL-002 reveal reset and QOL-004 dimensions. Document schema/protocol impact and old-client handling before release; migrate old fixed-height data without changing art/credits. No live service rollout without applicable authorization and compatible clients. This item explicitly amends the original equal-height game assumption, while retaining exactly three rounds and fixed width.

## QOL-008 — Review invite-only hosted admission

**State:** Deferred at owner request; no investigation/implementation now. Goal: only the owner and invited friends may use hosted resources; having a Google account must not grant service enrollment. The baseline already requires private service admission, so investigate the actual existing behavior when this item becomes active before proposing replacement networking/authentication.

Later acceptance should cover uninvited sign-in denial without allocating games/canvases/uploads, scoped expiring/revocable invitations, allowed friend admission, and continued per-room/per-canvas authorization. Do not confuse an accessible HTTPS login endpoint with open access to storage or gameplay. Resource/request limits still matter; invitation-only membership cannot guarantee zero cost from unsolicited traffic. Report current evidence, then scope only gaps. This deferred review does not certify present production restrictions or reopen settled GCP/Google choices.

## Evidence and next handoff

For every item, use the backlog's worker/review templates and the orchestration evidence contract. Include affected files/commands, task-relative diff, exact results, actual UI evidence, source/package identity, data/permission consequences, and K7 tests invalidated/refreshed. Mark Awaiting review, then stop. Codex records the disposition before another item begins.

First worker instruction: **Implement only QOL-002 from this document using the adjustments-backlog worker prompt. Preserve current unrelated work and the last tested release candidate. Return its evidence packet for Codex review.**
