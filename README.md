# Komaphone

Gartic Phone with the boys, but with Photoshop-style image editing.

Komaphone is a new party-game project built on [PhotoCraft](https://github.com/storytold/photocraft), the Rust image editor by the ArtCraft team and PhotoCraft contributors. This is an independent project, not an official ArtCraft product.

## Planned game loop

1. Create a private room and invite friends with a room code.
2. Everyone writes a starting prompt.
3. Pass the prompts around; each player creates an image using layers, brushes, cutouts, text, and effects.
4. Pass the images around; the next player guesses what they depict.
5. Alternate editing and guessing, then reveal each chain together.

## Current status

This repository contains the imported PhotoCraft editor and its Git history. Rooms, multiplayer networking, round timers, prompt rotation, submissions, and the reveal screen are planned and are **not implemented yet**. The editor still uses its upstream PhotoCraft application and crate names.

## Development

The existing editor uses Rust, egui/eframe, and wgpu, with a WebAssembly browser target. Start with [the development guide](docs/development.md) and [architecture](docs/architecture.md).

For Windows, install Rust stable 1.95+ and the Microsoft C++ Build Tools with the Desktop development with C++ workload, then run:

```sh
cargo run --release -p photocraft
```

## Repository setup

- `origin`: https://github.com/Teddy-ing/komaphone.git
- `upstream`: https://github.com/storytold/photocraft.git
- Imported upstream commit: `d7d138b1e50b6e2363d613d711e1a45830d330b8`

The next milestone is a playable private-room loop around the editor: join, prompt, edit, guess, and reveal.

## Attribution and license

Based on PhotoCraft by the ArtCraft team and PhotoCraft contributors. The imported source is dual-licensed under MIT OR Apache-2.0; retain [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), [NOTICE](NOTICE), and [asset attribution](ATTRIBUTION.md).

ArtCraft brand images have been removed from this project's working tree; they are not covered by the source-code license. Historical upstream commits retain their original contents. See the retained [brand license](docs/brand/LICENSE-brand.txt).
