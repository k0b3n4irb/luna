#!/usr/bin/env python3
"""console-costs.py — what the console charges, measured on luna.

    tools/console-costs.py [--luna target/release/luna] [--json]

Builds tiny LoROM probes in memory (no ROM, no file of anyone else's), runs
each twice with `luna state --until-pc` (once at the start of the measured
code, once at its end) and subtracts `stats.mclk.cpu_active` / `.dma`
(master clocks, 21.477 MHz NTSC: one master clock = 1/21 477 272 s). Nothing
here is a cycle count read from a table: every figure is a difference of two
snapshots of the emulator's own clock, so a change in luna moves it.

Probes: opcode fetch from SlowROM / FastROM, data reads at each bus speed,
MVN per byte, general DMA (setup + per byte, two sizes), the hardware
multiplier and divider (how many CPU cycles after the write the result is
right), and the DRAM refresh plus the frame length.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile

LUNA = "target/release/luna"
BASE = 0x8000  # LoROM bank 0, first byte of the image


class Asm:
    """Bytes at $00:8000 with labels; `jml`-style fixups are patched at the end."""

    def __init__(self):
        self.b = bytearray()
        self.labels = {}
        self.fix = []

    def here(self):
        return BASE + len(self.b)

    def label(self, name):
        self.labels[name] = self.here()

    def emit(self, *bs):
        self.b.extend(bs)

    def jml(self, bank, name):
        self.emit(0x5C)
        self.fix.append((len(self.b), name, bank))
        self.emit(0, 0, bank)

    def rom(self):
        for pos, name, bank in self.fix:
            a = self.labels[name]
            self.b[pos] = a & 0xFF
            self.b[pos + 1] = a >> 8
        img = bytearray(0x10000)
        img[: len(self.b)] = self.b
        img[0x7FC0:0x7FD5] = b"CONSOLE COST PROBE  "
        img[0x7FD5] = 0x20
        img[0x7FD7] = 0x08
        img[0x7FFC], img[0x7FFD] = 0x00, 0x80
        return bytes(img)


def write_rom(path, rom):
    with open(path, "wb") as f:
        f.write(rom)


def snapshot(rom, pc, tmp, luna):
    path = os.path.join(tmp, "probe.sfc")
    write_rom(path, rom)
    out = os.path.join(tmp, "o.json")
    r = subprocess.run(
        [luna, "state", path, "--force-mapper", "lorom", "--until-pc", pc, "--until-frame", "30",
         "--out", out],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        sys.exit("luna failed at %s: %s" % (pc, r.stderr[-300:]))
    d = json.load(open(out))
    return d


def window(asm, a, b, tmp, luna, peek=None):
    """(cpu_active, dma, peeked bytes) between label a and label b."""
    rom = asm.rom()
    sa = snapshot(rom, "%02X:%04X" % (asm.pcbank.get(a, 0), asm.labels[a]), tmp, luna)
    sb = snapshot(rom, "%02X:%04X" % (asm.pcbank.get(b, 0), asm.labels[b]), tmp, luna)
    ma, mb = sa["stats"]["mclk"], sb["stats"]["mclk"]
    return mb["cpu_active"] - ma["cpu_active"], mb["dma"] - ma["dma"]


def prog(body_slow=None):
    a = Asm()
    a.pcbank = {}
    a.emit(0x78, 0x18, 0xFB)  # sei, clc, xce (native, 8-bit A/X/Y)
    return a


def to_fast(a):
    """lda #1 ; sta $420D ; jml bank $80 (FastROM); the code after runs at 6 mclk fetches."""
    a.emit(0xA9, 0x01, 0x8D, 0x0D, 0x42)
    a.label("fast")
    a.jml(0x80, "fast_here")
    a.label("fast_here")
    return 0x80


def repeat(op, n):
    return bytes(op) * n


def probe_ops(luna, tmp):
    res = {}
    N = 64
    for speed in ("slow", "fast"):
        for name, op, expect in (
            ("NOP", [0xEA], None),
            ("LDA dp (WRAM mirror)", [0xA5, 0x10], None),
            ("LDA abs $4210 (CPU reg, FAST)", [0xAD, 0x10, 0x42], None),
            ("LDA abs $4016 (joypad, XSLOW)", [0xAD, 0x16, 0x40], None),
            ("LDA abs $0010 (WRAM mirror)", [0xAD, 0x10, 0x00], None),
            ("LDA long $7E0010 (WRAM)", [0xAF, 0x10, 0x00, 0x7E], None),
            ("LDA abs $2137 (PPU, FAST)", [0xAD, 0x37, 0x21], None),
            ("PHA", [0x48], None),
            ("PLA", [0x68], None),
        ):
            a = prog()
            bank = 0
            if speed == "fast":
                bank = to_fast(a)
            a.pcbank = {"s": bank, "e": bank}
            a.label("s")
            a.emit(*repeat(op, N))
            a.label("e")
            a.emit(0xEA)
            cpu, _ = window(a, "s", "e", tmp, luna)
            res[(name, speed)] = cpu / N
    return res


def probe_mvn(luna, tmp):
    out = {}
    for speed in ("slow", "fast"):
        a = prog()
        bank = to_fast(a) if speed == "fast" else 0
        a.pcbank = {"s": bank, "e": bank}
        a.emit(0xC2, 0x30)  # rep #$30
        a.emit(0xA2, 0x00, 0x00, 0xA0, 0x00, 0x20, 0xA9, 0xFF, 0x00)  # X=0 Y=$2000 A=$00FF (256 bytes)
        a.label("s")
        a.emit(0x54, 0x7E, 0x7E)  # mvn $7E,$7E
        a.label("e")
        a.emit(0xEA)
        cpu, _ = window(a, "s", "e", tmp, luna)
        out[speed] = cpu / 256
    return out


def dma_prog(size):
    a = prog()
    a.pcbank = {}
    a.emit(0xA9, 0x8F, 0x8D, 0x00, 0x21)  # INIDISP forced blank
    a.emit(0xA9, 0x80, 0x8D, 0x15, 0x21)  # VMAIN word increment after $2119
    a.emit(0xA9, 0x01, 0x8D, 0x00, 0x43)  # mode 1 (two registers)
    a.emit(0xA9, 0x18, 0x8D, 0x01, 0x43)  # B-bus $2118
    a.emit(0xA9, 0x00, 0x8D, 0x02, 0x43, 0x8D, 0x03, 0x43)  # A-bus $7E:0000
    a.emit(0xA9, 0x7E, 0x8D, 0x04, 0x43)
    a.emit(0xA9, size & 0xFF, 0x8D, 0x05, 0x43, 0xA9, size >> 8, 0x8D, 0x06, 0x43)
    a.emit(0xA9, 0x01)
    a.label("s")
    a.emit(0x8D, 0x0B, 0x42)  # sta $420B: DMA runs, CPU halted
    a.emit(0xEA)  # the DMA starts at the next opcode fetch: the window ends after this NOP
    a.label("e")
    a.emit(0xEA)
    return a


def probe_dma(luna, tmp):
    r = {}
    for size in (256, 512, 1024):
        cpu, dma = window(dma_prog(size), "s", "e", tmp, luna)
        # minus the STA abs to a FAST register (30) and the NOP (14) in SlowROM
        r[size] = (cpu + dma - 30 - 14, dma)
    return r


def alu_prog(kind, k):
    """Write the operands, wait k NOPs, read the result into dp $10.. ; stop at label e."""
    a = prog()
    a.pcbank = {}
    if kind == "mul":
        a.emit(0xA9, 0xFF, 0x8D, 0x02, 0x42)  # WRMPYA = $FF
        a.emit(0x8D, 0x03, 0x42)  # WRMPYB = $FF  -> $FE01 after 8 cycles
        a.emit(*repeat([0xEA], k))
        a.emit(0xAD, 0x16, 0x42, 0x85, 0x10, 0xAD, 0x17, 0x42, 0x85, 0x11)
    else:
        a.emit(0xA9, 0xFE, 0x8D, 0x04, 0x42, 0xA9, 0xFF, 0x8D, 0x05, 0x42)  # dividend $FFFE
        a.emit(0xA9, 0x0B, 0x8D, 0x06, 0x42)  # divisor 11 -> $1745 r7 after 16 cycles
        a.emit(*repeat([0xEA], k))
        a.emit(0xAD, 0x14, 0x42, 0x85, 0x10, 0xAD, 0x15, 0x42, 0x85, 0x11)
        a.emit(0xAD, 0x16, 0x42, 0x85, 0x12)
    a.label("e")
    a.emit(0x80, 0xFE)
    return a


def alu_ok(kind, k, tmp, luna):
    a = alu_prog(kind, k)
    rom = a.rom()
    path = os.path.join(tmp, "p.sfc")
    write_rom(path, rom)
    out = os.path.join(tmp, "o.json")
    r = subprocess.run(
        [luna, "state", path, "--force-mapper", "lorom", "--until-pc", "00:%04X" % a.labels["e"],
         "--until-frame", "30", "--peek", "7E:0010:3", "--out", out],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        sys.exit("luna failed: " + r.stderr[-300:])
    d = json.load(open(out))
    return bytes.fromhex(d["peeks"][0]["bytes_hex"])


def probe_alu(luna, tmp):
    out = {}
    for kind, want in (("mul", bytes([0x01, 0xFE])), ("div", bytes([0x45, 0x17, 0x07]))):
        first = None
        for k in range(0, 12):
            got = alu_ok(kind, k, tmp, luna)[: len(want)]
            if got == want:
                first = k
                break
        out[kind] = first
    return out


def probe_frame(luna, tmp):
    a = prog()
    a.pcbank = {}
    a.label("l")
    a.emit(0x80, 0xFE)
    path = os.path.join(tmp, "f.sfc")
    write_rom(path, a.rom())
    out = os.path.join(tmp, "o.json")
    subprocess.run(
        [luna, "state", path, "--force-mapper", "lorom", "--until-frame", "10", "--out", out],
        capture_output=True, text=True,
    )
    d = json.load(open(out))
    return d["stats"]["mclk"], d["stats"]["last_frame"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--luna", default=LUNA)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    with tempfile.TemporaryDirectory(prefix="luna-costs-") as tmp:
        ops = probe_ops(args.luna, tmp)
        mvn = probe_mvn(args.luna, tmp)
        dma = probe_dma(args.luna, tmp)
        alu = probe_alu(args.luna, tmp)
        _, frame = probe_frame(args.luna, tmp)
    result = {
        "ops_mclk_each": {"%s [%sROM]" % k: v for k, v in ops.items()},
        "mvn_mclk_per_byte": mvn,
        "dma": {str(s): {"cpu_mclk": c, "dma_mclk": d} for s, (c, d) in dma.items()},
        "alu_cycles_to_valid": alu,
        "frame": frame,
    }
    if args.json:
        print(json.dumps(result, indent=2))
        return
    print("Per instruction, master clocks (21.477 MHz); the NOP row is opcode fetch + 1 internal cycle:")
    for k, v in ops.items():
        print("  %-34s %-5s %6.1f" % (k[0], k[1], v))
    print("MVN per byte: SlowROM %.1f, FastROM %.1f master clocks" % (mvn["slow"], mvn["fast"]))
    s = sorted(dma)
    d1, d2 = dma[s[0]][0], dma[s[1]][0]
    per = (d2 - d1) / (s[1] - s[0])
    print("DMA (mode 1, WRAM -> VRAM): %s bytes = %d, %s = %d, %s = %d master clocks the CPU is halted;"
          % (s[0], dma[s[0]][0], s[1], dma[s[1]][0], s[2], dma[s[2]][0]))
    print("   per byte %.2f, fixed part %.0f" % (per, d1 - per * s[0]))
    print("Multiplier: the product is right when the read comes 8 CPU cycles after the write to $4203 "
          "(first NOP count that reads it right: %s); divider: 16 cycles after $4206 (%s). "
          "Earlier reads see the partial value." % (alu["mul"], alu["div"]))
    print("Frame: %s master clocks, of which refresh %s, CPU %s" %
          (frame["total"], frame["refresh"], frame["cpu_active"]))


if __name__ == "__main__":
    main()
