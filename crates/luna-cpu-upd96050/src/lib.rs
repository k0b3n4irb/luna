//! NEC uPD7725 / uPD96050 DSP core.
//!
//! These are the small Harvard-architecture DSPs NEC supplied to several
//! SNES cartridges: the **DSP-1** (uPD7725, used by Super Mario Kart,
//! Pilotwings, …) computes Mode 7 perspective matrices; DSP-2/3/4 and the
//! Seta ST010/11 use the larger **uPD96050**. ares unifies both as one
//! core (`component/processor/upd96050`); this is a faithful Rust port.
//!
//! The core is standalone — no SNES glue. A bus shim (e.g. luna-bus's
//! `Dsp1Mapper`) owns an instance, fills the program/data ROM from the
//! cartridge firmware, drives [`Upd96050::exec`] on a clock budget, and
//! bridges the CPU↔DSP data ports ([`Upd96050::read_dr`] etc.).
//!
//! Register bit widths differ by revision (`pc`/`rp`/`dp`); everything
//! else is identical, so the instruction decoder is shared.

#![forbid(unsafe_code)]

/// Which NEC DSP this core models — selects the `pc`/`rp`/`dp` widths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Revision {
    /// uPD7725 — DSP-1/1A/1B (11-bit PC, 10-bit RP, 8-bit DP).
    Upd7725,
    /// uPD96050 — DSP-2/3/4, ST010/11 (14/11/11-bit).
    Upd96050,
}

/// One ALU flag set (per accumulator).
#[derive(Clone, Copy, Default)]
struct Flag {
    ov0: bool, // overflow 0
    ov1: bool, // overflow 1
    z: bool,   // zero
    c: bool,   // carry
    s0: bool,  // sign 0
    s1: bool,  // sign 1
}

/// Status register (`SR`). Mixes externally-visible handshake bits
/// (`rqm`, `drs`, `drc`) with user flags; only some bits are writable.
#[derive(Clone, Copy, Default)]
struct Status {
    p0: bool,    // output port 0
    p1: bool,    // output port 1
    ei: bool,    // enable interrupts
    sic: bool,   // serial input control
    soc: bool,   // serial output control
    drc: bool,   // data-register size (0 = 16-bit, 1 = 8-bit)
    dma: bool,   // data-register DMA mode
    drs: bool,   // data-register status (mid 16-bit transfer)
    usf0: bool,  // user flag 0
    usf1: bool,  // user flag 1
    rqm: bool,   // request for master (data ready / awaited)
    siack: bool, // serial input acknowledge (not emulated)
    soack: bool, // serial output acknowledge (not emulated)
}

impl Status {
    /// Pack to the 16-bit value seen on the data bus / at `$SR`. `drs`
    /// reads as 0 when `drc` (8-bit mode) is set — ares `upd96050.hpp`.
    const fn to_u16(self) -> u16 {
        let drs = self.drs && !self.drc;
        (self.p0 as u16)
            | (self.p1 as u16) << 1
            | (self.ei as u16) << 7
            | (self.sic as u16) << 8
            | (self.soc as u16) << 9
            | (self.drc as u16) << 10
            | (self.dma as u16) << 11
            | (drs as u16) << 12
            | (self.usf0 as u16) << 13
            | (self.usf1 as u16) << 14
            | (self.rqm as u16) << 15
    }

    /// Assign the writable bits from a 16-bit value (the internal
    /// `siack`/`soack` are left untouched, matching ares `operator=`).
    const fn set_bits(&mut self, v: u16) {
        self.p0 = v & 0x0001 != 0;
        self.p1 = v & 0x0002 != 0;
        self.ei = v & 0x0080 != 0;
        self.sic = v & 0x0100 != 0;
        self.soc = v & 0x0200 != 0;
        self.drc = v & 0x0400 != 0;
        self.dma = v & 0x0800 != 0;
        self.drs = v & 0x1000 != 0;
        self.usf0 = v & 0x2000 != 0;
        self.usf1 = v & 0x4000 != 0;
        self.rqm = v & 0x8000 != 0;
    }
}

/// A register masked to a fixed bit width (`pc`/`rp`/`dp` — ares's
/// `VariadicNatural`).
#[derive(Clone, Copy, Default)]
struct Vn {
    value: u16,
    mask: u16,
}

impl Vn {
    const fn new(bits: u32) -> Self {
        Self {
            value: 0,
            mask: ((1u32 << bits) - 1) as u16,
        }
    }
    const fn get(self) -> u16 {
        self.value
    }
    const fn set(&mut self, v: u16) {
        self.value = v & self.mask;
    }
    /// Return the current value, then post-increment (wrapping at width).
    const fn post_inc(&mut self) -> u16 {
        let cur = self.value;
        self.value = self.value.wrapping_add(1) & self.mask;
        cur
    }
    const fn dec(&mut self) {
        self.value = self.value.wrapping_sub(1) & self.mask;
    }
}

#[derive(Clone, Copy, Default)]
struct Registers {
    stack: [u16; 16],
    pc: Vn,
    rp: Vn,
    dp: Vn,
    sp: u8, // 4-bit stack pointer
    si: u16,
    so: u16,
    k: i16,
    l: i16,
    m: i16,
    n: i16,
    a: i16, // accumulator A
    b: i16, // accumulator B
    tr: u16,
    trb: u16,
    dr: u16, // data register (CPU port)
    sr: Status,
}

/// A NEC uPD7725 / uPD96050 DSP instance.
pub struct Upd96050 {
    revision: Revision,
    program_rom: Box<[u32; 16384]>, // 24-bit words
    data_rom: Box<[u16; 2048]>,
    data_ram: Box<[u16; 2048]>,
    regs: Registers,
    flags_a: Flag,
    flags_b: Flag,
}

impl Upd96050 {
    /// Create a powered-on core of the given revision with empty ROMs.
    /// Load microcode with [`Self::load_program`] / [`Self::load_data`].
    #[must_use]
    pub fn new(revision: Revision) -> Self {
        let mut core = Self {
            revision,
            program_rom: boxed_array(),
            data_rom: boxed_array(),
            data_ram: boxed_array(),
            regs: Registers::default(),
            flags_a: Flag::default(),
            flags_b: Flag::default(),
        };
        core.power();
        core
    }

    /// Reset to power-on state (ROM contents preserved).
    pub fn power(&mut self) {
        let (pc, rp, dp) = match self.revision {
            Revision::Upd7725 => (11, 10, 8),
            Revision::Upd96050 => (14, 11, 11),
        };
        self.regs = Registers {
            pc: Vn::new(pc),
            rp: Vn::new(rp),
            dp: Vn::new(dp),
            ..Registers::default()
        };
        self.flags_a = Flag::default();
        self.flags_b = Flag::default();
    }

    /// Fill the program ROM (24-bit words, masked).
    pub fn load_program(&mut self, words: &[u32]) {
        for (slot, &w) in self.program_rom.iter_mut().zip(words) {
            *slot = w & 0x00FF_FFFF;
        }
    }

    /// Fill the data ROM (16-bit words).
    pub fn load_data(&mut self, words: &[u16]) {
        for (slot, &w) in self.data_rom.iter_mut().zip(words) {
            *slot = w;
        }
    }

    // ── External CPU data ports (ares processor/upd96050/memory.cpp) ──

    /// Read `$SR` — the high byte of the 16-bit status register.
    #[must_use]
    pub const fn read_sr(&self) -> u8 {
        (self.regs.sr.to_u16() >> 8) as u8
    }

    /// Write `$SR` — a no-op on hardware (status is read-only to the CPU).
    pub const fn write_sr(&mut self, _data: u8) {}

    /// Read `$DR` (the CPU↔DSP data port), advancing the 8/16-bit handshake.
    pub const fn read_dr(&mut self) -> u8 {
        if self.regs.sr.drc {
            // 8-bit
            self.regs.sr.rqm = false;
            self.regs.dr as u8
        } else if self.regs.sr.drs {
            // 16-bit, high byte
            self.regs.sr.rqm = false;
            self.regs.sr.drs = false;
            (self.regs.dr >> 8) as u8
        } else {
            // 16-bit, low byte
            self.regs.sr.drs = true;
            self.regs.dr as u8
        }
    }

    /// Write `$DR`, advancing the 8/16-bit handshake.
    pub fn write_dr(&mut self, data: u8) {
        if self.regs.sr.drc {
            // 8-bit
            self.regs.sr.rqm = false;
            self.regs.dr = (self.regs.dr & 0xFF00) | u16::from(data);
        } else if self.regs.sr.drs {
            // 16-bit, high byte
            self.regs.sr.rqm = false;
            self.regs.sr.drs = false;
            self.regs.dr = (u16::from(data) << 8) | (self.regs.dr & 0x00FF);
        } else {
            // 16-bit, low byte
            self.regs.sr.drs = true;
            self.regs.dr = (self.regs.dr & 0xFF00) | u16::from(data);
        }
    }

    /// Read a byte of data RAM via the external `DP` port (used by the
    /// larger DSPs that map data RAM into the SNES bus; DSP-1 does not).
    #[must_use]
    pub fn read_dp(&self, address: u16) -> u8 {
        let hi = address & 1 != 0;
        let a = ((address >> 1) & 2047) as usize;
        if hi {
            (self.data_ram[a] >> 8) as u8
        } else {
            self.data_ram[a] as u8
        }
    }

    /// Write a byte of data RAM via the external `DP` port.
    pub fn write_dp(&mut self, address: u16, data: u8) {
        let hi = address & 1 != 0;
        let a = ((address >> 1) & 2047) as usize;
        if hi {
            self.data_ram[a] = (self.data_ram[a] & 0x00FF) | (u16::from(data) << 8);
        } else {
            self.data_ram[a] = (self.data_ram[a] & 0xFF00) | u16::from(data);
        }
    }

    // ── Introspection (debugger / tests) ──

    /// `RQM` — set when the DSP is waiting on the master (data ready or
    /// awaited). Games poll this between `$DR` accesses.
    #[must_use]
    pub const fn rqm(&self) -> bool {
        self.regs.sr.rqm
    }
    /// Current program counter.
    #[must_use]
    pub const fn pc(&self) -> u16 {
        self.regs.pc.get()
    }
    /// Status register as a 16-bit value.
    #[must_use]
    pub const fn sr(&self) -> u16 {
        self.regs.sr.to_u16()
    }
    /// Accumulator A.
    #[must_use]
    pub const fn a(&self) -> i16 {
        self.regs.a
    }
    /// Accumulator B.
    #[must_use]
    pub const fn b(&self) -> i16 {
        self.regs.b
    }
    /// Data register (`DR`).
    #[must_use]
    pub const fn dr(&self) -> u16 {
        self.regs.dr
    }
    /// Data RAM contents (256 words for DSP-1, up to 2048 for uPD96050).
    #[must_use]
    pub fn data_ram(&self) -> &[u16] {
        &self.data_ram[..]
    }

    // ── Execution (ares processor/upd96050/instructions.cpp) ──

    /// Execute one instruction (fetch at `pc`, then the unconditional
    /// `K*L` multiply that updates `M`/`N`).
    pub fn exec(&mut self) {
        let pc = self.regs.pc.post_inc();
        let opcode = self.program_rom[pc as usize];
        match opcode >> 22 {
            0 => self.exec_op(opcode),
            1 => self.exec_rt(opcode),
            2 => self.exec_jp(opcode),
            _ => self.exec_ld(opcode), // 3
        }
        let result = i32::from(self.regs.k) * i32::from(self.regs.l);
        self.regs.m = (result >> 15) as i16; // sign + top 15 bits
        self.regs.n = (result << 1) as i16; // low 15 bits + 0
    }

    fn exec_op(&mut self, opcode: u32) {
        let pselect = (opcode >> 20) & 0x3;
        let alu = (opcode >> 16) & 0xF;
        let asl = (opcode >> 15) & 0x1;
        let dpl = (opcode >> 13) & 0x3;
        let dphm = (opcode >> 9) & 0xF;
        let rpdcr = (opcode >> 8) & 0x1;
        let src = (opcode >> 4) & 0xF;
        let dst = opcode & 0xF;

        let idb: u16 = match src {
            0 => self.regs.trb,
            1 => self.regs.a as u16,
            2 => self.regs.b as u16,
            3 => self.regs.tr,
            4 => self.regs.dp.get(),
            5 => self.regs.rp.get(),
            6 => self.data_rom[self.regs.rp.get() as usize],
            7 => 0x8000u16.wrapping_sub(u16::from(self.flags_a.s1)),
            8 => {
                self.regs.sr.rqm = true;
                self.regs.dr
            }
            9 => self.regs.dr,
            10 => self.regs.sr.to_u16(),
            11 | 12 => self.regs.si,
            13 => self.regs.k as u16,
            14 => self.regs.l as u16,
            _ => self.data_ram[self.regs.dp.get() as usize], // 15
        };

        if alu != 0 {
            let mut p: u16 = match pselect {
                0 => self.data_ram[self.regs.dp.get() as usize],
                1 => idb,
                2 => self.regs.m as u16,
                _ => self.regs.n as u16, // 3
            };
            let (q, mut flag, carry) = if asl == 0 {
                (self.regs.a as u16, self.flags_a, self.flags_b.c)
            } else {
                (self.regs.b as u16, self.flags_b, self.flags_a.c)
            };
            let c = u16::from(carry);

            let r: u16 = match alu {
                1 => q | p,
                2 => q & p,
                3 => q ^ p,
                4 => q.wrapping_sub(p),
                5 => q.wrapping_add(p),
                6 => q.wrapping_sub(p).wrapping_sub(c),
                7 => q.wrapping_add(p).wrapping_add(c),
                8 => {
                    p = 1;
                    q.wrapping_sub(1)
                }
                9 => {
                    p = 1;
                    q.wrapping_add(1)
                }
                10 => !q,
                11 => (q >> 1) | (q & 0x8000),
                12 => (q << 1) | c,
                13 => (q << 2) | 3,
                14 => (q << 4) | 15,
                _ => q.rotate_right(8), // 15 XCHG
            };

            flag.z = r == 0;
            flag.s0 = r & 0x8000 != 0;
            if !flag.ov1 {
                flag.s1 = flag.s0;
            }

            match alu {
                1 | 2 | 3 | 10 | 13 | 14 | 15 => {
                    flag.ov0 = false;
                    flag.ov1 = false;
                    flag.c = false;
                }
                11 => {
                    flag.ov0 = false;
                    flag.ov1 = false;
                    flag.c = q & 1 != 0;
                }
                12 => {
                    flag.ov0 = false;
                    flag.ov1 = false;
                    flag.c = q >> 15 != 0;
                }
                _ => {
                    // 4..=9: SUB/ADD/SBB/ADC/DEC/INC
                    let carries = q ^ p ^ r;
                    let rhs = if alu & 1 != 0 { r } else { q };
                    let overflow = (q ^ r) & (p ^ rhs);
                    flag.ov0 = overflow & 0x8000 != 0;
                    flag.ov1 = if flag.ov0 && flag.ov1 {
                        flag.s0 == flag.s1
                    } else {
                        flag.ov0 || flag.ov1
                    };
                    flag.c = (carries ^ overflow) & 0x8000 != 0;
                }
            }

            if asl == 0 {
                self.regs.a = r as i16;
                self.flags_a = flag;
            } else {
                self.regs.b = r as i16;
                self.flags_b = flag;
            }
        }

        // The OP "move" field is an embedded LD.
        self.exec_ld((u32::from(idb) << 6) | dst);

        if dst != 4 {
            let dp = self.regs.dp.get();
            let modified = match dpl {
                1 => (dp & 0xF0) + (dp.wrapping_add(1) & 0x0F), // DPINC
                2 => (dp & 0xF0) + (dp.wrapping_sub(1) & 0x0F), // DPDEC
                3 => dp & 0xF0,                                 // DPCLR
                _ => dp,
            };
            self.regs.dp.set(modified ^ ((dphm as u16) << 4));
        }
        if dst != 5 && rpdcr != 0 {
            self.regs.rp.dec();
        }
    }

    fn exec_rt(&mut self, opcode: u32) {
        self.exec_op(opcode);
        self.regs.sp = self.regs.sp.wrapping_sub(1) & 0x0F;
        self.regs.pc.set(self.regs.stack[self.regs.sp as usize]);
    }

    fn exec_jp(&mut self, opcode: u32) {
        let brch = (opcode >> 13) & 0x1FF;
        let na = ((opcode >> 2) & 0x7FF) as u16;
        let bank = ((opcode) & 0x3) as u16;
        let jp = (self.regs.pc.get() & 0x2000) | (bank << 11) | na;

        let cond = |b: bool| if b { Some(jp) } else { None };
        let target = match brch {
            0x000 => Some(self.regs.so), // JMPSO

            0x080 => cond(!self.flags_a.c),
            0x082 => cond(self.flags_a.c),
            0x084 => cond(!self.flags_b.c),
            0x086 => cond(self.flags_b.c),

            0x088 => cond(!self.flags_a.z),
            0x08a => cond(self.flags_a.z),
            0x08c => cond(!self.flags_b.z),
            0x08e => cond(self.flags_b.z),

            0x090 => cond(!self.flags_a.ov0),
            0x092 => cond(self.flags_a.ov0),
            0x094 => cond(!self.flags_b.ov0),
            0x096 => cond(self.flags_b.ov0),

            0x098 => cond(!self.flags_a.ov1),
            0x09a => cond(self.flags_a.ov1),
            0x09c => cond(!self.flags_b.ov1),
            0x09e => cond(self.flags_b.ov1),

            0x0a0 => cond(!self.flags_a.s0),
            0x0a2 => cond(self.flags_a.s0),
            0x0a4 => cond(!self.flags_b.s0),
            0x0a6 => cond(self.flags_b.s0),

            0x0a8 => cond(!self.flags_a.s1),
            0x0aa => cond(self.flags_a.s1),
            0x0ac => cond(!self.flags_b.s1),
            0x0ae => cond(self.flags_b.s1),

            0x0b0 => cond(self.regs.dp.get() & 0x0F == 0x00),
            0x0b1 => cond(self.regs.dp.get() & 0x0F != 0x00),
            0x0b2 => cond(self.regs.dp.get() & 0x0F == 0x0F),
            0x0b3 => cond(self.regs.dp.get() & 0x0F != 0x0F),

            0x0b4 => cond(!self.regs.sr.siack),
            0x0b6 => cond(self.regs.sr.siack),
            0x0b8 => cond(!self.regs.sr.soack),
            0x0ba => cond(self.regs.sr.soack),

            0x0bc => cond(!self.regs.sr.rqm),
            0x0be => cond(self.regs.sr.rqm),

            0x100 => Some(jp & !0x2000), // LJMP
            0x101 => Some(jp | 0x2000),  // HJMP

            0x140 => {
                // LCALL
                self.regs.stack[self.regs.sp as usize] = self.regs.pc.get();
                self.regs.sp = self.regs.sp.wrapping_add(1) & 0x0F;
                Some(jp & !0x2000)
            }
            0x141 => {
                // HCALL
                self.regs.stack[self.regs.sp as usize] = self.regs.pc.get();
                self.regs.sp = self.regs.sp.wrapping_add(1) & 0x0F;
                Some(jp | 0x2000)
            }
            _ => None,
        };
        if let Some(t) = target {
            self.regs.pc.set(t);
        }
    }

    fn exec_ld(&mut self, opcode: u32) {
        let id = ((opcode >> 6) & 0xFFFF) as u16;
        let dst = opcode & 0xF;
        match dst {
            0 => {}
            1 => self.regs.a = id as i16,
            2 => self.regs.b = id as i16,
            3 => self.regs.tr = id,
            4 => self.regs.dp.set(id),
            5 => self.regs.rp.set(id),
            6 => {
                self.regs.dr = id;
                self.regs.sr.rqm = true;
            }
            7 => {
                let cur = self.regs.sr.to_u16();
                self.regs.sr.set_bits((cur & 0x907C) | (id & !0x907C));
            }
            8 | 9 => self.regs.so = id,
            10 => self.regs.k = id as i16,
            11 => {
                self.regs.k = id as i16;
                self.regs.l = self.data_rom[self.regs.rp.get() as usize] as i16;
            }
            12 => {
                self.regs.l = id as i16;
                self.regs.k = self.data_ram[(self.regs.dp.get() | 0x40) as usize] as i16;
            }
            13 => self.regs.l = id as i16,
            14 => self.regs.trb = id,
            _ => self.data_ram[self.regs.dp.get() as usize] = id, // 15
        }
    }
}

/// Allocate a zeroed boxed fixed array without a large stack temporary.
fn boxed_array<T: Copy + Default, const N: usize>() -> Box<[T; N]> {
    vec![T::default(); N]
        .into_boxed_slice()
        .try_into()
        .ok()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    // LD instruction: bits 23:22 = 3, id = bits 21:6, dst = bits 3:0.
    const fn ld(id: u16, dst: u32) -> u32 {
        (3 << 22) | ((id as u32) << 6) | dst
    }
    // JP instruction: bits 23:22 = 2, brch = bits 21:13, na = bits 12:2.
    const fn jp(brch: u32, na: u32) -> u32 {
        (2 << 22) | (brch << 13) | (na << 2)
    }

    fn dsp() -> Upd96050 {
        Upd96050::new(Revision::Upd7725)
    }

    #[test]
    fn ld_immediate_to_accumulator() {
        let mut d = dsp();
        d.load_program(&[ld(0x1234, 1)]); // LD #$1234, A
        d.exec();
        assert_eq!(d.a() as u16, 0x1234);
        assert_eq!(d.pc(), 1);
    }

    #[test]
    fn kl_multiply_updates_m_and_n() {
        let mut d = dsp();
        // LD #$4000,K (dst 10) ; LD #2,L (dst 13)
        d.load_program(&[ld(0x4000, 10), ld(0x0002, 13)]);
        d.exec();
        d.exec();
        // K*L = 0x4000 * 2 = 0x8000 → M = 0x8000>>15 = 1, N = (0x8000<<1) as i16 = 0.
        assert_eq!(d.regs.m, 1);
        assert_eq!(d.regs.n, 0);
    }

    #[test]
    fn ljmp_sets_pc() {
        let mut d = dsp();
        d.load_program(&[jp(0x100, 5)]); // LJMP $005
        d.exec();
        assert_eq!(d.pc(), 5);
    }

    #[test]
    fn dr_handshake_16bit_low_then_high() {
        let mut d = dsp();
        // LD #$ABCD, DR (dst 6) — also sets RQM.
        d.load_program(&[ld(0xABCD, 6)]);
        d.exec();
        assert!(d.rqm());
        assert_eq!(d.read_dr(), 0xCD); // low byte first
        assert!(d.rqm(), "RQM still set mid 16-bit read");
        assert_eq!(d.read_dr(), 0xAB); // high byte clears RQM
        assert!(!d.rqm());
    }

    #[test]
    fn op_add_accumulator() {
        let mut d = dsp();
        // LD #$0005, A ; then OP: ALU=ADD(5), P=IDB(pselect 1), src=K, dst=0.
        // Set K=3 first so IDB (src 13 = K) feeds P.
        // Program: LD #3,K ; LD #5,A ; OP ADD A += K.
        // OP (bits 23:22 = 0): pselect=1 (P=IDB), ALU=5 (ADD), asl=0 (acc A),
        // src=13 (K), dst=0.
        let op_add = (1 << 20) | (5 << 16) | (13 << 4);
        d.load_program(&[ld(3, 10), ld(5, 1), op_add]);
        d.exec(); // K = 3
        d.exec(); // A = 5
        d.exec(); // A = 5 + 3 = 8
        assert_eq!(d.a(), 8);
    }
}
