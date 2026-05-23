//! [`Cpu`] state: registers, flags, reset, fetch helpers.

use crate::flags::{StatusFlags, bit};
use luna_bus::{Addr24, Bus, make_addr};

/// 65C816 CPU state.
///
/// Registers `A`, `X`, `Y` are stored as 16-bit values; when in 8-bit
/// width (`M = 1` for A, `X = 1` for index regs) only the low byte is
/// observable from the program. The high byte (`B`) of A is preserved
/// across width transitions.
#[derive(Debug, Clone)]
pub struct Cpu {
    /// Accumulator (16-bit; `M = 1` exposes only the low byte).
    pub a: u16,
    /// X index register (16-bit; `X = 1` exposes only the low byte).
    pub x: u16,
    /// Y index register.
    pub y: u16,
    /// Stack pointer.
    pub sp: u16,
    /// Direct page register.
    pub dp: u16,
    /// Program counter (within the program bank).
    pub pc: u16,
    /// Program bank register.
    pub pb: u8,
    /// Data bank register.
    pub db: u8,
    /// Status flags (`P`).
    pub p: StatusFlags,
    /// Emulation flag (hidden, not part of `P`).
    pub e: bool,
    /// Set by `STP`; CPU halts until reset.
    pub stopped: bool,
    /// Set by `WAI`; CPU pauses until an interrupt.
    pub waiting: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Build a CPU in its post-reset state (registers cleared, PC will be
    /// loaded from the reset vector on the next [`Cpu::reset`] call).
    #[must_use]
    pub fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0x01FF,
            dp: 0,
            pc: 0,
            pb: 0,
            db: 0,
            p: StatusFlags::RESET,
            e: true, // 65C816 boots in emulation mode.
            stopped: false,
            waiting: false,
        }
    }

    /// Perform a reset sequence: read the reset vector at `$00:FFFC` and
    /// load it into `PC`. Sets emulation mode, M/X/I, clears D, leaves
    /// other state (RAM contents) untouched.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) {
        self.e = true;
        self.p = StatusFlags::RESET;
        self.p.remove(bit::D);
        self.pb = 0;
        self.db = 0;
        self.dp = 0;
        self.sp = (self.sp & 0x00FF) | 0x0100; // SP high byte forced in E mode.
        // Reset vector at $00:FFFC / $00:FFFD.
        let lo = bus.read(make_addr(0, 0xFFFC));
        let hi = bus.read(make_addr(0, 0xFFFD));
        self.pc = u16::from(lo) | (u16::from(hi) << 8);
        self.stopped = false;
        self.waiting = false;
    }

    // -------------------------------------------------------------------
    // Fetch helpers — read bytes at PC and increment.
    // -------------------------------------------------------------------

    /// Read one byte at `PB:PC` and advance `PC`.
    #[inline]
    pub fn fetch_u8<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let addr = make_addr(self.pb, self.pc);
        let value = bus.read(addr);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    /// Read a little-endian `u16` at `PB:PC` and advance `PC` by 2.
    #[inline]
    pub fn fetch_u16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch_u8(bus);
        let hi = self.fetch_u8(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    /// Read a little-endian 24-bit value at `PB:PC` and advance `PC` by 3.
    #[inline]
    pub fn fetch_u24<B: Bus>(&mut self, bus: &mut B) -> Addr24 {
        let lo = self.fetch_u8(bus);
        let mid = self.fetch_u8(bus);
        let hi = self.fetch_u8(bus);
        Addr24::from(lo) | (Addr24::from(mid) << 8) | (Addr24::from(hi) << 16)
    }

    // -------------------------------------------------------------------
    // Flag-update helpers (used by many opcodes).
    // -------------------------------------------------------------------

    /// Update N and Z based on an 8-bit value.
    #[inline]
    pub fn set_nz8(&mut self, value: u8) {
        self.p.set(bit::Z, value == 0);
        self.p.set(bit::N, value & 0x80 != 0);
    }

    /// Update N and Z based on a 16-bit value.
    #[inline]
    pub fn set_nz16(&mut self, value: u16) {
        self.p.set(bit::Z, value == 0);
        self.p.set(bit::N, value & 0x8000 != 0);
    }

    /// 8-bit view of the accumulator.
    #[inline]
    #[must_use]
    pub fn a8(&self) -> u8 {
        self.a as u8
    }

    /// 8-bit view of X.
    #[inline]
    #[must_use]
    pub fn x8(&self) -> u8 {
        self.x as u8
    }

    /// 8-bit view of Y.
    #[inline]
    #[must_use]
    pub fn y8(&self) -> u8 {
        self.y as u8
    }

    /// Store the low byte of A while preserving the high byte (`B`).
    #[inline]
    pub fn set_a_low(&mut self, value: u8) {
        self.a = (self.a & 0xFF00) | u16::from(value);
    }

    /// Store the low byte of X. When `X = 1` (8-bit index), the high
    /// byte is forced to zero per 65C816 spec.
    #[inline]
    pub fn set_x_low(&mut self, value: u8) {
        if self.p.idx8() {
            self.x = u16::from(value);
        } else {
            self.x = (self.x & 0xFF00) | u16::from(value);
        }
    }

    /// Store the low byte of Y. Same caveat as [`Cpu::set_x_low`].
    #[inline]
    pub fn set_y_low(&mut self, value: u8) {
        if self.p.idx8() {
            self.y = u16::from(value);
        } else {
            self.y = (self.y & 0xFF00) | u16::from(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_bus::testing::RamBus;

    #[test]
    fn new_starts_in_emulation_mode() {
        let cpu = Cpu::new();
        assert!(cpu.e, "65C816 boots in emulation mode");
        assert!(cpu.p.contains(bit::M));
        assert!(cpu.p.contains(bit::X));
        assert!(cpu.p.contains(bit::I));
    }

    #[test]
    fn reset_loads_pc_from_vector() {
        let mut cpu = Cpu::new();
        let mut bus = RamBus::new();
        // Reset vector at $00:FFFC = $1234.
        bus.poke(0x00_FFFC, 0x34);
        bus.poke(0x00_FFFD, 0x12);
        cpu.reset(&mut bus);
        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.pb, 0);
        assert!(cpu.e);
    }

    #[test]
    fn fetch_u8_advances_pc() {
        let mut cpu = Cpu::new();
        let mut bus = RamBus::new();
        cpu.pc = 0x8000;
        bus.poke(0x00_8000, 0xAA);
        assert_eq!(cpu.fetch_u8(&mut bus), 0xAA);
        assert_eq!(cpu.pc, 0x8001);
    }

    #[test]
    fn fetch_u16_is_little_endian() {
        let mut cpu = Cpu::new();
        let mut bus = RamBus::new();
        cpu.pc = 0x8000;
        bus.poke_slice(0x00_8000, &[0x34, 0x12]);
        assert_eq!(cpu.fetch_u16(&mut bus), 0x1234);
        assert_eq!(cpu.pc, 0x8002);
    }

    #[test]
    fn fetch_u24_is_little_endian() {
        let mut cpu = Cpu::new();
        let mut bus = RamBus::new();
        cpu.pc = 0x8000;
        bus.poke_slice(0x00_8000, &[0x34, 0x12, 0x7E]);
        assert_eq!(cpu.fetch_u24(&mut bus), 0x7E_1234);
        assert_eq!(cpu.pc, 0x8003);
    }

    #[test]
    fn set_nz_8bit() {
        let mut cpu = Cpu::new();
        cpu.set_nz8(0);
        assert!(cpu.p.contains(bit::Z));
        assert!(!cpu.p.contains(bit::N));
        cpu.set_nz8(0x80);
        assert!(!cpu.p.contains(bit::Z));
        assert!(cpu.p.contains(bit::N));
        cpu.set_nz8(0x7F);
        assert!(!cpu.p.contains(bit::Z));
        assert!(!cpu.p.contains(bit::N));
    }

    #[test]
    fn set_nz_16bit() {
        let mut cpu = Cpu::new();
        cpu.set_nz16(0);
        assert!(cpu.p.contains(bit::Z));
        cpu.set_nz16(0x8000);
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn set_x_low_clears_high_byte_in_8bit_mode() {
        let mut cpu = Cpu::new();
        cpu.p.insert(bit::X);
        cpu.x = 0xABCD;
        cpu.set_x_low(0x12);
        assert_eq!(cpu.x, 0x0012);
    }

    #[test]
    fn set_x_low_preserves_high_byte_in_16bit_mode() {
        let mut cpu = Cpu::new();
        cpu.p.remove(bit::X);
        cpu.x = 0xABCD;
        cpu.set_x_low(0x12);
        assert_eq!(cpu.x, 0xAB12);
    }
}
