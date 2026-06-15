//! [`Spc700`] state — registers + fetch helpers + flag updates.

use crate::bus::SpcBus;
use crate::flags::{Psw, bit};
use serde::{Deserialize, Serialize};

/// SPC700 CPU state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Spc700 {
    /// Accumulator.
    pub a: u8,
    /// X index.
    pub x: u8,
    /// Y index.
    pub y: u8,
    /// Low byte of the stack pointer (stack lives at `$0100 + sp`).
    pub sp: u8,
    /// Program counter (16-bit).
    pub pc: u16,
    /// Program status word.
    pub psw: Psw,
    /// `true` after `STOP` until reset. The opcode dispatcher is
    /// exhaustive over all 256 opcodes, so this is the only stop cause.
    pub stopped: bool,
    /// `true` after `SLEEP` until an interrupt wakes us up.
    pub sleeping: bool,
    /// Set by a branch-family handler (BRA / Bcc / CBNE / DBNZ / BBS /
    /// BBC) when it takes the branch. `step` reads it to add the `+2`
    /// taken penalty, then clears it before the next instruction.
    pub branch_taken: bool,
}

impl Spc700 {
    /// Build a SPC700 with all registers zeroed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the CPU and load PC from the reset vector at `$FFFE/$FFFF`.
    ///
    /// On real hardware this points into the IPL ROM (which then
    /// handshakes with the main CPU). For unit tests, callers can
    /// poke their own vector before calling `reset`.
    pub fn reset<B: SpcBus>(&mut self, bus: &mut B) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xFF;
        self.psw = Psw::default();
        let lo = bus.read(0xFFFE);
        let hi = bus.read(0xFFFF);
        self.pc = u16::from(lo) | (u16::from(hi) << 8);
        self.stopped = false;
        self.sleeping = false;
    }

    // -----------------------------------------------------------------
    // Fetch helpers — read at PC and advance.
    // -----------------------------------------------------------------

    /// Read one byte at PC and advance PC by 1.
    #[inline]
    pub fn fetch_u8<B: SpcBus>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    /// Dummy read of the byte at PC **without** advancing — one bus
    /// cycle whose value is discarded. The SPC700 prefetches the byte
    /// after a 1-/short-operand opcode even when it isn't used; this is
    /// the per-cycle activity hardware (and Mesen2's `DummyRead`) shows
    /// for implied/register ops. Needed for cycle-faithful timer/DSP
    /// clocking, not for state.
    #[inline]
    pub fn dummy_read_pc<B: SpcBus>(&mut self, bus: &mut B) {
        let _ = bus.read(self.pc);
    }

    /// An internal idle cycle: no bus access, but the SPC still burns a
    /// cycle (clocking DSP + timers in the timing-accurate consumer).
    #[inline]
    pub fn idle<B: SpcBus>(bus: &mut B) {
        bus.idle();
    }

    /// Read a little-endian 16-bit value at PC and advance by 2.
    #[inline]
    pub fn fetch_u16<B: SpcBus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch_u8(bus);
        let hi = self.fetch_u8(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    // -----------------------------------------------------------------
    // Address helpers.
    // -----------------------------------------------------------------

    /// Translate a direct-page byte offset into a full 16-bit address.
    ///
    /// When `P=1`, the direct page lives at `$01xx`; when `P=0`, at
    /// `$00xx`.
    #[inline]
    #[must_use]
    pub fn direct_addr(&self, offset: u8) -> u16 {
        if self.psw.direct_page_high() {
            0x0100 | u16::from(offset)
        } else {
            u16::from(offset)
        }
    }

    // -----------------------------------------------------------------
    // Flag updates.
    // -----------------------------------------------------------------

    /// Set N and Z based on an 8-bit value.
    #[inline]
    pub const fn set_nz(&mut self, value: u8) {
        self.psw.set(bit::Z, value == 0);
        self.psw.set(bit::N, value & 0x80 != 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::RamBus;

    #[test]
    fn reset_loads_pc_from_fffe() {
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x34);
        bus.poke(0xFFFF, 0x12);
        cpu.reset(&mut bus);
        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.sp, 0xFF);
    }

    #[test]
    fn fetch_advances_pc() {
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        cpu.pc = 0x0200;
        bus.poke_slice(0x0200, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(cpu.fetch_u8(&mut bus), 0xAA);
        assert_eq!(cpu.fetch_u16(&mut bus), 0xCCBB);
        assert_eq!(cpu.pc, 0x0203);
    }

    #[test]
    fn direct_addr_follows_p_flag() {
        let mut cpu = Spc700::new();
        assert_eq!(cpu.direct_addr(0x42), 0x0042);
        cpu.psw.insert(bit::P);
        assert_eq!(cpu.direct_addr(0x42), 0x0142);
    }

    #[test]
    fn set_nz_8bit() {
        let mut cpu = Spc700::new();
        cpu.set_nz(0);
        assert!(cpu.psw.contains(bit::Z));
        cpu.set_nz(0x80);
        assert!(!cpu.psw.contains(bit::Z));
        assert!(cpu.psw.contains(bit::N));
    }
}
