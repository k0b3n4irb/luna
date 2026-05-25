//! CPU-system registers at `$4200-$420F` and `$4210-$421F`.
//!
//! These live "between" the 65C816 and the rest of the chip: the
//! multiplier / divider are functional units, NMITIMEN and the
//! interrupt-status registers gate the CPU's interrupt service, and
//! HVBJOY exposes the PPU's H/V-blank state to game code.
//!
//! Reference: <https://problemkaputt.de/fullsnes.htm> §"S-CPU Registers".

/// CPU-side registers.
///
/// Reads from write-only registers return open-bus (we return the last
/// value seen on the CPU bus, which the calling [`crate::Snes`]
/// SnesBus tracks). Writes to read-only registers are silently dropped.
#[derive(Debug, Default)]
pub struct CpuRegs {
    /// `$4200` — NMITIMEN: NMI enable (bit 7), V-IRQ (bit 5),
    /// H-IRQ (bit 4), joypad auto-read (bit 0).
    pub nmitimen: u8,
    /// `$4201` — WRIO: programmable I/O pins (write side). Bit 7
    /// connects to the PPU latch (writing 0 latches H/V counters).
    pub wrio: u8,
    /// `$4202` — WRMPYA: multiplicand.
    pub wrmpya: u8,
    /// `$4203` — WRMPYB: multiplier. Writing this triggers the
    /// 8x8 = 16-bit multiplication; the result lands in `rdmpy`.
    pub wrmpyb: u8,
    /// `$4204/$4205` — WRDIVL/H: 16-bit dividend.
    pub wrdiv: u16,
    /// `$4206` — WRDVDD: 8-bit divisor. Writing this triggers the
    /// 16/8 = 16-bit division; quotient in `rddiv`, remainder in `rdmpy`.
    pub wrdvdd: u8,
    /// `$4207/$4208` — HTIMEL/H: 9-bit H-counter trigger (low + bit 0
    /// of high).
    pub htime: u16,
    /// `$4209/$420A` — VTIMEL/H: 9-bit V-counter trigger.
    pub vtime: u16,

    /// `$4210` read side — bit 7 = "NMI flag" (set on vblank when
    /// NMI is enabled, OR on any read in some hardware). Cleared on
    /// read. Bits 0-3 = CPU version (we return 2, the most common rev).
    pub nmi_flag: bool,
    /// `$4211` read side — bit 7 = "IRQ flag" (set when the IRQ
    /// condition fires). Cleared on read.
    pub irq_flag: bool,
    /// `$4212` read side — HVBJOY: bit 7 = V-blank, bit 6 = H-blank,
    /// bit 0 = joypad auto-read busy.
    pub hvbjoy: u8,

    /// Result of the multiplication ($4216/$4217) or the division
    /// remainder. Hardware shares the same 16-bit "RDMPY" register.
    pub rdmpy: u16,
    /// Result of the division ($4214/$4215).
    pub rddiv: u16,

    // ---------- Joypad auto-read ----------
    /// Current bitmask for player 1 (16-bit). Bit layout per SNES
    /// hardware: B Y SEL START Up Down Left Right A X L R 0 0 0 0
    /// (MSB → LSB). The front-end pushes this via [`Self::set_joypad`].
    pub joypad1: u16,
    /// Same for player 2.
    pub joypad2: u16,
    /// `$4218/$4219` — latched player-1 state copied here at the
    /// start of every VBlank when `NMITIMEN.0` is set. Stays put
    /// between latches, so games that read these registers see a
    /// stable per-frame snapshot.
    pub joypad1_latched: u16,
    /// `$421A/$421B` — latched player-2 state.
    pub joypad2_latched: u16,
}

impl CpuRegs {
    /// Build a powered-on register file (all zero).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a register at the 16-bit CPU bank-0 offset (`$4200-$421F`).
    /// Returns `None` if `offset` is outside that range.
    pub fn read(&mut self, offset: u16) -> Option<u8> {
        let v = match offset {
            // Most $4200-$420A registers are write-only; reads return
            // open-bus on real hardware. We return 0 so tests can
            // distinguish "definitely write-only" from "stub MMIO" at
            // the SnesBus level.
            0x4210 => {
                // RDNMI: bit 7 = nmi flag (cleared by this read).
                let v = if self.nmi_flag { 0x80 } else { 0x00 } | 0x02; // CPU rev 2
                self.nmi_flag = false;
                v
            }
            0x4211 => {
                // TIMEUP: bit 7 = irq flag (cleared by this read).
                let v = if self.irq_flag { 0x80 } else { 0x00 };
                self.irq_flag = false;
                v
            }
            0x4212 => self.hvbjoy,
            0x4214 => self.rddiv as u8,
            0x4215 => (self.rddiv >> 8) as u8,
            0x4216 => self.rdmpy as u8,
            0x4217 => (self.rdmpy >> 8) as u8,
            // Joypad auto-read latches at $4218-$421F. Standard SNES
            // only has 2 controller ports, so $421C-$421F (joypads 3
            // and 4 via multitap) return 0.
            0x4218 => self.joypad1_latched as u8,
            0x4219 => (self.joypad1_latched >> 8) as u8,
            0x421A => self.joypad2_latched as u8,
            0x421B => (self.joypad2_latched >> 8) as u8,
            0x421C..=0x421F => 0x00,
            0x4200..=0x420F | 0x4213 => return None,
            _ => return None,
        };
        Some(v)
    }

    /// Push the current per-frame joypad state into the auto-read
    /// latches. Called by [`crate::Snes::advance_one_scanline`] when
    /// the scheduler crosses the VBlank entry line, IF `NMITIMEN.0`
    /// is set. Also raises `HVBJOY.0` (auto-read busy) for the few
    /// scanlines the hardware spends in the auto-read sequence — see
    /// [`Self::clear_joypad_busy`].
    ///
    /// D-pad opposing-direction quirk: per ares' `gamepad.cpp`
    /// `latch()`, real hardware physically prevents `up + down` and
    /// `left + right` from being asserted simultaneously. We resolve
    /// the case by clearing both opposing bits — keyboard players
    /// who hit conflicting keys see a "no-direction" outcome rather
    /// than a glitched latch.
    pub fn latch_joypad_auto_read(&mut self) {
        if self.nmitimen & 0x01 == 0 {
            return;
        }
        self.joypad1_latched = Self::clean_dpad(self.joypad1);
        self.joypad2_latched = Self::clean_dpad(self.joypad2);
        self.hvbjoy |= 0x01;
    }

    /// Clear opposing D-pad bits (Up vs Down, Left vs Right) to model
    /// the physical lockout on real-HW controllers. Bit layout per
    /// the SNES JOY1L/JOY1H pair: bit 11 = Up, bit 10 = Down, bit 9
    /// = Left, bit 8 = Right.
    fn clean_dpad(mask: u16) -> u16 {
        let mut m = mask;
        if m & 0x0C00 == 0x0C00 {
            m &= !0x0C00; // up + down → drop both
        }
        if m & 0x0300 == 0x0300 {
            m &= !0x0300; // left + right → drop both
        }
        m
    }

    /// Drop the auto-read busy bit. Called a few scanlines after
    /// [`Self::latch_joypad_auto_read`] so games polling
    /// `$4212 & 0x01` see it clear before they continue.
    pub fn clear_joypad_busy(&mut self) {
        self.hvbjoy &= !0x01;
    }

    /// Set the current button bitmask for controller `idx` (`0` or
    /// `1`). The bitmask isn't visible to game code until the next
    /// auto-read latch (typically the next VBlank).
    ///
    /// Bit layout (high → low): B Y SEL START Up Down Left Right
    /// A X L R 0 0 0 0.
    pub fn set_joypad(&mut self, idx: usize, mask: u16) {
        match idx {
            0 => self.joypad1 = mask,
            1 => self.joypad2 = mask,
            _ => {}
        }
    }

    /// Write a register at the 16-bit CPU bank-0 offset.
    pub fn write(&mut self, offset: u16, value: u8) -> bool {
        match offset {
            0x4200 => self.nmitimen = value,
            0x4201 => self.wrio = value,
            0x4202 => self.wrmpya = value,
            0x4203 => {
                self.wrmpyb = value;
                // 8x8 = 16-bit unsigned multiplication. Result writes
                // RDMPY ($4216/$4217). Real hardware takes 8 mclk.
                self.rdmpy = u16::from(self.wrmpya) * u16::from(self.wrmpyb);
            }
            0x4204 => self.wrdiv = (self.wrdiv & 0xFF00) | u16::from(value),
            0x4205 => self.wrdiv = (self.wrdiv & 0x00FF) | (u16::from(value) << 8),
            0x4206 => {
                self.wrdvdd = value;
                // 16/8 = 16-bit unsigned division. Quotient → RDDIV
                // ($4214/$4215). Remainder → RDMPY ($4216/$4217).
                // Division by zero on the SNES gives:
                //   quotient = $FFFF
                //   remainder = original dividend
                if value == 0 {
                    self.rddiv = 0xFFFF;
                    self.rdmpy = self.wrdiv;
                } else {
                    let dividend = u32::from(self.wrdiv);
                    let divisor = u32::from(value);
                    self.rddiv = (dividend / divisor) as u16;
                    self.rdmpy = (dividend % divisor) as u16;
                }
            }
            0x4207 => self.htime = (self.htime & 0xFF00) | u16::from(value),
            0x4208 => self.htime = (self.htime & 0x00FF) | (u16::from(value & 0x01) << 8),
            0x4209 => self.vtime = (self.vtime & 0xFF00) | u16::from(value),
            0x420A => self.vtime = (self.vtime & 0x00FF) | (u16::from(value & 0x01) << 8),
            // $420B (MDMAEN) and $420C (HDMAEN) are routed to the DMA
            // controller, not this struct.
            0x420B | 0x420C => return false,
            0x420D => {
                // MEMSEL — bit 0 = FastROM enable. Stored externally
                // (luna-core::Snes.fast_rom), so we return "not us".
                return false;
            }
            // Read-only registers — writes ignored.
            0x4210..=0x421F => {}
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // Multiplication
    // ---------------------------------------------------------------

    #[test]
    fn multiplication_7_times_8_yields_56() {
        let mut r = CpuRegs::new();
        r.write(0x4202, 7);
        r.write(0x4203, 8); // triggers multiply
        assert_eq!(r.read(0x4216).unwrap(), 56);
        assert_eq!(r.read(0x4217).unwrap(), 0);
    }

    #[test]
    fn multiplication_full_16bit_range() {
        // 255 × 255 = 65 025 = $FE01.
        let mut r = CpuRegs::new();
        r.write(0x4202, 0xFF);
        r.write(0x4203, 0xFF);
        assert_eq!(r.rdmpy, 0xFE01);
    }

    #[test]
    fn multiplication_triggers_only_on_4203_write() {
        // Writing $4202 alone should NOT trigger the multiplication.
        let mut r = CpuRegs::new();
        r.write(0x4202, 0xFF);
        assert_eq!(r.rdmpy, 0);
    }

    // ---------------------------------------------------------------
    // Division
    // ---------------------------------------------------------------

    #[test]
    fn division_100_by_3_gives_33_remainder_1() {
        let mut r = CpuRegs::new();
        r.write(0x4204, 100);
        r.write(0x4205, 0);
        r.write(0x4206, 3); // triggers division
        assert_eq!(r.rddiv, 33);
        assert_eq!(r.rdmpy, 1);
    }

    #[test]
    fn division_by_zero_returns_ffff_and_dividend() {
        let mut r = CpuRegs::new();
        r.write(0x4204, 0x34);
        r.write(0x4205, 0x12); // dividend = $1234
        r.write(0x4206, 0); // divisor = 0
        assert_eq!(r.rddiv, 0xFFFF);
        assert_eq!(r.rdmpy, 0x1234);
    }

    #[test]
    fn division_triggers_only_on_4206_write() {
        let mut r = CpuRegs::new();
        r.write(0x4204, 100);
        r.write(0x4205, 0);
        // No $4206 write — division should not run yet.
        assert_eq!(r.rddiv, 0);
    }

    // ---------------------------------------------------------------
    // NMITIMEN, RDNMI, TIMEUP
    // ---------------------------------------------------------------

    #[test]
    fn nmitimen_stores_byte_verbatim() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x81);
        assert_eq!(r.nmitimen, 0x81);
    }

    #[test]
    fn rdnmi_returns_flag_and_clears_it() {
        let mut r = CpuRegs::new();
        r.nmi_flag = true;
        let v = r.read(0x4210).unwrap();
        assert!(v & 0x80 != 0, "bit 7 reflects the latched NMI flag");
        // Subsequent read should NOT see the flag set.
        let v = r.read(0x4210).unwrap();
        assert_eq!(v & 0x80, 0);
    }

    #[test]
    fn rdnmi_reports_cpu_revision_in_low_nibble() {
        let mut r = CpuRegs::new();
        let v = r.read(0x4210).unwrap();
        // We model CPU revision 2 — most common in the wild.
        assert_eq!(v & 0x0F, 0x02);
    }

    #[test]
    fn timeup_returns_irq_flag_and_clears_it() {
        let mut r = CpuRegs::new();
        r.irq_flag = true;
        let v = r.read(0x4211).unwrap();
        assert!(v & 0x80 != 0);
        let v = r.read(0x4211).unwrap();
        assert_eq!(v, 0);
    }

    // ---------------------------------------------------------------
    // HTIME / VTIME 9-bit packing
    // ---------------------------------------------------------------

    #[test]
    fn htime_high_byte_uses_only_bit_0() {
        let mut r = CpuRegs::new();
        r.write(0x4207, 0xFF);
        r.write(0x4208, 0xFF); // only bit 0 should land in htime bit 8
        assert_eq!(r.htime, 0x01FF);
    }

    // ---------------------------------------------------------------
    // Range checks
    // ---------------------------------------------------------------

    #[test]
    fn reads_outside_range_return_none() {
        let mut r = CpuRegs::new();
        assert!(r.read(0x41FF).is_none());
        assert!(r.read(0x4220).is_none());
    }

    #[test]
    fn write_to_dma_enable_returns_false_so_caller_routes_elsewhere() {
        let mut r = CpuRegs::new();
        assert!(!r.write(0x420B, 0xFF), "$420B is DMA, not CpuRegs");
        assert!(!r.write(0x420C, 0xFF), "$420C is HDMA, not CpuRegs");
    }

    // ---------------------------------------------------------------
    // Joypad auto-read
    // ---------------------------------------------------------------

    #[test]
    fn joypad_latch_copies_live_state_when_nmitimen_0_set() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01); // NMITIMEN.0 = auto-read enable
        r.set_joypad(0, 0xAA55);
        r.set_joypad(1, 0x1234);
        r.latch_joypad_auto_read();
        // Latched mirrors live; reads expose them at $4218-$421B.
        assert_eq!(r.read(0x4218).unwrap(), 0x55);
        assert_eq!(r.read(0x4219).unwrap(), 0xAA);
        assert_eq!(r.read(0x421A).unwrap(), 0x34);
        assert_eq!(r.read(0x421B).unwrap(), 0x12);
        // HVBJOY.0 = busy raised.
        assert_eq!(r.hvbjoy & 0x01, 0x01);
    }

    #[test]
    fn joypad_latch_is_a_noop_when_nmitimen_0_clear() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x00); // bit 0 cleared
        r.set_joypad(0, 0xFFFF);
        r.latch_joypad_auto_read();
        // Latches stay at their default 0; reads see no buttons.
        assert_eq!(r.read(0x4218).unwrap(), 0x00);
        assert_eq!(r.read(0x4219).unwrap(), 0x00);
        assert_eq!(r.hvbjoy & 0x01, 0x00);
    }

    #[test]
    fn joypad_3_and_4_return_zero() {
        let mut r = CpuRegs::new();
        // No multitap modelled — $421C-$421F always read 0.
        for off in 0x421C..=0x421F {
            assert_eq!(r.read(off).unwrap(), 0x00);
        }
    }

    #[test]
    fn clear_joypad_busy_drops_hvbjoy_0_only() {
        let mut r = CpuRegs::new();
        r.hvbjoy = 0xFF;
        r.clear_joypad_busy();
        assert_eq!(r.hvbjoy, 0xFE, "only bit 0 should clear");
    }

    #[test]
    fn dpad_lockout_clears_opposing_directions() {
        // Bit layout per SNES JOY1L/JOY1H pair (high → low):
        //   B Y SEL START Up Down Left Right A X L R 0 0 0 0
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01); // auto-read enable
        // Up + Down + B held → keep B, drop both Up + Down.
        r.set_joypad(0, 0x8000 | 0x0800 | 0x0400);
        r.latch_joypad_auto_read();
        assert_eq!(r.joypad1_latched & 0x0C00, 0, "up + down lockout");
        assert_eq!(r.joypad1_latched & 0x8000, 0x8000, "B preserved");

        // Left + Right + Start held → keep Start, drop L + R.
        r.set_joypad(0, 0x1000 | 0x0200 | 0x0100);
        r.latch_joypad_auto_read();
        assert_eq!(r.joypad1_latched & 0x0300, 0, "left + right lockout");
        assert_eq!(r.joypad1_latched & 0x1000, 0x1000, "Start preserved");

        // Only Up held (no opposing) → passes through.
        r.set_joypad(0, 0x0800);
        r.latch_joypad_auto_read();
        assert_eq!(r.joypad1_latched, 0x0800);
    }

    #[test]
    fn joypad_bit_layout_byss_udlr_axlr() {
        // Compile-time sanity check that we agree with ares'
        // gamepad.cpp shift order. Test by name so a future
        // bit-flip will fire here loudly.
        let b = 0x8000u16;
        let y = 0x4000;
        let sel = 0x2000;
        let start = 0x1000;
        let up = 0x0800;
        let down = 0x0400;
        let left = 0x0200;
        let right = 0x0100;
        let a = 0x0080;
        let x = 0x0040;
        let l = 0x0020;
        let r_ = 0x0010;
        let all = b | y | sel | start | up | down | left | right | a | x | l | r_;
        // Bits 3..0 are the device signature — always zero for a
        // standard gamepad. So a "press everything" mask is
        // 0xFFF0.
        assert_eq!(all, 0xFFF0);
    }
}
