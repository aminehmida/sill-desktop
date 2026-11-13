#!/usr/bin/env bash
# Fetches the LGPL shared FFmpeg 9.0 the Windows and Linux builds link and ship (BtbN's builds),
# into vendor/ffmpeg/<platform>, and prints the FFMPEG_DIR to build with.
#   scripts/fetch-ffmpeg.sh [linux64|linuxarm64|win64|winarm64]
set -euo pipefail
cd "$(dirname "$0")/.."
platform="${1:-}"
if [ -z "$platform" ]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) platform=linux64 ;;
    Linux-aarch64) platform=linuxarm64 ;;
    MINGW*-x86_64 | MSYS*-x86_64) platform=win64 ;;
    MINGW*-aarch64 | MSYS*-aarch64) platform=winarm64 ;;
    *) echo "no prebuilt FFmpeg for $(uname -s)-$(uname -m): install FFmpeg 9 (macOS: brew install ffmpeg)" >&2; exit 1 ;;
  esac
fi
dest="vendor/ffmpeg/$platform"
if [ ! -d "$dest/include" ]; then
  case "$platform" in win*) ext=zip ;; *) ext=tar.xz ;; esac
  name="ffmpeg-n9.0-latest-$platform-lgpl-shared-9.0"
  mkdir -p vendor/ffmpeg
  curl -fsSL --retry 3 -o "vendor/ffmpeg/$name.$ext" "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$name.$ext"
  rm -rf "vendor/ffmpeg/$name" "$dest"
  if [ "$ext" = zip ]; then (cd vendor/ffmpeg && unzip -q "$name.zip") >&2; else tar -xJf "vendor/ffmpeg/$name.$ext" -C vendor/ffmpeg; fi
  mv "vendor/ffmpeg/$name" "$dest"
  rm -f "vendor/ffmpeg/$name.$ext"
fi
echo "$(pwd)/$dest"
