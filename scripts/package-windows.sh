#!/usr/bin/env bash
# Packages the release build for Windows as a folder with its FFmpeg DLLs, zipped into dist/.
#   FFMPEG_DIR=… scripts/package-windows.sh [target-triple]     (Git Bash; after cargo build --release)
set -euo pipefail
cd "$(dirname "$0")/.."
: "${FFMPEG_DIR:?the FFmpeg the build linked (scripts/fetch-ffmpeg.sh)}"
target="${1:-}"
bin="target/${target:+$target/}release"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
case "${target:-x86_64}" in aarch64*) arch=arm64 ;; *) arch=x64 ;; esac
name="Sill-Desktop-$version-windows-$arch"
rm -rf dist && mkdir -p "dist/$name"
cp "$bin/sill.exe" "dist/$name/Sill Desktop.exe"
ffmpeg=$(cygpath -u "$FFMPEG_DIR" 2>/dev/null || echo "$FFMPEG_DIR")
cp "$ffmpeg"/bin/avcodec-*.dll "$ffmpeg"/bin/avutil-*.dll "$ffmpeg"/bin/swresample-*.dll "dist/$name/"
cp LICENSE NOTICE "dist/$name/"
cp "$ffmpeg/LICENSE.txt" "dist/$name/FFmpeg-LICENSE.txt"
if command -v zip >/dev/null; then (cd dist && zip -qr "$name.zip" "$name"); else (cd dist && powershell -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '$name.zip'"); fi
ls -la dist/*.zip
