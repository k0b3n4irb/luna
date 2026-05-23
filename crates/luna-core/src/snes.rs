//! Top-level [`Snes`] machine struct.
//!
//! Wires together a `Cpu65816`, 128 KB of WRAM, the cartridge mapper, and
//! a placeholder MMIO stub. The PPU / APU / DMA are still TODOs; reads
//! from their registers return `0xFF` (open-bus convention) and writes
//! are silently dropped.

use luna_bus::lorom::LoRomMapper;
use luna_bus::{Addr24, Bus, MCycles, Mapper, MapperKind, address_speed, bank_of, offset_of};
use luna_cartridge::Cartridge;
use luna_cpu_65c816::Cpu;
use luna_ppu::Ppu;

/// Top-level SNES machine.
pub struct Snes {
    /// Main CPU (65C816).
    pub cpu: Cpu,
    /// Picture Processing Unit — VRAM / CGRAM / OAM + registers.
    pub ppu: Ppu,
    /// 128 KB Work RAM (banks `$7E-$7F` and the LowRAM mirror).
    pub wram: Box<[u8; 0x20000]>,
    /// Cartridge mapper (LoROM in P0.6; other mappers in V1+).
    pub mapper: Box<dyn Mapper + Send>,
    /// FastROM `MEMSEL` bit — when set, ROM in banks `$80-$FF` at
    /// `$8000-$FFFF` is FAST (6 mclk) instead of SLOW (8 mclk).
    pub fast_rom: bool,
    /// Latched NMI line (`$4210` read clears it).
    pub nmi_pending: bool,
    /// IRQ line currently asserted.
    pub irq_pending: bool,
    /// Total master cycles consumed since reset.
    pub total_mclk: MCycles,
}

impl Snes {
    /// Build a new machine from a parsed cartridge.
    ///
    /// Panics if the cartridge layout is not supported by the V1 mapper
    /// set — currently LoROM only. HiROM / SA-1 / Super FX land in
    /// later phases.
    pub fn from_cartridge(cart: Cartridge) -> Self {
        let mapper: Box<dyn Mapper + Send> = match cart.header.mapper_kind {
            MapperKind::LoRom => {
                let sram_bytes = (cart.header.sram_size_kb as usize) * 1024;
                Box::new(LoRomMapper::new(cart.rom, sram_bytes))
            }
            other => {
                panic!(
                    "luna-core P0.6 only supports LoROM; cartridge claims {other:?}. \
                     HiROM / SA-1 / Super FX land in P1+."
                );
            }
        };

        Self {
            cpu: Cpu::new(),
            ppu: Ppu::new(),
            wram: Box::new([0; 0x20000]),
            mapper,
            fast_rom: cart.header.fast_rom,
            nmi_pending: false,
            irq_pending: false,
            total_mclk: 0,
        }
    }

    /// Run the CPU reset sequence: read the reset vector at `$00:FFFC`
    /// via the bus and load `PC`.
    pub fn reset(&mut self) {
        let Snes {
            cpu,
            ppu,
            wram,
            mapper,
            fast_rom,
            nmi_pending,
            irq_pending,
            total_mclk,
        } = self;
        let mut bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
        };
        cpu.reset(&mut bus);
    }

    /// Execute one CPU instruction. Returns the master-cycle cost of
    /// that instruction (accumulated through [`Bus::io_cycle`]).
    pub fn step(&mut self) -> MCycles {
        let before = self.total_mclk;
        let Snes {
            cpu,
            ppu,
            wram,
            mapper,
            fast_rom,
            nmi_pending,
            irq_pending,
            total_mclk,
        } = self;
        let mut bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
        };
        cpu.step(&mut bus);
        self.total_mclk - before
    }
}

// =============================================================================
// SnesBus
// =============================================================================

/// View of the machine exposed to the CPU during one instruction. Re-built
/// from scratch on each [`Snes::step`] so the borrow checker can prove
/// that the CPU and the bus borrow disjoint fields of `Snes`.
struct SnesBus<'a> {
    wram: &'a mut [u8; 0x20000],
    mapper: &'a mut dyn Mapper,
    ppu: &'a mut Ppu,
    fast_rom: bool,
    nmi: &'a mut bool,
    irq: &'a mut bool,
    mclk_total: &'a mut MCycles,
}

impl<'a> SnesBus<'a> {
    /// Resolve `addr` against the WRAM regions; returns the in-array
    /// offset if it maps to WRAM, else `None`.
    fn wram_offset(addr: Addr24) -> Option<usize> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        // LowRAM mirror: banks $00-$3F and $80-$BF, offsets $0000-$1FFF.
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset < 0x2000 {
            return Some(usize::from(offset));
        }
        // Full WRAM: banks $7E-$7F at any offset.
        if matches!(bank, 0x7E..=0x7F) {
            let high = usize::from(bank - 0x7E) << 16;
            return Some(high | usize::from(offset));
        }
        None
    }
}

impl<'a> SnesBus<'a> {
    /// Returns `Some(offset)` if `addr` falls in the PPU MMIO range
    /// (`$00-$3F:$2100-$213F` and the `$80-$BF` mirror). The offset is
    /// relative to `$2100` (0x00-0x3F).
    fn ppu_offset(addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && matches!(offset, 0x2100..=0x213F) {
            Some((offset - 0x2100) as u8)
        } else {
            None
        }
    }
}

impl<'a> Bus for SnesBus<'a> {
    fn read(&mut self, addr: Addr24) -> u8 {
        let speed = address_speed(addr, self.fast_rom);
        self.io_cycle(speed.mcycles());

        if let Some(o) = Self::wram_offset(addr) {
            return self.wram[o];
        }
        if let Some(off) = Self::ppu_offset(addr) {
            return self.ppu.read(off);
        }
        if let Some(v) = self.mapper.read(addr) {
            return v;
        }
        // Open bus stub — anything we don't yet route returns 0xFF.
        // Real hardware exposes the last value on the data bus, which
        // we'll model when it matters (CPU-internal registers $4200+
        // land in P1.3).
        0xFF
    }

    fn write(&mut self, addr: Addr24, value: u8) {
        let speed = address_speed(addr, self.fast_rom);
        self.io_cycle(speed.mcycles());

        if let Some(o) = Self::wram_offset(addr) {
            self.wram[o] = value;
        } else if let Some(off) = Self::ppu_offset(addr) {
            self.ppu.write(off, value);
        } else if self.mapper.write(addr, value) {
            // Mapper claims SRAM writes; ROM writes are silently dropped.
        } else {
            // MMIO writes outside PPU still dropped in P1.1.
        }
    }

    fn io_cycle(&mut self, mcycles: MCycles) {
        *self.mclk_total = self.mclk_total.saturating_add(mcycles);
    }

    fn nmi_pending(&self) -> bool {
        *self.nmi
    }

    fn irq_pending(&self) -> bool {
        *self.irq
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use luna_bus::make_addr;

    /// Build a 32 KB LoROM that starts with `LDA #$42 ; STA $7E0000 ; STP`
    /// and has its reset vector pointing at `$8000`.
    fn demo_lorom() -> Cartridge {
        let mut rom = vec![0xEA; 32 * 1024]; // NOP-padded
        // Reset vector at $7FFC = $8000 (already in bank 0 LoROM space).
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        // Header at $7FC0.
        let off = 0x7FC0;
        for (i, b) in b"LUNA P0.6 DEMO       ".iter().enumerate() {
            rom[off + i] = *b;
        }
        rom[off + 0x15] = 0x20; // LoROM
        rom[off + 0x17] = 0x05; // 32 KB
        rom[off + 0x18] = 0x00; // no SRAM
        rom[off + 0x19] = 0x01; // NTSC
        // Checksum complement: 0x1234, checksum: !0x1234 = 0xEDCB.
        rom[off + 0x1C] = 0x34;
        rom[off + 0x1D] = 0x12;
        rom[off + 0x1E] = 0xCB;
        rom[off + 0x1F] = 0xED;
        // Program at $8000 (file offset 0):
        //   LDA #$42        A9 42
        //   STA $7E:0000    8F 00 00 7E
        //   STP             DB
        rom[0x0000] = 0xA9;
        rom[0x0001] = 0x42;
        rom[0x0002] = 0x8F;
        rom[0x0003] = 0x00;
        rom[0x0004] = 0x00;
        rom[0x0005] = 0x7E;
        rom[0x0006] = 0xDB;
        Cartridge::from_bytes(rom).unwrap()
    }

    #[test]
    fn from_cartridge_sets_initial_state() {
        let cart = demo_lorom();
        let snes = Snes::from_cartridge(cart);
        assert_eq!(snes.total_mclk, 0);
        assert!(!snes.nmi_pending);
        assert!(!snes.irq_pending);
    }

    #[test]
    fn reset_loads_pc_from_vector_via_lorom_mapper() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        assert_eq!(snes.cpu.pc, 0x8000);
        assert_eq!(snes.cpu.pb, 0x00);
    }

    #[test]
    fn step_lda_imm_then_sta_long() {
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        // LDA #$42
        snes.step();
        assert_eq!(snes.cpu.a8(), 0x42);
        // STA $7E:0000 — write goes through WRAM
        snes.step();
        assert_eq!(snes.wram[0], 0x42);
        // STP — CPU halts
        snes.step();
        assert!(snes.cpu.stopped);
    }

    #[test]
    fn wram_low_mirror_aliases_bank_7e() {
        // Direct write via the bus to bank 0, offset 0x100 should land
        // in WRAM[0x100] and be visible from bank 0x7E offset 0x100.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        let Snes {
            ppu,
            wram,
            mapper,
            fast_rom,
            nmi_pending,
            irq_pending,
            total_mclk,
            ..
        } = &mut snes;
        let mut bus = SnesBus {
            wram,
            mapper: mapper.as_mut(),
            ppu,
            fast_rom: *fast_rom,
            nmi: nmi_pending,
            irq: irq_pending,
            mclk_total: total_mclk,
        };
        bus.write(make_addr(0x00, 0x0100), 0xAA);
        // Read back from the mirror in $00:
        assert_eq!(bus.read(make_addr(0x00, 0x0100)), 0xAA);
        // And from $7E (full WRAM):
        assert_eq!(bus.read(make_addr(0x7E, 0x0100)), 0xAA);
    }

    #[test]
    fn cpu_writes_to_ppu_register_reach_the_ppu() {
        // Build a program that writes $42 to PPU $2100 (INIDISP).
        // Reuse demo_lorom() so the SRAM exponent / checksum etc. are
        // all set correctly — then patch in the program bytes.
        let cart = demo_lorom();
        let mut rom = cart.rom.clone();
        // Program at $8000 (file offset 0): LDA #$42, STA $2100
        rom[0] = 0xA9;
        rom[1] = 0x42;
        rom[2] = 0x8D;
        rom[3] = 0x00;
        rom[4] = 0x21;
        // Re-checksum so the header parser still accepts the ROM (we
        // overwrote the demo_lorom's STA-target program at offset 0).
        // For now, demo_lorom's checksum bytes at $7FDC-$7FDF are
        // already valid for the original ROM. Since we're patching
        // only 5 bytes, just keep the same checksum (parser only checks
        // complement vs checksum XOR, not against ROM contents).
        let cart = Cartridge::from_bytes(rom).unwrap();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset();
        snes.cpu.db = 0; // ensure data bank is 0 for abs addressing
        snes.step(); // LDA #$42
        snes.step(); // STA $2100
        assert_eq!(snes.ppu.inidisp, 0x42, "PPU INIDISP must reflect the write");
    }

    #[test]
    fn step_accumulates_master_cycles() {
        // NOP costs 6 master cycles (FastROM not set on a default LoROM,
        // so it's actually SLOW = 8). LDA #$42 reads opcode + operand =
        // 2 × 8 = 16. After running 1 NOP we should have ≥ 8 mclk total.
        let cart = demo_lorom();
        let mut snes = Snes::from_cartridge(cart);
        snes.reset(); // already pays 2 reads for the reset vector.
        let before = snes.total_mclk;
        snes.step(); // LDA #$42
        let after = snes.total_mclk;
        assert!(after > before, "step should advance master clock");
    }
}
