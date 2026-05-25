//! The [`Mapper`] trait for cartridge mappings.

use crate::types::Addr24;

/// SNES cartridge mapping mode.
///
/// Determines how the cartridge ROM and SRAM are mapped into the 24-bit
/// CPU address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapperKind {
    /// Mode 20 — 32 KB ROM pages at `$8000-$FFFF`, mirrored across many
    /// banks. Most common for small to medium games.
    LoRom,
    /// Mode 21 — 64 KB ROM pages at `$0000-$FFFF` of banks `$40-$7D`
    /// (and mirrors). Used for larger games.
    HiRom,
    /// Mode 25 — extended HiROM allowing > 32 Mbit ROMs.
    ExHiRom,
    /// SA-1 mapping (Super Mario RPG, Kirby Super Star, etc.).
    Sa1,
    /// Super FX mapping (Star Fox, Yoshi's Island, Doom).
    SuperFx,
    /// S-DD1 (Star Ocean, Street Fighter Alpha 2).
    Sdd1,
    /// SPC7110 (Far East of Eden Zero).
    Spc7110,
}

/// A cartridge mapper: routes CPU bus accesses to ROM, SRAM, and
/// coprocessor regions as defined by the mapping mode.
///
/// The mapper does NOT charge memory-speed cycles — that's the bus's
/// responsibility (see [`crate::Bus::io_cycle`]).
pub trait Mapper {
    /// Identify the mapping mode.
    fn kind(&self) -> MapperKind;

    /// Returns `Some(byte)` if `addr` falls inside a region this mapper
    /// owns (ROM / SRAM / coprocessor MMIO), or `None` if it falls
    /// outside (in which case the bus will route to WRAM / PPU / etc.).
    fn read(&mut self, addr: Addr24) -> Option<u8>;

    /// Mirror of [`Mapper::read`] for writes. Returns `true` if the
    /// mapper accepted the write (so the bus knows not to fall through),
    /// `false` otherwise.
    fn write(&mut self, addr: Addr24, value: u8) -> bool;

    /// Size of the ROM in bytes.
    fn rom_size(&self) -> usize;

    /// Size of the SRAM in bytes (0 if none).
    fn sram_size(&self) -> usize;

    /// Step the cartridge coprocessor (SA-1 / Super FX / DSP-1 / …)
    /// forward by approximately `main_mclk` master cycles of main-CPU
    /// progress. Default = no-op for plain LoROM / HiROM carts.
    ///
    /// Coproc mappers translate that to the right number of coproc
    /// instructions internally (e.g. SA-1 is 10.74 MHz ≈ 2× the main
    /// CPU's max FastROM rate, so it consumes ~2 SA-1 instructions
    /// per main-CPU \`mclk\`). Implementations should be safely
    /// callable even when their coprocessor is reset / stopped.
    fn step_coproc(&mut self, _main_mclk: u32) {}

    /// `true` while the cartridge coprocessor is asserting an IRQ line
    /// onto the main CPU (SA-1 SCNT bit 7 latched + SIE bit 7 enabled,
    /// for instance). The main-CPU bus ORs this into its own
    /// `irq_pending` so the CPU services it through its normal IRQ path.
    /// Plain LoROM / HiROM carts return `false`.
    fn coproc_main_irq_pending(&self) -> bool {
        false
    }
}
