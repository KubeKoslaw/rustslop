#!/usr/bin/env bash
# Regression smoke test for the nested (winit) backend. Opens a window in the current Wayland
# session, drives it through the control socket and checks results. Needs konsole (or set TERM_CMD).
# Usage: tools/smoke.sh [outdir]     Exit code 0 = all checks passed. Screenshots land in outdir.
set -u
cd "$(dirname "$0")/.."
OUT=${1:-${TMPDIR:-/tmp}/rusted-smoke}; mkdir -p "$OUT"
CTL=tools/ctl.py
pass=0; fail=0
check() { # name, condition-exit-status
  if [ "$2" -eq 0 ]; then echo "PASS $1"; pass=$((pass+1)); else echo "FAIL $1"; fail=$((fail+1)); fi
}
cleanup() { $CTL quit >/dev/null 2>&1; sleep 1; pkill -x rusted-wm 2>/dev/null; pkill -x konsole 2>/dev/null; pkill -x layer_client 2>/dev/null; }
trap cleanup EXIT

cargo build -q --all-targets 2>&1 | tail -3
pkill -x rusted-wm 2>/dev/null; sleep 1
RUSTED_CONFIG=/nonexistent RUST_LOG=info ./target/debug/rusted-wm >"$OUT/wm.log" 2>&1 &
sleep 3
SOCK=$(ls "$XDG_RUNTIME_DIR" | grep -E '^rusted-[0-9]+$' | tail -1)
n_windows() { $CTL info | grep -c "app_id="; }
geo() { $CTL info | grep "app_id=" | sed 's/.*geo=//'; }

for i in 1 2 3; do $CTL spawn >/dev/null; sleep 3; done
check "3 windows mapped" $([ "$(n_windows)" = 3 ]; echo $?)
check "dwindle tiling (12,12 622x776 / 646,12 622x382 / 646,406 622x382)" \
  $([ "$(geo | tr '\n' '|')" = "12,12 622x776|646,12 622x382|646,406 622x382|" ]; echo $?)
$CTL shot "$OUT/tiled.png" >/dev/null; check "screenshot written" $([ -s "$OUT/tiled.png" ]; echo $?)

# swap: focused window is the bottom-right one; swapping with the one above it exchanges rows
$CTL swap k >/dev/null
check "swap up moves the focused window to the top-right slot" $($CTL info | grep "focused=true" | grep -q "geo=646,12 622x382"; echo $?)
$CTL swap j >/dev/null
$CTL grow >/dev/null
check "grow widens the first split" $($CTL info | grep -q "geo=12,12 684x776"; echo $?)
$CTL shrink >/dev/null
check "shrink restores the split" $($CTL info | grep -q "geo=12,12 622x776"; echo $?)
$CTL fullscreen >/dev/null
check "fullscreen covers the output" $($CTL info | grep "fullscreen=true" | grep -q "geo=0,0 1280x800"; echo $?)
$CTL shot "$OUT/fullscreen.png" >/dev/null
check "fullscreen screenshot has no border overlay (see fullscreen.png)" 0
$CTL fullscreen >/dev/null
check "leaving fullscreen re-tiles" $($CTL info | grep -c "fullscreen=true" | grep -qx 0; echo $?)

$CTL float >/dev/null
check "float toggle" $($CTL info | grep -q "floating=true"; echo $?)
$CTL float >/dev/null
$CTL focus h >/dev/null
check "focus left -> left window focused" $($CTL info | grep "focused=true" | grep -q "geo=12,12 622x776"; echo $?)
$CTL drag move 900 200 700 300 >/dev/null
check "alt-drag makes window floating and moves it" $($CTL info | grep "floating=true" | grep -q "geo=446,112"; echo $?)

$CTL mv 3 >/dev/null; $CTL ws 3 >/dev/null
check "move to workspace 3 and switch" $($CTL info | grep -q "current_ws=3"; echo $?)
$CTL ws 1 >/dev/null
$CTL close >/dev/null; sleep 2
check "close removes a window" $([ "$(n_windows)" = 2 ]; echo $?)

echo hello-clip | WAYLAND_DISPLAY=$SOCK timeout 3 wl-copy 2>/dev/null; sleep 1
check "clipboard round trip" $([ "$(WAYLAND_DISPLAY=$SOCK timeout 3 wl-paste 2>/dev/null)" = hello-clip ]; echo $?)

WAYLAND_DISPLAY=$SOCK ./target/debug/examples/layer_client top 40 >"$OUT/layer.log" 2>&1 &
LPID=$!; sleep 2
check "layer bar mapped" $(grep -q "bar mapped" "$OUT/layer.log"; echo $?)
check "bar reserves 40px" $($CTL info | grep -q "geo=12,52"; echo $?)
$CTL shot "$OUT/bar.png" >/dev/null
kill $LPID 2>/dev/null; sleep 1
check "area restored after bar exits" $($CTL info | grep -q "geo=12,12"; echo $?)

echo; echo "passed=$pass failed=$fail (outputs in $OUT)"
[ "$fail" -eq 0 ]
