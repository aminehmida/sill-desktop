#!/usr/bin/env bash
# Builds the release app and installs it as /Applications/Sill Desktop.app (linked against
# Homebrew's FFmpeg: a build for this Mac, not one to hand out).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p sill-app
app="/Applications/Sill Desktop.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp packaging/macos/Info.plist "$app/Contents/Info.plist"
cp target/release/sill "$app/Contents/MacOS/sill"
cp packaging/macos/AppIcon.icns "$app/Contents/Resources/"
codesign --force --sign - "$app" 2>/dev/null
echo "installed $app"
