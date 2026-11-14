# Architecture

## Crates

| Crate | Role |
|---|---|
| `aerugo` | The executable: command line, settings, launcher, application loop |
| `aerugo-platform` | SDL3 window and events, user directories, frame limiter |
| `aerugo-vfs` | Finding the game installation, reading the VBF archives, the virtual file system |
| `aerugo-gfx` | Vulkan: instance, device, swapchain, frame pacing, pipelines, egui drawing |
| `aerugo-formats` | Readers for the games' data formats: text encoding, data tables |
| `aerugo-sim` | Simulation runtime: fixed tick, snapshots, interpolation |
| `aerugo-debug-ui` | egui integration with SDL3 input, debug panels |
| `xtask` | Developer commands (`cargo run -p xtask -- <command>`) |

More crates arrive with later milestones: file-format parsers, scene renderer, audio, video, the script
runtime, and one crate per game.

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
