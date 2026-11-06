# Komaphone agent implementation prompts

Use these in the Komaphone repository with the intended model, GPT-6 Astra, and goal mode/subagents where available. These are ordinary task instructions, not an API integration or a claim that a model setting guarantees completion. The product baseline and executable checks decide success.

## How to run

Run one coordinator at a time. It may delegate independent work to subagents. Choose either the reusable next-milestone prompt or one explicit milestone prompt below; do not start all prompts in parallel. Each prompt relies on tracked repository instructions, so a new session can recover context. Model and budget selection remain your app settings; no token or spending budget is implied here.

The working pattern uses scoped tasks, repository-held state, tests, and repair loops, consistent with [OpenAI's guidance for long-running Codex work](https://developers.openai.com/blog/run-long-horizon-tasks-with-codex). Explicit completion criteria and instructions help coding agents follow the intended task; see [OpenAI prompt guidance](https://developers.openai.com/api/docs/guides/prompt-engineering). The milestones and prompts themselves are Komaphone-specific engineering decisions.

## Reusable next milestone prompt

```text
Start a goal to complete the next dependency-ready Komaphone milestone in
docs/komaphone/delivery-plan.md. Read AGENTS.md, docs/komaphone/product-spec.md,
docs/komaphone/architecture.md, and docs/komaphone/status.md first.

Implement it end to end: inspect the current checkout, confirm prior milestone
evidence, choose routine reversible details, use subagents for independent
modules and review, integrate their work, run the required tests and visual
checks, and fix failures. Assign file ownership; keep shared schema/registration
edits under one coordinator and use the repository's per-agent Cargo target dirs.

Keep the agreed scope: Windows, Rust/egui/wgpu, private sync service, two/three
players, Animation and Exquisite Corpse first, deep-zoom shared world later.
Baseline 1.1 requires remix, guided zoom tours, and since-you-were-away playback;
deliver them at their assigned K4/K6 gates rather than treating them as optional.
Preserve upstream attribution and the working editor. Do not pick unrelated
Photoshop parity work. Keep the product baseline stable.

Use docs/komaphone/status.md as the durable handoff. Record exact commands,
results, artifacts/screenshots, assumptions, changed files, limitations, and the
next action; also append log/devlog.md. Do not mark complete until the milestone's
observable acceptance criteria and mandatory checks pass. If an external gate
cannot be exercised, complete independent work and identify exactly what remains.
Do not silently replace a requirement with a mock or a weaker feature.

Continue without asking about routine implementation choices. Ask only for a
material product change or genuinely missing external authorization/access.
Prepare any hosting/deployment decision so it is concrete and reviewable; do not
buy services or publish without applicable authorization. Finish with the working
result, how to try it, evidence, and remaining gaps. Stop at this milestone's gate.
```

## K0 Baseline and contracts

```text
Start a goal to complete K0 in docs/komaphone/delivery-plan.md. Follow AGENTS.md
and read all four Komaphone specification/architecture/plan/status documents.
Establish the actual build/test baseline and verify existing drawing, frame/GIF,
and recovery primitives. Implement minimal versioned game/storage/replay
contracts, validated IDs and limits, phase-filtered views, and a deterministic
headless game state machine with focused tests. Record the smallest suitable
Rust service/storage/transport design and its WASM boundary.

Delegate independent code/reuse and protocol-risk reviews. Own shared manifests
and registrations centrally; keep manifests valid. Do not build a speculative
framework or begin all later milestones. Finish K0's acceptance checks and update
docs/komaphone/status.md plus log/devlog.md with actual evidence and a precise K1
handoff. Routine reversible decisions are yours; preserve the frozen product.
```

## K1 Reliable local drawing

```text
Start a goal to complete K1 in docs/komaphone/delivery-plan.md. Read AGENTS.md and
the Komaphone product, architecture, plan, and status. Verify K0 prerequisites;
repair a small prerequisite gap if necessary and record it. Build the simplified
mouse-first workspace, durable local drafts, accurate save status, recovery,
and versioned replay recording from the first edit. Reuse the existing engine
and keep advanced capabilities available. Do not confuse autosave scheduling
with completed durable persistence.

Delegate workspace and persistence/replay work under explicit file ownership;
have an independent reviewer test crash/disk failure boundaries. Exercise the
listed tools and kill/reopen scenarios, inspect real UI screenshots, and run
AGENTS.md gates for changed crates. Finish only with K1 evidence; update tracked
status and local devlog. Do not spend time on public accounts, AI, or parity work.
```

## K2 Private rooms and synchronization service

```text
Start a goal to complete K2 in docs/komaphone/delivery-plan.md. Read AGENTS.md and
the Komaphone docs; verify K0/K1 evidence. Build the Rust private service, durable
room authority and asset store, invitation/membership checks, lobby UI, immutable
submissions, reconnect, timers/pause, moderator transfer, and local offline queue.
Keep the service and multi-client harness runnable locally with isolated data.
Never expose the editor automation endpoint as the multiplayer service.

Delegate service and client integration with agreed contracts; independently
test malformed/duplicate/reordered requests, unauthorized asset reads, client
restarts, and service restart. Drive two and three actual clients, not just UI
mocks. Record local run commands, limitations, required repository checks and
screenshots in status/devlog. Prepare external hosting decisions without buying
or deploying infrastructure. Complete K2's acceptance gate before stopping.
```

## K3 Animation and Exquisite Corpse

```text
Start a goal to complete K3 in docs/komaphone/delivery-plan.md. Read AGENTS.md and
the Komaphone docs; verify K1/K2 prerequisites. Implement both modes exactly as
the product baseline specifies: one chain per player, cyclic assignment,
previous-frame onion skin and metadata notes for Animation; three bands and
only the cropped joining strip for Exquisite Corpse. Include two-player repeat
authorship, full reveal, immutable submission, and interrupted-round recovery.

Delegate one mode to each of two subagents where available; keep shared lobby,
state, and protocol edits coordinated. Verify hidden data is withheld from
payloads/caches/control access, not simply covered by the UI. Run full games
with two and three clients, boundary-row assembly tests, submit races, and
reconnection at each phase. Inspect UI and run required repository checks.
Update status/devlog with evidence. Do not add classic prompt/guess mode.
```

## K4 Replay gallery and party release

```text
Start a goal to complete K4 in docs/komaphone/delivery-plan.md. Follow AGENTS.md
and the frozen Komaphone docs; verify K3 is integrated. Deliver per-frame and
per-segment drawing timelapse, whole-animation playback, gallery, GIF/frame PNG/
creature PNG exports, editable portable archives, and backup/restore. Replay
must account for erasing, undo/redo, fills, transforms, layers, imports and
checkpoint-only steps, ending at the accepted image. Notes/cursors stay out of
artwork. Legacy imports must not invent missing history.
Include G2 local remix: new editable drawings/animations from revealed results,
fresh IDs, independent changes, preserved frames/timing and original credits,
source revision/provenance, and a new replay baseline. Reject unrevealed sources;
test deleting either copy, archive restoration, and no mutation of the original.

Delegate gallery and replay/export independently; review recovery and archive
round trips. Produce a runnable Windows party build and local service runbook.
Verify both complete games survive close/reopen and export/import to a fresh
data directory. Smoke-test both modes across two separate Windows machines via
the private service before claiming friend-ready. If unavailable, record a
partial local-only candidate and the pending external gate. Inspect actual
reveals/exports, run applicable crate/clippy/WASM/corpus/performance gates, and
update status/devlog. The shared world is still outstanding.
```

## K5 Prove deep zoom

```text
Start a goal to complete K5 in docs/komaphone/delivery-plan.md. Read AGENTS.md and
the Komaphone docs; keep the completed party flows intact. Build a single-user
hierarchical scene prototype with bounded local drawing regions, stable IDs,
progressive previews, lateral expansion, bookmarks/home, and single-target
automatic zoom. Demonstrate new artwork inside artwork across six roughly 100x
detail steps, persistence/reopen, precision stability, and viewport-only loading.
Six levels are the test fixture, not the product limit.
Use stable region/local-coordinate camera targets reusable by K6 guided tours.

Delegate scene/storage and render/performance experiments with clear ownership.
Do not call a larger bitmap or a raised zoom constant a deep-zoom solution.
Inspect the rendered result, record memory/frame-time data, validate bounds,
then document the chosen world format and limits. Run all relevant repository
gates and update status/devlog. If the proof fails, identify and fix the failing
property rather than quietly reducing the requirement or claiming completion.
```

## K6 Persistent collaborative world

```text
Start a goal to complete K6 in docs/komaphone/delivery-plan.md. Follow AGENTS.md
and the Komaphone docs; verify the K5 proof and K2 service. Deliver the persistent
deep-zoom world with simultaneous drawing, author-owned layers, author-local
undo, ghost cursors/live previews, durable offline operations, service catch-up,
deterministic merge, bookmarks/auto-zoom, and selected-view/full-archive exports.
Also deliver required W7 saved multi-stop tours with GIF/PNG-sequence export,
W8 since-you-were-away highlights/navigation/replay, and G2 world remix with
animated/still content, independent ownership, and preserved provenance.

Delegate sync correctness and world UI/render integration; independently test
overlapping and non-overlapping online sessions, conflicting offline metadata,
duplicated/reordered messages, client/service crashes, and interrupted assets.
Require converged canonical scene hashes and preservation of both authors' work.
A's undo must not revert B. Presence loss must not lose committed drawing.
Verify tour pause/scrub/cancel, missing stops, pinned export revisions, and
save/sync/archive round trips. Catch-up must include offline uploads and undo/
deletions, preserve canonical replay when filtering authors, resume after restart,
and mark only an explicitly reviewed interval seen, never background downloads.
Verify world remix retries, deep placement, animation timing, credits, and source
independence. These are required completion gates, not optional polish.
Inspect the shared UI and rerun relevant performance, storage, WASM, and safety
gates. Update status/devlog with reproducible evidence and remaining limits.
```

## K7 Final integration and distribution

```text
Start a goal to complete K7 in docs/komaphone/delivery-plan.md. Read AGENTS.md and
the Komaphone docs. Audit every required product ID against actual evidence;
use subagents to review reliability/networking and UX/exports independently.
Fix gaps, prepare the Windows distribution, test clean install and upgrades,
backup restoration, protocol mismatch, quotas, and both party/shared-world flows.
Include all G2/W7/W8 remix, guided-tour/export, and catch-up acceptance cases.
Measure performance on the recorded reference machine and inspect final UI.

Prepare a concrete private hosting setup/runbook. Complete local work before
requesting genuinely missing deployment access or a hosting expense decision.
Verify distinct-machine/network use when available; label it unverified if not.
Do not claim the deployed product complete while that acceptance gate is missing.
Update status/devlog with exact checks, artifacts, release instructions, known
limits, and all outstanding work. Preserve attribution and baseline 1.1 scope.
```

## Resuming an interrupted run

```text
Resume the incomplete Komaphone milestone recorded in docs/komaphone/status.md.
Read AGENTS.md and the Komaphone product/architecture/plan first. Inspect the
actual working tree and existing processes before repeating work. Preserve
uncommitted user/agent changes. Check the last evidence, continue from the next
action, repair failures, and finish the original acceptance gate. Update tracked
status and log/devlog.md. Do not restart the project, broaden scope, or claim
that prior unverified checks passed.
```
