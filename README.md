# rustslop
yet another desktop manager ( but in rust ) 
fun project

A tiling + floating Wayland compositor written in Rust on [Smithay](https://github.com/Smithay/smithay).

## Features
- Dwindle tiling, per-window floating, fullscreen, 9 workspaces, focus borders
- Swap windows, resize splits, Alt/Super + mouse drag for floating windows
- xdg-shell (popups, no client decorations), wlr-layer-shell (bars/launchers), clipboard + primary selection
- TOML config with configurable keybinds and live reload (`config.example.toml`)
- Runs nested in another desktop (development) or as a real session on a TTY (DRM/KMS + libinput + libseat)

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

## Testing
`tools/smoke.sh` drives a nested instance through its control socket (`tools/ctl.py`) and checks tiling, floating, fullscreen, workspaces, clipboard and layer-shell.
