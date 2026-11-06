# Komaphone implementation status

Last updated: 2026-11-05. Product baseline: [1.1](product-spec.md). Current stage: **K3 complete**. This tracked file is the durable agent handoff; `log/devlog.md` is a supplementary local log.

## Confirmed decisions

- Windows desktop first; keep upstream Rust WASM compilation working.
- Plan for a small private sync service; no paid provider or production deployment selected.
- Animation and Exquisite Corpse before the persistent deep-zoom world.
- Owner-approved additions are required: G2 non-destructive remix (local projects in K4, world placement in K6), W7 guided tours/export, and W8 since-you-were-away highlights/playback (K6).
- Two/three-player behavior and other bounded defaults are recorded in the product specification as assumptions.

## Milestone ledger

| Milestone | Status | Evidence or remaining gate |
|---|---|---|
| Planning | Complete | Product baseline, architecture, delivery plan, agent prompts, and root documentation aligned; documentation consistency review recorded below. |
| K0 Baseline/contracts | Complete | 53 contract tests, 77 xtask tests, clippy/layers/WASM/format checks; native drawing/recovery/frame/GIF/mouse checks and three inspected screenshots. See K0 completion below. |
| K1 Local drawing/recovery | Complete | Mouse workspace, durable drafts/replay, real crash/recovery tests, inspected native/layout artifacts; 10,567 native tests and 6,232 release corpus tests passed, required checks completed, K1 latency targets met, 24 MP before/after evidence retained. |
| K2 Service/rooms | Complete | Real authenticated two/three-client service flow, durable offline recovery, immutable native submissions, host claim, skip/end and timed freeze passed; required checks, inspected layouts and nine quick benchmarks passed. Final evidence below. |
| K3 Party modes | Complete | Four actual UI games / 41 manual submissions plus timed recovery; authorized references/caches, durable notes/reveals and immutable turns; required checks and nine quick benchmarks passed. Final ledger below. |
| K4 Replay/gallery/release | Not started | Upstream GIF export exists; full game replay/archive/gallery and local remix do not. |
| K5 Deep zoom proof | Not started | Existing sparse finite raster/zoom does not satisfy nested-detail world. |
| K6 Collaborative world | Not started | Depends on K5/K2/K4; includes guided tours/export, catch-up playback, and world remix. |
| K7 Friend-ready release | Not started | Packaging, cross-machine network checks, restore and full integration remain. |

## Repository audit

Inspected 2026-11-04 without running builds. These are code observations, not runtime verification:

- Mouse strokes and timing: `crates/paint/src/lib.rs`; commands: `crates/engine/src/cmd/brush_cmds.rs`.
- Native recovery: `crates/format/src/autosave.rs` and `apps/photocraft/src/services.rs`.
- Animation timeline/frames: `crates/doc/src/video.rs`, `crates/engine/src/cmd/timeline_cmds.rs`.
- GIF export: `crates/engine/src/cmd/video_cmds.rs`; frame persistence: `crates/format/src/convert.rs` and `crates/format/tests/it/roundtrip.rs` (`video_layer_frames_survive_roundtrip`). Some upstream animation-parity comments are stale; check code/tests before rebuilding a feature.
- Existing whole-document undo (`crates/ops/src/lib.rs`) and Actions journal (`crates/engine/src/actions_record.rs`) are not ready-made collaborative undo or durable replay protocols.
- Fixed document dimensions, finite `i32` geometry, and bounded raster zoom require a new multiscale scene for the shared world.
- Previous local devlog reported Rust/Cargo unavailable on PATH during import; K0 must establish the current environment, not treat that historical report as a fresh check.

## Latest handoff

**K3 complete, 2026-11-05 local / 2026-11-06 UTC. Stop at this milestone's gate.** Animation and Exquisite Corpse now work through the native UI for two and three players, including authorized references, copy/onion controls, durable notes, immutable submissions and restored reveals. Four full games completed with 41 manual submissions, plus a timed recovery/incomplete-game scenario. Required tests, strict Clippy, WASM, layers, parity, scorecard, formatting, visual checks and all nine release quick benchmarks passed. No K3 functional or external-access gap remains.

Use the [K3 runbook](k3-party-modes.md) to launch the [retained Windows app](../../log/k3/bin/photocraft.exe) and [private service](../../log/k3/bin/photocraft-sync.exe) with separate client roots. The [native result](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/result.json), [independent review](../../log/k3/k3-acceptance-audit.json), [performance review](../../log/k3/root-performance-review.json) and final K3 ledger at the end of this file record exact evidence and limits. All 36 native PNGs were reviewed; the coordinator directly inspected six final native and four static captures. The four static captures reproduced byte-for-byte after cache removal. All agent caches and test processes are gone; runnable artifacts, logs, screenshots, reports, toolchain and corpus remain.

The coordinator verified prior K2 artifact hashes and eleven successful post-color checks before choosing K3. Existing uncommitted K0–K2 work is preserved on `main` at base `69a9aacb566e036264a93ed6da925cf96eeda0bf`. No CI change, deployment, purchase, publication or commit occurred. Baseline 1.1, attribution and the Advanced editor remain intact. This is a **loopback development build**, not a friend-ready release. **Next milestone in a later run: K4 replay, gallery, portable exports/archives, required local remix, and separate-machine private-network release verification.** K6 world remix, guided tours/export and since-you-were-away playback remain required.


**Historical K2 handoff:** the previous run stopped at the completed K2 gate. The real private Rust service and native Windows clients now provide authenticated invitations, two/three-player lobbies, authoritative transitions, durable offline queues, immutable submissions, host-loss moderation, explicit skip/end, restart recovery and deadline revision pins. Final review fixes include correct Leave acknowledgments and shared CMS conversion for canonical artwork/seams while preserving native profiles/depths.

The full native suite passed 8,920 tests; the affected engine/party suite passed 3,596 after the final color fix (overlapping totals), plus 21 client and 26 service tests. Strict Clippy, WASM, adversarial commands, layers, parity, scorecard, formatting and all nine quick benchmarks passed. The complete native harness passed with no skips against the final retained pair, with 17 independently checked assertions and eight inspected screenshots. Four refreshed static layouts matched the inspected images, including after cache removal. Exact commands, limits and artifacts are recorded below.

Try the [retained app](../../log/k2/bin/photocraft.exe) and [local service](../../log/k2/bin/photocraft-sync.exe) using [the runbook](k2-private-rooms.md), or run `& ./scripts/komaphone/k2-native-smoke.ps1` for isolated automated acceptance. All agent build caches are removed; runnable artifacts, benchmark reports, screenshots, logs, corpus cache and the toolchain remain. Changes are uncommitted on `main`; no CI change, deployment, purchase or publication occurred.

This is a loopback development build. Full party-mode presentation/games are K3; separate-machine release verification, gallery/export and local remix are K4. K6 world remix, guided tours/export and since-you-were-away playback remain required. Baseline 1.1 is unchanged. **Next action in a later run: the K3 prompt in [agent prompts](agent-prompts.md#k3-animation-and-exquisite-corpse), after verifying the retained K2 prerequisite evidence.**
## Planning review 2026-11-04

- Files: added `docs/komaphone/{product-spec,architecture,delivery-plan,agent-prompts,status}.md`; updated `AGENTS.md`, `README.md`, `ROADMAP.md`, and `docs/roadmap.md`; appended local devlog.
- Independent reviews: repository reuse audit, technical architecture review, and final requirements/prompt consistency review. Resolved tunable-default precedence, new game command IDs, previously authorized artwork privacy, six-level zoom fixture consistency, deadline/local-revision ownership, and the early separate-machine party release gate.
- `git diff --check`: passed (only Git's LF-to-CRLF conversion notices).
- PowerShell local Markdown-link and trailing-whitespace audit across the nine changed/new documentation files: passed, 82 local links resolved, no trailing whitespace.
- Runtime build/tests: not run; this change is documentation only. K0-K7 remain unimplemented/unverified.
- No new dependencies, runtime source edits, CI changes, service deployment, hosting purchase, or background process. No agent build directory was created. Changes remain in the working tree for review; no commit was made.

## Approved additions review 2026-11-04

- Owner requested all three proposed ideas be included. Product baseline is now 1.1 with G2 remix, W7 guided tours/export, and W8 catch-up highlights/playback required.
- Updated product, delivery gates, architecture contracts, goal prompts, root AGENTS/README, and this status. Local remix belongs to K4; tours/catch-up/world remix belong to K6. The party-first delivery order is unchanged.
- Independent acceptance review covered source-copy independence/credits, animation preservation, stable camera targets and pinned export revisions, unseen-change watermarks, offline arrivals, and interruption recovery.
- Validation passed: `git diff --check`; PowerShell audit of nine documentation files and 82 local links with no trailing whitespace; `rg` audit found no stale optional-scope or current-baseline-1.0 references. Runtime tests were not run for this documentation-only update.
- Implementation remains not started. Next action remains K0; no service was deployed and no commit was made.

## Required entry for every implementation handoff

Append an entry with:

- Date, milestone, status, owner/coordinator, and current branch/commit if applicable.
- Changes and file ownership; link runnable artifacts and inspected screenshots.
- Exact validation commands and pass/fail/blocked outcomes, with the relevant output summary and hardware/workload for benchmarks.
- Acceptance requirements proved and still outstanding. Never infer a pass from a skipped check.
- Reversible assumptions/technical decisions, unresolved risks, and any necessary external user action.
- Next concrete command/action and any active processes/ports or temporary target directories that need cleanup.

Keep the ledger current. Do not replace history with aspirational completion claims or put credentials/private invitations into this file.

## K0 implementation 2026-11-05 — chronology (completed below)

Coordinator: root agent; branch `main`, base `69a9aacb566e036264a93ed6da925cf96eeda0bf`.
The initial checkout already contained uncommitted planning edits in AGENTS/README/roadmaps
and `docs/komaphone`; these were preserved. Planning is the only prior milestone and has
no runtime evidence to inherit. K0 is the next dependency-ready gate; K1 has not started.

Ownership and delivered code:

- Coordinator: `crates/party/{Cargo.toml,README.md,src/lib.rs,src/contracts.rs,src/adapter.rs,tests/adapter.rs}`,
  `Cargo.lock`, `xtask/src/layers.rs`, integration and this handoff.
- `room_model`: `crates/party/src/room.rs`, `crates/party/tests/room.rs`.
- `reuse_audit`: [reuse evidence/commands](k0-reuse-audit.md), `crates/party/tests/contracts.rs`, independent safety review.
- `service_decision`: [service-stack decision](service-stack.md), independent privacy/protocol review.
- New L2 `photocraft-party`: fixed typed IDs, versioned project/event/request/room records,
  asset/replay/image bounds, deterministic two/three-player schedules, immutable submissions,
  phase-filtered asset authorization, frozen timer manifests/grace, reconnect/moderator takeover,
  validated snapshots and bounded memory transport/storage with commit-failure injection.
  No engine command/UI/editor/format/CI change. The memory adapter is explicitly not a service
  or proof of disk durability. [Crate contract and K1/K2 ownership](../../crates/party/README.md).

Environment and early evidence (final results still pending):

- `Get-Command cargo,rustc -ErrorAction SilentlyContinue`: neither present. Checked ordinary
  user/Program Files paths; no existing Rust/MSVC installation found.
- Official `rustup-init.exe` downloaded from `https://win.rustup.rs/x86_64`; SHA-256
  `38027D51D69A5D05493627A0634DBEA52D6CD793C8F072D2373D3F4163EEC7DB`.
  Installed workspace-local Rust 1.95.0 (`59807616e`, 2026-04-14), Cargo 1.95.0
  (`f2d3ce0bd`, 2026-03-21), rustfmt/clippy/WASM std under `target/k0-tools`;
  no persistent PATH/default system toolchain changes.
- `cargo test -p photocraft-paint --lib` first failed restricted DNS. Retried with network
  approval: dependencies fetched, then failed `linker link.exe not found`. No drawing tests ran.
- Official Microsoft-signed Build Tools installer first returned 1602 (cancelled). User
  replied they would complete installation. Native MSVC gates remain pending until installed.
- Added workspace-local `1.95.0-x86_64-pc-windows-gnu` for independent headless checks;
  this does not replace the native MSVC baseline. All commands below set `CARGO_HOME` to
  `$PWD/target/k0-tools/cargo`, `RUSTUP_HOME` to `$PWD/target/k0-tools/rustup`, prepend
  `$env:CARGO_HOME/bin` to process PATH, and use `CARGO_TARGET_DIR=$PWD/target/agent-root-gnu`.
- First `cargo +1.95.0-x86_64-pc-windows-gnu test -p photocraft-party`: 40 passed; next
  integrated run: 42 passed. Additional review regressions are being added before final run.
- `cargo +1.95.0-x86_64-pc-windows-gnu xtask layers`: **passed**, 32 crates, no violations.
- `cargo +1.95.0-x86_64-pc-windows-gnu test -p xtask`: 76 passed, 1 failed due to generated
  scorecard's protected-crate count (new crate changes 30/30 to 31/31); regeneration pending.
- Party/xtask clippy found enum sizes, simple conditionals and a test macro's complexity;
  fixes integrated, final rerun pending. Incremental hard-link fallback on this checkout is
  avoided for final runs with `CARGO_INCREMENTAL=0` (no source-policy change).
- `cargo xtask wasm` with `RUSTUP_TOOLCHAIN=1.95.0-x86_64-pc-windows-gnu` is running.
  Logs: `log/k0/{party-tests-gnu,clippy-gnu,layers-gnu,wasm-gnu}.log` (local/ignored).

Independent review fixes: separated flattened image from editable checkpoints, disallowed
reference aliases, blocked known-hash laundering through another player's asset list,
made reconnect transactional, retained immutable retries after a reference grant expires,
allowed moderator takeover at reveal, bounded full snapshots separately from wire messages,
and rejected forged future accepted submissions on restore. Fixture renderer provenance is
explicitly trusted; actual pixel derivation and disk/server durability remain K1–K3 gates.

Next: finish current WASM check; regenerate scorecard and rerun touched tests/clippy; once
MSVC is available run editor reuse tests and inspect offscreen snapshots using the exact
commands in the reuse audit. Final gate/status will be appended below. No deployment,
purchase, public service, credential, commit, or new UI capability is claimed. No listening
service/port. Agents created no build directories; coordinator targets/tools remain in use.

### K0 contract validation checkpoint

- Final `cargo test -p photocraft-party -p xtask` on Rust 1.95 Windows GNU: **passed**,
  53 party tests (7 adapter, 31 contracts, 15 room) and 77 xtask tests, zero ignored.
  Exact log: `log/k0/final-tests-gnu-unrestricted.log`. A sandboxed run had four pre-existing
  corpus fixture rename failures (`Access is denied`, OS error 5 in sandbox temp); the
  unrestricted rerun passed without code changes. Earlier stale scorecard was regenerated;
  its only content changes are the two protected-root counts 30/30 -> 31/31.
- `cargo clippy -p photocraft-party -p xtask --all-targets -- -D warnings`: **passed**;
  `log/k0/final-clippy-gnu.log`. `cargo fmt -p photocraft-party -p xtask -- --check`: **passed**.
- `cargo xtask wasm`: **passed**, all 25 crates and optional HEIF. Existing ui-egui dead-code
  warnings at `text/cjk_fonts.rs::effective_locale` and `tools/gradient_ui.rs::editing` retained.
  Final `cargo check -p photocraft-party --target wasm32-unknown-unknown`: **passed** after
  the last retry fix. `cargo xtask scorecard --check`: **passed**. No parity floors changed.
- Subsequent GNU paint-baseline attempt is not a pass: Windows dependencies need `dlltool.exe`;
  placing Rust's bundled dlltool on process PATH then fails its assembler subprocess
  (`dlltool.exe: CreateProcess`). Continue with the intended MSVC installation rather than
  treating GNU contracts as native editor evidence. `log/k0/paint-baseline-gnu.log`.
- Native reference machine identified: AMD Ryzen 7 8700F (8 cores / 16 threads), NVIDIA
  GeForce RTX 5060 Ti; virtual monitor also present. No latency/performance claim made.
- All required headless K0 contract checks now pass. K0 remains **in progress**, because
  native editor primitive tests and inspected screenshots are outstanding while the user
  completes Microsoft Build Tools/C++/Windows SDK installation. Do not advance to K1 yet.

### Native baseline underway

The C++ linker and SDK became usable during installation. Process-local environment setup
is saved at `log/k0/env-msvc.ps1`: import the Visual Studio Developer Shell from
`C:/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools`, enter x64 host/target mode,
select workspace-local Rust 1.95.0 MSVC, `CARGO_TARGET_DIR=target/agent-root`,
`CARGO_INCREMENTAL=0`, and (after initial paint run) `CARGO_BUILD_JOBS=4`. This does not
change global PATH. Reference machine RAM is 33,995,153,408 bytes (about 31.7 GiB).

- `. ./log/k0/env-msvc.ps1; cargo test -p photocraft-paint --lib`: **passed**, 140 tests,
  zero ignored, 2.40 s test runtime after 42.97 s initial compilation.
  Artifact: `log/k0/paint-baseline-msvc.log`. These are test timings, not product latency claims.
- Native autosave/frame/brush/video baseline sequence is now running; results follow.

The Microsoft Build Tools installation completed (`vswhere ... -property isComplete` -> `1`):
Visual Studio Build Tools 2022 17.14.37827.9, MSVC tools 14.44.35207, Windows SDK 10.0.26100.0.
The following commands used `. ./log/k0/env-msvc.ps1` and ran outside the restricted temp
sandbox, preserving personal documents and using the existing synthetic test fixtures:

| Exact command | Result | Local artifact |
|---|---|---|
| `cargo test -p photocraft-format --test it autosave::` | 12 passed, 0 ignored; failed-write recovery/flush/recovery retention covered | `log/k0/autosave-baseline-msvc.log` |
| `cargo test -p photocraft-format --test it video_layer_frames_survive_roundtrip` | 1 passed, 0 ignored | `log/k0/frames-baseline-msvc.log` |
| `cargo test -p photocraft-engine --lib brush_cmds::tests::` | 62 passed, 1 ignored (`pencil_latency_24mp`, an existing opt-in benchmark); no K0 assertion depends on it | `log/k0/brush-baseline-msvc.log` |
| `cargo test -p photocraft-engine --lib video_cmds::tests::` | 43 passed, 0 ignored; decoded GIF frame order/timing/transparency, cancellation and frame edits | `log/k0/video-baseline-msvc.log` |

The initial native engine test build took 10m04s; selected brush/video tests took 0.44s/0.07s.
No upstream editor/format source changes were required. Actual egui mouse-path test and
offscreen captures are the remaining acceptance work, currently compiling in the sole
coordinator native target. Baseline screenshot fixture is `log/k0/editor-fixture.json` and
contains only generated strokes, no personal/third-party image.

## K0 completion 2026-11-05

**Gate passed. Stop here; K1 has not started.** The coordinator verified subagent work and
resolved every concrete review finding. No required K0 check remains blocked or skipped.
The optional existing paint benchmark is explicitly excluded from the claims above.

Final native checks, using `. ./log/k0/env-msvc.ps1`:

| Command | Result / evidence |
|---|---|
| `cargo test -p photocraft-party -p xtask` | **Pass:** 53 party + 77 xtask tests; zero ignored; `log/k0/final-tests-msvc.log` |
| `cargo clippy -p photocraft-party -p xtask --all-targets -- -D warnings` | **Pass:** `log/k0/final-clippy-msvc.log` |
| `cargo xtask layers` | **Pass:** 32 crates, no violations; `log/k0/layers-msvc.log` |
| `cargo xtask scorecard --check` | **Pass:** generated file current; only protected-root count changed |
| `cargo fmt -p photocraft-party -p xtask -- --check` | **Pass** |
| `cargo test -p photocraft-ui-egui --test it live_stroke_canvas:: -- --nocapture` | **Pass:** 4 tests, 0 ignored, no `skipping: no GPU adapter` output; 14.77 s; `log/k0/mouse-ui-msvc.log` |

The full `cargo xtask wasm` result (25 crates + HEIF) and final party-only WASM recheck are
recorded above. No parity-generation/panic_hunt gate applies because no engine command was
added/changed. No corpus/performance gate applies because no format/render/I/O/hot-path
implementation changed. Existing format/editor tests were run as K0 reuse evidence.

### Inspected visual artifacts

All three captures use the real editor offscreen, with no visible native window and no
personal artwork. Fixture: [k0-editor-fixture.json](k0-editor-fixture.json), identical to
the local `log/k0/editor-fixture.json` used in the executed command. Reproduce from repo root:

```powershell
. ./log/k0/env-msvc.ps1
$k0Fixture = Get-Content docs/komaphone/k0-editor-fixture.json -Raw
cargo run --profile test -p photocraft-ui-egui --example snapshot -- --out log/k0/editor-1280.png --size 1280x720 --scale 1 --custom-titlebar --script $k0Fixture
cargo run --profile test -p photocraft-ui-egui --example snapshot -- --out log/k0/editor-1920.png --size 1920x1080 --scale 1 --custom-titlebar --script $k0Fixture
cargo run --profile test -p photocraft-ui-egui --example snapshot -- --out log/k0/editor-hidpi.png --size 1280x720 --scale 2 --custom-titlebar --script $k0Fixture
```

`--profile test` reuses the UI test compilation; it does not change fixture content or
renderer selection. On another machine use the normal documented Rust/MSVC development
environment instead of the local environment helper, and create `log/k0` first.

| Screenshot | Inspected result | SHA-256 |
|---|---|---|
| [1280 × 720](../../log/k0/editor-1280.png) | All colored strokes visible, selected brush and layer panel readable; compact upstream tabs elide normally | `1F1E2A0DCC4C9C544616002D8A853D784B6BDB95B154727BA3DAE9771F707C49` |
| [1920 × 1080](../../log/k0/editor-1920.png) | Same artwork, intact canvas and panels, readable full-size controls | `161352EF43E0035F7DC08E84548544B5F9E2FC58E9109AF2F2E7C4D383FEE5FD` |
| [1280 × 720 at 2× DPI (2560 × 1440 physical)](../../log/k0/editor-hidpi.png) | Crisp scaled text/icons and strokes; canvas and layers intact | `E894B5F51FB905D8A136D2CE155D410A5D9710025CBB5B208800EFB7D0227DD3` |

The coordinator opened and inspected every PNG. Logs `log/k0/snapshot-{1280,1920,hidpi}.log`
contain no failed control commands. The offscreen harness selected **Microsoft Basic Render
Driver, DX12, CPU device type** (software/WARP), despite the NVIDIA adapter being enumerated.
The captures prove native egui/wgpu rendering and visual correctness, not hardware-GPU
performance. The mouse harness's synthetic timing figures are likewise not physical-input
p95 or the K1 ten-layer performance fixture. No performance target was weakened.

### Result, limitations and next action

- All K0 acceptance items have evidence: actual toolchain/baseline; reused drawing,
  autosave, frame/GIF primitives; malformed/oversized contract rejection; legal/illegal
  transitions; full 2-/3-player schedules (all 1–4 animation rotations and corpse); scoped
  payloads/fetches; in-memory loss/duplicate/reorder/restart/failure behavior; maintained
  service-stack decision; and documented K1/K2 ownership/interfaces.
- K0 changed the nine `crates/party` files, `Cargo.lock`, `xtask/src/layers.rs`, generated
  `docs/scorecard.md`, `docs/komaphone/{architecture,status,k0-reuse-audit,service-stack}.md`,
  and the tracked JSON screenshot fixture. Existing planning changes remain uncommitted.
  Upstream attribution/licenses and editor implementation were preserved. No CI, packaging,
  hosting/deployment, provider purchase, or public service change. Product baseline stays 1.1.
- The memory adapter is a test harness. Real authentication, durable SQLite transactions,
  image/seam verification, waiting-room/next-game lifecycle and bounded network pagination
  remain K2/K3. Permanent local recovery and actual replay recording start in K1. Local
  remix stays K4; deep-world remix/tours/catch-up stay K6. No later milestone is claimed done.
- Follow the K1 prompt next, beginning with the shared contracts and reuse audit. Keep the
  current single coordinator for shared schemas/registrations; split curated workspace
  from native persistence/replay implementation. Test kill-after-ack/during-write and real
  disk failure there; K0 autosave tests do not replace those required K1 behaviors.
- All run-owned build/test processes finished; no service/port was opened. Removed the
  exact verified workspace paths `target/agent-root-gnu` and `target/agent-root` per the
  build-directory policy. Kept `target/k0-tools` (workspace-local Rust/Cargo tools/cache),
  local logs/PNGs, and `log/k0/env-msvc.ps1` for reproduction. The independently created
  `target/buildtools-verification` directory was left untouched. Re-running Cargo rebuilds
  its target cache. No commit was made.
- Final hygiene: `git diff --check` **passed** (only existing LF-to-CRLF notices);
  PowerShell local-link audit **passed**, 36 links across Komaphone Markdown and the crate
  README, including all three PNG artifacts. No unresolved review finding or external K0 gate.

## Build Tools installation verification 2026-11-05

- Scope: owner-requested Windows toolchain unblock only; no product source changes or milestone completion claim.
- Installed Microsoft-signed Visual Studio Build Tools 2022 17.14.44 with `--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended`. Installer exited 0. `vswhere -all -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -format json` reports complete, launchable, and no reboot required.
- Installation: `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools`; MSVC directory `VC\Tools\MSVC\14.44.35207`; Windows SDK libraries `C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0`.
- Runnable local evidence: `target/buildtools-verification/verify-cpp.cmd` initializes x64 `VsDevCmd.bat`, runs `cl /nologo /EHsc smoke.cpp /Fe:cpp-smoke.exe`, then executes it. PASS: Windows header/API compile, link, and execution (`C++ and Windows SDK OK`).
- With `CARGO_HOME=target/k0-tools/cargo` and `RUSTUP_HOME=target/k0-tools/rustup` resolved to absolute repository paths, local `rustc smoke.rs -o rust-smoke.exe` and execution PASS (`Rust MSVC compile and link OK`) from `target/buildtools-verification`, without initializing the developer shell in the Rust process.
- Original cancellation log reported a declined/timed-out UAC prompt. First retry encountered another installer window; after it closed, the next installation succeeded. No restart performed, installer no longer running.
- Next action: resume mandatory project Windows tests in the implementation chat. These smoke checks establish toolchain availability only; project acceptance tests were not run here. Verification artifacts remain under gitignored `target/buildtools-verification`.

## K1 implementation 2026-11-05 — in progress

Coordinator root; checkout `main` at `69a9aacb566e036264a93ed6da925cf96eeda0bf` with all existing K0/planning changes preserved. Selected K1 as next dependency-ready gate. K2 has not started. Product baseline remains 1.1.

Ownership: `workspace` owns new curated UI module/test; `local_store` owns new native storage implementation and crash tests; `replay_capture` owns new recorder module/tests and independent integration review. Coordinator owns all manifests, shared types (`local-store/src/types.rs`), engine registration/hooks, serialized UI state, native host/worker integration, final checks and handoff.

Prerequisite command `. ./log/k0/env-msvc.ps1; cargo test -p photocraft-party` passed: 53 tests, zero ignored. Log `log/k1/k0-prerequisite.log`. K0's inspected screenshots/native checks remain recorded above. Native MSVC helper/tool cache retained from K0; coordinator target `target/agent-root`, storage focused build target `target/agent-local-store`; no competing whole-app builds.

Current implementation (not yet accepted): permanent SQLite drafts using selected DELETE/EXTRA policy, chunk-deduplicated native checkpoints plus replay records in one transaction, background worker and exact acknowledgment state, project new/list/open/retry/status commands, baseline/resolved stroke/checkpoint/undo/redo recording, compact mouse-first workspace and Advanced return. Real data defaults to `%LOCALAPPDATA%/Komaphone/drafts.sqlite3`; `KOMAPHONE_DATA_DIR` isolates test roots. No network service/deployment/expense.

Dependency fetch first failed sandbox DNS (`getaddrinfo() thread failed to start`); retry using approved network execution fetched rusqlite 0.41.1 and bundled SQLite dependencies. Logs `log/k1/fetch{,-unrestricted}.log`. Initial integration `cargo check -p photocraft-engine -p photocraft-ui-egui -p photocraft --offline` is running, output `log/k1/integration-check.log`. No compile/pass claim yet.

Next: fix integration errors, run focused mouse/replay/durability/crash tests; add desktop acknowledgment/reopen and 10-layer latency evidence; inspect all required screenshots. Then run full touched-crate tests/clippy, layers/WASM/parity/panic_hunt/perf/scorecard as applicable. This entry is a recovery checkpoint, not a completed gate.

### K1 integration checkpoint (still in progress)

- Native sandbox build hit `ring` librarian `LNK1114` / OS error5 replacing its generated archive. Retried with approved unrestricted native execution. `cargo check -p photocraft-engine -p photocraft-ui-egui -p photocraft --offline` reached engine successfully, then found one UI ProjectId Display mismatch; changed to the existing `as_str()` API. Logs `log/k1/integration-check{,-unrestricted}.log`.
- Storage focused validation was inspected by coordinator: `cargo test -p photocraft-local-store --offline` passed14 entries (11 storage cases, two real process-kill acceptance cases and one child harness entry), zero ignored. `cargo clippy -p photocraft-local-store --all-targets --offline -- -D warnings` passed. Logs `log/k1/local-store-{tests,clippy}.log`. Tests include all depths, future/corrupt schemas, exact retries, SQLite FULL rollback, truncated database, checksummed/envelope associations, deduplication and replay gaps. During-write termination uses >2MiB unique data with journal and spilled uncommitted DB pages. Agent target removed after tests. Subsequent small encoder-error classification change awaits final retest.
- `. ./log/k0/env-msvc.ps1; cargo test -p photocraft-engine --test local_replay --test local_projects --offline -- --nocapture`: first run app/worker7 tests passed (one child harness entry included); recorder11 passed/1 failed because candidate-size guard was not yet wired. Wired guard before document swap; final rerun pending. Log `log/k1/engine-local-tests.log`. App worker test really kills its subprocess immediately after the same ACK consumed by UI, then loads retained layers/pixels/replay. This is separate from storage-only crash evidence.
- Initial persistence workload: test profile, same K0 Windows hardware,1536x1024,10layers,20 ordinary24px strokes: edit+durable ACK p50=14.00ms,p95=18.54ms,max=18.80ms, under1s. This is local disk/edit latency, not physical mouse-to-monitor latency. Final integrated rerun still required.
- Review fixes integrated: byte-bounded Unicode titles; bounded worker response queue; exact retained failures and per-project failure isolation; no nested command can drain tentative replay during rollback; distinct IDs for reopened legacy copies; duplicate-open coalescing; recovered-source copies get independent baseline; full sampled-brush assets preflight before serialization; metadata included in queue bounds; nonblocking close wait/error/explicit-discard behavior.
- Routine limit decision preserves Advanced editor: first-open documents outside finite local draft limits remain external documents with explicit File Save guidance; existing editor capability is retained. Managed local drawings reject excessive growth before mutation. Ordinary supported imports receive a baseline. Native encoded checkpoints are bounded64MiB, metadata/replay8MiB, queued snapshots512MiB/64events, retained failed drafts16/512MiB (a completed oversized boundary is retained and further growth stops). No original or history is pruned. Baseline1.1 unchanged.
- Ownership extended: workspace agent owns `dialogs/discard_ui.rs` and new native close tests; storage agent owns `engine/src/local_projects/native.rs` and `tests/local_project_failures.rs`; coordinator retains shared state/schema/registration. All agents now completing review/freeze, no parallel whole-app builds.
- Currently running `. ./log/k0/env-msvc.ps1; cargo test -p photocraft-ui-egui --test it komaphone_workspace:: --offline -- --nocapture` in `target/agent-root`, log `log/k1/workspace-tests.log`. New10-layer pointer-to-offscreen-render latency test and tracked `k1-workspace-fixture.json` added; screenshot example can use `--komaphone-data-dir` for real ACK state. No screenshots captured/inspected yet. Final mandatory gates and native app build remain pending; do not mark K1 complete.

### K1 validation checkpoint — 2026-11-05, 10:41 Pacific

- Initial full native test command: `cargo test -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft -p xtask --offline` (`log/k1/full-tests.log`). Built in 14m09s; app unit 215 passed/3 preexisting ignored, app integration14 passed, engine unit2876 passed/16 preexisting ignored, engine main integration434 passed/5 preexisting ignored and additional integration binaries passed. Stopped at new nested-rollback fixture failure: filenames intentionally survive history, so changed the fixture to undoable layer opacity. This run is not the final gate.
- Independent review found and fixed floating-selection preview loss on a refused commit: `cmd/float_cmds.rs` now retains the piece until edit success. New replay regression covers explicit/implicit drop, queue exhaustion and pixel-preserving retry. Added durable sampled-brush/settings replay round-trip at 8/16/32-bit depth. A test-only `Buffer.px` move error was fixed by retaining cloned vectors; rerun pending in `log/k1/engine-local-final2.log`.
- Diagnostic old-built UI tests:5 passed/3 failed (`log/k1/workspace-diagnostic.log`); corrections underway for pixel rectangle coordinates, raw-input modifier state and the narrow tool panel's bottom controls. Render latency diagnostic initially started outside the visible canvas; corrected to visible coordinates and explicit brush settings, with cursor-free pixel sampling. These failures are retained as diagnostics, not acceptance passes.
- Strict clippy exposed existing current-toolchain lints in PSD, I/O, engine and UI. Small behavior-preserving corrections are being included so the original strict gate can pass. No parity feature is added. Because PSD/I/O are now touched, the corpus gate is required. Pinned corpus download/checksum verification passed: `target/agent-root/debug/xtask.exe corpus --all`, `log/k1/corpus-fetch.log`; no corpus files are tracked.
- `cargo xtask layers`: **passed**,33 crates/no violations (`log/k1/layers.log`). `cargo xtask wasm`: **passed**,26 portable crates plus codecs/HEIF (`log/k1/wasm.log`); existing target-specific dead-code warnings recorded. Final rerun needed after UI freeze.
- Next: freeze remaining bounded UI fixes; run all new tests and full touched-crate tests with `--no-fail-fast`, strict clippy, corpus, panic_hunt, parity/scorecard, performance and three inspected workspace screenshots. K1 remains in progress; no service or hosting work started.

### K1 review checkpoint — 2026-11-05, 11:22 Pacific

- Second integrated run `cargo test -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft-psd -p photocraft-io -p photocraft -p xtask --no-fail-fast --offline` is still running (`log/k1/full-tests-final2.log`). UI unit tests now pass: 4,884 passed, 21 preexisting ignored. App, engine, store, PSD and I/O tests passed in this run; UI integration rendering still running. This is not the final all-gates claim.
- Fixed first-frame system-font provider admission without requiring optional embedded fonts; strict multilingual tests now pass with installed Windows fonts. No font files added and strict missing-glyph assertions retained. Restored the upstream Advanced tool inventory as a collapsed README list so the existing inventory regression remains meaningful. No removed brand asset restored.
- Independent final reviews found three further defects, now assigned to their file owners: compressible metadata could pass encoded checkpoint limits but fail bounded reopen; failed compound replay rollback compared managed tabs with excluded external tabs; close-dialog automation reported prior saved state during an unfinished preview. Regression fixes and retests are required before K1 can close.
- Strict clippy had passed before these final review fixes (`log/k1/clippy-strict-final3.log`); it must be rerun. Pinned corpus fetch is verified, but release corpus tests and performance run still remain. Final screenshots/native smoke and other mandatory gates are also pending. No K2 work or external deployment began.

### K1 final-source checkpoint — 2026-11-05, 11:45 Pacific

- Full native suite previously passed in `log/k1/full-tests-final2.log` (UI4,884 unit/199 integration). Subsequent independent review fixes are now frozen: pre-ACK bounded native decode, failed-compound rollback with excluded external tabs, complete conservative metadata accounting, preview-aware close inspection, and native upstream-update route removal. Added real engine corrupt-tail recovery test. Storage final focused run passed15 entries (`log/k1/local-store-final.log`).
- `full-tests-final3.log` stopped on one new regression's nonexistent `LoadedDraft.replay` field; fixed it to call `Store::replay_page`. Current single coordinator command `& ./log/k1/final-native-gates.ps1` is running: final full suite (`full-tests-final4.log`), strict clippy, ignored panic_hunt, WASM, parity and layers. Final engine K1 tests all passed: failures3, worker8, recovery1, recorder20, sampled-assets1. UI4,884 unit tests passed; integration still running. No K1 acceptance tests ignored.
- Inspected final layout artifacts: `log/k1/workspace-1280.png` (1280x720), `workspace-1920.png` (1920x1080), and `workspace-hidpi.png` (logical1280x720 at2x, physical2560x1440). Synthetic fixture `docs/komaphone/k1-workspace-fixture.json`, commands in snapshot logs. All14 tools, Fit/100%, RGB fields, layers and local/sync footer fit. Offscreen renderer uses WARP; its missing native file-dialog service disables Import in these screenshots. Real mouse PNG import passed and native Import is enabled.
- Actual rebuilt native app smoke passed: `& ./log/k1/native-smoke.ps1`, log `native-smoke-final.log`, data/result/screenshot `log/k1/native-smoke-d06e7e7fa6b64bf2895b96cc7827314b/`. Force-killed test process after revision5 ACK, restarted, reopened identical project with two layers and exact blue pixels after undo. Inspected `native-reopened.png`; no updater popup. Native GPU: RTX5060Ti, Vulkan, NVIDIA610.88, AutoVsync/latency1. Test ports/processes closed. Only isolated data/config roots used.
- First native visual smoke exposed an inherited PhotoCraft update offer; disabled the native update transport and upstream self-update finish hook. Independent Komaphone release channel remains future work; engine/UI generic machinery retained. App regression asserts supported:false and manual check refusal. No service was bought/published or source attribution removed.
- Remaining: finish current native gates; obtain final uncontended timing output; run `& ./log/k1/release-gates.ps1` (full release corpus, quick performance,24MP Advanced benchmark), inspect results and fix failures. Then final status/devlog, diff/link checks, cleanup and milestone gate. K1 remains in progress. Runnable development executable is temporarily retained in `log/k1/bin/photocraft.exe`; this is not an installer/release.

## K1 native acceptance checkpoint — 2026-11-05, before final release validation

This historical checkpoint is superseded by the completion entry below; its earlier results remain recorded for traceability.

### Delivered implementation and files

- Coordinator owns integration and shared schema/registration. New L4 `crates/local-store/{Cargo.toml,README.md,src/lib.rs,src/types.rs,src/native.rs,tests/store.rs,tests/process_crash.rs}` provides permanent, versioned SQLite drafts with chunk-deduplicated native checkpoints and replay records in one verified DELETE/EXTRA transaction. Receipt, project head, device sequence and data commit together. Every generated checkpoint passes the actual bounded reopen loader before ACK. No pruning or destructive schema migration.
- Engine integration: `crates/engine/{Cargo.toml,src/lib.rs,src/cmd.rs,src/jobs.rs,src/session/documents.rs,src/session/execute.rs,src/session/history.rs}`, new `src/{local_projects.rs,local_projects/native.rs,local_replay.rs,cmd/komaphone_project_cmds.rs}`, and `tests/{local_projects,local_project_failures,local_recovery,local_replay,local_replay_assets}.rs`. Stable commands are `komaphone.project.{new,list,open,retry,status}`. Existing editor IDs are reused. Worker queues are bounded and failures retain exact requests, ordering and unsaved snapshots; unrelated drafts can continue. Replay records baseline, resolved brush/assets/settings, undo/redo and explicit checkpoint fallbacks before Actions rewriting/coalescing.
- UI: `crates/ui-egui/src/{komaphone_workspace.rs,app.rs,app/frame.rs,canvas.rs,lib.rs,state.rs,panels/title_bar.rs,control/dispatch.rs,control/inspect.rs,control/params.rs,dialogs/discard_ui.rs,dialogs/discard_ui/local_tests.rs}`, `examples/snapshot.rs` and `tests/it/{main,komaphone_workspace,komaphone_latency}.rs`. Compact drawing workspace, Advanced return, draft list/reopen, per-document saved/pending/error state and safe close flow. Snapshot helper can use an isolated real local database; no fake save label.
- Native host: `apps/photocraft/src/{main.rs,services.rs,services/tests.rs,updates.rs}`. Komaphone title/default workspace, `%LOCALAPPDATA%/Komaphone/drafts.sqlite3`, bounded exit flush, isolated override support. The inherited upstream updater is unavailable until a separate Komaphone release channel is reviewed; it cannot offer to replace this fork. Generic engine/UI updater support remains.
- Correctness fixes required by validation: `engine/src/cmd/float_cmds.rs` retains a floating selection until its commit succeeds; `ui-egui/src/text/cjk_fonts.rs` and its `tests.rs` admit installed system fonts during the first frame even without optional bundled fonts. Strict glyph tests remain strict; no font files added. Conservative retained-memory accounting includes metadata, embedded assets and dynamic styles; arbitrary JSON traversal is depth bounded.
- Minimal current-toolchain lint fixes: `psd/src/codepage.rs`, `io/src/{psd_export/document.rs,smart_map/import.rs}`, `engine/src/cmd/atn_cmds/steps_layer_style.rs`, `ui-egui/src/{canvas/strokes.rs,canvas/tool_cursor/pen.rs,menus/menu_keys.rs,menus/menu_nav.rs}`. Their original semantics are preserved and the mandatory corpus gate is being run because PSD/I/O were touched.
- Shared/support files: `Cargo.lock`, `xtask/src/layers.rs`, `README.md`, `ATTRIBUTION.md`, `docs/control-protocol.md`, generated `docs/scorecard.md`, `docs/komaphone/{status.md,k1-local-drafts.md,k1-workspace-fixture.json}`. Parity generation produced no semantic checklist change. The README retains the Advanced tool inventory; synthetic K0/K1 fixtures have an attribution row. Initial K0/planning changes, upstream notices/licenses/contributor records and product baseline 1.1 are preserved. No CI, deployment, purchase, or commit.

Implementation ownership: `workspace` handled curated UI/mouse and close tests, then native updater policy; `local_store` handled store/worker/failure/recovery/assets tests; `replay_capture` handled recorder, rollback/budget tests, floating-selection retention and font fallback fix. Coordinator integrated shared files and ran all final checks. A separate cross-module review confirmed the real corruption fixture and pre-ACK bounded-loader guard. Finished agent build directories were removed; only coordinator `target/agent-root` is active for remaining checks.

### Observable acceptance

| Requirement | Executed evidence |
|---|---|
| Mouse drawing tools and retained Advanced editor | Eleven `komaphone_workspace` tests use actual egui pointer/text input: brush/pencil/eraser/fill, selections/lasso, move, scale/rotate, text, rectangle/ellipse/line, PNG import, layer edits and undo/redo, pan/wheel/100%, Advanced round-trip. Existing UI/parity tests stay green. Fit is visible in the inspected screenshots, but this test suite does not click it. |
| Durable restart, layers, undo-final pixels | Engine worker test force-kills its subprocess immediately after the UI-visible ACK, reopens the same project and checks layers/pixels/replay. Actual native executable separately force-killed after revision 5, then reopened two layers and identical blue pixels following undo. |
| Kill during a real write | Store subprocess killed at precommit after a real rollback journal and >2 MiB of spilled uncommitted database pages exist. Previous acknowledged revision, layers, replay and next device sequence survive. No mocked storage is used. |
| Failed writes cannot claim success | Real SQLITE_FULL/quota rollback, conflicting writer, encoding/size failures, malformed and future schema cases; exact retained retry and independent-project progress. Oversized compressible EXIF/manifest cannot receive ACK and leave prior database bytes unchanged. |
| Corrupt-head recovery | Store detects checksummed payload corruption; real Session opens the prior checkpoint, warns, durably saves an independently identified recovery baseline, preserves the original damaged head, permits edits and restores the copy after another restart. |
| Replay from first edit | Twenty recorder tests plus actual SQLite sampled-asset/settings replay at 8/16/32-bit depth. Canonical document/surface/CPU pixels match; undo/redo remain distinct; unsupported steps have checkpoints; failed rollback leaves no invented history, including with excluded external tabs. |
| Honest save/close state | Eight native close tests, workspace per-document state and gradient-preview tests. A previous ACK cannot certify an unfinished stroke/type/transform/gradient; retry, wait, cancel and explicit close-without-waiting are distinct. External Advanced files retain normal Save behavior. |
| Required visual sizes | Inspected [1280x720](../../log/k1/workspace-1280.png), [1920x1080](../../log/k1/workspace-1920.png), and [2x high DPI](../../log/k1/workspace-hidpi.png). Final rebuild regenerated byte-identical PNGs (`log/k1/snapshot-hashes.json`). All controls fit. Offscreen missing-native-file-dialog limitation is explicit; actual native Import is enabled and PNG mouse import is tested. |

### Exact verified native checks

All Cargo commands below start with `. ./log/k0/env-msvc.ps1`: Rust/Cargo 1.95.0 MSVC, `CARGO_TARGET_DIR=target/agent-root`, `CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=4`. Native compiler commands used approved unrestricted execution after the earlier sandbox archive-replacement failure. Existing unused-Cargo-patch notices and target-specific WASM dead-code warnings are recorded; no strict native lint warnings remain.

| Command | Result / local log |
|---|---|
| `cargo test -p photocraft-party` | K0 prerequisite: 53 passed, zero ignored; `k0-prerequisite.log`. |
| `cargo test -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft-psd -p photocraft-io -p photocraft -p xtask --no-fail-fast --offline` | **10,565 passed, zero failed**, 59 preexisting platform/manual/opt-in ignores; `full-tests-final4.log`. No K1 test ignored. Includes all doctests. |
| `cargo clippy -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft-psd -p photocraft-io -p photocraft -p xtask --all-targets --offline -- -D warnings` | **Passed**; `clippy-final.log`. No `--no-deps` relaxation. |
| `cargo test -p photocraft-engine --test panic_hunt --offline -- --ignored` | **Passed**, adversarial test ran, zero ignored; `panic-hunt.log`. |
| `cargo xtask wasm` | **Passed**, 26 portable crates and codecs/HEIF; `wasm-final.log`. |
| `cargo xtask parity` | **Passed**, existing 660/660 menu wiring unchanged; `parity.log`. This is wiring coverage, not a Photoshop behavior claim. |
| `cargo xtask layers` | **Passed**, 33 crates, no violations; `layers-final.log`. |
| `cargo xtask scorecard` | **Passed**; `scorecard.log`; protected crate roots now 32/32, including prior K0 and new K1 crates. Final xtask staleness/determinism test passed. Existing dead preferences remain 10/187. |
| `cargo fmt -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft-psd -p photocraft-io -p photocraft -p xtask -- --check` | **Passed**; `fmt-final.log`. |

Full-suite detail: app 215 unit +14 integration passed; engine 2,876 unit plus all integrations, including 33 K1 entries; UI 4,884 unit +199 integration passed; local store 12 storage +two real crash cases +child harness; PSD 320 unit +210 integration; I/O 572 unit +551 integration; xtask 77 passed. Intermediate failures remain in older logs as diagnostics; `full-tests-final4.log` is the accepted final native run.

### Native artifact and timing

- The pre-SVG development executable had SHA-256 `4C39EE336B3193DC550C5720A19C1AD080C8AC8B5A926EDC2460D11C8A5FE708`; it has been replaced by the final artifact listed in the completion entry. This remains a development build, not a signed installer or friend-ready party release. [Run guide](k1-local-drafts.md) has fully isolated trial commands and source-build instructions.
- `& ./log/k1/native-smoke.ps1` tested that exact retained executable, using unique draft/config roots and authenticated loopback control, then killed only its own test processes. [Result](../../log/k1/native-smoke-52f56d38f7b5420eb87f3735e66e655a/result.json), [native screenshot](../../log/k1/native-smoke-52f56d38f7b5420eb87f3735e66e655a/native-reopened.png), log `native-smoke-artifact.log`. Final artifact screenshot is byte-identical to the inspected rebuilt-app screenshot. No test port/app remains open.
- Screenshot reproduction: `$k1Fixture = Get-Content docs/komaphone/k1-workspace-fixture.json -Raw`; run `target/agent-root/debug/examples/snapshot.exe --out log/k1/workspace-1280.png --size 1280x720 --scale 1 --custom-titlebar --komaphone-data-dir log/k1/snapshot-final-1280 --script $k1Fixture`. Use `1920x1080 --scale 1` and `1280x720 --scale 2` with separate output/data paths for the other cases. Logs `snapshot-final-{1280,1920,hidpi}.log`. Rebuild the snapshot example after Cargo-cache cleanup.
- Reference machine: Windows, Ryzen 7 8700F (8 cores/16 threads), 31.7 GiB RAM, RTX 5060 Ti 16 GB, NVIDIA 610.88/Vulkan. Offscreen layout screenshots deliberately use kittest's WARP default; the latency fixture explicitly uses the native hardware-adapter policy.
- With no build running, `target/agent-root/debug/deps/local_projects-1a607b59a75d073d.exe ten_layer_ordinary_strokes_are_durable_within_one_second --exact --nocapture`: 1536x1024, ten ordinary layers, twenty 24px strokes, test profile: edit+durable ACK **p50 14.12 ms / p95 15.04 ms / max 18.59 ms**, under the 1 second target. Log `persistence-latency-final.log`. Initial integration measurement before final review guards was 14.00/18.54/18.80 ms; these single-run observations do not establish a statistically significant speedup.
- `target/agent-root/debug/deps/it-7f5114e654f59d7e.exe komaphone_latency::ten_layers_pointer_to_rendered_stroke_latency --exact --nocapture`: same canvas/layers, 1280x720, twenty real egui pointer moves while held: input-to-offscreen-readback **p50 41.53 ms / p95 43.13 ms / max 43.36 ms**, under 50 ms. Every sample appeared on the second frame; timing includes GPU readback and an explicit 16.667 ms frame interval. Log `visible-latency-final.log`. This measures the software input/render path, not physical mouse hardware, OS input queues or monitor scanout.

### Limits and remaining gate

Local-only K1: no room/service, multiplayer, submission, game UI, polished replay viewer/gallery, remix or shared world is claimed. K4 local remix and K6 world remix/tours/catch-up remain required. Reopening restores final editable pixels/layers and durable replay, not the previous interactive undo stack. Power loss, drive failure and verified portable backup/restore remain later hardening/archive work; process-kill and actual SQLite failure recovery are proven here.

Operational bounds and errors are documented in the [local-draft guide](k1-local-drafts.md) and [store contract](../../crates/local-store/README.md). No work is pruned to satisfy quotas. Oversized legacy documents remain usable in Advanced with explicit File Save guidance. Managed documents reject excessive growth before mutation. Shared references are conservatively overcounted in queue accounting; exact memory usage may be smaller.

Still pending: default release `cargo xtask test-corpus`, `cargo xtask perf --quick`, and the 24 MP release benchmark in `log/k1/release-gates.ps1`; final diff/link audit and coordinator cache cleanup. These checks are running and must not be labeled passed from this native evidence section.

### Release validation build tuning — 2026-11-05

The first default release corpus build was stopped while compiling, with no test result claimed (`log/k1/corpus-tests-jobs4.log`). Four Rust compiler workers each used about0.3–1.1GiB; measured free RAM was15.58GiB on the31.66GiB machine. Restarted the exact same `cargo xtask test-corpus` and following performance commands with process-local `CARGO_BUILD_JOBS=8`, retaining the repository's release optimization/LTO settings and all tests. Ten completed test executables/cache entries are reused. Verified no old Cargo/rustc/link/xtask processes remained before restart; only one coordinator build runs. `log/k1/release-gates.ps1` records this setting. This tunes compilation concurrency only, not product defaults or acceptance thresholds.

### SVG release crash fix — 2026-11-05

The required optimized corpus run exposed a retained I/O stack overflow after all release targets compiled. `target/agent-root/release/deps/photocraft_io-11a49d44c25f9d28.exe --test-threads 1 --nocapture` isolated `svg::tests::bounded_embedded_svg_gets_only_the_nesting_depth_the_drawing_left` on its original 1 MiB test thread (`log/k1/io-lib-release-serial.log`, exit `STATUS_STACK_OVERFLOW`). The pre-fix corpus output is retained as `log/k1/corpus-tests-pre-svg-fix.log`.

The workspace agent owns the fix in `crates/io/src/svg.rs` and `crates/io/src/svg/tests.rs`; the replay agent reviewed it read-only, finding no correctness blocker. Full native SVG operations now run on a scoped 8 MiB worker: parsing, import conversion, rendering and recursive parser-tree destruction all stay inside. Nested operations reuse the worker. Spawn/join failure returns `IoError::Svg`; tests prove nested errors and an escaped dependency panic do not unwind the caller. Native call overhead is one OS thread setup/join per top-level operation, including bounds-only calls. There is no new dependency or unsafe code. Existing depth512, byte/image/work/cache bounds, accepted inputs and small-stack test assertions are unchanged; WASM remains direct.

Executed after `. ./log/k0/env-msvc.ps1` and process-local `CARGO_BUILD_JOBS=8`:

`cargo test --release -p photocraft-io --lib --features photocraft-io/corpus,photocraft-io/heif --offline svg::tests:: -- --test-threads 1`

Result: **40 passed, zero failed/ignored**, including the original crashing case and deepest embedded-SVG chain; optimized build 3m41s, tests2.96s (`log/k1/svg-release-fix.log`). Full corpus/performance and post-fix native/WASM/artifact refresh remain required. The product baseline and K1 scope are unchanged.

The rebuilt full-corpus I/O integration executable was also exercised independently while engine compilation continued: `target/agent-root/release/deps/it-4f24be9f03cd311f.exe 'svg::' --test-threads 1` — **20 passed, zero failed/ignored**, including ordinary small-stack imports and accepted near-limit nested images (`log/k1/svg-release-integration-fix.log`). Its elapsed time is not a performance result because the build was active.

Release I/O preflight completed on the fresh full-corpus binaries: `photocraft_io-11a49d44c25f9d28.exe --test-threads 1` —575 passed/0 failed/2 preexisting manual ignores; `it-4f24be9f03cd311f.exe --test-threads 1` —596 passed/0 failed/9 preexisting ignores. Logs `io-release-preflight-unit.log` and `io-release-preflight-integration.log`. These include the pinned real-file oracle and mutation tests. The standard whole corpus gate still runs afterward with its normal settings; this preflight is not a substitute or a performance measurement.

### Standard release corpus gate passed — 2026-11-05

`cargo xtask test-corpus` completed after the SVG fix with the repository's normal release optimization/LTO and default test-thread settings: **6,232 passed, zero failed, 40 preexisting opt-in/manual/platform ignores, 42 test groups**. No K1 test was ignored. Pinned corpus inputs were fetched and verified by the runner. Exact output: `log/k1/corpus-tests.log`; aggregate result: `log/k1/corpus-counts.json`. This supersedes the pre-fix failed corpus run, not its diagnostic record. The quick performance and 24 MP release runs remain in progress/pending; post-fix native, WASM, executable, screenshot and latency refresh remain required before K1 closes.

### Quick performance run — 2026-11-05

`cargo xtask perf --quick` returned success after executing all nine planned benchmark programs; all report `status: ok`, with no failed benchmark. Hardware class is Windows x86_64 / Ryzen 7 8700F / NVIDIA GeForce RTX 5060 Ti. Exact console output is `log/k1/perf-quick.log`; JSON and Markdown results currently reside in `target/agent-root/perf/` and will be retained under `log/k1/perf-quick-results/` before cache cleanup.

Limits: quick mode uses smaller workloads and does not apply full-size budgets. Automatic regression comparison was **not performed** because the versioned baseline is for `macos-aarch64-apple-m4-pro-apple-m4-pro`; there is no matching Windows baseline. This is a successful required command/run, not proof of zero performance regressions or that every upstream target is met. For example, layout text-move release reports about131ms median, above its printed16ms target; existing unmeasurable upstream scenarios remain listed. No budget, baseline, or scope was weakened or rewritten. K1's separate ten-layer drawing/durability targets are directly measured, and the full24MP release benchmark remains pending.

### 24 MP release measurement — 2026-11-05

`cargo run --release -p photocraft-ui-egui --example interactive_bench --offline -- --size 6000x4000 --reps 3 --json log/k1/interactive-24mp.json` completed successfully. The report confirms6000×4000 pixels, GPU rendering enabled, 32 measured operations, three warm samples per operation plus a separate cold sample. Maximum reported process peakRSS was1.848GiB. Full output and data: `log/k1/interactive-24mp.log` and `interactive-24mp.json`.

| Operation on the synthetic 24 MP document | Median / maximum warm time |
|---|---|
| 60px brush dab, command + refresh | 0.494 / 0.938ms |
| 40-dab 120px brush stroke | 8.550 / 10.101ms |
| Gaussian Blur radius40 + refresh | 249.492 / 250.891ms |
| Full `.pcraft` encoding | 175.292 / 176.445ms |
| PSD save | 295.726 / 296.784ms |
| PSD reopen + refresh, three layers | 94.802 / 95.066ms |
| CPU flatten, three layers | 79.046 / 83.204ms |
| PNG export | 353.541 / 358.670ms |

This is an Advanced-editor heavy-operation measurement, not the K1 managed-draft workload or a physical mouse-to-display test. There is no comparable pre-K1 24MP run on this machine, so before/after regression comparison for these rows is unavailable and is not claimed as passed. The earlier/final K1 ten-layer measurements are comparable observations of their own workloads only. No performance baseline or threshold was changed. Independent gate review confirmed that quick-run execution, full-size measurements and direct K1 target checks must be reported separately. Post-SVG native/WASM/artifact/timing refresh is now running in `log/k1/finish-post-svg.ps1`.

### Comparable 24 MP baseline preparation — 2026-11-05

To supply the requested before/after heavy-operation evidence, archived the original checkout commit `69a9aacb566e036264a93ed6da925cf96eeda0bf` into isolated `log/k1/baseline-69a9aac/`, without changing the working product. The original and current `crates/ui-egui/examples/interactive_bench.rs` are byte-identical: SHA256 `96A577952E7CE2DE401E622D553BD046E79031C19C7D12238D6BC4002F794860` (`log/k1/baseline-preparation.json`). The verified current release benchmark executable is retained temporarily as `log/k1/bin/interactive_bench-current.exe` with an adjacent hash, so the final implementation can be remeasured immediately after the baseline run. This narrow comparison is queued behind the native checks; only the coordinator builds, serially, in its own target directory. No broader Windows baseline or performance claim is invented.

### Post-SVG native refresh failure checkpoint — 2026-11-05

The refreshed native suite found one existing UI unit failure: `menus::native_menu::tests::opening_a_document_changes_state_not_structure` at `crates/ui-egui/src/menus/native_menu/tests.rs:156`, expected equal structure fingerprints but got9312310541458268803 and12425703053248169019. The other4,883 UI unit tests passed (21 preexisting ignores); the no-fail-fast suite is still collecting integration/doctest outcomes. Its earlier pre-SVG run passed this test. The workspace agent is diagnosing the menu/state fixture read-only; no assertion has been relaxed and no fix is assumed. `log/k1/full-tests-post-svg.log` is diagnostic, not an accepted complete native pass. The gate runner will stop before artifact refresh. K1 remains open; baseline comparison remains queued, no second build is active.

### Native-menu test race isolated — 2026-11-05

The failing test passed unchanged when run alone: `target/agent-root/debug/deps/photocraft_ui_egui-f57dba86b1ed484e.exe menus::native_menu::tests::opening_a_document_changes_state_not_structure --exact --nocapture` (1 passed, zero failed; `log/k1/native-menu-race-isolated.log`). The full log places the two real plug-in installation tests immediately around its failure. They mutate `photocraft_plugins::registry` process-wide; installing a plug-in correctly changes menu structure during the other test's successive snapshots.

The workspace agent owns a 13-line test-only fix in `crates/ui-egui/src/menus/plugin_ui.rs` and `crates/ui-egui/src/menus/native_menu/tests.rs`: one shared `cfg(test)` mutex guards both real installer tests and the three tests requiring unchanged registry contents across snapshots. All production code paths, real installation/menu generation and existing assertions remain intact. Search confirmed no other UI-lib registry mutators. The replay agent is reviewing the fix independently. The prior run is retained as `log/k1/full-tests-post-svg.log` (10,566 passed, one failed, 59 existing ignores); the new complete native run uses `log/k1/full-tests-final5.log`. No benchmark source or runtime implementation changed in this fix. K1 remains open until the refreshed run and remaining verification finish.

### Refreshed complete native suite passed — 2026-11-05

After the test-only registry guard, the exact seven-package command from the native-check table passed again: **10,567 passed, zero failed, 59 preexisting ignores, 51 test groups including doctests** (`log/k1/full-tests-final5.log`, `native-counts-final5.json`). This supersedes the diagnostic `full-tests-post-svg.log`; the original failure and successful isolated reproduction remain retained. UI unit tests are4,884 passed again; I/O unit tests are574 passed, including the two new SVG-worker regressions. All K1 tests ran. Independent replay-agent review found no coverage loss or lock-lifetime/deadlock issue in the test-only fix. Strict clippy, adversarial commands, WASM, artifact refresh and direct K1 timings follow in the same serial runner; the isolated24MP comparison remains queued.

The remaining refreshed correctness gates passed: strict seven-package clippy with `--all-targets --offline -- -D warnings` (`clippy-post-svg.log`); the actual ignored-by-default `panic_hunt` test ran and passed1/0failed/0ignored (`panic-hunt-post-svg.log`); `cargo xtask wasm` passed26portable crates plus codecs/HEIF (`wasm-post-svg.log`); `cargo xtask parity` retained660/660wiring (`parity-post-svg.log`); `cargo xtask layers` passed33crates (`layers-post-svg.log`). Formatting also passed after the registry guard. The build/run/artifact/timing stages are now active; the before/after heavy-operation comparison and final cleanup/audit remain pending.

### Final native artifact, visuals and K1 timing — 2026-11-05

The retained development executable [photocraft.exe](../../log/k1/bin/photocraft.exe) now has SHA-256 `B108AA82292DFEF75C052DC9AE5C200BC869035E89D1B791C760850D2B33B38C` (adjacent `photocraft.sha256`). `cargo build -p photocraft --offline` and the rebuilt snapshot example passed. `log/k1/finish-post-svg.ps1` then tested that exact copied executable, with no build running during the direct K1 timing tests.

- Native recovery: [result JSON](../../log/k1/native-smoke-82859477cf2741cb8321d918d521b335/result.json), [inspected native screenshot](../../log/k1/native-smoke-82859477cf2741cb8321d918d521b335/native-reopened.png), `native-smoke-post-svg.log`. The actual app was killed immediately after revision 5 acknowledgment, reopened the same project, and retained both layers and exact blue final pixels after undo. Komaphone workspace was enabled; upstream updates were unsupported; real native Import was enabled. The script closed its own test processes.
- The regenerated 1280×720, 1920×1080 and logical 1280×720 at 2× PNGs are byte-identical to the inspected layouts. `snapshot-hashes-post-svg.json` records the three hashes; `snapshot-post-svg-{1280,1920,hidpi}.log` records the runs. The final native reopen screenshot is also byte-identical to the earlier inspected image (SHA-256 `C28D8D2139C2D33EA2EEE2F3C649DA1A269AB98463CC8168D211A90D9D81BFF5`) and was inspected again.
- `target/agent-root/debug/deps/local_projects-1a607b59a75d073d.exe ten_layer_ordinary_strokes_are_durable_within_one_second --exact --nocapture`: **passed**, 1536×1024, ten layers, twenty ordinary strokes, p50 **14.02 ms**, p95 **18.99 ms**, max **34.69 ms**, versus the 1 second target. Log `persistence-latency-post-svg.log`.
- `target/agent-root/debug/deps/it-7f5114e654f59d7e.exe komaphone_latency::ten_layers_pointer_to_rendered_stroke_latency --exact --nocapture`: **passed**, same canvas/layers, 1280×720 viewport, twenty pointer moves; p50 **41.50 ms**, p95 **42.43 ms**, max **44.02 ms**, versus the 50 ms target. Every sample appeared on frame 2. Vulkan / RTX 5060 Ti / NVIDIA 610.88, no fallback. Log `visible-latency-post-svg.log`. This remains software input-to-offscreen-readback including the explicit frame interval, not physical device/OS/scanout latency.

Comparable observations before the final SVG/test-isolation work were pointer p50/p95/max 41.53/43.13/43.36 ms and durability 14.12/15.04/18.59 ms on the same workloads. The initial integration durability run was 14.00/18.54/18.80 ms. All are single 20-sample observations; they show the K1 targets being met, not a statistically established speedup. The quick-performance reports have been retained under `log/k1/perf-quick-results/` outside the disposable Cargo cache. Only the narrow 24 MP before/after comparison and final audit/cleanup remain before closure.

### Same-machine 24 MP before/after evidence — 2026-11-05

`log/k1/compare-24mp.ps1` completed both serial runs, followed by `log/k1/analyze-24mp.ps1`. Original source was commit `69a9aacb566e036264a93ed6da925cf96eeda0bf`; the current benchmark executable was captured after the SVG runtime fix and before the later test-only registry guard. No runtime code changed afterward. Benchmark source hashes match exactly, existing dependency versions and release settings match, and both logs identify NVIDIA GeForce RTX 5060 Ti, 6000×4000, 8-bit RGB, three warm samples and GPU canvas enabled.

Artifacts: [before JSON](../../log/k1/interactive-24mp-before.json), [after JSON](../../log/k1/interactive-24mp-after.json), [all 32 compared rows](../../log/k1/interactive-24mp-comparison.json); corresponding `.log` files retain the exact commands/output. Inputs are deterministic generated gradients/noise. Format rows encode/decode memory buffers; the benchmark does not open personal files, persist settings or drafts, or invoke the desktop updater.

| Operation | Original median (ms) | Current median (ms) |
|---|---:|---:|
| 60 px brush dab + refresh | 0.572 | 0.507 |
| 40-dab 120 px brush stroke | 9.596 | 9.087 |
| Gaussian Blur radius 40 + refresh | 246.542 | 245.353 |
| Full `.pcraft` encoding | 168.698 | 168.389 |
| PSD save | 300.568 | 292.179 |
| PSD reopen + refresh, three layers | 90.095 | 90.340 |
| CPU flatten, three layers | 81.619 | 80.614 |
| PNG export | 356.970 | 354.829 |
| JPEG export | 412.951 | 416.118 |

None of the 32 sampled medians increased by both 15% and 1 ms, used here only to flag observations for review. This single sequential pair is not a statistically calibrated regression baseline or proof of a speedup: each row has one initial invocation and three warm samples, with possible thermal/cache/scheduling variation. The initial invocation is not an OS/disk cold-cache measurement. Individual operations may use CPU fallback even though the canvas is GPU-enabled; both adapters match and neither log reports an error. These results cover Advanced commands/composition/in-memory formats, separately from K1 SQLite durability and software input latency. The missing broader Windows quick-performance baseline remains an explicit coverage limit; no versioned baseline or budget was changed.

## K1 completion — 2026-11-05

**Complete; stop at K1.** The observable gate in the delivery plan is satisfied: mouse drawing and Advanced access, durable local drafts and replay, acknowledged-save restart with layers/undo-final pixels, a real process kill during an uncommitted write, actual storage-failure handling without false success, corruption recovery, and inspected required layouts. K0's 53 contract tests were rerun before integration. The source checkout began at `69a9aacb566e036264a93ed6da925cf96eeda0bf`; existing planning/K0 work was preserved. Implementation files and ownership are listed in the native acceptance checkpoint above. The final validation fixes additionally touch `crates/io/src/svg.rs`, `crates/io/src/svg/tests.rs`, `crates/ui-egui/src/menus/plugin_ui.rs` and `crates/ui-egui/src/menus/native_menu/tests.rs`; independent reviews and regression evidence are recorded above.

### Accepted final checks

Commands used `. ./log/k0/env-msvc.ps1`, Rust 1.95 / MSVC toolchain, the coordinator's `target/agent-root`, and serial build pipelines. Release compilation used eight workers after the recorded memory check; no test, optimization, assertion, acceptance threshold or product requirement was relaxed. Logs below are under `log/k1/`.

| Exact command | Final accepted result / log |
|---|---|
| `cargo test -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft-psd -p photocraft-io -p photocraft -p xtask --no-fail-fast --offline` | 10,567 passed, zero failed, 59 existing opt-in/platform/manual ignores, 51 groups including doctests; `full-tests-final5.log`, `native-counts-final5.json`. No K1 test ignored. |
| `cargo clippy -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft-psd -p photocraft-io -p photocraft -p xtask --all-targets --offline -- -D warnings` | Passed without dependency lint relaxation; `clippy-post-svg.log`. |
| `cargo test -p photocraft-engine --test panic_hunt --offline -- --ignored` | Adversarial test actually ran: one passed, zero failed/ignored; `panic-hunt-post-svg.log`. |
| `cargo xtask wasm` | 26 portable crates plus codecs/HEIF passed; `wasm-post-svg.log`. |
| `cargo xtask layers` | 33 crates passed; `layers-post-svg.log`. |
| `cargo xtask parity` | 660/660 menu wiring retained; `parity-post-svg.log`. |
| `cargo xtask scorecard` | Passed; `scorecard.log`. Final xtask tests also passed generated-file staleness/determinism checks. |
| `cargo fmt -p photocraft-engine -p photocraft-ui-egui -p photocraft-local-store -p photocraft-psd -p photocraft-io -p photocraft -p xtask -- --check` | Passed after the final test-only edit; `fmt-post-svg.log`. |
| `cargo xtask test-corpus` | Normal release/LTO/default threads: 6,232 passed, zero failed, 40 existing ignores, 42 groups; `corpus-tests.log`, `corpus-counts.json`. Pinned real-file inputs verified. |
| `cargo xtask perf --quick` | All nine planned benchmark programs completed successfully; `perf-quick.log`, retained `perf-quick-results/`. Full-size budgets and a matching Windows regression baseline are not covered by this command. |
| `cargo run --release -p photocraft-ui-egui --example interactive_bench --offline -- --size 6000x4000 --reps 3 --json log/k1/interactive-24mp.json` | All 32 operations measured; `interactive-24mp.log/json`. The additional same-machine original/current comparison and its limits are recorded directly above. |
| `cargo build -p photocraft --offline` | Passed; `build-post-svg.log`; copied executable verified by native recovery smoke. |

### Delivered artifact and acceptance evidence

Run [the retained Windows development executable](../../log/k1/bin/photocraft.exe) using the isolated commands in [the local-draft guide](k1-local-drafts.md), then choose **New drawing**. SHA-256: `B108AA82292DFEF75C052DC9AE5C200BC869035E89D1B791C760850D2B33B38C`. `& ./log/k1/native-smoke.ps1` passed again after the Cargo cache was removed, without loading the Rust/MSVC helper in that shell: [result](../../log/k1/native-smoke-dfcc0ed7884549f8815a7c5b2faa9d7f/result.json), [native reopened image](../../log/k1/native-smoke-dfcc0ed7884549f8815a7c5b2faa9d7f/native-reopened.png), `native-smoke-after-cleanup.log`. This verifies the actual retained app, revision 5 acknowledgment, forced termination, relaunch, two layers and exact undo-final blue pixels.

The final [1280x720](../../log/k1/workspace-1280.png), [1920x1080](../../log/k1/workspace-1920.png), and [2x high-DPI](../../log/k1/workspace-hidpi.png) images match the inspected layouts byte-for-byte (`snapshot-hashes-post-svg.json`). The native reopen image also matches the inspected native image. The real native Import control is enabled; automated mouse import uses the actual decoder with a picker stub. The operating-system file dialog itself was not clicked.

Final uncontended ten-layer measurements: software pointer-to-rendered-readback p95 **42.43 ms** against the 50 ms target, and edit-to-durable-ACK p95 **18.99 ms** against the 1 second target. Exact executable invocations, p50/max, hardware and workload appear in the final native artifact/timing entry above; logs are `visible-latency-post-svg.log` and `persistence-latency-post-svg.log`. These are bounded local observations, not physical mouse-to-display or broad statistical performance guarantees.

### Cleanup, limitations and next action

Verified and removed the coordinator's 43.39 GiB build cache plus the temporary original-checkout source/archive; no `target/agent-*` directory remains. Retained the working executable, benchmark copy, logs/screenshots/reports, corpus cache, `target/k0-tools` and build-tool verification. `cleanup-preflight.json` and `cleanup-result.json` record checked absolute paths and 61.68 GiB free after cleanup; `closure-check.json` confirms the executable hash, zero task-owned build/app processes and zero agent build directories. Rebuilding examples/tests will recreate a target directory.

This is a local development build, not an installer or multiplayer/friend-ready release. The operational storage bounds, failure behavior and explicit Advanced File Save fallback are documented; data is not pruned to meet quotas. Reopen restores final editable content and durable replay, not the old interactive undo stack. Power loss/drive failure and portable archive backup/restore remain later work. Broader quick-performance regression coverage lacks a matching Windows baseline; the completed narrow 24 MP comparison does not replace it. No external K1 acceptance gate is blocked.

The product baseline remains 1.1 and the working editor/attribution are preserved. No CI change, service deployment, purchase, publication or commit was made. **Next action in a later run: K2 private service and rooms.** K2 was not started here; K4 local remix and K6 world remix, guided tours/export and since-you-were-away playback remain required at their assigned gates.

Final read-only closure review by `local_store` independently checked the delivery gates, final counts/logs, retained executable hash and after-cleanup restart result; no unmet K1 criterion or misleading current completion claim was found. `& ./log/k1/final-audit.ps1` passed: **191 local Markdown links across 15 documents**, no broken links or trailing whitespace, and `git diff --check` passed (only Git's LF-to-CRLF notices). Output: `log/k1/audit-final.log`.

## K2 start — 2026-11-05

The coordinator read AGENTS.md, baseline 1.1, delivery plan, architecture, status, service-stack and bounded agent prompts. K2 is the next dependency-ready milestone. Existing uncommitted planning/K0/K1 changes on main at 69a9aacb566e036264a93ed6da925cf96eeda0bf are preserved. No K3–K7 scope is started, no deployment/purchase is authorized or claimed.

Ownership: coordinator owns shared party schema/protocol/room transitions, engine/app registrations, UI/state, Cargo.lock/layers, integration and this ledger. `service` owns new apps/komaphone-service (package photocraft-sync). `client` owns new crates/room-client and engine rooms/komaphone_room_cmds modules. `review` owns new party lifecycle/protocol tests and independent fault review/harness. New manifests are written atomically by their owner; shared manifests stay coordinator-only. Each agent uses its own target/agent-<name>; whole-app builds stay serialized with the coordinator.

Prerequisite evidence independently checked: retained K1 executable SHA256 matches B108AA82292DFEF75C052DC9AE5C200BC869035E89D1B791C760850D2B33B38C; final log counts match 10,567 passed/zero failed/59 preexisting ignores across 51 groups, and panic_hunt actually ran. Fresh `& ./log/k1/native-smoke.ps1` initially timed out on sandboxed loopback despite the app listener; the same command with approved unrestricted execution passed in 2.7 seconds. Result: log/k1/native-smoke-88352d5d2dcd4c2dbfdc2152c248223b/result.json, with native-reopened.png. Forced termination followed revision 5 ACK; restart retained both layers and exact undo-final pixels. Its own processes were closed.

K2 choices preserve the recorded HTTP polling/SQLite design. Shared RPC schema is versioned and bounded; raw capability-bearing requests deliberately omit Debug. Credentials use safe Windows Credential Manager API through keyring 3.6.3 (MIT/Apache, Windows-native backend); tokens/invitations are 256-bit cryptographic secrets, never labels. Pairing establishes participant/device credential bindings, and room membership remains separately authorized. Native adapters run HTTP/storage off the UI thread. Waiting players receive no active assignment or prior reveal. Previous terminal snapshots must be archived transactionally before a NextGame reset. Timer and fault checks remain required.

Dependency fetch: `. ./log/k0/env-msvc.ps1; cargo fetch --locked` failed restricted DNS (no test ran); approved `cargo fetch` downloaded the selected axum additions and is resolving new native dependencies. The coordinator retains target/k0-tools and uses target/agent-root. Tests, visuals, process durability and protocol/privacy gates are not yet passed. Next: finish shared contracts/lifecycle tests, integrate service/client workers and lobby, then run the real isolated multi-client harness and mandatory checks.

### K2 integration checkpoint — 2026-11-05

The first native service suite passed 19 tests, zero failures/ignores (`log/k2/service-tests-4.log`): five canonical asset/full two/three-player mode and backup tests, three real HTTP multi-client/protocol/reconnect tests, eight independent auth/invite/receipt/timer/process-kill tests, and three actual SQLite quota/schema/corruption tests. The coordinator inspected the full accepted result groups. This is initial service evidence, not milestone closure; final integration and native app gates remain pending. Sandbox runs had Windows 10013 loopback restrictions and MSVC `lib.exe` LNK1114/OS5 generated archive replacement errors; approved unrestricted runs exercised the same sources/dependencies successfully. Root native `cargo test -p photocraft-engine -p photocraft-ui-egui -p photocraft -p xtask --no-run --offline` is retrying outside the sandbox with output in `native-build-retry.log`.

Shared lifecycle changes add options with ready-consent reset, leave/rejoin, waiting members without roster mutation or reveal access, terminal NextGame and moderation takeover after terminal host loss. Service archives the previous terminal state transactionally and rejects historical game/project identity reuse. Submissions/freeze manifests accept stale (never future) room revisions because independent peers/heartbeats can race; epoch, assignment, ownership and immutable payload remain mandatory, and metadata/moderator actions retain strict compare-and-set. The old K0 test that prohibited every late join now checks impersonated late joins; new independent lifecycle tests cover authorized waiting and frozen roster privacy. Initial updated party run passed 64 tests; additional concurrency/moderation regressions are awaiting rerun.

Root added optional schema-1 `ReplayRef.manifest` for compatibility with old baseline-only fixtures, plus a bounded typed ReplayArtifact carrying every locally recorded step and its exact result checkpoint. Actual native submissions will include the manifest; nonzero replay histories must have one on the service. Draft creation uses the authority-assigned project identity and does not reassign an unrelated local drawing. Export preparation runs on the existing local I/O worker and preserves the local draft/history on quota or network failure. Mode-specific reference/onion-skin/seam editor presentation and complete reveal experience remain K3; K2 validates canonical transmitted artwork and authorization at the service boundary.

Current K2 code paths additionally include engine room command modules, rooms client, assigned draft preparation, native app startup, serialized lobby view state, private-room controls and safe automation inspection. Raw invitations are excluded from Debug, ordinary UI state/status, Actions and drawing journals. Explicit invitation sharing is a separate command. The service remains loopback-only; non-loopback binding fails closed. External TLS/service-route deployment and separate-machine friend-readiness belong to later gates, with no publication or purchase performed here.

### K2 validation and deadline review checkpoint — 2026-11-05

Independent party verification now passes 75 tests/zero failed/zero ignored plus strict all-target clippy (`party-review-packs.log`, `party-review-packs-clippy.log`, target/agent-review, jobs 2). Service final implementation passes 24 tests/zero failures/ignores and strict clippy/format (`service-tests-final.log`, `service-clippy-final.log`, `service-fmt-final.log`). Retained service executable log/k2/bin/photocraft-sync.exe SHA256 7842D13E4411BDB26A7C069771F6A5212A8694FED723889D4ECF1C9B7DBA5B2A. Its checked 1.38 GiB target/agent-service was removed; root/review/client targets remain active. A scoped official RustSec review and exact license evidence are recorded in the service README and log/k2/dependency-*; this is explicitly not a whole-lockfile security audit. Matchit requires both MIT and BSD-3-Clause notices when distributing.

Replay packing removes the per-edit top-level asset ceiling: intermediate native checkpoints are framed into checksum-validated containers up to 64 MiB each, while final/baseline checkpoints remain directly referenced. Every step and undo/redo result is retained. A 300-checkpoint hostile-framing/round-trip suite passed in the party/service tests; the native preparation regression with 270 actual stored edits is queued. The outer 512 MiB data bound and native outbox's 384 MiB serialized-request/8192-request limits remain explicit and failure preserves originals. Native package generation checks dimensions before upload; no replay parameters are executed by the service.

Service incomplete uploads now remain under the explicit quota across long outages, rather than expiring acknowledged chunks behind a durable client retry. A seven-day resume test covers this reversible operational choice. No complete or unsynced work is pruned. Baseline 1.1 experience/order/exclusions are unchanged.

Review identified and fixed or is testing three native races: interleave room polls during uploads and prepend the exact immutable FreezeReport; correlate worker command sequences so stale status cannot unlock a pending submission; persist the first deadline revision pin before heavy package preparation, then reuse it after restart/later local ACK/tab switches. Only actual pending submissions should freeze assigned drawing; unrelated pending room metadata must not block offline drawing. Last two integration details are being finalized in client/engine; their final tests are pending. Exported submission IDs are deterministic from assignment/project/revision so re-preparation cannot invent a conflicting duplicate. The local save indicator remains separate from pending sync and service acceptance.

Initial root build exposed a JSON macro expression issue (fixed), a deprecated egui ID constructor (fixed), and an in-progress shared-tree client API race (old compiled client/new engine source). No assertion or lint was weakened. Root native build4 is refreshing against integrated code; full correctness gates, real multi-app smoke and inspected final screenshots remain required. The prepared native harness creates isolated service plus three actual apps and will exercise lobby mouse input, native durable submissions, host/client/service kills, queued work, and timer freeze. K2 is still in progress.

### K2 native integration build checkpoint — 2026-11-05

The coordinator's integrated `cargo test -p photocraft-engine -p photocraft-ui-egui -p photocraft -p xtask --no-run --offline` passed in 6m48s (`log/k2/native-build-5.log`) after fixing a moved PathBuf input in the snapshot example. This proves compilation only; full tests and native acceptance remain pending. A diagnostic executable was copied to `log/k2/bin/photocraft-integration.exe` (SHA256 F463B3A960AF6AB03C3286F89AEE186AFE98995551295AA3DDD6706701764160). It is not the final deliverable, as strict matching-pin revision enforcement was added afterward.

The client now persists the first FreezePin before heavy export, distinguishes actual pending submissions from metadata requests, and rejects any later revision for the same pin even if saved locally. Review's four engine boundary tests cover tab switches, startup listing, edits while frozen, and invalid/future pins; final compiled execution is pending. The native harness now launches three real isolated app roots against the real service. Initial diagnostic setup paired three clients but did not complete; the reviewer is adding durable failure capture before rerunning. No native smoke pass is claimed.

Independent format review identified the service's 1 MiB native manifest limit as stricter than the existing local store's 8 MiB limit. The service owner is aligning it to 8 MiB and adding real checkpoint acceptance/rejection tests. The 64 MiB object and 512 MiB decoded/room bounds stay intact. This changes a bounded operational setting, preserving the agreed drawing experience and product baseline.

Automatic approval review rejected the first Clippy launch because it believed the previous whole-app build still ran. The coordinator verified the successful build log and absence of its parent/compiler processes, then the same standalone check was approved. No competing whole-app build or approval bypass occurred. Clippy is running in `target/agent-root`; focused client/service checks use their own directories.

Run instructions and trust/recovery/limit details are now in [the K2 runbook](k2-private-rooms.md). The actual desktop data override is `KOMAPHONE_DATA_DIR`, not a CLI flag; the offscreen snapshot example separately supports `--komaphone-data-dir`. Control command IDs and secret handling are documented in `docs/control-protocol.md`. Next: finish native recovery/timer acceptance, fix strict lints, execute all mandatory checks, inspect screenshots, retain final binaries and close only the K2 gate.

### K2 native recovery and timer diagnostic — 2026-11-05

The real three-app harness passed its base run at `log/k2/native-rooms-6dc957b3e4e44d95aad77f264fe8aad9/result.json`. It exercised mouse Ready, two-player Animation start and a real saved native checkpoint/replay submission, late-arrival waiting privacy, host termination/moderator claim/rejoin, service outage, durable offline drawing and queued metadata across app termination, service restart retaining accepted art, and next-game three-player Corpse with a real accepted native submission. The coordinator inspected the accepted-art, service-unavailable and three-player lobby screenshots; the reviewer inspected all six. These are actual native controls and authoritative records, not mock projections.

The full timed diagnostic also passed at `log/k2/native-rooms-072e3108cf2b4502a25333c6fb735bf1/result.json` with `timedGateExercised=true`: the real 6000 ms service timer entered Transfer, the app automatically submitted saved revision 3 from its persistent pin while another tab was active, and the unrelated local draft remained editable. The diagnostic app/service hashes are recorded in each result. Final acceptance still requires rerunning the latest binaries; the app has since gained strict matching-pin validation and a Komaphone empty-workspace screen.

The reusable harness is now tracked at `scripts/komaphone/k2-native-smoke.ps1`, writing isolated artifacts under `log/k2`; the original local script is a compatibility wrapper. Initial harness failures were corrected without weakening product checks: PowerShell's reserved Host variable, missing peer-poll synchronization for strict metadata CAS, obsolete command spelling, and a pause issued before explicitly resuming host-loss pause. Old test credentials were removed with the refreshed cleanup helper, and caller environment variables are restored. No personal profile was used.

Final service checks after aligning the native manifest bound passed 25 tests/zero failures/ignores (10 asset, 4 HTTP, 8 independent, 3 storage), strict all-target Clippy and format. Logs: `service-tests-manifest-alignment.log`, `service-clippy-manifest-alignment.log`, `service-fmt-manifest-alignment.log`, `service-manifest-bound-test.log`. Retained service SHA256 is now 542C44CB09C901B98733BB64FD538A8446A5A366EC7DC08BC3FF17C6FC246102; previous hashes above are historical. Service target cache was checked and removed. Client final checks passed 18 tests/zero failed/ignored, strict Clippy and refreshed standalone WASM. Root strict Clippy passed in 58.15 seconds (`root-clippy-3.log`) after simplifying two Boolean expressions and one nested invitation-copy condition; no lint was suppressed. Full regression execution and final visual artifacts remain pending.

### K2 final review corrections — 2026-11-05

The native Leave path exposed a genuine adapter mismatch: the service correctly returned Disconnected after committing Leave, but the client only accepted that response for the separate Disconnect operation. The tracked harness reproduced pending work and subsequent unauthorized polling (`log/k2/native-rooms-0f9b0696b7cb401c88e17d666bb9e329/timeout-client-3.json`). Service receipt retries were independently checked and remain safe. The client now atomically acknowledges Leave, stops polling, clears removed lobby/waiting projections, preserves frozen active-game assignments for explicit reconnect, and resumes polling after a successful Create/Join. Three regression cases cover leave/restart/same-identity rejoin, active assignment retention and rejecting Disconnected for unrelated actions. Client final tests are now 21 passed/zero failed/ignored; strict Clippy and WASM passed in the existing client logs. Native acceptance has been extended with lobby Leave, new invitation and same-identity rejoin and must pass on the latest app.

The coordinator directly ran the compiled engine's two assigned-draft preparation tests (`target/agent-root/debug/deps/photocraft_engine-b5dd5c65d171c598.exe rooms::drafts::tests --test-threads=1 --nocapture`), both passed in 1.55 seconds (`replay-package-focused.log`). This includes 270 actual saved revisions, 269 replay events, 268 packed intermediate checkpoints and fewer than ten top-level assets, plus exact undo-endpoint and seam retention. Full touched-crate tests remain the final gate.

Visual review corrected the empty Komaphone workspace to show private-room/local-drawing entry points and explicit PhotoCraft attribution, keeping upstream About credits and Advanced editor support. No baseline feature or milestone moved. Current build log name `app-build-final.log` is provisional until refreshed after the Leave fix; the ledger will identify the final retained executable by hash.

### K2 observable acceptance passed — final artifacts, global checks still running

`& ./scripts/komaphone/k2-native-smoke.ps1` passed with no flags/skipped checks against the retained final app and service. Result: [native result](../../log/k2/native-rooms-baf41258c15043f189e63b14c815ccd8/result.json); compact assertions/hashes/cleanup: [native review](../../log/k2/native-review-final.json). App SHA256 **53B980478FC2FF4B49B1B6CAE1E4BD073C249E057E3A1C63E170ED54C971CE0E**; service SHA256 **542C44CB09C901B98733BB64FD538A8446A5A366EC7DC08BC3FF17C6FC246102**. Both are retained in `log/k2/bin`; the app copy matched its just-built source binary. Cleanup helper SHA256 FF29048E48CCCB55972E2B0D660CF6D7AD6E10EA61081747D32ED7CEED5431B8. No production source change followed this native run.

The final native flow exercises two/three actual Windows clients; mouse Ready; real accepted Animation checkpoint/replay and Corpse checkpoint/replay/seam; private late-arrival waiting; host kill/claim/stable-identity rejoin; service kill/restart retaining accepted artwork; actual assigned-project edits while a Pause request is queued offline; exact project, two layers and canonical pixel recovery after client kill; explicit Skip advancing round 0 to 1 once; archive retaining one artwork/replay and three labeled missing entries with the exact skipped project/reason; real Leave ACK clearing pending/polling, new invitation and same-identity rejoin; and a 6000 ms service timer pinning/submitting saved revision 3 while another local tab remains editable. Five isolated credential references (2+2+1) were removed after stopping every harness process. The earlier unassigned-draft diagnostics and pre-fix Leave failures remain historical evidence, not the final acceptance claim.

The reviewer inspected all eight final native PNGs. The coordinator independently inspected final [outage/assigned work](../../log/k2/native-rooms-baf41258c15043f189e63b14c815ccd8/client-2/service-unavailable.png), [incomplete archive](../../log/k2/native-rooms-baf41258c15043f189e63b14c815ccd8/client-2/incomplete-skip-archive.png), [three-player lobby](../../log/k2/native-rooms-baf41258c15043f189e63b14c815ccd8/client-2/lobby-three.png), and [automatic timer acceptance](../../log/k2/native-rooms-baf41258c15043f189e63b14c815ccd8/client-2/automatic-timed-submission.png), as well as earlier accepted-art/host-loss evidence. Final offscreen captures were rendered with `& ./log/k2/capture-final.ps1` and all four inspected: [home 1280](../../log/k2/home-1280.png), [room entry 1280](../../log/k2/room-entry-1280.png), [1920](../../log/k2/room-entry-1920.png), and [2x high DPI, 2560x1440 pixels](../../log/k2/room-entry-hidpi.png). Fields and controls fit, attribution is visible, and the return-to-drawing path is reachable. `snapshot-hashes.json` records their hashes; native screenshots prove connected states separately from these static entry/layout captures.

A separate reviewer mapped every K2 bullet/DoneWhen clause to HTTP, direct SQLite, native-process and pure/synthetic evidence in [the acceptance audit](../../log/k2/k2-acceptance-audit.json). All observable K2 criteria are now exercised; it leaves overall completion pending the coordinator checks. Extra Skip coverage was added after that audit found only a pure-domain test. A harness reconnect race was resolved by waiting for waiting-player presence as well as frozen members before issuing strict-CAS Resume; no authority rule was weakened.

Final sequential mandatory runner: `& ./log/k2/verify-root.ps1`, exact commands/exits/durations in `root-verification.json`. So far parity and scorecard generation passed; layers passed for 35 crates. The full native suite is running: all four engine deadline tests passed; UI unit group passed 4,884 tests, 21 preexisting ignores; broader UI integrations remain in progress with no reported failure so far. WASM, final Clippy, ignored panic_hunt, format and quick-performance gates remain pending. The retained binary is a working local development candidate, not yet a closed milestone, installer or cross-machine release.

### K2 canonical-color review correction — 2026-11-05

A final independent source review found a real K2 color assumption: CPU composite RGB values retain their document profile, while the submitted PNG deliberately carries no profile metadata. Direct float-to-byte quantization therefore made a linear-RGB midpoint appear as code 128 instead of sRGB code 188. A shared L2 `party::canonical_color` adapter now resolves the same RGB/gray composite profile as the editor and uses the existing cached CMS transform on float samples before quantization. CMYK/Lab composites already in sRGB are not converted twice; alpha and editable native checkpoints remain unchanged. This is a correctness fix within K2, not a product-baseline change. Party adds existing lower-layer doc/CMS dependencies; no new external package was selected.

Ownership: review owns the new `crates/party/src/canonical_color.rs` and five regression tests; coordinator owns party manifest/module/lock registration and the actual engine package regression; service owns canonical validation plus real accepted/rejected color/seam submission tests. The engine package test covers U8/U16/F32, exact PNG/seam code 188, original profile/depth and unchanged native samples. Tests are being executed, not yet claimed passed.

Before this correction, `root-verification.json` recorded all native tests (8,920 passed, zero failed, 47 existing ignores), strict Clippy, 27-crate WASM plus HEIF, layers (35), parity, scorecard, format and one actually executed ignored panic_hunt test passing. The already-running release performance build saw the new engine call against its previously compiled party dependency and failed E0433; no benchmark pass is claimed from that attempt. Its Cargo process exited. The integrated helper is registered and the shared lock refreshed with `cargo metadata --offline --format-version 1`; the new serial `& ./log/k2/verify-color.ps1` reruns affected tests/checks, builds final app/snapshot and then repeats quick performance. Service uses its own target directory. Final native acceptance and artifact hashes will be refreshed after both binaries are rebuilt. No assertion, lint or scope requirement was weakened.

### K2 final integrated artifacts and checks

All observable K2 criteria passed again after the color correction. The final command `& ./scripts/komaphone/k2-native-smoke.ps1` ran without flags/skips against the retained executables, not Cargo's transient paths. [Final native result](../../log/k2/native-rooms-622574da9aed4c5f83f152e2f5276799/result.json), [independent assertions and eight inspected PNGs](../../log/k2/native-review-final.json), and `log/k2/native-post-color.log` supersede the earlier native-pair evidence. The reviewer independently checked 17 assertions; the coordinator inspected outage, incomplete archive, three-player lobby and timed-submission images. The harness stopped its own processes and removed five isolated credential references (2+2+1).

Retained Windows development artifacts:

- [Komaphone app](../../log/k2/bin/photocraft.exe), SHA256 `68570CF29F916514C3B3738F03EB27F5655BCB4E35C85705955C75C0B1106DA5`.
- [Private service](../../log/k2/bin/photocraft-sync.exe), SHA256 `4F539E76A3B0C945933ECBE3CC5BC9266C81EE789B79C0A324641494F99D4524`.
- [Credential cleanup helper](../../log/k2/bin/cleanup_credentials.exe), SHA256 `FF29048E48CCCB55972E2B0D660CF6D7AD6E10EA61081747D32ED7CEED5431B8`; used only for isolated harness credential references.

How to try: follow [the K2 runbook](k2-private-rooms.md). Its launch commands use this retained pair and separate `%LOCALAPPDATA%\Komaphone-K2-Try` service/client roots; choose **Private room**, connect to the loopback service, create a room/invitation, join from the second/third client, Ready and Start. Native saved drawings can be submitted. For repeatable automated acceptance, run `& ./scripts/komaphone/k2-native-smoke.ps1` from the repository root; no Cargo build is needed while these artifacts are present. Advanced editor and upstream attribution remain available.

The following accepted checks supersede earlier pending descriptions. Rust 1.95 / MSVC toolchain, `. ./log/k0/env-msvc.ps1`, serial coordinator builds in `target/agent-root`; service used its own `target/agent-service` with two workers, client/review their own targets. Exact dates/durations/exits are in [initial verification](../../log/k2/root-verification.json) and [post-color verification](../../log/k2/post-color-verification.json). An initial performance attempt failed during shared-tree color integration as recorded above; the successful final performance result and limits below supersede that attempt.

| Exact command | Accepted result and local evidence |
|---|---|
| `cargo test -p photocraft-local-store -p photocraft-engine -p photocraft-ui-egui -p photocraft -p xtask --offline --no-fail-fast` | 8,920 passed, zero failed, 47 existing opt-in/platform/manual ignores in 45 groups; `root-tests.log`, `root-test-counts.json`. Includes UI mouse/privacy, four deadline tests and native draft/recovery regressions. |
| `cargo test -p photocraft-party -p photocraft-engine --offline --no-fail-fast` | After final color edit: 3,596 passed, zero failed, 22 existing ignores in 43 groups; `color-tests.log`, `color-test-counts.json`. Includes 80 party tests (five new color tests) and the actual U8/U16/F32 package/profile/seam regression. Counts overlap the prior engine suite; do not add them as unique tests. |
| `cargo test -p photocraft-room-client --offline` | 21 passed, zero failed/ignored; `client-tests-native.log`. Includes durable outbox/process kills, Windows credential round-trip, strict freeze pin and Leave/restart/rejoin cases. |
| `cargo test -p photocraft-sync --locked --offline` | Final 26 passed, zero failed/ignored in seven groups; `service-tests-color-final.log`, `service-color-counts.json`. Actual HTTP/SQLite authorization, retry/order, child-process crash, corruption/quota and canonical image/replay/seam checks. |
| `cargo clippy -p photocraft-party -p photocraft-engine -p photocraft-ui-egui -p photocraft -p xtask --all-targets --offline -- -D warnings` | Passed after final source edit; `color-clippy.log`. Earlier root Clippy also passed. |
| `cargo clippy -p photocraft-room-client --all-targets --offline -- -D warnings` | Passed; `client-clippy.log`. |
| `cargo clippy -p photocraft-sync --all-targets --locked --offline -- -D warnings` | Passed after final service edit; `service-clippy-color-final.log`. |
| `cargo test -p photocraft-engine --test panic_hunt --offline -- --ignored` | Actually executed after final edit: one passed, zero failed/ignored; `color-panic-hunt.log`. The ordinary suite's ignore is not counted as proof. |
| `cargo xtask wasm` | 27 portable crates plus optional codecs/HEIF passed after shared dependency change; `color-wasm.log`. |
| `cargo xtask layers` | 35 crates passed; `color-layers.log`. Party remains L2; doc/CMS dependencies are lower layers. |
| `cargo xtask parity` | 660/660 existing menu items retained; `color-parity.log`; generated parity document refreshed. |
| `cargo xtask scorecard` | Passed; `color-scorecard.log`; generated scorecard refreshed. No new performance budget or parity floor claim. |
| `cargo fmt --all --check` | Passed after final source edit; `color-fmt.log`; service owner also checked its package. |
| `cargo build -p photocraft --offline` | Passed; `color-app-build.log`; retained source/copy hashes checked and actual retained binary exercised above. |
| `cargo build -p photocraft-sync --locked --offline` | Passed; `service-build-color-final.log`, `service-color-final-evidence.json`; retained copy independently hash-checked. |
| `cargo build -p photocraft-ui-egui --example snapshot --offline` and `& ./log/k2/capture-final.ps1` | Rebuilt and rendered all four final captures; `color-snapshot-build.log`, `capture-post-color.log`. `snapshot-post-color-comparison.json`: all four byte-identical to inspected home/1280/1920/high-DPI PNGs. |

K2 adds/changes these implementation areas (prior uncommitted K0/K1 edits are preserved):

- Shared domain: `crates/party/Cargo.toml`, `src/{lib,contracts,room,protocol,replay_artifact,canonical_color}.rs`, lifecycle/protocol/replay/color tests and README. Coordinator owned schema/registration; review owned independent tests and color adapter.
- Service: `apps/komaphone-service/{Cargo.toml,README.md}`, `src/{lib,main,server,store,assets}.rs`, `tests/{assets,http,independent,storage_failures}.rs`. Service agent owned implementation; reviewer owned independent/HTTP fault checks.
- Native client: `crates/room-client/{Cargo.toml,README.md}`, `src/{lib,worker,transport,store,credentials}.rs`, tests and `examples/cleanup_credentials.rs`. Client agent owned adapter/commands; coordinator integrated shared registrations.
- Engine: `Cargo.toml`, `src/{lib,cmd,rooms,local_projects}.rs`, `src/cmd/komaphone_room_cmds.rs`, `src/rooms/drafts.rs`, `src/local_projects/native.rs`, `src/session/execute.rs`, and `tests/room_deadline.rs`.
- Thin UI/app: `crates/ui-egui/src/{lib,app,komaphone_rooms,komaphone_workspace,state}.rs`, `app/frame.rs`, `control/{dispatch,inspect,params}.rs`, `examples/snapshot.rs`, room integration test and `tests/it/main.rs`; `apps/photocraft/src/main.rs` enables the native adapter.
- Integration/handoff: `Cargo.lock`, `xtask/src/layers.rs`, `scripts/komaphone/k2-native-smoke.ps1`, root README, `docs/control-protocol.md`, party/client/service READMEs, `docs/komaphone/{k2-private-rooms,service-stack,status}.md`, generated parity/scorecard, and supplementary `log/devlog.md`.

No K2 change modifies the core PSD/I/O/format/compositor implementation; the K1 real-file corpus evidence remains recorded above. K2 exercises actual native checkpoint serialization/reopen through service and draft tests. K2 introduces bounded party-size preparation (1536x1024 or 1024x512), not a new general 24 MP editor algorithm. No UI parity floor, accepted input depth, assertion or safety rule was lowered.

### K2 completion — 2026-11-05

**Complete; stop at K2.** All four implementation bullets and six DoneWhen clauses in the delivery plan have runnable evidence. The independent [acceptance audit](../../log/k2/k2-acceptance-audit.json) maps authorization, duplicate/reordered requests, host/service loss, independent membership/identity, pending local recovery and control-port separation to actual HTTP/SQLite/native tests. K3-specific reference/onion-skin/copy/seam presentation and complete game reveals were not silently substituted into this gate or claimed delivered.

Final performance command: `cargo xtask perf --quick`, exit 0 in 1,138.1 seconds including release compilation (`log/k2/color-perf-quick.log`). All nine planned benchmark programs report `status: ok`, no failures, and the runner labels the run uncontended. Retained [results](../../log/k2/perf-quick-results/results.json) and [summary](../../log/k2/perf-quick-results/summary.md) include each workload/row. Hardware: Windows x86_64, AMD Family 25 Model 117 CPU, 16 logical cores, NVIDIA GeForce RTX 5060 Ti. Quick mode does not evaluate full-size budgets; regression comparison was skipped because the stored baseline is macOS/Apple M4 Pro. Some layout operations still report over-16-ms annotations (for example text visibility median 134.30 ms). This is not a claim that every editor latency target is met, nor a calibrated before/after regression conclusion. The K1 24 MP evidence remains separate; no unrelated Photoshop-parity work was added.

Cleanup used `& ./log/k2/cleanup-root.ps1` after all eleven final verification commands exited 0. It checked resolved absolute paths, absence of reparse points and active compilers, and tested binary hashes; copied the complete benchmark reports; then removed only the coordinator's 40,299,157,052-byte generated cache. [Cleanup preflight](../../log/k2/cleanup-preflight.json) and [result](../../log/k2/cleanup-result.json) confirm no `target/agent-*` directories remain, both retained binaries still match the tested hashes, and 64,869,695,488 bytes were free afterward. Client/review/service cleanup evidence is retained separately. No test-owned app/service process or credential remains from the final harness.

The snapshot executable is also retained at `log/k2/bin/snapshot.exe`; the capture script now uses that path. `& ./log/k2/capture-final.ps1` passed once more after cache removal: [after-cleanup evidence](../../log/k2/capture-after-cleanup.json), four PNGs byte-identical to the inspected images. Reproducing normal Cargo checks recreates an agent target; trying the retained app/service and smoke harness needs no build.

Remaining limitations are explicit operational/release boundaries: loopback HTTP only (non-loopback bind rejected), bounded storage/outbox assets with originals retained on failure, local development artifacts rather than an installer, and no cross-machine/TLS/hosted route exercised. **No external gate remains unmet for K2.** K4 must exercise its separate-Windows-machine/private-network release gate and backup/archive flows before claiming friend readiness. K3 is the next dependency-ready milestone; no K3 work starts in this run. K4 local remix and K6 world remix, guided tours/export and catch-up playback remain mandatory. Product baseline, Rust/egui/wgpu architecture, editor support and upstream attribution are preserved.

Final documentation gate: & ./log/k2/final-audit.ps1 passed, resolving 236 local Markdown links across 19 documents with no broken links or trailing whitespace; git diff --check passed (only LF-to-CRLF notices). Output: log/k2/audit-final.log. Independent reviews found no remaining implementation issue; all final artifacts are retained outside the removed build caches.

Final independent closure review rechecked all eleven zero-exit commands, nine successful benchmark records, current retained hashes, zero agent target directories and four after-cleanup captures. No remaining K2 acceptance or implementation gap was found. A stale pending-capture sentence in the local audit JSON was corrected to the completed evidence.

## K3 party modes 2026-11-05 — final acceptance ledger

Status: **complete; stop at the K3 gate**. All dates below are local 2026-11-05 unless a UTC timestamp is supplied. The [prerequisite review](../../log/k3/prerequisite-review.json) verified K2 before this run. The independent final review found no missing K3 functional acceptance criterion; no external K3 gate requires access or authorization.

### Delivered behavior and file ownership

- Coordinator (`target/agent-root`): shared engine/UI registrations and integration in `crates/engine/src/{cmd.rs,rooms.rs,local_projects.rs,jobs.rs,session/execute.rs}`, `cmd/{komaphone_game_cmds,komaphone_room_cmds}.rs`, `rooms/{drafts,media,media_tests}.rs`; shared client `lib.rs`, `worker.rs`, `media.rs`, `download.rs`; UI `lib.rs`, `app/frame.rs`, `canvas.rs`, `control/inspect.rs`, `komaphone_{rooms,workspace,game_view,game_view_tests}.rs`; documentation and coordinated gates. Existing project canvases stay fixed during an assignment; command dispatch and background-result application both honor frozen/prior-turn ownership without freezing unrelated local drawings.
- Animation agent (`target/agent-animation`): `crates/engine/src/rooms/animation.rs`, `crates/engine/tests/room_animation.rs`, `crates/ui-egui/src/komaphone_animation.rs`, final engine and full WASM checks. Previous-frame references and adjustable onion skin remain view overlays. Copy creates an independent undoable layer and preserves native color/profile/depth through existing CMS. The pristine blank background is hidden for transparent copies; edited layers remain. Reveal supports chain selection, play/pause, frame scrub, 1–24 fps, loop, separate credits and explicit notes.
- Corpse/client agent (`target/agent-corpse`): `crates/engine/src/rooms/corpse.rs`, `crates/ui-egui/src/komaphone_corpse.rs`; client `store.rs`, new `store_notes.rs`, media/presentation/note-retention tests, focused durability integration, client checks and final affected UI WASM check. Corpse renders only the preceding 24-row strip above the editable 1024 × 512 band. Reveal joins three original bands into 1024 × 1536 with no duplicated seam rows; the head author intentionally returns for legs with two players.
- Review agent (`target/agent-review`): `apps/komaphone-service/tests/k3_games.rs`, `crates/engine/tests/room_privacy.rs`, `crates/room-client/tests/{assignment_scope,upload_ack}.rs`, `scripts/komaphone/k3-native-smoke.ps1`; independent service/privacy/recovery review and real native acceptance. Service production code is unchanged from K2 and was rebuilt for this gate. Shared schema/registration changes stayed coordinated; agents did not launch competing whole-app builds.
- Documentation: added [K3 runbook](k3-party-modes.md), updated `README.md`, `docs/control-protocol.md`, this status and local `log/devlog.md`; regenerated `docs/parity-checklist.md` and `docs/scorecard.md`. No new dependency, core-format change, CI edit, parity-floor reduction or product-spec revision belongs to K3. Earlier K0–K2 edits remain in the checkout.

The client fetches only currently authorized canonical PNGs, rechecks grants across delayed chunks/events, verifies size/hash and acknowledges only accepted cache entries. CPU images and GPU textures are pruned when grants change. Hidden native checkpoints/replays are not prefetched. Notes remain metadata, not artwork/replay pixels; saved notes and reveal positions commit transactionally. Old own-turn drawings remain readable and immutable after restart. Service/client restarts preserve editing, queued upload, timed revision pins, Transfer pause and reveal navigation.

### Observable acceptance

`& ./scripts/komaphone/k3-native-smoke.ps1` passed **without `-Diagnostic`**, exit 0, **153.498 seconds**, 2026-11-06 05:29:43–05:32:17 UTC. It used isolated roots, a real service and three actual Windows clients. [Result](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/result.json), [wrapper](../../log/k3/review-native-final-run.json), [independent verification](../../log/k3/review-native-final-verification.json), [coordinator verification](../../log/k3/root-final-native-verification.json).

| Actual UI game | Players | Rounds | Accepted manual submissions |
|---|---:|---:|---:|
| Animation | 2 | 4 | 8 |
| Exquisite Corpse | 2 | 3 | 6 |
| Animation | 3 | 6 | 18 |
| Exquisite Corpse | 3 | 3 | 9 |

The run records 174 actual control interactions, nine numeric-control changes and 67 expected assignment command refusals. Mouse Ready/Start/Open/draw/Submit/copy/onion/reveal/play/loop/parts/next-game controls were exercised. Onion opacity changed 0.30 → 0.65; playback speed and frame changed through their visible numeric controls. Native games use the default two Animation rotations; headless schedule/reveal tests cover configured one–four rotations.

Every player reached the expected game/epoch/revision before strict comparisons of authorized manifest, decoded cache and actual GPU texture hashes. Before reveal, only the authorized prior frame/note or Corpse strip was available; forbidden hash fetches and command access were tested independently. Colored boundary rows prove seam alignment and the final band bottom row; references/notes/cursors remain outside canonical pixels. Reversed/racing submissions and retries cannot duplicate turns. The separate HTTP harness completes the same four games against real SQLite and HTTP.

The native run restarted clients during editing, queued upload and reveal, and restarted the service/client during timed Transfer. It verified identical saved pixels, accepted offline work, frozen prior-turn documents after reopen and restored reveal selection. An additional timed frame was accepted. With one player still disconnected, the host clicked Skip (revision 83), Resume (84) and End; the retained incomplete game contains one artwork and eight explicitly missing entries, including the skipped participant reason. All three credential-cleanup helpers exited 0, and the harness stopped its processes.

### Exact verification commands and outcomes

All Cargo commands used the pinned local Rust **1.95.0 MSVC** toolchain, cached dependencies/offline mode, `CARGO_INCREMENTAL=0` and the owner's target directory. `. ./log/k0/env-msvc.ps1` establishes the toolchain. Whole-app work was serialized in `target/agent-root`; other agents used separate directories. Existing ignored tests are reported separately and do not satisfy acceptance.

| Command | Observed result and evidence |
|---|---|
| `cargo test -p photocraft-engine --offline --no-fail-fast` | 3,536 passed, 0 failed, 22 existing ignores; 37 groups, 145.54 s. [Full record](../../log/k3/engine-full-test-result.json). Later affected checks below cover final changes; totals overlap. |
| `cargo test -p photocraft-engine --test room_animation --test room_privacy --test room_deadline --offline` | 15 passed, 0 failed/ignored; 119.88 s including compile. [Final engine ledger](../../log/k3/engine-acceptance-verification.json). |
| `cargo test -p photocraft-engine --lib rooms:: --offline` | 14 passed, 0 failed/ignored; 275.53 s including compile; source hashes unchanged before/after. Same final engine ledger. |
| `cargo test -p photocraft-engine --test panic_hunt --offline -- --ignored` | 1 explicitly executed adversarial-command test passed, 0 failed/ignored; 24.59 s including compile. Same final engine ledger. |
| `cargo test -p photocraft-room-client --offline --no-fail-fast` | 58 passed, 0 failed/ignored across eight targets; 28.19 s. Includes process-kill durability, assignment scope, note quota/migration/corruption, download grants and upload acknowledgments. [Client ledger](../../log/k3/corpse-verification.json), [counts](../../log/k3/corpse-client-test-counts.json). |
| `cargo test -p photocraft-sync --offline` | 27 passed, 0 failed/ignored; [service ledger](../../log/k3/review-service-verification.json). |
| `cargo test -p photocraft-sync --test k3_games --offline -- --nocapture` | 1 compound real four-game HTTP test passed, 0 failed/ignored; 2.03 s runtime. Coordinator reran retained executable successfully in 2.86 s. [HTTP log](../../log/k3/review-http-games-native.log). |
| `cargo test -p photocraft-ui-egui -p photocraft -p xtask --offline --no-fail-fast` | Initial run: app 215+14, UI integration 200 and xtask 77 passed; eight new UI-library fixture failures fixed below. [Initial ledger](../../log/k3/root-verification-before-ui-fixture-fix.json). The failing library is excluded from accepted counts. |
| `cargo test -p photocraft-ui-egui -p photocraft -p xtask --lib --offline --no-fail-fast` | Final full UI library: 4,902 passed, 0 failed, 21 existing ignores; 330.34 s including compile. Combined with unaffected targets above: **5,408 passed, 0 unresolved failures, 25 existing ignores**, without double counting. [Accepted counts](../../log/k3/accepted-ui-app-test-counts.json), [final ledger](../../log/k3/post-fix-verification.json). |
| `cargo clippy -p photocraft-engine -p photocraft-ui-egui -p photocraft --all-targets --offline -- -D warnings` | Passed; 144.97 s. [Final root ledger](../../log/k3/post-fix-verification.json). |
| `cargo clippy -p photocraft-room-client --all-targets --offline -- -D warnings` | Passed; [client ledger](../../log/k3/corpse-verification.json). |
| `cargo clippy -p photocraft-sync --all-targets --offline -- -D warnings` | Passed; [service ledger](../../log/k3/review-service-verification.json). |
| `cargo xtask wasm` | Passed 27 crates plus optional HEIF check; 373.35 s, two jobs. [WASM record](../../log/k3/animation-wasm-result.json). |
| `cargo check -p photocraft-ui-egui --target wasm32-unknown-unknown --offline` | Final affected check after the two control-inspection IDs passed; 287.56 s. [Record](../../log/k3/corpse-final-ui-wasm-result.json). Existing `effective_locale` dead-code warning retained. |
| `cargo check -p photocraft-room-client --target wasm32-unknown-unknown --offline` | Passed; [client ledger](../../log/k3/corpse-verification.json). |
| `cargo xtask layers` | Passed, 35 crates, 25.05 s. [Structural ledger](../../log/k3/root-structure-verification.json). |
| `cargo xtask parity` | Passed, 660/660 retained; 264.21 s. Regenerated checklist; floor unchanged. Same structural ledger. |
| `cargo xtask scorecard` | Passed, 1.62 s. Existing 10/187 dead preferences unchanged; no new performance claim. Same structural ledger. |
| `cargo fmt --all --check` | Passed, 11.80 s. [Final root ledger](../../log/k3/post-fix-verification.json). |
| `cargo build -p photocraft --offline` | Final app passed, 148.11 s; retained/hash checked. [Final root ledger](../../log/k3/post-fix-verification.json). |
| `cargo build -p photocraft-sync --offline` | Passed; [service artifact and provenance](../../log/k3/review-service-artifact.json). |
| `cargo build -p photocraft-room-client --example cleanup_credentials --offline` | Passed; final helper used for all three native cleanup operations. [Client ledger](../../log/k3/corpse-verification.json). |
| `cargo build -p photocraft-ui-egui --example snapshot --offline` and `& ./log/k3/capture-final.ps1` | Passed; final snapshot build 129.45 s, four refreshed layouts byte-identical to the directly inspected PNGs. [Comparison](../../log/k3/snapshot-post-fix-comparison.json). |
| `cargo xtask perf --quick` | Passed, all nine benchmark cases `ok`, zero reported failures; 2,139.968 s including cold release compilation, four jobs. [Execution](../../log/k3/perf-verification.json), [independently checked report](../../log/k3/root-performance-review.json), [raw results](../../log/k3/perf-quick-results/results.json). Quick mode does not evaluate full-size budgets. |
| `& ./log/k3/verify-final-evidence.ps1` | Passed: independently checks all four game counts, each player's exact CPU/GPU grants, nine changed numeric values, recovery, previous-turn immutability, offline skip/resume, incomplete game and three credential cleanups. |
| `& ./log/k3/final-audit.ps1` / `git diff --check` | Passed after final handoff; 20 Markdown files, 302 local links resolved, no trailing whitespace or diff errors. [Final audit](../../log/k3/audit-final.log). Git emitted only LF/CRLF conversion notices. |

The coordinator independently hash-checked and ran all eight final client executables ([record](../../log/k3/root-final-client-recheck.json)), final engine room/privacy tests ([record](../../log/k3/root-engine-final-recheck.json)) and the real HTTP binary. Final source provenance matches all **2,405** app manifest entries ([record](../../log/k3/root-app-provenance-final.json)); service source hashes also match ([record](../../log/k3/review-service-provenance-final.json)). Full-corpus work was not rerun because K3 does not change format/io/psd/compose/gpu/text; K1's retained corpus evidence remains above. K3 exercises real native checkpoint/replay serialization and reopening through its focused engine/service tests.

### Artifacts and visual review

| Retained artifact | SHA256 |
|---|---|
| [Windows app](../../log/k3/bin/photocraft.exe) | `A81E1DB08D4CDDEFDBE403B6CE6D96F8B3901F26FE8A46870247252845E420D2` |
| [Private service](../../log/k3/bin/photocraft-sync.exe) | `F32C2789EA9F023BA46584A46D8E2FE8C19E77F71444F7534149DE095880F3A8` |
| [Credential cleanup helper](../../log/k3/bin/cleanup_credentials.exe) | `1C0ED6B961F0C77E792F297E94907B6712FFBD7028DAB1251C92ED014355ED2A` |
| [Offscreen renderer](../../log/k3/bin/snapshot.exe) | `A89F5DAC9872EA7C3F73FB4537C0BB6DBD4A6D86EFE7C527FDDE6863840ABE5C` |
| [Native harness](../../scripts/komaphone/k3-native-smoke.ps1) | `606CFEBC052EF2EBAF0B7DFF60EA537A3C20781FF6D026C17A8319258EAF3BFE` |

The independent reviewer viewed all **36 final native PNGs** ([hashes/dimensions/observations](../../log/k3/review-native-visual-inspection.json)); the coordinator directly viewed six representative final captures ([record](../../log/k3/root-native-visual-review.json)). No blocking layout defect remained. At 720 px height, lower notes/credits use ordinary vertical scrolling; primary controls and artwork remain accessible. Representative inspected images:

- [Animation copy/onion/note](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/client-1/animation-2-copy-onion-1.png) and [animation reveal controls](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/client-2/animation-2-reveal-controls.png).
- [Corpse seam at 1280 × 720](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/client-1/corpse-2-reference-1280.png) and [restored three-player creature](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/client-2/corpse-3-reveal-reopened.png).
- [Transfer restart](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/client-1/transfer-reconnect.png) and [incomplete timed reveal](../../log/k3/native-rooms-8f9665b7760f4005b7e3c2cb38e631f0/client-1/timed-incomplete-archive.png).
- Static [home](../../log/k3/home-1280.png), [1280 room entry](../../log/k3/room-entry-1280.png), [1920 room entry](../../log/k3/room-entry-1920.png), [2× high-DPI room entry](../../log/k3/room-entry-hidpi.png) were directly inspected and refreshed with the final renderer; native reference/reveal layouts also ran at both 1280 × 720 and 1920 × 1080.

### Corrections, operational assumptions and limits

Failed attempts were retained and excluded from acceptance. Sandbox MSVC generated-archive replacement/temp-file OS5 failures were retried with reviewed native compiler execution, using the identical offline commands. Real diagnostic findings were fixed: duplicate/covered controls, primary actions below notes, repeated-artwork upload ACK handling, durable note loss across assignment changes, cross-room note identity, previous-turn mutation and Resume after an explicit offline skip. The final client accepts only checked monotonic prefix ACKs and an exactly matching complete asset. The note archive is transactional and scoped by normalized endpoint, room and assignment.

Eight new headless UI test fixtures initially failed because pinned egui requires unconsumed `TexturesDelta` to be cleared. Test-only cleanup fixed them without weakening assertions; the entire affected library passed again. Provisional native runs are not acceptance evidence: the final harness waits for enabled controls/stable geometry, uses actual numeric-field interaction, waits for each peer's authorized phase/revision and requires exact cache equality. Missing Skip/End inspection IDs were registered before the final app build. No failed or skipped check was counted as passed.

Presentation safety ceilings are 36 images, 8 MiB per PNG, separate 256 MiB compressed and decoded caches, plus bounded GPU copies. These protect allocation; they are not typical memory measurements. The per-assignment note recovery table permits 1,024 assignments or 2 MiB, refuses growth at the limit and preserves existing acknowledged text. Explicit same-assignment clearing is allowed. This routine operational bound avoids unbounded storage without dropping saved notes; historical-note UI belongs to K4. Existing K2 records migrate/default missing fields, with ambiguous legacy note scope rejected without loss. Product baseline 1.1 is unchanged.

The app/service artifacts and evidence are local development outputs under gitignored `log/`; the tracked source, harness, runbook and this ledger reproduce the work. No production hosting is selected. Cross-machine play, polished per-drawing replay/gallery/exports/archives and required local remix remain K4. Deep zoom is K5; shared-world remix, tours/export and catch-up are K6. No K3 external gate is deferred. All agent caches are removed after exact workspace-path, no-reparse-point, native-process and retained-hash checks. Root removed 19,946 generated files / 18,390,509,408 bytes and verified 46 retained hashes unchanged; all three native credential cleanups passed and no test process remains. Toolchain and corpus caches are retained. The next run should follow the [K4 agent prompt](agent-prompts.md#k4-replay-gallery-and-party-release) after checking this final artifact/evidence ledger.

### K3 final closure and performance limits

The final quick-performance report covers all nine configured benchmark cases: filters, multiband, bevel, camera raw, cursor, layout, GPU editing scenarios, CPU editing scenarios and text. All returned `ok`; failures are empty. It records 65 tracked quick scenarios, ten not run in quick mode and four inherited not-measurable scenarios. The latter are not counted as passes. Machine: Windows x86-64, 16 logical cores, `AMD64 Family 25 Model 117 Stepping 2`, NVIDIA GeForce RTX 5060 Ti, Windows 10.0.26200.9711. The checked-in baseline is macOS/Apple M4 Pro, so the automatic cross-machine regression comparison was skipped. Full-size performance budgets do not apply to quick mode. Windows load averages are unavailable; reported `contended:false` does not prove an idle machine. Layout output retains its >16 ms observations (for example, text-move release median 154.48 ms); this is not a claim that every editor operation meets 16 ms. No unrelated Photoshop parity/performance work was added.

Independent review verified all nine raw reports parse and the retained aggregate SHA256 matches. The root [cleanup record](../../log/k3/root-cache-cleanup-result.json) and [process audit](../../log/k3/root-final-process-audit.json) show no remaining agent cache or test process. After cleanup, `& ./log/k3/capture-final.ps1` again passed using the retained executable, with all four PNGs byte-identical to the inspected images ([comparison](../../log/k3/snapshot-after-cleanup-comparison.json)). Runnable binaries, test executables, screenshots, full logs and raw benchmark reports remain under `log/k3`; no user data was deleted.

K3 is complete. Follow the [play instructions](k3-party-modes.md) to try the local build, or rerun `& ./scripts/komaphone/k3-native-smoke.ps1` for isolated acceptance. The next implementation run should read the K4 prompt and preserve its required local remix and separate-machine release gate. No K4 or world implementation was started in this run.
## GitHub handoff 2026-11-05

Owner authorized committing and pushing the completed K0–K3 checkout to origin (Teddy-ing/komaphone), main. This publication includes source, tests, tracked acceptance harnesses, runbooks and this evidence ledger. Gitignored local binaries, screenshots, raw logs, credentials, caches and corpus remain local; linked log artifacts are not included in Git. The K3 completion evidence above is unchanged. No source changes or new deployment accompany this handoff.
