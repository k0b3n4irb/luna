#!/usr/bin/env python3
"""Generate the binary data blobs for main.asm (Mode 6 OPT test ROM).

Run before assembling:  python3 gen_data.py && bass main.asm
"""
import struct

# Two solid index-1 tiles (char 0 + char 1). A hi-res 16-px tile is
# char N (left 8) + char N+1 (right 8), so both must be solid for a fully
# solid 16-px bar.
solid = bytearray()
for _ in range(8):
    solid += bytes([0xFF, 0x00])  # plane0 = $FF (index 1), plane1 = 0
solid += bytes(16)                # planes 2,3 = 0
open("tiles.bin", "wb").write(solid + solid)

# BG1 tilemap 32x32: entry(C) = char 0, palette group C & 3
# → vertical bars red/green/blue/white repeating every 4 tile columns.
m = bytearray()
for _row in range(32):
    for c in range(32):
        m += struct.pack("<H", (c & 3) << 10)
open("bg1map.bin", "wb").write(m)

# BG3 OPT data 32x32: row 0 entry(T) = enable+offset for screen columns
# >= 16 (entry T drives screen column T+1). 0x2010 = BG1 enable (bit 13)
# + H-offset 16. Other rows zero (no vertical OPT).
g = bytearray()
for row in range(32):
    for t in range(32):
        g += struct.pack("<H", 0x2010 if (row == 0 and t >= 15) else 0x0000)
open("bg3map.bin", "wb").write(g)

# 64-colour palette: backdrop gray + palette-group 0..3 colour 1.
pal = [0] * 64
pal[0] = 0x4210   # gray backdrop
pal[1] = 0x001F   # red   (group 0)
pal[17] = 0x03E0  # green (group 1)
pal[33] = 0x7C00  # blue  (group 2)
pal[49] = 0x7FFF  # white (group 3)
open("pal.bin", "wb").write(b"".join(struct.pack("<H", c) for c in pal))
print("wrote tiles.bin bg1map.bin bg3map.bin pal.bin")
