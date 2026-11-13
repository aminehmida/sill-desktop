#!/usr/bin/env bash
# The bench: the app against the synthetic host, end to end, in a real window. It pairs, streams a
# window, sends keys, a click, a scroll and a paste, changes a setting, takes a screenshot and
# leaves; then the host's log is checked for what the app sent. Results are in bench/out.
#   scripts/bench.sh [extra sill arguments…]      (e.g. --decoder software)
set -euo pipefail
cd "$(dirname "$0")/.."
out=bench/out
rm -rf "$out"; mkdir -p "$out/host" "$out/device"
# BENCH_BIN: a folder with built binaries to run instead of building (CI benches what it ships).
if [ -n "${BENCH_BIN:-}" ]; then bin=$BENCH_BIN; else cargo build -q -p sill-testhost -p sill-app; bin=${CARGO_TARGET_DIR:-target}/debug; fi
port=$((20000 + RANDOM % 20000))
code=482913557208
"$bin/sill-testhost" --port "$port" --dir "$out/host" --code "$code" --input-log --audio >"$out/host.log" 2>&1 &
host=$!
trap 'kill $host 2>/dev/null || true' EXIT
for _ in $(seq 50); do grep -q '^ADDRESS=' "$out/host.log" && break; sleep 0.1; done
grep -q '^ADDRESS=' "$out/host.log" || { echo "the test host did not start"; cat "$out/host.log"; exit 1; }

script="pair:127.0.0.1:$port,$code;wait:streaming;sleep:0.6;shot:$out/1-connected.png;select:desktop;wait:pictures,60;sleep:0.6;shot:$out/2-desktop.png;ui:apps;sleep:0.5;shot:$out/3-apps.png;ui:settings;sleep:0.5;shot:$out/4-settings.png;ui:none;select:first;sleep:0.9;press:4;press:25,m;text:hello;click:0.25,0.75;scroll:0.1;fps:120;paste:from the bench;sleep:0.7;shot:$out/5-window.png;ui:bar;sleep:0.5;shot:$out/6-window-bar.png;ui:none;disconnect;wait:idle;sleep:0.3;shot:$out/7-macs.png;ui:add;sleep:0.3;shot:$out/8-add.png;exit"
status=0
"$bin/sill" --config-dir "$out/device" --allow-loopback --no-discovery --null-sound --size 1180x760 --timeout 40 \
  --auto "$script" "$@" >"$out/app.log" 2>&1 || status=$?
sleep 0.3
kill $host 2>/dev/null || true
grep '^BENCH' "$out/app.log" || true
fail() { echo "FAIL: $1"; status=1; }
[ "$status" = 0 ] || { echo "FAIL: the app left with $status"; tail -20 "$out/app.log"; }
want() { grep -q "$1" "$out/host.log" || fail "the host never saw $2"; }
want '"hidUsage":4,"down":true' 'the A key'
want '"hidUsage":25,"down":true,"modifiers":1048576' '⌘V'
want 'hello'      'the typed text'
want '0.25'       'the click'
want 'from the bench' 'the pasted text'
for shot in 2-desktop 5-window 7-macs; do [ -s "$out/$shot.png" ] || fail "no screenshot $shot"; done
[ "$status" = 0 ] && echo "BENCH PASSED" || echo "BENCH FAILED (see $out)"
exit $status
