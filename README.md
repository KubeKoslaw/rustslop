# rustslop
yet another desktop manager ( but in rust ) 
fun project

A tiling + floating Wayland compositor written in Rust on [Smithay](https://github.com/Smithay/smithay).

## Features
- Dwindle tiling, per-window floating, fullscreen, swap/resize, focus borders, 9 workspaces per monitor
- Multi-monitor: per-output workspaces, hotplug, moving windows between outputs, fractional scale,
  and per-connector mode/placement (`[outputs.DP-1] mode = "1920x1080@75"`, `position = "1920,0"`)
- xdg-shell (popups, no client decorations), wlr-layer-shell (bars/launchers), clipboard + primary selection
- XWayland, grim-style screenshots/screencopy, session lock, idle-inhibit, presentation-time
- TOML config with configurable keybinds, validation, live reload and pywal palettes (`config.example.toml`)
- Optional effects in `[appearance]`: rounded window corners with borders that follow them, soft shadows,
  backdrop blur behind translucent panels and terminals, and window/layout/workspace animations
- `rusted-wm config` CLI (comment-preserving get/set/bind/autostart), a control socket, and a GTK4 settings app
- Runs nested in another desktop (development), as a real session on a TTY (DRM/KMS + libinput + libseat),
  or headless on a GPU render node for automated tests

## Build and run
```bash
cargo build --release
./target/release/rusted-wm            # nested window when started inside another desktop
./target/release/rusted-wm --tty      # real display, from a free VT (needs seatd or elogind)
```
Default modifier: `Super` on a real session, `Alt` when nested. `Mod+Enter` opens the terminal, `Mod+Q` closes, `Mod+Shift+E` quits.

## Login session
```bash
cargo build --release
doas contrib/install.sh        # or sudo; use `uninstall` to remove
```
Then pick **Rusted** in your display manager (SDDM etc.), or run `exec rusted-session` from a TTY. Session log: `~/.local/state/rusted-wm/session.log`.

## Appearance and rice
The shipped defaults keep the old rectangular look. The rice preset enables the graphite/lavender theme
together with rounded corners, shadows, blur and animations:

```bash
cargo build --release --workspace      # compositor, session launcher and the GTK4 settings app
python3 contrib/rice/install.py --dry-run
python3 contrib/rice/install.py        # backs up every file it touches, then installs
```
It checks dependencies first, edits `~/.config/rusted-wm/config.toml` through `rusted-wm config` (so comments
and layout are preserved) and validates the result with `--check-config`. Restart the session afterwards.
To undo: `python3 contrib/rice/install.py --restore ~/.config/rusted-rice-backups/<stamp>/manifest.json`.
Everything installed by the preset lives in `~/.config/rusted-rice` plus the native config dirs it lists.

`rusted-settings` (built from the workspace) edits the same config through the same CLI, with schema-driven
pages for general options, keybinds, default apps, input, colors, appearance and audio.

## Testing
`tools/smoke.sh` drives a nested instance through its control socket (`tools/ctl.py`) and checks tiling,
floating, fullscreen, workspaces, clipboard, layer-shell, XWayland, lock, screenshots and pywal.

`python3 tools/appearance-test.py` is the GPU pixel test for the effects: rounded clipping, borders, shadows,
backdrop blur (including panels' transparent gaps), scaling, multi-monitor workspaces, screenshot/screencopy
parity, animation settling and lock-screen isolation. It needs a DRM render node and Pillow.
