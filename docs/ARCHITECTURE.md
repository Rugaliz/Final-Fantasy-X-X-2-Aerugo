# Architecture

## Crates

| Crate | Role |
|---|---|
| `aerugo` | The executable: command line, settings, launcher, application loop |
| `aerugo-platform` | SDL3 window and events, user directories, frame limiter |
| `aerugo-vfs` | Finding the game installation, reading the VBF archives, the virtual file system |
| `aerugo-gfx` | Vulkan: instance, device, swapchain, frame pacing, pipelines, egui drawing |
| `aerugo-formats` | Readers for the games' data formats: text encoding, data tables, Phyre clusters, textures, models, motions, sound banks and events, scripts |
| `aerugo-script` | The script machine: workers, priority levels, requests, natives with a coverage tracker |
| `aerugo-sim` | Simulation runtime: fixed tick, snapshots, interpolation; field events (script, actors, messages, camera, player), saves |
| `aerugo-audio` | Float mixer with windowed-sinc resampling and a limiter, SDL3 playback |
| `aerugo-video` | WebM movies: Matroska demuxing, VP8 through libvpx (loaded at run time), Vorbis through lewton |
| `aerugo-debug-ui` | egui integration with SDL3 input, debug panels |
| `xtask` | Developer commands (`cargo run -p xtask -- <command>`) |

More crates arrive with later milestones: one crate per game.

### Dependency rules

- Format parsers and the virtual file system do no rendering and no window work.
- Game logic (the simulation and, later, the game crates) never depends on graphics, audio or the window.
  It communicates through plain data: per-tick **snapshots** of what should be on screen, and event queues.
  This keeps the logic deterministic and testable without a GPU.
- `unsafe` code is limited to `aerugo-gfx`, `aerugo-platform` and thin FFI wrappers. Every `unsafe` block
  carries a `SAFETY:` comment.

## Time: fixed tick, interpolated frames

The games' logic counts frames at a fixed rate (30 per second). Aerugo runs the simulation at exactly
that rate regardless of the display:

1. Real time accumulates; for each whole tick period the simulation advances one tick (at most a few per
   frame, so a slow frame slows the game down instead of skipping logic).
2. After each tick the simulation publishes a snapshot: cameras, objects with stable ids and transforms,
   2D layers, effects.
3. Every displayed frame interpolates between the last two snapshots by the fraction of the tick that has
   elapsed. Objects flagged as "discontinuous" (teleports, camera cuts, newly created) are not blended.

The result is smooth motion at 60, 144, 240 Hz or more with logic that behaves exactly as at 30 Hz, at the
cost of one tick (about 33 ms) of display latency. Interpolation can be switched off.

## State ownership

A running game lives entirely inside one session value owned by the application. Leaving a game drops it
and returns to the launcher. There are no mutable globals in game code.

## Game data

The Steam release stores its data in three archives (`FFX_Data.vbf`, `FFX2_Data.vbf`, `metamenu.vbf`).
`aerugo-vfs` mounts them read-only and resolves paths case-insensitively. A loose-file directory can be
layered on top for development overrides. The archive format is described in
[formats/vbf.md](formats/vbf.md).

## Rendering

Vulkan 1.3 through `ash`: dynamic rendering, synchronization2, two frames in flight. Shaders are written in
GLSL and compiled to SPIR-V at build time. The renderer never waits on the simulation; it only reads
snapshots.

Mesh parts carry a material state read from the game's effect clusters (blending, culling, depth); pipelines are
built per blend state and the rest is Vulkan 1.3 dynamic state. A headless renderer draws scenes offscreen with
CPU readback for golden-image tests.

Field scenes follow the remaster's shaders. Each map part ends with the colour curve its shader variant has
(read from the variant's compiled code: the field curve, the sky curve or none), alpha-tests against its
material's threshold (with multisampling, the passes that use alpha to coverage do that instead), and the water
and ice shaders sample their cube maps. Characters take the remaster's character light: an ambient plus three
directional lights from the field light set the event scripts set, dimmed by the map's glow settings, times each
character's colour and shade (eased towards the walk mesh's shade where it stands); a character's alpha fades
it with a blended pass. Fog stays off until a script sets its limit. After the scene: ambient occlusion, glow,
the unsharp mask and the brightness setting, with 8× multisampling.

The fragment shader reads its per-frame constants from one storage buffer: the scene constants, then joint
matrices, then two slots per water or ice part, the character light's two slots, and one slot per tinted draw.
