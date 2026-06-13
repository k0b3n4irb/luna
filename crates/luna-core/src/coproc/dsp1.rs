//! DSP-1 cartridge mapper (bus shim around the NEC uPD7725 core).
//!
//! The DSP-1 sits beside the game ROM on the cartridge and answers two
//! memory-mapped ports — the data register (`DR`) and status register
//! (`SR`). The CPU writes a command + operands to `DR`, polls `SR.rqm`
//! until the chip signals the result is ready, then reads it back from
//! `DR`. Games like Super Mario Kart use it for Mode 7 perspective math.
//!
//! [`Dsp1Mapper`] wraps a base ROM/SRAM mapper (`HiROM` for SMK's `1K`
//! board, `LoROM` for the `1B` boards) and intercepts the DSP DR/SR window,
//! delegating everything else. The chip itself is the standalone
//! [`luna_cpu_upd96050`] core, advanced on the `step_coproc` clock budget.

use luna_bus::hirom::HiRomMapper;
use luna_bus::lorom::LoRomMapper;
use luna_bus::mapper::{Mapper, MapperKind};
use luna_bus::types::{Addr24, NTSC_MASTER_HZ, bank_of, offset_of};
use luna_cpu_upd96050::{Revision, Upd96050};

/// DSP-1 oscillator (ares `loaduPD7725` default).
const DSP1_HZ: u64 = 7_600_000;

/// Combined `dsp1b.rom` firmware: program `0x1800` (2048 × 3-byte LE words)
/// followed by data `0x800` (1024 × 2-byte LE words).
const PROGRAM_BYTES: usize = 0x1800;
const FIRMWARE_BYTES: usize = 0x2000;

/// Parse a combined `dsp1b.rom` (little-endian) into the core's program +
/// data ROM. Returns `false` if the blob is too small.
fn load_firmware(dsp: &mut Upd96050, fw: &[u8]) -> bool {
    if fw.len() < FIRMWARE_BYTES {
        return false;
    }
    let program: Vec<u32> = (0..2048)
        .map(|i| {
            let o = i * 3;
            u32::from(fw[o]) | u32::from(fw[o + 1]) << 8 | u32::from(fw[o + 2]) << 16
        })
        .collect();
    let data: Vec<u16> = (0..1024)
        .map(|i| {
            let o = PROGRAM_BYTES + i * 2;
            u16::from(fw[o]) | u16::from(fw[o + 1]) << 8
        })
        .collect();
    dsp.load_program(&program);
    dsp.load_data(&data);
    true
}

/// A DSP-1 cartridge: a base ROM/SRAM mapper + the uPD7725 chip.
pub struct Dsp1Mapper {
    base: Box<dyn Mapper + Send>,
    dsp: Upd96050,
    /// `true` = `HiROM` `1K` board (DSP at `$00-1F:6000`); `false` = `LoROM`
    /// `1B` board (DSP at `$20-3F:8000`).
    hirom: bool,
    /// `false` when no firmware was supplied — the chip stays inert (the
    /// game runs but Mode 7 math returns nothing, as before).
    has_firmware: bool,
    /// DSP-cycle accumulator (numerator over [`NTSC_MASTER_HZ`]).
    cycle_acc: u64,
}

impl Dsp1Mapper {
    /// Build a DSP-1 mapper. `hirom` selects the base layout + DSP window;
    /// `firmware` is the combined `dsp1b.rom` (8 KB) when available.
    #[must_use]
    pub fn new(rom: Vec<u8>, sram_bytes: usize, firmware: Option<&[u8]>, hirom: bool) -> Self {
        let base: Box<dyn Mapper + Send> = if hirom {
            Box::new(HiRomMapper::new(rom, sram_bytes))
        } else {
            Box::new(LoRomMapper::new(rom, sram_bytes))
        };
        let mut dsp = Upd96050::new(Revision::Upd7725);
        let has_firmware = firmware.is_some_and(|fw| load_firmware(&mut dsp, fw));
        Self {
            base,
            dsp,
            hirom,
            has_firmware,
            cycle_acc: 0,
        }
    }

    /// If `(bank, offset)` falls in the DSP DR/SR window, return
    /// `Some(is_sr)` — the masked offset's bit 0 selects SR (odd) vs DR
    /// (even) per ares `necdsp/memory.cpp`.
    const fn dsp_select(&self, bank: u8, offset: u16) -> Option<bool> {
        let in_window = if self.hirom {
            // HiROM 1K: banks $00-$1F / $80-$9F at $6000-$7FFF.
            matches!(bank, 0x00..=0x1F | 0x80..=0x9F) && offset >= 0x6000 && offset <= 0x7FFF
        } else {
            // LoROM 1B: banks $20-$3F / $A0-$BF at $8000-$FFFF.
            matches!(bank, 0x20..=0x3F | 0xA0..=0xBF) && offset >= 0x8000
        };
        if in_window {
            Some(offset & 1 != 0)
        } else {
            None
        }
    }
}

impl Mapper for Dsp1Mapper {
    fn kind(&self) -> MapperKind {
        MapperKind::Dsp1
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        if let Some(is_sr) = self.dsp_select(bank_of(addr), offset_of(addr)) {
            return Some(if is_sr {
                self.dsp.read_sr()
            } else {
                self.dsp.read_dr()
            });
        }
        self.base.read(addr)
    }

    fn write(&mut self, addr: Addr24, value: u8) -> bool {
        if let Some(is_sr) = self.dsp_select(bank_of(addr), offset_of(addr)) {
            if is_sr {
                self.dsp.write_sr(value);
            } else {
                self.dsp.write_dr(value);
            }
            return true;
        }
        self.base.write(addr, value)
    }

    fn rom_size(&self) -> usize {
        self.base.rom_size()
    }

    fn sram_size(&self) -> usize {
        self.base.sram_size()
    }

    fn reset(&mut self) {
        self.base.reset();
        self.dsp.power();
        self.cycle_acc = 0;
    }

    fn step_coproc(&mut self, main_mclk: u32) {
        if !self.has_firmware {
            return;
        }
        // Advance the DSP at 7.6 MHz relative to the 21.477 MHz master
        // clock. ares synchronises the DSP before every DR/SR access; the
        // per-bus-access cadence here gives the same effect — the chip is
        // caught up whenever the CPU polls SR.rqm.
        self.cycle_acc += u64::from(main_mclk) * DSP1_HZ;
        while self.cycle_acc >= NTSC_MASTER_HZ {
            self.dsp.exec();
            self.cycle_acc -= NTSC_MASTER_HZ;
        }
    }
}
