#!/usr/bin/env python3
"""Generate placeholder screenshots for the Rift site and README.

Stdlib only (zlib + struct). Each image is a dark rift-neon card with the
feature name in a blocky 5x7 font. Real screenshots overwrite these files
in place - see README.md in this directory for the exact filenames.

    python3 docs/screenshots/gen_placeholders.py
"""
import os
import struct
import zlib

W, H = 1600, 1000
BG = (8, 10, 24)
GRID = (16, 20, 44)
MAGENTA = (255, 46, 190)
CYAN = (0, 229, 255)
FG = (214, 224, 255)
DIM = (110, 120, 170)

SHOTS = {
    "hero": "RIFT",
    "blocks": "COMMAND BLOCKS",
    "ai-chat": "AI CHAT SIDEBAR",
    "cmdk": "CMD+K ASK ABOUT THIS",
    "fix-suggestion": "INLINE FIX SUGGESTION",
    "nl-command": "# NATURAL LANGUAGE",
    "splits": "SPLITS + TABS",
    "palette": "COMMAND PALETTE",
    "browser": "BUILT-IN BROWSER",
    "preview-accept": "PREVIEW THEN ACCEPT",
    "effects-crt": "CRT EFFECT",
    "hud": "HUD",
}

# 5x7 glyphs, rows top->bottom, '#' = on.
FONT = {
    "A": [".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
    "B": ["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."],
    "C": [".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."],
    "D": ["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."],
    "E": ["#####", "#....", "#....", "####.", "#....", "#....", "#####"],
    "F": ["#####", "#....", "#....", "####.", "#....", "#....", "#...."],
    "G": [".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".####"],
    "H": ["#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
    "I": [".###.", "..#..", "..#..", "..#..", "..#..", "..#..", ".###."],
    "J": ["..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##.."],
    "K": ["#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"],
    "L": ["#....", "#....", "#....", "#....", "#....", "#....", "#####"],
    "M": ["#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#"],
    "N": ["#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#"],
    "O": [".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
    "P": ["####.", "#...#", "#...#", "####.", "#....", "#....", "#...."],
    "Q": [".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#"],
    "R": ["####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"],
    "S": [".####", "#....", "#....", ".###.", "....#", "....#", "####."],
    "T": ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."],
    "U": ["#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
    "V": ["#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.."],
    "W": ["#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#"],
    "X": ["#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"],
    "Y": ["#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.."],
    "Z": ["#####", "....#", "...#.", "..#..", ".#...", "#....", "#####"],
    "0": [".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."],
    "1": ["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."],
    "2": [".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"],
    "3": ["####.", "....#", "....#", ".###.", "....#", "....#", "####."],
    "4": ["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."],
    "5": ["#####", "#....", "####.", "....#", "....#", "#...#", ".###."],
    "6": [".###.", "#....", "#....", "####.", "#...#", "#...#", ".###."],
    "7": ["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."],
    "8": [".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."],
    "9": [".###.", "#...#", "#...#", ".####", "....#", "....#", ".###."],
    "+": [".....", "..#..", "..#..", "#####", "..#..", "..#..", "....."],
    "-": [".....", ".....", ".....", "#####", ".....", ".....", "....."],
    "#": [".#.#.", ".#.#.", "#####", ".#.#.", "#####", ".#.#.", ".#.#."],
    ".": [".....", ".....", ".....", ".....", ".....", ".##..", ".##.."],
    "/": ["....#", "....#", "...#.", "..#..", ".#...", "#....", "#...."],
    ">": [".#...", "..#..", "...#.", "....#", "...#.", "..#..", ".#..."],
    "_": [".....", ".....", ".....", ".....", ".....", ".....", "#####"],
    " ": ["....."] * 7,
}


def text_width(s, scale):
    return len(s) * 6 * scale - scale


def draw_text(px, s, x0, y0, scale, color):
    for i, ch in enumerate(s):
        glyph = FONT.get(ch, FONT[" "])
        gx = x0 + i * 6 * scale
        for r, row in enumerate(glyph):
            for c, bit in enumerate(row):
                if bit != "#":
                    continue
                for dy in range(scale):
                    y = y0 + r * scale + dy
                    if not 0 <= y < H:
                        continue
                    base = y * W
                    for dx in range(scale):
                        x = gx + c * scale + dx
                        if 0 <= x < W:
                            px[base + x] = color


def rect(px, x0, y0, x1, y1, color):
    for y in range(max(0, y0), min(H, y1)):
        base = y * W
        for x in range(max(0, x0), min(W, x1)):
            px[base + x] = color


def render(label):
    px = [BG] * (W * H)
    for y in range(0, H, 40):
        rect(px, 0, y, W, y + 1, GRID)
    for x in range(0, W, 40):
        rect(px, x, 0, x + 1, H, GRID)
    # window frame + title bar
    m = 60
    rect(px, m, m, W - m, m + 3, MAGENTA)
    rect(px, m, H - m - 3, W - m, H - m, CYAN)
    rect(px, m, m, m + 3, H - m, MAGENTA)
    rect(px, W - m - 3, m, W - m, H - m, CYAN)
    rect(px, m + 3, m + 3, W - m - 3, m + 56, (14, 17, 38))
    for i, col in enumerate(((255, 64, 112), (255, 214, 64), (0, 240, 160))):
        cx = m + 34 + i * 30
        rect(px, cx - 7, m + 22, cx + 7, m + 36, col)
    # label
    scale = 12
    while text_width(label, scale) > W - 2 * m - 120 and scale > 4:
        scale -= 1
    tw = text_width(label, scale)
    ty = H // 2 - 7 * scale // 2 - 20
    draw_text(px, label, (W - tw) // 2, ty, scale, FG)
    # accent underline
    ul = min(tw, 360)
    rect(px, (W - ul) // 2, ty + 7 * scale + 30, (W + ul) // 2, ty + 7 * scale + 36, MAGENTA)
    sub = "SCREENSHOT PLACEHOLDER"
    draw_text(px, sub, (W - text_width(sub, 4)) // 2, ty + 7 * scale + 70, 4, DIM)
    return px


def write_png(path, px):
    raw = bytearray()
    for y in range(H):
        raw.append(0)
        for (r, g, b) in px[y * W:(y + 1) * W]:
            raw += bytes((r, g, b))

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    for name, label in SHOTS.items():
        path = os.path.join(here, name + ".png")
        write_png(path, render(label))
        print("wrote", os.path.relpath(path))


if __name__ == "__main__":
    main()
