# Memory, DMA & the bus

## The bus & mappers

| | |
|---|---|
| **On the console** | one 24-bit address bus; the cartridge board decides what answers where |
| **In luna** | `crates/luna-bus/src/` (`bus.rs`, `mapper.rs`, one file per board, `speed.rs`), `crates/luna-cartridge/src/lib.rs` (the header and the detection) |
| **Proven by** | unit tests of the mirroring, the open bus and the speed table; the golden suite boots through it |
| **Grade** | **B+** (scorecard: *Bus / mappers*) |
| **Open gaps** | LoROM save RAM mapping up to 2 MB, the checksum hard-reject, boards outside the supported set ([the scorecard row](https://github.com/k0b3n4irb/luna/blob/main/docs/accuracy_scorecard.md)) |

> The table above opens every subsystem page: what the hardware is, where
> luna's port lives, what proves it, its grade in the
> [accuracy scorecard](../method/accuracy.md#where-luna-stands), and what is
> still open.

`luna-bus` is the foundation every CPU and the system glue build on. It defines
the `Bus` trait, the 24-bit `Addr24` address type, the `MapperKind` enum, and
the per-mapper shims that translate a SNES address into a physical location:

| Mapper | Used by |
|---|---|
| **LoROM** | the majority of the library |
| **HiROM** | larger / later titles |
| **ExHiROM** | a few oversized carts |
| **SA-1** | the SA-1 coprocessor board |
| **Super FX** | the GSU boards |
| **DSP-1** | the uPD7725 boards |
| **S-DD1** | the decompressor board |

Mapper detection scores the ROM header (reset-vector validity, opcode
plausibility, checksum, map-mode/offset agreement) the way the hardware
reference does, and the highest-scoring layout wins. Unmapped or write-only
reads return the **open-bus** value (the last byte the data bus carried),
latched in the MDR.

Access timing follows the hardware's bus-wait behaviour: `$2000–$3FFF` and
`$4200–$5FFF` are fast (6 master cycles), `$4000–$41FF` (the joypad ports) is
extra-slow (12), and FastROM (`$80–$FF` at `$8000–$FFFF`) drops from 8 to 6 when
enabled.

## DMA & HDMA

| | |
|---|---|
| **On the console** | eight channels that copy between the A-bus and a `$21xx` port: in one burst (DMA) or a few bytes per scanline (HDMA) |
| **In luna** | `crates/luna-core/src/dma/` (`controller.rs`, `channel.rs`, `bus.rs`) |
| **Proven by** | a regression test per confirmed divergence in that module; `tools/validate-hdma-corpus.sh` on commercial titles, because the golden suite does not reach the edge cases |
| **Grade** | **A−** (scorecard: *DMA / HDMA*) |
| **Open gaps** | [`docs/hdma_ares_audit.md`](https://github.com/k0b3n4irb/luna/blob/main/docs/hdma_ares_audit.md), a line-by-line comparison with ares: rows 13, 15 to 17 and 19 are open |

The DMA and HDMA controllers live in `luna-core` as the `crate::dma` module.
**DMA** moves a block between the A-bus and a B-bus port (typically a PPU
register) and halts the CPU while it runs. **HDMA** streams a small table to a
register on each scanline, which is how games paint gradients, raster splits and
status bars.

HDMA is a [pillar subsystem](../method/faithful-port.md): it is shared by every
game and has been the source of repeated game-specific rendering bugs, so it is
held to a living, line-by-line audit against the hardware reference — covering
the edge cases (count-0 line headers, mid-frame enable, indirect addressing, the
transfer-mode patterns) that the golden test suite alone does not reach.
