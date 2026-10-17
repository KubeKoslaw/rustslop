# PROGRESS LOG — rusted-wm

Handoff document so any AI tool (or human) can continue. **Update this file after every big change or fix.** Roadmap is in `PLAN.md`.

## Project
Personal Wayland compositor in Rust on **Smithay 0.7.0**. Goal: Hyprland-style dynamic tiling + per-window floating toggle. Developed nested (winit backend) inside the current Wayland session; TTY/DRM backend comes in Phase 5.

Env: Linux (Gentoo), Wayland session, cargo 1.98.1, edition 2024. System libs present: wayland-server, xkbcommon.
(Project folder was originally `~/Dokumenty/rusted`, now `~/Dokumenty/rustslop`.)

## Current status: Phase 3 DONE (tiling + floating + workspaces + borders), verified via control socket + screenshots
Phase 4 (popups, layer-shell, decorations, clipboard polish, config file) is next. Real-keyboard path (Alt keybinds through winit) has NOT been verified by the agent — see "Testing" below.

## Layout
- `Cargo.toml` — deps: `smithay` (features `backend_winit, renderer_glow, wayland_frontend, desktop`), `tracing`, `tracing-subscriber` (`env-filter`), `png`.
- `src/main.rs` — setup (Display, Output, State, sockets) + polling loop: winit events -> control socket commands -> render (`desktop::space::render_output`, borders as custom elements) -> frame callbacks -> clients -> submit -> offscreen screenshot if requested.
- `src/state.rs` — `State` struct + Wayland handler impls (Compositor, Shm, Output, XdgShell, Seat, DataDevice). `window_for_surface` searches all workspaces.
- `src/wm.rs` — window management: `Workspace`/`Entry` (window, floating flag, float rect, border buffers), `add_window`, `remove_window`, `relayout`, focus (`focus_window/focus_top/focus_next/focus_dir`), `toggle_floating`, `switch_workspace`, `move_to_workspace`, mouse drags (`begin_drag/update_drag`), `border_elements`. Only the current workspace's windows are mapped in the `Space`. Floating windows use z-index Shell+1 so they stay above tiled ones. `relayout` re-maps in existing z-order (Space::map_element raises).
- `src/layout.rs` — PURE layout math (`dwindle`, `default_float_rect`) with unit tests (`cargo test`). Add new layouts here.
- `src/input.rs` — `Action` enum, keybinds, pointer/click/axis handling. Modifier = **Alt** while nested (Super is taken by the host). Keys: `Alt+Enter` terminal (env `RUSTED_TERMINAL`, default konsole), `Alt+Q` close, `Alt+Tab` cycle focus, `Alt+h/j/k/l` focus direction, `Alt+F` toggle floating, `Alt+1..9` workspace, `Alt+Shift+1..9` move window to workspace, `Alt+Shift+E` quit. Mouse: `Alt+LeftDrag` move, `Alt+RightDrag` resize (tiled windows become floating); click focuses.
- `src/control.rs` — debug control socket (see Testing). `tools/ctl.py` is the client.
- Constants (gap 12, border 2, colours, 9 workspaces) are in `src/wm.rs`.

## Testing (important for AI agents)
No input devices are needed. Start the compositor (opens a nested window), then drive it:
```bash
./target/debug/rusted-wm &                  # prints control socket path: $XDG_RUNTIME_DIR/rusted-N-ctl
tools/ctl.py info                           # dump outputs/workspaces/windows (geometry, floating, focus)
tools/ctl.py spawn                          # open terminal (same as Alt+Enter)
tools/ctl.py shot /tmp/x.png                # offscreen screenshot -> view the PNG
```
Commands: `spawn`, `exec CMD ARGS`, `close`, `next`, `float`, `quit`, `focus h|j|k|l`, `ws N`, `mv N`, `motion X Y`, `click X Y [right]`, `drag move|resize X1 Y1 X2 Y2`, `shot PATH`, `info`.
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

## Known limitations / TODO
- Polling loop with sleep instead of calloop (planned to move to calloop in Phase 2).
- Only the dwindle layout; no layout switching, no swap/move of tiled windows, no resize of tile ratios.
- Borders are drawn above all windows (a floating window overlapping another window's border would be overdrawn by that border).
- Output scale is assumed 1.0.
- Child processes spawned by keybind are never `wait()`ed (zombies on exit) — fix with double-fork or a SIGCHLD handler.
- No client cursor image support (host cursor is shown).
- Popups, layer-shell, decorations not implemented.

## How to run
```bash
cargo build
./target/debug/rusted-wm            # RUST_LOG=debug for more logs
WAYLAND_DISPLAY=rusted-1 QT_QPA_PLATFORM=wayland konsole   # or press Alt+Enter inside the nested window
```
