#!/usr/bin/env python3
"""Convert an image to a truecolor half-block ANSI logo for fastfetch (file-raw).

Usage: ansi-logo.py IMAGE WIDTH OUTPUT

Paper-white is removed with a flood fill from the borders, so light skin inside
the silhouette stays opaque (no holes in the face) while the anti-aliased white
fringe and small near-white islands at the outline become transparent. Edge
pixels are blended towards the Rusted desktop colour, never towards paper.
"""
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter

BG = (20, 21, 29)          # rusted desktop colour
PAPER = 16                 # flood-fill tolerance around white
PALE = 205                 # pale, low-saturation pixels (the shadow tip) count as paper…
PALE_SATURATION = 45
BOTTOM_BAND = 0.84         # …but only in the bottom band, where the shadow tip lives
TRIM_BOTTOM = 6            # source pixels shaved off the bottom (the little paint drip)


def paper_mask(im):
    work = im.copy()
    sentinel = (255, 0, 255)
    seeds = [(0, 0), (im.width - 1, 0), (0, im.height - 1), (im.width - 1, im.height - 1),
             (im.width // 2, 0), (im.width // 2, im.height - 1), (0, im.height // 2), (im.width - 1, im.height // 2)]
    for seed in seeds:
        if min(work.getpixel(seed)) > 240:
            ImageDraw.floodfill(work, seed, sentinel, thresh=PAPER)
    mask = Image.new('L', work.size, 0)
    src, dst = work.load(), mask.load()
    for y in range(work.height):
        for x in range(work.width):
            if src[x, y] == sentinel:
                dst[x, y] = 255
    mask = mask.filter(ImageFilter.MaxFilter(3))  # eat the anti-aliased fringe

    # The pale tip at the bottom of the painted shadow is not paper-white, so it survives
    # the flood fill. Clear pale/low-saturation pixels only in the bottom band: the hair
    # highlights higher up are left alone.
    r, g, b = im.split()
    low = ImageChops.darker(r, ImageChops.darker(g, b))
    high = ImageChops.lighter(r, ImageChops.lighter(g, b))
    saturation = ImageChops.subtract(high, low)
    pale = ImageChops.darker(
        low.point(lambda v: 255 if v > PALE else 0),
        saturation.point(lambda v: 255 if v < PALE_SATURATION else 0),
    )
    bottom = Image.new('L', im.size, 0)
    ImageDraw.Draw(bottom).rectangle((0, int(im.height * BOTTOM_BAND), im.width, im.height), fill=255)
    mask = ImageChops.lighter(mask, ImageChops.darker(pale, bottom))
    return mask


def art(path, width):
    im = Image.open(path).convert('RGB')
    paper = paper_mask(im)
    figure = paper.point(lambda v: 0 if v else 255)

    rgba = im.convert('RGBA')
    rgba.putalpha(figure)
    flat = Image.new('RGB', im.size, BG)
    flat.paste(rgba, mask=figure)

    box = figure.getbbox()
    pad = 2
    box = (max(0, box[0] - pad), max(0, box[1] - pad), min(im.width, box[2] + pad), max(1, min(im.height, box[3] + pad - TRIM_BOTTOM)))
    flat, paper = flat.crop(box), paper.crop(box)

    rows = max(1, round(width * flat.height / flat.width / 2))
    flat = flat.resize((width, rows * 2), Image.LANCZOS)
    paper = paper.resize((width, rows * 2), Image.LANCZOS).point(lambda v: 255 if v > 127 else 0)
    paper = paper.filter(ImageFilter.MaxFilter(3))

    lines = []
    for y in range(0, rows * 2, 2):
        line = []
        for x in range(width):
            top, bottom = flat.getpixel((x, y)), flat.getpixel((x, y + 1))
            t, b = paper.getpixel((x, y)) > 0, paper.getpixel((x, y + 1)) > 0
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


def main():
    if len(sys.argv) != 4:
        raise SystemExit('usage: ansi-logo.py IMAGE WIDTH OUTPUT')
    lines = art(sys.argv[1], int(sys.argv[2]))
    with open(sys.argv[3], 'w') as out:
        out.write('\n'.join(lines) + '\n')
    print(f'{sys.argv[3]}: {sys.argv[2]}x{len(lines)}')


if __name__ == '__main__':
    main()
