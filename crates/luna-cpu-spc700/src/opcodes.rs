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
            // IPL ROM family — opcodes the SNES boot ROM uses
            // ---------------------------------------------------------
            0xBD => {
                // MOV SP,X — SP = X (does NOT set flags on real HW).
                self.sp = self.x;
            }
            0xC6 => {
                // MOV (X),A — store A at direct page byte addressed by X.
                let addr = self.direct_addr(self.x);
                bus.write(addr, self.a);
            }
            0x1D => {
                // DEC X
                self.x = self.x.wrapping_sub(1);
                let v = self.x;
                self.set_nz(v);
            }
            0xFC => {
                // INC Y
                self.y = self.y.wrapping_add(1);
                let v = self.y;
                self.set_nz(v);
            }
            0xAB => {
                // INC dp — read, increment, write back; update N/Z.
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr).wrapping_add(1);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x8F => {
                // MOV dp,#imm — store imm into a direct page byte. The
                // SPC syntax orders the operands `(imm, dp)` in object
                // code (CPU eats imm first, then dp).
                let imm = self.fetch_u8(bus);
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                bus.write(addr, imm);
            }
            0x78 => {
                // CMP dp,#imm — compare direct page byte with imm,
                // set N/Z/C without writing back.
                let imm = self.fetch_u8(bus);
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let mem = bus.read(addr);
                self.cmp_u8(mem, imm);
            }
            0xEB => {
                // MOV Y,dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                self.y = v;
                self.set_nz(v);
            }
            0x7E => {
                // CMP Y,dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let mem = bus.read(addr);
                let y = self.y;
                self.cmp_u8(y, mem);
            }
            0xCB => {
                // MOV dp,Y
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                bus.write(addr, self.y);
            }
            0xD7 => {
                // MOV [dp]+Y,A — indirect indexed Y store. Read 16-bit
                // pointer at (dp), add Y, store A there.
                let dp = self.fetch_u8(bus);
                let plo = bus.read(self.direct_addr(dp));
                let phi = bus.read(self.direct_addr(dp.wrapping_add(1)));
                let ptr = u16::from(plo) | (u16::from(phi) << 8);
                let target = ptr.wrapping_add(u16::from(self.y));
                bus.write(target, self.a);
            }
            0xDD => {
                // MOV A,Y
                self.a = self.y;
                let v = self.a;
                self.set_nz(v);
            }
            0x5D => {
                // MOV X,A
                self.x = self.a;
                let v = self.x;
                self.set_nz(v);
            }
            0xBA => {
                // MOVW YA,dp — 16-bit load. Y ← (dp+1), A ← (dp).
                // Sets N/Z based on the resulting 16-bit YA.
                let dp = self.fetch_u8(bus);
                let lo = bus.read(self.direct_addr(dp));
                let hi = bus.read(self.direct_addr(dp.wrapping_add(1)));
                self.a = lo;
                self.y = hi;
                let v16 = u16::from(lo) | (u16::from(hi) << 8);
                self.psw.set(bit::Z, v16 == 0);
                self.psw.set(bit::N, v16 & 0x8000 != 0);
            }
            0xDA => {
                // MOVW dp,YA — 16-bit store. (dp) ← A, (dp+1) ← Y.
                let dp = self.fetch_u8(bus);
                bus.write(self.direct_addr(dp), self.a);
                bus.write(self.direct_addr(dp.wrapping_add(1)), self.y);
            }
            0x1F => {
                // JMP [!abs+X] — indirect-indexed jump. Read 16-bit
                // pointer at (abs + X), jump there.
                let base = self.fetch_u16(bus);
                let ptr = base.wrapping_add(u16::from(self.x));
                let lo = bus.read(ptr);
                let hi = bus.read(ptr.wrapping_add(1));
                self.pc = u16::from(lo) | (u16::from(hi) << 8);
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

    /// Update N / Z / C for an 8-bit compare `lhs - rhs`. No value is
    /// stored back — only flags change. C is set when `lhs >= rhs`
    /// (unsigned), matching 65C816 / SPC700 semantics.
    fn cmp_u8(&mut self, lhs: u8, rhs: u8) {
        let result = lhs.wrapping_sub(rhs);
        self.set_nz(result);
        self.psw.set(bit::C, lhs >= rhs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iplrom::{IPL_ROM, IPL_ROM_BASE};
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

    // -------------------------------------------------------------------
    // IPL ROM family — opcodes the SNES boot ROM needs
    // -------------------------------------------------------------------

    #[test]
    fn mov_sp_x_assigns_stack_pointer() {
        let (mut cpu, mut bus) = run(&[0xCD, 0xEF, 0xBD]);
        cpu.step(&mut bus); // MOV X,#$EF
        cpu.step(&mut bus); // MOV SP,X
        assert_eq!(cpu.x, 0xEF);
        assert_eq!(cpu.sp, 0xEF);
    }

    #[test]
    fn dec_x_decrements_and_sets_zero() {
        let (mut cpu, mut bus) = run(&[0xCD, 0x01, 0x1D, 0x1D]);
        cpu.step(&mut bus); // MOV X,#$01
        cpu.step(&mut bus); // DEC X → 0, Z set
        assert_eq!(cpu.x, 0);
        assert!(cpu.psw.contains(bit::Z));
        cpu.step(&mut bus); // DEC X → $FF, N set
        assert_eq!(cpu.x, 0xFF);
        assert!(cpu.psw.contains(bit::N));
    }

    #[test]
    fn inc_y_increments_and_sets_zero_on_wrap() {
        let (mut cpu, mut bus) = run(&[0x8D, 0xFF, 0xFC]);
        cpu.step(&mut bus); // MOV Y,#$FF
        cpu.step(&mut bus); // INC Y → 0
        assert_eq!(cpu.y, 0);
        assert!(cpu.psw.contains(bit::Z));
    }

    #[test]
    fn mov_dp_imm_stores_byte() {
        let (mut cpu, mut bus) = run(&[0x8F, 0x42, 0x10]);
        cpu.step(&mut bus); // MOV $10,#$42
        assert_eq!(bus.peek(0x0010), 0x42);
    }

    #[test]
    fn cmp_dp_imm_sets_zero_when_equal() {
        let (mut cpu, mut bus) = run(&[0x8F, 0x42, 0x10, 0x78, 0x42, 0x10]);
        cpu.step(&mut bus); // MOV $10,#$42
        cpu.step(&mut bus); // CMP $10,#$42
        assert!(cpu.psw.contains(bit::Z));
        assert!(cpu.psw.contains(bit::C));
    }

    #[test]
    fn mov_indirect_x_a_stores_at_dp_x() {
        // Set X=$10, A=$42, then MOV (X),A — write $42 to dp byte $10.
        let (mut cpu, mut bus) = run(&[0xCD, 0x10, 0xE8, 0x42, 0xC6]);
        cpu.step(&mut bus); // MOV X,#$10
        cpu.step(&mut bus); // MOV A,#$42
        cpu.step(&mut bus); // MOV (X),A
        assert_eq!(bus.peek(0x0010), 0x42);
    }

    #[test]
    fn movw_ya_dp_loads_16bit_word() {
        let (mut cpu, mut bus) = run(&[0xBA, 0x10]);
        bus.poke(0x0010, 0x34);
        bus.poke(0x0011, 0x12);
        cpu.step(&mut bus); // MOVW YA,$10
        assert_eq!(cpu.a, 0x34);
        assert_eq!(cpu.y, 0x12);
    }

    #[test]
    fn movw_dp_ya_stores_16bit_word() {
        let (mut cpu, mut bus) = run(&[0xE8, 0x34, 0x8D, 0x12, 0xDA, 0x10]);
        cpu.step(&mut bus); // MOV A,#$34
        cpu.step(&mut bus); // MOV Y,#$12
        cpu.step(&mut bus); // MOVW $10,YA
        assert_eq!(bus.peek(0x0010), 0x34);
        assert_eq!(bus.peek(0x0011), 0x12);
    }

    #[test]
    fn jmp_indirect_abs_x_loads_pc_from_table() {
        // Build a 1-entry jump table at $0500: $0500 = $80, $0501 = $03 →
        // entry value $0380. With X=0, JMP [!$0500+X] jumps to $0380.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke_slice(0x0200, &[0x1F, 0x00, 0x05]);
        bus.poke(0x0500, 0x80);
        bus.poke(0x0501, 0x03);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x0380);
    }

    // -------------------------------------------------------------------
    // Full IPL ROM integration test
    // -------------------------------------------------------------------

    /// Boot a fresh SPC into its IPL ROM and step until it parks in
    /// the "wait for $CC kick" loop. After that, `$F4 = $AA` and
    /// `$F5 = $BB` — the canonical signal the main CPU reads at
    /// `$2140 / $2141` to know the SPC is alive.
    #[test]
    fn ipl_rom_reaches_kick_wait_loop_and_writes_handshake() {
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        // Drop the IPL ROM at its canonical address $FFC0..=$FFFF.
        bus.poke_slice(IPL_ROM_BASE, &IPL_ROM);
        cpu.reset(&mut bus);
        // Reset vector ($FFFE/$FFFF in the IPL ROM) → $FFC0.
        assert_eq!(cpu.pc, 0xFFC0);
        // 240 instructions is way more than the worst-case path
        // through "clear $00..$EF then write handshake" — enough to
        // park us in the wait loop at $FFCF.
        for _ in 0..2000 {
            cpu.step(&mut bus);
            if cpu.pc == 0xFFCF {
                // We're in the "wait for $CC kick" CMP loop —
                // exactly what we want to verify.
                break;
            }
        }
        assert_eq!(cpu.pc, 0xFFCF, "SPC did not park in the wait loop");
        assert_eq!(bus.peek(0x00F4), 0xAA, "mailbox 0 should hold $AA");
        assert_eq!(bus.peek(0x00F5), 0xBB, "mailbox 1 should hold $BB");
    }

    /// After the kick wait loop, drop $CC into $F4 (= what the main
    /// CPU would do via `STA $2140,#$CC`) and confirm the IPL ROM
    /// proceeds into the byte-transfer block.
    #[test]
    fn ipl_rom_advances_past_kick_when_cc_is_written() {
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke_slice(IPL_ROM_BASE, &IPL_ROM);
        cpu.reset(&mut bus);
        for _ in 0..2000 {
            cpu.step(&mut bus);
            if cpu.pc == 0xFFCF {
                break;
            }
        }
        // Simulate the main CPU's $CC kick.
        bus.poke(0x00F4, 0xCC);
        // Now run until the IPL ROM leaves the wait loop. The BRA
        // at $FFD4 jumps to $FFEF (the per-byte transfer setup).
        for _ in 0..200 {
            cpu.step(&mut bus);
            if cpu.pc >= 0xFFD4 && cpu.pc != 0xFFCF {
                break;
            }
        }
        assert!(
            cpu.pc >= 0xFFD4,
            "SPC stuck in wait loop after kick, pc=${:04X}",
            cpu.pc
        );
    }
}
