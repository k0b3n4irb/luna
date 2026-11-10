#!/usr/bin/env python3
"""Write a 32 KiB LoROM whose picture says on which scanline its NMI fired.

The ROM waits for the NMI, then latches the vertical counter ($2137, $213D)
and writes its low byte to CGRAM colour 0, with the backdrop the only layer
shown: the whole picture is one flat colour that is a function of the NMI line.
A second opinion (Mesen2 through a pixel-for-pixel screen comparison, or
any other emulator) reads the same colour only if both start the NMI on the
same line. It is the probe that makes a one-line shift of VBlank visible,
which the settled screens of ordinary ROMs do not (25 of 25 identical with
VBlank moved from line 225 to 226).

usage: tools/make-nmi-line-probe.py OUT.sfc   (a .sym with `main` is written beside it)
Synthetic: no game, no SDK code.
"""
import sys


def build() -> bytearray:
    rom = bytearray(b"\xFF" * 0x8000)

    def put(a, b):
        rom[a - 0x8000:a - 0x8000 + len(b)] = bytes(b)

    code = [
        0x78, 0x18, 0xFB,                      # SEI ; CLC ; XCE
        0xA9, 0x80, 0x8D, 0x00, 0x21,          # force blank
        0xA9, 0x00, 0x8D, 0x00, 0x42,          # NMI off
        0xA9, 0x00, 0x8D, 0x2C, 0x21,          # TM = 0: only the backdrop shows
        0xA9, 0x0F, 0x8D, 0x00, 0x21,          # display on, full brightness
        0xA9, 0x80, 0x8D, 0x00, 0x42,          # NMI on
        0xCB, 0x4C, 0x00, 0x00,                # loop: WAI ; JMP loop
    ]
    loop = 0x8000 + len(code) - 4
    code[-2], code[-1] = loop & 0xFF, loop >> 8
    put(0x8000, code)
    nmi = [
        0xAD, 0x10, 0x42,                      # acknowledge: LDA $4210
        0xAD, 0x3F, 0x21,                      # LDA $213F: reset the counter flip-flop
        0xAD, 0x37, 0x21,                      # LDA $2137: latch the counters
        0xAD, 0x3D, 0x21,                      # LDA $213D: vertical counter, low byte
        0x9C, 0x21, 0x21,                      # STZ $2121: CGRAM address 0
        0x8D, 0x22, 0x21,                      # STA $2122: colour low byte = the line
        0x9C, 0x22, 0x21,                      # STZ $2122: colour high byte
        0x40,                                  # RTI
    ]
    put(0x8040, nmi)
    put(0xFFC0, list(b"NMI LINE PROBE        "[:21]))
    put(0xFFD5, [0x20, 0x00, 0x08, 0x00, 0x01, 0x33, 0x00])  # LoROM, 256 Kbit, NTSC
    for v in (0xFFE4, 0xFFE6, 0xFFE8, 0xFFEA, 0xFFEC, 0xFFEE, 0xFFF4, 0xFFF6, 0xFFF8, 0xFFFA, 0xFFFE):
        put(v, [0x40, 0x80])
    put(0xFFFC, [0x00, 0x80])
    # The checksum covers the whole image with its own four bytes in place:
    # the complement and the sum written first, then the sum taken again.
    total = sum(rom) & 0xFFFF
    put(0xFFDC, [(total ^ 0xFFFF) & 0xFF, (total ^ 0xFFFF) >> 8, total & 0xFF, total >> 8])
    total = sum(rom) & 0xFFFF
    put(0xFFDE, [total & 0xFF, total >> 8])
    put(0xFFDC, [(total ^ 0xFFFF) & 0xFF, (total ^ 0xFFFF) >> 8])
    return rom


def main(argv):
    if len(argv) != 2:
        print(__doc__)
        return 2
    out = argv[1]
    with open(out, "wb") as f:
        f.write(build())
    with open(out.rsplit(".", 1)[0] + ".sym", "w") as f:
        f.write("[labels]\n00:8000 main\n00:8040 NmiEntry\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
