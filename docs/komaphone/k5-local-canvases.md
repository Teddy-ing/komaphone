# K5 local canvases and deep zoom

This document describes the native K5 prototype under [product baseline 1.3](product-spec.md) and the [delivery plan](delivery-plan.md). It adds independent local canvases and direct drawing at successive scales to the Windows Rust/egui/wgpu app. The existing party documents, finite editor, attribution, and two/three-player roster remain separate.

**Acceptance status: passed, including the stroke-quality follow-up.** The latest build passed 5,183 affected-crate tests, 209 independent native checks including 34 input-integrity checks, and inspection of 28 native acceptance images plus the old/new stroke comparison. Strict Clippy, WASM, accessibility and required quick-performance commands pass. See [status](status.md) for exact evidence, benchmark limitations, preserved failures and the separate K6 handoff.

## Try the implementation

The verified local Windows preview and synthetic four-canvas demo are ready in this checkout. From the repository root, run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy RemoteSigned -File .\log\k5-lines\Start-Komaphone.ps1 -Demo
```

Close the older preview first. Choose **Canvases**, select **Nested houses**, and use its **Depth 0–6** or **Lateral** bookmarks. **Auto zoom** travels to the selected bookmark; draw directly after moving or zooming. The helper verifies app/demo hashes and reuses `%LOCALAPPDATA%\Komaphone\K5-preview\demo-profile`, preserving existing drawings. Omit `-Demo` for the separate `manual-profile`. Use `-StrokeDemo` instead to open a separate `stroke-quality-profile`; choose **Canvases → Round brush comparison** to inspect the synthetic zoomed strokes. Seeds are copied only when the destination database is absent. This changes execution policy only for that PowerShell process. [Latest manifest](../../log/k5-lines/preview.json), [launcher](../../log/k5-lines/Start-Komaphone.ps1), [verification](../../log/k5-lines/final-launcher-verification.json), [independent preservation checks](../../log/k5-lines/launcher-review/README.md). These retained local artifacts are gitignored; this is not an installer or another-PC release.

The updated brush removes square miter protrusions with round joins/caps and screen-adaptive antialiasing. Existing stored points remain unchanged. New gestures retain intermediate mouse moves and the release endpoint; natural hand wobble remains, with no trailing stabilizer. [Research and public sources](stroke-rendering-research.md), [before](../../log/k5/native-f935a581040642c383770ce32220ea58/stroke-zoom-16.png), [after](../../log/k5/native-d7d54ed7ccef43c28fec0c3ab798e317/stroke-zoom-16.png).

Use the repository's Windows Rust/MSVC setup from [development](../development.md). A dedicated PowerShell session with separate local data/config directories keeps this experiment apart from existing party work:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $PWD 'target/agent-k5-manual'
$env:KOMAPHONE_DATA_DIR = Join-Path $env:LOCALAPPDATA 'Komaphone/K5-source/data'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $env:LOCALAPPDATA 'Komaphone/K5-source/config'
cargo run -p photocraft -- --new-instance
```

This is a source-build recipe, not evidence that the command passed on another machine. Reuse the same data/config paths to reopen the same canvases. Close the app normally after it says **Saved locally**. A fresh data directory creates a fresh local owner identity.

1. Choose **Canvases** from Home or the drawing workspace. Enter a name and choose **Create canvas**. Repeat for at least four canvases as an isolation fixture, not a product limit; two may share a display name. The short ID shown beneath each name distinguishes them.
2. Draw with **Brush**. Roll the wheel to zoom at the pointer, then draw immediately at the new scale. Middle-drag, Space-drag, or **Pan** moves across the same canvas. Internal storage regions need no manual creation or opening.
3. New strokes normally begin at the selected screen width. **Lock thickness** preserves that width in canvas coordinates as you zoom; existing strokes retain their original width. A lock that exceeds the next scale's stroke limit produces an error rather than silently changing thickness.
4. Use **Eraser**, **Undo**, and **Redo**. Each canvas has its own history. Name a location and choose **Save this view**; **Go** restores it directly, while **Auto zoom** travels to it with **Pause**, **Resume**, and **Cancel**. **Home** returns to the origin at root scale.
5. Rename and switch canvases, wait for **Saved locally**, close, and reopen with the same paths. Artwork, names, bookmarks, camera, history and active selection persist independently. **Party & drawings** returns to the existing finite workspace.

The default database is `%LOCALAPPDATA%\Komaphone\canvases.sqlite3`; `KOMAPHONE_DATA_DIR` changes its parent directory. It is separate from `drafts.sqlite3` and `rooms.sqlite3`. There is no K5 canvas service connection, member list, invitation, export, or archive UI. Do not copy a live database as a backup; after clean shutdown preserve the complete canvas database and any surviving SQLite recovery journal before diagnosis.

## Implementation and ownership boundaries

| Area | Source | Responsibility |
|---|---|---|
| L1 scene contracts and math | [scene](../../crates/scene/src/lib.rs), [coordinates](../../crates/scene/src/coordinates.rs), [projection](../../crates/scene/src/render.rs) | Validated identities, exact positions/cameras, spatial envelopes, bounded retained-stroke projection. No UI or filesystem dependency. |
| Native local store | [canvas adapter](../../crates/local-store/src/canvas/native.rs) | Separate SQLite database, transaction boundaries, checksums, history, viewport-indexed reads. Filesystem/SQLite implementation is excluded on WASM. |
| Engine command and worker seam | [commands](../../crates/engine/src/cmd/komaphone_canvas_cmds.rs), [coordination](../../crates/engine/src/canvases.rs), [worker](../../crates/engine/src/canvases/native.rs) | Active-canvas validation, bounded asynchronous writes, committed receipts, pending strokes and navigation. |
| Native workspace | [canvas UI](../../crates/ui-egui/src/komaphone_canvas.rs) | Mouse interaction, preview rendering and visible save/errors; mutations use engine commands. |

All canvas command IDs use `komaphone.canvas.*`: `status`, `list`, `view`, `create`, `open`, `rename`, `camera`, `stroke`, `home`, `bookmark`, `go`, `flight`, `undo`, `redo`, and `retry`. `status` reports pending/durable state and viewport counters; `view` exposes hashes of the currently loaded canonical strokes only when camera/revision match. Enqueue responses explicitly say `saved:false`. They are not durable acknowledgments.

The worker serializes changes. A successful mutation produces a committed `CanvasInfo` receipt separately from the subsequent list refresh. Failure to refresh unrelated metadata cannot turn an accepted Create/Undo/Redo into a retryable mutation and repeat it. Failed uncommitted actions remain queued for explicit Retry. Selection waits for pending edits and cancels automatic camera flight before the next tick. Unsaved gesture previews and failed-enqueue drafts remain visibly distinct from committed artwork.

Normal window close checks canvas work separately from finite-document save prompts. An active stroke becomes a retained draft; **Keep drawing** returns to it, **Retry saving** uses the existing engine command, and **Close without saving** explicitly accepts loss of unacknowledged canvas work. That answer does not discard finite documents. New accepted canvas mutations, a finite-document quit prompt, or cancellation invalidate an earlier canvas discard decision. As in the existing finite editor, an unrecoverable GPU-window loss permits exit when no dialog can be displayed; only acknowledged data is guaranteed after that failure.

## Coordinate and rendering choices

Each canvas has a random stable ID and a stable local owner ID. Its `RegionKey` is `(depth, x, y)` within that canvas. Parent coordinates derive by Euclidean division, including negative positions; no mutable parent graph can form cycles. A region spans 1024 local units and has 100 subdivisions per axis at the next depth. Depths 0 through 12 are supported. Six successive 100× steps are the required acceptance fixture, not a six-level product cap.

Coordinates use signed 128-bit fixed-point ticks. At depth `d`, a region spans `1024 × 1024 × 100^(12-d)` ticks. Authored point positions quantize to 1/1024 of a local unit at their authored depth. The world permits root-region positions from -1,000,000 through 999,999 on each axis, subject to complete stroke bounds. Decimal strings preserve wide integer values through JSON automation. Finer strokes add genuinely new detail; magnifying old point coordinates cannot create detail that was never authored.

Cameras carry canvas identity, region identity, exact local tick offsets, and bounded screen zoom. Zoom normalizes across depths automatically: root zoom is at least 0.5; deeper local zoom is at least 1; all local zoom remains below 100. The largest accepted view therefore touches at most 17 × 17 root cells. Drawing chooses the pointer's region automatically and freezes the stroke's anchor and width. A stroke may cross region boundaries and is stored once, with multiple index references.

Rendering subtracts the exact camera origin before floating-point display conversion. Ordinary strokes use clipped paths and [round brush meshes](../../crates/scene/src/round.rs). Display-only approximation permits 0.125 physical pixel of centerline error plus 0.125 pixel of arc error before antialiasing and floating-point rounding; every zoom derives from original samples. The private simplifier caps distance evaluations at 32 × N and retains complete source intervals on exhaustion. Huge ancestor widths retain the original clipped capsule polygons, and extremely long thin segments retain double-double clipping. Camera flights reach exact endpoints. Precision is finite and explicit; the prototype does not promise mathematical infinity or full Endless Paper rendering parity.

| Strategy evaluated | Benefit | Cost and K5 decision |
|---|---|---|
| Retained strokes with small display summaries | Newly authored detail remains independent of a coarse bitmap. Stable stroke IDs/order support original-data recovery and future operation history. | Selected for K5's opaque freehand marks. Projection and spatial indexes need explicit bounds; complex brush textures, transparent compositing and image assets are not provided by this path. |
| Editable raster regions | Reuses mature finite-editor paint/compositing behavior and handles raster imports naturally. | Each region needs resolution, mip, seam, cache and invalidation rules; simply magnifying a finite bitmap fails direct at-scale drawing. Not implemented as the K5 canonical scene. |
| Hybrid retained originals with raster preview tiles | Could cache expensive coarse artwork while retaining editable detail and finite images. | Adds cache versioning, rasterization/invalidation and mixed color/alpha semantics. It remains a possible K6 extension requiring measured benefit and explicit migration; no hybrid-tile implementation is claimed. |

K5 summaries are decimated retained strokes, not raster tiles. Each summary has at most 24 samples and is used only when the entire original stroke extent, including width, projects to at most two pixels. Full payloads remain authoritative and are used again when zooming in. Opaque sRGBA colors are explicit, and erasing paints the opaque white paper in chronological order. Nonopaque input is rejected. The finite editor retains its existing depth/profile support; any future scene asset conversion must go through CMS rather than treating imported pixels as generic sRGB.

Images imported through the existing editor remain finite raster assets with their source pixel resolution. K5 does not import them into the canvas or turn them into unlimited detail. World image/animation insertion and world remix remain K6 work.

## Storage, viewport loading, and limits

The experimental database uses schema version 1, application ID `0x4b434e56`, SQLite DELETE journaling, synchronous EXTRA, foreign keys, and a checked page quota. Foreign, unversioned nonempty, and future-version stores are rejected before schema mutation. Metadata, original strokes, summaries, and summary-selection index metadata have bounded reads and SHA256 validation.

Create/switch atomically persist the outgoing camera plus the new active selection. Stroke insertion, spatial indexes and history counters commit together. An identical stroke ID with the identical original request is an idempotent retry; conflicting content is rejected. Undo/redo changes active/redoable flags without deleting original payloads. Drawing after undo preserves the abandoned redo branch. Canvas metadata revision can advance for naming/camera/bookmarks while stroke ordering retains its immutable insertion revision.

Every storage lookup includes canvas identity. A viewport read holds one SQLite read transaction, queries visible indexed cells and coarser authored marks, deduplicates stroke IDs, then reads only the selected original or summary payloads in revision order. The query level becomes finer when its visible cells fit the budget, avoiding unrelated fine descendants that merely share a coarse ancestor. It does not deserialize a whole scene to cull it. Returned counters include queried cells, candidate index rows, original/summary strokes, bytes and samples. Tests corrupt offscreen payloads, including descendants inside a common parent, and confirm they are not fetched.

| Bound | Current value |
|---|---:|
| Canvas names | 1–96 UTF-8 bytes, nonblank, no control characters |
| Local canvases / bookmarks per canvas | 1024 / 256 |
| Retained strokes per canvas | 1,000,000, also limited by database quota |
| Depth | 0–12, 100× finer per level |
| Stroke samples / absolute local sample coordinate | 8192 / 8192 units |
| Stroke diameter | Greater than zero, at most 2048 local units |
| Original or summary payload / canvas metadata | 1 MiB / 128 KiB |
| Maximum viewport side | 8192 pixels |
| Spatial cells per queried depth / returned index candidates | 512 / 65,536 |
| Loaded logical strokes / samples / payload bytes per view | 4096 / 131,072 / 16 MiB |
| Aggregate generated round-mesh vertices plus legacy fill-polygon points per UI frame | 2,097,152 |
| Summary samples / projected extent threshold | 24 / at most 2 pixels |
| SQLite page cache / default canvas database quota | 256 KiB / 2 GiB |
| Pending engine actions / pending strokes | 32 / 32 |

These are upper safety bounds, not promised capacities at every density or frame rate. Source samples remain capped separately from generated display geometry. Legacy fill polygons still undergo egui tessellation, so the geometry count is not a universal GPU-memory bound. Quota exhaustion and overly dense views return actionable errors without deleting saved artwork. An aggregate projection failure stops further rendering with a visible warning; a partial view is not complete acceptance evidence. A stroke reaching its sample/extent limit can save its valid prefix while retaining the visible limit warning.

Recovery proves complete acknowledged transactions survive process termination. Unacknowledged input can be lost. Corrupt payload/hash/version tests preserve other saved state and report failure; they do not repair corrupt canonical content. A corrupt metadata row can prevent the startup canvas list from loading until repaired, even though other canvases remain independently readable through the adapter. There is no in-app salvage picker in K5.

## Verified scene and storage evidence

The following ledgers describe original K5 acceptance. The later stroke-quality changes and their current 37 scene / 5,146 UI test results are recorded separately in [status](status.md#k5-stroke-quality-follow-up--2026-11-08-complete).

The final scene and storage ledgers were independently rehashed before and after rerunning their retained test binaries: all 31 recorded input entries matched, with no source changes during that audit. See [independent audit](../../log/k5/independent-evidence-audit.json), [scene ledger](../../log/k5/scene-verification.json), [store ledger](../../log/k5/store-verification.json), and [store counts](../../log/k5/store-counts.json). These local artifacts are gitignored; this tracked document records their checks and limitations rather than treating the log directory as the only handoff.

| Exact command | Result |
|---|---|
| `cargo test -p photocraft-scene --offline -- --nocapture` | Exit 0; 18 passed, 0 failed/ignored: coordinates 8, projection benchmark 1, rendering 9. |
| `cargo clippy -p photocraft-scene --all-targets --offline -- -D warnings` | Exit 0. |
| `cargo check -p photocraft-scene --target wasm32-unknown-unknown --offline` | Exit 0. |
| `cargo test -p photocraft-local-store --offline --no-fail-fast` | Exit 0; 55 passed, 0 failed/ignored; 22.8272608 seconds including build. |
| `cargo clippy -p photocraft-local-store --all-targets --offline -- -D warnings` | Exit 0; 3.2111195 seconds. |
| `cargo check -p photocraft-local-store --target wasm32-unknown-unknown --offline` | Exit 0; 1.6027738 seconds. |

Scene used `target/agent-k5-scene`; storage used `target/agent-k5-store`. The ledgers record Rust 1.95.0 MSVC, dev/test debug information disabled and incremental compilation disabled; storage used two build jobs. Existing archive tests initially hit sandbox access-denied errors. Their unchanged tests passed with reviewed native access on isolated synthetic data; the failed sandbox log remains retained.

The 20 canvas tests comprise 13 store cases plus a crash helper and six actual child-process kills: before and after commit for append, switch, and create. The before-append test verifies an on-disk rollback journal and actual database growth before killing the child; a retained run observed growth from 45,056 to 180,224 bytes. The after-commit case preserves all 8192 samples. Other cases cover four canvases with duplicate names, camera/bookmark/history isolation, idempotence, stale revisions, corruption, foreign identities, quotas, summary hashes, and offscreen loading.

The independent rerun executed these exact binaries with `--nocapture`, totaling **73 passed, 0 failed/ignored**; binary SHA256 values and individual commands are retained in the audit:

```text
target/agent-k5-scene/debug/deps/coordinates-9d8e061df5688aa7.exe       8
target/agent-k5-scene/debug/deps/projection_benchmark-92c5ad933c02847c.exe 1
target/agent-k5-scene/debug/deps/render-9952661f7e8d0821.exe            9
target/agent-k5-store/debug/deps/canvas_crash-0e4a018b2dc2e172.exe      7
target/agent-k5-store/debug/deps/canvas_store-0668609fb51bf775.exe     13
target/agent-k5-store/debug/deps/gallery_crash-64456ee73bcb0496.exe     3
target/agent-k5-store/debug/deps/gallery_store-7118341edc799bc5.exe    17
target/agent-k5-store/debug/deps/process_crash-675dcea03c33a8ae.exe     3
target/agent-k5-store/debug/deps/store-75b8567a23125f5e.exe            12
```

Final source SHA256 anchors are `DC0A5D568DEDA617559822902B5BABEEBABD218353FD37D3AC81A0A7D1227F42` for scene coordinates, `A07BABA7B351114C74197306D2956EB8796A7B78B4BB333DF08E26514DBCAF1B` for scene rendering, and `603E247D2EBA4E753553E32B1417C30791720147457B223764E0D2B0A9E63CF6` for the native canvas adapter. Recheck these if those files change.

The retained projection-only benchmark covers seven four-stroke houses at depths 0–6, six 100× steps, a 1536 × 1024 viewport, 20 warmups and 300 measurements per depth. The independent rerun measured p50 0.0216–0.0475 ms and p95 0.0292–0.0584 ms across depths. This excludes SQLite queries, egui tessellation, GPU submission/presentation and full app work. It is not a passing native frame-time result or proof of the 33 ms target. The raw output is [retained separately](../../log/k5/recheck-projection_benchmark-92c5ad933c02847c.log).

## Original K5 coordinator acceptance record

- **Final native app:** [candidate 2](../../log/k5/bin/candidate-2-photocraft.exe), SHA256 `CF770EADF4576073E8D8F216CA78B6CC3842523017DC73BC62B385188FFC1252`; [build record](../../log/k5/candidate-2.json), 2,683 stable compiled source/resource inputs. The matching snapshot executable is SHA256 `0F9FF1E953BE38DB4A418E05172A2A46EAE017BAF58E7000D6DB97E3B686A4BC`.
- **Actual native interaction/reopen:** `& ./scripts/komaphone/k5-native-smoke.ps1 -AppExe ./log/k5/bin/candidate-2-photocraft.exe` passed in52.10s with [209 independent checks](../../log/k5/native-701fec40f3cf4d1bb4727eaa0c250448/final-audit.json): four isolated canvases, root plus six100× levels, boundary crossings and far lateral drawing, eraser/undo, screen/locked brush width, pause/resume/cancel/exact automatic zoom, interrupted-close retry, and process-kill/switch recovery. [Raw result](../../log/k5/native-701fec40f3cf4d1bb4727eaa0c250448/result.json). The strengthened harness is SHA256 `12B9193B94978EFAFD9B011B5771AE59A95C0507D58BBE8A9DC2DF9824D0438E` and rejects unexpected retained input samples. Keep the earlier failed run `native-3275dbe0e037491fb879a4fa90a67904`: its root outline/door stored13/9 samples instead of6/4. Physical-input interleaving is supported by saved trajectories and the input path; raw provenance was not captured. No application source change was needed, and that failed visual run remains failed.
- **Visual inspection:** all14 initial/reopened depth images and six further views pass [independent inspection](../../log/k5/native-701fec40f3cf4d1bb4727eaa0c250448/independent-visual-review.json); the coordinator also inspected root, recovered depth-six, lateral and ancestor/detail images. Houses stay recognizable, the old magnified edge remains straight beside new detail, and1280/1920 controls are readable. The final-source [2× screenshot](../../log/k5/high-dpi-793d7457f72a4f38b2aab3d9aebde6d2/canvas-1280-2x.png) passed the default accessibility audit and independent inspection at2560×1440. The partial rectangle at the bottom-right of root views is the separate initial square fixture, not a displaced door.
- **Performance:** Windows11 Home10.0.26200, Ryzen7 8700F,33,995,153,408bytes RAM, RTX5060Ti/Vulkan NVIDIA610.88; dev optimized + debuginfo app. The final native workload uses1920×1080, eight warmup and64 measured pan/zoom rounds,582 consecutive frame intervals: p50 **5.0015ms**, p95 **5.2638ms**, max5.9513ms against33ms; separate CPU UI/tessellation p953.1079ms, peak working set333,467,648bytes. Final DB196,608bytes. The root view selects5 originals/17 summaries; depth-six selects22 originals/0 summaries,110 points/9,751payload bytes,28 queried cells/50 candidate rows. These small originals do not shrink further when summarized; the indexed-store tests separately prove offscreen payloads are not loaded. The unchanged K1 rendered-readback check passed p9544.31ms against50ms. Neither timing claims physical input-to-display latency or completed GPU execution. Other K4 app/service processes are recorded; no global host exclusivity is claimed.
- **Mandatory integration checks:** full engine3,575passes/22existingignores plus explicit adversarial `panic_hunt`1pass; app232passes/3existingignores; full UI5,137passes/22existingignores; xtask77passes, plus the73 scene/store tests above. All touched-crate strict Clippy, layers36crates, full WASM28crates plusHEIF with the final affected UI recheck, parity660/660, format and exact scorecard generation pass. Exact `cargo xtask perf --quick` passed all nine benchmarks in1,956.81s including normal builds, with2,685 unchanged inputs: [verification](../../log/k5/perf-quick-20261108T052245Z/verification.json), [report](../../log/k5/perf-quick-20261108T052245Z/reports/results.json). Quick mode does not apply full-workload budgets; automatic regression comparison is skipped because the committed baseline is macOS/M4, not this Windows/NVIDIA machine. No PSD/I/O/codecs/compose/GPU/text/native-format implementation changed, so no additional corpus run is required; prior evidence remains intact. The [independent gate audit](../../log/k5/final-gate-audit.json) records source correspondence and preserved failures.

## K6 handoff and preserved gaps

K5 identities and canvas-scoped targets are foundations, not authentication or friend authorization. K6 must distinguish the local profile, canvas owner, stroke author, authenticated writer, and per-canvas access rights through an explicit versioned migration. The local owner is not a service account, and knowledge of a canvas ID must never become access authority. Any user may own canvases with independent member lists; baseline 1.3 has no fixed four-member product cap. Any operational limits must be configurable and supported by evidence. K6 must add owner/editor membership, separate invitations and friend lists, authorization and revocation, author-owned layers, durable sync/deduplication, offline conflict/recovery handling, presence, and convergence. Party rosters stay at two/three players.

K6 also owns world remix, image/animation assets, selected-view/full-archive exports, multi-stop tours and their GIF/PNG-sequence exports, and catch-up playback with per-viewer reviewed revisions. Reuse exact camera targets, but persist/version routes and pin scene revisions for export. Current undo/redo retains original strokes and branch state without the chronology of each undo/redo action; it is not a complete catch-up event stream. Define an explicit versioned scene/replay baseline and migration before promoting this experimental database into a shared-world format, preserving originals on failure.

K4-LAUNCH-01 remains the owner's accepted unresolved laptop/startup/export-path limitation for K7. K5 does not retest or close it. No hosting, deployment, purchase, public publication, or external sharing is part of this local prototype.
