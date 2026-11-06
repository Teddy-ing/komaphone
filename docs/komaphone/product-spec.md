# Komaphone product baseline

Version 1.1, frozen 2026-11-04. This baseline defines the intended product, not implemented functionality. The owner explicitly chose Windows desktop first, a small private sync service, and Animation plus Exquisite Corpse before the shared canvas, then approved guided zoom tours, since-you-were-away playback, and remixing finished games as required features. Defaults below fill remaining details so agents can work without repeated product questions.

## Purpose

Komaphone lets a small group of friends draw, surprise one another, and keep what they make. It combines the approachable play of a drawing party game with useful editor tools, dependable saves, and an uncluttered interface for adults. The long-term shared canvas is a place to build an increasingly detailed world together, including when friends are online at different times.

Success means friends can install it, complete both party modes with a mouse, close and reopen without losing acknowledged saves, enjoy the reveals, and export their work. A later release adds a collaborative world with genuinely nested detail. Photoshop parity is not the product goal.

## Scope and defaults

| Decision | Baseline |
|---|---|
| Players | Two or three total in private rooms; one-person local drawing and mode preview for development. Larger groups are later work. |
| Platform | Windows desktop first; preserve the existing Rust WASM build. Browser multiplayer and browser crash recovery are not first-release requirements. |
| Input | Mouse and keyboard; no tablet needed, no pressure-dependent essential controls. |
| Network | Small private service for room authority and durable store-and-forward sync. Local drawing works offline. Direct peer-to-peer is an optional future optimization. |
| Identity | Local profile with display name and stable device identity; private invitation/pairing. No public accounts, discovery feed, or matchmaking. |
| Timing | Untimed rounds by default, Ready/Submit to advance; optional host-set timer and pause. Service enforces phase transitions. |
| Visual design | Calm, compact, readable UI using existing theme tokens; canvas gets most of the space. No childish decoration or mandatory avatars. |
| Delivery order | Reliable drawing, private rooms, both party modes, replay/gallery/export, then shared deep zoom. |
| Canvas defaults | Party drawing 1536 x 1024; corpse 1024 x 1536 with three 512 px bands. sRGB 8-bit defaults only; retain depth/color-general engine APIs. |
| Service operation | Build and test locally first. Provider, cost, and deployment are a later concrete decision, not permission to purchase hosting now. |

## Drawing workspace

**E1 Essential tools.** Brush, pencil, eraser, bucket fill, eyedropper, colors/palettes, basic shapes, selection/lasso, move/scale/rotate, simple text, and image import. Visible brush size, opacity, hardness where supported, smoothing, and presets. Include both thin and very broad brushes within tested resource limits. Brush size follows document coordinates and remains predictable while zooming.

**E2 Navigation.** Smooth cursor-centered wheel zoom, drag/space pan, fit view, reset zoom, and visible current tool. Useful keyboard shortcuts and discoverable buttons for undo/redo. Standard finite party canvases can be zoomed closely; the multiscale world is a separate capability.

**E3 Layers.** Add, rename, reorder, hide, duplicate, delete, lock, and change opacity. Show a compact layer list. Keep existing editing capabilities available through an Advanced workspace where practical; do not remove functioning subsystems just to simplify the primary screen.

**E4 Scope control.** No new AI generation, professional color/prepress workflows, vector authoring suite, RAW pipeline, plug-in ecosystem, or Photoshop-parity campaign. Simple shapes may reuse vector internals without exposing a complex vector workflow. Existing capabilities remain compatible.

**E5 Game constraints.** The party editor only edits the assigned frame/segment. Reference images, handoff notes, and seam strips are overlays or side panels, never silently baked into artwork. Advanced commands cannot bypass assignment boundaries or export hidden game content.

## Rooms and common game rules

**L1 Lobby.** Create/join privately, show participants and connection state, choose mode, set round options, ready up, and start. Joining needs a real capability/token, not a guessable short room number alone. Names are labels, not identity or authorization.

**L2 Flow.** Lobby -> active round -> submissions accepted -> next round -> reveal -> archived game. A submission is immutable once accepted. Repeated clicks/retries cannot create duplicates. Rejection leaves a recoverable local draft. A saved local draft and an accepted submission are different states.

**L3 Interruptions.** Disconnect preserves the assignment and draft. Reconnect restores the same player and turn. The default policy pauses progression for a missing player; the host can explicitly skip with a labeled placeholder or end and archive an incomplete game. A timed round pauses on disconnect in the initial release. On deadline, freeze each connected editor and submit its last locally durable work; the service allows a bounded transfer grace, then records explicit missing submissions. It never invents a completed drawing.

**L4 Host and membership.** Freeze the roster/order when a game starts. New arrivals wait for the next game. The host is a moderator, not the only copy of game state. On host disconnect, pause; another connected participant may claim moderation through one service-serialized transition. Service restart restores the room at its last committed transition. No hidden in-memory-only rounds.

**L5 Privacy.** Each participant receives only the assignment and reference material permitted for that phase. Never deliver newly unauthorized artwork through payloads, thumbnail caches, exports, automation inspection, or replay before reveal. The party UI must not surface forbidden prior content. A player may retain their own previously authorized drawings and recovery files; the design cannot erase that knowledge, especially when the two-player corpse rotation returns to the head author. Preserve those private drafts without exposing other players' work. This is private play among friends, not an adversarial tournament; the service administrator can access stored content in the initial design.

## Animation

**A1 Parallel chains.** With N players, create N separate animations. At round r (zero-based), chain owner i is edited by player (i + r) modulo N. Everyone draws one frame per round. Default length is two rotations, 2N frames per animation; the host may choose one to four rotations before starting.

**A2 Reference.** Round zero starts blank. Later rounds show only the immediately preceding submitted frame, with a side-by-side view and adjustable onion skin. Start the new frame blank by default; provide an explicit Copy previous frame action for convenient incremental motion. The previous frame remains immutable.

**A3 Handoff instructions.** A short optional note accompanies each submitted frame for the next editor. Notes are metadata outside the image. They are excluded from frame rasterization, GIFs, and drawing replays; show them in reveal only if the viewer explicitly opens notes.

**A4 Reveal.** Show every chain with participant credits and frame order. Play/pause, scrub individual frames, adjust playback speed, and choose looping. Each frame can show its own drawing timelapse; the finished animation also plays as a whole. These are distinct playback modes.

**A5 Export.** Download/save an animated GIF, a PNG frame sequence, and an editable game archive. Default playback is 6 frames per second, adjustable from 1 to 24. Respect GIF timing/color limits, show a preview, and keep source frames lossless in the archive. Export failure never discards the original.

## Exquisite Corpse

**C1 Parts.** Each player starts one creature. Three rounds produce head, torso/arms, and legs/feet on equally wide, vertically stacked bands. Use the same rotation formula as Animation, for exactly three rounds.

**C2 Small groups.** Three players produce creatures with three authors each. With two players, the head author also draws that creature's legs; this is intentional and explained in the lobby. Both configurations must be tested. This keeps the mode usable with one friend.

**C3 Joining strip.** The head/body editor draws to the bottom edge of their segment. The next editor sees only its last 24 px as a locked reference strip above their own blank segment. The reference does not consume or duplicate output pixels. The strip provides alignment at the neck/waist while hiding the rest. No full prior segment is delivered before reveal.

**C4 Reveal/export.** Assemble each creature in the correct order at full resolution, without gaps, duplicated seam rows, or an added reference overlay. Allow part-by-part reveal, full view, per-part drawing replay, PNG export, and an editable archive. Preserve author credits separately from artwork.

## Saving replay and gallery

**R1 Honest durability.** Track Drawing/Unsaved, Saved locally, Sync pending, and Synced independently. A save indicator follows confirmed durable storage, never just an enqueued job. Every completed edit and submitted artifact must survive a process kill after its save acknowledgment. An unfinished stroke may lose only its not-yet-durable portion; do not claim zero loss for unacknowledged input.

**R2 Recovery.** Resume drafts, game position, submissions, and shared-world edits after normal close, process termination, or network loss. Store game IDs and data beyond the upstream temporary recovery lifecycle. Disk-full, corrupt/truncated records, failed exports, and schema upgrades return actionable errors while preserving the last readable version.

**R3 Replay.** Capture drawing history from the first new Komaphone document, even before the reveal player is built. Record timing, resolved brush/settings/assets, erasing, fills, transforms, layer edits, and undo/redo. Timelapse reflects what was drawn and subsequently changed; imported images may appear as one step. Unsupported commands get a persisted checkpoint step rather than silently disappearing from playback. Legacy imports start from a baseline image with no invented earlier history.

**R4 Recovery versus backup.** Local saves protect against app interruption, not disk loss. The service preserves synced artifacts; provide a portable full archive and a documented backup/restore procedure. No automatic pruning of unsynced work. Set explicit quotas, surface space usage, and stop accepting growth gracefully at the limit. Deletions go through recoverable trash before permanent removal.

**G1 Gallery.** Show drafts, finished/incomplete games, animations, creatures, and shared worlds with thumbnails, dates, authors, mode, and save/sync state. Reopen, rename, favorite, filter, replay, and export. Store originals and metadata, not only thumbnails/GIFs. Local-only drafts stay private; accepted game artifacts are shared with room participants after their permitted reveal.

**G2 Remix finished work.** A Remix action copies a revealed creature, animation, or selected frame into a new editable project, or into an owned region of the shared world once that destination exists. Keep full animation frames and timing unless the user explicitly chooses a still frame. The copy gets its own identity and editable ownership; changes never modify the original. Preserve source revision, authors, and a visible attribution link in metadata and archives without burning credits into artwork. The copied result becomes the new replay baseline; retain access to the original history separately without pretending the remixer drew it. Required local-project remix ships with K4; the shared-world destination ships with K6. Unrevealed game content is never a remix source.

## Persistent shared world

**W1 Independent space.** The shared world lives outside timed lobbies. Open it anytime; edits persist between sessions. Service availability lets Alice upload, leave, and Bob later download without Alice's computer staying on. During service outage, local editing queues changes and visibly waits for sync.

**W2 Lateral expansion and deep detail.** Pan beyond the original region and create new drawing regions on demand. Zoom into a hut, then a pipe, then a tiny house, then another scene and continue adding detail at each scale. Increasing the magnification of one bitmap does not satisfy this requirement. Use a hierarchy of drawing regions at different scales with bounded local coordinates and progressive rendering. Depth and storage are finite in implementation; expose honest limits instead of promising mathematical infinity.

**W3 Interaction.** Creating a deeper region takes at most a simple Draw detail here action at the current view. Navigate and paint across transitions without a modal file-opening workflow. Support zoom out/home and location bookmarks. Include smooth automatic zoom playback toward a chosen detail/bookmark, plus pause and cancel; browsing never edits artwork. K5 proves single-target auto-zoom; K6 extends it into the required guided tours in W7.

**W4 Collaboration.** Two or three people can draw simultaneously, see named ghost cursors and in-progress stroke previews, reconnect, and receive durable edits. Each person owns editable layers within regions initially; they may see one another's work but cannot erase it accidentally. Personal undo only reverses that author's operations, never the entire shared document. Shared destructive editing is deferred.

**W5 Offline merge.** Reconnecting merges independent operations with stable identities and deterministic ordering. Duplicate/reordered messages converge. Conflicting metadata or references preserve a recoverable variant and explain resolution. Server timestamps alone are not a merge algorithm. Never replace a newer world with an old whole-document upload.

**W6 Performance and archives.** Load/render visible regions and suitable previews, with bounded caches; avoid loading the entire world every frame. Export a selected view/region to PNG and the full world to a portable archive. Full-world flat PNG export is not meaningful for arbitrarily extended/deep content. Validate the scene model before building the full world UI.

**W7 Guided zoom tours.** Save named routes through multiple bookmarked details, reorder stops, and set travel speed and pause duration. Play, pause, scrub, cancel, and replay the route through lateral and deep-zoom transitions without editing the world. Export the rendered journey as an animated GIF and PNG frame sequence, using a frozen world revision so concurrent edits cannot change the export halfway through. Exclude UI, cursors, and review highlights from exported artwork. Offer bounded duration/resolution settings and keep the original world and route intact on failure or cancellation. Tours persist through save/reopen, sync to world members, and travel with world archives. Missing/deleted destinations show an actionable repair or skip option. This is a required K6 feature.

**W8 Since you were away.** On returning to a world, show changes since that viewer's last reviewed revision, highlight affected regions by author, and jump to each location even at deep scales. Replay the interval with play/pause/scrub/speed controls, including erases, undo/redo, deletions, remixes, and edits uploaded after offline work. Default to friends' changes with an option to include your own; author filtering controls highlights and navigation while replay preserves canonical scene history. Keep unseen changes until explicitly reviewed or marked seen; background synchronization does not clear them. Review uses a fixed end revision so new arrivals stay unseen. First visits start with a clearly labeled current-state baseline. Presence/cursor motion is excluded, and reviewing changes never modifies artwork. This is a required K6 feature, including restart/offline recovery of the review position.

These additions are required in version 1.1. Their dependency order preserves the party-first release: local remix in K4, camera foundations in K5, then tours, catch-up playback, and world remix in K6.

## Scope boundaries and revisions

Classic prompt/draw/guess, public sharing, social feeds, competitive scoring, voice chat, mobile, production browser multiplayer, arbitrary-size groups, AI art, and simultaneous editing of the same destructive pixel layer are deferred. Keep this baseline stable; agents may refine mechanisms and tune bounded defaults with recorded evidence, but must not redefine a requested experience as complete by reducing it.

| Date | Version | Change |
|---|---|---|
| 2026-11-04 | 1.0 | Initial owner vision captured; Windows, private service, and party-first order explicitly confirmed. Remaining defaults are implementation assumptions. |
| 2026-11-04 | 1.1 | Owner approved all three proposed additions as required scope: guided zoom tours/export (W7), since-you-were-away highlights/playback (W8), and non-destructive remix (G2). |
