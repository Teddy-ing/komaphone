# Komaphone agent implementation prompts

Use these in the Komaphone repository with the intended model, GPT-6 Astra, and goal mode/subagents where available. These are ordinary task instructions, not an API integration or a claim that a model setting guarantees completion. The product baseline and executable checks decide success.

## Current owner workflow

Since 2026-11-15, use the [orchestration workflow](orchestration-workflow.md) for Codex review and owner-relayed Antigravity implementation. It includes the bounded K7 continuation and evidence-query prompts. The older implementation prompts below are worker task templates; they do not instruct Codex to launch implementation automatically. Keep all acceptance requirements intact.

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
party players, Animation and Exquisite Corpse first, deep-zoom canvases later.
Baseline 1.4 requires multiple named canvases with independently chosen dynamic
member lists of variable size, plus remix, guided tours, and catch-up playback.
Named groups are examples only; there is no fixed four-person canvas product cap.
Deliver these at their assigned K4/K5/K6 gates. Keep party roster limits intact.
Read docs/komaphone/hosting-plan.md for required GCP Compute Engine hosting and
Google sign-in: account/linking and Linux deployment foundations in K6, hosted
independent-public-network acceptance in K6.5, then K7 hardening and fresh friend onboarding from another household. Target total recurring
USD $20/month or less; approximately $30 is acceptable. Include all resource
costs before credits; do not silently grow beyond that preference. Choose K6.5
after K6 rather than skipping directly to K7. LAN/VPN tests do not prove the
normal hosted friend experience. Local drawing must remain usable offline.
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
the private service before claiming private-network readiness. K6.5 separately
proves hosted internet play; do not generalize this LAN result. If unavailable, record a
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
Use the product spec's Endless Paper behavior reference: pan/zoom and immediately
draw at the new scale, with automatic region selection and predictable brush
width. No mandatory Create detail step or visible internal region boundaries.
Evaluate retained strokes/raster/hybrid as needed without adding vector UI.
Create/name/rename/switch multiple local canvases with independent stable IDs,
owner identity, drawings/history/bookmarks and recovery; test four canvases,
including identical names. K6 supplies friend invitations and network enforcement.

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
Read docs/komaphone/hosting-plan.md before freezing identity/storage contracts.
Implement versioned account/profile/device separation and safe legacy linking
with ownership proof; preserve IDs, accepted authorship, memberships, queued
edits and local recovery. Do not link by email/name alone. Exercise wrong-account
linking, account switches and isolated queues through a test-only verifier
adapter; real Google login is a K6.5 gate. Keep local drawing usable signed out.
Prove the existing service builds/runs on Linux with persistence/restart and the
proposed trusted-HTTPS termination route in an isolated local environment.
Prepare repeatable GCP configuration, resource inventory and a current complete
cost estimate, aiming for <=USD $20/month with approximately $30 acceptable.
Missing cloud/OAuth access does not block independent K6 work; record K6.5's
remaining inputs and do not call a local deployment spike hosted acceptance.
Implement W9 independent canvas ownership and invite/accept/remove/leave controls.
Verify arbitrary overlapping/non-overlapping groups, different owners, and
canvases excluding the primary test user. Exercise 2/4/8 concurrent editors as
test fixtures, not caps, plus larger paginated member lists. Separate invited
membership from concurrent connection capacity; document measured operational
limits, not preset friend slots. Party games remain two/three players.
Migrate K5's local-only owner checks into per-canvas authorization: separate
owner, authenticated writer and viewer; retain author-local undo and local IDs.
Enforce per-canvas authorization on lists/names/thumbnails, assets, edits, cursors,
history, tours, exports and catch-up; global pairing is not membership. Test
wrong-world references, stale invitations, active/offline revocation and recovery
of rejected drafts. Rejoining cannot silently publish an old revoked queue.
Copies/archive imports are owner-only; cross-canvas remix explicitly names and
authorizes the destination without granting access to private source history.

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
Stop at K6's acceptance gate; the next milestone is K6.5, not K7.
```

## K6.5 GCP hosting and Google sign-in

```text
Start a goal to complete K6.5 in docs/komaphone/delivery-plan.md. Read AGENTS.md,
the Komaphone product/architecture/status, and docs/komaphone/hosting-plan.md.
Verify K6 identity/linking, Linux feasibility and world evidence plus K4 party
evidence. Preserve current artwork and all existing two/three-player and dynamic
canvas-membership rules. Do not restart the project or rewrite working sync.

Complete native system-browser Google OAuth/OIDC sign-in with PKCE/state/nonce,
server-verified identity and revocable Komaphone sessions. Keep profile IDs and
device writers distinct, prove authority before linking old profiles, preserve
queued/local work and credits, and isolate account switches. Test cancellation,
expired/wrong-audience/replayed tokens, credential storage/redaction and access
revocation. Google sign-in grants no automatic room/canvas access. Enforce private
service admission and scoped invitations; close any legacy enrollment bypass.
Local drawing/recovery remains usable signed out or during a service outage.

Prepare the existing Rust service for one small Linux GCP Compute Engine VM,
persistent disk/SQLite, trusted renewable HTTPS, supervised boot/restart,
bounded logs/resources, and consistent off-VM backups. Deliver repeatable
deploy/update/rollback assets and simple admin/friend runbooks. Package a normal
configured client connection: no VPN/Tailscale, manual certificate installation,
home-router setup or developer-token copying required of friends. Keep editor
automation loopback-only and native Rust/WASM boundaries intact.

Refresh pricing for all VM/address/disk/backup/traffic/log/domain costs and taxes.
Target total recurring <=USD $20/month; approximately $30/month is acceptable.
Record ordinary pre-credit cost, credit eligibility/expiry, capacity measurements,
light/higher-use forecasts and budget alerts (which do not cap spend). Do not
silently add infrastructure or increase recurring spend materially beyond ~$30.
The provider/login/budget choices are settled; ask only for genuinely missing
project/access/deployment decisions after making the proposed deployment concrete
and reviewable. Use any applicable authorization already present; planning alone
does not authorize provisioning. Continue independent local work if access is
missing and leave actual cloud/login/remote evidence explicitly pending.

Delegate independent authentication/migration and deployment/runbook modules
with explicit file ownership; independently review authorization and recovery.
When concretely authorized, deploy and test packaged Windows clients on independent public internet connections using actual Google
sign-in and normal invites. The owner-approved H4 clarification allows home
broadband plus a cellular hotspot with phone Wi-Fi off, even in one home;
K7 retains fresh friend onboarding from another household.
Complete both party modes, reconnect during phases, service restart and exports.
Exercise simultaneous and non-overlapping canvas use, non-owner edits, denied
nonmembers, offline queues, account switching, tours/catch-up/remix and fresh
off-VM backup restoration. Verify HTTPS renewal/reboot. Retain broader K6 member
fixtures and exact two/three-player coverage; report actual remote configurations.
Fix K4-LAUNCH-01 earlier if it prevents these clients launching, keeping its K7
regression gate. Inspect UI and run all required checks for affected crates.

Complete every hosting-plan K6.5 acceptance case; a mock login, same-LAN test,
VPN workaround or deployment script alone cannot close the milestone. Record
exact checks, build/configuration versions, artifacts, costs, tested runbooks,
limitations and genuinely missing external evidence in tracked status and local
devlog. Stop at K6.5's gate. K7 then hardens this demonstrated hosted experience.
```

## K7 Final integration and distribution

```text
Start a goal to complete K7 in docs/komaphone/delivery-plan.md. Read AGENTS.md and
the Komaphone docs including docs/komaphone/hosting-plan.md. Verify accepted K6
and K6.5 evidence before claiming this release ready. Audit every required
product ID, including H1-H4, against actual evidence;
use subagents to review reliability/networking and UX/exports independently.
Fix gaps, prepare the Windows distribution, test clean install and upgrades,
backup restoration, protocol mismatch, quotas, and both party/shared-world flows.
Include all G2/W7/W8 remix, guided-tour/export, and catch-up acceptance cases.
Include W9 isolation, variable member groups including more than four, and
revocation tests. Keep benchmark group sizes separate from product limits.
Measure performance on the recorded reference machine and inspect final UI.

Repeat the proven K6.5 GCP/Google-sign-in path with fresh installs and upgrades
on separate household networks, without VPN/manual certificates. Retest account
linking/switching, backup restoration, rollback, TLS renewal, offline use and the
K4-LAUNCH-01 laptop launcher/export issue. K7 is not the first internet test.
Recheck the full pre-credit recurring cost (<=USD $20 target, approximately $30
acceptable) and measured limits. Continue independent checks if external access
is missing; leave those gates unverified and do not claim the deployed product
complete. Do not ask again to choose the already selected provider/login/budget.
Update status/devlog with exact checks, artifacts, release instructions, known
limits, and all outstanding work. Preserve attribution and baseline 1.4 scope.
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
