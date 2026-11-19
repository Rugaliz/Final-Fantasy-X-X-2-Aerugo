# Architecture

## Crates

| Crate | Role |
|---|---|
| `aerugo` | The executable: command line, settings, launcher, application loop |
| `aerugo-platform` | SDL3 window and events, user directories, frame limiter |
| `aerugo-vfs` | Finding the game installation, reading the VBF archives, the virtual file system |
| `aerugo-gfx` | Vulkan: instance, device, swapchain, frame pacing, the games' programs and render states, frame recording, egui drawing |
| `aerugo-dxbc` | Translating the games' compiled Direct3D 11 shaders to SPIR-V (dxbc-spirv, built from `third_party/`) |
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
- `unsafe` code is limited to `aerugo-gfx`, `aerugo-platform` and thin FFI wrappers (`aerugo-dxbc`). Every `unsafe` block
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

Vulkan 1.3 through `ash`: dynamic rendering, synchronization2, two frames in flight. The renderer never waits on the
simulation; it only reads snapshots.

Scenes are drawn with the games' own compiled Direct3D 11 programs, read from the effect clusters and translated to
SPIR-V by `aerugo-dxbc` (DXVK's dxbc-spirv, with DXVK's binding layout and float controls). Each pipeline takes its
pass's stored Direct3D state as it is (rasterizer, depth-stencil and blend descriptions, the input layout from the
pass's stream descriptions), and each program's constant buffers are filled by variable name from its own
reflection data: the material's values, the scene camera and fog, the object's world and skin matrices, the light
set. Samplers are converted from the materials' sampler states the way the original's runtime converts them. A value
no source supplies stays zero and is logged, and a draw whose program samples a texture nothing binds is left out
with a warning.

A frame is a list of operations (set targets, clear, draw, resolve, copy) built by the field renderer
(`aerugo::phyre`) and recorded by `aerugo-gfx`. After the scene the post chain replays the original's passes in its
order with the post effects' own programs: ambient occlusion, glow, unsharp mask, SMAA and brightness. Aerugo's own
GLSL (compiled to SPIR-V at build time) is only for its test quads; egui draws the interface. A headless renderer
draws frames offscreen with CPU readback for tests, and local checks write frames in the oracle's capture form to
compare with the original's draw by draw.
