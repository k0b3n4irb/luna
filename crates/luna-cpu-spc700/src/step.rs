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
use crate::flags::bit;

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
    #[allow(clippy::too_many_lines)]
    fn execute_cycle<B: SpcBus>(&mut self, bus: &mut B) -> bool {
        match self.op {
            // =============================================================
            // Implied / register-only ops — one post-fetch cycle each:
            // a `dummy_read_pc` (or a state change + dummy read).
            // =============================================================
            0x00 => {
                // NOP
                self.dummy_read_pc(bus);
                true
            }
            0x20 => {
                // CLRP
                self.psw.remove(bit::P);
                self.dummy_read_pc(bus);
                true
            }
            0x40 => {
                // SETP
                self.psw.insert(bit::P);
                self.dummy_read_pc(bus);
                true
            }
            0x60 => {
                // CLRC
                self.psw.remove(bit::C);
                self.dummy_read_pc(bus);
                true
            }
            0x80 => {
                // SETC
                self.psw.insert(bit::C);
                self.dummy_read_pc(bus);
                true
            }
            0xE0 => {
                // CLRV
                self.psw.remove(bit::V | bit::H);
                self.dummy_read_pc(bus);
                true
            }
            0xA0 => {
                // EI — dummy read; idle.
                if self.op_step == 0 {
                    self.psw.insert(bit::I);
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                } else {
                    bus.idle();
                    true
                }
            }
            0xC0 => {
                // DI — dummy read; idle.
                if self.op_step == 0 {
                    self.psw.remove(bit::I);
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                } else {
                    bus.idle();
                    true
                }
            }
            0xED => {
                // NOTC — dummy read; idle.
                if self.op_step == 0 {
                    self.psw.0 ^= bit::C;
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                } else {
                    bus.idle();
                    true
                }
            }
            0xEF => {
                // SLEEP
                self.sleeping = true;
                self.dummy_read_pc(bus);
                true
            }
            0xFF => {
                // STOP
                self.stopped = true;
                self.dummy_read_pc(bus);
                true
            }
            // Register transfers / inc / dec / shifts on A (all: op; dummy read).
            0xBD => {
                // MOV SP,X (no flags)
                self.sp = self.x;
                self.dummy_read_pc(bus);
                true
            }
            0x1D => {
                // DEC X
                self.x = self.x.wrapping_sub(1);
                let v = self.x;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x3D => {
                // INC X
                self.x = self.x.wrapping_add(1);
                let v = self.x;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0xFC => {
                // INC Y
                self.y = self.y.wrapping_add(1);
                let v = self.y;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0xDC => {
                // DEC Y
                self.y = self.y.wrapping_sub(1);
                let v = self.y;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0xBC => {
                // INC A
                self.a = self.a.wrapping_add(1);
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x9C => {
                // DEC A
                self.a = self.a.wrapping_sub(1);
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x1C => {
                // ASL A
                let a = self.a;
                self.psw.set(bit::C, a & 0x80 != 0);
                self.a = a << 1;
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x5C => {
                // LSR A
                let a = self.a;
                self.psw.set(bit::C, a & 0x01 != 0);
                self.a = a >> 1;
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x3C => {
                // ROL A
                let a = self.a;
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, a & 0x80 != 0);
                self.a = (a << 1) | c_in;
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x7C => {
                // ROR A
                let a = self.a;
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, a & 0x01 != 0);
                self.a = (a >> 1) | (c_in << 7);
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0xDD => {
                // MOV A,Y
                self.a = self.y;
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x5D => {
                // MOV X,A
                self.x = self.a;
                let v = self.x;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x9D => {
                // MOV X,SP
                self.x = self.sp;
                let v = self.x;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0xFD => {
                // MOV Y,A
                self.y = self.a;
                let v = self.y;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }
            0x7D => {
                // MOV A,X
                self.a = self.x;
                let v = self.a;
                self.set_nz(v);
                self.dummy_read_pc(bus);
                true
            }

            // =============================================================
            // Immediate MOV into a register: fetch imm.
            // =============================================================
            0xE8 => {
                let v = self.fetch_u8(bus);
                self.a = v;
                self.set_nz(v);
                true
            }
            0xCD => {
                let v = self.fetch_u8(bus);
                self.x = v;
                self.set_nz(v);
                true
            }
            0x8D => {
                let v = self.fetch_u8(bus);
                self.y = v;
                self.set_nz(v);
                true
            }

            // =============================================================
            // ALU/CMP A,#imm — fetch imm; compute.
            // =============================================================
            0x08 => {
                let imm = self.fetch_u8(bus);
                self.a |= imm;
                let v = self.a;
                self.set_nz(v);
                true
            }
            0x28 => {
                let imm = self.fetch_u8(bus);
                self.a &= imm;
                let v = self.a;
                self.set_nz(v);
                true
            }
            0x48 => {
                let imm = self.fetch_u8(bus);
                self.a ^= imm;
                let v = self.a;
                self.set_nz(v);
                true
            }
            0x88 => {
                let imm = self.fetch_u8(bus);
                let a = self.a;
                self.a = self.adc_u8(a, imm);
                true
            }
            0xA8 => {
                let imm = self.fetch_u8(bus);
                let a = self.a;
                self.a = self.sbc_u8(a, imm);
                true
            }
            0x68 => {
                let imm = self.fetch_u8(bus);
                let a = self.a;
                self.cmp_u8(a, imm);
                true
            }
            0xC8 => {
                let imm = self.fetch_u8(bus);
                let x = self.x;
                self.cmp_u8(x, imm);
                true
            }
            0xAD => {
                let imm = self.fetch_u8(bus);
                let y = self.y;
                self.cmp_u8(y, imm);
                true
            }

            // =============================================================
            // MOV A,dp / register loads from dp: fetch dp; read [dp].
            // =============================================================
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
            0xEB => {
                // MOV Y,dp
                if self.op_step == 0 {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                } else {
                    let addr = self.direct_addr(self.oper as u8);
                    let v = bus.read(addr);
                    self.y = v;
                    self.set_nz(v);
                    true
                }
            }
            0xF8 => {
                // MOV X,dp
                if self.op_step == 0 {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                } else {
                    let addr = self.direct_addr(self.oper as u8);
                    let v = bus.read(addr);
                    self.x = v;
                    self.set_nz(v);
                    true
                }
            }
            // ALU/CMP A,dp: fetch dp; read [dp]; compute.
            0x04 | 0x24 | 0x44 | 0x64 | 0x84 | 0xA4 => {
                if self.op_step == 0 {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                } else {
                    let v = bus.read(self.direct_addr(self.oper as u8));
                    self.alu_a(self.op, v);
                    true
                }
            }
            // CMP X,dp / CMP Y,dp: fetch dp; read [dp]; compare.
            0x3E => {
                if self.op_step == 0 {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                } else {
                    let mem = bus.read(self.direct_addr(self.oper as u8));
                    let x = self.x;
                    self.cmp_u8(x, mem);
                    true
                }
            }
            0x7E => {
                if self.op_step == 0 {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                } else {
                    let mem = bus.read(self.direct_addr(self.oper as u8));
                    let y = self.y;
                    self.cmp_u8(y, mem);
                    true
                }
            }

            // =============================================================
            // MOV dp,reg — fetch dp; dummy-read [dp]; write [dp].
            // =============================================================
            0xC4 | 0xCB | 0xD8 => match self.op_step {
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
                    let v = match self.op {
                        0xC4 => self.a,
                        0xCB => self.y,
                        _ => self.x,
                    };
                    bus.write(self.addr_lat, v);
                    true
                }
            },

            // =============================================================
            // MOV reg,dp+idx — fetch dp; idle; read [dp+idx].
            // =============================================================
            0xF4 => {
                // MOV A,dp+X
                match self.op_step {
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
                        let addr = self.direct_addr((self.oper as u8).wrapping_add(self.x));
                        let v = bus.read(addr);
                        self.a = v;
                        self.set_nz(v);
                        true
                    }
                }
            }
            0xF9 => {
                // MOV X,dp+Y
                match self.op_step {
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
                        let addr = self.direct_addr((self.oper as u8).wrapping_add(self.y));
                        let v = bus.read(addr);
                        self.x = v;
                        self.set_nz(v);
                        true
                    }
                }
            }
            0xFB => {
                // MOV Y,dp+X
                match self.op_step {
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
                        let addr = self.direct_addr((self.oper as u8).wrapping_add(self.x));
                        let v = bus.read(addr);
                        self.y = v;
                        self.set_nz(v);
                        true
                    }
                }
            }
            // ALU/CMP A,dp+X — fetch dp; idle; read [dp+X]; compute.
            0x14 | 0x34 | 0x54 | 0x74 | 0x94 | 0xB4 => match self.op_step {
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
                    let v = bus.read(self.direct_addr((self.oper as u8).wrapping_add(self.x)));
                    self.alu_a(self.op, v);
                    true
                }
            },

            // =============================================================
            // MOV dp+idx,reg — fetch dp; idle; dummy-read; write.
            // =============================================================
            0xD4 | 0xDB => match self.op_step {
                // MOV dp+X,A / MOV dp+X,Y
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.addr_lat = self.direct_addr((self.oper as u8).wrapping_add(self.x));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let _ = bus.read(self.addr_lat);
                    self.op_step = 3;
                    false
                }
                _ => {
                    let v = if self.op == 0xD4 { self.a } else { self.y };
                    bus.write(self.addr_lat, v);
                    true
                }
            },
            0xD9 => match self.op_step {
                // MOV dp+Y,X
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.addr_lat = self.direct_addr((self.oper as u8).wrapping_add(self.y));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let _ = bus.read(self.addr_lat);
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.x);
                    true
                }
            },

            // =============================================================
            // MOV reg,!abs — fetch16; read [abs].
            // =============================================================
            0xE5 | 0xE9 | 0xEC => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                _ => {
                    let v = bus.read(self.oper);
                    match self.op {
                        0xE5 => self.a = v,
                        0xE9 => self.x = v,
                        _ => self.y = v,
                    }
                    self.set_nz(v);
                    true
                }
            },
            // ALU/CMP A,!abs — fetch16; read [abs]; compute.
            0x05 | 0x25 | 0x45 | 0x65 | 0x85 | 0xA5 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                _ => {
                    let v = bus.read(self.oper);
                    self.alu_a(self.op, v);
                    true
                }
            },
            // CMP X,!abs / CMP Y,!abs — fetch16; read; compare.
            0x1E | 0x5E => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                _ => {
                    let mem = bus.read(self.oper);
                    let reg = if self.op == 0x1E { self.x } else { self.y };
                    self.cmp_u8(reg, mem);
                    true
                }
            },

            // =============================================================
            // MOV !abs,reg — fetch16; dummy-read; write.
            // =============================================================
            0xC5 | 0xC9 | 0xCC => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    let _ = bus.read(self.oper);
                    self.op_step = 3;
                    false
                }
                _ => {
                    let v = match self.op {
                        0xC5 => self.a,
                        0xC9 => self.x,
                        _ => self.y,
                    };
                    bus.write(self.oper, v);
                    true
                }
            },

            // =============================================================
            // MOV A,!abs+idx — fetch16; idle; read [abs+idx].
            // =============================================================
            0xF5 | 0xF6 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                _ => {
                    let idx = if self.op == 0xF5 { self.x } else { self.y };
                    let v = bus.read(self.oper.wrapping_add(u16::from(idx)));
                    self.a = v;
                    self.set_nz(v);
                    true
                }
            },
            // ALU/CMP A,!abs+X — fetch16; idle; read [abs+X]; compute.
            0x15 | 0x35 | 0x55 | 0x75 | 0x95 | 0xB5 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                _ => {
                    let v = bus.read(self.oper.wrapping_add(u16::from(self.x)));
                    self.alu_a(self.op, v);
                    true
                }
            },
            // ALU/CMP A,!abs+Y — fetch16; idle; read [abs+Y]; compute.
            0x16 | 0x36 | 0x56 | 0x76 | 0x96 | 0xB6 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                _ => {
                    let v = bus.read(self.oper.wrapping_add(u16::from(self.y)));
                    self.alu_a(self.op, v);
                    true
                }
            },

            // =============================================================
            // MOV !abs+idx,A — fetch16; idle; dummy-read; write.
            // =============================================================
            0xD5 | 0xD6 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    let idx = if self.op == 0xD5 { self.x } else { self.y };
                    self.addr_lat = self.oper.wrapping_add(u16::from(idx));
                    self.op_step = 3;
                    false
                }
                3 => {
                    let _ = bus.read(self.addr_lat);
                    self.op_step = 4;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.a);
                    true
                }
            },

            // =============================================================
            // ALU/CMP A,(X) — dummy-read PC; read [X]; compute.
            // =============================================================
            0x06 | 0x26 | 0x46 | 0x66 | 0x86 | 0xA6 => {
                if self.op_step == 0 {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                } else {
                    let v = bus.read(self.direct_addr(self.x));
                    self.alu_a(self.op, v);
                    true
                }
            }
            0xE6 => {
                // MOV A,(X) — dummy-read PC; read [X].
                if self.op_step == 0 {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                } else {
                    let v = bus.read(self.direct_addr(self.x));
                    self.a = v;
                    self.set_nz(v);
                    true
                }
            }
            0xBF => {
                // MOV A,(X)+ — dummy-read PC; read [X]; idle; X++.
                match self.op_step {
                    0 => {
                        self.dummy_read_pc(bus);
                        self.op_step = 1;
                        false
                    }
                    1 => {
                        let v = bus.read(self.direct_addr(self.x));
                        self.ptr_lat = u16::from(v);
                        self.op_step = 2;
                        false
                    }
                    _ => {
                        bus.idle();
                        let v = self.ptr_lat as u8;
                        self.a = v;
                        self.x = self.x.wrapping_add(1);
                        self.set_nz(v);
                        true
                    }
                }
            }
            0xC6 => {
                // MOV (X),A — dummy-read PC; dummy-read [X]; write [X].
                match self.op_step {
                    0 => {
                        self.dummy_read_pc(bus);
                        self.addr_lat = self.direct_addr(self.x);
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
                }
            }
            0xAF => {
                // MOV (X)+,A — dummy-read PC; idle; write [X]; X++.
                match self.op_step {
                    0 => {
                        self.dummy_read_pc(bus);
                        self.op_step = 1;
                        false
                    }
                    1 => {
                        bus.idle();
                        self.op_step = 2;
                        false
                    }
                    _ => {
                        bus.write(self.direct_addr(self.x), self.a);
                        self.x = self.x.wrapping_add(1);
                        true
                    }
                }
            }

            // =============================================================
            // ALU/CMP A,[dp+X] — fetch dp; idle; read lo; read hi; read tgt.
            // =============================================================
            0x07 | 0x27 | 0x47 | 0x67 | 0x87 | 0xA7 => match self.op_step {
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
                2 => {
                    let dpx = (self.oper as u8).wrapping_add(self.x);
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(dpx)));
                    self.op_step = 3;
                    false
                }
                3 => {
                    let dpx = (self.oper as u8).wrapping_add(self.x);
                    self.ptr_lat |= u16::from(bus.read(self.direct_addr(dpx.wrapping_add(1)))) << 8;
                    self.op_step = 4;
                    false
                }
                _ => {
                    let v = bus.read(self.ptr_lat);
                    self.alu_a(self.op, v);
                    true
                }
            },
            // ALU/CMP A,[dp]+Y — fetch dp; idle; read lo; read hi; read tgt.
            0x17 | 0x37 | 0x57 | 0x77 | 0x97 | 0xB7 => match self.op_step {
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
                2 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 3;
                    false
                }
                3 => {
                    let hi = bus.read(self.direct_addr((self.oper as u8).wrapping_add(1)));
                    self.ptr_lat |= u16::from(hi) << 8;
                    self.op_step = 4;
                    false
                }
                _ => {
                    let v = bus.read(self.ptr_lat.wrapping_add(u16::from(self.y)));
                    self.alu_a(self.op, v);
                    true
                }
            },

            // =============================================================
            // MOV A,[dp+X] — fetch dp; idle; read lo; read hi; read tgt.
            // =============================================================
            0xE7 => match self.op_step {
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
                2 => {
                    let dpx = (self.oper as u8).wrapping_add(self.x);
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(dpx)));
                    self.op_step = 3;
                    false
                }
                3 => {
                    let dpx = (self.oper as u8).wrapping_add(self.x);
                    self.ptr_lat |= u16::from(bus.read(self.direct_addr(dpx.wrapping_add(1)))) << 8;
                    self.op_step = 4;
                    false
                }
                _ => {
                    let v = bus.read(self.ptr_lat);
                    self.a = v;
                    self.set_nz(v);
                    true
                }
            },
            // MOV [dp+X],A — fetch dp; idle; read lo; read hi; dummy-read; write.
            0xC7 => match self.op_step {
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
                2 => {
                    let dpx = (self.oper as u8).wrapping_add(self.x);
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(dpx)));
                    self.op_step = 3;
                    false
                }
                3 => {
                    let dpx = (self.oper as u8).wrapping_add(self.x);
                    self.ptr_lat |= u16::from(bus.read(self.direct_addr(dpx.wrapping_add(1)))) << 8;
                    self.op_step = 4;
                    false
                }
                4 => {
                    let _ = bus.read(self.ptr_lat);
                    self.op_step = 5;
                    false
                }
                _ => {
                    bus.write(self.ptr_lat, self.a);
                    true
                }
            },
            // MOV A,[dp]+Y — fetch dp; idle; read lo; read hi; read tgt+Y.
            0xF7 => match self.op_step {
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
                2 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 3;
                    false
                }
                3 => {
                    let hi = bus.read(self.direct_addr((self.oper as u8).wrapping_add(1)));
                    self.ptr_lat |= u16::from(hi) << 8;
                    self.op_step = 4;
                    false
                }
                _ => {
                    let v = bus.read(self.ptr_lat.wrapping_add(u16::from(self.y)));
                    self.a = v;
                    self.set_nz(v);
                    true
                }
            },
            // MOV [dp]+Y,A — fetch dp; read lo; read hi; idle; dummy-read; write.
            0xD7 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let hi = bus.read(self.direct_addr((self.oper as u8).wrapping_add(1)));
                    self.ptr_lat |= u16::from(hi) << 8;
                    self.op_step = 3;
                    false
                }
                3 => {
                    bus.idle();
                    self.addr_lat = self.ptr_lat.wrapping_add(u16::from(self.y));
                    self.op_step = 4;
                    false
                }
                4 => {
                    let _ = bus.read(self.addr_lat);
                    self.op_step = 5;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.a);
                    true
                }
            },

            // =============================================================
            // RMW dp — fetch dp; read; (modify in place); write.
            // =============================================================
            0x0B | 0x2B | 0x4B | 0x6B | 0x8B | 0xAB => match self.op_step {
                0 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 1;
                    false
                }
                1 => {
                    let v = bus.read(self.addr_lat);
                    self.ptr_lat = u16::from(self.rmw(self.op, v));
                    self.op_step = 2;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.ptr_lat as u8);
                    true
                }
            },
            // RMW dp+X — fetch dp; idle; read; write.
            0x1B | 0x3B | 0x5B | 0x7B | 0x9B | 0xBB => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.addr_lat = self.direct_addr((self.oper as u8).wrapping_add(self.x));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let v = bus.read(self.addr_lat);
                    self.ptr_lat = u16::from(self.rmw(self.op, v));
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.ptr_lat as u8);
                    true
                }
            },
            // RMW !abs — fetch16; read; write.
            0x0C | 0x2C | 0x4C | 0x6C | 0x8C | 0xAC => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    let v = bus.read(self.oper);
                    self.ptr_lat = u16::from(self.rmw(self.op, v));
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.oper, self.ptr_lat as u8);
                    true
                }
            },

            // =============================================================
            // SET1 / CLR1 dp.bit — fetch dp; read; write.
            // =============================================================
            0x02 | 0x22 | 0x42 | 0x62 | 0x82 | 0xA2 | 0xC2 | 0xE2 | 0x12 | 0x32 | 0x52 | 0x72
            | 0x92 | 0xB2 | 0xD2 | 0xF2 => match self.op_step {
                0 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 1;
                    false
                }
                1 => {
                    let bit = (self.op >> 5) & 0x07;
                    let mem = bus.read(self.addr_lat);
                    self.ptr_lat = u16::from(if self.op & 0x10 == 0 {
                        mem | (1 << bit)
                    } else {
                        mem & !(1 << bit)
                    });
                    self.op_step = 2;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.ptr_lat as u8);
                    true
                }
            },

            // =============================================================
            // MOV dp,#imm — fetch imm; fetch dp; dummy-read; write.
            // =============================================================
            0x8F => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 2;
                    false
                }
                2 => {
                    let _ = bus.read(self.addr_lat);
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.oper as u8);
                    true
                }
            },
            // CMP dp,#imm — fetch imm; fetch dp; read; idle.
            0x78 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 2;
                    false
                }
                2 => {
                    let mem = bus.read(self.addr_lat);
                    let imm = self.oper as u8;
                    self.cmp_u8(mem, imm);
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.idle();
                    true
                }
            },
            // ALU dp,#imm (OR/AND/EOR/ADC/SBC) — fetch imm; fetch dp; read; write.
            0x18 | 0x38 | 0x58 | 0x98 | 0xB8 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 2;
                    false
                }
                2 => {
                    let mem = bus.read(self.addr_lat);
                    let imm = self.oper as u8;
                    self.ptr_lat = u16::from(self.alu_mem(self.op, mem, imm));
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.ptr_lat as u8);
                    true
                }
            },

            // =============================================================
            // ALU (dp),(dp) — fetch src; read src; fetch dst; read dst; write.
            // =============================================================
            0x09 | 0x29 | 0x49 | 0x89 | 0xA9 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let dst = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dst);
                    self.op_step = 3;
                    false
                }
                3 => {
                    let d = bus.read(self.addr_lat);
                    let s = self.ptr_lat as u8;
                    self.oper2 = u16::from(self.alu_mem(self.op, d, s));
                    self.op_step = 4;
                    false
                }
                _ => {
                    bus.write(self.addr_lat, self.oper2 as u8);
                    true
                }
            },
            // CMP (dp),(dp) — fetch src; read src; fetch dst; read dst; idle.
            0x69 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let dst = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dst);
                    self.op_step = 3;
                    false
                }
                3 => {
                    let d = bus.read(self.addr_lat);
                    let s = self.ptr_lat as u8;
                    self.cmp_u8(d, s);
                    self.op_step = 4;
                    false
                }
                _ => {
                    bus.idle();
                    true
                }
            },
            // MOV dp,dp — fetch src; read src; fetch dst; write dst.
            0xFA => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    self.oper2 = u16::from(self.fetch_u8(bus));
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.direct_addr(self.oper2 as u8), self.ptr_lat as u8);
                    true
                }
            },

            // =============================================================
            // ALU (X),(Y) — dummy-read PC; read [Y]; read [X]; write [X].
            // =============================================================
            0x19 | 0x39 | 0x59 | 0x99 | 0xB9 => match self.op_step {
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.y)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let lhs = bus.read(self.direct_addr(self.x));
                    let rhs = self.ptr_lat as u8;
                    self.oper2 = u16::from(self.alu_mem(self.op, lhs, rhs));
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.direct_addr(self.x), self.oper2 as u8);
                    true
                }
            },
            // CMP (X),(Y) — dummy-read PC; read [Y]; read [X]; idle.
            0x79 => match self.op_step {
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.y)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    let lhs = bus.read(self.direct_addr(self.x));
                    let rhs = self.ptr_lat as u8;
                    self.cmp_u8(lhs, rhs);
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.idle();
                    true
                }
            },

            // =============================================================
            // 16-bit word read ops on YA / dp word.
            // =============================================================
            0xBA => match self.op_step {
                // MOVW YA,dp — fetch; read lo; idle; read hi.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                _ => {
                    let hi = bus.read(self.direct_addr((self.oper as u8).wrapping_add(1)));
                    let lo = self.ptr_lat as u8;
                    self.a = lo;
                    self.y = hi;
                    let v16 = u16::from(lo) | (u16::from(hi) << 8);
                    self.psw.set(bit::Z, v16 == 0);
                    self.psw.set(bit::N, v16 & 0x8000 != 0);
                    true
                }
            },
            0x7A | 0x9A => match self.op_step {
                // ADDW/SUBW YA,dp — fetch; read lo; idle; read hi.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                _ => {
                    let hi = bus.read(self.direct_addr((self.oper as u8).wrapping_add(1)));
                    let mem = (self.ptr_lat & 0xFF) | (u16::from(hi) << 8);
                    let ya = u16::from(self.a) | (u16::from(self.y) << 8);
                    if self.op == 0x7A {
                        let sum = u32::from(ya) + u32::from(mem);
                        let result = sum as u16;
                        self.a = result as u8;
                        self.y = (result >> 8) as u8;
                        self.psw.set(bit::Z, result == 0);
                        self.psw.set(bit::N, result & 0x8000 != 0);
                        self.psw.set(bit::C, sum > 0xFFFF);
                        let v = ((ya ^ result) & (mem ^ result) & 0x8000) != 0;
                        self.psw.set(bit::V, v);
                        let h = ((ya & 0x0FFF) + (mem & 0x0FFF)) > 0x0FFF;
                        self.psw.set(bit::H, h);
                    } else {
                        let diff = u32::from(ya).wrapping_sub(u32::from(mem));
                        let result = diff as u16;
                        self.a = result as u8;
                        self.y = (result >> 8) as u8;
                        self.psw.set(bit::Z, result == 0);
                        self.psw.set(bit::N, result & 0x8000 != 0);
                        self.psw.set(bit::C, ya >= mem);
                        let v = ((ya ^ mem) & (ya ^ result) & 0x8000) != 0;
                        self.psw.set(bit::V, v);
                        let h = (ya & 0x0FFF) < (mem & 0x0FFF);
                        self.psw.set(bit::H, !h);
                    }
                    true
                }
            },
            0x5A => match self.op_step {
                // CMPW YA,dp — fetch; read lo; read hi.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                _ => {
                    let hi = bus.read(self.direct_addr((self.oper as u8).wrapping_add(1)));
                    let mem = (self.ptr_lat & 0xFF) | (u16::from(hi) << 8);
                    let ya = u16::from(self.a) | (u16::from(self.y) << 8);
                    let result = ya.wrapping_sub(mem);
                    self.psw.set(bit::Z, result == 0);
                    self.psw.set(bit::N, result & 0x8000 != 0);
                    self.psw.set(bit::C, ya >= mem);
                    true
                }
            },
            // INCW/DECW dp — fetch; read lo; write lo; read hi; write hi.
            0x3A | 0x1A => match self.op_step {
                0 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.oper = u16::from(dp);
                    self.op_step = 1;
                    false
                }
                1 => {
                    let lo = bus.read(self.addr_lat);
                    let adj = if self.op == 0x3A {
                        u16::from(lo).wrapping_add(1)
                    } else {
                        u16::from(lo).wrapping_sub(1)
                    };
                    self.ptr_lat = adj;
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.write(self.addr_lat, self.ptr_lat as u8);
                    self.op_step = 3;
                    false
                }
                3 => {
                    let hi_addr = self.direct_addr((self.oper as u8).wrapping_add(1));
                    let hi = bus.read(hi_addr);
                    self.ptr_lat = self.ptr_lat.wrapping_add(u16::from(hi) << 8);
                    self.op_step = 4;
                    false
                }
                _ => {
                    let hi_addr = self.direct_addr((self.oper as u8).wrapping_add(1));
                    bus.write(hi_addr, (self.ptr_lat >> 8) as u8);
                    self.psw.set(bit::Z, self.ptr_lat == 0);
                    self.psw.set(bit::N, self.ptr_lat & 0x8000 != 0);
                    true
                }
            },
            // MOVW dp,YA — fetch; dummy-read lo; write lo; write hi.
            0xDA => match self.op_step {
                0 => {
                    let dp = self.fetch_u8(bus);
                    self.oper = u16::from(dp);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 1;
                    false
                }
                1 => {
                    let _ = bus.read(self.addr_lat);
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.write(self.addr_lat, self.a);
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.write(self.direct_addr((self.oper as u8).wrapping_add(1)), self.y);
                    true
                }
            },

            // =============================================================
            // Bit-on-memory / carry ops.
            // =============================================================
            0x0A | 0x2A | 0x8A => match self.op_step {
                // OR1 / OR1-inv / EOR1 — fetch16; read; idle.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    let addr = self.oper & 0x1FFF;
                    let b = ((self.oper >> 13) & 0x07) as u8;
                    let bit_set = (bus.read(addr) >> b) & 1 != 0;
                    let c = self.psw.contains(bit::C);
                    let nc = match self.op {
                        0x0A => c | bit_set,
                        0x2A => c || !bit_set,
                        _ => c ^ bit_set,
                    };
                    self.psw.set(bit::C, nc);
                    self.op_step = 3;
                    false
                }
                _ => {
                    bus.idle();
                    true
                }
            },
            0x4A | 0x6A | 0xAA => match self.op_step {
                // AND1 / AND1-inv / MOV1 C,m.b — fetch16; read.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                _ => {
                    let addr = self.oper & 0x1FFF;
                    let b = ((self.oper >> 13) & 0x07) as u8;
                    let bit_set = (bus.read(addr) >> b) & 1 != 0;
                    let c = self.psw.contains(bit::C);
                    let nc = match self.op {
                        0x4A => c & bit_set,
                        0x6A => c && !bit_set,
                        _ => bit_set,
                    };
                    self.psw.set(bit::C, nc);
                    true
                }
            },
            0xCA => match self.op_step {
                // MOV1 m.b,C — fetch16; read; idle; write.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    let addr = self.oper & 0x1FFF;
                    self.addr_lat = addr;
                    self.ptr_lat = u16::from(bus.read(addr));
                    self.op_step = 3;
                    false
                }
                3 => {
                    bus.idle();
                    self.op_step = 4;
                    false
                }
                _ => {
                    let b = ((self.oper >> 13) & 0x07) as u8;
                    let mask = 1u8 << b;
                    let mem = self.ptr_lat as u8;
                    let v = if self.psw.contains(bit::C) {
                        mem | mask
                    } else {
                        mem & !mask
                    };
                    bus.write(self.addr_lat, v);
                    true
                }
            },
            0xEA => match self.op_step {
                // NOT1 m.b — fetch16; read; write.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    self.addr_lat = self.oper & 0x1FFF;
                    self.ptr_lat = u16::from(bus.read(self.addr_lat));
                    self.op_step = 3;
                    false
                }
                _ => {
                    let b = ((self.oper >> 13) & 0x07) as u8;
                    bus.write(self.addr_lat, (self.ptr_lat as u8) ^ (1u8 << b));
                    true
                }
            },

            // =============================================================
            // TSET1 / TCLR1 !abs — fetch16; read; dummy-read; write.
            // =============================================================
            0x0E | 0x4E => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    let mem = bus.read(self.oper);
                    self.ptr_lat = u16::from(mem);
                    let diff = self.a.wrapping_sub(mem);
                    self.set_nz(diff);
                    self.op_step = 3;
                    false
                }
                3 => {
                    let _ = bus.read(self.oper);
                    self.op_step = 4;
                    false
                }
                _ => {
                    let mem = self.ptr_lat as u8;
                    let v = if self.op == 0x0E {
                        mem | self.a
                    } else {
                        mem & !self.a
                    };
                    bus.write(self.oper, v);
                    true
                }
            },

            // =============================================================
            // Branches Bcc — fetch rel; [idle; idle] when taken.
            // =============================================================
            0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 | 0x2F => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    let cond = match self.op {
                        0x2F => true,
                        0x10 => !self.psw.contains(bit::N),
                        0x30 => self.psw.contains(bit::N),
                        0x50 => !self.psw.contains(bit::V),
                        0x70 => self.psw.contains(bit::V),
                        0x90 => !self.psw.contains(bit::C),
                        0xB0 => self.psw.contains(bit::C),
                        0xD0 => !self.psw.contains(bit::Z),
                        _ => self.psw.contains(bit::Z),
                    };
                    if cond {
                        self.branch_taken = true;
                        self.op_step = 1;
                        false
                    } else {
                        true
                    }
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
                    true
                }
            },

            // =============================================================
            // CBNE dp,rel — fetch dp; read; idle; fetch disp; [idle; idle].
            // =============================================================
            0x2E => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.oper2 = u16::from(self.fetch_u8(bus));
                    if self.a == self.ptr_lat as u8 {
                        true
                    } else {
                        self.branch_taken = true;
                        self.op_step = 4;
                        false
                    }
                }
                4 => {
                    bus.idle();
                    self.op_step = 5;
                    false
                }
                _ => {
                    bus.idle();
                    let rel = self.oper2 as u8 as i8;
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                    true
                }
            },
            // CBNE dp+X,rel — fetch dp; idle; read; idle; fetch disp; [idle;idle].
            0xDE => match self.op_step {
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
                2 => {
                    let addr = self.direct_addr((self.oper as u8).wrapping_add(self.x));
                    self.ptr_lat = u16::from(bus.read(addr));
                    self.op_step = 3;
                    false
                }
                3 => {
                    bus.idle();
                    self.op_step = 4;
                    false
                }
                4 => {
                    self.oper2 = u16::from(self.fetch_u8(bus));
                    if self.a == self.ptr_lat as u8 {
                        true
                    } else {
                        self.branch_taken = true;
                        self.op_step = 5;
                        false
                    }
                }
                5 => {
                    bus.idle();
                    self.op_step = 6;
                    false
                }
                _ => {
                    bus.idle();
                    let rel = self.oper2 as u8 as i8;
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                    true
                }
            },
            // BBS/BBC dp.bit,rel — fetch dp; read; idle; fetch disp; [idle;idle].
            0x03 | 0x23 | 0x43 | 0x63 | 0x83 | 0xA3 | 0xC3 | 0xE3 | 0x13 | 0x33 | 0x53 | 0x73
            | 0x93 | 0xB3 | 0xD3 | 0xF3 => match self.op_step {
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.direct_addr(self.oper as u8)));
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.oper2 = u16::from(self.fetch_u8(bus));
                    let bit = (self.op >> 5) & 0x07;
                    let is_set = self.ptr_lat as u8 & (1 << bit) != 0;
                    // even high nibble = BBS (branch if set); odd = BBC.
                    let take = if self.op & 0x10 == 0 { is_set } else { !is_set };
                    if take {
                        self.branch_taken = true;
                        self.op_step = 4;
                        false
                    } else {
                        true
                    }
                }
                4 => {
                    bus.idle();
                    self.op_step = 5;
                    false
                }
                _ => {
                    bus.idle();
                    let rel = self.oper2 as u8 as i8;
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                    true
                }
            },
            // DBNZ dp,rel — fetch dp; read; write; fetch disp; [idle;idle].
            0x6E => match self.op_step {
                0 => {
                    let dp = self.fetch_u8(bus);
                    self.addr_lat = self.direct_addr(dp);
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.ptr_lat = u16::from(bus.read(self.addr_lat).wrapping_sub(1));
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.write(self.addr_lat, self.ptr_lat as u8);
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.oper2 = u16::from(self.fetch_u8(bus));
                    if self.ptr_lat as u8 != 0 {
                        self.branch_taken = true;
                        self.op_step = 4;
                        false
                    } else {
                        true
                    }
                }
                4 => {
                    bus.idle();
                    self.op_step = 5;
                    false
                }
                _ => {
                    bus.idle();
                    let rel = self.oper2 as u8 as i8;
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                    true
                }
            },
            // DBNZ Y,rel — dummy-read PC; idle; fetch disp; [idle;idle].
            0xFE => match self.op_step {
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.y = self.y.wrapping_sub(1);
                    self.op_step = 2;
                    false
                }
                2 => {
                    self.oper2 = u16::from(self.fetch_u8(bus));
                    if self.y != 0 {
                        self.branch_taken = true;
                        self.op_step = 3;
                        false
                    } else {
                        true
                    }
                }
                3 => {
                    bus.idle();
                    self.op_step = 4;
                    false
                }
                _ => {
                    bus.idle();
                    let rel = self.oper2 as u8 as i8;
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                    true
                }
            },

            // =============================================================
            // Stack push/pop.
            // =============================================================
            0x2D | 0x4D | 0x6D | 0x0D => match self.op_step {
                // PUSH A/X/Y/PSW — dummy-read PC; push; idle.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    let v = match self.op {
                        0x2D => self.a,
                        0x4D => self.x,
                        0x6D => self.y,
                        _ => self.psw.0,
                    };
                    self.push_u8(bus, v);
                    self.op_step = 2;
                    false
                }
                _ => {
                    bus.idle();
                    true
                }
            },
            0xAE | 0xCE | 0xEE | 0x8E => match self.op_step {
                // POP A/X/Y/PSW — dummy-read PC; idle; pull.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.op_step = 2;
                    false
                }
                _ => {
                    let v = self.pop_u8(bus);
                    match self.op {
                        0xAE => self.a = v,
                        0xCE => self.x = v,
                        0xEE => self.y = v,
                        _ => self.psw.0 = v,
                    }
                    true
                }
            },

            // =============================================================
            // Calls / jumps / returns.
            // =============================================================
            0x5F => {
                // JMP !abs — fetch16.
                if self.op_step == 0 {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                } else {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.pc = self.oper;
                    true
                }
            }
            0x1F => match self.op_step {
                // JMP [!abs+X] — fetch16; idle; read lo; read hi.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                3 => {
                    let ptr = self.oper.wrapping_add(u16::from(self.x));
                    self.ptr_lat = u16::from(bus.read(ptr));
                    self.op_step = 4;
                    false
                }
                _ => {
                    let ptr = self.oper.wrapping_add(u16::from(self.x));
                    let hi = bus.read(ptr.wrapping_add(1));
                    self.pc = (self.ptr_lat & 0xFF) | (u16::from(hi) << 8);
                    true
                }
            },
            0x3F => match self.op_step {
                // CALL !abs — fetch16; idle; push hi; push lo; idle; idle.
                0 => {
                    self.oper = u16::from(self.fetch_u8(bus));
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.oper |= u16::from(self.fetch_u8(bus)) << 8;
                    self.op_step = 2;
                    false
                }
                2 => {
                    bus.idle();
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.push_u8(bus, (self.pc >> 8) as u8);
                    self.op_step = 4;
                    false
                }
                4 => {
                    self.push_u8(bus, self.pc as u8);
                    self.op_step = 5;
                    false
                }
                5 => {
                    bus.idle();
                    self.op_step = 6;
                    false
                }
                _ => {
                    bus.idle();
                    self.pc = self.oper;
                    true
                }
            },
            0x4F => match self.op_step {
                // PCALL — fetch; idle; push hi; push lo; idle.
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
                2 => {
                    self.push_u8(bus, (self.pc >> 8) as u8);
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.push_u8(bus, self.pc as u8);
                    self.op_step = 4;
                    false
                }
                _ => {
                    bus.idle();
                    self.pc = 0xFF00 | (self.oper & 0xFF);
                    true
                }
            },
            // TCALL N — dummy-read PC; idle; push hi; push lo; idle; read vec lo; read vec hi.
            0x01 | 0x11 | 0x21 | 0x31 | 0x41 | 0x51 | 0x61 | 0x71 | 0x81 | 0x91 | 0xA1 | 0xB1
            | 0xC1 | 0xD1 | 0xE1 | 0xF1 => match self.op_step {
                0 => {
                    self.dummy_read_pc(bus);
                    let n = (self.op >> 4) & 0x0F;
                    self.addr_lat = 0xFFDE_u16.wrapping_sub(u16::from(n) * 2);
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.op_step = 2;
                    false
                }
                2 => {
                    self.push_u8(bus, (self.pc >> 8) as u8);
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.push_u8(bus, self.pc as u8);
                    self.op_step = 4;
                    false
                }
                4 => {
                    bus.idle();
                    self.op_step = 5;
                    false
                }
                5 => {
                    self.ptr_lat = u16::from(bus.read(self.addr_lat));
                    self.op_step = 6;
                    false
                }
                _ => {
                    let hi = bus.read(self.addr_lat.wrapping_add(1));
                    self.pc = (self.ptr_lat & 0xFF) | (u16::from(hi) << 8);
                    true
                }
            },
            0x6F => match self.op_step {
                // RET — dummy-read PC; idle; pull lo; pull hi.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.op_step = 2;
                    false
                }
                2 => {
                    self.ptr_lat = u16::from(self.pop_u8(bus));
                    self.op_step = 3;
                    false
                }
                _ => {
                    let hi = self.pop_u8(bus);
                    self.pc = (self.ptr_lat & 0xFF) | (u16::from(hi) << 8);
                    true
                }
            },
            0x7F => match self.op_step {
                // RETI — dummy-read PC; idle; pull P; pull lo; pull hi.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    bus.idle();
                    self.op_step = 2;
                    false
                }
                2 => {
                    self.psw.0 = self.pop_u8(bus);
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.ptr_lat = u16::from(self.pop_u8(bus));
                    self.op_step = 4;
                    false
                }
                _ => {
                    let hi = self.pop_u8(bus);
                    self.pc = (self.ptr_lat & 0xFF) | (u16::from(hi) << 8);
                    true
                }
            },
            0x0F => match self.op_step {
                // BRK — dummy-read PC; push hi; push lo; push P; idle;
                // read vec lo; read vec hi.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 => {
                    self.push_u8(bus, (self.pc >> 8) as u8);
                    self.op_step = 2;
                    false
                }
                2 => {
                    self.push_u8(bus, self.pc as u8);
                    self.op_step = 3;
                    false
                }
                3 => {
                    self.push_u8(bus, self.psw.0);
                    self.op_step = 4;
                    false
                }
                4 => {
                    bus.idle();
                    self.op_step = 5;
                    false
                }
                5 => {
                    self.ptr_lat = u16::from(bus.read(0xFFDE));
                    self.op_step = 6;
                    false
                }
                _ => {
                    let hi = bus.read(0xFFDF);
                    self.pc = (self.ptr_lat & 0xFF) | (u16::from(hi) << 8);
                    self.psw.insert(bit::B);
                    self.psw.remove(bit::I);
                    true
                }
            },

            // =============================================================
            // MUL / DIV / XCN / DAA / DAS — implied multi-idle ops.
            // =============================================================
            0xCF => match self.op_step {
                // MUL YA — dummy-read PC; 7 idle.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1..=6 => {
                    bus.idle();
                    self.op_step += 1;
                    false
                }
                _ => {
                    bus.idle();
                    let product = u16::from(self.y) * u16::from(self.a);
                    self.a = product as u8;
                    self.y = (product >> 8) as u8;
                    let v = self.y;
                    self.set_nz(v);
                    true
                }
            },
            0x9E => match self.op_step {
                // DIV YA,X — dummy-read PC; 10 idle.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1..=9 => {
                    bus.idle();
                    self.op_step += 1;
                    false
                }
                _ => {
                    bus.idle();
                    let ya = u16::from(self.a) | (u16::from(self.y) << 8);
                    let x = u16::from(self.x);
                    let y = u16::from(self.y);
                    self.psw.set(bit::H, (y & 0x0F) >= (x & 0x0F));
                    self.psw.set(bit::V, y >= x);
                    if y < (x << 1) {
                        self.a = (ya / x) as u8;
                        self.y = (ya % x) as u8;
                    } else {
                        let ya = i32::from(ya);
                        let x = i32::from(x);
                        self.a = (255 - (ya - (x << 9)) / (256 - x)) as u8;
                        self.y = (x + (ya - (x << 9)) % (256 - x)) as u8;
                    }
                    let a = self.a;
                    self.set_nz(a);
                    true
                }
            },
            0x9F => match self.op_step {
                // XCN A — dummy-read PC; 3 idle; swap.
                0 => {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                }
                1 | 2 => {
                    bus.idle();
                    self.op_step += 1;
                    false
                }
                _ => {
                    bus.idle();
                    self.a = self.a.rotate_left(4);
                    let v = self.a;
                    self.set_nz(v);
                    true
                }
            },
            0xDF => {
                // DAA — dummy-read PC; idle; adjust.
                if self.op_step == 0 {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                } else {
                    bus.idle();
                    if self.psw.contains(bit::C) || self.a > 0x99 {
                        self.a = self.a.wrapping_add(0x60);
                        self.psw.insert(bit::C);
                    }
                    if self.psw.contains(bit::H) || (self.a & 0x0F) > 0x09 {
                        self.a = self.a.wrapping_add(0x06);
                    }
                    let v = self.a;
                    self.set_nz(v);
                    true
                }
            }
            0xBE => {
                // DAS — dummy-read PC; idle; adjust.
                if self.op_step == 0 {
                    self.dummy_read_pc(bus);
                    self.op_step = 1;
                    false
                } else {
                    bus.idle();
                    if !self.psw.contains(bit::C) || self.a > 0x99 {
                        self.a = self.a.wrapping_sub(0x60);
                        self.psw.remove(bit::C);
                    }
                    if !self.psw.contains(bit::H) || (self.a & 0x0F) > 0x09 {
                        self.a = self.a.wrapping_sub(0x06);
                    }
                    let v = self.a;
                    self.set_nz(v);
                    true
                }
            }
        }
    }

    /// Apply an ALU op against A using the value `v`, matching the
    /// atomic A,operand handlers (OR/AND/EOR/CMP/ADC/SBC selected by
    /// the opcode's low nibble: $4/$5/$6/$7/$d+X etc. share an op
    /// family by high nibble). Sets the same flags as the atomic core.
    fn alu_a(&mut self, op: u8, v: u8) {
        match op & 0xF0 {
            0x00 | 0x10 => {
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x20 | 0x30 => {
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x40 | 0x50 => {
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x60 | 0x70 => {
                let a = self.a;
                self.cmp_u8(a, v);
            }
            0x80 | 0x90 => {
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            _ => {
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
        }
    }

    /// Apply an ALU op between two memory operands `(lhs, rhs)` and
    /// return the result, matching the atomic `(dp),(dp)` / `(X),(Y)` /
    /// `dp,#imm` handlers. OR/AND/EOR set N/Z here; ADC/SBC set their
    /// full flag set inside `adc_u8`/`sbc_u8`.
    fn alu_mem(&mut self, op: u8, lhs: u8, rhs: u8) -> u8 {
        match op & 0xF0 {
            0x00 | 0x10 => {
                let v = lhs | rhs;
                self.set_nz(v);
                v
            }
            0x20 | 0x30 => {
                let v = lhs & rhs;
                self.set_nz(v);
                v
            }
            0x40 | 0x50 => {
                let v = lhs ^ rhs;
                self.set_nz(v);
                v
            }
            0x80 | 0x90 => self.adc_u8(lhs, rhs),
            _ => self.sbc_u8(lhs, rhs),
        }
    }

    /// Read-modify-write transform for a single memory byte (ASL/ROL/
    /// LSR/ROR/DEC/INC) selected by opcode high nibble, updating C/N/Z
    /// exactly like the atomic RMW handlers.
    fn rmw(&mut self, op: u8, v: u8) -> u8 {
        let r = match op & 0xF0 {
            0x00 | 0x10 => {
                // ASL
                self.psw.set(bit::C, v & 0x80 != 0);
                v << 1
            }
            0x20 | 0x30 => {
                // ROL
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x80 != 0);
                (v << 1) | c_in
            }
            0x40 | 0x50 => {
                // LSR
                self.psw.set(bit::C, v & 0x01 != 0);
                v >> 1
            }
            0x60 | 0x70 => {
                // ROR
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x01 != 0);
                (v >> 1) | (c_in << 7)
            }
            0x80 | 0x90 => v.wrapping_sub(1), // DEC
            _ => v.wrapping_add(1),           // INC
        };
        self.set_nz(r);
        r
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

    // ------------------------------------------------------------------
    // Randomized differential — THE gate for Stage 2 opcode porting.
    //
    // For every ported opcode, run many random initial states (regs +
    // all flags + full 64 KB RAM + random operand bytes) through both the
    // atomic `step()` (Tom-Harte-validated ground truth) and the
    // cycle-stepped `step_instruction()`, asserting identical cycle count,
    // bus access trace, architectural registers, and memory. Add a newly
    // ported opcode to `PORTED` and this proves it byte-/cycle-exact.
    // ------------------------------------------------------------------

    /// Opcodes ported to `execute_cycle`. Extend as each group lands.
    /// All 256 opcodes are now ported (SLEEP $EF / STOP $FF excluded —
    /// they halt the core, so the cycle-stepped halt window is modelled
    /// separately in `step_cycle` and is not directly comparable to the
    /// atomic `step()`'s instantaneous flag-set return).
    const PORTED: &[u8] = &{
        let mut a = [0u8; 254];
        let mut i = 0usize;
        let mut op = 0usize;
        while op < 256 {
            if op != 0xEF && op != 0xFF {
                a[i] = op as u8;
                i += 1;
            }
            op += 1;
        }
        a
    };

    fn lcg(s: &mut u64) -> u8 {
        *s = s
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*s >> 56) as u8
    }

    /// Fresh CPU + 64 KB random RAM from `seed`, with `op` + 3 random
    /// operand bytes at PC = `0x0200`.
    fn rand_state(op: u8, seed: u64) -> (Spc700, RecBus) {
        let mut s = seed;
        let mut bus = RecBus::new();
        for b in &mut bus.mem {
            *b = lcg(&mut s);
        }
        let mut cpu = Spc700::new();
        cpu.a = lcg(&mut s);
        cpu.x = lcg(&mut s);
        cpu.y = lcg(&mut s);
        cpu.sp = lcg(&mut s);
        cpu.psw = crate::flags::Psw(lcg(&mut s));
        cpu.pc = 0x0200;
        bus.mem[0x0200] = op;
        bus.mem[0x0201] = lcg(&mut s);
        bus.mem[0x0202] = lcg(&mut s);
        bus.mem[0x0203] = lcg(&mut s);
        (cpu, bus)
    }

    #[test]
    fn differential_all_ported_opcodes() {
        for &op in PORTED {
            for trial in 0..256u64 {
                let seed = (u64::from(op) << 40) ^ trial.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                let (mut ca, mut ba) = rand_state(op, seed);
                let (mut cb, mut bb) = rand_state(op, seed);
                let cyc_a = u32::from(ca.step(&mut ba));
                let cyc_b = cb.step_instruction(&mut bb);
                assert_eq!(cyc_a, cyc_b, "op ${op:02X} trial {trial}: cycle count");
                assert_eq!(ba.trace, bb.trace, "op ${op:02X} trial {trial}: bus trace");
                assert_eq!(
                    (ca.a, ca.x, ca.y, ca.sp, ca.pc, ca.psw.0),
                    (cb.a, cb.x, cb.y, cb.sp, cb.pc, cb.psw.0),
                    "op ${op:02X} trial {trial}: registers"
                );
                assert_eq!(ba.mem[..], bb.mem[..], "op ${op:02X} trial {trial}: memory");
            }
        }
    }
}
