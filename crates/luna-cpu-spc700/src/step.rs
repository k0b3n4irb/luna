//! Cycle-stepped (mid-instruction-resumable) SPC700 execution.
//!
//! The atomic [`Spc700::step`](crate::Spc700::step) runs a whole
//! instruction per call; this module adds [`Spc700::step_cycle`], which
//! advances **exactly one SPC cycle** (one bus access — read / write /
//! idle) and resumes at [`Spc700::op_step`](crate::Spc700) on the next
//! call. This lets a driver stop the SPC at *exactly* the CPU's cycle at
//! each mailbox access, so the CPU↔SPC interleave is cycle-exact (ares
//! runs the SMP as a cooperative thread; Mesen2 as an explicit
//! `_opCode`/`_opStep` state machine — this mirrors the latter).
//!
//! ## Staging (see the cycle-stepped-SPC plan)
//!
//! Opcodes are ported to micro-steps **group by group**, each validated
//! byte- and cycle-exact against the atomic `step()` by the equivalence
//! harness in this module's tests (and, downstream, the Tom-Harte +
//! trajectory harnesses). Until every opcode is ported, `step_cycle`
//! panics on an un-ported opcode and the atomic path still drives
//! emulation — there is no behavioural cutover yet.

use crate::bus::SpcBus;
use crate::cpu::Spc700;

/// Outcome of one [`Spc700::step_cycle`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// The instruction is still in progress; call `step_cycle` again.
    Running,
    /// The instruction completed on this cycle.
    Complete,
}

impl Spc700 {
    /// Advance the SPC700 by **exactly one cycle** (one bus access),
    /// resuming mid-instruction. The first cycle of an instruction
    /// fetches the opcode; subsequent cycles run [`Self::execute_cycle`].
    ///
    /// Returns [`StepResult::Complete`] on the cycle that finishes the
    /// instruction (so a full instruction is N `step_cycle` calls = its
    /// true cycle count).
    pub fn step_cycle<B: SpcBus>(&mut self, bus: &mut B) -> StepResult {
        if !self.in_instruction {
            if self.stopped || self.sleeping {
                // Halted: still burns wall-clock (mirrors `step()`'s
                // 2-cycle return, here as two idle cycles so the bus
                // clocks the timer/DSP). Modelled as a 2-cycle pseudo-op.
                bus.idle();
                self.op = 0; // sentinel handled below
                self.op_step = 0xFE; // halt micro-state
                self.in_instruction = true;
                return StepResult::Running;
            }
            // Cycle 1: fetch the opcode.
            self.branch_taken = false;
            self.op = self.fetch_u8(bus);
            self.op_step = 0;
            self.in_instruction = true;
            return StepResult::Running;
        }
        if self.op_step == 0xFE {
            // Second halt cycle.
            bus.idle();
            self.in_instruction = false;
            return StepResult::Complete;
        }
        let done = self.execute_cycle(bus);
        if done {
            self.in_instruction = false;
            self.op_step = 0;
            StepResult::Complete
        } else {
            StepResult::Running
        }
    }

    /// Run `step_cycle` to completion and return the instruction's cycle
    /// count. Test/bring-up helper that mirrors one atomic `step()`.
    pub fn step_instruction<B: SpcBus>(&mut self, bus: &mut B) -> u32 {
        let mut cycles = 0u32;
        loop {
            cycles += 1;
            if self.step_cycle(bus) == StepResult::Complete {
                return cycles;
            }
        }
    }

    /// Execute one post-fetch cycle of the current opcode (`self.op`),
    /// resuming at `self.op_step`. Returns `true` on the last cycle.
    ///
    /// Each arm performs exactly one bus access per `op_step` so the
    /// access sequence matches the atomic handler in `opcodes.rs`
    /// byte-for-byte. Ported group by group (plan Stage 2).
    fn execute_cycle<B: SpcBus>(&mut self, bus: &mut B) -> bool {
        match self.op {
            // NOP — implied: dummy-read PC (cycle 2).
            0x00 => {
                self.dummy_read_pc(bus);
                true
            }
            // MOV A,#imm
            0xE8 => {
                let v = self.fetch_u8(bus);
                self.a = v;
                self.set_nz(v);
                true
            }
            // MOV X,#imm
            0xCD => {
                let v = self.fetch_u8(bus);
                self.x = v;
                self.set_nz(v);
                true
            }
            // MOV Y,#imm
            0x8D => {
                let v = self.fetch_u8(bus);
                self.y = v;
                self.set_nz(v);
                true
            }
            // MOV A,dp — fetch dp; read [dp]. (2-step opcodes use if/else;
            // 3+-step use `match self.op_step` — both stay clippy-clean.)
            0xE4 => {
                if self.op_step == 0 {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                } else {
                    let addr = self.direct_addr(self.oper as u8);
                    let v = bus.read(addr);
                    self.a = v;
                    self.set_nz(v);
                    true
                }
            }
            // MOV dp,A — fetch dp; dummy-read [dp]; write [dp].
            0xC4 => match self.op_step {
                0 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 1;
                    false
                }
                1 => {
                    let _ = bus.read(self.addr_lat);
                    self.op_step = 2;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.a);
                    true
                }
            },
            // BRA — fetch rel; idle; idle (always taken).
            0x2F => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.op_step = 2;
                    false
                }
                _ => {
                    bus.idle();
                    let off = self.oper as u8 as i8;
                    self.pc = self.pc.wrapping_add_signed(i16::from(off));
                    self.branch_taken = true;
                    true
                }
            },
            other => panic!("step_cycle: opcode ${other:02X} not yet ported to micro-steps"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::SpcBus;

    /// `SpcBus` over flat RAM that records every access (kind, addr,
    /// value) so the cycle-stepped path can be proven identical to the
    /// atomic path access-for-access.
    struct RecBus {
        mem: Box<[u8]>,
        trace: Vec<(char, u16, u8)>,
    }
    impl RecBus {
        fn new() -> Self {
            Self {
                mem: vec![0u8; 0x1_0000].into_boxed_slice(),
                trace: Vec::new(),
            }
        }
    }
    impl SpcBus for RecBus {
        fn read(&mut self, addr: u16) -> u8 {
            let v = self.mem[usize::from(addr)];
            self.trace.push(('R', addr, v));
            v
        }
        fn write(&mut self, addr: u16, value: u8) {
            self.mem[usize::from(addr)] = value;
            self.trace.push(('W', addr, value));
        }
        fn idle(&mut self) {
            self.trace.push(('I', 0, 0));
        }
    }

    /// Assert the cycle-stepped path matches the atomic `step()` for a
    /// program at PC=0x0200: identical final registers, identical cycle
    /// count, and identical bus access trace (kind+addr+value, in order).
    fn assert_equiv(prog: &[u8], setup: impl Fn(&mut Spc700, &mut RecBus)) {
        // Atomic reference.
        let mut cpu_a = Spc700::new();
        cpu_a.pc = 0x0200;
        let mut bus_a = RecBus::new();
        for (i, &b) in prog.iter().enumerate() {
            bus_a.mem[0x0200 + i] = b;
        }
        setup(&mut cpu_a, &mut bus_a);
        let cyc_a = u32::from(cpu_a.step(&mut bus_a));

        // Cycle-stepped.
        let mut cpu_b = Spc700::new();
        cpu_b.pc = 0x0200;
        let mut bus_b = RecBus::new();
        for (i, &b) in prog.iter().enumerate() {
            bus_b.mem[0x0200 + i] = b;
        }
        setup(&mut cpu_b, &mut bus_b);
        let cyc_b = cpu_b.step_instruction(&mut bus_b);

        assert_eq!(cyc_a, cyc_b, "cycle count differs for prog {prog:02X?}");
        assert_eq!(
            bus_a.trace, bus_b.trace,
            "bus trace differs for prog {prog:02X?}"
        );
        // Compare the architectural registers (ignore the resumable-core
        // scratch fields, which only the stepped path uses).
        assert_eq!(cpu_a.a, cpu_b.a, "A differs");
        assert_eq!(cpu_a.x, cpu_b.x, "X differs");
        assert_eq!(cpu_a.y, cpu_b.y, "Y differs");
        assert_eq!(cpu_a.pc, cpu_b.pc, "PC differs");
        assert_eq!(cpu_a.psw.0, cpu_b.psw.0, "PSW differs");
        assert_eq!(cpu_a.sp, cpu_b.sp, "SP differs");
        assert_eq!(
            cpu_a.branch_taken, cpu_b.branch_taken,
            "branch_taken differs"
        );
        assert_eq!(bus_a.mem[..], bus_b.mem[..], "memory differs");
    }

    #[test]
    fn equiv_nop() {
        assert_equiv(&[0x00], |_, _| {});
    }

    #[test]
    fn equiv_mov_a_imm() {
        assert_equiv(&[0xE8, 0x7F], |_, _| {});
        assert_equiv(&[0xE8, 0x00], |_, _| {}); // sets Z
        assert_equiv(&[0xE8, 0x80], |_, _| {}); // sets N
    }

    #[test]
    fn equiv_mov_xy_imm() {
        assert_equiv(&[0xCD, 0x42], |_, _| {});
        assert_equiv(&[0x8D, 0x00], |_, _| {});
    }

    #[test]
    fn equiv_mov_a_dp() {
        assert_equiv(&[0xE4, 0x30], |_, bus| bus.mem[0x0030] = 0x99);
    }

    #[test]
    fn equiv_mov_dp_a() {
        assert_equiv(&[0xC4, 0x30], |cpu, _| cpu.a = 0xAB);
    }

    #[test]
    fn equiv_bra() {
        assert_equiv(&[0x2F, 0x05], |_, _| {}); // forward
        assert_equiv(&[0x2F, 0xFB], |_, _| {}); // backward
    }
}
