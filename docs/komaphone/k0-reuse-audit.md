# K0 editor reuse audit

Inspected 2026-11-05 against `69a9aacb566e036264a93ed6da925cf96eeda0bf` plus the existing planning changes. This is a source audit and runnable validation recipe, not a runtime pass. Actual commands, toolchain failures, results, and inspected artifacts belong in [status.md](status.md). No editor implementation was changed by this audit.

## Reuse decisions

| Primitive | Existing implementation and evidence to exercise | K1/K4 boundary |
|---|---|---|
| Mouse drawing | `crates/paint/src/lib.rs`: `StrokePoint` defaults to pressure 1 with no tilt/rotation, retains millisecond timing; seeded brush dynamics. `crates/engine/src/cmd/brush_cmds.rs`: `paint.stroke`, `paint.pencil`, live stroke commit, coordinate and brush validation. | Keep existing command IDs and depth/color-general APIs. A portable Actions journal is not the durable canonical replay stream. Record resolved targets/settings/assets and outcome before journal coalescing/portability rewrites. |
| Live mouse preview and commit | `crates/ui-egui/tests/it/live_stroke_canvas.rs`: synthetic pointer down/move/up through the actual egui application, across zoom/smoothing/8-/16-bit cases and CPU/GPU canvas paths. Checks the stroke appears before release and release changes at most 1/255 per channel. `brush_cmds::tests::a_commit_takes_the_live_strokes_pixels_and_they_equal_a_fresh_render` compares live commit with seeded fresh render and undo. | Reuse input and rendering routes; a passing engine test alone does not establish mouse UI usability or latency on the 1536 x 1024, 10-layer Komaphone fixture. |
| Native autosave/recovery | `crates/format/src/autosave.rs`: asynchronous coalesced saves, `request_checked`, completed revision outcomes, `RecoveryStore::flush`, recovery entry adoption and incremental `.pcraft` bundles. `apps/photocraft/src/services.rs` injects these effects into the UI. | A queued save is not acknowledgment. Consume successful completion for the exact local revision. Recovery entries are discarded on explicit save/close and therefore cannot be the permanent Komaphone project/gallery store. |
| Stored animation frames | `crates/doc/src/video.rs`, `crates/format/src/{manifest,convert}.rs`; `roundtrip::video_layer_frames_survive_roundtrip` asserts three frames, original-frame copies, fps, source metadata, timeline duration and pixel content survive a native round trip. `video_cmds::tests::edits_to_a_frame_survive_scrubbing_and_render` checks edited frame independence and PNG output. | Reuse surfaces/timeline/native serialization. This is not party scheduling, assignment privacy, per-frame authorship, immutable submission, or a replay timeline. Do not rebuild the existing frame persistence based on stale parity comments. |
| GIF and PNG sequence export | `crates/engine/src/cmd/video_cmds.rs`: `file.export.renderVideo` and `render_video_with` render saved frame surfaces through the current compositor; caller-owned writer and cancellation/progress callbacks allow later authorized destinations. | Reuse the encoder/writer seam in K4, add game-specific selection/privacy/metadata and explicit loop/timing controls. GIF quantizes to a per-frame palette, thresholds alpha below 0.5 as transparent, rounds delays to centiseconds, and currently always requests looping. Original lossless frames must remain archived. |

## Focused baseline commands

Run from the repository root in PowerShell, with the coordinator's isolated target directory. Rust MSVC 1.95 or newer and Visual Studio C++ Build Tools are the documented Windows prerequisites; `wasm32-unknown-unknown` is also needed for the mandatory WASM gate. Do not run simultaneous whole-app builds. These filters select existing tests rather than introducing a second test harness.

```powershell
$env:CARGO_TARGET_DIR = 'target/agent-k0'
cargo test -p photocraft-paint --lib
cargo test -p photocraft-engine --lib brush_cmds::tests::
cargo test -p photocraft-format --test it autosave::
cargo test -p photocraft-format --test it video_layer_frames_survive_roundtrip
cargo test -p photocraft-engine --lib video_cmds::tests::
cargo test -p photocraft-ui-egui --test it live_stroke_canvas:: -- --nocapture
```

The paint suite contains hard brush, erase, selection, opacity, smoothing and all-depth checks. The autosave suite covers completed save/recover equality, a failed write followed by recovery, queue coalescing, retained entries across a second recovery, flushing all queued revisions, entry isolation and sidecar path constraints. These are useful K0 primitive tests; they do not replace the K1 process-kill-after-ack/during-write and disk-full acceptance tests.

The complete `video_cmds::tests::` filter includes:

- `a_gif_rendered_by_render_video_loads_back_frame_for_frame`: decoder-based two-frame order/color/fps round trip.
- `animated_gif_keeps_transparency` and `a_fully_transparent_animated_gif_frame_encodes`.
- `animated_gif_delay_follows_timelines_under_one_fps`.
- `edits_to_a_frame_survive_scrubbing_and_render`.
- `render_writer_progress_and_cancellation_leave_session_unchanged`.
- Loaded timeline/frame-staging bounds and malformed footage rejection.

Prefer these to the older `render_video_to_animated_gif` check alone, whose frame count assertion scans for the image-descriptor byte rather than decoding the animation. The existing decoder-based round trip is stronger evidence.

The live-canvas tests catch renderer initialization failure, print `skipping: no GPU adapter`, and return successfully. Read `--nocapture` output: that result is **unverified**, not a visual or GPU pass. CPU fallback also prints a note; record the actual route. Synthetic events cover the application input path but do not establish physical mouse hardware latency.

If desktop service-boundary verification is needed after compiling the app:

```powershell
cargo test -p photocraft recovery_tests:: -- --nocapture
```

`apps/photocraft/src/services/recovery_tests.rs` tests deferred recovery of pixels/metadata, retained snapshots after corrupt tile decode, and explicit discard. It uses isolated generated temporary fixtures.

## Offscreen visual route

`crates/ui-egui/examples/snapshot.rs` renders the real app with egui/wgpu without opening a window or stealing focus. Its script is the existing control protocol's `[method, params]` array; it can drive `ui.pointer` after setting a tool. Use generated geometry, not personal photos. A minimal editor baseline image is:

```powershell
$script = '[["engine.execute",{"command":"file.new","params":{"width":1536,"height":1024,"background":"white"}}],["engine.execute",{"command":"paint.stroke","params":{"points":[[200,500],[600,200],[1000,500]],"size":36,"hardness":1,"color":"#204080"}}],["ui.set",{"tool":"brush"}]]'
cargo run -p photocraft-ui-egui --example snapshot -- --out log/k0-editor-1280.png --size 1280x720 --scale 1 --custom-titlebar --script $script
cargo run -p photocraft-ui-egui --example snapshot -- --out log/k0-editor-1920.png --size 1920x1080 --scale 1 --custom-titlebar --script $script
cargo run -p photocraft-ui-egui --example snapshot -- --out log/k0-editor-hidpi.png --size 1280x720 --scale 2 --custom-titlebar --script $script
```

Create `log` first if absent. The high-DPI example produces a 2560 x 1440 physical image from a 1280 x 720 point window. Inspect actual saved PNGs, and check stderr for script failures: the example reports a failed control command but can continue to render a screenshot. `--safe-gpu` uses the CPU canvas compositor while still requiring a functioning wgpu window/offscreen adapter. It is not a GPU-free renderer.

For native-window fallback, [the control protocol](../control-protocol.md) documents authenticated loopback `--control`, isolated automation read/write roots, `ui.pointer`, and `ui.screenshot`. That privileged editor control endpoint is not a multiplayer transport. Separate-instance testing must also respect the documented single-instance behavior; do not interfere with an existing user instance.

## Integration and remaining risks

- Keep local project/replay persistence owned independently of the curated UI. Introduce a completion result that names a project, revision and durable replay endpoint; make saved/sync state independent. Do not relabel upstream temporary recovery as the permanent store.
- Capture replay before Actions rewrites/coalesces parameters. Resolve brush seed, targets, selection and referenced assets. Unsupported pixel changes need persisted checkpoint transitions; undo/redo are visible events. `StrokePoint.time` alone is insufficient for retained replay.
- GIF export stages quantized frames in memory. Existing per-frame and duration bounds are useful but K4 must apply the smaller game contract limits and validate total output resources, frame order, loop, approximately 6 fps default timing, transparency and cancellation.
- Existing `.pcraft` structures and editor command authorization need explicit Komaphone project/assignment boundaries; an advanced workspace must not fetch or inspect hidden references. No new room network or privacy behavior was found by this audit.
- K0 source reuse does not close K1 workspace/restart tests, K3 privacy/assembly, K4 replay/remix/archive, or later world acceptance. The baseline remains version 1.1 with those requirements at their assigned gates.

Ownership for this audit: `reuse_audit` owns only this file; coordinator owns contracts, registration, validation, status and devlog. No Cargo target directory or process was created by the audit agent.
