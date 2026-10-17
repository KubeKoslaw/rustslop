# PROGRESS LOG — rusted-wm

Handoff document so any AI tool (or human) can continue. **Update this file after every big change or fix.** Roadmap is in `PLAN.md`.

## Project
Personal Wayland compositor in Rust on **Smithay 0.7.0**. Goal: Hyprland-style dynamic tiling + per-window floating toggle. Developed nested (winit backend) inside the current Wayland session; TTY/DRM backend comes in Phase 5.

Env: Linux (Gentoo), Wayland session, cargo 1.98.1, edition 2024. System libs present: wayland-server, xkbcommon.
(Project folder was originally `~/Dokumenty/rusted`, now `~/Dokumenty/rustslop`.)

## Current status: Phase 5a DONE (calloop event loop) — branch `feat/phase-5a-calloop`
Phase 5b (real TTY/DRM backend) is BLOCKED on a system package: `libseat` is not installed (`pkg-config libseat` fails; Smithay's `backend_session_libseat` needs it). User must run `sudo emerge sys-auth/seatd` (enable USE `elogind` since this machine runs elogind) — then add Smithay features `backend_drm, backend_gbm, backend_libinput, backend_session_libseat, backend_udev` and write `src/backend/tty.rs`. libinput, gbm, libdrm, udev, libdisplay-info are already present. 5b can only be fully tested on a spare TTY by the user (the agent cannot see a TTY); plan: compile-check + logs, user verifies.

## Layout
- `Cargo.toml` — deps: `smithay` (features `backend_winit, renderer_glow, wayland_frontend, desktop`), `tracing`, `tracing-subscriber` (`env-filter`), `png`.
- `src/config.rs` — TOML config (`$RUSTED_CONFIG` or `~/.config/rusted-wm/config.toml`; example in `config.example.toml`): gap, border, terminal, modifier (alt|super), colours, autostart, keybinds (merged over defaults, `"none"` removes). Parsed into `Binding`s. Invalid values are reported and fall back to defaults (never fatal). Unit tested.
- `examples/layer_client.rs` — tiny layer-shell test bar (`WAYLAND_DISPLAY=rusted-1 ./target/debug/examples/layer_client top 40`).
- `src/main.rs` — builds State and a **calloop** `EventLoop<State>` with sources: Wayland display (dispatch_clients), listening socket (`rusted-N`), control socket, and the backend. Exits when `state.running` is false.
- `src/backend/winit.rs` — nested backend: `Winit::new` (window + Output), `Winit::register` (winit input/resize source + 16 ms frame timer that renders via `render_output`, sends frame callbacks, submits, and serves pending screenshots). A tty backend should live beside it as `backend/tty.rs`.
- `src/state.rs` — `State` struct + Wayland handler impls (Compositor, Shm, Output, XdgShell, Seat, DataDevice). `window_for_surface` searches all workspaces.
- `src/wm.rs` — window management: `Workspace`/`Entry` (window, floating flag, float rect, border buffers), `add_window`, `remove_window`, `relayout`, focus (`focus_window/focus_top/focus_next/focus_dir`), `toggle_floating`, `switch_workspace`, `move_to_workspace`, mouse drags (`begin_drag/update_drag`), `border_elements`. Only the current workspace's windows are mapped in the `Space`. Floating windows use z-index Shell+1 so they stay above tiled ones. `relayout` re-maps in existing z-order (Space::map_element raises).
- `src/layout.rs` — PURE layout math (`dwindle`, `default_float_rect`) with unit tests (`cargo test`). Add new layouts here.
- `src/input.rs` — `Action` enum, keybind matching against `config.bindings`, pointer/click/axis handling, popup dismissal, layer-aware `surface_under`. Default modifier = **Alt** while nested (Super is taken by the host; set `modifier = "super"` for a real session). Default keys: `mod+Enter` terminal, `mod+Q` close, `mod+Tab` cycle focus, `mod+h/j/k/l` focus direction, `mod+F` toggle floating, `mod+1..9` workspace, `mod+Shift+1..9` move window, `mod+Shift+R` reload config, `mod+Shift+E` quit. Mouse: `Alt+LeftDrag` move, `Alt+RightDrag` resize (tiled windows become floating); click focuses.
- `src/control.rs` — debug control socket (see Testing). `tools/ctl.py` is the client.
- Constants (gap 12, border 2, colours, 9 workspaces) are in `src/wm.rs`.

## Testing (important for AI agents)
**Run `tools/smoke.sh [outdir]` after every change** — builds, starts the nested compositor, and checks tiling, floating, focus, drag, workspaces, close, clipboard and layer-shell (12 checks, exit 0 = all pass). Screenshots go to outdir.

No input devices are needed. Start the compositor (opens a nested window), then drive it:
```bash
./target/debug/rusted-wm &                  # prints control socket path: $XDG_RUNTIME_DIR/rusted-N-ctl
tools/ctl.py info                           # dump outputs/workspaces/windows (geometry, floating, focus)
tools/ctl.py spawn                          # open terminal (same as Alt+Enter)
tools/ctl.py shot /tmp/x.png                # offscreen screenshot -> view the PNG
```
Commands: `reload` (re-read config), `spawn`, `exec CMD ARGS`, `close`, `next`, `float`, `quit`, `focus h|j|k|l`, `ws N`, `mv N`, `motion X Y`, `click X Y [right]`, `drag move|resize X1 Y1 X2 Y2`, `shot PATH`, `info`.
Real keyboard test: `ydotool` is installed but needs `ydotoold` running and `/dev/uinput` access (root-only by default) and the nested window must have host focus.

## Log
### Phase 1 — skeleton (2026-10-17)
- `cargo init --name rusted-wm`, added Smithay deps.
- Based code on Smithay's `examples/minimal.rs` (in `~/.cargo/registry/src/*/smithay-0.7.0/examples/`), split into state.rs/main.rs.
- Fix: `tracing_subscriber::EnvFilter` needs the `env-filter` feature -> `cargo add tracing-subscriber --features env-filter`.
- Note: `std::env::set_var` is `unsafe` in edition 2024 (wrapped in `unsafe`).
- Verified: builds; running it opens a nested winit window, logs `listening on WAYLAND_DISPLAY=rusted-1`, socket `$XDG_RUNTIME_DIR/rusted-1` exists. Not yet verified with a real client (no foot/alacritty/kitty/weston-terminal installed at test time; install one and run `WAYLAND_DISPLAY=rusted-1 <client>`).

### Phase 2 — windows & input (2026-10-17)
- `state.rs`: `new_toplevel` wraps the surface in `desktop::Window`, maps into `Space`, relayouts, focuses; `toplevel_destroyed` unmaps; `commit` calls `window.on_commit()` for the root surface. Added `OutputHandler`, `delegate_output!`, xdg-output.
- `main.rs`: creates `Output` (+ global, Transform::Flipped180 as required by winit backend), maps it into the Space, renders via `desktop::space::render_output` + `OutputDamageTracker`, sends frame callbacks with `Window::send_frame`, submits only when there is damage. Handles `WinitEvent::Resized` (updates output mode + relayout) and `CloseRequested`.
- Verified automatically: konsole (`QT_QPA_PLATFORM=wayland`, `WAYLAND_DISPLAY=rusted-1`) connects, logs `new toplevel`, disconnects cleanly. NOT visually verified (no screenshot possible from the agent): check that the konsole window fills the nested window, typing works, Alt+Enter/Alt+Q/Alt+Tab work, mouse click focus works.
- Harmless startup log: `[EGL] BAD_SURFACE eglQuerySurface` (buffer_age queried before first frame).

### Phase 3 — tiling, floating, workspaces, borders, debug tooling (2026-10-17)
- Added `layout.rs` (dwindle, 5 unit tests), `wm.rs`, `control.rs`, `tools/ctl.py`.
- Borders: 4 `SolidColorBuffer`s per window (stable ids so damage tracking works), drawn just OUTSIDE the window rect (needs GAP >= BORDER) as custom render elements; focused = blue.
- Dialogs (toplevel with a parent) start floating.
- Tiled windows get xdg Tiled* states; windows on other workspaces are unmapped and deactivated.
- BUG FIXED: taking a screenshot by `copy_framebuffer` on the window's own framebuffer made the next `eglSwapBuffers` fail (`BAD_SURFACE`/`BAD_ALLOC`, compositor died). Fix: render into an offscreen `GlesTexture` (`Offscreen::create_buffer` + `Bind::bind`) and read that back; PNG rows are flipped because GL reads bottom-up (output is Flipped180).
- BUG FIXED: borrow error from `self.space.elements().next()` temporary — collect to a local first.
- Verified through the control socket: 3-window dwindle tiling (screenshot checked), float toggle, focus direction, drag move/resize, move to workspace, workspace switch, close removes window.

### Phase 4 — protocols, config (2026-10-17)
- Popups: `PopupManager`; `new_popup` sets geometry from the positioner, `commit` forwards to `popups.commit` and sends the initial configure. Verified with a Konsole context menu. Simple popup grab replacement: a click outside every popup of the focused window dismisses them (`dismiss_popups_outside`). Real `xdg_popup.grab` is NOT implemented.
- xdg-decoration: always ServerSide (= clients draw no title bars). Verified (Konsole lost its title bar).
- wlr-layer-shell: `WlrLayerShellState` + `layer_map_for_output`. Tiling area = `non_exclusive_zone()` so bars reserve space; `relayout` after each layer commit/destroy. Exclusive-keyboard layers (launchers) get focus; clicking a layer that accepts keyboard focuses it. Layers receive frame callbacks. BUG FIXED: Smithay's `arrange()` never sends the initial configure, so clients never mapped — the commit handler now calls `layer_surface().send_pending_configure()`. Verified with `examples/layer_client.rs` (bar maps, windows shrink by its exclusive zone, area restores when it exits).
- Selection: primary selection + wlr-data-control added; `SeatHandler::focus_changed` now calls `set_data_device_focus`/`set_primary_focus` (clipboard follows keyboard focus). Verified `wl-copy`/`wl-paste` round trip.
- Also added: viewporter, config file + live reload (`reload` command/keybind), autostart, child processes are reaped by a waiter thread (fixes zombies).
- Config test coverage: 4 unit tests (defaults, overrides/removal, bad input, colours). Live reload verified (gap 12->30->0, bad colour reported).
- Pitfall for agents: never `pkill -f <pattern>` where the pattern appears in your own command line — it kills the shell. Use `pkill -x name`.

### Phase 5a — calloop refactor (2026-10-17)
- Replaced the hand-written polling loop (sleep 4 ms) with a calloop `EventLoop<State>`; backend code moved to `src/backend/winit.rs`; frame callbacks/popup cleanup moved to `State::send_frames` (wm.rs); `State.pending_shot` carries screenshot requests from the control socket to the backend.
- Why: libinput, libseat sessions, udev and DRM page-flip events are all calloop event sources, so the TTY backend needs this.
- Gotchas: `Control` implements `AsFd` (via its listener) so it can be a `Generic` source; the display source flushes clients after each dispatch; a frame flushes clients before `submit` (swap may block).
- Verified with the new `tools/smoke.sh`: 12/12 checks pass (same behaviour as before the refactor).

## Known limitations / TODO
- Polling loop with sleep instead of calloop (planned to move to calloop in Phase 2).
- Only the dwindle layout; no layout switching, no swap/move of tiled windows, no resize of tile ratios.
- Borders are drawn above all windows (a floating window overlapping another window's border would be overdrawn by that border).
- Output scale is assumed 1.0.
- No real popup grab (see Phase 4); Qt menus opened by an instantaneous synthetic click (press+release back to back) may close immediately.
- Layer surfaces are always mapped on the single output; the requested output is ignored. Keyboard-interactive `OnDemand` layers only get focus on click.
- No fractional scale / cursor-shape / xdg-activation / presentation-time yet.
- No client cursor image support (host cursor is shown).
- Popups, layer-shell, decorations not implemented.

## How to run
```bash
cargo build
./target/debug/rusted-wm            # RUST_LOG=debug for more logs
WAYLAND_DISPLAY=rusted-1 QT_QPA_PLATFORM=wayland konsole   # or press Alt+Enter inside the nested window
```
