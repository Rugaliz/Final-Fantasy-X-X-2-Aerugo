#!/bin/sh
# Build Aerugo (release) into one executable you can drop into the game folder.
#
#   scripts/package.sh                 build, then copy to dist/aerugo
#   scripts/package.sh <game folder>   build, then also copy into that folder
#
# The game folder is the one holding data/FFX_Data.vbf. Started from there, Aerugo uses that folder as the game
# (a --game-dir or AERUGO_GAME_DIR still takes precedence). It needs SDL3 and a Vulkan driver on the system.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

echo "Building release..."
cargo build --release -p aerugo

mkdir -p dist
cp target/release/aerugo dist/aerugo
chmod +x dist/aerugo
echo "Built: $repo/dist/aerugo"

if [ $# -ge 1 ]; then
    game=$1
    if [ ! -f "$game/data/FFX_Data.vbf" ]; then
        echo "error: $game does not look like the game folder (no data/FFX_Data.vbf)" >&2
        exit 1
    fi
    cp dist/aerugo "$game/aerugo"
    chmod +x "$game/aerugo"
    echo "Installed: $game/aerugo (run it from there)"
fi
