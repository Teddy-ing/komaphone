# Komaphone adjustments backlog

Created 2026-11-17 at the owner's request. A living queue for owner-requested quality-of-life changes, fine-tuning and later additions. Active queue items are recorded below; examples used to explain this workflow are not requests. Follow the [orchestration workflow](orchestration-workflow.md): Antigravity implements and gathers evidence; Codex reviews the packet; the owner relays messages.

## Scheduling and scope

These items may proceed before the sole remaining K7 acceptance gate — fresh friend onboarding from another household (`H1`, `H2`, `H4`) — while that tester awaits availability (the remaining `K4-LAUNCH-01` laptop relative-export/reopen subcheck was explicitly waived by the owner for K7 on 2026-11-17 with full-path export guidance retained). This does not mark unexecuted tests passed, waive external friend onboarding, or close K7. Read the current [status](status.md) before selecting work; do not assume every remaining gate is only a scheduling issue. Preserve pending reviews and the last tested release candidate.

Run one bounded item at a time. Do not overlap implementation with another worker modifying the same checkout or live service. Finish an item's evidence packet and Codex review before starting the next implementation item. New ideas can be added to the queue anytime without starting them. User priority overrides arrival order; dependencies determine readiness.

An accepted adjustment is not proof of K7 completion. Each change must identify which previous acceptance results remain applicable and which need rerunning against the new candidate. Keep old evidence intact; update the final package/source hashes and required release regressions. Changes to game rules, storage, protocol, auth or permissions require explicit compatibility and migration analysis. Record owner-approved changes to frozen product behavior in the specification/revision history; a backlog entry does not silently override the baseline.

## Queue

| ID | Requested outcome | Priority/dependencies | State | Handoff/evidence/review |
|---|---|---|---|---|
| `QOL-001` | Replace inherited `"PhotoCraft"` product wording in the Safe GPU / CPU-compositor fallback notice modal with **Komaphone** while preserving fallback controls and upstream attribution | Normal; independent (do not modify or rebuild Candidate 3 until owner requests implementation) | Proposed | Queued 2026-11-17 from Checkpoint C isolated Safe-GPU screenshot (`log/k7/launcher-candidate3/screenshots/cycle-3-safegpu.png`); Candidate 3 (`9A54729E…` / app `1146466D…`) intentionally unchanged |

Assign stable IDs `QOL-001`, `QOL-002`, and so on as actual requests arrive; do not reuse IDs. Use **Proposed -> Ready -> Implementing -> Awaiting review -> Accepted**. Review can return an item to **Changes required**; use **Blocked** or **Deferred** with a specific reason when appropriate. Only Codex's recorded disposition moves an item to Accepted in this workflow; identify whether it is based on supplied evidence or direct checks.

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

Update the item to Awaiting review, tracked status and local devlog. Return the
orchestration workflow's compact evidence packet: task-relative diff, exact
commands/results, failures/skips, screenshot paths where relevant, source and
artifact identities, remaining limits and affected K7 gates. Stop for Codex
review; do not begin the next item or label your own work Accepted.
```

## Codex review prompt

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
