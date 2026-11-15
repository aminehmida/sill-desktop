#!/usr/bin/env bash
# Packages the release build as "Sill Desktop.app" with its FFmpeg inside, zipped into dist/.
#   FFMPEG_DIR=… scripts/package-macos.sh        (after cargo build --release -p sill-app)
set -euo pipefail
cd "$(dirname "$0")/.."
: "${FFMPEG_DIR:?the FFmpeg the build linked (scripts/build-ffmpeg-macos.sh)}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
arch=$(uname -m)
app="dist/Sill Desktop.app"
rm -rf dist && mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
sed "s/<string>0.6.1<\/string>/<string>${version%%-*}<\/string>/" packaging/macos/Info.plist > "$app/Contents/Info.plist"
cp target/release/sill "$app/Contents/MacOS/sill"
# The libraries the executable names, as real files (not the version symlinks).
for lib in $(otool -L target/release/sill | awk '/@rpath\/lib(av|sw)/ {print $1}'); do
  cp -L "$FFMPEG_DIR/lib/$(basename "$lib")" "$app/Contents/Frameworks/"
done
# Its libraries are in the bundle now, not where the build found them.
install_name_tool -delete_rpath "$FFMPEG_DIR/lib" "$app/Contents/MacOS/sill" 2>/dev/null || true
install_name_tool -add_rpath @executable_path/../Frameworks "$app/Contents/MacOS/sill"
cp LICENSE NOTICE packaging/macos/AppIcon.icns "$app/Contents/Resources/"
codesign --force --deep --sign - "$app"
(cd dist && ditto -c -k --keepParent "Sill Desktop.app" "Sill-Desktop-$version-macos-$arch.zip")
ls -la dist/*.zip
