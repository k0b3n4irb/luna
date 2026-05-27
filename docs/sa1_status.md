# SA-1 status

Snapshot: 2026-05-27. SA-1 ROMs now run end-to-end in luna — the
chip-side 65C816 instance, shared IRAM, B-bus mapping, DMA catch-up
during main-CPU bursts, and the PPU read path all agree with
ares + Mesen2.

## Reference reproducer

opensnes ships an SA-1 starfield demo that the chip computes 128 dot
positions per frame for via a sine-table loop. luna now renders the
full murmuration (~128 white dots on a deep-blue field), matching
the reference output from Mesen2:

```bash
./target/release/luna state -n 100000000 \
    --screenshot /tmp/sa1_starfield.png \
    /home/kobenairb/workspace/opensnes/examples/memory/sa1_starfield/sa1_starfield.sfc
```

## What was fixed

Two independent bugs were uncovered by the starfield reproducer:

1. **OAM peek mask was bit-AND not modulo** (commit `0ae7249`).
   `Oam::peek/poke/read` used `data[addr & 0x21F]` where `0x21F` is
   the ring SIZE (544 = 0x220 in exclusive, 0x21F inclusive), not a
   wrap mask. Since 0x21F bit-patterns to `bits 0..4 + bit 9`, any
   `addr` in `32..0x1FF` lost its bit-5+ entropy and aliased back
   into the first 32 bytes. Visible effect: sprites 8..15 rendered
   with sprite 0..7's bytes, sprites 16..23 with sprite 0..7's, etc.
   — a clean 32-byte sprite wrap.
2. **DMA bursts didn't catch the coprocessor up between bytes**
   (commit `2ee0cbe`). `DmaChannel::run` held the bus for the full
   transfer (up to ~262k mclks) without stepping the SA-1. Now
   `DmaBus::tick(8)` runs per byte transferred, matching ares
   (`Thread::step(2) + synchronize`) and Mesen2 (`IncMasterClock4
   → SyncCoprocessors → Sa1::Run`).

Bug #1 was the visible cause of the starfield collapse. Bug #2 is
architecturally correct independent of #1 — without it the SA-1
sees a frozen timeline during long DMAs.

## Known remaining audit gaps

- **CIWP/SIWP defaults** (`luna-bus/src/sa1.rs:355-363`) initialise to
  `0xFF` (all-allow); ares + Mesen2 reset both to `0x00` (all-block).
  No observable regression in current test ROMs, but this is a
  semantic deviation worth fixing for any future code that probes
  the reset state.

## Related references

- ares: `ares/sfc/coprocessor/sa1/sa1.cpp:63-94` (cooperative
  scheduler), `ares/sfc/coprocessor/sa1/io.cpp` (CIWP/SIWP reset).
- Mesen2: `Core/SNES/Coprocessors/SA1/Sa1.cpp` (`Run()` cadence),
  `Core/SNES/SnesDmaController.cpp` (`CopyDmaByte` interleave).
- The `.claude/rules/reference-first.md` rule lists the running tally
  of bit-layout / mask bugs that secondary docs missed but the canonical
  sources caught.
