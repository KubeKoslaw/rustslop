# Rusted rice preset

Graphite/lavender desktop theme: Waybar islands fed by the compositor's own control socket, Fuzzel,
Mako, Foot, Swaylock, a NixOS wallpaper and matching GTK/Qt palettes.

## Install

```bash
cargo build --release --workspace       # rusted-wm must exist; rusted-settings is optional here
python3 contrib/rice/install.py --dry-run
python3 contrib/rice/install.py
```

The installer:

- checks for `waybar`, `fuzzel`, `mako`, `foot`, `swaybg`, `swaylock`, `playerctl` and Pillow before touching anything;
- copies app configs to their native locations (`~/.config/foot`, `fuzzel`, `mako`, `swaylock`, `gtk-3.0`,
  `gtk-4.0`, `qt5ct`, `qt6ct`) and everything else to `~/.config/rusted-rice`;
- edits `~/.config/rusted-wm/config.toml` through `rusted-wm config` (comment-preserving), sets the
  `[appearance]` effects, default apps and the autostart entries, then validates the file with `--check-config`;
- derives a dimmed lock-screen wallpaper from the shipped one;
- writes `~/.config/rusted-rice/session.env` (cursor, Qt style) which `rusted-session` sources.

Every file it replaces is first copied to `~/.config/rusted-rice-backups/<timestamp>/`, together with a
`manifest.json` recording hashes.

## Restore

```bash
python3 contrib/rice/install.py --restore ~/.config/rusted-rice-backups/<timestamp>/manifest.json
```

Restore refuses to overwrite files that changed since installation; move those aside first.

## Notes

- Waybar is started by `waybar.py supervise`, one bar per monitor; workspace clicks switch that monitor only.
- `wallpaper.py` keeps swaybg in sync with the `[wallpaper]` config section, so the settings app's Wallpaper
  page (file picker or thumbnail gallery) applies a new image without restarting the session.
- `palette.py` derives a pywal-compatible palette (`~/.cache/wal/colors.json`) from the wallpaper, using
  `wal` when installed and a Pillow extractor otherwise. Switch pywal on in Settings → Theme; `wallpaper.py`
  regenerates the palette whenever the wallpaper changes and the compositor/Waybar follow it live.
- `fastfetch/config.jsonc` + `logo.txt` are themed to match and `fastfetch.bash` shows them once in every
  interactive foot shell. The installer sources that snippet from `~/.bashrc` (marker-guarded, backed up,
  `--no-bashrc` opts out); other terminals and non-interactive shells are unaffected.
- Effects are live-reloadable: set `appearance.blur_enabled=false` (etc.) and reload. Disabling the whole
  `[appearance]` section restores the rectangular renderer.
- The preset does not copy anything from the wallpaper source archive, only the wallpaper itself
  (`wallpapers/nixos.png`).
