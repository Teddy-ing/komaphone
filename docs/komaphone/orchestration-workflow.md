# Codex review and Antigravity implementation

Owner-selected workflow, 2026-11-15. Applies to finishing K7 and subsequent owner-requested small adjustments. Product baseline 1.4 and all acceptance requirements remain unchanged. This document changes responsibility and handoff format, not model settings or tool permissions.

## Responsibilities and quota discipline

- Codex scopes work, writes handoffs, reviews supplied evidence, asks focused questions and gives a qualified disposition. It does not start implementation, broad code investigation, builds or subagents by default in this workflow. The owner currently has little Codex quota; prefer one compact review and one consolidated question packet per checkpoint.
- Antigravity is the implementation and investigation worker. The owner manually carries prompts and responses between tools; there is no assumed direct subagent connection or shared conversational memory. Antigravity must explicitly read repository instructions and the current handoff. Its tool access and capabilities must be established, not assumed.
- Only one implementation worker/coordinator writes at a time. Preserve existing dirty/untracked work, user drawings, profiles, cloud resources and build caches. Never reset/clean/mass-commit the checkout to manufacture a clean baseline. Record pre-existing changes separately from this task's edits using before/after snapshots or patches; HEAD alone is insufficient in this dirty checkout.
- Keep each task bounded to one observable outcome. Small adjustments need a before/after behavior, touched-area boundary and relevant regression checks; they do not authorize cleanup campaigns or architecture rewrites. Finish K7 closure separately from new polish unless a fix is necessary for acceptance.
- Run required checks in the worker environment. Reuse unchanged valid evidence with exact source/artifact provenance; do not repeat broad suites without a change, failed check or unresolved concern. Preserve every failed, stopped, ignored and unavailable result. Never reduce acceptance standards merely to save quota.
- Codex initially reviews the packet only. If crucial evidence is missing, request exact evidence from Antigravity rather than searching the whole repo. For auth, migrations, sync, data loss or live operations, request a narrowly scoped independent code/artifact check when needed; if the owner prefers no direct inspection, report the unresolved assurance limit instead of certifying safety. A factual-looking response can still omit context or be wrong.

## Evidence packet required at each checkpoint

Store detailed evidence in a task-specific local log directory and update tracked status with durable findings. Return a concise index, not thousands of log lines. Include:

1. Task ID, requested outcome, what changed and what did not, baseline commit plus relevant dirty-file hashes, end-state source hashes, and exact candidate/build/package identities. State the environment and timestamp.
2. Files changed by this task, diff statistics and a task-relative patch (including newly added files). Separate pre-existing edits. Include the relevant complete functions with paths, symbols and line ranges for nontrivial changes; include caller/validation boundaries when behavior depends on them. Label omissions and truncation.
3. Acceptance matrix: requirement -> exact command or manual procedure -> observed result -> evidence path -> tested source/binary. Record exit codes, pass/fail/ignored counts, and whether a check was local, hosted, owner-observed or friend-observed. A command listed without a result is not executed evidence.
4. UI changes: before/after screenshots from the actual candidate and steps used. Report who inspected them; merely generating a screenshot is not visual acceptance.
5. Failures, unfinished work, uncertain interpretations, tests not run and why. Identify any changes after testing and which results they invalidate. Keep raw logs/receipts unedited; create a separate corrected/addendum record when necessary.
6. Data/migration/auth/operational impact, recovery/rollback procedure and proposed next action. Redact tokens, credentials, private artwork and unnecessary identity information. Identify redactions without changing the meaning of code or results.

Use evidence labels: SOURCE EXCERPT, EXECUTED RESULT, OBSERVED UI, INFERENCE, or NOT VERIFIED. Exact code establishes what is written; execution establishes only the tested behavior. Do not turn either into an unsupported claim that all paths are safe. Hashes bind artifacts to a report but do not independently prove the report truthful or the code correct.

## Copyable worker continuation prompt: K7 checkpoint A

```text
Continue Komaphone K7 from docs/komaphone/status.md, especially the K7 pause
handoff dated 2026-11-14. Read AGENTS.md, product-spec.md, delivery-plan.md,
architecture.md, k7-release-checklist.md, k7-operations.md and
docs/komaphone/orchestration-workflow.md. Reconcile newer ledger entries first.
Codex is the reviewer; you are the implementation/evidence worker. Preserve
the existing dirty tree, artwork, profiles, candidate2 artifacts and retained
target/agent-k7-root cache. Do not restart implementation or repeat completed
native acceptance without a documented reason. Do not deploy or stop a live
service in this checkpoint.

Complete only the remaining local performance/provenance checkpoint:
1. Inspect the retained perf script and receipts. Preserve partial evidence and
   finish the required quick benchmarks using the existing cache; a stopped
   previous run is not a pass. Record failures and fix only demonstrated blockers.
2. Follow the handoff's rebind-candidate2 procedure after benchmarks. Preserve
   original receipts and record fresh source pins and exact binary comparisons.
   If hashes differ, stop the equivalence claim, identify the difference and
   report the required retesting; never relabel an untested binary as candidate2.
3. Reconcile local results into the tracked K7 ledger/checklist and local devlog.
   Keep hosted operations, K4-LAUNCH-01 and fresh other-household onboarding open.
4. Return the workflow's compact evidence packet with task-relative changes,
   exact results/artifact hashes, remaining gates and proposed next checkpoint.
Stop here for review. Do not add fine-tuning features or mark all K7 complete.
```

Next checkpoint: prepare/review current-state hosted upgrade evidence using the existing operations plan, then execute only within applicable live authorization. Reuse settled GCP/Google/budget decisions. A Codex review disposition is not new authorization for destructive work, extra spend or live changes. Subsequent checkpoints cover hosted regression, the affected laptop case and actual fresh friend onboarding from another household. K6.5's accepted broadband/cellular-hotspot test does not replace that K7 gate. If external participants are unavailable, finish independent work and leave the gate pending.

## Copyable evidence request to Antigravity

```text
Evidence-only investigation; do not edit source, rerun broad suites, deploy,
or repair anything in this response. Answer questions Q1...Qn separately.
For each, return:
- Exact file path, symbol, line range and current file hash.
- Verbatim complete relevant function/block, including validation/callers needed
  to answer the question. Mark omitted context; do not replace code with a paraphrase.
- Existing test command, exit code, result excerpt, receipt path and tested
  source/binary hash if available. Say NOT RUN or NOT VERIFIED when unavailable.
- Separate observed facts from inference. If evidence permits several answers,
  say so; do not force a single certain answer or invent missing evidence.
Keep the response focused on the questions. Redact credentials, not semantics.
Questions: [Codex inserts a consolidated numbered list here.]
```

## Copyable Codex checkpoint review prompt

```text
Review this Antigravity evidence packet under
docs/komaphone/orchestration-workflow.md. Conserve quota: begin with the packet
and relevant handoff only. No broad repository investigation, implementation,
builds or subagents. Check scope, acceptance coverage, internal consistency,
source/artifact provenance, failure disclosure and data/auth risks.
Return either a short qualified disposition or one consolidated evidence-only
question prompt for Antigravity. Request exact source/diff/result excerpts where
needed. Do not equate a worker's assertion with independent verification.
If a critical uncertainty needs direct inspection, state the specific files or
check needed and why; do not broaden into a full code audit. Keep required gates
open until supported, and separate local acceptance from hosted/friend acceptance.
Packet: [paste the concise worker report here.]
```

Review dispositions: **needs evidence**, **changes required**, **checkpoint accepted based on supplied evidence**, or **independently verified for the explicitly listed checks**. A checkpoint acceptance is not whole-app certification or K7 completion. Record exactly which direct checks, if any, Codex performed. Future tuning tasks use the same cycle: bounded behavior request -> worker patch/evidence -> focused review -> required corrections -> disposition.
