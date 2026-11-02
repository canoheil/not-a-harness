#!/usr/bin/env python3
"""Render the app icon (icon.png + icon.ico) from icons/logo.svg.

Pure-python scanline rasterizer - no image libraries needed. The output
has a real alpha channel: the rounded-square plate is opaque black, the
horse is the logo gradient, everything else is fully transparent (the
old icon had semi-white edge pixels which showed up as white corners).

Usage: python3 scripts/render_icon.py   (from the repo root)
"""

import binascii
import re
import struct
import zlib

SVG = "crates/desktop/icons/logo.svg"
PNG_OUT = "crates/desktop/icons/icon.png"
ICO_OUT = "crates/desktop/icons/icon.ico"
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
PNG_SIZE = 512

SS = 4  # supersampling factor (4x4 samples per output pixel)


# ----------------------------------------------------------------- svg parse

def parse_svg(path):
    src = open(path, encoding="utf-8").read()
    stops = re.findall(r'offset="([\d.]+)%"\s+stop-color="(#[0-9a-fA-F]{6})"', src)
    grad = [(float(o) / 100.0, tuple(int(c[i:i + 2], 16) for i in (1, 3, 5)))
            for o, c in stops]
    m = re.search(r'x1="([\d.]+)"\s+y1="([\d.]+)"\s+x2="([\d.]+)"\s+y2="([\d.]+)"', src)
    gline = tuple(float(m.group(i)) for i in range(1, 5))

    elements = []  # each element: list of subpaths; subpath: list of (x, y)
    for pm in re.finditer(r'<path\s+d="(.*?)"', src, re.S):
        subs, cur = [], []
        for tok in re.finditer(r'([MLZmlz])|(-?[\d.]+),(-?[\d.]+)', pm.group(1)):
            if tok.group(1):
                if tok.group(1) in "Zz" and cur:
                    subs.append(cur)
                    cur = []
            else:
                x, y = float(tok.group(2)), float(tok.group(3))
                if not cur:
                    pass
                cur.append((x, y))
        if cur:
            subs.append(cur)
        elements.append(subs)
    for pm in re.finditer(r'<polygon\s+points="([^"]+)"', src):
        pts = [tuple(map(float, p.split(","))) for p in pm.group(1).split()]
        elements.append([pts])
    return grad, gline, elements


# ---------------------------------------------------------------- rasterizer

def transform(subs, s, tx, ty):
    return [[(x * s + tx, y * s + ty) for (x, y) in sub] for sub in subs]


def spans_for(elements, y):
    """Even-odd x-spans of every element at row y (already unioned)."""
    row = None  # lazily allocated
    for subs in elements:
        xs = []
        for sub in subs:
            n = len(sub)
            for i in range(n):
                x1, y1 = sub[i]
                x2, y2 = sub[(i + 1) % n]
                if (y1 <= y < y2) or (y2 <= y < y1):
                    xs.append(x1 + (y - y1) * (x2 - x1) / (y2 - y1))
        xs.sort()
        if xs:
            if row is None:
                row = bytearray(W)
            for i in range(0, len(xs) - 1, 2):
                a = max(0, int(xs[i] + 0.5))
                b = min(W, int(xs[i + 1] + 0.5))
                if b > a:
                    row[a:b] = b"\x01" * (b - a)
    return row


def in_rounded_rect(x, y, size, radius):
    half = size / 2.0
    r = radius
    cx = cy = half
    dx = abs(x - cx)
    dy = abs(y - cy)
    if dx <= half - r or dy <= half - r:
        return True
    return (dx - (half - r)) ** 2 + (dy - (half - r)) ** 2 <= r * r


def grad_color(grad, gline, x, y):
    x1, y1, x2, y2 = gline
    dx, dy = x2 - x1, y2 - y1
    t = ((x - x1) * dx + (y - y1) * dy) / (dx * dx + dy * dy)
    t = 0.0 if t < 0 else (1.0 if t > 1 else t)
    for i in range(len(grad) - 1):
        o0, c0 = grad[i]
        o1, c1 = grad[i + 1]
        if t <= o1 or i == len(grad) - 2:
            u = 0.0 if o1 == o0 else (t - o0) / (o1 - o0)
            u = 0.0 if u < 0 else (1.0 if u > 1 else u)
            return tuple(c0[k] + (c1[k] - c0[k]) * u for k in range(3))
    return grad[-1][1]


def render(size, grad, gline, elements):
    global W
    W = size * SS  # everything below lives in shading units (size * SS)
    # fit the horse (viewBox 1000x1207) into the plate at 80% height
    s = size * SS * 0.80 / 1207.0
    tx = (size * SS - 1000.0 * s) / 2.0
    ty = (size * SS - 1207.0 * s) / 2.0
    telems = [transform(e, s, tx, ty) for e in elements]
    radius = size * SS * 0.16

    out = bytearray(size * size * 4)
    acc = [0.0] * (size * 4)  # premultiplied row accumulator over all sy
    for oy in range(size):
        for i in range(size * 4):
            acc[i] = 0.0
        for sy in range(SS):
            y = oy * SS + sy + 0.5
            row = spans_for(telems, y)
            for ox in range(size):
                o = ox * 4
                for sx in range(SS):
                    x = ox * SS + sx + 0.5
                    if row is not None and row[ox * SS + sx]:
                        cr, cg, cb = grad_color(grad, gline,
                                                (x - tx) / s, (y - ty) / s)
                        acc[o] += cr
                        acc[o + 1] += cg
                        acc[o + 2] += cb
                        acc[o + 3] += 255.0
                    elif in_rounded_rect(x, y, radius * 6.25, radius):
                        acc[o + 3] += 255.0
        base = oy * size * 4
        for ox in range(size):
            o = base + ox * 4
            a = acc[ox * 4 + 3]
            if a > 0:
                # straight alpha: color sum over covered samples, where
                # each sample contributed a=255 to the alpha sum
                out[o] = int(acc[ox * 4] * 255.0 / a + 0.5)
                out[o + 1] = int(acc[ox * 4 + 1] * 255.0 / a + 0.5)
                out[o + 2] = int(acc[ox * 4 + 2] * 255.0 / a + 0.5)
            out[o + 3] = int(a / (SS * SS) + 0.5)
    return out, size


# ---------------------------------------------------------------- png + ico

def png_chunk(typ, body):
    return struct.pack(">I", len(body)) + typ + body + \
        struct.pack(">I", binascii.crc32(typ + body) & 0xFFFFFFFF)


def write_png(path, rgba, size):
    stride = size * 4
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        raw += rgba[y * stride:(y + 1) * stride]
    png = b"\x89PNG\r\n\x1a\n"
    png += png_chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    png += png_chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += png_chunk(b"IEND", b"")
    open(path, "wb").write(png)
    return png


def write_ico(path, entries):
    out = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    for size, png in entries:
        w = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(png), offset)
        offset += len(png)
    for _, png in entries:
        out += png
    open(path, "wb").write(out)


def main():
    grad, gline, elements = parse_svg(SVG)
    print(f"parsed {len(elements)} shape(s), {len(grad)} gradient stops")
    master, size = render(PNG_SIZE, grad, gline, elements)
    png = write_png(PNG_OUT, master, size)
    print(f"{PNG_OUT}: {size}x{size} RGBA ({len(png)} bytes)")
    entries = []
    for s in ICO_SIZES:
        rgba, _ = render(s, grad, gline, elements)
        entries.append((s, write_png_bytes(rgba, s)))
    write_ico(ICO_OUT, entries)
    print(f"{ICO_OUT}: sizes {ICO_SIZES} ({sum(len(p) for _, p in entries)} bytes)")


def write_png_bytes(rgba, size):
    stride = size * 4
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        raw += rgba[y * stride:(y + 1) * stride]
    png = b"\x89PNG\r\n\x1a\n"
    png += png_chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    png += png_chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += png_chunk(b"IEND", b"")
    return png


if __name__ == "__main__":
    main()
