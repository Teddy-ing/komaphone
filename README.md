# Komaphone

Private drawing games with friends, useful Photoshop-style tools, and work that survives closing the app.

Komaphone is a new party-game project built on [PhotoCraft](https://github.com/storytold/photocraft), the Rust image editor by the ArtCraft team and PhotoCraft contributors. This is an independent project, not an official ArtCraft product.

## Planned experience

1. Open a simple, mouse-first Windows drawing workspace with brushes, layers, undo, pan, and zoom.
2. Join a private room with two or three players.
3. Play **Animation**: start separate animations, pass frames around with a previous-frame reference and handoff notes, then reveal and export the results.
4. Play **Exquisite Corpse**: draw heads, bodies, and legs while seeing only the joining strip from the preceding section, then reveal the assembled creatures.
5. Keep drawings, frame replays, and finished games in a recoverable gallery.
6. Later, build a persistent shared world with simultaneous drawing, ghost cursors, asynchronous updates, and nested deep zoom.
7. Remix finished creations without changing originals, replay friends' additions since your last visit, and save/export guided zoom tours through the shared world.

The agreed baseline is **Windows first**, **a small private sync service**, and **party modes before the shared world**. The earlier classic prompt/edit/guess proposal is deferred.

Start with the [product specification](docs/komaphone/product-spec.md), [delivery plan](docs/komaphone/delivery-plan.md), and [copyable agent prompts](docs/komaphone/agent-prompts.md). [Implementation status](docs/komaphone/status.md) separates planned work from verified behavior.

## Current status

The native app opens the Komaphone workspace with permanent local drafts, automatic saves, recovery, and durable replay recording. Private local rooms support invitations, two/three-player lobbies, accepted submissions, reconnect, host takeover and timers through a separate Rust service. Animation now includes previous-frame copy, adjustable onion skin, handoff notes and full-chain playback. Exquisite Corpse includes locked seam references and assembled three-part reveals. Full two/three-player games passed native UI and headless acceptance. The Advanced workspace retains the imported PhotoCraft editor. Use the [local drawing guide](docs/komaphone/k1-local-drafts.md) or [party-mode runbook](docs/komaphone/k3-party-modes.md); the [status ledger](docs/komaphone/status.md) records milestone gates, verified evidence and limitations.

The current service is loopback-only, for isolated clients on one Windows computer. The drawing replay viewer/gallery, exports, local remix and a separate-machine friend-ready release are K4. The deep-zoom shared world and its required remix/tour/catch-up features follow later. Existing animation and GIF primitives remain available in Advanced. Machine names stay `photocraft-*`. Komaphone has no automatic update channel yet and does not offer upstream PhotoCraft releases.

## Development

The existing editor uses Rust, egui/eframe, and wgpu, with a WebAssembly browser target. Start with [the development guide](docs/development.md) and [architecture](docs/architecture.md).

For Windows, install Rust stable 1.95+ and the Microsoft C++ Build Tools with the Desktop development with C++ workload, then run:

```sh
cargo run --release -p photocraft
```

<details>
<summary>Retained Advanced editor tools</summary>

The Advanced workspace retains the upstream editor's tool inventory. The compact
Komaphone workspace presents the essential drawing tools described in the product baseline.

<h4>🧰 68 tools</h4>
Move · Artboard · Rectangular and Elliptical Marquee · Single Row Marquee · Single Column Marquee · Lasso · Polygonal Lasso · Magnetic Lasso · Object Selection · Quick Selection · Magic Wand · Crop · Perspective Crop · Slice · Slice Select · Frame · Eyedropper · Color Sampler · Ruler · Note · Count · Remove · Spot Healing Brush · Healing Brush · Patch · Content-Aware Move · Red Eye · Brush · Pencil · Mixer Brush · Color Replacement · Clone Stamp · Pattern Stamp · History Brush · Art History Brush · Eraser · Background Eraser · Magic Eraser · Gradient · Paint Bucket · Blur · Sharpen · Smudge · Dodge · Burn · Sponge · Pen · Freeform Pen · Curvature Pen · Add Anchor Point · Delete Anchor Point · Convert Point · Horizontal and Vertical Type · Horizontal and Vertical Type Mask · Path Selection · Direct Selection · Rectangle · Ellipse · Triangle · Polygon · Line · Custom Shape · Hand · Rotate View · Zoom

</details>

## Repository setup

- `origin`: https://github.com/Teddy-ing/komaphone.git
- `upstream`: https://github.com/storytold/photocraft.git
- Imported upstream commit: `d7d138b1e50b6e2363d613d711e1a45830d330b8`

The first delivery track establishes reliable local drawing, then private rooms, Animation, Exquisite Corpse, replay, and gallery. The shared world follows a separate deep-zoom prototype gate.

## Attribution and license

Based on PhotoCraft by the ArtCraft team and PhotoCraft contributors. The imported source is dual-licensed under MIT OR Apache-2.0; retain [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), [NOTICE](NOTICE), and [asset attribution](ATTRIBUTION.md).

ArtCraft brand images have been removed from this project's working tree; they are not covered by the source-code license. Historical upstream commits retain their original contents. See the retained [brand license](docs/brand/LICENSE-brand.txt).
