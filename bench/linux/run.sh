#!/usr/bin/env bash
# Runs a command in the Linux bench container (default: build, test and the headless bench).
#   bench/linux/run.sh [command…]
set -euo pipefail
cd "$(dirname "$0")/../.."
docker build -q -t sill-desktop-linux bench/linux >/dev/null
cmd=${*:-'cargo test --workspace && xvfb-run -a -s "-screen 0 1600x1000x24" scripts/bench.sh'}
docker run --rm -v "$PWD":/src -v sill-desktop-target:/target -v sill-desktop-cargo:/root/.cargo/registry \
  sill-desktop-linux bash -lc 'export FFMPEG_DIR=$(scripts/fetch-ffmpeg.sh) && export LD_LIBRARY_PATH=$FFMPEG_DIR/lib && '"$cmd"
