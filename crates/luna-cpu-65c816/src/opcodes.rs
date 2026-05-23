//! Opcode dispatch and a representative subset of instruction handlers.
//!
//! P0.4a covers loads / stores (LDA/STA in multiple modes), jumps and
//! conditional branches, mode-control opcodes (XCE / SEP / REP) and the
//! flag-toggle family. The remaining ~225 opcodes are stubbed with a
//! clear panic message so the dispatch table is complete and Tom Harte
//! tests can be wired up in P0.4b without further plumbing changes.

use crate::addressing::{absolute, absolute_long, direct_page, read_word};
use crate::cpu::Cpu;
use crate::flags::bit;
use luna_bus::Bus;

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
            0x18 => self.p.remove(bit::C),  // CLC
            0x38 => self.p.insert(bit::C),  // SEC
            0x58 => self.p.remove(bit::I),  // CLI
            0x78 => self.p.insert(bit::I),  // SEI
            0xB8 => self.p.remove(bit::V),  // CLV
            0xD8 => self.p.remove(bit::D),  // CLD
            0xF8 => self.p.insert(bit::D),  // SED

            // -----------------------------------------------------------
            // Loads (LDA)
            // -----------------------------------------------------------
            0xA9 => self.lda_imm(bus),
            0xA5 => self.lda_dp(bus),
            0xAD => self.lda_abs(bus),
            0xAF => self.lda_long(bus),

            // -----------------------------------------------------------
            // Stores (STA)
            // -----------------------------------------------------------
            0x85 => self.sta_dp(bus),
            0x8D => self.sta_abs(bus),
            0x8F => self.sta_long(bus),

            // -----------------------------------------------------------
            // Jumps
            // -----------------------------------------------------------
            0x4C => self.jmp_abs(bus),
            0x5C => self.jmp_long(bus),

            // -----------------------------------------------------------
            // Branches (8-bit signed PC-relative)
            // -----------------------------------------------------------
            0x80 => self.branch_if(bus, true),                              // BRA
            0x10 => self.branch_if(bus, !self.p.contains(bit::N)),          // BPL
            0x30 => self.branch_if(bus, self.p.contains(bit::N)),           // BMI
            0x50 => self.branch_if(bus, !self.p.contains(bit::V)),          // BVC
            0x70 => self.branch_if(bus, self.p.contains(bit::V)),           // BVS
            0x90 => self.branch_if(bus, !self.p.contains(bit::C)),          // BCC
            0xB0 => self.branch_if(bus, self.p.contains(bit::C)),           // BCS
            0xD0 => self.branch_if(bus, !self.p.contains(bit::Z)),          // BNE
            0xF0 => self.branch_if(bus, self.p.contains(bit::Z)),           // BEQ

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
    fn sta_long_16bit_writes_two_bytes() {
        // CLC, XCE, REP #$20, LDA #$ABCD, STA $7E1234
        let prog = &[0x18, 0xFB, 0xC2, 0x20, 0xA9, 0xCD, 0xAB, 0x8F, 0x34, 0x12, 0x7E];
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
