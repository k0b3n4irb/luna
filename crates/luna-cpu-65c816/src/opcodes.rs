//! Opcode dispatch and a representative subset of instruction handlers.
//!
//! P0.4a covers loads / stores (LDA/STA in multiple modes), jumps and
//! conditional branches, mode-control opcodes (XCE / SEP / REP) and the
//! flag-toggle family. The remaining ~225 opcodes are stubbed with a
//! clear panic message so the dispatch table is complete and Tom Harte
//! tests can be wired up in P0.4b without further plumbing changes.

use crate::addressing::{
    absolute, absolute_indexed_x, absolute_indexed_y, absolute_long, absolute_long_indexed_x,
    direct_page, direct_page_indexed_indirect, direct_page_indexed_x, direct_page_indexed_y,
    direct_page_indirect, direct_page_indirect_long, direct_page_indirect_long_y,
    direct_page_indirect_y, read_word, stack_relative, stack_relative_indirect_y,
};
use crate::cpu::Cpu;
use crate::flags::bit;
use luna_bus::{Addr24, Bus};

impl Cpu {
    /// Execute one instruction: fetch opcode at `PB:PC` and dispatch.
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.stopped || self.waiting {
            // Spin doing nothing; in production, the bus should signal
            // an interrupt and the wrapper would call clear_wai/clear_stp.
            return;
        }
        let opcode = self.fetch_u8(bus);
        self.execute(opcode, bus);
    }

    /// Dispatch on a fetched opcode. Inlined into the match by LLVM.
    #[allow(clippy::too_many_lines)]
    fn execute<B: Bus>(&mut self, opcode: u8, bus: &mut B) {
        match opcode {
            // -----------------------------------------------------------
            // Mode control
            // -----------------------------------------------------------
            0xFB => self.xce(),
            0xE2 => self.sep(bus),
            0xC2 => self.rep(bus),

            // -----------------------------------------------------------
            // Flag toggles
            // -----------------------------------------------------------
            0x18 => self.p.remove(bit::C), // CLC
            0x38 => self.p.insert(bit::C), // SEC
            0x58 => self.p.remove(bit::I), // CLI
            0x78 => self.p.insert(bit::I), // SEI
            0xB8 => self.p.remove(bit::V), // CLV
            0xD8 => self.p.remove(bit::D), // CLD
            0xF8 => self.p.insert(bit::D), // SED

            // -----------------------------------------------------------
            // Loads (LDA — accumulator-width)
            // -----------------------------------------------------------
            0xA9 => self.lda_imm(bus),
            0xA5 => self.lda_dp(bus),
            0xA7 => self.lda_dp_indirect_long(bus),
            0xAD => self.lda_abs(bus),
            0xAF => self.lda_long(bus),
            0xB5 => self.lda_dp_x(bus),
            0xB2 => self.lda_dp_indirect(bus),
            0xB7 => self.lda_dp_indirect_long_y(bus),
            0xBD => self.lda_abs_x(bus),
            0xBF => self.lda_long_x(bus),
            0xB9 => self.lda_abs_y(bus),
            0xB1 => self.lda_dp_indirect_y(bus),
            0xA3 => self.lda_sr_s(bus),
            0xB3 => self.lda_sr_s_y(bus),
            0xA1 => self.lda_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Loads (LDX, LDY — index-register width)
            // -----------------------------------------------------------
            0xA2 => self.ldx_imm(bus),
            0xA6 => self.ldx_dp(bus),
            0xAE => self.ldx_abs(bus),
            0xB6 => self.ldx_dp_y(bus),
            0xBE => self.ldx_abs_y(bus),
            0xA0 => self.ldy_imm(bus),
            0xA4 => self.ldy_dp(bus),
            0xAC => self.ldy_abs(bus),
            0xB4 => self.ldy_dp_x(bus),
            0xBC => self.ldy_abs_x(bus),

            // -----------------------------------------------------------
            // Stores (STA — accumulator-width)
            // -----------------------------------------------------------
            0x85 => self.sta_dp(bus),
            0x87 => self.sta_dp_indirect_long(bus),
            0x8D => self.sta_abs(bus),
            0x8F => self.sta_long(bus),
            0x95 => self.sta_dp_x(bus),
            0x92 => self.sta_dp_indirect(bus),
            0x97 => self.sta_dp_indirect_long_y(bus),
            0x9D => self.sta_abs_x(bus),
            0x9F => self.sta_long_x(bus),
            0x99 => self.sta_abs_y(bus),
            0x91 => self.sta_dp_indirect_y(bus),
            0x83 => self.sta_sr_s(bus),
            0x93 => self.sta_sr_s_y(bus),
            0x81 => self.sta_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Stores (STX, STY — index-register width)
            // -----------------------------------------------------------
            0x86 => self.stx_dp(bus),
            0x8E => self.stx_abs(bus),
            0x96 => self.stx_dp_y(bus),
            0x84 => self.sty_dp(bus),
            0x8C => self.sty_abs(bus),
            0x94 => self.sty_dp_x(bus),

            // -----------------------------------------------------------
            // Store-zero (STZ — accumulator-width zero write)
            // -----------------------------------------------------------
            0x64 => self.stz_dp(bus),
            0x74 => self.stz_dp_x(bus),
            0x9C => self.stz_abs(bus),
            0x9E => self.stz_abs_x(bus),

            // -----------------------------------------------------------
            // Jumps
            // -----------------------------------------------------------
            0x4C => self.jmp_abs(bus),
            0x5C => self.jmp_long(bus),

            // -----------------------------------------------------------
            // Branches (8-bit signed PC-relative)
            // -----------------------------------------------------------
            0x80 => self.branch_if(bus, true), // BRA
            0x10 => self.branch_if(bus, !self.p.contains(bit::N)), // BPL
            0x30 => self.branch_if(bus, self.p.contains(bit::N)), // BMI
            0x50 => self.branch_if(bus, !self.p.contains(bit::V)), // BVC
            0x70 => self.branch_if(bus, self.p.contains(bit::V)), // BVS
            0x90 => self.branch_if(bus, !self.p.contains(bit::C)), // BCC
            0xB0 => self.branch_if(bus, self.p.contains(bit::C)), // BCS
            0xD0 => self.branch_if(bus, !self.p.contains(bit::Z)), // BNE
            0xF0 => self.branch_if(bus, self.p.contains(bit::Z)), // BEQ

            // -----------------------------------------------------------
            // Increment / decrement A
            // -----------------------------------------------------------
            0x1A => self.inc_a(),
            0x3A => self.dec_a(),

            // -----------------------------------------------------------
            // Misc
            // -----------------------------------------------------------
            0xEA => { /* NOP */ }
            0xCB => self.waiting = true, // WAI
            0xDB => self.stopped = true, // STP

            // Everything else: P0.4b territory.
            other => panic!(
                "luna-cpu-65c816: opcode 0x{other:02X} not yet implemented \
                 (PB:PC=${:02X}:{:04X})",
                self.pb,
                self.pc.wrapping_sub(1),
            ),
        }
    }

    // ===================================================================
    // Mode control
    // ===================================================================

    /// `XCE` — exchange C and E flags. The canonical way to switch the
    /// CPU between emulation (E=1) and native (E=0) mode.
    fn xce(&mut self) {
        let c = self.p.contains(bit::C);
        let e = self.e;
        self.p.set(bit::C, e);
        self.e = c;
        if self.e {
            // Entering emulation mode forces M and X to 1 and resets the
            // high byte of SP.
            self.p.insert(bit::M);
            self.p.insert(bit::X);
            self.sp = (self.sp & 0x00FF) | 0x0100;
            self.x &= 0x00FF;
            self.y &= 0x00FF;
        }
    }

    /// `SEP #imm` — set bits in P. In emulation mode, the M and X bits
    /// cannot be cleared, but `SEP` only sets bits, so no special-case
    /// needed beyond the index-width truncation.
    fn sep<B: Bus>(&mut self, bus: &mut B) {
        let mask = self.fetch_u8(bus);
        self.p.insert(mask);
        if self.p.idx8() {
            self.x &= 0x00FF;
            self.y &= 0x00FF;
        }
    }

    /// `REP #imm` — reset (clear) bits in P. In emulation mode, M and X
    /// are forced to 1 and cannot be cleared by REP.
    fn rep<B: Bus>(&mut self, bus: &mut B) {
        let mask = self.fetch_u8(bus);
        let mut effective = mask;
        if self.e {
            effective &= !(bit::M | bit::X);
        }
        self.p.remove(effective);
    }

    // ===================================================================
    // Loads (LDA)
    // ===================================================================

    fn lda_imm<B: Bus>(&mut self, bus: &mut B) {
        if self.p.acc8() {
            let v = self.fetch_u8(bus);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            let v = self.fetch_u16(bus);
            self.a = v;
            self.set_nz16(v);
        }
    }

    fn lda_from_addr<B: Bus>(&mut self, bus: &mut B, addr: luna_bus::Addr24) {
        if self.p.acc8() {
            let v = bus.read(addr);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            let v = read_word(bus, addr);
            self.a = v;
            self.set_nz16(v);
        }
    }

    fn lda_dp<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_abs<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_long<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_long(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_x(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect_long(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect_y(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect_long_y(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_indirect(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_indexed_x(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_indexed_y(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_long_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_long_indexed_x(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let addr = stack_relative(self, bus);
        self.lda_from_addr(bus, addr);
    }

    fn lda_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = stack_relative_indirect_y(self, bus);
        self.lda_from_addr(bus, addr);
    }

    // ===================================================================
    // Loads (LDX, LDY — width gated by the X flag, NOT M)
    // ===================================================================

    fn ldx_imm<B: Bus>(&mut self, bus: &mut B) {
        if self.p.idx8() {
            let v = self.fetch_u8(bus);
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            let v = self.fetch_u16(bus);
            self.x = v;
            self.set_nz16(v);
        }
    }

    fn ldx_from_addr<B: Bus>(&mut self, bus: &mut B, addr: Addr24) {
        if self.p.idx8() {
            let v = bus.read(addr);
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            let v = read_word(bus, addr);
            self.x = v;
            self.set_nz16(v);
        }
    }

    fn ldx_dp<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page(self, bus);
        self.ldx_from_addr(bus, addr);
    }

    fn ldx_abs<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute(self, bus);
        self.ldx_from_addr(bus, addr);
    }

    fn ldx_dp_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_y(self, bus);
        self.ldx_from_addr(bus, addr);
    }

    fn ldx_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_indexed_y(self, bus);
        self.ldx_from_addr(bus, addr);
    }

    fn ldy_imm<B: Bus>(&mut self, bus: &mut B) {
        if self.p.idx8() {
            let v = self.fetch_u8(bus);
            self.set_y_low(v);
            self.set_nz8(v);
        } else {
            let v = self.fetch_u16(bus);
            self.y = v;
            self.set_nz16(v);
        }
    }

    fn ldy_from_addr<B: Bus>(&mut self, bus: &mut B, addr: Addr24) {
        if self.p.idx8() {
            let v = bus.read(addr);
            self.set_y_low(v);
            self.set_nz8(v);
        } else {
            let v = read_word(bus, addr);
            self.y = v;
            self.set_nz16(v);
        }
    }

    fn ldy_dp<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page(self, bus);
        self.ldy_from_addr(bus, addr);
    }

    fn ldy_abs<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute(self, bus);
        self.ldy_from_addr(bus, addr);
    }

    fn ldy_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_x(self, bus);
        self.ldy_from_addr(bus, addr);
    }

    fn ldy_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_indexed_x(self, bus);
        self.ldy_from_addr(bus, addr);
    }

    // ===================================================================
    // Stores (STA)
    // ===================================================================

    fn sta_to_addr<B: Bus>(&mut self, bus: &mut B, addr: luna_bus::Addr24) {
        if self.p.acc8() {
            bus.write(addr, self.a8());
        } else {
            bus.write(addr, self.a as u8);
            bus.write(addr.wrapping_add(1), (self.a >> 8) as u8);
        }
    }

    fn sta_dp<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_abs<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_long<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_long(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_x(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect_long(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect_y(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indirect_long_y(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_indirect(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_indexed_x(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_indexed_y(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_long_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_long_indexed_x(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let addr = stack_relative(self, bus);
        self.sta_to_addr(bus, addr);
    }

    fn sta_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = stack_relative_indirect_y(self, bus);
        self.sta_to_addr(bus, addr);
    }

    // ===================================================================
    // Stores (STX, STY — width gated by the X flag)
    // ===================================================================

    fn stx_to_addr<B: Bus>(&mut self, bus: &mut B, addr: Addr24) {
        if self.p.idx8() {
            bus.write(addr, self.x8());
        } else {
            bus.write(addr, self.x as u8);
            bus.write(addr.wrapping_add(1), (self.x >> 8) as u8);
        }
    }

    fn sty_to_addr<B: Bus>(&mut self, bus: &mut B, addr: Addr24) {
        if self.p.idx8() {
            bus.write(addr, self.y8());
        } else {
            bus.write(addr, self.y as u8);
            bus.write(addr.wrapping_add(1), (self.y >> 8) as u8);
        }
    }

    fn stx_dp<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page(self, bus);
        self.stx_to_addr(bus, addr);
    }

    fn stx_abs<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute(self, bus);
        self.stx_to_addr(bus, addr);
    }

    fn stx_dp_y<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_y(self, bus);
        self.stx_to_addr(bus, addr);
    }

    fn sty_dp<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page(self, bus);
        self.sty_to_addr(bus, addr);
    }

    fn sty_abs<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute(self, bus);
        self.sty_to_addr(bus, addr);
    }

    fn sty_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_x(self, bus);
        self.sty_to_addr(bus, addr);
    }

    // ===================================================================
    // Store-zero (width gated by the M flag, like STA)
    // ===================================================================

    fn stz_to_addr<B: Bus>(&mut self, bus: &mut B, addr: Addr24) {
        bus.write(addr, 0);
        if !self.p.acc8() {
            bus.write(addr.wrapping_add(1), 0);
        }
    }

    fn stz_dp<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page(self, bus);
        self.stz_to_addr(bus, addr);
    }

    fn stz_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = direct_page_indexed_x(self, bus);
        self.stz_to_addr(bus, addr);
    }

    fn stz_abs<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute(self, bus);
        self.stz_to_addr(bus, addr);
    }

    fn stz_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let addr = absolute_indexed_x(self, bus);
        self.stz_to_addr(bus, addr);
    }

    // ===================================================================
    // Jumps
    // ===================================================================

    fn jmp_abs<B: Bus>(&mut self, bus: &mut B) {
        let target = self.fetch_u16(bus);
        self.pc = target;
    }

    fn jmp_long<B: Bus>(&mut self, bus: &mut B) {
        let target = self.fetch_u24(bus);
        self.pc = target as u16;
        self.pb = (target >> 16) as u8;
    }

    // ===================================================================
    // Branches
    // ===================================================================

    fn branch_if<B: Bus>(&mut self, bus: &mut B, condition: bool) {
        let offset = self.fetch_u8(bus) as i8;
        if condition {
            self.pc = self.pc.wrapping_add_signed(i16::from(offset));
        }
    }

    // ===================================================================
    // Increment / decrement A
    // ===================================================================

    fn inc_a(&mut self) {
        if self.p.acc8() {
            let v = self.a8().wrapping_add(1);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            self.a = self.a.wrapping_add(1);
            self.set_nz16(self.a);
        }
    }

    fn dec_a(&mut self) {
        if self.p.acc8() {
            let v = self.a8().wrapping_sub(1);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            self.a = self.a.wrapping_sub(1);
            self.set_nz16(self.a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_bus::testing::RamBus;

    /// Build a CPU sitting at `$00:8000`, fed by the given program bytes.
    fn run(program: &[u8]) -> (Cpu, RamBus) {
        let mut cpu = Cpu::new();
        let mut bus = RamBus::new();
        // Reset vector → $8000.
        bus.poke(0x00_FFFC, 0x00);
        bus.poke(0x00_FFFD, 0x80);
        bus.poke_slice(0x00_8000, program);
        cpu.reset(&mut bus);
        bus.reset_cycle_counter();
        (cpu, bus)
    }

    // -------------------------------------------------------------------
    // Flag toggles
    // -------------------------------------------------------------------

    #[test]
    fn sec_clc_round_trip() {
        let (mut cpu, mut bus) = run(&[0x38, 0x18]); // SEC, CLC
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::C));
        cpu.step(&mut bus);
        assert!(!cpu.p.contains(bit::C));
    }

    #[test]
    fn sei_cli_toggles_irq_disable() {
        let (mut cpu, mut bus) = run(&[0x58, 0x78]); // CLI, SEI
        cpu.step(&mut bus);
        assert!(!cpu.p.contains(bit::I));
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::I));
    }

    // -------------------------------------------------------------------
    // Mode switching
    // -------------------------------------------------------------------

    #[test]
    fn xce_switches_to_native_mode() {
        // CLC then XCE → C was 0 → E becomes 0 (native).
        let (mut cpu, mut bus) = run(&[0x18, 0xFB]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE
        assert!(!cpu.e, "should be native after XCE with C=0");
    }

    #[test]
    fn xce_native_to_native_via_rep() {
        // SEC, XCE  → emulation (E=1)
        // CLC, XCE  → native    (E=0)
        // REP #$30  → clear M and X
        let (mut cpu, mut bus) = run(&[0x38, 0xFB, 0x18, 0xFB, 0xC2, 0x30]);
        for _ in 0..4 {
            cpu.step(&mut bus);
        }
        assert!(!cpu.e);
        cpu.step(&mut bus); // REP #$30
        assert!(!cpu.p.contains(bit::M));
        assert!(!cpu.p.contains(bit::X));
    }

    #[test]
    fn rep_does_not_clear_m_x_in_emulation_mode() {
        // Boot in emulation mode (E=1). Try REP #$30. M and X must stay set.
        let (mut cpu, mut bus) = run(&[0xC2, 0x30]);
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::M));
        assert!(cpu.p.contains(bit::X));
    }

    // -------------------------------------------------------------------
    // Loads
    // -------------------------------------------------------------------

    #[test]
    fn lda_imm_8bit() {
        // LDA #$42 → A.low = $42, N=0 Z=0
        let (mut cpu, mut bus) = run(&[0xA9, 0x42]);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x42);
        assert!(!cpu.p.contains(bit::Z));
        assert!(!cpu.p.contains(bit::N));
    }

    #[test]
    fn lda_imm_8bit_sets_zero() {
        let (mut cpu, mut bus) = run(&[0xA9, 0x00]);
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::Z));
    }

    #[test]
    fn lda_imm_8bit_sets_negative() {
        let (mut cpu, mut bus) = run(&[0xA9, 0x80]);
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn lda_imm_16bit() {
        // Switch to native + clear M, then LDA #$ABCD.
        let (mut cpu, mut bus) = run(&[0x18, 0xFB, 0xC2, 0x20, 0xA9, 0xCD, 0xAB]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE → native
        cpu.step(&mut bus); // REP #$20 → M cleared
        cpu.step(&mut bus); // LDA #$ABCD (16-bit)
        assert_eq!(cpu.a, 0xABCD);
    }

    #[test]
    fn lda_abs() {
        let (mut cpu, mut bus) = run(&[0xAD, 0x00, 0x20]); // LDA $2000
        bus.poke(0x00_2000, 0x55);
        cpu.db = 0;
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x55);
    }

    #[test]
    fn lda_long() {
        let (mut cpu, mut bus) = run(&[0xAF, 0x34, 0x12, 0x7E]); // LDA $7E1234
        bus.poke(0x7E_1234, 0xAB);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0xAB);
    }

    #[test]
    fn lda_dp() {
        let (mut cpu, mut bus) = run(&[0xA5, 0x10]); // LDA $10
        cpu.dp = 0x0100;
        bus.poke(0x00_0110, 0x99);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x99);
    }

    #[test]
    fn lda_dp_x() {
        // LDA $10,X with X=4, DP=0x100 → reads $0114
        let (mut cpu, mut bus) = run(&[0xB5, 0x10]);
        cpu.dp = 0x0100;
        cpu.x = 0x04;
        bus.poke(0x00_0114, 0x66);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x66);
    }

    #[test]
    fn lda_dp_indirect() {
        // LDA ($10) with DP=0x100, pointer at 0x110 = $2000, DB=$7E → $7E:2000
        let (mut cpu, mut bus) = run(&[0xB2, 0x10]);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        bus.poke_slice(0x00_0110, &[0x00, 0x20]);
        bus.poke(0x7E_2000, 0x77);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x77);
    }

    #[test]
    fn lda_dp_indirect_long() {
        // LDA [$10] reads a 24-bit pointer
        let (mut cpu, mut bus) = run(&[0xA7, 0x10]);
        cpu.dp = 0x0100;
        bus.poke_slice(0x00_0110, &[0x34, 0x12, 0x7E]);
        bus.poke(0x7E_1234, 0x88);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x88);
    }

    #[test]
    fn lda_dp_indirect_y() {
        // LDA ($10),Y with Y=5
        let (mut cpu, mut bus) = run(&[0xB1, 0x10]);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        cpu.y = 0x05;
        bus.poke_slice(0x00_0110, &[0x00, 0x20]); // pointer = $2000
        bus.poke(0x7E_2005, 0xBE);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0xBE);
    }

    #[test]
    fn lda_abs_x() {
        // LDA $1000,X
        let (mut cpu, mut bus) = run(&[0xBD, 0x00, 0x10]);
        cpu.db = 0;
        cpu.x = 0x04;
        bus.poke(0x00_1004, 0xC0);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0xC0);
    }

    #[test]
    fn lda_abs_y() {
        let (mut cpu, mut bus) = run(&[0xB9, 0x00, 0x10]); // LDA $1000,Y
        cpu.db = 0;
        cpu.y = 0x10;
        bus.poke(0x00_1010, 0xD1);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0xD1);
    }

    #[test]
    fn lda_long_x() {
        // LDA $7E1000,X with X=4
        let (mut cpu, mut bus) = run(&[0xBF, 0x00, 0x10, 0x7E]);
        cpu.x = 0x04;
        bus.poke(0x7E_1004, 0xE2);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0xE2);
    }

    #[test]
    fn lda_sr_s() {
        // LDA $04,S — reads $00:SP+4
        let (mut cpu, mut bus) = run(&[0xA3, 0x04]);
        cpu.sp = 0x01F0;
        bus.poke(0x00_01F4, 0x42);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x42);
    }

    #[test]
    fn ldx_imm_8bit() {
        let (mut cpu, mut bus) = run(&[0xA2, 0x55]);
        cpu.step(&mut bus);
        assert_eq!(cpu.x8(), 0x55);
    }

    #[test]
    fn ldx_imm_16bit_with_x_cleared() {
        // CLC, XCE, REP #$10, LDX #$ABCD
        let (mut cpu, mut bus) = run(&[0x18, 0xFB, 0xC2, 0x10, 0xA2, 0xCD, 0xAB]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE → native
        cpu.step(&mut bus); // REP #$10 → X cleared
        cpu.step(&mut bus); // LDX #$ABCD (16-bit)
        assert_eq!(cpu.x, 0xABCD);
    }

    #[test]
    fn ldx_abs() {
        let (mut cpu, mut bus) = run(&[0xAE, 0x00, 0x20]); // LDX $2000
        cpu.db = 0;
        bus.poke(0x00_2000, 0x33);
        cpu.step(&mut bus);
        assert_eq!(cpu.x8(), 0x33);
    }

    #[test]
    fn ldx_dp_y() {
        let (mut cpu, mut bus) = run(&[0xB6, 0x10]); // LDX $10,Y
        cpu.dp = 0x0100;
        cpu.y = 0x04;
        bus.poke(0x00_0114, 0x77);
        cpu.step(&mut bus);
        assert_eq!(cpu.x8(), 0x77);
    }

    #[test]
    fn ldy_imm_8bit() {
        let (mut cpu, mut bus) = run(&[0xA0, 0x99]);
        cpu.step(&mut bus);
        assert_eq!(cpu.y8(), 0x99);
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn ldy_abs() {
        let (mut cpu, mut bus) = run(&[0xAC, 0x00, 0x20]); // LDY $2000
        cpu.db = 0;
        bus.poke(0x00_2000, 0x44);
        cpu.step(&mut bus);
        assert_eq!(cpu.y8(), 0x44);
    }

    #[test]
    fn stx_abs_writes_x() {
        // LDX #$77, STX $2000
        let (mut cpu, mut bus) = run(&[0xA2, 0x77, 0x8E, 0x00, 0x20]);
        cpu.db = 0;
        cpu.step(&mut bus); // LDX
        cpu.step(&mut bus); // STX $2000
        assert_eq!(bus.peek(0x00_2000), 0x77);
    }

    #[test]
    fn sty_dp_writes_y() {
        let (mut cpu, mut bus) = run(&[0xA0, 0x88, 0x84, 0x10]);
        cpu.dp = 0x0100;
        cpu.step(&mut bus); // LDY #$88
        cpu.step(&mut bus); // STY $10 → $0110
        assert_eq!(bus.peek(0x00_0110), 0x88);
    }

    #[test]
    fn stz_abs_writes_zero() {
        // Pre-fill memory then STZ over it.
        let (mut cpu, mut bus) = run(&[0x9C, 0x00, 0x20]); // STZ $2000
        cpu.db = 0;
        bus.poke(0x00_2000, 0xFF);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x00_2000), 0x00);
    }

    #[test]
    fn stz_abs_16bit_clears_two_bytes() {
        // CLC, XCE, REP #$20, STZ $2000
        let (mut cpu, mut bus) = run(&[0x18, 0xFB, 0xC2, 0x20, 0x9C, 0x00, 0x20]);
        cpu.db = 0;
        bus.poke_slice(0x00_2000, &[0xAA, 0xBB]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE
        cpu.step(&mut bus); // REP #$20
        cpu.step(&mut bus); // STZ $2000
        assert_eq!(bus.peek(0x00_2000), 0x00);
        assert_eq!(bus.peek(0x00_2001), 0x00);
    }

    #[test]
    fn ldy_dp_x() {
        let (mut cpu, mut bus) = run(&[0xB4, 0x10]); // LDY $10,X
        cpu.dp = 0x0100;
        cpu.x = 0x04;
        bus.poke(0x00_0114, 0xCC);
        cpu.step(&mut bus);
        assert_eq!(cpu.y8(), 0xCC);
    }

    // -------------------------------------------------------------------
    // Stores
    // -------------------------------------------------------------------

    #[test]
    fn sta_abs_8bit() {
        // LDA #$42, STA $2000
        let (mut cpu, mut bus) = run(&[0xA9, 0x42, 0x8D, 0x00, 0x20]);
        cpu.db = 0;
        cpu.step(&mut bus); // LDA #$42
        cpu.step(&mut bus); // STA $2000
        assert_eq!(bus.peek(0x00_2000), 0x42);
    }

    #[test]
    fn sta_dp_x_writes_at_indexed_dp() {
        // LDA #$42, STA $10,X
        let (mut cpu, mut bus) = run(&[0xA9, 0x42, 0x95, 0x10]);
        cpu.dp = 0x0100;
        cpu.x = 0x04;
        cpu.step(&mut bus); // LDA #$42
        cpu.step(&mut bus); // STA $10,X → $0114
        assert_eq!(bus.peek(0x00_0114), 0x42);
    }

    #[test]
    fn sta_dp_indirect_writes_through_pointer() {
        // LDA #$55, STA ($10)
        let (mut cpu, mut bus) = run(&[0xA9, 0x55, 0x92, 0x10]);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        bus.poke_slice(0x00_0110, &[0x00, 0x20]); // pointer = $2000
        cpu.step(&mut bus); // LDA
        cpu.step(&mut bus); // STA ($10) → $7E:2000
        assert_eq!(bus.peek(0x7E_2000), 0x55);
    }

    #[test]
    fn sta_dp_indirect_y_writes_with_y_offset() {
        let (mut cpu, mut bus) = run(&[0xA9, 0x66, 0x91, 0x10]);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        cpu.y = 0x05;
        bus.poke_slice(0x00_0110, &[0x00, 0x20]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x7E_2005), 0x66);
    }

    #[test]
    fn sta_long_16bit_writes_two_bytes() {
        // CLC, XCE, REP #$20, LDA #$ABCD, STA $7E1234
        let prog = &[
            0x18, 0xFB, 0xC2, 0x20, 0xA9, 0xCD, 0xAB, 0x8F, 0x34, 0x12, 0x7E,
        ];
        let (mut cpu, mut bus) = run(prog);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE → native
        cpu.step(&mut bus); // REP #$20
        cpu.step(&mut bus); // LDA #$ABCD
        cpu.step(&mut bus); // STA $7E1234
        assert_eq!(bus.peek(0x7E_1234), 0xCD);
        assert_eq!(bus.peek(0x7E_1235), 0xAB);
    }

    // -------------------------------------------------------------------
    // Jumps & branches
    // -------------------------------------------------------------------

    #[test]
    fn jmp_abs_redirects_pc_in_same_bank() {
        let (mut cpu, mut bus) = run(&[0x4C, 0x00, 0x90]); // JMP $9000
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(cpu.pb, 0);
    }

    #[test]
    fn jmp_long_changes_bank() {
        let (mut cpu, mut bus) = run(&[0x5C, 0x00, 0x90, 0x80]); // JMP $80:9000
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(cpu.pb, 0x80);
    }

    #[test]
    fn bne_taken_when_z_clear() {
        // LDA #$01 (Z=0), BNE +2, LDA #$AA (skipped), LDA #$BB
        let (mut cpu, mut bus) = run(&[0xA9, 0x01, 0xD0, 0x02, 0xA9, 0xAA, 0xA9, 0xBB]);
        cpu.step(&mut bus); // LDA #$01
        cpu.step(&mut bus); // BNE +2 → skip LDA #$AA
        cpu.step(&mut bus); // LDA #$BB
        assert_eq!(cpu.a8(), 0xBB);
    }

    #[test]
    fn beq_not_taken_when_z_clear() {
        // LDA #$01 (Z=0), BEQ +2, LDA #$AA, LDA #$BB
        let (mut cpu, mut bus) = run(&[0xA9, 0x01, 0xF0, 0x02, 0xA9, 0xAA, 0xA9, 0xBB]);
        cpu.step(&mut bus); // LDA #$01
        cpu.step(&mut bus); // BEQ +2 → not taken
        cpu.step(&mut bus); // LDA #$AA
        assert_eq!(cpu.a8(), 0xAA);
    }

    #[test]
    fn bra_always_taken() {
        // BRA -2 (back to itself), STP fallback so the test can detect
        // infinite-loop misuse — we step exactly once.
        let (mut cpu, mut bus) = run(&[0x80, 0xFE]);
        let start_pc = cpu.pc;
        cpu.step(&mut bus); // BRA -2
        assert_eq!(cpu.pc, start_pc, "BRA -2 should land back at the start");
    }

    // -------------------------------------------------------------------
    // INC / DEC A
    // -------------------------------------------------------------------

    #[test]
    fn inc_a_8bit_wraps_to_zero() {
        let (mut cpu, mut bus) = run(&[0xA9, 0xFF, 0x1A]); // LDA #$FF, INC A
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x00);
        assert!(cpu.p.contains(bit::Z));
    }

    #[test]
    fn dec_a_8bit_sets_negative() {
        let (mut cpu, mut bus) = run(&[0xA9, 0x00, 0x3A]); // LDA #$00, DEC A
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0xFF);
        assert!(cpu.p.contains(bit::N));
    }

    // -------------------------------------------------------------------
    // Misc
    // -------------------------------------------------------------------

    #[test]
    fn nop_just_advances_pc() {
        let (mut cpu, mut bus) = run(&[0xEA]);
        let pc_before = cpu.pc;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, pc_before.wrapping_add(1));
    }

    #[test]
    fn wai_pauses_until_cleared() {
        let (mut cpu, mut bus) = run(&[0xCB]);
        cpu.step(&mut bus); // WAI
        assert!(cpu.waiting);
        let pc_after_wai = cpu.pc;
        cpu.step(&mut bus); // should be a no-op while waiting
        assert_eq!(cpu.pc, pc_after_wai);
    }

    #[test]
    fn stp_halts() {
        let (mut cpu, mut bus) = run(&[0xDB]);
        cpu.step(&mut bus);
        assert!(cpu.stopped);
    }
}
