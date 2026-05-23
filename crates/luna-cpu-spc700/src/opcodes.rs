//! SPC700 opcode dispatch + first batch of instruction handlers (TDD).
//!
//! Scope of this file (P2.SPC.1):
//! - dispatch infrastructure (match-based, LLVM lowers to jump-table)
//! - flag-toggle family (CLRP / SETP / CLRC / SETC / EI / DI / CLRV / NOTC)
//! - immediate MOVs for A / X / Y
//! - direct-page and absolute MOV A,!abs / MOV !abs,A round-trips
//! - the 8 conditional + unconditional branches
//! - NOP / SLEEP / STOP
//!
//! The remaining ~225 opcodes land in subsequent batches with the
//! same TDD pattern as the 65C816 work.

use crate::bus::SpcBus;
use crate::cpu::Spc700;
use crate::flags::bit;

impl Spc700 {
    /// Execute one instruction.
    pub fn step<B: SpcBus>(&mut self, bus: &mut B) {
        if self.stopped {
            return;
        }
        if self.sleeping {
            // Sleep until an interrupt wakes us — we'll wire that
            // when timers/mailboxes are in.
            return;
        }
        let opcode = self.fetch_u8(bus);
        self.execute(opcode, bus);
    }

    #[allow(clippy::too_many_lines)]
    fn execute<B: SpcBus>(&mut self, opcode: u8, bus: &mut B) {
        match opcode {
            // ---------------------------------------------------------
            // No-op + sleep + stop
            // ---------------------------------------------------------
            0x00 => { /* NOP */ }
            0xEF => self.sleeping = true,
            0xFF => self.stopped = true,

            // ---------------------------------------------------------
            // Flag toggles
            // ---------------------------------------------------------
            0x20 => self.psw.remove(bit::P),          // CLRP
            0x40 => self.psw.insert(bit::P),          // SETP
            0x60 => self.psw.remove(bit::C),          // CLRC
            0x80 => self.psw.insert(bit::C),          // SETC
            0xA0 => self.psw.insert(bit::I),          // EI
            0xC0 => self.psw.remove(bit::I),          // DI
            0xE0 => self.psw.remove(bit::V | bit::H), // CLRV
            0xED => self.psw.0 ^= bit::C,             // NOTC

            // ---------------------------------------------------------
            // Immediate MOV
            // ---------------------------------------------------------
            0xE8 => {
                // MOV A,#imm
                let v = self.fetch_u8(bus);
                self.a = v;
                self.set_nz(v);
            }
            0xCD => {
                // MOV X,#imm
                let v = self.fetch_u8(bus);
                self.x = v;
                self.set_nz(v);
            }
            0x8D => {
                // MOV Y,#imm
                let v = self.fetch_u8(bus);
                self.y = v;
                self.set_nz(v);
            }

            // ---------------------------------------------------------
            // Direct-page MOV (load + store) — the most common form
            // ---------------------------------------------------------
            0xE4 => {
                // MOV A,dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                self.a = v;
                self.set_nz(v);
            }
            0xF4 => {
                // MOV A,dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr);
                self.a = v;
                self.set_nz(v);
            }
            0xC4 => {
                // MOV dp,A
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                bus.write(addr, self.a);
            }
            0xD4 => {
                // MOV dp+X,A
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                bus.write(addr, self.a);
            }

            // ---------------------------------------------------------
            // Absolute MOV — `!abs` in SPC asm notation
            // ---------------------------------------------------------
            0xE5 => {
                // MOV A,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.a = v;
                self.set_nz(v);
            }
            0xC5 => {
                // MOV !abs,A
                let addr = self.fetch_u16(bus);
                bus.write(addr, self.a);
            }
            0xE9 => {
                // MOV X,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.x = v;
                self.set_nz(v);
            }
            0xEC => {
                // MOV Y,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.y = v;
                self.set_nz(v);
            }
            0xC9 => {
                // MOV !abs,X
                let addr = self.fetch_u16(bus);
                bus.write(addr, self.x);
            }
            0xCC => {
                // MOV !abs,Y
                let addr = self.fetch_u16(bus);
                bus.write(addr, self.y);
            }

            // ---------------------------------------------------------
            // Branches — 8-bit signed PC-relative
            // ---------------------------------------------------------
            0x2F => self.branch_if(bus, true), // BRA
            0x10 => self.branch_if(bus, !self.psw.contains(bit::N)), // BPL
            0x30 => self.branch_if(bus, self.psw.contains(bit::N)), // BMI
            0x50 => self.branch_if(bus, !self.psw.contains(bit::V)), // BVC
            0x70 => self.branch_if(bus, self.psw.contains(bit::V)), // BVS
            0x90 => self.branch_if(bus, !self.psw.contains(bit::C)), // BCC
            0xB0 => self.branch_if(bus, self.psw.contains(bit::C)), // BCS
            0xD0 => self.branch_if(bus, !self.psw.contains(bit::Z)), // BNE
            0xF0 => self.branch_if(bus, self.psw.contains(bit::Z)), // BEQ

            // Everything else: future P2.SPC.2+ territory.
            other => panic!(
                "luna-cpu-spc700: opcode 0x{other:02X} not yet implemented (PC=${:04X})",
                self.pc.wrapping_sub(1),
            ),
        }
    }

    fn branch_if<B: SpcBus>(&mut self, bus: &mut B, condition: bool) {
        let offset = self.fetch_u8(bus) as i8;
        if condition {
            self.pc = self.pc.wrapping_add_signed(i16::from(offset));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::RamBus;

    /// Build a CPU sitting at $0200 with the given program bytes.
    fn run(program: &[u8]) -> (Spc700, RamBus) {
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        // Reset vector → $0200.
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke_slice(0x0200, program);
        cpu.reset(&mut bus);
        (cpu, bus)
    }

    // -------------------------------------------------------------------
    // NOP / SLEEP / STOP
    // -------------------------------------------------------------------

    #[test]
    fn nop_just_advances_pc() {
        let (mut cpu, mut bus) = run(&[0x00]);
        let pc_before = cpu.pc;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, pc_before.wrapping_add(1));
    }

    #[test]
    fn sleep_sets_sleeping_flag() {
        let (mut cpu, mut bus) = run(&[0xEF]);
        cpu.step(&mut bus);
        assert!(cpu.sleeping);
    }

    #[test]
    fn stop_sets_stopped_flag() {
        let (mut cpu, mut bus) = run(&[0xFF]);
        cpu.step(&mut bus);
        assert!(cpu.stopped);
    }

    // -------------------------------------------------------------------
    // Flag toggles
    // -------------------------------------------------------------------

    #[test]
    fn setp_clrp_toggles_p_and_changes_direct_page() {
        let (mut cpu, mut bus) = run(&[0x40, 0x20]); // SETP, CLRP
        cpu.step(&mut bus);
        assert!(cpu.psw.contains(bit::P));
        assert_eq!(cpu.direct_addr(0x10), 0x0110);
        cpu.step(&mut bus);
        assert!(!cpu.psw.contains(bit::P));
        assert_eq!(cpu.direct_addr(0x10), 0x0010);
    }

    #[test]
    fn setc_clrc_notc() {
        let (mut cpu, mut bus) = run(&[0x80, 0xED, 0x60]); // SETC, NOTC, CLRC
        cpu.step(&mut bus);
        assert!(cpu.psw.contains(bit::C));
        cpu.step(&mut bus);
        assert!(!cpu.psw.contains(bit::C), "NOTC flipped C");
        cpu.step(&mut bus);
        assert!(!cpu.psw.contains(bit::C));
    }

    #[test]
    fn ei_di_toggles_interrupt_flag() {
        let (mut cpu, mut bus) = run(&[0xA0, 0xC0]); // EI, DI
        cpu.step(&mut bus);
        assert!(cpu.psw.contains(bit::I));
        cpu.step(&mut bus);
        assert!(!cpu.psw.contains(bit::I));
    }

    #[test]
    fn clrv_clears_both_v_and_h() {
        let (mut cpu, mut bus) = run(&[0xE0]);
        cpu.psw.insert(bit::V | bit::H);
        cpu.step(&mut bus);
        assert!(!cpu.psw.contains(bit::V));
        assert!(!cpu.psw.contains(bit::H));
    }

    // -------------------------------------------------------------------
    // MOV immediate
    // -------------------------------------------------------------------

    #[test]
    fn mov_a_imm_sets_flags() {
        let (mut cpu, mut bus) = run(&[0xE8, 0x80]); // MOV A,#$80
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.psw.contains(bit::N));
        assert!(!cpu.psw.contains(bit::Z));
    }

    #[test]
    fn mov_x_imm_zero_sets_z() {
        let (mut cpu, mut bus) = run(&[0xCD, 0x00]);
        cpu.step(&mut bus);
        assert_eq!(cpu.x, 0);
        assert!(cpu.psw.contains(bit::Z));
    }

    #[test]
    fn mov_y_imm_round_trip() {
        let (mut cpu, mut bus) = run(&[0x8D, 0x42]);
        cpu.step(&mut bus);
        assert_eq!(cpu.y, 0x42);
    }

    // -------------------------------------------------------------------
    // Direct-page MOV
    // -------------------------------------------------------------------

    #[test]
    fn mov_a_dp_reads_from_direct_page() {
        let (mut cpu, mut bus) = run(&[0xE4, 0x10]); // MOV A,$10
        bus.poke(0x0010, 0x55);
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0x55);
    }

    #[test]
    fn mov_dp_a_writes_to_direct_page() {
        // MOV A,#$77 ; MOV $20,A
        let (mut cpu, mut bus) = run(&[0xE8, 0x77, 0xC4, 0x20]);
        cpu.step(&mut bus); // MOV A,#$77
        cpu.step(&mut bus); // MOV $20,A
        assert_eq!(bus.peek(0x0020), 0x77);
    }

    #[test]
    fn mov_a_dp_with_p_flag_reads_page_1() {
        // SETP ; MOV A,$10 → reads $0110
        let (mut cpu, mut bus) = run(&[0x40, 0xE4, 0x10]);
        bus.poke(0x0110, 0xAA);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0xAA);
    }

    #[test]
    fn mov_a_dp_plus_x_indexes_with_x() {
        // MOV X,#$04 ; MOV A,$10+X → reads $0014
        let (mut cpu, mut bus) = run(&[0xCD, 0x04, 0xF4, 0x10]);
        bus.poke(0x0014, 0x88);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0x88);
    }

    // -------------------------------------------------------------------
    // Absolute MOV
    // -------------------------------------------------------------------

    #[test]
    fn mov_a_abs_reads_anywhere() {
        let (mut cpu, mut bus) = run(&[0xE5, 0x34, 0x12]); // MOV A,$1234
        bus.poke(0x1234, 0xCE);
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0xCE);
    }

    #[test]
    fn mov_abs_x_writes_x_to_memory() {
        // MOV X,#$BB ; MOV $1234,X
        let (mut cpu, mut bus) = run(&[0xCD, 0xBB, 0xC9, 0x34, 0x12]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x1234), 0xBB);
    }

    #[test]
    fn mov_abs_y_writes_y_to_memory() {
        let (mut cpu, mut bus) = run(&[0x8D, 0x55, 0xCC, 0x34, 0x12]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x1234), 0x55);
    }

    // -------------------------------------------------------------------
    // Branches
    // -------------------------------------------------------------------

    #[test]
    fn bra_always_jumps() {
        // BRA +$10 — from PC=$0200, after the 2-byte op PC=$0202,
        // target = $0212.
        let (mut cpu, mut bus) = run(&[0x2F, 0x10]);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x0212);
    }

    #[test]
    fn bne_taken_when_z_clear() {
        // MOV A,#$01 (Z=0), BNE +$04, then would-be-skipped block
        let (mut cpu, mut bus) = run(&[0xE8, 0x01, 0xD0, 0x04]);
        cpu.step(&mut bus); // MOV A
        cpu.step(&mut bus); // BNE
        assert_eq!(cpu.pc, 0x0208);
    }

    #[test]
    fn beq_not_taken_when_z_clear() {
        let (mut cpu, mut bus) = run(&[0xE8, 0x01, 0xF0, 0x04]);
        cpu.step(&mut bus); // MOV A
        cpu.step(&mut bus); // BEQ
        // PC should be just past the BEQ (no jump): $0204.
        assert_eq!(cpu.pc, 0x0204);
    }

    #[test]
    fn bcs_taken_after_setc() {
        let (mut cpu, mut bus) = run(&[0x80, 0xB0, 0x10]);
        cpu.step(&mut bus); // SETC
        cpu.step(&mut bus); // BCS
        // After SETC ($0201), BCS at $0201 with rel byte at $0202.
        // After fetching the rel byte PC=$0203, + $10 = $0213.
        assert_eq!(cpu.pc, 0x0213);
    }
}
