#!/usr/bin/env python3
"""Convert an image to a monochrome, edge-aware ASCII logo for fastfetch.

Usage: ascii-logo.py IMAGE WIDTH OUTPUT

Paper-white is found with a flood fill from the borders, so light skin inside the
silhouette stays part of the portrait. Edges (Sobel, lightly blurred) are drawn
with the densest characters, which keeps the face and hair readable at low
resolution; shading uses "@%#*+=-:. ". Output lines are prefixed with $1 so the
fastfetch config can colour the whole logo with logo.color.1.
"""
import sys

from PIL import Image, ImageDraw, ImageFilter

RAMP = " .:-=+*#%@"        # light -> dark
EDGE = 28                  # Sobel magnitude that counts as an outline


def paper_mask(im):
    work = im.copy()
    sentinel = (255, 0, 255)
    seeds = [(0, 0), (im.width - 1, 0), (0, im.height - 1), (im.width - 1, im.height - 1),
             (im.width // 2, 0), (im.width // 2, im.height - 1), (0, im.height // 2), (im.width - 1, im.height // 2)]
    for seed in seeds:
        if min(work.getpixel(seed)) > 240:
            ImageDraw.floodfill(work, seed, sentinel, thresh=16)
    mask = Image.new('L', work.size, 0)
    src, dst = work.load(), mask.load()
    for y in range(work.height):
        for x in range(work.width):
            if src[x, y] == sentinel:
                dst[x, y] = 255
    return mask.filter(ImageFilter.MaxFilter(3))


def art(path, width):
    im = Image.open(path).convert('RGB')
    paper = paper_mask(im)
    figure = paper.point(lambda v: 0 if v else 255)

    rgba = im.convert('RGBA')
    rgba.putalpha(figure)
    flat = Image.new('RGB', im.size, (20, 21, 29))
    flat.paste(rgba, mask=figure)

    box = figure.getbbox()
    pad = 4
    box = (max(0, box[0] - pad), max(0, box[1] - pad), min(im.width, box[2] + pad),
           max(1, min(im.height, box[3] + pad - 6)))
    flat, figure = flat.crop(box), figure.crop(box)

    height = max(1, round(width * figure.height / figure.width * 0.5))
    gray = flat.convert('L').resize((width, height), Image.LANCZOS)
    edges = gray.filter(ImageFilter.GaussianBlur(0.6)).filter(ImageFilter.FIND_EDGES)
    mask = figure.resize((width, height), Image.LANCZOS).point(lambda v: 255 if v > 127 else 0)

    lines = []
    for y in range(height):
        row = []
        for x in range(width):
            if not mask.getpixel((x, y)):
                row.append(' ')
                continue
            lum = gray.getpixel((x, y)) / 255
            if edges.getpixel((x, y)) > EDGE:
                row.append('#' if lum < 0.55 else '%')
            else:
                row.append(RAMP[min(len(RAMP) - 1, int(lum * (len(RAMP) - 1) + 0.5))])
        lines.append(''.join(row).rstrip())
    return lines


def main():
    if len(sys.argv) != 4:
        raise SystemExit('usage: ascii-logo.py IMAGE WIDTH OUTPUT')
    lines = art(sys.argv[1], int(sys.argv[2]))
    with open(sys.argv[3], 'w') as out:
        out.write('\n'.join('$1' + line for line in lines) + '\n')
    print(f'{sys.argv[3]}: {sys.argv[2]} cols, {len(lines)} lines')


if __name__ == '__main__':
    main()
