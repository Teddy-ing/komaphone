# Antigravity continuous implementation workflow

Owner update, 2026-11-21. This replaces the 2026-11-15 mandatory Codex review/handoff workflow. Antigravity implements, investigates, verifies, records results and continues to the next ready authorized item. Codex review is optional and occurs only when the owner requests it. Product requirements and runnable acceptance checks remain in force.

## Continue without review pauses

Follow the current [backlog](adjustments-backlog.md), [status](status.md), product specification and delivery plan. Finish the active task first. Once its applicable acceptance checks pass, record it as **Complete (worker-verified)** and proceed to the next ready item in owner priority order. Do not wait for a Codex response, evidence-packet approval or per-item owner confirmation. Work remains bounded and sequential; do not launch competing writers or whole-app builds.

Old instructions saying Stop for Codex review, Awaiting review or only Codex may accept are superseded. For an item already awaiting review, reconcile its existing receipts with current source/artifact hashes, complete any genuinely missing checks, then close it as worker-verified and continue. Do not rerun valid unchanged checks solely because the workflow changed. Preserve historical Codex dispositions and failed/skipped evidence; do not represent worker verification as independent review.

Keep deferred items deferred and respect real dependencies. If a task is blocked by missing external access or a participant, record the exact gap and continue independent ready work. Do not weaken a requirement or mark an external test passed because the user is confident it will work. Implementation completion and release readiness remain separate.

## Verification and durable handoff

For each item record changed files, a task-relative diff or equivalent provenance, exact commands/results, failures/ignored checks, tested source/artifact identities, visual evidence where relevant, and release checks invalidated/refreshed. Update tracked status, the backlog and local devlog. Keep raw detailed logs locally and a compact durable summary; a large paste-ready Codex packet is no longer required at every item.

Preserve user artwork, local recovery, credentials, existing dirty/untracked work, accepted artifacts and upstream attribution. Follow repository crate/layer/WASM/input-safety checks. Use current instructions and retained receipts rather than restarting old milestones or overwriting existing changes.

This workflow change removes review pauses only. It does not grant new deployment, destructive-action, spending or scope authorization. Reuse existing authorization and settled choices; ask only for genuinely missing material decisions after finishing independent preparation. Do not provision replacement resources or deploy incompatible clients/services just because an item is worker-verified.

## Copyable continuation prompt

```text
Continue the active Komaphone task under AGENTS.md and
 docs/komaphone/orchestration-workflow.md, then advance through the ready backlog
in owner priority order. The owner removed mandatory Codex review handoffs on
2026-11-21. Do not stop after each item for Codex approval.

For items awaiting review, reconcile existing evidence with current source,
finish missing required checks, mark Complete (worker-verified) when supported,
and continue. Preserve historical results and do not rerun valid checks without
reason. Keep deferred items deferred and external release gates explicitly open.
Record concise results, source/artifact identities, limitations and next actions
in the backlog/status/devlog. Continue independent ready work around external
blockers. Preserve existing authorization boundaries for live operations, data,
spending and material product changes. No automatic Codex audit is required.
```

Codex may still help organize new requests or perform a focused review when explicitly asked. Such a review is optional and does not become a recurring gate for future items.
