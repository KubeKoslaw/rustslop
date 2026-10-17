#!/usr/bin/env python3
"""Wallpaper-derived colour palette for rusted-wm (pywal-compatible).

`wal` is used when it is installed; otherwise the palette is extracted from the
image with Pillow. Either way the result is written to ~/.cache/wal/colors.json
(honouring $RUSTED_WAL_FILE), which the compositor, Waybar and the settings app
already follow.

Usage: palette.py generate [IMAGE]   (default: [wallpaper] path from the config)
       palette.py show               print the current palette
"""
import colorsys
import json
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

from PIL import Image


def config_path():
    if path := os.environ.get("RUSTED_CONFIG"):
        return Path(path)
    base = Path(os.environ.get("XDG_CONFIG_HOME") or (Path.home() / ".config"))
    return base / "rusted-wm" / "config.toml"


def read_config():
    try:
        with open(config_path(), "rb") as stream:
            return tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError):
        return {}


def wal_path():
    if path := os.environ.get("RUSTED_WAL_FILE"):
        return Path(path)
    return Path.home() / ".cache" / "wal" / "colors.json"


def wallpaper():
    path = str(read_config().get("wallpaper", {}).get("path", "")).strip()
    return os.path.expanduser(os.path.expandvars(path))


def hexcolor(rgb):
    return "#%02x%02x%02x" % tuple(max(0, min(255, round(channel))) for channel in rgb)


def rgb(color):
    color = color.lstrip("#")
    return tuple(int(color[i : i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def luma(color):
    r, g, b = (channel / 255 for channel in color)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def darken(color, target):
    """Scale a colour so its luma is at most `target` (keeps the hue)."""
    if luma(color) <= target:
        return color
    return tuple(channel * target / max(luma(color), 1e-6) for channel in color)


def lighten(color, target):
    if luma(color) >= target:
        return color
    if luma(color) <= 1e-6:
        return (target * 255,) * 3
    factor = min(4.0, target / luma(color))
    return tuple(min(255, channel * factor) for channel in color)


def saturation(color):
    return colorsys.rgb_to_hsv(*(channel / 255 for channel in color))[1]


def extract(image_path):
    image = Image.open(image_path).convert("RGB")
    image.thumbnail((256, 256))
    quantized = image.quantize(colors=24, method=Image.Quantize.MEDIANCUT)
    palette = quantized.getpalette() or []
    entries = []
    for count, index in quantized.getcolors() or []:
        entries.append((count, tuple(palette[index * 3 : index * 3 + 3])))
    entries.sort(reverse=True)
    if not entries:
        raise SystemExit(f"{image_path}: no colours found")

    total = sum(count for count, _ in entries) or 1
    weighted = [(count / total, color) for count, color in entries]
    dominant = weighted[0][1]
    lightest = max(weighted, key=lambda item: luma(item[1]))[1]

    # A dark, slightly tinted surface from the wallpaper's dominant colour.
    background = darken(dominant, 0.09)
    foreground = lighten(lightest, 0.85)
    while (luma(foreground) + 0.05) / (luma(background) + 0.05) < 4.5:
        foreground = lighten(foreground, min(1.0, luma(foreground) + 0.05))

    # The accent is the most common vivid colour; color4 is what [colors] uses for focus.
    vivid = []
    for minimum in (0.35, 0.25, 0.15):
        vivid = [(weight, color) for weight, color in weighted if weight >= 0.02 and saturation(color) >= minimum and luma(color) >= 0.2]
        if vivid:
            break
    vivid.sort(reverse=True)
    accent = vivid[0][1] if vivid else rgb("#b4a1f5")
    if luma(accent) < 0.45:
        accent = lighten(accent, 0.45)
    accent_hue = colorsys.rgb_to_hsv(*[c / 255 for c in accent])[0]

    def hue_of(color):
        return colorsys.rgb_to_hsv(*[c / 255 for c in color])[0]

    def rotated(hue, saturation=0.45, value=0.85):
        return tuple(channel * 255 for channel in colorsys.hsv_to_rgb(hue % 1.0, saturation, value))

    # Prefer a complementary colour actually present in the image, otherwise rotate the hue.
    complement = next((color for _, color in vivid if 0.3 < abs(hue_of(color) - accent_hue) < 0.7), rotated(accent_hue + 0.5))
    analog = next((color for _, color in vivid if 0.04 < abs(hue_of(color) - accent_hue) <= 0.2), rotated(accent_hue + 0.08))
    soft = mix(accent, foreground, 0.45)

    palette = {
        1: darken(accent, 0.42),
        2: mix(accent, foreground, 0.2),
        3: analog,
        4: accent,
        5: complement,
        6: lighten(complement, 0.55),
    }
    colors = {"color0": hexcolor(background), "color7": hexcolor(foreground), "color8": hexcolor(mix(background, foreground, 0.45)), "color15": hexcolor(lighten(foreground, 0.95))}
    for index, color in palette.items():
        colors[f"color{index}"] = hexcolor(color)
        colors[f"color{9 + index - 1}"] = hexcolor(lighten(color, 0.72))
    colors["color4"] = hexcolor(accent)
    colors["color12"] = hexcolor(lighten(accent, 0.8))
    colors["color6"] = hexcolor(soft)
    return {
        "wallpaper": str(image_path),
        "alpha": "100",
        "special": {"background": hexcolor(background), "foreground": hexcolor(foreground), "cursor": hexcolor(accent)},
        "colors": colors,
    }


def generate(image=None):
    image = image or wallpaper()
    if not image or not Path(image).is_file():
        raise SystemExit(f"palette: no wallpaper image ({image or 'empty [wallpaper] path'})")
    out = wal_path()
    if shutil.which("wal") and not os.environ.get("RUSTED_WAL_FILE"):
        result = subprocess.run(["wal", "-i", image, "-n", "-q"], capture_output=True, text=True)
        if result.returncode == 0 and out.is_file():
            return json.loads(out.read_text())
        print(f"palette: wal failed ({result.stderr.strip() or result.returncode}); using the built-in extractor", file=sys.stderr)
    data = extract(image)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(data, indent=2) + "\n")
    return data


def load():
    try:
        return json.loads(wal_path().read_text())
    except (OSError, ValueError):
        return None


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "generate"
    if mode == "generate":
        data = generate(sys.argv[2] if len(sys.argv) > 2 else None)
        print(f"palette written to {wal_path()}")
        print(" ".join(data["colors"][f"color{i}"] for i in range(8)))
    elif mode == "show":
        data = load()
        if not data:
            raise SystemExit(f"palette: no palette at {wal_path()}")
        for name, value in [*data.get("special", {}).items(), *data.get("colors", {}).items()]:
            print(f"{name:10} {value}")
    else:
        raise SystemExit("usage: palette.py [generate [IMAGE] | show]")


if __name__ == "__main__":
    main()
