"""Convert an image to a truecolor half-block ANSI logo (fastfetch file-raw).

Usage: ansi-logo.py IMAGE WIDTH OUTPUT
White paper stays transparent, so the art sits on the terminal background.
"""
from PIL import Image
import sys

def bg(pixel):
    r, g, b = pixel
    return r > 238 and g > 236 and b > 228

def art(path, width, pad=10):
    im = Image.open(path).convert('RGB')
    mask = im.convert('L').point(lambda p: 255 if p < 245 else 0)
    box = mask.getbbox()
    im = im.crop((max(0, box[0] - pad), max(0, box[1] - pad), min(im.width, box[2] + pad), min(im.height, box[3] + pad)))
    rows = max(1, round(width * im.height / im.width / 2))
    im = im.resize((width, rows * 2), Image.LANCZOS)
    lines = []
    for y in range(0, rows * 2, 2):
        line = []
        for x in range(width):
            top, bottom = im.getpixel((x, y)), im.getpixel((x, y + 1))
            t, b = bg(top), bg(bottom)
            if t and b:
                line.append('\033[49m ')
            elif t:
                line.append(f'\033[38;2;{bottom[0]};{bottom[1]};{bottom[2]}m\033[49m▄')
            elif b:
                line.append(f'\033[38;2;{top[0]};{top[1]};{top[2]}m\033[49m▀')
            else:
                line.append(f'\033[38;2;{top[0]};{top[1]};{top[2]}m\033[48;2;{bottom[0]};{bottom[1]};{bottom[2]}m▀')
        lines.append(''.join(line) + '\033[0m')
    return lines

if __name__ == '__main__':
    width = int(sys.argv[2])
    open(sys.argv[3], 'w').write('\n'.join(art(sys.argv[1], width)) + '\n')
    print(f'{sys.argv[3]}: {width}x{len(art(sys.argv[1], width))}')
