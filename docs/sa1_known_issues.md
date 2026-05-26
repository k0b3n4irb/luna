# SA-1 known issues

Status snapshot, 2026-05-26. luna boots SA-1 ROMs (sa1_hello reaches
"BOOTED OK!"; SMRPG advances past its sky-and-clouds intro start) but
something downstream of the chip's main loop / shared-IRAM / DMA path
mangles the data the main CPU eventually sees.

## Reproducer

`opensnes` ships an SA-1 starfield demo at
`examples/memory/sa1_starfield/sa1_starfield.sfc` that the SA-1
computes 128 dot positions per frame for. luna renders ~10 dots
clustered tightly; the same ROM in Mesen2 shows a clean murmuration.

```bash
./target/release/luna state -n 100000000 \
    --screenshot /tmp/sa1.png \
    /home/kobenairb/workspace/opensnes/examples/memory/sa1_starfield/sa1_starfield.sfc
```

## What was traced (session 2026-05-26)

Probed via a one-off integration test that drove the ROM, captured
both the SA-1's IRAM buffer and the PPU's final OAM, then logged the
DMA + OAM-port traffic in between:

1. **SA-1 IRAM is correct.** `peek_memory(0x00, 0x3010, 64)` reads
   back 128 distinct (X, Y) tuples — the SA-1's sine-table loop runs
   end-to-end every frame.
2. **Main-CPU bus read of IRAM is correct.** `lda.l $0000, X` with
   `X = 0x3010 + i*4` returns the right byte for every `i ∈ [0, 127]`
   (256 distinct X values, 256 distinct byte returns). 65C816 absolute-
   long-indexed-X is not the bug.
3. **WRAM shadow OAM is correct.** The C library's per-bird store
   into `oamMemory[i*4]` at `$7E:0300+i*4` deposits 126 distinct (X, Y)
   pairs in WRAM. C inner loop and stack-relative addressing are fine.
4. **DMA-channel-0 setup is correct.** Logging in
   `DmaChannel::run` shows `mode=OneByteOneReg, bbad=0x04 ($2104),
   a=7E:0300, das=0x0220 (= 544 bytes)`. `das` is loaded in full
   from `$4305/$4306`; the byte-count register is not truncated.
5. **OAM port traffic is correct.** Logging in `Oam::write_gated`
   shows all 544 writes per DMA happen, with `allow_data = true` and
   addresses incrementing 0, 1, 2, …, 543 (then wrapping). The exact
   byte values from the WRAM shadow flow through unchanged: e.g., the
   last DMA into byte 0x20 wrote `0xCD` (= bird 8's X, matching WRAM
   at `$7E:0320`).
6. **And yet the final PPU OAM is wrong.** Sampling `oam_full[0..512]`
   immediately after the run shows 8 distinct (X, Y) pairs repeating
   16× across the 128 sprites. Specifically `data[k] == data[k mod 32]`
   for all `k ∈ [0, 511]` — a clean 32-byte wrap somewhere in the
   path between `write_gated` completing and the renderer reading
   `oam.peek`.

`Oam::data` is `[u8; 0x220]` (544 entries), `peek` is `data[addr & 0x21F]`
— neither path has an obvious 32-byte mask. `reload_address_from_latch`
fires once per frame as expected (at `vcounter == vdisp`); not the
source of the wrap. `wram_offset` has no 32-byte aliasing either.

The bug is somewhere the writes-to-data and peek-from-data disagree.
A possible suspect not yet investigated: the **interaction with the
SA-1 `Sa1Chip` running the SA-1 catch-up scheduler while the main
CPU's DMA is mid-transfer**. Both the SA-1 and main CPU drive memory
through their respective `step()` cycles; if one of them is
mid-snapshot when DMA reads or writes happen, a `data[]` update can
be observed pre-write by the other.

## Next steps when picking this back up

1. Add a `peek_oam_data` API that bypasses `peek`/`write_gated` and
   directly returns the raw 544-byte array. Confirm `data[]` itself
   has the 32-byte repeat — not just `peek`'s view of it.
2. Run the smoke test against a non-SA-1 ROM that hits the same
   OAM-DMA pattern (DKC and Bomberman trigger it on their title
   screens). If those are *also* wrong but visually OK because their
   sprite layout happens to be tolerant, the bug is generic OAM/DMA
   and not SA-1-specific.
3. If the smoke test confirms #2, instrument the OAM `data` array
   itself (lock-write tracing) to find which path corrupts bytes
   `[32..511]`.
4. If non-SA-1 ROMs work fine, the bug *is* SA-1-specific — likely
   a mid-DMA `Sa1Chip` step that interleaves writes/reads into shared
   resources in a way that violates the DMA's read-then-write
   atomicity.
