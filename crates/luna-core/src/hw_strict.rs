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
    /// R10 — a CPU write to `$2100` (INIDISP) while the picture is drawn
    /// (`HBlank` included), with bit 7 set on the data bus just before the write
    /// and bit 7 clear in the value: the PPU latches the register early, from
    /// the previous bus byte, and on a three-chip console corrupts OBJ tile
    /// data. The mitigation is a write whose preceding bus byte has bit 7
    /// clear (`sta $0f2100`, not `sta $802100`). Source: the hardware tests of
    /// undisbeliever/snes-test-roms (`inidisp-d7-glitch-test.asm`), not a
    /// reference emulator: neither ares (`ppu/io.cpp:193-197`) nor Mesen2
    /// (`SnesPpu.cpp:1889-1896`) models the early latch.
    InidispEarlyLatch,
    /// R11 — a CPU write to `$2133` (SETINI) while the picture is drawn
    /// (force blank clear, before `VBlank`) when bits 1 or 6 of the value
    /// differ from the previous bus byte: the PPU may latch the register early
    /// and show the old bits (`setini-early-read-mode7ex.asm`: red or blue
    /// dots). That `VBlank` and force blank exempt it is a HYPOTHESIS carried
    /// over from INIDISP, not a measurement.
    SetiniEarlyLatch,
    /// R12 — brightness 0 (`$2100` bits 0-3 clear) with the display on
    /// (force blank clear), over a picture that is not black, held for every
    /// picture line of one frame. A console does not render brightness 0 as
    /// black but as a faint picture; luna (like ares and Mesen2) draws black.
    /// Source: undisbeliever's `inidisp_brightness_0.asm` ("a simple test to
    /// demonstrate brightness 0 is not black on a SNES console"); the file
    /// states neither the level nor the console model, so none is claimed.
    /// An ADVISORY of a fidelity difference, not a hazard: the console loses
    /// nothing, the picture is merely not the one luna shows. A shorter run
    /// (a fade passing through, a few lines at the top of a frame) is
    /// deliberately not flagged: measured, it is what clean commercial ROMs do.
    BrightnessZero,
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
            Self::InidispEarlyLatch => "R10",
            Self::SetiniEarlyLatch => "R11",
            Self::BrightnessZero => "R12",
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
            Self::InidispEarlyLatch => {
                "$2100 write with bit 7 on the data bus and bit 7 clear in the value, picture drawn: early latch"
            }
            Self::SetiniEarlyLatch => {
                "$2133 write differing from the data bus in bit 1 or 6, picture drawn: early latch (hypothesis)"
            }
            Self::BrightnessZero => {
                "brightness 0 with the display on over a lit picture, a whole frame: a console shows it faintly, luna draws black (advisory)"
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
    /// Consecutive picture lines that were brightness 0 with a lit row (R12).
    dark_run: u32,
    /// Address of the last CPU write to `$2100`, the site R12 is reported at.
    pub(crate) inidisp_pc: u32,
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

    /// A picture line was drawn: judge R12 (`dark_lit`: brightness 0, display
    /// on, a pixel above black on the row). The rule fires once, on the line
    /// where the run of such lines reaches `last_line`, the last picture
    /// line, which a run can only do by having started on line 1: one whole
    /// frame. A shorter run is a blip (a fade-in begun a few lines into the
    /// frame, a status bar) and is not flagged.
    pub(crate) fn picture_line(&mut self, dark_lit: bool, line: u16, last_line: u16, frame: u64) {
        if !dark_lit {
            self.dark_run = 0;
            return;
        }
        self.dark_run += 1;
        if self.dark_run == u32::from(last_line) {
            self.note(
                HwRule::BrightnessZero,
                self.inidisp_pc,
                0x2100,
                MemOrigin::Cpu,
                (frame, line, 0),
            );
        }
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

/// Does a CPU write to `$2100` / `$2133` break R10 / R11? `offset` is the byte
/// offset from `$2100`, `prev_bus` the byte on the data bus just before the
/// write (the previous bus value, not the value written), `value` the byte
/// written. Pure, so the edges are testable. Unlike [`ppu_access_rule`] it
/// looks at the bus, so only a CPU write can ask: a DMA drives the B bus
/// itself.
#[must_use]
pub const fn early_latch_rule(
    offset: u8,
    prev_bus: u8,
    value: u8,
    line: u16,
    vblank_start: u16,
    force_blank: bool,
) -> Option<HwRule> {
    let drawing = line < vblank_start;
    match offset {
        // The mitigation of the hardware test is to enter force blank with
        // bit 7 set, so a value with bit 7 set is silent.
        0x00 if drawing && prev_bus & 0x80 != 0 && value & 0x80 == 0 => {
            Some(HwRule::InidispEarlyLatch)
        }
        0x33 if drawing && !force_blank && (prev_bus ^ value) & 0x42 != 0 => {
            Some(HwRule::SetiniEarlyLatch)
        }
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
    fn early_latch_edges() {
        // R10: bit 7 on the bus, bit 7 clear in the value, picture drawn.
        let r10 = |prev, val, line| early_latch_rule(0x00, prev, val, line, NTSC, false);
        assert_eq!(r10(0x8F, 0x0F, 100), Some(HwRule::InidispEarlyLatch));
        assert_eq!(r10(0x8F, 0x0F, 0), Some(HwRule::InidispEarlyLatch));
        assert_eq!(r10(0x8F, 0x0F, 224), Some(HwRule::InidispEarlyLatch));
        assert_eq!(r10(0x8F, 0x0F, 225), None, "VBlank");
        assert_eq!(r10(0x21, 0x0F, 100), None, "bus bit 7 clear");
        assert_eq!(r10(0x8F, 0x8F, 100), None, "entering force blank");
        assert_eq!(
            early_latch_rule(0x00, 0x8F, 0x0F, 230, OVERSCAN, false),
            Some(HwRule::InidispEarlyLatch)
        );
        // R10 does not depend on force blank (the register is being written).
        assert_eq!(
            early_latch_rule(0x00, 0x80, 0x00, 10, NTSC, true),
            Some(HwRule::InidispEarlyLatch)
        );
        // R11: bits 1 / 6 differ, force blank clear, picture drawn.
        let r11 = |prev, val, line, fb| early_latch_rule(0x33, prev, val, line, NTSC, fb);
        assert_eq!(r11(0x00, 0x40, 100, false), Some(HwRule::SetiniEarlyLatch));
        assert_eq!(r11(0x00, 0x02, 100, false), Some(HwRule::SetiniEarlyLatch));
        assert_eq!(r11(0x40, 0x40, 100, false), None, "same bits");
        assert_eq!(
            r11(0x00, 0x01, 100, false),
            None,
            "bit 0 is not in the mask"
        );
        assert_eq!(r11(0x00, 0x40, 225, false), None, "VBlank");
        assert_eq!(r11(0x00, 0x40, 100, true), None, "force blank");
        // Other registers are not concerned.
        assert_eq!(early_latch_rule(0x01, 0x80, 0x00, 100, NTSC, false), None);
    }

    #[test]
    fn r12_fires_once_when_the_run_covers_every_picture_line() {
        let mut log = HwFaultLog::default();
        for line in 1..=224 {
            log.picture_line(true, line, 224, 7);
        }
        let (faults, _) = log.take();
        assert_eq!(faults.len(), 1);
        assert_eq!(faults[0].rule, HwRule::BrightnessZero);
        assert_eq!((faults[0].frame, faults[0].line), (7, 224));
        // The next frame: line 0 shows nothing (not lit), the run restarts.
        log.picture_line(false, 0, 224, 8);
        for line in 1..=224 {
            log.picture_line(true, line, 224, 8);
        }
        assert_eq!(log.take().0[0].count, 1, "a new journal after take");
    }

    #[test]
    fn r12_ignores_a_blip_shorter_than_the_frame() {
        let mut log = HwFaultLog::default();
        // 223 lines, one short of a frame; then the picture comes up.
        for line in 1..=223 {
            log.picture_line(true, line, 224, 1);
        }
        log.picture_line(false, 224, 224, 1);
        log.picture_line(false, 0, 224, 2);
        // A fade begun a few lines into the frame.
        for line in 9..=224 {
            log.picture_line(true, line, 224, 2);
        }
        assert!(log.take().0.is_empty());
    }

    #[test]
    fn r12_follows_the_overscan_height() {
        let mut log = HwFaultLog::default();
        for line in 1..=224 {
            log.picture_line(true, line, 239, 1);
        }
        assert!(log.take().0.is_empty(), "224 lines are short of 239");
        for line in 225..=239 {
            log.picture_line(true, line, 239, 1);
        }
        assert_eq!(log.take().0.len(), 1);
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
