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

            // Everything else: we haven't implemented this opcode yet.
            // Record it for diagnostics and stop the CPU gracefully so
            // the host emulator doesn't deadlock and the user can see
            // exactly which byte to add next.
            other => {
                let pc_of_opcode = self.pc.wrapping_sub(1);
                self.unimplemented_opcode = Some((other, pc_of_opcode));
                self.stopped = true;
                // Rewind PC so re-stepping after we *add* the opcode
                // starts at the right place (caller's responsibility
                // to clear `stopped` + `unimplemented_opcode`).
                self.pc = pc_of_opcode;
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

    #[test]
    fn unimplemented_opcode_records_and_stops() {
        // $EA (NOT1 abs.bit, not yet implemented) — running it
        // should NOT panic and instead leave the CPU stopped with
        // the opcode captured.
        let (mut cpu, mut bus) = run(&[0xEA]);
        cpu.step(&mut bus);
        assert!(cpu.stopped);
        assert_eq!(cpu.unimplemented_opcode, Some((0xEA, 0x0200)));
        // PC rewinds so adding the opcode and re-stepping picks it up
        // again from the same spot.
        assert_eq!(cpu.pc, 0x0200);
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
