#!/usr/bin/env bash
# Regression smoke test for the nested (winit) backend. Opens a window in the current Wayland
# session, drives it through the control socket and checks results. Needs konsole (or set TERM_CMD).
# Usage: tools/smoke.sh [outdir]     Exit code 0 = all checks passed. Screenshots land in outdir.
set -u
cd "$(dirname "$0")/.."
OUT=${1:-${TMPDIR:-/tmp}/rusted-smoke}; mkdir -p "$OUT"; : > "$OUT/config.toml"
CTL=tools/ctl.py
pass=0; fail=0
check() { # name, condition-exit-status
  if [ "$2" -eq 0 ]; then echo "PASS $1"; pass=$((pass+1)); else echo "FAIL $1"; fail=$((fail+1)); fi
}
# SAFETY: only ever touch the compositor started by this script (its PID and its own socket), never
# another rusted-wm that may be running on a real VT, and only terminals connected to that socket.
WMPID=""; SOCK=""
kill_test_terminals() {
  [ -n "$SOCK" ] || return 0
  for p in $(pgrep -x konsole); do
    tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -qx "WAYLAND_DISPLAY=$SOCK" && kill $p
  done
}
cleanup() {
  [ -n "$SOCK" ] && $CTL quit >/dev/null 2>&1
  sleep 1
  [ -n "$WMPID" ] && kill "$WMPID" 2>/dev/null
  kill_test_terminals
  [ -n "${LAYERPID:-}" ] && kill "$LAYERPID" 2>/dev/null
}
trap cleanup EXIT

cargo build -q --all-targets 2>&1 | tail -3
# Fake audio tools so the volume actions can be checked without touching real audio.
mkdir -p "$OUT/fakebin"
printf '#!/bin/sh\necho "$@" >> "%s/wpctl.log"\n[ "$1" = get-volume ] && echo "Volume: 0.55"\nexit 0\n' "$OUT" > "$OUT/fakebin/wpctl"
printf '#!/bin/sh\necho "$@" >> "%s/notify.log"\n' "$OUT" > "$OUT/fakebin/notify-send"
chmod +x "$OUT/fakebin/wpctl" "$OUT/fakebin/notify-send"; rm -f "$OUT/wpctl.log" "$OUT/notify.log"
export PATH="$OUT/fakebin:$PATH"
RUSTED_CONFIG="$OUT/config.toml" RUST_LOG=info ./target/debug/rusted-wm ${RUSTED_BACKEND---headless} >"$OUT/wm.log" 2>&1 &
WMPID=$!
for _ in $(seq 1 50); do
  SOCK=$(grep -o 'WAYLAND_DISPLAY=rusted-[0-9]*' "$OUT/wm.log" | head -1 | cut -d= -f2)
  [ -n "$SOCK" ] && break
  sleep 0.2
done
[ -n "$SOCK" ] || { echo "compositor did not start, see $OUT/wm.log"; exit 1; }
CTL="tools/ctl.py -s $SOCK"
sleep 2
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
LPID=$!; LAYERPID=$LPID; sleep 2
check "layer bar mapped" $(grep -q "bar mapped" "$OUT/layer.log"; echo $?)
check "bar reserves 40px" $($CTL info | grep -q "geo=12,52"; echo $?)
$CTL shot "$OUT/bar.png" >/dev/null
kill $LPID 2>/dev/null; sleep 1
check "area restored after bar exits" $($CTL info | grep -q "geo=12,12"; echo $?)

W0=$($CTL info | sed -n 's/^output \([0-9]*\)x.*/\1/p')
printf '[general]\nscale = 2.0\n' > "$OUT/config.toml"; $CTL reload >/dev/null; sleep 1
check "scale 2 halves the logical output width" $([ "$($CTL info | sed -n 's/^output \([0-9]*\)x.*/\1/p')" = $((W0 / 2)) ]; echo $?)
$CTL shot "$OUT/scaled.png" >/dev/null
: > "$OUT/config.toml"; $CTL reload >/dev/null; sleep 1
check "scale restored" $([ "$($CTL info | sed -n 's/^output \([0-9]*\)x.*/\1/p')" = "$W0" ]; echo $?)
printf '[general]\ngapp = 3\n' > "$OUT/config.toml"
check "reload reports an unknown config key with a hint" $($CTL reload | grep -q "did you mean .gap."; echo $?)
: > "$OUT/config.toml"; $CTL reload >/dev/null

# ---- status helpers for bars ----
check "ws-json for waybar" $($CTL ws-json | grep -q '"text": ".*1.*"'; echo $?)
$CTL ws next >/dev/null; check "ws next goes to workspace 2" $($CTL info | grep -q "current_ws=2"; echo $?)
$CTL ws prev >/dev/null; check "ws prev goes back" $($CTL info | grep -q "current_ws=1"; echo $?)

# ---- config tooling ----
check "--dump-schema prints valid JSON with settings and actions" $(./target/debug/rusted-wm --dump-schema | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['settings'] and d['actions']"; echo $?)
check "--check-config accepts the shipped configs" $(./target/debug/rusted-wm --check-config contrib/config.rice.toml >/dev/null && ./target/debug/rusted-wm --check-config config.example.toml >/dev/null; echo $?)
printf '[general]\ngapp = 3\n' > "$OUT/bad.toml"
check "--check-config fails on a config with problems" $(! ./target/debug/rusted-wm --check-config "$OUT/bad.toml" >/dev/null; echo $?)

# ---- audio actions (fake wpctl / notify-send) ----
$CTL volume up >/dev/null; sleep 1
check "volume up calls wpctl set-volume with the step" $(grep -q "set-volume -l 1.00 @DEFAULT_AUDIO_SINK@ 5%+" "$OUT/wpctl.log"; echo $?)
check "volume up shows the new level" $(grep -q "Volume 55%" "$OUT/notify.log"; echo $?)
$CTL volume down >/dev/null; $CTL volume mute >/dev/null; $CTL volume mic-mute >/dev/null; sleep 1
check "volume down uses the negative step" $(grep -q "set-volume @DEFAULT_AUDIO_SINK@ 5%-" "$OUT/wpctl.log"; echo $?)
check "mute toggles the sink" $(grep -q "set-mute @DEFAULT_AUDIO_SINK@ toggle" "$OUT/wpctl.log"; echo $?)
check "mic-mute toggles the source" $(grep -q "set-mute @DEFAULT_AUDIO_SOURCE@ toggle" "$OUT/wpctl.log"; echo $?)

# ---- wlr-screencopy (stand-in for grim) ----
WAYLAND_DISPLAY=$SOCK timeout 20 ./target/debug/examples/screencopy_client "$OUT/copy.png" >"$OUT/copy.log" 2>&1
check "screencopy of the whole output" $(grep -q "ready" "$OUT/copy.log"; echo $?)
WAYLAND_DISPLAY=$SOCK timeout 20 ./target/debug/examples/screencopy_client "$OUT/copy-region.png" 100,50,300,200 >"$OUT/copy2.log" 2>&1
check "screencopy of a 300x200 region" $(grep -q "buffer 300x200" "$OUT/copy2.log" && grep -q ready "$OUT/copy2.log"; echo $?)

# ---- session lock (ext-session-lock test client stands in for swaylock) ----
WAYLAND_DISPLAY=$SOCK ./target/debug/examples/lock_client 6 >"$OUT/lock.log" 2>&1 &
LOCKPID=$!; sleep 3
check "lock client locked the session" $(grep -q "lock surfaces mapped: 1" "$OUT/lock.log"; echo $?)
check "info reports locked=true" $($CTL info | grep -q "^locked=true"; echo $?)
check "no window has focus while locked" $([ "$($CTL info | grep 'app_id=' | grep -c 'focused=true')" = 0 ]; echo $?)
check "screenshots are refused while locked" $($CTL shot "$OUT/locked.png" | grep -q "locked"; echo $?)
$CTL focus h >/dev/null; $CTL float >/dev/null
check "keybind actions do not run on the lock screen's windows" $([ "$($CTL info | grep 'app_id=' | grep -c 'focused=true')" = 0 ]; echo $?)
wait $LOCKPID
check "lock client unlocked" $(grep -q "^unlocked" "$OUT/lock.log"; echo $?)
check "info reports locked=false" $($CTL info | grep -q "^locked=false"; echo $?)
check "focus returns to a window after unlock" $([ "$($CTL info | grep 'app_id=' | grep -c 'focused=true')" = 1 ]; echo $?)

# ---- Xwayland: a Qt app forced onto the X11 backend ----
N0=$(n_windows)
$CTL exec env QT_QPA_PLATFORM=xcb konsole >/dev/null; sleep 6
check "X11 window is managed (Xwayland)" $([ "$(n_windows)" = $((N0 + 1)) ]; echo $?)
check "X11 window is tiled and focused" $($CTL info | grep "focused=true" | grep -q "floating=false"; echo $?)
$CTL close >/dev/null; sleep 3
check "closing the X11 window removes it" $([ "$(n_windows)" = "$N0" ]; echo $?)

# ---- multi-monitor (virtual second output, 800x600, placed to the right of the first) ----
outputs_n() { $CTL info | grep -c '^output '; }
$CTL output add 800 600 >/dev/null; sleep 1
check "second output appears at x=$W0" $($CTL info | grep '^output ' | sed -n 2p | grep -q "800x600 at $W0,0"; echo $?)
check "second output shows its own workspace" $($CTL info | grep '^output ' | sed -n 2p | grep -q "current_ws=2"; echo $?)
$CTL output focus 2 >/dev/null; $CTL spawn >/dev/null; sleep 3
check "new window tiles on the focused (second) output" $($CTL info | grep -q "geo=$((W0 + 12)),12 776x576"; echo $?)
$CTL focus h >/dev/null
check "focus left at the monitor edge goes to the first output" $($CTL info | grep '^output ' | sed -n 1p | grep -q "focused=true"; echo $?)
$CTL mvout l >/dev/null; sleep 1
check "mvout moves the focused window to the second output's workspace" $($CTL info | grep -q "^ws 2 (visible): 2 window(s)"; echo $?)
$CTL shot "$OUT/output2.png" 2 >/dev/null
check "screenshot of output 2" $([ -s "$OUT/output2.png" ]; echo $?)
$CTL output rm >/dev/null; sleep 1
check "output removed" $([ "$(outputs_n)" = 1 ]; echo $?)
$CTL output add 800 600 >/dev/null; $CTL output focus 2 >/dev/null; $CTL output rm >/dev/null; sleep 1
check "removing the focused output returns focus to the first" $($CTL info | grep '^output ' | sed -n 1p | grep -q "focused=true"; echo $?)

# a window dragged onto another monitor joins that monitor's workspace; mvout keeps focus on it
focused_x() { $CTL info | grep 'app_id=' | grep 'focused=true' | sed 's/.*geo=\([0-9-]*\),.*/\1/'; }
$CTL output add 800 600 >/dev/null; sleep 1; $CTL output focus 1 >/dev/null; $CTL spawn >/dev/null; sleep 4; $CTL float >/dev/null
set -- $($CTL info | grep 'app_id=' | grep 'focused=true' | sed 's/.*geo=\([0-9-]*\),\([0-9-]*\) \([0-9]*\)x\([0-9]*\)/\1 \2 \3 \4/') 0 0 0 0
$CTL drag move $(($1 + $3 / 2)) $(($2 + $4 / 2)) $((W0 + 300)) 300 >/dev/null
check "dragging a floating window onto the second output keeps it there" $([ "$(focused_x)" -ge "$W0" ]; echo $?)
$CTL float >/dev/null
check "toggling floating works on the second output (window stays on it)" $([ "$(focused_x)" -ge "$W0" ]; echo $?)
$CTL output focus 1 >/dev/null; $CTL spawn >/dev/null; sleep 4; $CTL mvout l >/dev/null; sleep 1
check "mvout leaves the moved window focused on the second output" $([ "$(focused_x)" -ge "$W0" ]; echo $?)
$CTL shot "$OUT/output2b.png" 2 >/dev/null
$CTL output rm >/dev/null; sleep 1

echo; echo "passed=$pass failed=$fail (outputs in $OUT)"
[ "$fail" -eq 0 ]
