# Contributing

## Ground rules

- **No game content** in the repository: no assets, no extracted files, no original executables or code.
  Tests that need game data read it from your own installation (see below) and skip themselves when it is
  not available.
- Code is MIT-licensed. Dependencies must use compatible permissive licences; `cargo deny check` enforces
  this.
- Keep game logic free of graphics, audio and window code (see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)).

## Before every commit

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check        # install with: cargo install cargo-deny
```

## Tests that use your game installation

Set `AERUGO_GAME_DIR` to the game folder (the one containing `data/FFX_Data.vbf`):

```sh
AERUGO_GAME_DIR="/path/to/FINAL FANTASY FFX&FFX-2 HD Remaster" cargo test --workspace
cargo run -p xtask -- check-game        # validate an installation
cargo run -p xtask -- verify-archives   # read every file in every archive (takes a while)
```

Without the variable those tests print a notice and pass.

## Vulkan tests

`aerugo-gfx` contains a headless rendering test. It needs a Vulkan driver; a software implementation such as
Mesa's lavapipe works. It skips itself when no Vulkan device is present.

### Golden images

`cargo test -p aerugo golden` renders, for each of the 20 effects most used by models, the parts that use them
from a fixed camera and compares the result with reference PNGs in `AERUGO_GOLDEN_DIR` (needs `AERUGO_GAME_DIR`
and a Vulkan device). The references are game imagery: keep them outside the repository. Create or refresh them
with `AERUGO_GOLDEN_UPDATE=1`; failing cases leave a `*.actual.png` next to the reference. The test also checks
that face culling keeps the visible side of every case.

`cargo test -p aerugo layered_maps` opens every map with pre-rendered layers through the viewer's code and renders
its best stored camera at 4:3, 16:9, 21:9 and 32:9 in crop and fit. It reports how many maps frame their background
cleanly and fails if a known-good map or the overall count regresses.

## Platform notes

- **Linux**: install SDL3 from your distribution (`sdl3` / `libsdl3-dev`), and the Vulkan loader.
- **Windows**: build with the `sdl-from-source` feature to compile SDL3 statically (needs CMake):
  `cargo run --release --features sdl-from-source`.

## Unsafe code

Allowed only in `aerugo-gfx`, `aerugo-platform` and FFI wrappers. Each `unsafe` block needs a `// SAFETY:`
comment explaining why it is sound.
