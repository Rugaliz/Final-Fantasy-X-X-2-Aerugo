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

## Platform notes

- **Linux**: install SDL3 from your distribution (`sdl3` / `libsdl3-dev`), and the Vulkan loader.
- **Windows**: build with the `sdl-from-source` feature to compile SDL3 statically (needs CMake):
  `cargo run --release --features sdl-from-source`.

## Unsafe code

Allowed only in `aerugo-gfx`, `aerugo-platform` and FFI wrappers. Each `unsafe` block needs a `// SAFETY:`
comment explaining why it is sound.
