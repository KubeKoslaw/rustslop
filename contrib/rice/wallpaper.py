#!/usr/bin/env python3
"""Keep swaybg in sync with the [wallpaper] section of rusted-wm's config.

Started from autostart. Watches the config file and restarts swaybg whenever
`wallpaper.path` or `wallpaper.mode` changes, so the settings app applies a new
wallpaper live. An empty path means no swaybg; the compositor's background
colour shows instead.
"""
import os
import signal
import socket
import subprocess
import sys
import time
import tomllib
from pathlib import Path

try:
    import palette
except ImportError:  # palette.py lives next to this script in the installed rice
    palette = None

MODES = ("fill", "fit", "stretch", "center", "tile")


def config_path():
    if path := os.environ.get("RUSTED_CONFIG"):
        return Path(path)
    base = Path(os.environ.get("XDG_CONFIG_HOME") or (Path.home() / ".config"))
    return base / "rusted-wm" / "config.toml"


def read():
    try:
        with open(config_path(), "rb") as stream:
            data = tomllib.load(stream)
    except FileNotFoundError:
        return ("", "fill", False)
    except (OSError, tomllib.TOMLDecodeError) as error:
        print(f"wallpaper: cannot read {config_path()}: {error}", file=sys.stderr)
        return None
    section = data.get("wallpaper", {})
    image = str(section.get("path", "")).strip()
    mode = str(section.get("mode", "fill")).strip() or "fill"
    if mode not in MODES:
        print(f"wallpaper: unknown mode {mode!r}, using fill", file=sys.stderr)
        mode = "fill"
    pywal = bool(data.get("colors", {}).get("pywal", False))
    return (os.path.expanduser(os.path.expandvars(image)), mode, pywal)


def refresh_palette(image):
    """Regenerate the pywal-compatible palette when it does not match the wallpaper."""
    if palette is None or not image or not Path(image).is_file():
        return
    try:
        out = palette.wal_path()
        if out.is_file() and Path(image).stat().st_mtime <= out.stat().st_mtime:
            data = palette.load()
            if data and data.get("wallpaper") == image:
                return
        palette.generate(image)
    except Exception as error:  # never take the wallpaper down with it
        print(f"wallpaper: palette generation failed: {error}", file=sys.stderr)


def session_alive():
    """True while this compositor's control socket exists (the helper outlives swaybg otherwise)."""
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    display = os.environ.get("WAYLAND_DISPLAY")
    if not runtime or not display:
        return True
    try:
        with socket.socket(socket.AF_UNIX) as conn:
            conn.settimeout(0.5)
            conn.connect(str(Path(runtime) / f"{display}-ctl"))
        return True
    except OSError:
        return False


def stop(child):
    if child is None or child.poll() is not None:
        return
    child.terminate()
    try:
        child.wait(timeout=2)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()


def main():
    child = None
    current = None
    stamp = None
    running = True

    def stop_all(*_):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, stop_all)
    signal.signal(signal.SIGINT, stop_all)
    path = config_path()
    misses = 0
    while running:
        try:
            stat = path.stat()
            now = (stat.st_mtime_ns, stat.st_size)
        except OSError:
            now = None
        if now != stamp:
            stamp = now
            settings = read()
            if settings is not None and settings != current:
                current = settings
                stop(child)
                child = None
                image, mode, pywal = current
                if image and Path(image).is_file():
                    try:
                        child = subprocess.Popen(["swaybg", "-i", image, "-m", mode])
                    except FileNotFoundError:
                        print("wallpaper: swaybg is not installed", file=sys.stderr)
                    if pywal:
                        refresh_palette(image)
                elif image:
                    print(f"wallpaper: {image} does not exist", file=sys.stderr)
        # Exit with the session instead of lingering as a second watcher after a restart.
        misses = 0 if session_alive() else misses + 1
        if misses > 20:
            break
        time.sleep(0.5)
    stop(child)


if __name__ == "__main__":
    main()
