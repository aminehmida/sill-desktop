#!/usr/bin/env bash
# Cross-compiles for Windows on ARM, copies the build into the bench VM (bench/windows/make-vm.sh)
# and runs the bench there, in its desktop session. Results come back to bench/out-windows.
#   scripts/bench-windows.sh [extra sill arguments…]
set -euo pipefail
cd "$(dirname "$0")/.."
fusion="/Applications/VMware Fusion.app/Contents/Public"
vm="$HOME/Virtual Machines/sill-win"
vmx="$vm/sill-win.vmx"
target=aarch64-pc-windows-msvc
export PATH="/opt/homebrew/opt/llvm/bin:/opt/homebrew/opt/lld/bin:$PATH" XWIN_ACCEPT_LICENSE=1
export FFMPEG_DIR="$(scripts/fetch-ffmpeg.sh winarm64)"
cargo xwin build -q --cross-compiler clang --target $target -p sill-testhost -p sill-app
rm -rf bench/win-pkg bench/win-pkg.zip bench/out-windows && mkdir -p bench/win-pkg bench/out-windows
cp target/$target/debug/sill.exe target/$target/debug/sill-testhost.exe "$FFMPEG_DIR"/bin/*.dll bench/windows/bench.ps1 bench/win-pkg/
(cd bench/win-pkg && zip -qr ../win-pkg.zip .)
guest() { "$fusion/vmrun" -T fusion -gu sill -gp "$(cat "$vm/pass.txt")" "$@"; }
ps='C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
"$fusion/vmrun" list | grep -q "$vmx" || "$fusion/vmrun" -T fusion start "$vmx" nogui
guest copyFileFromHostToGuest "$vmx" "$PWD/bench/win-pkg.zip" 'C:\sill\pkg.zip'
guest runProgramInGuest "$vmx" "$ps" -Command "Get-Process sill,sill-testhost -ErrorAction SilentlyContinue | Stop-Process -Force; Remove-Item C:\sill\app -Recurse -Force -ErrorAction SilentlyContinue; Expand-Archive C:\sill\pkg.zip C:\sill\app -Force"
guest runProgramInGuest "$vmx" -interactive -activeWindow "$ps" -ExecutionPolicy Bypass -File 'C:\sill\app\bench.ps1' "$@" || true
for f in result.txt app.log app.err host.log host.err 2-desktop.png 3-apps.png 4-settings.png 5-window.png 6-window-bar.png 7-macs.png; do
  guest copyFileFromGuestToHost "$vmx" "C:\\sill\\app\\out\\$f" "$PWD/bench/out-windows/$f" >/dev/null 2>&1 || true
done
grep -a '^BENCH' bench/out-windows/app.log | grep -av 'BENCH ok' || true
tr -d '\r' < bench/out-windows/result.txt
grep -q 'BENCH PASSED' bench/out-windows/result.txt
