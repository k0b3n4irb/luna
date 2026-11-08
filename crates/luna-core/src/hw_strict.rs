//! Hardware strict mode — a read-only journal of accesses a real console
//! refuses, ignores or corrupts, which an emulator is free to let through.
//!
//! Each rule is a *fact of the hardware* taken from the references (ares
//! `ppu/io.cpp`, `cpu/dma.cpp`, `cpu/io.cpp`; Mesen2 `SnesPpu.cpp`
//! `CanAccessVram` / `CanAccessCgram`). The journal only observes: it
//! never changes what the emulation does, so a frame hash is identical with
//! and without it. Off by default ([`crate::Snes::enable_hw_strict`]).

use std::collections::HashMap;

use crate::snes::MemOrigin;

/// One rule of the hardware strict mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HwRule {
    /// R1 — a write to `$2118` / `$2119` while the picture is drawn (not in
    /// `VBlank`, not forced blank): the byte is dropped (ares `writeVRAM`,
    /// Mesen2 `CanAccessVram`).
    VramWrite,
    /// R2 — a write to `$2104` while the picture is drawn: it lands on the
    /// sprite the PPU is evaluating, not at the OAM address (ares
    /// `writeOAM`).
    OamWrite,
    /// R3 — a write to `$2122` while the beam is in the picture window
    /// (line 1 to the last visible one, H clock 88 to 1095): it lands on the
    /// entry the PPU is fetching (ares `writeCGRAM`, Mesen2
    /// `CanAccessCgram`).
    CgramWrite,
    /// R4 — a read of `$2138` / `$2139` / `$213A` / `$213B` under the
    /// conditions of R1-R3: the value is not the addressed byte.
    PpuRead,
    /// R5 — a DMA / HDMA A-bus access in the forbidden zone (banks
    /// `$00-$3F` / `$80-$BF`: `$2100-$21FF`, `$4000-$41FF`, `$4200-$421F`,
    /// `$4300-$437F`): a read returns 0, a write is dropped (ares
    /// `Channel::validA`, `dma.cpp:54-60`).
    DmaForbiddenA,
    /// R6 — a DMA transfer between WRAM and `$2180` (WMDATA): the B side is
    /// suppressed and a B-to-A read returns 0 (ares `Channel::transfer`,
    /// `dma.cpp:93-94`).
    DmaWramPort,
    /// R8 — a manual read of `$4016` / `$4017` while the auto-joypad read is
    /// running (`$4212` bit 0 set): the pad's shift register is being
    /// clocked by the console, so the bit read is not the one the program
    /// expects (ares `cpu/io.cpp` `$4212`, `$4016`).
    AutoJoypadRead,
    /// R9 — a CPU read of `$4218-$421F` while the auto-joypad read is running
    /// (`$4212` bit 0 set): the registers are the shift registers the console
    /// is filling, so the program gets a partial word (ares `cpu/io.cpp`
    /// returns `io.joy1..4` unfiltered; Mesen2 flags the read as
    /// `SnesReadDuringAutoJoy`). R7 was never issued.
    AutoJoypadRegRead,
}

impl HwRule {
    /// Short stable code, `R1`..: what a report and a test name.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::VramWrite => "R1",
            Self::OamWrite => "R2",
            Self::CgramWrite => "R3",
            Self::PpuRead => "R4",
            Self::DmaForbiddenA => "R5",
            Self::DmaWramPort => "R6",
            Self::AutoJoypadRead => "R8",
            Self::AutoJoypadRegRead => "R9",
        }
    }

    /// One-line statement of the rule.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::VramWrite => "VRAM write ($2118/$2119) outside VBlank and forced blank: dropped",
            Self::OamWrite => "OAM write ($2104) outside VBlank and forced blank: misdirected",
            Self::CgramWrite => "CGRAM write ($2122) while the PPU fetches colours: misdirected",
            Self::PpuRead => {
                "PPU memory read ($2138-$213B) while the picture is drawn: not the addressed byte"
            }
            Self::DmaForbiddenA => {
                "DMA A-bus access in $2100-$21FF / $4000-$41FF / $4200-$421F / $4300-$437F: read as 0 or dropped"
            }
            Self::DmaWramPort => "DMA between WRAM and $2180 (WMDATA): the B side does not happen",
            Self::AutoJoypadRead => "manual $4016/$4017 read during the auto-joypad read",
            Self::AutoJoypadRegRead => {
                "$4218-$421F read during the auto-joypad read: a partial word"
            }
        }
    }
}

/// One distinct fault: a rule broken from one instruction at one address,
/// with how many times it recurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwFault {
    /// The rule broken.
    pub rule: HwRule,
    /// 24-bit address of the instruction whose access (or DMA start) it was.
    pub pc: u32,
    /// The address touched: `$21xx` for a PPU register, the A-bus address
    /// for a DMA rule, `$4016` / `$4017` for the joypad.
    pub addr: u32,
    /// Who made the access.
    pub origin: MemOrigin,
    /// Frame of the first occurrence.
    pub frame: u64,
    /// Scanline of the first occurrence.
    pub line: u16,
    /// Master-clock position in the line of the first occurrence.
    pub hclock: u16,
    /// How many times this (rule, pc, address) happened.
    pub count: u64,
}

/// Distinct faults kept before further new ones are only counted.
pub const HW_FAULT_MAX_DISTINCT: usize = 4096;

/// The journal: faults folded by (rule, pc, address), bounded.
#[derive(Debug, Default)]
pub struct HwFaultLog {
    faults: Vec<HwFault>,
    index: HashMap<(HwRule, u32, u32), usize>,
    /// New distinct faults not kept because the journal was full.
    pub overflow: u64,
}

impl HwFaultLog {
    /// Record one occurrence.
    pub(crate) fn note(
        &mut self,
        rule: HwRule,
        pc: u32,
        addr: u32,
        origin: MemOrigin,
        at: (u64, u16, u16),
    ) {
        if let Some(&i) = self.index.get(&(rule, pc, addr)) {
            self.faults[i].count += 1;
            return;
        }
        if self.faults.len() >= HW_FAULT_MAX_DISTINCT {
            self.overflow += 1;
            return;
        }
        self.index.insert((rule, pc, addr), self.faults.len());
        self.faults.push(HwFault {
            rule,
            pc,
            addr,
            origin,
            frame: at.0,
            line: at.1,
            hclock: at.2,
            count: 1,
        });
    }

    /// Drain the journal (in order of first occurrence) and its overflow.
    pub(crate) fn take(&mut self) -> (Vec<HwFault>, u64) {
        self.index.clear();
        (
            std::mem::take(&mut self.faults),
            std::mem::take(&mut self.overflow),
        )
    }
}

/// First and last H clock (master cycles, within a line) of the window in
/// which the PPU owns CGRAM: `[88, 1096)` (ares `readCGRAM`/`writeCGRAM`
/// `hcounter() >= 88 && < 1096`; Mesen2 `CanAccessCgram`).
const CGRAM_WINDOW: std::ops::Range<u16> = 88..1096;

/// Does a PPU-register access break a rule at this beam position? `offset`
/// is the byte offset from `$2100`. `vblank_start` is the first `VBlank`
/// line (225, or 240 with overscan: ares `vdisp`, Mesen2
/// `_vblankStartScanline`). Pure, so the edges are testable.
#[must_use]
pub fn ppu_access_rule(
    offset: u8,
    write: bool,
    line: u16,
    hclock: u16,
    vblank_start: u16,
    force_blank: bool,
) -> Option<HwRule> {
    // ares: `!displayDisable && vcounter() < vdisp()` (line 0 included).
    let drawing = !force_blank && line < vblank_start;
    let fetching_colours = drawing && line > 0 && CGRAM_WINDOW.contains(&hclock);
    match (offset, write) {
        (0x18 | 0x19, true) if drawing => Some(HwRule::VramWrite),
        (0x04, true) if drawing => Some(HwRule::OamWrite),
        (0x22, true) if fetching_colours => Some(HwRule::CgramWrite),
        (0x38..=0x3A, false) if drawing => Some(HwRule::PpuRead),
        (0x3B, false) if fetching_colours => Some(HwRule::PpuRead),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC: u16 = 225;
    const OVERSCAN: u16 = 240;

    #[test]
    fn vram_write_edge_is_the_vblank_line() {
        // Last picture line: refused. First VBlank line: allowed.
        assert_eq!(
            ppu_access_rule(0x18, true, 224, 600, NTSC, false),
            Some(HwRule::VramWrite)
        );
        assert_eq!(ppu_access_rule(0x18, true, 225, 600, NTSC, false), None);
        assert_eq!(
            ppu_access_rule(0x19, true, 239, 600, OVERSCAN, false),
            Some(HwRule::VramWrite)
        );
        assert_eq!(ppu_access_rule(0x19, true, 240, 600, OVERSCAN, false), None);
        // Under overscan the edge moves: line 225 is still picture.
        assert_eq!(
            ppu_access_rule(0x19, true, 225, 600, OVERSCAN, false),
            Some(HwRule::VramWrite)
        );
    }

    #[test]
    fn line_zero_is_still_refused_for_vram_and_oam() {
        assert_eq!(
            ppu_access_rule(0x18, true, 0, 10, NTSC, false),
            Some(HwRule::VramWrite)
        );
        assert_eq!(
            ppu_access_rule(0x04, true, 0, 10, NTSC, false),
            Some(HwRule::OamWrite)
        );
    }

    #[test]
    fn forced_blank_silences_every_rule() {
        for off in [0x04, 0x18, 0x19, 0x22] {
            assert_eq!(ppu_access_rule(off, true, 100, 600, NTSC, true), None);
        }
        for off in [0x38, 0x39, 0x3A, 0x3B] {
            assert_eq!(ppu_access_rule(off, false, 100, 600, NTSC, true), None);
        }
    }

    #[test]
    fn cgram_window_edges() {
        let w = |line, h| ppu_access_rule(0x22, true, line, h, NTSC, false);
        assert_eq!(w(100, 87), None);
        assert_eq!(w(100, 88), Some(HwRule::CgramWrite));
        assert_eq!(w(100, 1095), Some(HwRule::CgramWrite));
        assert_eq!(w(100, 1096), None);
        // Line 0 and VBlank: the PPU is not fetching colours.
        assert_eq!(w(0, 600), None);
        assert_eq!(w(225, 600), None);
        assert_eq!(w(224, 600), Some(HwRule::CgramWrite));
    }

    #[test]
    fn reads_follow_the_same_conditions() {
        assert_eq!(
            ppu_access_rule(0x39, false, 50, 10, NTSC, false),
            Some(HwRule::PpuRead)
        );
        assert_eq!(ppu_access_rule(0x39, false, 225, 10, NTSC, false), None);
        assert_eq!(
            ppu_access_rule(0x3B, false, 50, 600, NTSC, false),
            Some(HwRule::PpuRead)
        );
        assert_eq!(ppu_access_rule(0x3B, false, 50, 1100, NTSC, false), None);
        // A read of the status register is not one of the rules.
        assert_eq!(ppu_access_rule(0x3F, false, 50, 600, NTSC, false), None);
    }

    #[test]
    fn the_journal_folds_by_rule_pc_and_address() {
        let mut log = HwFaultLog::default();
        for _ in 0..3 {
            log.note(HwRule::VramWrite, 0x8000, 0x2118, MemOrigin::Cpu, (1, 2, 3));
        }
        log.note(HwRule::VramWrite, 0x8004, 0x2118, MemOrigin::Cpu, (1, 2, 3));
        let (faults, overflow) = log.take();
        assert_eq!(overflow, 0);
        assert_eq!(faults.len(), 2);
        assert_eq!(faults[0].count, 3);
        assert_eq!(faults[1].count, 1);
    }
}
