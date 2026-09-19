#!/usr/bin/env python3
"""Generate 16x16 1-bit dot matrix font data for dazitui."""

import os
import string
import struct
from PIL import Image, ImageDraw, ImageFont


def main():
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    data_dir = os.path.join(repo_root, "dazitui-core", "data")
    chars = set()

    for fname in os.listdir(data_dir):
        if fname.endswith(".txt"):
            with open(os.path.join(data_dir, fname), "r", encoding="utf-8") as f:
                for line in f:
                    for c in line.strip():
                        chars.add(c)

    # Include ASCII printable
    for c in string.printable:
        if 32 <= ord(c) < 127:
            chars.add(c)

    sorted_chars = sorted(list(chars), key=lambda c: ord(c))
    print(f"Generating glyphs for {len(sorted_chars)} characters...")

    font_path = "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"
    font = ImageFont.truetype(font_path, 15)

    records = []
    for c in sorted_chars:
        codepoint = ord(c)
        im = Image.new("L", (16, 16), 0)
        draw = ImageDraw.Draw(im)
        bbox = font.getbbox(c)
        if bbox:
            w = bbox[2] - bbox[0]
            h = bbox[3] - bbox[1]
            x = (16 - w) // 2 - bbox[0]
            y = (16 - h) // 2 - bbox[1]
            draw.text((x, y), c, fill=255, font=font)

        # Pack 16x16 into 32 bytes (16 rows, 2 bytes per row, MSB first)
        glyph_bytes = bytearray(32)
        for r in range(16):
            row_bits = 0
            for col in range(16):
                if im.getpixel((col, r)) > 100:
                    row_bits |= 1 << (15 - col)
            glyph_bytes[r * 2] = (row_bits >> 8) & 0xFF
            glyph_bytes[r * 2 + 1] = row_bits & 0xFF
        records.append((codepoint, bytes(glyph_bytes)))

    out_path = os.path.join(data_dir, "font16.bin")
    with open(out_path, "wb") as f:
        f.write(b"DT16")
        f.write(struct.pack("<I", len(records)))
        for cp, b in records:
            f.write(struct.pack("<I", cp))
            f.write(b)

    file_size = os.path.getsize(out_path)
    print(f"Wrote {out_path}, size: {file_size} bytes ({file_size / 1024:.1f} KB)")


if __name__ == "__main__":
    main()
