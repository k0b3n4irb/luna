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
            0xF7 => {
                // MOV A,[dp]+Y — indirect indexed Y load.
                let dp = self.fetch_u8(bus);
                let plo = bus.read(self.direct_addr(dp));
                let phi = bus.read(self.direct_addr(dp.wrapping_add(1)));
                let ptr = u16::from(plo) | (u16::from(phi) << 8);
                let target = ptr.wrapping_add(u16::from(self.y));
                let v = bus.read(target);
                self.a = v;
                self.set_nz(v);
            }
            0xC7 => {
                // MOV [dp+X],A — indirect-indexed-X store. Pointer at
                // direct_addr(dp+X) / direct_addr(dp+X+1).
                let dp = self.fetch_u8(bus);
                let dpx = dp.wrapping_add(self.x);
                let plo = bus.read(self.direct_addr(dpx));
                let phi = bus.read(self.direct_addr(dpx.wrapping_add(1)));
                let target = u16::from(plo) | (u16::from(phi) << 8);
                bus.write(target, self.a);
            }
            0xE7 => {
                // MOV A,[dp+X] — indirect-indexed-X load.
                let dp = self.fetch_u8(bus);
                let dpx = dp.wrapping_add(self.x);
                let plo = bus.read(self.direct_addr(dpx));
                let phi = bus.read(self.direct_addr(dpx.wrapping_add(1)));
                let target = u16::from(plo) | (u16::from(phi) << 8);
                let v = bus.read(target);
                self.a = v;
                self.set_nz(v);
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
            // 16-bit word ops on YA / direct-page word
            // ---------------------------------------------------------
            0x7A => {
                // ADDW YA,dp — YA += (dp+1):(dp). Sets N/V/H/Z/C.
                let dp = self.fetch_u8(bus);
                let lo = bus.read(self.direct_addr(dp));
                let hi = bus.read(self.direct_addr(dp.wrapping_add(1)));
                let mem = u16::from(lo) | (u16::from(hi) << 8);
                let ya = u16::from(self.a) | (u16::from(self.y) << 8);
                let sum = u32::from(ya) + u32::from(mem);
                let result = sum as u16;
                self.a = result as u8;
                self.y = (result >> 8) as u8;
                self.psw.set(bit::Z, result == 0);
                self.psw.set(bit::N, result & 0x8000 != 0);
                self.psw.set(bit::C, sum > 0xFFFF);
                let v = ((ya ^ result) & (mem ^ result) & 0x8000) != 0;
                self.psw.set(bit::V, v);
                // Half-carry from bit 11 → bit 12 (per SPC700 spec for 16-bit ops).
                let h = ((ya & 0x0FFF) + (mem & 0x0FFF)) > 0x0FFF;
                self.psw.set(bit::H, h);
            }
            0x9A => {
                // SUBW YA,dp — YA -= (dp+1):(dp). Sets N/V/H/Z/C.
                let dp = self.fetch_u8(bus);
                let lo = bus.read(self.direct_addr(dp));
                let hi = bus.read(self.direct_addr(dp.wrapping_add(1)));
                let mem = u16::from(lo) | (u16::from(hi) << 8);
                let ya = u16::from(self.a) | (u16::from(self.y) << 8);
                let diff = u32::from(ya).wrapping_sub(u32::from(mem));
                let result = diff as u16;
                self.a = result as u8;
                self.y = (result >> 8) as u8;
                self.psw.set(bit::Z, result == 0);
                self.psw.set(bit::N, result & 0x8000 != 0);
                // SPC700 SUBW: C set when no borrow (ya >= mem).
                self.psw.set(bit::C, ya >= mem);
                let v = ((ya ^ mem) & (ya ^ result) & 0x8000) != 0;
                self.psw.set(bit::V, v);
                let h = (ya & 0x0FFF) < (mem & 0x0FFF);
                self.psw.set(bit::H, !h);
            }
            0x5A => {
                // CMPW YA,dp — compare 16-bit YA with (dp+1):(dp).
                let dp = self.fetch_u8(bus);
                let lo = bus.read(self.direct_addr(dp));
                let hi = bus.read(self.direct_addr(dp.wrapping_add(1)));
                let mem = u16::from(lo) | (u16::from(hi) << 8);
                let ya = u16::from(self.a) | (u16::from(self.y) << 8);
                let result = ya.wrapping_sub(mem);
                self.psw.set(bit::Z, result == 0);
                self.psw.set(bit::N, result & 0x8000 != 0);
                self.psw.set(bit::C, ya >= mem);
            }
            0x3A => {
                // INCW dp — (dp+1):(dp) += 1.
                let dp = self.fetch_u8(bus);
                let lo_addr = self.direct_addr(dp);
                let hi_addr = self.direct_addr(dp.wrapping_add(1));
                let v = (u16::from(bus.read(lo_addr)) | (u16::from(bus.read(hi_addr)) << 8))
                    .wrapping_add(1);
                bus.write(lo_addr, v as u8);
                bus.write(hi_addr, (v >> 8) as u8);
                self.psw.set(bit::Z, v == 0);
                self.psw.set(bit::N, v & 0x8000 != 0);
            }
            0x1A => {
                // DECW dp — (dp+1):(dp) -= 1.
                let dp = self.fetch_u8(bus);
                let lo_addr = self.direct_addr(dp);
                let hi_addr = self.direct_addr(dp.wrapping_add(1));
                let v = (u16::from(bus.read(lo_addr)) | (u16::from(bus.read(hi_addr)) << 8))
                    .wrapping_sub(1);
                bus.write(lo_addr, v as u8);
                bus.write(hi_addr, (v >> 8) as u8);
                self.psw.set(bit::Z, v == 0);
                self.psw.set(bit::N, v & 0x8000 != 0);
            }

            // ---------------------------------------------------------
            // Shifts / inc / dec on direct page and absolute memory.
            // Each does read-modify-write and updates N/Z (plus C for
            // shifts).
            // ---------------------------------------------------------
            0x0B => {
                // ASL dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                self.psw.set(bit::C, v & 0x80 != 0);
                let v = v << 1;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x1B => {
                // ASL dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr);
                self.psw.set(bit::C, v & 0x80 != 0);
                let v = v << 1;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x0C => {
                // ASL !abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.psw.set(bit::C, v & 0x80 != 0);
                let v = v << 1;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x4B => {
                // LSR dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                self.psw.set(bit::C, v & 0x01 != 0);
                let v = v >> 1;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x5B => {
                // LSR dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr);
                self.psw.set(bit::C, v & 0x01 != 0);
                let v = v >> 1;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x4C => {
                // LSR !abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.psw.set(bit::C, v & 0x01 != 0);
                let v = v >> 1;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x2B => {
                // ROL dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x80 != 0);
                let v = (v << 1) | c_in;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x3B => {
                // ROL dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr);
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x80 != 0);
                let v = (v << 1) | c_in;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x2C => {
                // ROL !abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x80 != 0);
                let v = (v << 1) | c_in;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x6B => {
                // ROR dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x01 != 0);
                let v = (v >> 1) | (c_in << 7);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x7B => {
                // ROR dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr);
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x01 != 0);
                let v = (v >> 1) | (c_in << 7);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x6C => {
                // ROR !abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, v & 0x01 != 0);
                let v = (v >> 1) | (c_in << 7);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x8B => {
                // DEC dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr).wrapping_sub(1);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x9B => {
                // DEC dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr).wrapping_sub(1);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x8C => {
                // DEC !abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr).wrapping_sub(1);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0xBB => {
                // INC dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr).wrapping_add(1);
                bus.write(addr, v);
                self.set_nz(v);
            }
            0xAC => {
                // INC !abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr).wrapping_add(1);
                bus.write(addr, v);
                self.set_nz(v);
            }

            // ---------------------------------------------------------
            // Shifts on accumulator
            // ---------------------------------------------------------
            0x1C => {
                // ASL A — shift A left, bit 7 → C.
                let a = self.a;
                self.psw.set(bit::C, a & 0x80 != 0);
                self.a = a << 1;
                let v = self.a;
                self.set_nz(v);
            }
            0x5C => {
                // LSR A — shift A right, bit 0 → C.
                let a = self.a;
                self.psw.set(bit::C, a & 0x01 != 0);
                self.a = a >> 1;
                let v = self.a;
                self.set_nz(v);
            }
            0x3C => {
                // ROL A — rotate left through C.
                let a = self.a;
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, a & 0x80 != 0);
                self.a = (a << 1) | c_in;
                let v = self.a;
                self.set_nz(v);
            }
            0x7C => {
                // ROR A — rotate right through C.
                let a = self.a;
                let c_in = u8::from(self.psw.contains(bit::C));
                self.psw.set(bit::C, a & 0x01 != 0);
                self.a = (a >> 1) | (c_in << 7);
                let v = self.a;
                self.set_nz(v);
            }

            // ---------------------------------------------------------
            // Arithmetic / logical on A with dp / abs operand
            // ---------------------------------------------------------
            0x04 => {
                // OR A,dp
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp));
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x05 => {
                // OR A,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x24 => {
                // AND A,dp
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp));
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x25 => {
                // AND A,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x44 => {
                // EOR A,dp
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp));
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x45 => {
                // EOR A,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x65 => {
                // CMP A,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                let a = self.a;
                self.cmp_u8(a, v);
            }
            0x75 => {
                // CMP A,!abs+X
                let base = self.fetch_u16(bus);
                let addr = base.wrapping_add(u16::from(self.x));
                let v = bus.read(addr);
                let a = self.a;
                self.cmp_u8(a, v);
            }
            0x76 => {
                // CMP A,!abs+Y
                let base = self.fetch_u16(bus);
                let addr = base.wrapping_add(u16::from(self.y));
                let v = bus.read(addr);
                let a = self.a;
                self.cmp_u8(a, v);
            }
            0x74 => {
                // CMP A,dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr);
                let a = self.a;
                self.cmp_u8(a, v);
            }
            // Indexed-DP MOV of X / Y
            0xD8 => {
                // MOV dp,X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                bus.write(addr, self.x);
            }
            0xD9 => {
                // MOV dp+Y,X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.y));
                bus.write(addr, self.x);
            }
            0xDB => {
                // MOV dp+X,Y
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                bus.write(addr, self.y);
            }
            0xF8 => {
                // MOV X,dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                self.x = v;
                self.set_nz(v);
            }
            0xF9 => {
                // MOV X,dp+Y
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.y));
                let v = bus.read(addr);
                self.x = v;
                self.set_nz(v);
            }
            0xFB => {
                // MOV Y,dp+X
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp.wrapping_add(self.x));
                let v = bus.read(addr);
                self.y = v;
                self.set_nz(v);
            }
            0x84 => {
                // ADC A,dp
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp));
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0x85 => {
                // ADC A,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0xA4 => {
                // SBC A,dp
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp));
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
            0xA5 => {
                // SBC A,!abs
                let addr = self.fetch_u16(bus);
                let v = bus.read(addr);
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
            // OR/AND/EOR/ADC/SBC A,dp+X
            0x14 => {
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp.wrapping_add(self.x)));
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x34 => {
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp.wrapping_add(self.x)));
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x54 => {
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp.wrapping_add(self.x)));
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x94 => {
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp.wrapping_add(self.x)));
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0xB4 => {
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp.wrapping_add(self.x)));
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
            // OR/AND/EOR/ADC/SBC A,!abs+X
            0x15 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.x)));
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x35 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.x)));
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x55 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.x)));
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x95 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.x)));
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0xB5 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.x)));
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
            // OR/AND/EOR/ADC/SBC A,!abs+Y
            0x16 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.y)));
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x36 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.y)));
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x56 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.y)));
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x96 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.y)));
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0xB6 => {
                let base = self.fetch_u16(bus);
                let v = bus.read(base.wrapping_add(u16::from(self.y)));
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
            // OR/AND/EOR/ADC/SBC A,(X) — register-indirect
            0x06 => {
                let v = bus.read(self.direct_addr(self.x));
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x26 => {
                let v = bus.read(self.direct_addr(self.x));
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x46 => {
                let v = bus.read(self.direct_addr(self.x));
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x66 => {
                // CMP A,(X)
                let v = bus.read(self.direct_addr(self.x));
                let a = self.a;
                self.cmp_u8(a, v);
            }
            0x86 => {
                let v = bus.read(self.direct_addr(self.x));
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0xA6 => {
                let v = bus.read(self.direct_addr(self.x));
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
            // OR/AND/EOR/CMP/ADC/SBC A,[dp+X]
            0x07 => {
                let v = self.read_indirect_x(bus);
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x27 => {
                let v = self.read_indirect_x(bus);
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x47 => {
                let v = self.read_indirect_x(bus);
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x67 => {
                let v = self.read_indirect_x(bus);
                let a = self.a;
                self.cmp_u8(a, v);
            }
            0x87 => {
                let v = self.read_indirect_x(bus);
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0xA7 => {
                let v = self.read_indirect_x(bus);
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }
            // OR/AND/EOR/CMP/ADC/SBC A,[dp]+Y
            0x17 => {
                let v = self.read_indirect_y(bus);
                self.a |= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x37 => {
                let v = self.read_indirect_y(bus);
                self.a &= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x57 => {
                let v = self.read_indirect_y(bus);
                self.a ^= v;
                let r = self.a;
                self.set_nz(r);
            }
            0x77 => {
                let v = self.read_indirect_y(bus);
                let a = self.a;
                self.cmp_u8(a, v);
            }
            0x97 => {
                let v = self.read_indirect_y(bus);
                let a = self.a;
                self.a = self.adc_u8(a, v);
            }
            0xB7 => {
                let v = self.read_indirect_y(bus);
                let a = self.a;
                self.a = self.sbc_u8(a, v);
            }

            // ---------------------------------------------------------
            // Memory-to-memory ops (dp,dp). Object-code order is
            // `op src_dp dst_dp` — the assembler syntax reads "OR
            // (dst), (src)" so the read happens at the *first* byte
            // and the write at the second.
            // ---------------------------------------------------------
            0x09 => {
                // OR (dp),(dp)
                let src = self.fetch_u8(bus);
                let dst = self.fetch_u8(bus);
                let s = bus.read(self.direct_addr(src));
                let d_addr = self.direct_addr(dst);
                let v = bus.read(d_addr) | s;
                bus.write(d_addr, v);
                self.set_nz(v);
            }
            0x29 => {
                // AND (dp),(dp)
                let src = self.fetch_u8(bus);
                let dst = self.fetch_u8(bus);
                let s = bus.read(self.direct_addr(src));
                let d_addr = self.direct_addr(dst);
                let v = bus.read(d_addr) & s;
                bus.write(d_addr, v);
                self.set_nz(v);
            }
            0x49 => {
                // EOR (dp),(dp)
                let src = self.fetch_u8(bus);
                let dst = self.fetch_u8(bus);
                let s = bus.read(self.direct_addr(src));
                let d_addr = self.direct_addr(dst);
                let v = bus.read(d_addr) ^ s;
                bus.write(d_addr, v);
                self.set_nz(v);
            }
            0x69 => {
                // CMP (dp),(dp)
                let src = self.fetch_u8(bus);
                let dst = self.fetch_u8(bus);
                let s = bus.read(self.direct_addr(src));
                let d = bus.read(self.direct_addr(dst));
                self.cmp_u8(d, s);
            }
            0x89 => {
                // ADC (dp),(dp)
                let src = self.fetch_u8(bus);
                let dst = self.fetch_u8(bus);
                let s = bus.read(self.direct_addr(src));
                let d_addr = self.direct_addr(dst);
                let d = bus.read(d_addr);
                let v = self.adc_u8(d, s);
                bus.write(d_addr, v);
            }
            0xA9 => {
                // SBC (dp),(dp)
                let src = self.fetch_u8(bus);
                let dst = self.fetch_u8(bus);
                let s = bus.read(self.direct_addr(src));
                let d_addr = self.direct_addr(dst);
                let d = bus.read(d_addr);
                let v = self.sbc_u8(d, s);
                bus.write(d_addr, v);
            }
            0x18 => {
                // OR dp,#imm — `op imm dp` order.
                let imm = self.fetch_u8(bus);
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr) | imm;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x38 => {
                // AND dp,#imm
                let imm = self.fetch_u8(bus);
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr) & imm;
                bus.write(addr, v);
                self.set_nz(v);
            }
            0x58 => {
                // EOR dp,#imm
                let imm = self.fetch_u8(bus);
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr) ^ imm;
                bus.write(addr, v);
                self.set_nz(v);
            }

            // ---------------------------------------------------------
            // CBNE — compare A to dp, branch if not equal.
            // ---------------------------------------------------------
            0x2E => {
                // CBNE dp,rel
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp));
                let rel = self.fetch_u8(bus) as i8;
                if self.a != v {
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                }
            }
            0xDE => {
                // CBNE dp+X,rel
                let dp = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(dp.wrapping_add(self.x)));
                let rel = self.fetch_u8(bus) as i8;
                if self.a != v {
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                }
            }

            // ---------------------------------------------------------
            // PCALL — page call to $FF00 + imm
            // ---------------------------------------------------------
            0x4F => {
                let target_lo = self.fetch_u8(bus);
                let return_pc = self.pc;
                self.push_u8(bus, (return_pc >> 8) as u8);
                self.push_u8(bus, return_pc as u8);
                self.pc = 0xFF00 | u16::from(target_lo);
            }

            // ---------------------------------------------------------
            // MUL / DIV
            // ---------------------------------------------------------
            0xCF => {
                // MUL YA — 16-bit unsigned multiply: YA = Y * A.
                let product = u16::from(self.y) * u16::from(self.a);
                self.a = product as u8;
                self.y = (product >> 8) as u8;
                // N/Z computed on Y after the operation (per spec).
                let v = self.y;
                self.set_nz(v);
            }
            0x9E => {
                // DIV YA,X — unsigned 16/8 division.
                //   A = YA / X (quotient, low byte)
                //   Y = YA % X (remainder)
                // Real SPC700 quirks: the result can overflow when
                // Y >= X (sets V). Behaviour for X == 0 is hardware-
                // specific; we mirror the common emulator convention
                // of returning (A=$FF, Y=A) and setting V.
                let ya = u16::from(self.a) | (u16::from(self.y) << 8);
                let x = u16::from(self.x);
                match ya.checked_div(x) {
                    Some(q) => {
                        let r = ya % x;
                        self.psw.set(bit::V, q > 0xFF);
                        self.a = q as u8;
                        self.y = r as u8;
                    }
                    None => {
                        // X = 0 on real HW gives undefined-but-
                        // observed (A = $FF, Y = $FF) with V set.
                        self.a = 0xFF;
                        self.y = 0xFF;
                        self.psw.insert(bit::V);
                    }
                }
                self.psw.set(bit::H, (self.y & 0x0F) >= (self.x & 0x0F));
                let a = self.a;
                self.set_nz(a);
            }

            // ---------------------------------------------------------
            // Bit-set / bit-clear on direct page (SET1 / CLR1).
            // Opcode bits 7..4 = $0,$2,$4,$6,$8,$A,$C,$E → SET1 bit N
            // Opcode bits 7..4 = $1,$3,$5,$7,$9,$B,$D,$F → CLR1 bit N
            // The low nibble is always $2.
            // ---------------------------------------------------------
            0x02 | 0x22 | 0x42 | 0x62 | 0x82 | 0xA2 | 0xC2 | 0xE2 => {
                // SET1 dp.bit
                let bit = (opcode >> 5) & 0x07;
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr) | (1 << bit);
                bus.write(addr, v);
            }
            0x12 | 0x32 | 0x52 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
                // CLR1 dp.bit
                let bit = (opcode >> 5) & 0x07;
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr) & !(1 << bit);
                bus.write(addr, v);
            }

            // ---------------------------------------------------------
            // Branch on bit set/clear (BBS / BBC).
            // 16 opcodes in pairs: $03/$13/$23/$33/.../$F3.
            // Opcode bits 7..5 = bit index. Low nibble = $3.
            // High-nibble even → BBS (branch if set), odd → BBC.
            // Format: opcode, dp, rel.
            // ---------------------------------------------------------
            0x03 | 0x23 | 0x43 | 0x63 | 0x83 | 0xA3 | 0xC3 | 0xE3 => {
                // BBS dp.bit, rel
                let bit = (opcode >> 5) & 0x07;
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                let rel = self.fetch_u8(bus) as i8;
                if v & (1 << bit) != 0 {
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                }
            }
            0x13 | 0x33 | 0x53 | 0x73 | 0x93 | 0xB3 | 0xD3 | 0xF3 => {
                // BBC dp.bit, rel
                let bit = (opcode >> 5) & 0x07;
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr);
                let rel = self.fetch_u8(bus) as i8;
                if v & (1 << bit) == 0 {
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                }
            }

            // ---------------------------------------------------------
            // Indexed-absolute MOV (table store / load patterns)
            // ---------------------------------------------------------
            0xD5 => {
                // MOV !abs+X,A
                let base = self.fetch_u16(bus);
                let addr = base.wrapping_add(u16::from(self.x));
                bus.write(addr, self.a);
            }
            0xD6 => {
                // MOV !abs+Y,A
                let base = self.fetch_u16(bus);
                let addr = base.wrapping_add(u16::from(self.y));
                bus.write(addr, self.a);
            }
            0xF5 => {
                // MOV A,!abs+X
                let base = self.fetch_u16(bus);
                let addr = base.wrapping_add(u16::from(self.x));
                let v = bus.read(addr);
                self.a = v;
                self.set_nz(v);
            }
            0xF6 => {
                // MOV A,!abs+Y
                let base = self.fetch_u16(bus);
                let addr = base.wrapping_add(u16::from(self.y));
                let v = bus.read(addr);
                self.a = v;
                self.set_nz(v);
            }
            0x3D => {
                // INC X
                self.x = self.x.wrapping_add(1);
                let v = self.x;
                self.set_nz(v);
            }
            0xDC => {
                // DEC Y
                self.y = self.y.wrapping_sub(1);
                let v = self.y;
                self.set_nz(v);
            }
            0xBC => {
                // INC A
                self.a = self.a.wrapping_add(1);
                let v = self.a;
                self.set_nz(v);
            }
            0x9C => {
                // DEC A
                self.a = self.a.wrapping_sub(1);
                let v = self.a;
                self.set_nz(v);
            }
            0x9F => {
                // XCN A — exchange nibbles of A.
                self.a = self.a.rotate_left(4);
                let v = self.a;
                self.set_nz(v);
            }
            0x6E => {
                // DBNZ dp,rel — decrement direct-page byte, branch if
                // result != 0. Very common in driver tight loops.
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let v = bus.read(addr).wrapping_sub(1);
                bus.write(addr, v);
                let rel = self.fetch_u8(bus) as i8;
                if v != 0 {
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                }
            }
            0xFE => {
                // DBNZ Y,rel — decrement Y, branch if Y != 0.
                self.y = self.y.wrapping_sub(1);
                let rel = self.fetch_u8(bus) as i8;
                if self.y != 0 {
                    self.pc = self.pc.wrapping_add_signed(i16::from(rel));
                }
            }

            // ---------------------------------------------------------
            // Auto-increment / extra MOV variants
            // ---------------------------------------------------------
            0xAF => {
                // MOV (X)+,A — store A at direct page X, then X++.
                let addr = self.direct_addr(self.x);
                bus.write(addr, self.a);
                self.x = self.x.wrapping_add(1);
            }
            0xBF => {
                // MOV A,(X)+ — A ← *(direct[X]), then X++.
                let addr = self.direct_addr(self.x);
                let v = bus.read(addr);
                self.a = v;
                self.x = self.x.wrapping_add(1);
                self.set_nz(v);
            }
            0xE6 => {
                // MOV A,(X) — A ← *(direct[X]).
                let addr = self.direct_addr(self.x);
                let v = bus.read(addr);
                self.a = v;
                self.set_nz(v);
            }
            0x9D => {
                // MOV X,SP
                self.x = self.sp;
                let v = self.x;
                self.set_nz(v);
            }
            0xFD => {
                // MOV Y,A
                self.y = self.a;
                let v = self.y;
                self.set_nz(v);
            }
            0x7D => {
                // MOV A,X
                self.a = self.x;
                let v = self.a;
                self.set_nz(v);
            }

            // ---------------------------------------------------------
            // Compare with immediate
            // ---------------------------------------------------------
            0x68 => {
                // CMP A,#imm
                let imm = self.fetch_u8(bus);
                let a = self.a;
                self.cmp_u8(a, imm);
            }
            0xC8 => {
                // CMP X,#imm
                let imm = self.fetch_u8(bus);
                let x = self.x;
                self.cmp_u8(x, imm);
            }
            0xAD => {
                // CMP Y,#imm
                let imm = self.fetch_u8(bus);
                let y = self.y;
                self.cmp_u8(y, imm);
            }
            0x3E => {
                // CMP X,dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let mem = bus.read(addr);
                let x = self.x;
                self.cmp_u8(x, mem);
            }
            0x1E => {
                // CMP X,!abs
                let addr = self.fetch_u16(bus);
                let mem = bus.read(addr);
                let x = self.x;
                self.cmp_u8(x, mem);
            }
            0x5E => {
                // CMP Y,!abs
                let addr = self.fetch_u16(bus);
                let mem = bus.read(addr);
                let y = self.y;
                self.cmp_u8(y, mem);
            }
            0x64 => {
                // CMP A,dp
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let mem = bus.read(addr);
                let a = self.a;
                self.cmp_u8(a, mem);
            }

            // ---------------------------------------------------------
            // Arithmetic / logical with immediate
            // ---------------------------------------------------------
            0x28 => {
                // AND A,#imm
                let imm = self.fetch_u8(bus);
                self.a &= imm;
                let v = self.a;
                self.set_nz(v);
            }
            0x48 => {
                // EOR A,#imm
                let imm = self.fetch_u8(bus);
                self.a ^= imm;
                let v = self.a;
                self.set_nz(v);
            }
            0x08 => {
                // OR A,#imm
                let imm = self.fetch_u8(bus);
                self.a |= imm;
                let v = self.a;
                self.set_nz(v);
            }
            0x88 => {
                // ADC A,#imm
                let imm = self.fetch_u8(bus);
                let a = self.a;
                self.a = self.adc_u8(a, imm);
            }
            0xA8 => {
                // SBC A,#imm
                let imm = self.fetch_u8(bus);
                let a = self.a;
                self.a = self.sbc_u8(a, imm);
            }

            // ---------------------------------------------------------
            // Stack: PUSH / POP
            // ---------------------------------------------------------
            0x2D => {
                // PUSH A
                let a = self.a;
                self.push_u8(bus, a);
            }
            0x4D => {
                // PUSH X
                let x = self.x;
                self.push_u8(bus, x);
            }
            0x6D => {
                // PUSH Y
                let y = self.y;
                self.push_u8(bus, y);
            }
            0x0D => {
                // PUSH PSW
                let p = self.psw.0;
                self.push_u8(bus, p);
            }
            0xAE => {
                // POP A
                self.a = self.pop_u8(bus);
            }
            0xCE => {
                // POP X
                self.x = self.pop_u8(bus);
            }
            0xEE => {
                // POP Y
                self.y = self.pop_u8(bus);
            }
            0x8E => {
                // POP PSW
                self.psw.0 = self.pop_u8(bus);
            }

            // ---------------------------------------------------------
            // Calls / jumps
            // ---------------------------------------------------------
            0x3F => {
                // CALL !abs — push PC (return addr) then jump.
                let target = self.fetch_u16(bus);
                let return_pc = self.pc;
                self.push_u8(bus, (return_pc >> 8) as u8);
                self.push_u8(bus, return_pc as u8);
                self.pc = target;
            }
            0x6F => {
                // RET — pop low / high return PC.
                let lo = self.pop_u8(bus);
                let hi = self.pop_u8(bus);
                self.pc = u16::from(lo) | (u16::from(hi) << 8);
            }
            0x5F => {
                // JMP !abs
                let target = self.fetch_u16(bus);
                self.pc = target;
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

            // ---------------------------------------------------------
            // TCALL N — table call. Single-byte opcode `$X1`, where the
            // high nibble selects one of 16 vectors at `$FFC0..=$FFDF`
            // (TCALL 0 reads `$FFDE/$FFDF`, TCALL 15 reads
            // `$FFC0/$FFC1`). Push return PC, jump through the vector.
            // ---------------------------------------------------------
            0x01 | 0x11 | 0x21 | 0x31 | 0x41 | 0x51 | 0x61 | 0x71 | 0x81 | 0x91 | 0xA1 | 0xB1
            | 0xC1 | 0xD1 | 0xE1 | 0xF1 => {
                let n = (opcode >> 4) & 0x0F;
                let vec_addr = 0xFFDE_u16.wrapping_sub(u16::from(n) * 2);
                let return_pc = self.pc;
                self.push_u8(bus, (return_pc >> 8) as u8);
                self.push_u8(bus, return_pc as u8);
                let lo = bus.read(vec_addr);
                let hi = bus.read(vec_addr.wrapping_add(1));
                self.pc = u16::from(lo) | (u16::from(hi) << 8);
            }

            // ---------------------------------------------------------
            // ALU on (X),(Y) — memory-memory ALU through register-
            // indirect addressing. `(X)` resolves to `direct_addr(X)`;
            // `(Y)` to `direct_addr(Y)`. The result lands at `(X)`,
            // except for CMP which only updates flags. Each opcode is
            // a single byte (no operands).
            // ---------------------------------------------------------
            0x19 => {
                // OR (X),(Y)
                let dx = self.direct_addr(self.x);
                let dy = self.direct_addr(self.y);
                let v = bus.read(dx) | bus.read(dy);
                bus.write(dx, v);
                self.set_nz(v);
            }
            0x39 => {
                // AND (X),(Y)
                let dx = self.direct_addr(self.x);
                let dy = self.direct_addr(self.y);
                let v = bus.read(dx) & bus.read(dy);
                bus.write(dx, v);
                self.set_nz(v);
            }
            0x59 => {
                // EOR (X),(Y)
                let dx = self.direct_addr(self.x);
                let dy = self.direct_addr(self.y);
                let v = bus.read(dx) ^ bus.read(dy);
                bus.write(dx, v);
                self.set_nz(v);
            }
            0x79 => {
                // CMP (X),(Y)
                let dx = self.direct_addr(self.x);
                let dy = self.direct_addr(self.y);
                let lhs = bus.read(dx);
                let rhs = bus.read(dy);
                self.cmp_u8(lhs, rhs);
            }
            0x99 => {
                // ADC (X),(Y)
                let dx = self.direct_addr(self.x);
                let dy = self.direct_addr(self.y);
                let lhs = bus.read(dx);
                let rhs = bus.read(dy);
                let r = self.adc_u8(lhs, rhs);
                bus.write(dx, r);
            }
            0xB9 => {
                // SBC (X),(Y)
                let dx = self.direct_addr(self.x);
                let dy = self.direct_addr(self.y);
                let lhs = bus.read(dx);
                let rhs = bus.read(dy);
                let r = self.sbc_u8(lhs, rhs);
                bus.write(dx, r);
            }

            // ---------------------------------------------------------
            // ALU dp,#imm — read-modify-write on direct page with an
            // immediate right-hand operand. Object-code order matches
            // the existing OR/AND/EOR/CMP dp,#imm family above: imm
            // first, then dp. ADC and SBC update N/V/H/Z/C.
            // ---------------------------------------------------------
            0x98 => {
                // ADC dp,#imm — needed by DKC and Secret of Mana.
                let imm = self.fetch_u8(bus);
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let mem = bus.read(addr);
                let v = self.adc_u8(mem, imm);
                bus.write(addr, v);
            }
            0xB8 => {
                // SBC dp,#imm
                let imm = self.fetch_u8(bus);
                let dp = self.fetch_u8(bus);
                let addr = self.direct_addr(dp);
                let mem = bus.read(addr);
                let v = self.sbc_u8(mem, imm);
                bus.write(addr, v);
            }

            // ---------------------------------------------------------
            // MOV dp,dp — direct-page byte copy. Source first, then
            // destination, in object code. Does NOT update flags on
            // real SPC700. Needed by Chrono Trigger / Super Bomberman.
            // ---------------------------------------------------------
            0xFA => {
                let src = self.fetch_u8(bus);
                let dst = self.fetch_u8(bus);
                let v = bus.read(self.direct_addr(src));
                bus.write(self.direct_addr(dst), v);
            }

            // ---------------------------------------------------------
            // Bit operations against memory.bit / Carry. The operand
            // is a 16-bit word: low 13 bits = absolute address, high
            // 3 bits = bit index. See [`Self::fetch_mem_bit`].
            // ---------------------------------------------------------
            0x0A => {
                // OR1 C, m.b
                let (addr, b) = self.fetch_mem_bit(bus);
                let bit_set = (bus.read(addr) >> b) & 1 != 0;
                let c = self.psw.contains(bit::C);
                self.psw.set(bit::C, c | bit_set);
            }
            0x2A => {
                // OR1 C, /m.b (inverted source bit)
                let (addr, b) = self.fetch_mem_bit(bus);
                let bit_set = (bus.read(addr) >> b) & 1 != 0;
                let c = self.psw.contains(bit::C);
                self.psw.set(bit::C, c | !bit_set);
            }
            0x4A => {
                // AND1 C, m.b
                let (addr, b) = self.fetch_mem_bit(bus);
                let bit_set = (bus.read(addr) >> b) & 1 != 0;
                let c = self.psw.contains(bit::C);
                self.psw.set(bit::C, c & bit_set);
            }
            0x6A => {
                // AND1 C, /m.b
                let (addr, b) = self.fetch_mem_bit(bus);
                let bit_set = (bus.read(addr) >> b) & 1 != 0;
                let c = self.psw.contains(bit::C);
                self.psw.set(bit::C, c & !bit_set);
            }
            0x8A => {
                // EOR1 C, m.b
                let (addr, b) = self.fetch_mem_bit(bus);
                let bit_set = (bus.read(addr) >> b) & 1 != 0;
                let c = self.psw.contains(bit::C);
                self.psw.set(bit::C, c ^ bit_set);
            }
            0xAA => {
                // MOV1 C, m.b — load C from bit.
                let (addr, b) = self.fetch_mem_bit(bus);
                let bit_set = (bus.read(addr) >> b) & 1 != 0;
                self.psw.set(bit::C, bit_set);
            }
            0xCA => {
                // MOV1 m.b, C — store C into bit.
                let (addr, b) = self.fetch_mem_bit(bus);
                let mem = bus.read(addr);
                let mask = 1u8 << b;
                let v = if self.psw.contains(bit::C) {
                    mem | mask
                } else {
                    mem & !mask
                };
                bus.write(addr, v);
            }
            0xEA => {
                // NOT1 m.b — toggle a bit in memory.
                let (addr, b) = self.fetch_mem_bit(bus);
                let mem = bus.read(addr);
                bus.write(addr, mem ^ (1u8 << b));
            }

            // ---------------------------------------------------------
            // TSET1 / TCLR1 !abs — test-and-set / test-and-clear all
            // bits selected by A on a memory byte. Both update N/Z
            // from `A - mem` (without writing the subtraction back).
            // ---------------------------------------------------------
            0x0E => {
                // TSET1 !abs — mem |= A, flags from A - mem (the
                // value *before* the OR is used for the compare).
                let addr = self.fetch_u16(bus);
                let mem = bus.read(addr);
                // N/Z come from the subtraction A - mem, but C is NOT
                // updated by TSET1.
                let diff = self.a.wrapping_sub(mem);
                self.set_nz(diff);
                bus.write(addr, mem | self.a);
            }
            0x4E => {
                // TCLR1 !abs — mem &= !A, flags from A - mem.
                let addr = self.fetch_u16(bus);
                let mem = bus.read(addr);
                let diff = self.a.wrapping_sub(mem);
                self.set_nz(diff);
                bus.write(addr, mem & !self.a);
            }

            // ---------------------------------------------------------
            // BCD adjust — DAA after add, DAS after sub. Both update
            // C and N/Z and consume one byte (no operand).
            // ---------------------------------------------------------
            0xDF => {
                // DAA — decimal adjust accumulator after addition.
                if self.psw.contains(bit::C) || self.a > 0x99 {
                    self.a = self.a.wrapping_add(0x60);
                    self.psw.insert(bit::C);
                }
                if self.psw.contains(bit::H) || (self.a & 0x0F) > 0x09 {
                    self.a = self.a.wrapping_add(0x06);
                }
                let v = self.a;
                self.set_nz(v);
            }
            0xBE => {
                // DAS — decimal adjust accumulator after subtraction.
                if !self.psw.contains(bit::C) || self.a > 0x99 {
                    self.a = self.a.wrapping_sub(0x60);
                    self.psw.remove(bit::C);
                }
                if !self.psw.contains(bit::H) || (self.a & 0x0F) > 0x09 {
                    self.a = self.a.wrapping_sub(0x06);
                }
                let v = self.a;
                self.set_nz(v);
            }

            // ---------------------------------------------------------
            // BRK / RETI — software interrupt and return.
            // BRK pushes PC and PSW, sets B, clears I, then jumps
            // through the vector at `$FFDE`. RETI pops PSW then PC.
            // ---------------------------------------------------------
            0x0F => {
                // BRK
                let return_pc = self.pc;
                self.push_u8(bus, (return_pc >> 8) as u8);
                self.push_u8(bus, return_pc as u8);
                let p = self.psw.0;
                self.push_u8(bus, p);
                self.psw.insert(bit::B);
                self.psw.remove(bit::I);
                let lo = bus.read(0xFFDE);
                let hi = bus.read(0xFFDF);
                self.pc = u16::from(lo) | (u16::from(hi) << 8);
            }
            0x7F => {
                // RETI
                self.psw.0 = self.pop_u8(bus);
                let lo = self.pop_u8(bus);
                let hi = self.pop_u8(bus);
                self.pc = u16::from(lo) | (u16::from(hi) << 8);
            }
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

    /// 8-bit add-with-carry. Sets N/V/H/Z/C. The half-carry flag is
    /// set from bit 4 of `(a & 0x0F) + (b & 0x0F) + carry`. Overflow
    /// (V) is signed-overflow detection.
    fn adc_u8(&mut self, a: u8, b: u8) -> u8 {
        let c_in = u16::from(self.psw.contains(bit::C));
        let sum = u16::from(a) + u16::from(b) + c_in;
        let result = sum as u8;
        self.set_nz(result);
        self.psw.set(bit::C, sum > 0xFF);
        // Half-carry from bit 3 → bit 4.
        let h = ((a & 0x0F) + (b & 0x0F) + c_in as u8) > 0x0F;
        self.psw.set(bit::H, h);
        // Signed overflow: operands had matching sign and result differs.
        let v = ((a ^ result) & (b ^ result) & 0x80) != 0;
        self.psw.set(bit::V, v);
        result
    }

    /// 8-bit subtract-with-borrow. The SPC700's SBC is "ADC of the
    /// one's complement" — borrow comes from `!carry`. Sets N/V/H/Z/C.
    fn sbc_u8(&mut self, a: u8, b: u8) -> u8 {
        // SBC A,b = ADC A,~b — same flag rules.
        self.adc_u8(a, !b)
    }

    /// Push a byte onto the stack at `$01xx` (sp = low byte) and
    /// decrement the stack pointer.
    fn push_u8<B: SpcBus>(&mut self, bus: &mut B, value: u8) {
        bus.write(0x0100 | u16::from(self.sp), value);
        self.sp = self.sp.wrapping_sub(1);
    }

    /// Pre-increment the stack pointer, then read the byte it now
    /// points to.
    fn pop_u8<B: SpcBus>(&mut self, bus: &mut B) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x0100 | u16::from(self.sp))
    }

    /// Fetch a `dp` byte, resolve `[dp+X]` as a 16-bit pointer in
    /// direct page, and read the byte the pointer points to.
    fn read_indirect_x<B: SpcBus>(&mut self, bus: &mut B) -> u8 {
        let dp = self.fetch_u8(bus);
        let dpx = dp.wrapping_add(self.x);
        let lo = bus.read(self.direct_addr(dpx));
        let hi = bus.read(self.direct_addr(dpx.wrapping_add(1)));
        let target = u16::from(lo) | (u16::from(hi) << 8);
        bus.read(target)
    }

    /// Fetch the 2-byte `m.b` operand used by bit-on-memory ops
    /// (`MOV1 C, m.b`, `AND1 C, m.b`, etc.).
    ///
    /// The operand is laid out little-endian: low 13 bits give the
    /// absolute byte address (0..=$1FFF), top 3 bits the bit index
    /// within that byte. Returns `(address, bit_index)`.
    fn fetch_mem_bit<B: SpcBus>(&mut self, bus: &mut B) -> (u16, u8) {
        let word = self.fetch_u16(bus);
        let addr = word & 0x1FFF;
        let bit = ((word >> 13) & 0x07) as u8;
        (addr, bit)
    }

    /// Fetch a `dp` byte, resolve `[dp]` as a 16-bit pointer, then
    /// read the byte at `pointer + Y`.
    fn read_indirect_y<B: SpcBus>(&mut self, bus: &mut B) -> u8 {
        let dp = self.fetch_u8(bus);
        let lo = bus.read(self.direct_addr(dp));
        let hi = bus.read(self.direct_addr(dp.wrapping_add(1)));
        let ptr = u16::from(lo) | (u16::from(hi) << 8);
        let target = ptr.wrapping_add(u16::from(self.y));
        bus.read(target)
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

    // -------------------------------------------------------------------
    // P2.SPC.5 batch — common opcodes the SMW music driver needs
    // -------------------------------------------------------------------

    #[test]
    fn mov_x_plus_a_stores_and_increments() {
        // X=$10, A=$42, MOV (X)+,A → $0010 ← $42, X = $11.
        let (mut cpu, mut bus) = run(&[0xCD, 0x10, 0xE8, 0x42, 0xAF]);
        cpu.step(&mut bus); // MOV X,#$10
        cpu.step(&mut bus); // MOV A,#$42
        cpu.step(&mut bus); // MOV (X)+,A
        assert_eq!(bus.peek(0x0010), 0x42);
        assert_eq!(cpu.x, 0x11);
    }

    #[test]
    fn cmp_a_imm_sets_carry_and_zero_when_equal() {
        let (mut cpu, mut bus) = run(&[0xE8, 0x42, 0x68, 0x42]);
        cpu.step(&mut bus); // MOV A,#$42
        cpu.step(&mut bus); // CMP A,#$42
        assert!(cpu.psw.contains(bit::Z));
        assert!(cpu.psw.contains(bit::C));
    }

    #[test]
    fn cmp_x_imm_clears_carry_when_lhs_less_than_rhs() {
        let (mut cpu, mut bus) = run(&[0xCD, 0x10, 0xC8, 0x20]);
        cpu.step(&mut bus); // MOV X,#$10
        cpu.step(&mut bus); // CMP X,#$20
        assert!(!cpu.psw.contains(bit::C));
        assert!(cpu.psw.contains(bit::N));
    }

    #[test]
    fn and_or_eor_with_immediate() {
        let (mut cpu, mut bus) = run(&[0xE8, 0xF0, 0x28, 0x0F, 0xE8, 0xF0, 0x08, 0x0F, 0x48, 0xFF]);
        cpu.step(&mut bus); // MOV A,#$F0
        cpu.step(&mut bus); // AND A,#$0F → $00
        assert_eq!(cpu.a, 0);
        cpu.step(&mut bus); // MOV A,#$F0
        cpu.step(&mut bus); // OR A,#$0F → $FF
        assert_eq!(cpu.a, 0xFF);
        cpu.step(&mut bus); // EOR A,#$FF → $00
        assert_eq!(cpu.a, 0);
    }

    #[test]
    fn adc_imm_with_no_carry_in() {
        let (mut cpu, mut bus) = run(&[0xE8, 0x05, 0x60, 0x88, 0x03]);
        cpu.step(&mut bus); // MOV A,#$05
        cpu.step(&mut bus); // CLRC
        cpu.step(&mut bus); // ADC A,#$03 → $08
        assert_eq!(cpu.a, 0x08);
        assert!(!cpu.psw.contains(bit::C));
    }

    #[test]
    fn sbc_imm_propagates_borrow() {
        // SBC is "ADC of one's complement". Start with C set (no borrow).
        let (mut cpu, mut bus) = run(&[0xE8, 0x10, 0x80, 0xA8, 0x03]);
        cpu.step(&mut bus); // MOV A,#$10
        cpu.step(&mut bus); // SETC
        cpu.step(&mut bus); // SBC A,#$03 → $0D
        assert_eq!(cpu.a, 0x0D);
        // C set means no borrow.
        assert!(cpu.psw.contains(bit::C));
    }

    #[test]
    fn push_a_pop_a_round_trip() {
        let (mut cpu, mut bus) = run(&[0xE8, 0x42, 0x2D, 0xE8, 0x00, 0xAE]);
        cpu.step(&mut bus); // MOV A,#$42
        cpu.step(&mut bus); // PUSH A
        cpu.step(&mut bus); // MOV A,#$00 (clobber)
        cpu.step(&mut bus); // POP A → $42
        assert_eq!(cpu.a, 0x42);
    }

    #[test]
    fn call_pushes_return_pc_and_jumps() {
        // Place CALL !$0300 at $0200. RET at $0300 should pop back to
        // $0203 (the instruction after the 3-byte CALL).
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke_slice(0x0200, &[0x3F, 0x00, 0x03]);
        bus.poke(0x0300, 0x6F); // RET
        cpu.reset(&mut bus);
        cpu.sp = 0xFF;
        cpu.step(&mut bus); // CALL $0300
        assert_eq!(cpu.pc, 0x0300);
        cpu.step(&mut bus); // RET
        assert_eq!(cpu.pc, 0x0203);
    }

    #[test]
    fn jmp_abs_loads_pc_directly() {
        let (mut cpu, mut bus) = run(&[0x5F, 0x34, 0x12]);
        cpu.step(&mut bus); // JMP $1234
        assert_eq!(cpu.pc, 0x1234);
    }

    #[test]
    fn mov_abs_x_store_indexes_correctly() {
        let (mut cpu, mut bus) = run(&[0xCD, 0x05, 0xE8, 0x42, 0xD5, 0x00, 0x03]);
        cpu.step(&mut bus); // MOV X,#$05
        cpu.step(&mut bus); // MOV A,#$42
        cpu.step(&mut bus); // MOV $0300+X,A
        assert_eq!(bus.peek(0x0305), 0x42);
    }

    #[test]
    fn inc_x_decrement_y_set_flags() {
        let (mut cpu, mut bus) = run(&[0xCD, 0xFF, 0x3D, 0x8D, 0x01, 0xDC]);
        cpu.step(&mut bus); // MOV X,#$FF
        cpu.step(&mut bus); // INC X → 0 (wrap, Z set)
        assert_eq!(cpu.x, 0);
        assert!(cpu.psw.contains(bit::Z));
        cpu.step(&mut bus); // MOV Y,#$01
        cpu.step(&mut bus); // DEC Y → 0
        assert_eq!(cpu.y, 0);
        assert!(cpu.psw.contains(bit::Z));
    }

    #[test]
    fn dbnz_dp_loops_until_zero() {
        // Counter at $10, init to 3. DBNZ $10,-3 at $0203 should:
        //   iter1: $10 = 2, branch back
        //   iter2: $10 = 1, branch back
        //   iter3: $10 = 0, fall through
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x0010, 3);
        // At $0200: NOP NOP NOP DBNZ $10,-3 NOP
        // After fetching the 3-byte DBNZ, PC = $0206. To branch back
        // to the DBNZ opcode itself at $0203, rel = $FD (-3).
        bus.poke_slice(0x0200, &[0x00, 0x00, 0x00, 0x6E, 0x10, 0xFD, 0x00]);
        cpu.reset(&mut bus);
        cpu.pc = 0x0203; // start at the DBNZ
        cpu.step(&mut bus); // iter 1
        assert_eq!(bus.peek(0x0010), 2);
        assert_eq!(cpu.pc, 0x0203);
        cpu.step(&mut bus); // iter 2
        assert_eq!(bus.peek(0x0010), 1);
        cpu.step(&mut bus); // iter 3 — falls through
        assert_eq!(bus.peek(0x0010), 0);
        assert_eq!(cpu.pc, 0x0206);
    }

    #[test]
    fn set1_clr1_toggle_specific_bit_in_dp() {
        // SET1 dp.3 sets bit 3 of $10; CLR1 dp.7 clears bit 7.
        let (mut cpu, mut bus) = run(&[0x62, 0x10, 0xF2, 0x10]);
        bus.poke(0x0010, 0b1000_0000);
        cpu.step(&mut bus); // SET1 $10.3
        assert_eq!(bus.peek(0x0010), 0b1000_1000);
        cpu.step(&mut bus); // CLR1 $10.7
        assert_eq!(bus.peek(0x0010), 0b0000_1000);
    }

    #[test]
    fn bbs_branches_when_target_bit_is_set() {
        // BBS $10.5,+8 — should branch since bit 5 of $10 is set.
        let (mut cpu, mut bus) = run(&[0xA3, 0x10, 0x08]);
        bus.poke(0x0010, 0b0010_0000);
        let pc_before = cpu.pc;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, pc_before + 3 + 8);
    }

    #[test]
    fn bbc_does_not_branch_when_bit_is_set() {
        let (mut cpu, mut bus) = run(&[0xB3, 0x10, 0x08]);
        bus.poke(0x0010, 0b0010_0000);
        let pc_before = cpu.pc;
        cpu.step(&mut bus);
        assert_eq!(
            cpu.pc,
            pc_before + 3,
            "BBC should NOT branch when bit is set"
        );
    }

    // (The "unimplemented-opcode safety net" test was retired when
    // SPC700 reached full 256/256 opcode coverage; the dispatch is
    // now exhaustive and the catch-all arm was removed in favour of
    // compile-time exhaustiveness checking.)

    // -------------------------------------------------------------------
    // P3.SPC.X batch — fill out the remaining 39 opcodes that real-world
    // SNES audio drivers (DKC, Chrono Trigger, Secret of Mana, Super
    // Bomberman, etc.) actually use.
    // -------------------------------------------------------------------

    #[test]
    fn adc_dp_imm_adds_immediate_to_direct_page() {
        // $98 — needed by DKC and Secret of Mana.
        // $10 ← 5 ; CLRC ; ADC $10,#3 → $10 == 8.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke_slice(0x0200, &[0x8F, 0x05, 0x10, 0x60, 0x98, 0x03, 0x10]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus); // MOV $10,#$05
        cpu.step(&mut bus); // CLRC
        cpu.step(&mut bus); // ADC $10,#$03
        assert_eq!(bus.peek(0x0010), 0x08);
    }

    #[test]
    fn sbc_dp_imm_subtracts_immediate_from_direct_page() {
        // $B8.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        // $10 ← $20 ; SETC (no borrow) ; SBC $10,#$05 → $1B.
        bus.poke_slice(0x0200, &[0x8F, 0x20, 0x10, 0x80, 0xB8, 0x05, 0x10]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x0010), 0x1B);
    }

    #[test]
    fn mov_dp_dp_copies_without_flags() {
        // $FA — needed by Chrono Trigger and Super Bomberman.
        // Seed $10=$AB and $20=$00; MOV $20,$10 should leave $20=$AB.
        // Object code order is (src=$10, dst=$20).
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x0010, 0xAB);
        bus.poke(0x0020, 0x00);
        bus.poke_slice(0x0200, &[0xFA, 0x10, 0x20]);
        cpu.reset(&mut bus);
        // Pre-set a known flag pattern (without `P` so direct page
        // stays at $00xx); confirm MOV dp,dp doesn't touch it.
        cpu.psw.0 = 0x85; // N + I + C, no P
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x0020), 0xAB);
        assert_eq!(cpu.psw.0, 0x85, "MOV dp,dp must not touch flags");
    }

    #[test]
    fn tcall_pushes_return_pc_and_jumps_via_vector() {
        // TCALL 5 ($51) reads vector at $FFDE - 2*5 = $FFD4. Place a
        // target there and confirm the jump.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0xFFD4, 0x80);
        bus.poke(0xFFD5, 0x03); // vector → $0380
        bus.poke_slice(0x0200, &[0x51]); // TCALL 5
        cpu.reset(&mut bus);
        cpu.sp = 0xFF;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x0380);
        // Return PC pushed = $0201 (after the 1-byte TCALL).
        assert_eq!(bus.peek(0x01FF), 0x02);
        assert_eq!(bus.peek(0x01FE), 0x01);
        assert_eq!(cpu.sp, 0xFD);
    }

    #[test]
    fn tcall_zero_uses_vector_at_ffde() {
        // TCALL 0 ($01) — boundary check, reads $FFDE itself.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0xFFDE, 0x77);
        bus.poke(0xFFDF, 0x12);
        bus.poke_slice(0x0200, &[0x01]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x1277);
    }

    #[test]
    fn tcall_fifteen_uses_vector_at_ffc0() {
        // TCALL 15 ($F1) — opposite boundary.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0xFFC0, 0x55);
        bus.poke(0xFFC1, 0xAA);
        bus.poke_slice(0x0200, &[0xF1]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0xAA55);
    }

    #[test]
    fn or_x_y_combines_two_dp_bytes() {
        // $19 OR (X),(Y) — needed by Chrono Trigger sound driver.
        // X=$10, Y=$20 ; $10=$0F, $20=$F0 ; OR (X),(Y) → $10 = $FF.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x0010, 0x0F);
        bus.poke(0x0020, 0xF0);
        bus.poke_slice(0x0200, &[0xCD, 0x10, 0x8D, 0x20, 0x19]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus); // MOV X,#$10
        cpu.step(&mut bus); // MOV Y,#$20
        cpu.step(&mut bus); // OR (X),(Y)
        assert_eq!(bus.peek(0x0010), 0xFF);
        assert!(cpu.psw.contains(bit::N));
    }

    #[test]
    fn cmp_x_y_sets_zero_when_equal() {
        // $79 CMP (X),(Y) — flag-only variant.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x0010, 0x42);
        bus.poke(0x0020, 0x42);
        bus.poke_slice(0x0200, &[0xCD, 0x10, 0x8D, 0x20, 0x79]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        cpu.step(&mut bus); // CMP (X),(Y)
        assert!(cpu.psw.contains(bit::Z));
        assert!(cpu.psw.contains(bit::C));
        // The memory was NOT written.
        assert_eq!(bus.peek(0x0010), 0x42);
    }

    #[test]
    fn mov1_c_mem_bit_loads_bit_into_c() {
        // $AA MOV1 C, m.b — operand $A012 = address $0012, bit 5.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x0012, 0b0010_0000); // bit 5 set
        bus.poke_slice(0x0200, &[0xAA, 0x12, 0xA0]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        assert!(cpu.psw.contains(bit::C));
    }

    #[test]
    fn mov1_mem_bit_c_stores_c_into_bit() {
        // $CA MOV1 m.b, C — store. Operand $4012 = address $0012, bit 2.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x0012, 0b1111_0000); // bit 2 clear initially
        bus.poke_slice(0x0200, &[0x80, 0xCA, 0x12, 0x40]); // SETC then MOV1
        cpu.reset(&mut bus);
        cpu.step(&mut bus); // SETC
        cpu.step(&mut bus); // MOV1 $0012.bit2, C
        assert_eq!(bus.peek(0x0012), 0b1111_0100);
    }

    #[test]
    fn not1_mem_bit_toggles_bit_in_memory() {
        // $EA NOT1 m.b — toggle bit 7 of $0015.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x0015, 0b0000_1111);
        // Bit 7, address $0015. Operand word = (7 << 13) | $0015 = $E015.
        bus.poke_slice(0x0200, &[0xEA, 0x15, 0xE0]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x0015), 0b1000_1111);
        // Run it again — bit 7 toggles back.
        bus.poke_slice(0x0203, &[0xEA, 0x15, 0xE0]);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x0015), 0b0000_1111);
    }

    #[test]
    fn tset1_abs_or_with_a_and_sets_flags_from_compare() {
        // $0E. A=$F0, mem at $1234 = $0F. TSET1 sets mem |= A = $FF,
        // and N/Z come from A - mem (= $F0 - $0F = $E1; N set, Z clear).
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x1234, 0x0F);
        bus.poke_slice(0x0200, &[0xE8, 0xF0, 0x0E, 0x34, 0x12]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus); // MOV A,#$F0
        cpu.step(&mut bus); // TSET1 !$1234
        assert_eq!(bus.peek(0x1234), 0xFF);
        assert!(cpu.psw.contains(bit::N));
        assert!(!cpu.psw.contains(bit::Z));
    }

    #[test]
    fn tclr1_abs_and_with_not_a() {
        // $4E. A=$0F, mem=$FF. TCLR1 sets mem &= !A = $F0, and N/Z
        // from A - mem = $0F - $FF = $10 (N clear, Z clear).
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0x1234, 0xFF);
        bus.poke_slice(0x0200, &[0xE8, 0x0F, 0x4E, 0x34, 0x12]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x1234), 0xF0);
    }

    #[test]
    fn daa_after_bcd_add() {
        // 0x19 + 0x28 = 0x41 in binary; DAA should turn it into 0x47.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke_slice(0x0200, &[0xE8, 0x19, 0x60, 0x88, 0x28, 0xDF]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus); // MOV A,#$19
        cpu.step(&mut bus); // CLRC
        cpu.step(&mut bus); // ADC A,#$28 → $41 (with H set since 9+8=17)
        cpu.step(&mut bus); // DAA → $47
        assert_eq!(cpu.a, 0x47);
    }

    #[test]
    fn das_after_bcd_sub() {
        // 0x50 - 0x25 = 0x2B binary; DAS adjusts to 0x25.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke_slice(0x0200, &[0xE8, 0x50, 0x80, 0xA8, 0x25, 0xBE]);
        cpu.reset(&mut bus);
        cpu.step(&mut bus); // MOV A,#$50
        cpu.step(&mut bus); // SETC (no borrow)
        cpu.step(&mut bus); // SBC A,#$25 → $2B (H clear: 0-5 borrows)
        cpu.step(&mut bus); // DAS → $25
        assert_eq!(cpu.a, 0x25);
    }

    #[test]
    fn reti_pops_psw_then_pc() {
        // Push PSW=$AB and PC=$1234, then RETI.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke_slice(0x0200, &[0x7F]);
        cpu.reset(&mut bus);
        cpu.sp = 0xFC;
        // Stack grows downward; RETI pops in order PSW, lo, hi.
        bus.poke(0x01FD, 0xAB); // PSW
        bus.poke(0x01FE, 0x34); // PC lo
        bus.poke(0x01FF, 0x12); // PC hi
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.psw.0, 0xAB);
        assert_eq!(cpu.sp, 0xFF);
    }

    #[test]
    fn brk_pushes_state_and_jumps_via_ffde() {
        // BRK ($0F) — push PC then PSW, jump through $FFDE/$FFDF.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        bus.poke(0xFFFE, 0x00);
        bus.poke(0xFFFF, 0x02);
        bus.poke(0xFFDE, 0x77);
        bus.poke(0xFFDF, 0x12);
        bus.poke_slice(0x0200, &[0x0F]);
        cpu.reset(&mut bus);
        cpu.sp = 0xFF;
        cpu.psw.0 = 0x42;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x1277);
        // Stack: $01FF=PC hi, $01FE=PC lo, $01FD=PSW.
        assert_eq!(bus.peek(0x01FF), 0x02);
        assert_eq!(bus.peek(0x01FE), 0x01);
        assert_eq!(bus.peek(0x01FD), 0x42);
        assert!(cpu.psw.contains(bit::B));
        assert!(!cpu.psw.contains(bit::I));
    }

    #[test]
    fn full_256_opcode_coverage_compile_check() {
        // Compile-time/structural check: the `execute` match no
        // longer has a catch-all, which means the compiler must see
        // arms for all 256 byte values. If a future change drops one,
        // this file won't build — that's the regression we want.
        // The runtime side of the check just exercises the formerly-
        // missing opcodes to make sure they don't panic.
        let mut cpu = Spc700::new();
        let mut bus = RamBus::new();
        for op in [
            0x01u8, 0x0A, 0x0E, 0x0F, 0x19, 0x39, 0x4A, 0x4E, 0x59, 0x6A, 0x79, 0x7F, 0x8A, 0x98,
            0x99, 0xAA, 0xB8, 0xB9, 0xBE, 0xCA, 0xDF, 0xEA, 0xF1, 0xFA,
        ] {
            bus.poke(0xFFFE, 0x00);
            bus.poke(0xFFFF, 0x02);
            bus.poke_slice(0x0200, &[op, 0x00, 0x00]);
            cpu.reset(&mut bus);
            cpu.step(&mut bus);
            // No assertion about state — only that no panic + the CPU
            // didn't get stuck on `unimplemented_opcode`.
            assert!(cpu.unimplemented_opcode.is_none(), "opcode ${op:02X}");
        }
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
