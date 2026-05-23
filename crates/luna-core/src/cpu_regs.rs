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
            0x4200..=0x420F | 0x4213 | 0x4218..=0x421F => return None,
            _ => return None,
        };
        Some(v)
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
}
