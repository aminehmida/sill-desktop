#!/usr/bin/env bash
# Builds the small LGPL FFmpeg the Mac app ships: only what Sill needs (the HEVC and AAC decoders,
# VideoToolbox), as shared libraries, into vendor/ffmpeg/macos. Prints the directory.
# Homebrew's FFmpeg is a GPL build with dozens of libraries behind it: fine to develop against,
# not to hand out inside an Apache-licensed app.
set -euo pipefail
cd "$(dirname "$0")/.."
version=9.0
dest="$PWD/vendor/ffmpeg/macos"
if [ ! -f "$dest/lib/libavcodec.dylib" ]; then
  mkdir -p vendor/ffmpeg/src && cd vendor/ffmpeg/src
  if [ ! -d "ffmpeg-$version" ]; then
    curl -fsSL --retry 3 -o ffmpeg.tar.xz "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz"
    tar -xJf ffmpeg.tar.xz && rm ffmpeg.tar.xz
  fi
  cd "ffmpeg-$version"
  ./configure --prefix="$dest" --enable-shared --disable-static --disable-programs --disable-doc \
    --disable-everything --disable-autodetect --disable-network \
    --disable-avdevice --disable-avfilter --disable-avformat --disable-swscale --disable-swresample \
    --enable-decoder=hevc,aac --enable-parser=hevc,aac --enable-videotoolbox --enable-hwaccel=hevc_videotoolbox \
    --extra-cflags="-mmacosx-version-min=12.0" --extra-ldflags="-mmacosx-version-min=12.0" \
    --install-name-dir='@rpath' >&2
  make -j"$(sysctl -n hw.ncpu)" >&2
  make install >&2
fi
echo "$dest"
