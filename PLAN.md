# Plan: Rust Wayland compositor (dynamic tiling + floating) on Smithay

## Context
Build a personal Wayland compositor in Rust, in `/home/kuba/Dokumenty/rusted` (currently empty; cargo 1.98 present, user runs Wayland). Design: Hyprland-style dynamic tiling with per-window floating toggle. Foundation: Smithay. Develop nested (winit backend) first, add TTY/DRM later. Working name: `rusted-wm`.

## Architecture
- `main.rs` – calloop event loop, backend selection (`--winit` default, `--tty` later).
- `state.rs` – `State` struct implementing Smithay handler traits (CompositorHandler, XdgShellHandler, SeatHandler, ShmHandler, OutputHandler, DataDeviceHandler).
- `backend/winit.rs` – nested window, renderer (GLES), output, frame loop. `backend/tty.rs` (phase 5): libseat session, udev, DRM/GBM, libinput.
- `layout/` – pure, unit-testable logic with no Smithay types: `tiling.rs` (dwindle/master-stack, returns rects), `floating.rs`, `workspace.rs`.
- `input.rs` – keybinding table (Super+Enter spawn terminal, Super+Q close, Super+F toggle floating, Super+1..9 workspaces, Super+h/j/k/l focus, Super+drag move/resize floating).
- `config.rs` – TOML config (keybinds, gaps, border colors, autostart) with later hot reload.
- `render.rs` – window elements, borders, background.

## Phases
1. **Skeleton**: `cargo init`, Smithay deps (`smithay` with `backend_winit`, `renderer_glow`, `wayland_frontend`, `desktop`), nested window with solid background, socket created. Verify: `WAYLAND_DISPLAY=wayland-1 foot` (or any client) connects.
2. **Windows & input**: xdg-shell toplevels mapped via `smithay::desktop::Space`, keyboard/pointer focus, spawn terminal and close keybinds.
3. **Tiling + floating**: layout module with unit tests; workspaces; per-window floating flag; gaps and borders; mouse move/resize for floating.
4. **Polish/protocols**: popups, layer-shell (for bars/launchers like waybar, fuzzel), xdg-decoration, clipboard/data-device, fractional scale, config file.
5. **Real session**: TTY/DRM backend, libinput, multi-monitor, session launcher entry, XWayland (optional).
6. **Extras**: animations, IPC socket, screenshots, screen-share portal.

## Dev approach with AI
Keep layout logic pure and test-driven; add one protocol/feature per step; use Smithay's `anvil` example and niri source as references. Commit per phase.

## Verification
- `cargo build` / `cargo test` (layout unit tests) each phase.
- Run nested: `cargo run`, then launch clients into its socket and check tiling, floating toggle, workspace switching.
- Phase 5: test on a spare TTY.
