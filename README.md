# Aerugo

An open-source, from-scratch engine reimplementation of **FINAL FANTASY X | X-2 HD Remaster**, written in Rust.

Aerugo reads the game data from your own installed copy of the Steam release and runs it with a new engine:
a Vulkan renderer, SDL3 for windowing, input and audio, and a faithful reimplementation of the game logic.

> **Status: early development.** Nothing is playable yet. The current milestone builds the foundations:
> window, Vulkan renderer bootstrap, game detection, archive reading and the launcher.
> See [docs/ROADMAP.md](docs/ROADMAP.md).

## Goals

- Faithful game logic for FINAL FANTASY X and FINAL FANTASY X-2 (including Last Mission), tick for tick.
- Any resolution up to 8K, aspect ratios from 4:3 to 32:9, display rates of 240 Hz and above
  (the simulation keeps the original tick rate; rendering interpolates between ticks).
- High-quality audio: 32-bit float mixing, high-quality resampling, English or Japanese voices.
- One executable with a launcher that detects which games are installed.
- Built-in developer tools: asset browser, model and map viewers.
- Linux and Windows.

## You need your own copy of the game

Aerugo contains **no game data**. It needs the Steam release of *FINAL FANTASY X/X-2 HD Remaster*
installed on your machine; it finds the installation automatically or you can point it at the folder.
Please do not open issues asking for game files.

## Building

Requirements:

- Rust 1.97 or newer (`rustup` picks the pinned toolchain automatically)
- A Vulkan 1.3 capable GPU and driver
- SDL3 3.2 or newer as a system library (Linux: your distribution's `sdl3` package; Windows: see
  [CONTRIBUTING.md](CONTRIBUTING.md))

```sh
cargo run --release
```

Useful options:

```sh
cargo run --release -- --game-dir "/path/to/FINAL FANTASY FFX&FFX-2 HD Remaster"
cargo run --release -- --list-games        # print what was detected and exit
cargo run --release -- --portable          # keep settings next to the executable
cargo run --release -- --test-scene        # timing test scene (30 Hz simulation at display rate)
cargo run --release -- --viewer            # asset viewer (data tables, textures, …)
cargo run --release -- --fps-limit 240 --present-mode mailbox
cargo run --release -- --help              # all options
```

The asset viewer shows Japanese, Chinese and Korean text with a system font (Noto Sans CJK on Linux, the
standard CJK fonts on Windows); nothing is bundled.

Keys: **F3** debug panel, **F11** or **Alt+Enter** fullscreen, **Esc** back to the launcher.
Settings are stored in `settings.toml` in your config folder (`~/.config/aerugo` on Linux,
`%APPDATA%\aerugo` on Windows), or in `aerugo-portable/` with `--portable`.

## License

Aerugo's source code is released under the [MIT License](LICENSE).

FINAL FANTASY is a registered trademark of Square Enix Holdings Co., Ltd. Aerugo is an independent
fan project and is not affiliated with, endorsed by, or sponsored by Square Enix. All game content
belongs to its respective owners and is not distributed with this project.
