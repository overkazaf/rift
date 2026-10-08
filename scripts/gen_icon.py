#!/usr/bin/env python3
"""Generate a simple cyberpunk-style icon for Rift terminal."""
import struct, zlib, math

SIZE = 256
BG = (18, 18, 32)       # dark blue-purple
ACCENT = (180, 130, 255) # neon purple
GLOW = (100, 60, 200)    # dim glow

def px(r, g, b, a=255):
    return bytes([r, g, b, a])

def dist(x1, y1, x2, y2):
    return math.sqrt((x1-x2)**2 + (y1-y2)**2)

img = bytearray()
for y in range(SIZE):
    img.append(0)  # PNG filter: None
    for x in range(SIZE):
        # Background with subtle radial gradient
        d = dist(x, y, SIZE//2, SIZE//2) / (SIZE//2)
        bg_factor = max(0, 1.0 - d * 0.3)
        br = int(BG[0] * bg_factor)
        bg = int(BG[1] * bg_factor)
        bb = int(BG[2] * bg_factor)

        # Rounded rectangle border
        margin = 20
        radius = 40
        in_rect = True
        if x < margin or x >= SIZE - margin or y < margin or y >= SIZE - margin:
            in_rect = False
        # Round corners
        corners = [
            (margin + radius, margin + radius),
            (SIZE - margin - radius, margin + radius),
            (margin + radius, SIZE - margin - radius),
            (SIZE - margin - radius, SIZE - margin - radius),
        ]
        for cx, cy in corners:
            if (x < margin + radius or x >= SIZE - margin - radius) and \
               (y < margin + radius or y >= SIZE - margin - radius):
                if dist(x, y, cx, cy) > radius:
                    in_rect = False

        if not in_rect:
            img.extend(px(0, 0, 0, 0))  # transparent outside
            continue

        # Draw "R" letter (blocky pixel art style)
        cx, cy = SIZE // 2, SIZE // 2
        # Normalized coords in the letter space
        lx = (x - margin - 50) / (SIZE - 2*margin - 100)
        ly = (y - margin - 40) / (SIZE - 2*margin - 80)

        is_letter = False
        # Vertical bar (left side of R)
        if 0.05 <= lx <= 0.25 and 0.0 <= ly <= 1.0:
            is_letter = True
        # Top horizontal bar
        if 0.05 <= lx <= 0.75 and 0.0 <= ly <= 0.15:
            is_letter = True
        # Middle horizontal bar
        if 0.05 <= lx <= 0.75 and 0.42 <= ly <= 0.55:
            is_letter = True
        # Right vertical (top half - bump of R)
        if 0.6 <= lx <= 0.8 and 0.0 <= ly <= 0.55:
            is_letter = True
        # Diagonal leg of R
        if 0.42 <= ly <= 1.0:
            leg_x = 0.25 + (ly - 0.42) * 0.9
            if leg_x - 0.1 <= lx <= leg_x + 0.1:
                is_letter = True

        if is_letter and 0.0 <= lx <= 1.0 and 0.0 <= ly <= 1.0:
            # Neon glow effect
            glow_strength = 0.8 + 0.2 * math.sin(y * 0.05)
            r = int(ACCENT[0] * glow_strength)
            g = int(ACCENT[1] * glow_strength)
            b = int(ACCENT[2] * glow_strength)
            img.extend(px(min(r,255), min(g,255), min(b,255)))
        else:
            img.extend(px(br, bg, bb))

# Encode as PNG
def make_png(width, height, raw_rgba):
    def chunk(ctype, data):
        c = ctype + data
        crc = struct.pack('>I', zlib.crc32(c) & 0xffffffff)
        return struct.pack('>I', len(data)) + c + crc

    sig = b'\x89PNG\r\n\x1a\n'
    ihdr = struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0)  # 8bit RGBA
    idat = zlib.compress(bytes(raw_rgba), 9)

    return sig + chunk(b'IHDR', ihdr) + chunk(b'IDAT', idat) + chunk(b'IEND', b'')

png_data = make_png(SIZE, SIZE, img)
with open('assets/icon.png', 'wb') as f:
    f.write(png_data)
print(f"Icon generated: assets/icon.png ({len(png_data)} bytes)")
