#!/usr/bin/env python3
"""Generate placeholder battle figurines (pure stdlib, no PIL).

Draws a simple humanoid silhouette per (class, race) into a 256x256 RGBA PNG,
feet at bottom-center, authored facing right. Throwaway dev helper so the sprite
render path can be verified before real art exists.
"""
import struct
import zlib
import os

W = H = 256
FEET_Y = 250

CLASS_COLORS = {
    "warrior": (180, 70, 60),   # red-brown
    "mage":    (80, 90, 200),   # blue
    "ranger":  (70, 150, 80),   # green
}
RACES = ["human"]


def blank():
    return bytearray(W * H * 4)  # all transparent (alpha 0)


def put(buf, x, y, rgba):
    if 0 <= x < W and 0 <= y < H:
        i = (y * W + x) * 4
        buf[i:i + 4] = bytes(rgba)


def fill_circle(buf, cx, cy, r, rgba):
    for y in range(cy - r, cy + r + 1):
        for x in range(cx - r, cx + r + 1):
            if (x - cx) ** 2 + (y - cy) ** 2 <= r * r:
                put(buf, x, y, rgba)


def fill_rect(buf, x0, y0, x1, y1, rgba):
    for y in range(y0, y1):
        for x in range(x0, x1):
            put(buf, x, y, rgba)


def fill_trapezoid(buf, y0, y1, top_half_w, bot_half_w, cx, rgba):
    span = max(1, y1 - y0)
    for y in range(y0, y1):
        t = (y - y0) / span
        hw = int(top_half_w + (bot_half_w - top_half_w) * t)
        for x in range(cx - hw, cx + hw):
            put(buf, x, y, rgba)


def draw_figure(color):
    buf = blank()
    cx = W // 2
    body = color
    dark = tuple(int(c * 0.7) for c in color)
    a = 255

    # legs
    fill_rect(buf, cx - 22, 195, cx - 4, FEET_Y, dark + (a,))
    fill_rect(buf, cx + 4, 195, cx + 22, FEET_Y, dark + (a,))
    # torso (tapered)
    fill_trapezoid(buf, 95, 200, 26, 34, cx, body + (a,))
    # arms
    fill_rect(buf, cx - 40, 100, cx - 26, 175, dark + (a,))
    fill_rect(buf, cx + 26, 100, cx + 40, 175, dark + (a,))
    # head
    fill_circle(buf, cx, 65, 30, body + (a,))
    # facing-right marker: small nose bump on the right of the head
    fill_circle(buf, cx + 28, 65, 8, body + (a,))
    return buf


def write_png(path, buf):
    raw = bytearray()
    stride = W * 4
    for y in range(H):
        raw.append(0)  # filter type 0
        raw.extend(buf[y * stride:(y + 1) * stride])

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0)
    idat = zlib.compress(bytes(raw), 9)
    with open(path, "wb") as f:
        f.write(sig)
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", idat))
        f.write(chunk(b"IEND", b""))


def main():
    out = os.path.join(os.path.dirname(__file__), "..", "assets", "images", "units")
    out = os.path.abspath(out)
    os.makedirs(out, exist_ok=True)
    for cls, color in CLASS_COLORS.items():
        fig = draw_figure(color)
        for race in RACES:
            p = os.path.join(out, f"{cls}_{race}.png")
            write_png(p, fig)
            print("wrote", p)


if __name__ == "__main__":
    main()
