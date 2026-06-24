# Memory, DMA & the bus

## The bus & mappers

`luna-bus` is the foundation every CPU and the system glue build on. It defines
the `Bus` trait, the 24-bit `Addr24` address type, the `MapperKind` enum, and
the per-mapper shims that translate a SNES address into a physical location:

| Mapper | Used by |
|---|---|
| **LoROM** | the majority of the library |
| **HiROM** | larger / later titles |
| **ExHiROM** | a few oversized carts |
| **SA-1** | the SA-1 coprocessor board |

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
