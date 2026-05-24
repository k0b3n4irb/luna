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
use luna_bus::{Addr24, Bus, MCycles, make_addr};

/// Master-cycle cost of one "WAI poll" step. The 65C816 in WAI does
/// nothing useful, but the system master clock keeps running — so
/// the scheduler must keep advancing or NMI never fires. Anything
/// `> 0` works; 8 mclk (one slow bus access) keeps WAI loops cheap
/// while still letting a frame complete in O(scanlines × dots).
const WAI_TICK_MCYCLES: MCycles = 8;

impl Cpu {
    /// Execute one instruction: fetch opcode at `PB:PC` and dispatch.
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.stopped {
            return;
        }
        // WAI: the CPU pauses until an interrupt arrives. A latched NMI
        // wakes us up; otherwise we keep the CPU clock running so the
        // scheduler can advance through scanlines and eventually deliver
        // an NMI at VBlank (line 225 on NTSC). Skipping the io_cycle
        // here would deadlock games that sit in a `WAI; BRA -3` VBlank-
        // wait loop — they'd never get an NMI because the scheduler
        // would never reach line 225. 8 mclk ≈ one "slow" bus tick; the
        // exact value doesn't matter as long as it's > 0.
        if self.waiting {
            // Both NMI and (unmasked) IRQ wake the CPU from WAI. The
            // service of either fires below in the normal interrupt
            // path; here we just clear the waiting flag.
            if self.pending_nmi || (self.pending_irq && !self.p.contains(bit::I)) {
                self.waiting = false;
            } else {
                bus.io_cycle(WAI_TICK_MCYCLES);
                return;
            }
        }
        // 65C816 invariant: in emulation mode, the high byte of S is
        // always $01. The Tom Harte test suite supplies arbitrary
        // initial SP values, so we normalize at the START of every
        // step — that way pushes inside the instruction write to the
        // right page, and operations that read SP directly (TSC,
        // stack-relative addressing) see the canonical value.
        if self.e {
            self.sp = 0x0100 | (self.sp & 0x00FF);
        }
        // Service a pending NMI BEFORE fetching the next opcode. NMI
        // is edge-triggered; we consume the latch and run the standard
        // 65C816 NMI sequence (push PB(native)/PC/P, jump to vector).
        if self.pending_nmi {
            self.pending_nmi = false;
            self.service_nmi(bus);
            if self.e {
                self.sp = 0x0100 | (self.sp & 0x00FF);
            }
            return;
        }
        // IRQ is checked after NMI (NMI always wins) and only fires
        // when the `I` mask flag is clear.
        if self.pending_irq && !self.p.contains(bit::I) {
            self.pending_irq = false;
            self.service_irq(bus);
            if self.e {
                self.sp = 0x0100 | (self.sp & 0x00FF);
            }
            return;
        }
        let opcode = self.fetch_u8(bus);
        self.execute(opcode, bus);
        // Defensive re-pin at end-of-step in case any inner sequence
        // momentarily violated the invariant.
        if self.e {
            self.sp = 0x0100 | (self.sp & 0x00FF);
        }
    }

    /// Run the NMI service sequence.
    ///
    /// Pushes PB (native only), PC and P; sets I, clears D; jumps to
    /// the NMI vector ($FFEA native / $FFFA emulation). The pushed P
    /// has the B bit CLEARED — that's how the handler distinguishes a
    /// BRK from an NMI/IRQ in emulation mode.
    fn service_nmi<B: Bus>(&mut self, bus: &mut B) {
        self.service_software_interrupt(
            bus, /* vec_native */ 0xFFEA, /* vec_emulation */ 0xFFFA,
            /* set_b_bit_in_emulation */ false,
        );
    }

    /// Run the IRQ service sequence — identical to NMI but jumps
    /// through the IRQ vector ($FFEE native / $FFFE emulation).
    /// The B bit is CLEARED on the pushed status so the handler can
    /// tell apart a BRK from an IRQ/NMI in emulation mode.
    fn service_irq<B: Bus>(&mut self, bus: &mut B) {
        self.service_software_interrupt(
            bus, /* vec_native */ 0xFFEE, /* vec_emulation */ 0xFFFE,
            /* set_b_bit_in_emulation */ false,
        );
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
            // Jumps & calls
            // -----------------------------------------------------------
            0x4C => self.jmp_abs(bus),
            0x5C => self.jmp_long(bus),
            0x6C => self.jmp_abs_indirect(bus),
            0xDC => self.jmp_abs_indirect_long(bus),
            0x7C => self.jmp_abs_indexed_indirect(bus),
            0x20 => self.jsr_abs(bus),
            0x22 => self.jsl_long(bus),
            0xFC => self.jsr_abs_indexed_indirect(bus),
            0x60 => self.rts(bus),
            0x6B => self.rtl(bus),
            0x82 => self.brl(bus),

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
            // Increment / decrement memory (read-modify-write, M-width)
            // -----------------------------------------------------------
            0xE6 => self.inc_dp(bus),
            0xEE => self.inc_abs(bus),
            0xF6 => self.inc_dp_x(bus),
            0xFE => self.inc_abs_x(bus),
            0xC6 => self.dec_dp(bus),
            0xCE => self.dec_abs(bus),
            0xD6 => self.dec_dp_x(bus),
            0xDE => self.dec_abs_x(bus),

            // -----------------------------------------------------------
            // Increment / decrement index registers (X-width)
            // -----------------------------------------------------------
            0xE8 => self.inx(),
            0xC8 => self.iny(),
            0xCA => self.dex(),
            0x88 => self.dey(),

            // -----------------------------------------------------------
            // Arithmetic — ADC (Add with Carry, binary mode only for now)
            // -----------------------------------------------------------
            0x69 => self.adc_imm(bus),
            0x65 => self.adc_dp(bus),
            0x67 => self.adc_dp_indirect_long(bus),
            0x6D => self.adc_abs(bus),
            0x6F => self.adc_long(bus),
            0x75 => self.adc_dp_x(bus),
            0x72 => self.adc_dp_indirect(bus),
            0x77 => self.adc_dp_indirect_long_y(bus),
            0x7D => self.adc_abs_x(bus),
            0x7F => self.adc_long_x(bus),
            0x79 => self.adc_abs_y(bus),
            0x71 => self.adc_dp_indirect_y(bus),
            0x63 => self.adc_sr_s(bus),
            0x73 => self.adc_sr_s_y(bus),
            0x61 => self.adc_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Arithmetic — SBC (Subtract with Carry, binary mode only)
            // -----------------------------------------------------------
            0xE9 => self.sbc_imm(bus),
            0xE5 => self.sbc_dp(bus),
            0xE7 => self.sbc_dp_indirect_long(bus),
            0xED => self.sbc_abs(bus),
            0xEF => self.sbc_long(bus),
            0xF5 => self.sbc_dp_x(bus),
            0xF2 => self.sbc_dp_indirect(bus),
            0xF7 => self.sbc_dp_indirect_long_y(bus),
            0xFD => self.sbc_abs_x(bus),
            0xFF => self.sbc_long_x(bus),
            0xF9 => self.sbc_abs_y(bus),
            0xF1 => self.sbc_dp_indirect_y(bus),
            0xE3 => self.sbc_sr_s(bus),
            0xF3 => self.sbc_sr_s_y(bus),
            0xE1 => self.sbc_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Comparisons — CMP (vs A, M-width)
            // -----------------------------------------------------------
            0xC9 => self.cmp_imm(bus),
            0xC5 => self.cmp_dp(bus),
            0xC7 => self.cmp_dp_indirect_long(bus),
            0xCD => self.cmp_abs(bus),
            0xCF => self.cmp_long(bus),
            0xD5 => self.cmp_dp_x(bus),
            0xD2 => self.cmp_dp_indirect(bus),
            0xD7 => self.cmp_dp_indirect_long_y(bus),
            0xDD => self.cmp_abs_x(bus),
            0xDF => self.cmp_long_x(bus),
            0xD9 => self.cmp_abs_y(bus),
            0xD1 => self.cmp_dp_indirect_y(bus),
            0xC3 => self.cmp_sr_s(bus),
            0xD3 => self.cmp_sr_s_y(bus),
            0xC1 => self.cmp_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Comparisons — CPX, CPY (X-width)
            // -----------------------------------------------------------
            0xE0 => self.cpx_imm(bus),
            0xE4 => self.cpx_dp(bus),
            0xEC => self.cpx_abs(bus),
            0xC0 => self.cpy_imm(bus),
            0xC4 => self.cpy_dp(bus),
            0xCC => self.cpy_abs(bus),

            // -----------------------------------------------------------
            // Logical — AND (M-width, sets N/Z)
            // -----------------------------------------------------------
            0x29 => self.and_imm(bus),
            0x25 => self.and_dp(bus),
            0x27 => self.and_dp_indirect_long(bus),
            0x2D => self.and_abs(bus),
            0x2F => self.and_long(bus),
            0x35 => self.and_dp_x(bus),
            0x32 => self.and_dp_indirect(bus),
            0x37 => self.and_dp_indirect_long_y(bus),
            0x3D => self.and_abs_x(bus),
            0x3F => self.and_long_x(bus),
            0x39 => self.and_abs_y(bus),
            0x31 => self.and_dp_indirect_y(bus),
            0x23 => self.and_sr_s(bus),
            0x33 => self.and_sr_s_y(bus),
            0x21 => self.and_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Logical — ORA (M-width, sets N/Z)
            // -----------------------------------------------------------
            0x09 => self.ora_imm(bus),
            0x05 => self.ora_dp(bus),
            0x07 => self.ora_dp_indirect_long(bus),
            0x0D => self.ora_abs(bus),
            0x0F => self.ora_long(bus),
            0x15 => self.ora_dp_x(bus),
            0x12 => self.ora_dp_indirect(bus),
            0x17 => self.ora_dp_indirect_long_y(bus),
            0x1D => self.ora_abs_x(bus),
            0x1F => self.ora_long_x(bus),
            0x19 => self.ora_abs_y(bus),
            0x11 => self.ora_dp_indirect_y(bus),
            0x03 => self.ora_sr_s(bus),
            0x13 => self.ora_sr_s_y(bus),
            0x01 => self.ora_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Logical — EOR (M-width, sets N/Z)
            // -----------------------------------------------------------
            0x49 => self.eor_imm(bus),
            0x45 => self.eor_dp(bus),
            0x47 => self.eor_dp_indirect_long(bus),
            0x4D => self.eor_abs(bus),
            0x4F => self.eor_long(bus),
            0x55 => self.eor_dp_x(bus),
            0x52 => self.eor_dp_indirect(bus),
            0x57 => self.eor_dp_indirect_long_y(bus),
            0x5D => self.eor_abs_x(bus),
            0x5F => self.eor_long_x(bus),
            0x59 => self.eor_abs_y(bus),
            0x51 => self.eor_dp_indirect_y(bus),
            0x43 => self.eor_sr_s(bus),
            0x53 => self.eor_sr_s_y(bus),
            0x41 => self.eor_dp_x_indirect(bus),

            // -----------------------------------------------------------
            // Logical — BIT (test bits, special flag semantics)
            // -----------------------------------------------------------
            0x89 => self.bit_imm(bus),
            0x24 => self.bit_dp(bus),
            0x2C => self.bit_abs(bus),
            0x34 => self.bit_dp_x(bus),
            0x3C => self.bit_abs_x(bus),

            // -----------------------------------------------------------
            // Shifts — ASL (Arithmetic Shift Left)
            // -----------------------------------------------------------
            0x0A => self.asl_a(),
            0x06 => self.asl_dp(bus),
            0x0E => self.asl_abs(bus),
            0x16 => self.asl_dp_x(bus),
            0x1E => self.asl_abs_x(bus),

            // -----------------------------------------------------------
            // Shifts — LSR (Logical Shift Right)
            // -----------------------------------------------------------
            0x4A => self.lsr_a(),
            0x46 => self.lsr_dp(bus),
            0x4E => self.lsr_abs(bus),
            0x56 => self.lsr_dp_x(bus),
            0x5E => self.lsr_abs_x(bus),

            // -----------------------------------------------------------
            // Shifts — ROL (Rotate Left through Carry)
            // -----------------------------------------------------------
            0x2A => self.rol_a(),
            0x26 => self.rol_dp(bus),
            0x2E => self.rol_abs(bus),
            0x36 => self.rol_dp_x(bus),
            0x3E => self.rol_abs_x(bus),

            // -----------------------------------------------------------
            // Shifts — ROR (Rotate Right through Carry)
            // -----------------------------------------------------------
            0x6A => self.ror_a(),
            0x66 => self.ror_dp(bus),
            0x6E => self.ror_abs(bus),
            0x76 => self.ror_dp_x(bus),
            0x7E => self.ror_abs_x(bus),

            // -----------------------------------------------------------
            // TSB / TRB — test-and-{set,reset} bits in memory
            // -----------------------------------------------------------
            0x04 => self.tsb_dp(bus),
            0x0C => self.tsb_abs(bus),
            0x14 => self.trb_dp(bus),
            0x1C => self.trb_abs(bus),

            // -----------------------------------------------------------
            // Stack — push / pull / effective address pushes
            // -----------------------------------------------------------
            0x48 => self.pha(bus),
            0xDA => self.phx(bus),
            0x5A => self.phy(bus),
            0x08 => self.php(bus),
            0x8B => self.phb(bus),
            0x0B => self.phd(bus),
            0x4B => self.phk(bus),
            0x68 => self.pla(bus),
            0xFA => self.plx(bus),
            0x7A => self.ply(bus),
            0x28 => self.plp(bus),
            0xAB => self.plb(bus),
            0x2B => self.pld(bus),
            0xF4 => self.pea(bus),
            0xD4 => self.pei(bus),
            0x62 => self.per(bus),

            // -----------------------------------------------------------
            // Inter-register transfers
            // -----------------------------------------------------------
            0xAA => self.tax(),
            0xA8 => self.tay(),
            0x8A => self.txa(),
            0x98 => self.tya(),
            0x9B => self.txy(),
            0xBB => self.tyx(),
            0xBA => self.tsx(),
            0x9A => self.txs(),
            0x5B => self.tcd(),
            0x7B => self.tdc(),
            0x1B => self.tcs(),
            0x3B => self.tsc(),
            0xEB => self.xba(),

            // -----------------------------------------------------------
            // Misc
            // -----------------------------------------------------------
            0xEA => { /* NOP */ }
            0xCB => self.waiting = true, // WAI
            0xDB => self.stopped = true, // STP

            // -----------------------------------------------------------
            // Interrupts & misc
            // -----------------------------------------------------------
            0x00 => self.brk(bus),
            0x02 => self.cop(bus),
            0x40 => self.rti(bus),
            0x42 => self.wdm(bus),

            // -----------------------------------------------------------
            // Block moves
            // -----------------------------------------------------------
            0x54 => self.mvn(bus),
            0x44 => self.mvp(bus),
            // All 256 opcode values are explicitly handled above.
            // The compiler validates exhaustiveness; no catch-all needed.
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

    /// `JMP ($abs)` — read 16-bit pointer at $00:operand, jump to it
    /// (program bank stays the same).
    fn jmp_abs_indirect<B: Bus>(&mut self, bus: &mut B) {
        let ptr_off = self.fetch_u16(bus);
        let lo = bus.read(make_addr(0, ptr_off));
        let hi = bus.read(make_addr(0, ptr_off.wrapping_add(1)));
        self.pc = u16::from(lo) | (u16::from(hi) << 8);
    }

    /// `JMP [$abs]` — read 24-bit pointer at $00:operand, jump to it
    /// (program bank changes).
    fn jmp_abs_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let ptr_off = self.fetch_u16(bus);
        let lo = bus.read(make_addr(0, ptr_off));
        let mid = bus.read(make_addr(0, ptr_off.wrapping_add(1)));
        let hi = bus.read(make_addr(0, ptr_off.wrapping_add(2)));
        self.pc = u16::from(lo) | (u16::from(mid) << 8);
        self.pb = hi;
    }

    /// `JMP ($abs,X)` — pointer is fetched from `PB:(operand + X)`.
    /// Program bank stays the same.
    fn jmp_abs_indexed_indirect<B: Bus>(&mut self, bus: &mut B) {
        let base = self.fetch_u16(bus);
        let ptr_off = base.wrapping_add(self.x);
        let lo = bus.read(make_addr(self.pb, ptr_off));
        let hi = bus.read(make_addr(self.pb, ptr_off.wrapping_add(1)));
        self.pc = u16::from(lo) | (u16::from(hi) << 8);
    }

    /// `JSR $abs` — push (PC - 1) at the post-fetch PC (so the return
    /// address points at the **last byte of the JSR instruction**, and
    /// RTS will increment by 1 to land on the next instruction).
    fn jsr_abs<B: Bus>(&mut self, bus: &mut B) {
        let target = self.fetch_u16(bus);
        let return_addr = self.pc.wrapping_sub(1);
        self.push_u16(bus, return_addr);
        self.pc = target;
    }

    /// `JSL $long` — push the program bank, then push PC-1 (16-bit),
    /// then jump to the 24-bit target. RTL undoes the push order.
    fn jsl_long<B: Bus>(&mut self, bus: &mut B) {
        let target = self.fetch_u24(bus);
        let return_pc = self.pc.wrapping_sub(1);
        self.push_u8(bus, self.pb);
        self.push_u16(bus, return_pc);
        self.pb = (target >> 16) as u8;
        self.pc = target as u16;
    }

    /// `JSR ($abs,X)` — like JMP (abs,X) but pushes the return address.
    /// IMPORTANT: per the 65C816 manual, the return address is pushed
    /// **before** the operand is read for X-indirection (so PC points
    /// at the last byte of the JSR operand, not past it).
    fn jsr_abs_indexed_indirect<B: Bus>(&mut self, bus: &mut B) {
        let base = self.fetch_u16(bus);
        // Return address = current PC minus 1 (pointing at the high byte
        // of the operand we just fetched).
        let return_addr = self.pc.wrapping_sub(1);
        self.push_u16(bus, return_addr);
        let ptr_off = base.wrapping_add(self.x);
        let lo = bus.read(make_addr(self.pb, ptr_off));
        let hi = bus.read(make_addr(self.pb, ptr_off.wrapping_add(1)));
        self.pc = u16::from(lo) | (u16::from(hi) << 8);
    }

    /// `RTS` — pull PC, increment by 1, stay in the same program bank.
    fn rts<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.pull_u16(bus);
        self.pc = pc.wrapping_add(1);
    }

    /// `RTL` — pull PC (16-bit), then PB; increment PC by 1.
    fn rtl<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.pull_u16(bus);
        let pb = self.pull_u8(bus);
        self.pc = pc.wrapping_add(1);
        self.pb = pb;
    }

    /// `BRL rel16` — branch always, with a signed 16-bit PC-relative
    /// displacement.
    fn brl<B: Bus>(&mut self, bus: &mut B) {
        let rel = self.fetch_u16(bus) as i16;
        self.pc = self.pc.wrapping_add_signed(rel);
    }

    // ===================================================================
    // BRK / COP — software interrupts
    //
    // Each is a 2-byte instruction: opcode + "signature" byte (ignored
    // by the CPU but available to the handler via the saved PC).
    //
    // Vectors (native mode):
    //   BRK: $00:FFE6/FFE7   COP: $00:FFE4/FFE5
    // Vectors (emulation mode):
    //   BRK: $00:FFFE/FFFF (shared with IRQ)   COP: $00:FFF4/FFF5
    //
    // Native mode: push PB, PC (16-bit, post-signature), P; clear PB;
    //   set I, clear D; PC ← vector.
    // Emulation mode: push PC (16-bit), P with the B (break) bit set
    //   for BRK; clear PB; set I, clear D; PC ← vector.
    // ===================================================================

    fn brk<B: Bus>(&mut self, bus: &mut B) {
        // Skip the signature byte (CPU "consumes" but doesn't use it).
        let _signature = self.fetch_u8(bus);
        self.service_software_interrupt(
            bus, /* vec_native */ 0xFFE6, /* vec_emulation */ 0xFFFE,
            /* set_b_bit_in_emulation */ true,
        );
    }

    fn cop<B: Bus>(&mut self, bus: &mut B) {
        let _signature = self.fetch_u8(bus);
        // Per fullsnes / Tom Harte, COP sets B=1 in the pushed P byte
        // in emulation mode (same as BRK). The B=0 case only applies
        // to hardware IRQ/NMI, which we don't yet service.
        self.service_software_interrupt(
            bus, /* vec_native */ 0xFFE4, /* vec_emulation */ 0xFFF4,
            /* set_b_bit_in_emulation */ true,
        );
    }

    fn service_software_interrupt<B: Bus>(
        &mut self,
        bus: &mut B,
        vec_native: u16,
        vec_emulation: u16,
        set_b_bit_in_emulation: bool,
    ) {
        if self.e {
            // Emulation mode: 6502-compatible stack frame.
            self.push_u16(bus, self.pc);
            let pushed_p = if set_b_bit_in_emulation {
                self.p.bits() | 0x10 // "B" flag set on stack for BRK
            } else {
                self.p.bits() & !0x10 // B clear for hardware-style IRQ/COP
            };
            self.push_u8(bus, pushed_p);
            let lo = bus.read(make_addr(0, vec_emulation));
            let hi = bus.read(make_addr(0, vec_emulation.wrapping_add(1)));
            self.pc = u16::from(lo) | (u16::from(hi) << 8);
        } else {
            // Native mode: full 65816 stack frame.
            self.push_u8(bus, self.pb);
            self.push_u16(bus, self.pc);
            self.push_u8(bus, self.p.bits());
            let lo = bus.read(make_addr(0, vec_native));
            let hi = bus.read(make_addr(0, vec_native.wrapping_add(1)));
            self.pc = u16::from(lo) | (u16::from(hi) << 8);
        }
        self.pb = 0;
        self.p.remove(bit::D);
        self.p.insert(bit::I);
    }

    // ===================================================================
    // RTI — return from interrupt
    //
    // Pulls P (and, in native mode, PB as well). E mode pulls the
    // standard 6502 frame.
    // ===================================================================

    fn rti<B: Bus>(&mut self, bus: &mut B) {
        let new_p = self.pull_u8(bus);
        self.pc = self.pull_u16(bus);
        // Emulation forces M and X to stay set in P.
        let effective = if self.e {
            new_p | bit::M | bit::X
        } else {
            new_p
        };
        self.p = crate::flags::StatusFlags(effective);
        if !self.e {
            self.pb = self.pull_u8(bus);
        }
        if self.p.idx8() {
            self.x &= 0x00FF;
            self.y &= 0x00FF;
        }
    }

    // ===================================================================
    // WDM — reserved opcode, treated as a 2-byte NOP.
    //
    // It still consumes one operand byte (read but ignored), preserving
    // PC alignment for any code that uses it as a placeholder.
    // ===================================================================

    fn wdm<B: Bus>(&mut self, bus: &mut B) {
        let _operand = self.fetch_u8(bus);
    }

    // ===================================================================
    // MVN / MVP — block move
    //
    // 3-byte instructions: opcode, dest_bank, src_bank.
    // Per iteration: write one byte from `src_bank:X` to `dest_bank:Y`,
    // then ±1 on X and Y, -1 on A. If A != $FFFF the PC is rewound by
    // 3 so the *next* CPU step re-executes the same instruction. This
    // matches the hardware's behavior of being interruptible mid-block
    // and is the granularity at which the Tom Harte test suite expects
    // single-step state to advance.
    //
    // MVN (Move Negative): X and Y increment; copy proceeds from low
    //   addresses to high. Despite the name "negative", the semantics
    //   are forward copy — the source/destination operands are encoded
    //   in the order dest, src in the instruction stream.
    // MVP (Move Positive): X and Y decrement; backward copy. Used to
    //   move overlapping regions toward higher addresses.
    //
    // DB is updated to dest_bank as a side effect — software relying
    // on DB after MVN/MVP must be aware.
    // ===================================================================

    /// Execute a full MVN / MVP block move in one step.
    ///
    /// The Tom Harte 65816 test suite models a single MVN/MVP test case
    /// as the **entire** block transfer (not one iteration). Each
    /// iteration writes one byte at `dest_bank:Y` from `src_bank:X`,
    /// then ±1 on X and Y, -1 on A. The loop terminates when A wraps
    /// from $0000 to $FFFF.
    ///
    /// DB is updated to `dest_bank` as a side effect. Index registers
    /// are truncated to 8-bit if the X flag is set during the move.
    fn block_move<B: Bus>(&mut self, bus: &mut B, increment: bool) {
        let dest_bank = self.fetch_u8(bus);
        let src_bank = self.fetch_u8(bus);
        self.db = dest_bank;
        loop {
            let byte = bus.read(make_addr(src_bank, self.x));
            bus.write(make_addr(dest_bank, self.y), byte);
            if increment {
                self.x = self.x.wrapping_add(1);
                self.y = self.y.wrapping_add(1);
            } else {
                self.x = self.x.wrapping_sub(1);
                self.y = self.y.wrapping_sub(1);
            }
            if self.p.idx8() {
                self.x &= 0x00FF;
                self.y &= 0x00FF;
            }
            let prev_a = self.a;
            self.a = self.a.wrapping_sub(1);
            if prev_a == 0 {
                // A just wrapped from $0000 to $FFFF — we're done.
                break;
            }
        }
    }

    fn mvn<B: Bus>(&mut self, bus: &mut B) {
        self.block_move(bus, true);
    }

    fn mvp<B: Bus>(&mut self, bus: &mut B) {
        self.block_move(bus, false);
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

    // ===================================================================
    // INC / DEC memory (read-modify-write at M-width)
    // ===================================================================

    fn modify_memory<B: Bus>(&mut self, bus: &mut B, addr: Addr24, op: fn(u16) -> u16) {
        if self.p.acc8() {
            let v = bus.read(addr);
            let new = op(u16::from(v)) as u8;
            bus.write(addr, new);
            self.set_nz8(new);
        } else {
            let v = read_word(bus, addr);
            let new = op(v);
            bus.write(addr, new as u8);
            bus.write(addr.wrapping_add(1), (new >> 8) as u8);
            self.set_nz16(new);
        }
    }

    fn inc_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_add(1));
    }
    fn inc_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_add(1));
    }
    fn inc_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_add(1));
    }
    fn inc_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_add(1));
    }
    fn dec_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_sub(1));
    }
    fn dec_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_sub(1));
    }
    fn dec_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_sub(1));
    }
    fn dec_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        self.modify_memory(bus, a, |v| v.wrapping_sub(1));
    }

    // ===================================================================
    // INX / INY / DEX / DEY  (X-width)
    // ===================================================================

    fn inx(&mut self) {
        if self.p.idx8() {
            let v = self.x8().wrapping_add(1);
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            self.x = self.x.wrapping_add(1);
            self.set_nz16(self.x);
        }
    }
    fn iny(&mut self) {
        if self.p.idx8() {
            let v = self.y8().wrapping_add(1);
            self.set_y_low(v);
            self.set_nz8(v);
        } else {
            self.y = self.y.wrapping_add(1);
            self.set_nz16(self.y);
        }
    }
    fn dex(&mut self) {
        if self.p.idx8() {
            let v = self.x8().wrapping_sub(1);
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            self.x = self.x.wrapping_sub(1);
            self.set_nz16(self.x);
        }
    }
    fn dey(&mut self) {
        if self.p.idx8() {
            let v = self.y8().wrapping_sub(1);
            self.set_y_low(v);
            self.set_nz8(v);
        } else {
            self.y = self.y.wrapping_sub(1);
            self.set_nz16(self.y);
        }
    }

    // ===================================================================
    // ADC / SBC core
    //
    // Both binary (D=0) and BCD (D=1) modes are implemented. The 65C816
    // BCD adjustment is applied per nibble (4 nibbles in 16-bit mode).
    // Reference: WDC 65C816 manual + matched against Tom Harte
    // ProcessorTests.
    // ===================================================================

    /// Add `value` to A with carry; sets N/V/Z/C per width. Dispatches
    /// between binary (D=0) and BCD (D=1) modes.
    fn adc_value(&mut self, value: u16) {
        if self.p.contains(bit::D) {
            self.adc_value_bcd(value);
        } else {
            self.adc_value_binary(value);
        }
    }

    /// Subtract `value` from A with borrow (carry inverted). Dispatches
    /// between binary (D=0) and BCD (D=1) modes.
    fn sbc_value(&mut self, value: u16) {
        if self.p.contains(bit::D) {
            self.sbc_value_bcd(value);
        } else if self.p.acc8() {
            // Binary SBC = ADC of one's complement (carry is "not borrow").
            self.adc_value_binary(u16::from(!(value as u8)));
        } else {
            self.adc_value_binary(!value);
        }
    }

    /// Binary ADC: full-width add with carry.
    fn adc_value_binary(&mut self, value: u16) {
        let c_in = u32::from(self.p.contains(bit::C));
        if self.p.acc8() {
            let a = u32::from(self.a8());
            let v = u32::from(value as u8);
            let raw = a + v + c_in;
            let result = raw as u8;
            self.p.set(bit::C, raw > 0xFF);
            let overflow = (!(a ^ v) & (a ^ u32::from(result))) & 0x80 != 0;
            self.p.set(bit::V, overflow);
            self.set_a_low(result);
            self.set_nz8(result);
        } else {
            let a = u32::from(self.a);
            let v = u32::from(value);
            let raw = a + v + c_in;
            let result = raw as u16;
            self.p.set(bit::C, raw > 0xFFFF);
            let overflow = (!(a ^ v) & (a ^ u32::from(result))) & 0x8000 != 0;
            self.p.set(bit::V, overflow);
            self.a = result;
            self.set_nz16(result);
        }
    }

    /// BCD ADC: nibble-by-nibble decimal add. Reference WDC 65C816
    /// manual §5.4 and the corresponding cases in the Tom Harte
    /// ProcessorTests dataset.
    fn adc_value_bcd(&mut self, value: u16) {
        let c_in = u32::from(self.p.contains(bit::C));
        if self.p.acc8() {
            let a = u32::from(self.a8());
            let b = u32::from(value as u8);

            // Low nibble.
            let mut lo = (a & 0xF) + (b & 0xF) + c_in;
            if lo > 9 {
                lo += 6;
            }
            // High nibble; carry from low nibble enters here.
            let mut hi = ((a >> 4) & 0xF) + ((b >> 4) & 0xF) + (lo >> 4);

            // V flag from the unadjusted-high partial result.
            let unadj = (hi << 4) | (lo & 0xF);
            let overflow = (!(a ^ b) & (a ^ unadj)) & 0x80 != 0;
            self.p.set(bit::V, overflow);

            if hi > 9 {
                hi += 6;
            }
            let result = (((hi & 0xF) << 4) | (lo & 0xF)) as u8;
            self.p.set(bit::C, hi > 0xF);
            self.set_a_low(result);
            self.set_nz8(result);
        } else {
            let a = u32::from(self.a);
            let b = u32::from(value);

            let mut n0 = (a & 0xF) + (b & 0xF) + c_in;
            if n0 > 9 {
                n0 += 6;
            }
            let mut n1 = ((a >> 4) & 0xF) + ((b >> 4) & 0xF) + (n0 >> 4);
            if n1 > 9 {
                n1 += 6;
            }
            let mut n2 = ((a >> 8) & 0xF) + ((b >> 8) & 0xF) + (n1 >> 4);
            if n2 > 9 {
                n2 += 6;
            }
            let mut n3 = ((a >> 12) & 0xF) + ((b >> 12) & 0xF) + (n2 >> 4);

            let unadj = (n3 << 12) | ((n2 & 0xF) << 8) | ((n1 & 0xF) << 4) | (n0 & 0xF);
            let overflow = (!(a ^ b) & (a ^ unadj)) & 0x8000 != 0;
            self.p.set(bit::V, overflow);

            if n3 > 9 {
                n3 += 6;
            }
            let result =
                (((n3 & 0xF) << 12) | ((n2 & 0xF) << 8) | ((n1 & 0xF) << 4) | (n0 & 0xF)) as u16;
            self.p.set(bit::C, n3 > 0xF);
            self.a = result;
            self.set_nz16(result);
        }
    }

    /// BCD SBC: nibble-by-nibble decimal subtract. Operates as a binary
    /// subtract with per-nibble adjustment when a nibble's "tens
    /// borrow" bit is set.
    fn sbc_value_bcd(&mut self, value: u16) {
        let c_in = self.p.contains(bit::C);
        let borrow = u32::from(!c_in);
        if self.p.acc8() {
            let a = u32::from(self.a8());
            let b = u32::from(value as u8);

            // Standard binary subtract first.
            let raw = a.wrapping_sub(b).wrapping_sub(borrow);

            // V flag from the raw (binary) intermediate.
            let overflow = ((a ^ b) & (a ^ raw)) & 0x80 != 0;
            self.p.set(bit::V, overflow);

            // Per-nibble BCD adjustment: if a nibble borrowed (its
            // "tens" bit is set), subtract 6 from that position.
            let mut result = raw;
            if (a & 0xF).wrapping_sub(b & 0xF).wrapping_sub(borrow) & 0x10 != 0 {
                result = result.wrapping_sub(6);
            }
            if raw & 0x100 != 0 {
                result = result.wrapping_sub(0x60);
            }

            self.p.set(bit::C, raw & 0x100 == 0);
            let result_u8 = result as u8;
            self.set_a_low(result_u8);
            self.set_nz8(result_u8);
        } else {
            let a = u32::from(self.a);
            let b = u32::from(value);

            let raw = a.wrapping_sub(b).wrapping_sub(borrow);

            let overflow = ((a ^ b) & (a ^ raw)) & 0x8000 != 0;
            self.p.set(bit::V, overflow);

            let mut result = raw;
            // Check tens borrow at each nibble boundary.
            let n0_borrow = (a & 0xF).wrapping_sub(b & 0xF).wrapping_sub(borrow);
            if n0_borrow & 0x10 != 0 {
                result = result.wrapping_sub(6);
            }
            let n1_borrow = ((a >> 4) & 0xF)
                .wrapping_sub((b >> 4) & 0xF)
                .wrapping_sub(n0_borrow >> 4 & 1);
            if n1_borrow & 0x10 != 0 {
                result = result.wrapping_sub(0x60);
            }
            let n2_borrow = ((a >> 8) & 0xF)
                .wrapping_sub((b >> 8) & 0xF)
                .wrapping_sub(n1_borrow >> 4 & 1);
            if n2_borrow & 0x10 != 0 {
                result = result.wrapping_sub(0x600);
            }
            if raw & 0x10000 != 0 {
                result = result.wrapping_sub(0x6000);
            }

            self.p.set(bit::C, raw & 0x10000 == 0);
            let result_u16 = result as u16;
            self.a = result_u16;
            self.set_nz16(result_u16);
        }
    }

    fn adc_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = if self.p.acc8() {
            u16::from(self.fetch_u8(bus))
        } else {
            self.fetch_u16(bus)
        };
        self.adc_value(v);
    }

    fn sbc_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = if self.p.acc8() {
            u16::from(self.fetch_u8(bus))
        } else {
            self.fetch_u16(bus)
        };
        self.sbc_value(v);
    }

    fn arithmetic_read_from<B: Bus>(&mut self, bus: &mut B, addr: Addr24) -> u16 {
        if self.p.acc8() {
            u16::from(bus.read(addr))
        } else {
            read_word(bus, addr)
        }
    }

    // ADC modes
    fn adc_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_long<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_long_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }
    fn adc_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.adc_value(v);
    }

    // SBC modes (same wiring, sbc_value internally)
    fn sbc_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_long<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_long_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }
    fn sbc_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.sbc_value(v);
    }

    // ===================================================================
    // Comparisons (CMP / CPX / CPY)
    //
    // Sets N / Z / C as if `reg - value` was computed; the register is
    // NOT modified. C is set when `reg >= value` (no borrow). The V flag
    // is not affected (unlike SBC).
    // ===================================================================

    fn compare_8(&mut self, reg: u8, value: u8) {
        let result = reg.wrapping_sub(value);
        self.p.set(bit::C, reg >= value);
        self.set_nz8(result);
    }

    fn compare_16(&mut self, reg: u16, value: u16) {
        let result = reg.wrapping_sub(value);
        self.p.set(bit::C, reg >= value);
        self.set_nz16(result);
    }

    /// CMP core: compare A with `value` at the M-flag width.
    fn cmp_value(&mut self, value: u16) {
        if self.p.acc8() {
            self.compare_8(self.a8(), value as u8);
        } else {
            self.compare_16(self.a, value);
        }
    }

    /// CPX/CPY core: compare an index register with `value` at the
    /// X-flag width.
    fn compare_index(&mut self, reg: u16, value: u16) {
        if self.p.idx8() {
            self.compare_8(reg as u8, value as u8);
        } else {
            self.compare_16(reg, value);
        }
    }

    // CMP — same dispatch pattern as ADC/SBC.

    fn cmp_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = if self.p.acc8() {
            u16::from(self.fetch_u8(bus))
        } else {
            self.fetch_u16(bus)
        };
        self.cmp_value(v);
    }
    fn cmp_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_long<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_long_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }
    fn cmp_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.cmp_value(v);
    }

    // CPX/CPY — only 3 addressing modes each.

    fn index_read_from<B: Bus>(&mut self, bus: &mut B, addr: Addr24) -> u16 {
        if self.p.idx8() {
            u16::from(bus.read(addr))
        } else {
            read_word(bus, addr)
        }
    }

    fn cpx_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = if self.p.idx8() {
            u16::from(self.fetch_u8(bus))
        } else {
            self.fetch_u16(bus)
        };
        self.compare_index(self.x, v);
    }
    fn cpx_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.index_read_from(bus, a);
        self.compare_index(self.x, v);
    }
    fn cpx_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.index_read_from(bus, a);
        self.compare_index(self.x, v);
    }
    fn cpy_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = if self.p.idx8() {
            u16::from(self.fetch_u8(bus))
        } else {
            self.fetch_u16(bus)
        };
        self.compare_index(self.y, v);
    }
    fn cpy_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.index_read_from(bus, a);
        self.compare_index(self.y, v);
    }
    fn cpy_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.index_read_from(bus, a);
        self.compare_index(self.y, v);
    }

    // ===================================================================
    // Logical (AND / ORA / EOR)
    //
    // All three operate on the accumulator at M-width, set N and Z based
    // on the result, and leave V/C alone.
    // ===================================================================

    fn logical_imm_fetch<B: Bus>(&mut self, bus: &mut B) -> u16 {
        if self.p.acc8() {
            u16::from(self.fetch_u8(bus))
        } else {
            self.fetch_u16(bus)
        }
    }

    fn and_value(&mut self, value: u16) {
        if self.p.acc8() {
            let v = self.a8() & (value as u8);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            let v = self.a & value;
            self.a = v;
            self.set_nz16(v);
        }
    }

    fn ora_value(&mut self, value: u16) {
        if self.p.acc8() {
            let v = self.a8() | (value as u8);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            let v = self.a | value;
            self.a = v;
            self.set_nz16(v);
        }
    }

    fn eor_value(&mut self, value: u16) {
        if self.p.acc8() {
            let v = self.a8() ^ (value as u8);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            let v = self.a ^ value;
            self.a = v;
            self.set_nz16(v);
        }
    }

    // AND dispatch.
    fn and_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = self.logical_imm_fetch(bus);
        self.and_value(v);
    }
    fn and_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_long<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_long_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }
    fn and_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.and_value(v);
    }

    // ORA dispatch.
    fn ora_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = self.logical_imm_fetch(bus);
        self.ora_value(v);
    }
    fn ora_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_long<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_long_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }
    fn ora_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.ora_value(v);
    }

    // EOR dispatch.
    fn eor_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = self.logical_imm_fetch(bus);
        self.eor_value(v);
    }
    fn eor_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_dp_indirect_long<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_long<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_dp_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_dp_indirect_long_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_long_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_abs_y<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_long_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_long_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_dp_indirect_y<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_sr_s<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_sr_s_y<B: Bus>(&mut self, bus: &mut B) {
        let a = stack_relative_indirect_y(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }
    fn eor_dp_x_indirect<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_indirect(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.eor_value(v);
    }

    // ===================================================================
    // BIT — test bits (special flag semantics)
    //
    // - Z is set based on (A & M) — like AND.
    // - N is bit 7 (or 15) of M (NOT of A & M).
    // - V is bit 6 (or 14) of M.
    // - **Exception**: BIT #imm only updates Z; N and V are unchanged.
    // ===================================================================

    fn bit_value(&mut self, value: u16, immediate: bool) {
        if self.p.acc8() {
            let v = value as u8;
            let result = self.a8() & v;
            self.p.set(bit::Z, result == 0);
            if !immediate {
                self.p.set(bit::N, v & 0x80 != 0);
                self.p.set(bit::V, v & 0x40 != 0);
            }
        } else {
            let result = self.a & value;
            self.p.set(bit::Z, result == 0);
            if !immediate {
                self.p.set(bit::N, value & 0x8000 != 0);
                self.p.set(bit::V, value & 0x4000 != 0);
            }
        }
    }

    fn bit_imm<B: Bus>(&mut self, bus: &mut B) {
        let v = self.logical_imm_fetch(bus);
        self.bit_value(v, true);
    }
    fn bit_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.bit_value(v, false);
    }
    fn bit_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.bit_value(v, false);
    }
    fn bit_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.bit_value(v, false);
    }
    fn bit_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        let v = self.arithmetic_read_from(bus, a);
        self.bit_value(v, false);
    }

    // ===================================================================
    // ASL / LSR / ROL / ROR
    //
    // Each acts on a u16 value (M-width-aware); returns the new value.
    // C is set to the bit shifted out. N and Z reflect the result.
    // Accumulator forms reuse the helper; memory forms wrap it in a
    // read-modify-write.
    // ===================================================================

    fn asl_compute(&mut self, value: u16) -> u16 {
        if self.p.acc8() {
            let v = value as u8;
            let carry = v & 0x80 != 0;
            let result = v << 1;
            self.p.set(bit::C, carry);
            self.set_nz8(result);
            u16::from(result)
        } else {
            let carry = value & 0x8000 != 0;
            let result = value << 1;
            self.p.set(bit::C, carry);
            self.set_nz16(result);
            result
        }
    }

    fn lsr_compute(&mut self, value: u16) -> u16 {
        if self.p.acc8() {
            let v = value as u8;
            let carry = v & 1 != 0;
            let result = v >> 1;
            self.p.set(bit::C, carry);
            self.set_nz8(result);
            u16::from(result)
        } else {
            let carry = value & 1 != 0;
            let result = value >> 1;
            self.p.set(bit::C, carry);
            self.set_nz16(result);
            result
        }
    }

    fn rol_compute(&mut self, value: u16) -> u16 {
        let c_in = u16::from(self.p.contains(bit::C));
        if self.p.acc8() {
            let v = value as u8;
            let carry = v & 0x80 != 0;
            let result = (v << 1) | (c_in as u8);
            self.p.set(bit::C, carry);
            self.set_nz8(result);
            u16::from(result)
        } else {
            let carry = value & 0x8000 != 0;
            let result = (value << 1) | c_in;
            self.p.set(bit::C, carry);
            self.set_nz16(result);
            result
        }
    }

    fn ror_compute(&mut self, value: u16) -> u16 {
        let c_in = self.p.contains(bit::C);
        if self.p.acc8() {
            let v = value as u8;
            let carry = v & 1 != 0;
            let result = (v >> 1) | (if c_in { 0x80 } else { 0 });
            self.p.set(bit::C, carry);
            self.set_nz8(result);
            u16::from(result)
        } else {
            let carry = value & 1 != 0;
            let result = (value >> 1) | (if c_in { 0x8000 } else { 0 });
            self.p.set(bit::C, carry);
            self.set_nz16(result);
            result
        }
    }

    fn modify_memory_with<B: Bus>(
        &mut self,
        bus: &mut B,
        addr: Addr24,
        op: fn(&mut Cpu, u16) -> u16,
    ) {
        if self.p.acc8() {
            let v = u16::from(bus.read(addr));
            let new = op(self, v) as u8;
            bus.write(addr, new);
        } else {
            let v = read_word(bus, addr);
            let new = op(self, v);
            bus.write(addr, new as u8);
            bus.write(addr.wrapping_add(1), (new >> 8) as u8);
        }
    }

    fn asl_a(&mut self) {
        let v = if self.p.acc8() {
            u16::from(self.a8())
        } else {
            self.a
        };
        let new = self.asl_compute(v);
        self.assign_a(new);
    }
    fn lsr_a(&mut self) {
        let v = if self.p.acc8() {
            u16::from(self.a8())
        } else {
            self.a
        };
        let new = self.lsr_compute(v);
        self.assign_a(new);
    }
    fn rol_a(&mut self) {
        let v = if self.p.acc8() {
            u16::from(self.a8())
        } else {
            self.a
        };
        let new = self.rol_compute(v);
        self.assign_a(new);
    }
    fn ror_a(&mut self) {
        let v = if self.p.acc8() {
            u16::from(self.a8())
        } else {
            self.a
        };
        let new = self.ror_compute(v);
        self.assign_a(new);
    }

    fn assign_a(&mut self, value: u16) {
        if self.p.acc8() {
            self.set_a_low(value as u8);
        } else {
            self.a = value;
        }
    }

    fn asl_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory_with(bus, a, Self::asl_compute);
    }
    fn asl_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory_with(bus, a, Self::asl_compute);
    }
    fn asl_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::asl_compute);
    }
    fn asl_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::asl_compute);
    }
    fn lsr_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory_with(bus, a, Self::lsr_compute);
    }
    fn lsr_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory_with(bus, a, Self::lsr_compute);
    }
    fn lsr_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::lsr_compute);
    }
    fn lsr_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::lsr_compute);
    }
    fn rol_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory_with(bus, a, Self::rol_compute);
    }
    fn rol_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory_with(bus, a, Self::rol_compute);
    }
    fn rol_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::rol_compute);
    }
    fn rol_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::rol_compute);
    }
    fn ror_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory_with(bus, a, Self::ror_compute);
    }
    fn ror_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory_with(bus, a, Self::ror_compute);
    }
    fn ror_dp_x<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::ror_compute);
    }
    fn ror_abs_x<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute_indexed_x(self, bus);
        self.modify_memory_with(bus, a, Self::ror_compute);
    }

    // ===================================================================
    // TSB / TRB — test bits in memory, then set/reset them.
    //
    // - Z is set based on (A & M), BEFORE the write.
    // - Memory becomes M | A (TSB) or M & ~A (TRB).
    // - N and V are NOT affected.
    // ===================================================================

    fn tsb_compute(&mut self, value: u16) -> u16 {
        if self.p.acc8() {
            let v = value as u8;
            let a = self.a8();
            self.p.set(bit::Z, (v & a) == 0);
            u16::from(v | a)
        } else {
            self.p.set(bit::Z, (value & self.a) == 0);
            value | self.a
        }
    }

    fn trb_compute(&mut self, value: u16) -> u16 {
        if self.p.acc8() {
            let v = value as u8;
            let a = self.a8();
            self.p.set(bit::Z, (v & a) == 0);
            u16::from(v & !a)
        } else {
            self.p.set(bit::Z, (value & self.a) == 0);
            value & !self.a
        }
    }

    fn tsb_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory_with(bus, a, Self::tsb_compute);
    }
    fn tsb_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory_with(bus, a, Self::tsb_compute);
    }
    fn trb_dp<B: Bus>(&mut self, bus: &mut B) {
        let a = direct_page(self, bus);
        self.modify_memory_with(bus, a, Self::trb_compute);
    }
    fn trb_abs<B: Bus>(&mut self, bus: &mut B) {
        let a = absolute(self, bus);
        self.modify_memory_with(bus, a, Self::trb_compute);
    }

    // ===================================================================
    // Inter-register transfers
    //
    // Width rules summary:
    // - TAX / TAY: width = X flag (the destination width). Source A is
    //   read at the SAME width — i.e. the low byte of A if X=1, else
    //   the full 16-bit A.
    // - TXA / TYA: width = M flag (the destination width). Source X / Y
    //   is read at M-width.
    // - TXY / TYX: width = X flag (both are index registers).
    // - TSX: width = X flag.
    // - TXS: in EMULATION mode, only X.low → SP.low, SP.high forced to
    //   $01. In native mode, full 16-bit transfer.
    // - TCD / TDC / TCS / TSC: **always 16-bit**, regardless of M.
    //   TCD / TCS load DP / SP from the full 16-bit accumulator (B:A).
    //   TDC / TSC load A (full 16-bit) from DP / SP.
    // - XBA: swap A.low ↔ A.high; sets N/Z from the new low byte.
    // ===================================================================

    fn tax(&mut self) {
        if self.p.idx8() {
            let v = self.a8();
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            self.x = self.a;
            self.set_nz16(self.x);
        }
    }

    fn tay(&mut self) {
        if self.p.idx8() {
            let v = self.a8();
            self.set_y_low(v);
            self.set_nz8(v);
        } else {
            self.y = self.a;
            self.set_nz16(self.y);
        }
    }

    fn txa(&mut self) {
        if self.p.acc8() {
            let v = self.x8();
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            self.a = self.x;
            self.set_nz16(self.a);
        }
    }

    fn tya(&mut self) {
        if self.p.acc8() {
            let v = self.y8();
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            self.a = self.y;
            self.set_nz16(self.a);
        }
    }

    fn txy(&mut self) {
        if self.p.idx8() {
            let v = self.x8();
            self.set_y_low(v);
            self.set_nz8(v);
        } else {
            self.y = self.x;
            self.set_nz16(self.y);
        }
    }

    fn tyx(&mut self) {
        if self.p.idx8() {
            let v = self.y8();
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            self.x = self.y;
            self.set_nz16(self.x);
        }
    }

    fn tsx(&mut self) {
        if self.p.idx8() {
            let v = self.sp as u8;
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            self.x = self.sp;
            self.set_nz16(self.x);
        }
    }

    fn txs(&mut self) {
        // Emulation mode pins SP.high to 0x01.
        self.sp = if self.e {
            0x0100 | u16::from(self.x as u8)
        } else {
            self.x
        };
        // TXS does NOT update flags.
    }

    fn tcd(&mut self) {
        // Always 16-bit: DP ← full A (regardless of M flag).
        self.dp = self.a;
        self.set_nz16(self.dp);
    }

    fn tdc(&mut self) {
        // Always 16-bit: A ← DP.
        self.a = self.dp;
        self.set_nz16(self.a);
    }

    fn tcs(&mut self) {
        // Always 16-bit, except emulation pins SP.high to 0x01.
        self.sp = if self.e {
            0x0100 | (self.a & 0x00FF)
        } else {
            self.a
        };
        // TCS does NOT update flags.
    }

    fn tsc(&mut self) {
        // Always 16-bit: A ← SP.
        self.a = self.sp;
        self.set_nz16(self.a);
    }

    fn xba(&mut self) {
        // Swap the two bytes of the full 16-bit A. Flags reflect the
        // NEW low byte (the previous high byte). M flag does not gate
        // the swap itself, but the flags use the 8-bit-result formula.
        self.a = self.a.rotate_left(8);
        self.set_nz8(self.a as u8);
    }

    // ===================================================================
    // Stack primitives
    //
    // Push: write at $00:SP, then SP--.
    // Pull: SP++, then read at $00:SP.
    // In emulation mode, SP wraps within $0100-$01FF (high byte pinned
    // to 0x01).
    // 16-bit values are pushed high-byte first so they read back
    // little-endian.
    // ===================================================================

    fn push_u8<B: Bus>(&mut self, bus: &mut B, value: u8) {
        bus.write(make_addr(0, self.sp), value);
        self.sp = if self.e {
            0x0100 | (self.sp.wrapping_sub(1) & 0x00FF)
        } else {
            self.sp.wrapping_sub(1)
        };
    }

    fn pull_u8<B: Bus>(&mut self, bus: &mut B) -> u8 {
        self.sp = if self.e {
            0x0100 | (self.sp.wrapping_add(1) & 0x00FF)
        } else {
            self.sp.wrapping_add(1)
        };
        bus.read(make_addr(0, self.sp))
    }

    fn push_u16<B: Bus>(&mut self, bus: &mut B, value: u16) {
        self.push_u8(bus, (value >> 8) as u8);
        self.push_u8(bus, value as u8);
    }

    fn pull_u16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.pull_u8(bus);
        let hi = self.pull_u8(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    // ===================================================================
    // PH* / PL* — push and pull registers
    // ===================================================================

    fn pha<B: Bus>(&mut self, bus: &mut B) {
        if self.p.acc8() {
            self.push_u8(bus, self.a8());
        } else {
            self.push_u16(bus, self.a);
        }
    }

    fn phx<B: Bus>(&mut self, bus: &mut B) {
        if self.p.idx8() {
            self.push_u8(bus, self.x8());
        } else {
            self.push_u16(bus, self.x);
        }
    }

    fn phy<B: Bus>(&mut self, bus: &mut B) {
        if self.p.idx8() {
            self.push_u8(bus, self.y8());
        } else {
            self.push_u16(bus, self.y);
        }
    }

    fn php<B: Bus>(&mut self, bus: &mut B) {
        self.push_u8(bus, self.p.bits());
    }

    fn phb<B: Bus>(&mut self, bus: &mut B) {
        self.push_u8(bus, self.db);
    }

    fn phd<B: Bus>(&mut self, bus: &mut B) {
        self.push_u16(bus, self.dp);
    }

    fn phk<B: Bus>(&mut self, bus: &mut B) {
        self.push_u8(bus, self.pb);
    }

    fn pla<B: Bus>(&mut self, bus: &mut B) {
        if self.p.acc8() {
            let v = self.pull_u8(bus);
            self.set_a_low(v);
            self.set_nz8(v);
        } else {
            let v = self.pull_u16(bus);
            self.a = v;
            self.set_nz16(v);
        }
    }

    fn plx<B: Bus>(&mut self, bus: &mut B) {
        if self.p.idx8() {
            let v = self.pull_u8(bus);
            self.set_x_low(v);
            self.set_nz8(v);
        } else {
            let v = self.pull_u16(bus);
            self.x = v;
            self.set_nz16(v);
        }
    }

    fn ply<B: Bus>(&mut self, bus: &mut B) {
        if self.p.idx8() {
            let v = self.pull_u8(bus);
            self.set_y_low(v);
            self.set_nz8(v);
        } else {
            let v = self.pull_u16(bus);
            self.y = v;
            self.set_nz16(v);
        }
    }

    fn plp<B: Bus>(&mut self, bus: &mut B) {
        let new_p = self.pull_u8(bus);
        // Emulation mode forces M and X to 1 in P (they cannot be
        // cleared while E=1).
        let effective = if self.e {
            new_p | bit::M | bit::X
        } else {
            new_p
        };
        self.p = crate::flags::StatusFlags(effective);
        if self.p.idx8() {
            self.x &= 0x00FF;
            self.y &= 0x00FF;
        }
    }

    fn plb<B: Bus>(&mut self, bus: &mut B) {
        let v = self.pull_u8(bus);
        self.db = v;
        self.set_nz8(v);
    }

    fn pld<B: Bus>(&mut self, bus: &mut B) {
        let v = self.pull_u16(bus);
        self.dp = v;
        self.set_nz16(v);
    }

    // ===================================================================
    // PEA / PEI / PER — push effective address
    //
    // PEA #imm16    — push the next two operand bytes as a 16-bit value
    // PEI (dp)      — read 16-bit pointer at DP+dp, push it
    // PER rel16     — push PC + signed 16-bit displacement (after fetch)
    // ===================================================================

    fn pea<B: Bus>(&mut self, bus: &mut B) {
        let v = self.fetch_u16(bus);
        self.push_u16(bus, v);
    }

    fn pei<B: Bus>(&mut self, bus: &mut B) {
        let dp_off = u16::from(self.fetch_u8(bus));
        let ptr = self.dp.wrapping_add(dp_off);
        let lo = bus.read(make_addr(0, ptr));
        let hi = bus.read(make_addr(0, ptr.wrapping_add(1)));
        let v = u16::from(lo) | (u16::from(hi) << 8);
        self.push_u16(bus, v);
    }

    fn per<B: Bus>(&mut self, bus: &mut B) {
        let rel = self.fetch_u16(bus) as i16;
        let target = self.pc.wrapping_add_signed(rel);
        self.push_u16(bus, target);
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
    fn jmp_abs_indirect_reads_pointer_in_bank0() {
        // JMP ($1000) where memory $001000 = $9000 → PC = $9000
        let (mut cpu, mut bus) = run(&[0x6C, 0x00, 0x10]);
        bus.poke_slice(0x00_1000, &[0x00, 0x90]);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(cpu.pb, 0);
    }

    #[test]
    fn jmp_abs_indirect_long_changes_bank() {
        // JMP [$1000] where memory $001000 = $7E:9000
        let (mut cpu, mut bus) = run(&[0xDC, 0x00, 0x10]);
        bus.poke_slice(0x00_1000, &[0x00, 0x90, 0x7E]);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(cpu.pb, 0x7E);
    }

    #[test]
    fn jsr_rts_round_trip() {
        // JSR $9000 at $8000 ; at $9000: RTS.
        // Use a short main program so run()'s reset-vector setup is
        // preserved, then poke the subroutine separately.
        let (mut cpu, mut bus) = run(&[0x20, 0x00, 0x90]);
        bus.poke(0x00_9000, 0x60); // RTS at $9000
        cpu.step(&mut bus); // JSR $9000
        assert_eq!(cpu.pc, 0x9000);
        cpu.step(&mut bus); // RTS
        assert_eq!(cpu.pc, 0x8003, "RTS lands on the byte AFTER JSR's operand");
    }

    #[test]
    fn jsl_rtl_round_trip_across_banks() {
        // JSL $00:9000 ; at $00:9000 → RTL.
        let (mut cpu, mut bus) = run(&[0x22, 0x00, 0x90, 0x00]);
        bus.poke(0x00_9000, 0x6B); // RTL at $9000
        cpu.step(&mut bus); // JSL
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(cpu.pb, 0x00);
        cpu.step(&mut bus); // RTL
        assert_eq!(cpu.pc, 0x8004);
        assert_eq!(cpu.pb, 0x00);
    }

    // -------------------------------------------------------------------
    // BRK / COP / RTI / WDM / MVN / MVP
    // -------------------------------------------------------------------

    #[test]
    fn nmi_in_emulation_jumps_via_fffa_vector_with_b_clear() {
        // After reset (E=1), set up the NMI vector at $00:FFFA → $9000.
        let (mut cpu, mut bus) = run(&[0xEA]); // NOP padding
        bus.poke_slice(0x00_FFFA, &[0x00, 0x90]);
        // Pretend the hardware just raised NMI.
        cpu.trigger_nmi();
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000, "NMI jumped via $FFFA vector");
        assert_eq!(cpu.pb, 0);
        assert!(cpu.p.contains(bit::I), "I set after NMI");
        assert!(!cpu.p.contains(bit::D), "D cleared after NMI");
        // P pushed with B = 0 (this is what tells the handler "I'm an
        // NMI, not a BRK").
        let pushed_p = bus.peek(0x00_01FD);
        assert_eq!(pushed_p & 0x10, 0, "B bit must be clear in NMI-pushed P");
    }

    #[test]
    fn trigger_nmi_is_idempotent_pending_until_step() {
        let (mut cpu, _bus) = run(&[0xEA]);
        cpu.trigger_nmi();
        cpu.trigger_nmi();
        cpu.trigger_nmi();
        assert!(cpu.pending_nmi, "still latched as a single pending edge");
    }

    #[test]
    fn irq_services_via_fffe_vector_when_i_clear() {
        // Place IRQ vector at $00:FFFE/FFFF and trigger an IRQ with
        // I clear. The CPU should jump through the vector.
        let (mut cpu, mut bus) = run(&[0xEA]); // NOP — never executes
        bus.poke_slice(0x00_FFFE, &[0x00, 0x80]);
        cpu.p.remove(bit::I);
        cpu.trigger_irq();
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x8000, "IRQ should redirect through $FFFE");
        assert!(!cpu.pending_irq, "edge consumed");
    }

    #[test]
    fn irq_is_masked_when_i_flag_set() {
        let (mut cpu, mut bus) = run(&[0xEA]); // NOP
        bus.poke_slice(0x00_FFFE, &[0x00, 0x80]);
        cpu.p.insert(bit::I);
        cpu.trigger_irq();
        let pc_before = cpu.pc;
        cpu.step(&mut bus);
        // The NOP ran, the IRQ stayed pending.
        assert_eq!(cpu.pc, pc_before.wrapping_add(1));
        assert!(cpu.pending_irq, "still latched while I masks it");
    }

    #[test]
    fn nmi_wins_over_pending_irq() {
        // Both edges latched; NMI runs first.
        let (mut cpu, mut bus) = run(&[0xEA]);
        bus.poke_slice(0x00_FFFA, &[0x00, 0x90]); // NMI vector
        bus.poke_slice(0x00_FFFE, &[0x00, 0xA0]); // IRQ vector
        cpu.p.remove(bit::I);
        cpu.trigger_nmi();
        cpu.trigger_irq();
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000, "NMI vector wins over IRQ");
        assert!(cpu.pending_irq, "IRQ stays pending for the next step");
    }

    #[test]
    fn irq_wakes_a_waiting_cpu_when_i_clear() {
        let (mut cpu, mut bus) = run(&[0xCB]); // WAI
        bus.poke_slice(0x00_FFFE, &[0x00, 0x80]);
        cpu.step(&mut bus); // WAI
        assert!(cpu.waiting);
        cpu.p.remove(bit::I);
        cpu.trigger_irq();
        cpu.step(&mut bus);
        assert!(!cpu.waiting);
        assert_eq!(cpu.pc, 0x8000);
    }

    #[test]
    fn nmi_wakes_a_waiting_cpu() {
        // CPU executes WAI → waiting = true. Trigger NMI → next step
        // services it and clears waiting.
        let (mut cpu, mut bus) = run(&[0xCB]); // WAI
        bus.poke_slice(0x00_FFFA, &[0x00, 0x90]);
        cpu.step(&mut bus); // WAI sets waiting
        assert!(cpu.waiting);
        let pc_after_wai = cpu.pc;

        // Without NMI: step does nothing.
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, pc_after_wai);

        // With NMI: WAI clears, then NMI services on the same step.
        cpu.trigger_nmi();
        cpu.step(&mut bus);
        assert!(!cpu.waiting);
        assert_eq!(cpu.pc, 0x9000);
    }

    #[test]
    fn brk_in_emulation_jumps_via_fffe_vector() {
        let (mut cpu, mut bus) = run(&[0x00, 0xAA]); // BRK + signature
        // BRK vector at $FFFE/$FFFF → $9000.
        bus.poke_slice(0x00_FFFE, &[0x00, 0x90]);
        cpu.step(&mut bus); // BRK
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(cpu.pb, 0);
        assert!(cpu.p.contains(bit::I));
        assert!(!cpu.p.contains(bit::D));
        // P was pushed with B set (emulation, BRK-style).
        let pushed_p = bus.peek(0x00_01FD);
        assert!(
            pushed_p & 0x10 != 0,
            "B bit must be set in pushed P for BRK"
        );
    }

    #[test]
    fn cop_uses_fff4_vector_in_emulation() {
        let (mut cpu, mut bus) = run(&[0x02, 0xAA]);
        bus.poke_slice(0x00_FFF4, &[0x00, 0x90]);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000);
        // Per Tom Harte / fullsnes, both BRK and COP set B=1 in the
        // pushed P byte in emulation mode. The B=0 distinction only
        // applies to hardware IRQ/NMI (not yet serviced).
        let pushed_p = bus.peek(0x00_01FD);
        assert!(
            pushed_p & 0x10 != 0,
            "B bit must be set for COP in emulation"
        );
    }

    #[test]
    fn rti_pulls_p_and_pc_in_emulation() {
        // Push fake interrupt frame: P=$24, PC=$9000.
        let (mut cpu, mut bus) = run(&[0x40]); // RTI
        bus.poke(0x00_01FD, 0x24); // P (pulled first)
        bus.poke(0x00_01FE, 0x00); // PCL
        bus.poke(0x00_01FF, 0x90); // PCH
        cpu.sp = 0x01FC;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x9000);
        // P was pulled, but M/X are forced on in emulation mode.
        assert_eq!(cpu.p.bits(), 0x24 | bit::M | bit::X);
    }

    #[test]
    fn wdm_consumes_one_operand_byte() {
        let (mut cpu, mut bus) = run(&[0x42, 0xAA]);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x8002, "WDM advances PC by 2 like other 2-byte ops");
    }

    #[test]
    fn mvn_copies_full_block_in_one_step() {
        // MVN dest_bank src_bank — copy 3 bytes from $7E:0000 to $7E:0100.
        // Tom Harte models the entire block move as one CPU step.
        let (mut cpu, mut bus) = run(&[0x54, 0x7E, 0x7E]);
        bus.poke_slice(0x7E_0000, &[0xAA, 0xBB, 0xCC]);
        cpu.x = 0x0000;
        cpu.y = 0x0100;
        cpu.a = 0x0002; // copy 3 bytes total (len - 1)
        cpu.p.remove(bit::X); // 16-bit X/Y

        cpu.step(&mut bus); // single step copies all 3 bytes

        assert_eq!(bus.peek(0x7E_0100), 0xAA);
        assert_eq!(bus.peek(0x7E_0101), 0xBB);
        assert_eq!(bus.peek(0x7E_0102), 0xCC);
        assert_eq!(cpu.x, 0x0003);
        assert_eq!(cpu.y, 0x0103);
        assert_eq!(cpu.a, 0xFFFF, "A wraps from $0000 → $FFFF at end of move");
        assert_eq!(cpu.pc, 0x8003, "PC advances past the 3-byte MVN");
        assert_eq!(cpu.db, 0x7E, "MVN updates DB to the destination bank");
    }

    #[test]
    fn mvp_copies_full_block_backward_in_one_step() {
        // MVP — same as MVN but X/Y decrement. Copy 1 byte (A=$0000).
        let (mut cpu, mut bus) = run(&[0x44, 0x7E, 0x7E]);
        bus.poke(0x7E_0002, 0xCC);
        cpu.x = 0x0002;
        cpu.y = 0x0102;
        cpu.a = 0x0000;
        cpu.p.remove(bit::X);

        cpu.step(&mut bus);

        assert_eq!(bus.peek(0x7E_0102), 0xCC);
        assert_eq!(cpu.x, 0x0001);
        assert_eq!(cpu.y, 0x0101);
        assert_eq!(cpu.a, 0xFFFF);
        assert_eq!(cpu.pc, 0x8003);
    }

    #[test]
    fn brl_branches_with_16bit_displacement() {
        // BRL +$0100 from PC=$8000. After fetching the 3-byte opcode
        // PC=$8003, target = $8103.
        let (mut cpu, mut bus) = run(&[0x82, 0x00, 0x01]);
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, 0x8103);
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

    #[test]
    fn inc_abs_modifies_memory_and_sets_flags() {
        // INC $2000 — memory contains $7F → becomes $80, N=1
        let (mut cpu, mut bus) = run(&[0xEE, 0x00, 0x20]);
        cpu.db = 0;
        bus.poke(0x00_2000, 0x7F);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x00_2000), 0x80);
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn dec_dp_wraps_zero_to_ff() {
        // DEC $10 (DP=$0100, memory $0110 = $00)
        let (mut cpu, mut bus) = run(&[0xC6, 0x10]);
        cpu.dp = 0x0100;
        bus.poke(0x00_0110, 0x00);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x00_0110), 0xFF);
        assert!(cpu.p.contains(bit::N));
        assert!(!cpu.p.contains(bit::Z));
    }

    #[test]
    fn inc_16bit_writes_two_bytes() {
        // CLC, XCE, REP #$20, INC $2000 (memory $00FF → $0100)
        let prog = &[0x18, 0xFB, 0xC2, 0x20, 0xEE, 0x00, 0x20];
        let (mut cpu, mut bus) = run(prog);
        cpu.db = 0;
        bus.poke_slice(0x00_2000, &[0xFF, 0x00]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE
        cpu.step(&mut bus); // REP #$20
        cpu.step(&mut bus); // INC $2000
        assert_eq!(bus.peek(0x00_2000), 0x00);
        assert_eq!(bus.peek(0x00_2001), 0x01);
    }

    #[test]
    fn inx_increments_x_and_sets_zero_on_wrap() {
        // LDX #$FF, INX → X=$00, Z=1
        let (mut cpu, mut bus) = run(&[0xA2, 0xFF, 0xE8]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.x8(), 0x00);
        assert!(cpu.p.contains(bit::Z));
    }

    // -------------------------------------------------------------------
    // AND / ORA / EOR
    // -------------------------------------------------------------------

    #[test]
    fn and_imm_masks_a() {
        // LDA #$F0, AND #$0F → A=0, Z=1
        let (mut cpu, mut bus) = run(&[0xA9, 0xF0, 0x29, 0x0F]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x00);
        assert!(cpu.p.contains(bit::Z));
    }

    #[test]
    fn ora_imm_combines() {
        // LDA #$10, ORA #$01 → A=$11
        let (mut cpu, mut bus) = run(&[0xA9, 0x10, 0x09, 0x01]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x11);
        assert!(!cpu.p.contains(bit::Z));
    }

    #[test]
    fn eor_imm_xors() {
        // LDA #$FF, EOR #$0F → A=$F0
        let (mut cpu, mut bus) = run(&[0xA9, 0xFF, 0x49, 0x0F]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0xF0);
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn and_abs_reads_memory() {
        let (mut cpu, mut bus) = run(&[0xA9, 0xFF, 0x2D, 0x00, 0x20]);
        cpu.db = 0;
        bus.poke(0x00_2000, 0x55);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x55);
    }

    // -------------------------------------------------------------------
    // BIT (special flag semantics)
    // -------------------------------------------------------------------

    #[test]
    fn bit_abs_sets_n_and_v_from_memory() {
        // BIT $2000 where memory = $C0 (N=1, V=1), A=$00 → Z=1
        let (mut cpu, mut bus) = run(&[0x2C, 0x00, 0x20]);
        cpu.db = 0;
        cpu.a = 0;
        bus.poke(0x00_2000, 0xC0);
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::N));
        assert!(cpu.p.contains(bit::V));
        assert!(cpu.p.contains(bit::Z));
    }

    #[test]
    fn bit_imm_only_touches_zero() {
        // Pre-set N and V. BIT #$00 with A=$FF → Z=1, N/V untouched.
        let (mut cpu, mut bus) = run(&[0x89, 0x00]);
        cpu.p.insert(bit::N);
        cpu.p.insert(bit::V);
        cpu.a = 0xFF;
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::Z));
        assert!(cpu.p.contains(bit::N), "BIT #imm must NOT change N");
        assert!(cpu.p.contains(bit::V), "BIT #imm must NOT change V");
    }

    // -------------------------------------------------------------------
    // ASL / LSR / ROL / ROR
    // -------------------------------------------------------------------

    #[test]
    fn asl_a_shifts_left_and_sets_carry_from_msb() {
        // LDA #$81, ASL A → A=$02, C=1, N=0
        let (mut cpu, mut bus) = run(&[0xA9, 0x81, 0x0A]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x02);
        assert!(cpu.p.contains(bit::C));
        assert!(!cpu.p.contains(bit::N));
    }

    #[test]
    fn lsr_a_shifts_right_and_sets_carry_from_lsb() {
        // LDA #$03, LSR A → A=$01, C=1
        let (mut cpu, mut bus) = run(&[0xA9, 0x03, 0x4A]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x01);
        assert!(cpu.p.contains(bit::C));
    }

    #[test]
    fn rol_a_rotates_through_carry() {
        // SEC, LDA #$40, ROL A → A=($40<<1)|1 = $81, C=0, N=1
        let (mut cpu, mut bus) = run(&[0x38, 0xA9, 0x40, 0x2A]);
        cpu.step(&mut bus); // SEC
        cpu.step(&mut bus); // LDA
        cpu.step(&mut bus); // ROL A
        assert_eq!(cpu.a8(), 0x81);
        assert!(!cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn ror_a_rotates_carry_into_msb() {
        // SEC, LDA #$02, ROR A → A=$81 ($02>>1 | $80), C=0, N=1
        let (mut cpu, mut bus) = run(&[0x38, 0xA9, 0x02, 0x6A]);
        cpu.step(&mut bus); // SEC
        cpu.step(&mut bus); // LDA
        cpu.step(&mut bus); // ROR A
        assert_eq!(cpu.a8(), 0x81);
        assert!(!cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn asl_abs_modifies_memory_in_place() {
        let (mut cpu, mut bus) = run(&[0x0E, 0x00, 0x20]);
        cpu.db = 0;
        bus.poke(0x00_2000, 0x40);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x00_2000), 0x80);
        assert!(!cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::N));
    }

    // -------------------------------------------------------------------
    // TSB / TRB
    // -------------------------------------------------------------------

    #[test]
    fn tsb_sets_bits_and_z_reflects_pre_state() {
        // A=$0F, memory $20 = $30 — pre-AND = $00, so Z=0 after TSB?
        // Wait: $0F & $30 = $00 → Z=1. Then memory becomes $0F|$30 = $3F.
        let (mut cpu, mut bus) = run(&[0x0C, 0x00, 0x20]);
        cpu.db = 0;
        cpu.a = 0x0F;
        bus.poke(0x00_2000, 0x30);
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::Z));
        assert_eq!(bus.peek(0x00_2000), 0x3F);
    }

    // -------------------------------------------------------------------
    // Inter-register transfers
    // -------------------------------------------------------------------

    #[test]
    fn tax_copies_a_low_to_x_in_emulation() {
        // LDA #$42, TAX → X.low=$42
        let (mut cpu, mut bus) = run(&[0xA9, 0x42, 0xAA]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.x8(), 0x42);
    }

    #[test]
    fn txa_copies_x_into_a_at_m_width() {
        // LDX #$10, TXA → A.low=$10
        let (mut cpu, mut bus) = run(&[0xA2, 0x10, 0x8A]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x10);
    }

    #[test]
    fn txs_in_emulation_pins_sp_high_to_01() {
        // LDX #$AB, TXS → SP=$01AB.
        let (mut cpu, mut bus) = run(&[0xA2, 0xAB, 0x9A]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.sp, 0x01AB, "emulation forces SP high to 0x01");
    }

    #[test]
    fn txs_in_native_uses_full_x() {
        // CLC, XCE, REP #$10, LDX #$ABCD, TXS → SP=$ABCD
        let prog = &[0x18, 0xFB, 0xC2, 0x10, 0xA2, 0xCD, 0xAB, 0x9A];
        let (mut cpu, mut bus) = run(prog);
        for _ in 0..4 {
            cpu.step(&mut bus);
        } // through LDX
        cpu.step(&mut bus); // TXS
        assert_eq!(cpu.sp, 0xABCD);
    }

    #[test]
    fn tcd_copies_full_a_to_dp_regardless_of_m() {
        // Emulation mode (M=1) but TCD is always 16-bit.
        let (mut cpu, mut bus) = run(&[0x5B]);
        cpu.a = 0xABCD;
        cpu.step(&mut bus);
        assert_eq!(cpu.dp, 0xABCD);
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn tdc_copies_dp_to_full_a() {
        let (mut cpu, mut bus) = run(&[0x7B]);
        cpu.dp = 0x1234;
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0x1234);
        assert!(!cpu.p.contains(bit::N));
    }

    // -------------------------------------------------------------------
    // Stack
    // -------------------------------------------------------------------

    #[test]
    fn pha_pla_round_trip_8bit() {
        // LDA #$42, PHA, LDA #$00, PLA → A=$42
        let (mut cpu, mut bus) = run(&[0xA9, 0x42, 0x48, 0xA9, 0x00, 0x68]);
        cpu.step(&mut bus); // LDA #$42
        let sp_before = cpu.sp;
        cpu.step(&mut bus); // PHA
        assert_eq!(cpu.sp, sp_before - 1);
        cpu.step(&mut bus); // LDA #$00
        cpu.step(&mut bus); // PLA
        assert_eq!(cpu.a8(), 0x42);
        assert_eq!(cpu.sp, sp_before, "PLA must restore SP");
    }

    #[test]
    fn pha_16bit_pushes_two_bytes_in_little_endian_layout() {
        // CLC, XCE, REP #$20, LDA #$ABCD, PHA → stack has $AB at $1FF, $CD at $1FE.
        let prog = &[0x18, 0xFB, 0xC2, 0x20, 0xA9, 0xCD, 0xAB, 0x48];
        let (mut cpu, mut bus) = run(prog);
        for _ in 0..4 {
            cpu.step(&mut bus);
        } // through LDA
        cpu.step(&mut bus); // PHA
        // After PHA SP = original - 2. Original SP after reset = $01FF.
        // PHA writes high byte first ($AB at $1FF), then low byte ($CD at $1FE).
        assert_eq!(bus.peek(0x00_01FF), 0xAB);
        assert_eq!(bus.peek(0x00_01FE), 0xCD);
        assert_eq!(cpu.sp, 0x01FD);
    }

    #[test]
    fn php_plp_round_trip_preserves_flags() {
        // Set C and N via SEC and LDA #$80 in emulation mode, PHP,
        // CLC + clear N via LDA #$00, then PLP → flags restored.
        let (mut cpu, mut bus) = run(&[0x38, 0xA9, 0x80, 0x08, 0x18, 0xA9, 0x00, 0x28]);
        cpu.step(&mut bus); // SEC
        cpu.step(&mut bus); // LDA #$80
        let saved = cpu.p.bits();
        cpu.step(&mut bus); // PHP
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // LDA #$00 (clears N, sets Z)
        cpu.step(&mut bus); // PLP
        assert_eq!(cpu.p.bits(), saved);
    }

    #[test]
    fn plp_in_emulation_keeps_m_and_x_set() {
        // Push $00 (no flags), then PLP. In emulation mode the loaded P
        // must still have M and X = 1.
        let (mut cpu, mut bus) = run(&[0xA9, 0x00, 0x48, 0x28]);
        cpu.step(&mut bus); // LDA #$00 (sets Z, clears N)
        cpu.step(&mut bus); // PHA
        // Push extra zero byte to act as the "P" we'll pull.
        cpu.p.remove(bit::M | bit::X);
        cpu.step(&mut bus); // PLP — pulls the $00 we just pushed
        assert!(cpu.p.contains(bit::M));
        assert!(cpu.p.contains(bit::X));
    }

    #[test]
    fn pea_pushes_16bit_immediate() {
        // PEA #$1234
        let (mut cpu, mut bus) = run(&[0xF4, 0x34, 0x12]);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x00_01FF), 0x12);
        assert_eq!(bus.peek(0x00_01FE), 0x34);
        assert_eq!(cpu.sp, 0x01FD);
    }

    #[test]
    fn per_pushes_pc_plus_signed_rel() {
        // PER $0010 from PC=$8000. After fetching the 3-byte opcode PC=$8003,
        // target = $8003 + $0010 = $8013. Stack receives $80 at $1FF, $13 at $1FE.
        let (mut cpu, mut bus) = run(&[0x62, 0x10, 0x00]);
        cpu.step(&mut bus);
        assert_eq!(bus.peek(0x00_01FF), 0x80);
        assert_eq!(bus.peek(0x00_01FE), 0x13);
    }

    #[test]
    fn xba_swaps_a_bytes_and_sets_flags_from_new_low() {
        // A=$ABCD → XBA → A=$CDAB, N reflects $AB (bit 7 set)
        let (mut cpu, mut bus) = run(&[0xEB]);
        cpu.a = 0xABCD;
        cpu.step(&mut bus);
        assert_eq!(cpu.a, 0xCDAB);
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn trb_clears_bits() {
        // A=$0F, memory = $3F → A&M = $0F (≠0) so Z=0, memory becomes
        // $3F & ~$0F = $30.
        let (mut cpu, mut bus) = run(&[0x1C, 0x00, 0x20]);
        cpu.db = 0;
        cpu.a = 0x0F;
        bus.poke(0x00_2000, 0x3F);
        cpu.step(&mut bus);
        assert!(!cpu.p.contains(bit::Z));
        assert_eq!(bus.peek(0x00_2000), 0x30);
    }

    #[test]
    fn bit_abs_clears_zero_when_overlap() {
        // BIT $2000, memory = $0F, A = $01 → Z=0 (overlap), N=0, V=0
        let (mut cpu, mut bus) = run(&[0x2C, 0x00, 0x20]);
        cpu.db = 0;
        cpu.a = 0x01;
        bus.poke(0x00_2000, 0x0F);
        cpu.step(&mut bus);
        assert!(!cpu.p.contains(bit::Z));
    }

    #[test]
    fn dey_decrements_y() {
        let (mut cpu, mut bus) = run(&[0xA0, 0x05, 0x88]);
        cpu.step(&mut bus); // LDY #$05
        cpu.step(&mut bus); // DEY
        assert_eq!(cpu.y8(), 0x04);
        assert!(!cpu.p.contains(bit::Z));
    }

    // -------------------------------------------------------------------
    // Misc
    // -------------------------------------------------------------------

    // -------------------------------------------------------------------
    // ADC / SBC (binary mode)
    // -------------------------------------------------------------------

    #[test]
    fn adc_imm_basic() {
        // CLC, LDA #$10, ADC #$20 → A=$30, C=0, V=0
        let (mut cpu, mut bus) = run(&[0x18, 0xA9, 0x10, 0x69, 0x20]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // LDA #$10
        cpu.step(&mut bus); // ADC #$20
        assert_eq!(cpu.a8(), 0x30);
        assert!(!cpu.p.contains(bit::C));
        assert!(!cpu.p.contains(bit::V));
    }

    #[test]
    fn adc_imm_carries_out_at_overflow() {
        // CLC, LDA #$FF, ADC #$01 → A=$00, C=1, Z=1
        let (mut cpu, mut bus) = run(&[0x18, 0xA9, 0xFF, 0x69, 0x01]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // LDA #$FF
        cpu.step(&mut bus); // ADC #$01
        assert_eq!(cpu.a8(), 0x00);
        assert!(cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::Z));
    }

    #[test]
    fn adc_imm_uses_carry_in() {
        // SEC, LDA #$10, ADC #$20 → A=$31, C=0
        let (mut cpu, mut bus) = run(&[0x38, 0xA9, 0x10, 0x69, 0x20]);
        cpu.step(&mut bus); // SEC
        cpu.step(&mut bus); // LDA #$10
        cpu.step(&mut bus); // ADC #$20
        assert_eq!(cpu.a8(), 0x31);
        assert!(!cpu.p.contains(bit::C));
    }

    #[test]
    fn adc_imm_signed_overflow_pos_plus_pos_eq_neg() {
        // CLC, LDA #$50, ADC #$50 → A=$A0 (negative), V=1
        let (mut cpu, mut bus) = run(&[0x18, 0xA9, 0x50, 0x69, 0x50]);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // LDA #$50
        cpu.step(&mut bus); // ADC #$50
        assert_eq!(cpu.a8(), 0xA0);
        assert!(cpu.p.contains(bit::V));
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn sbc_imm_basic_with_carry_set() {
        // SEC (no borrow), LDA #$30, SBC #$10 → A=$20, C=1
        let (mut cpu, mut bus) = run(&[0x38, 0xA9, 0x30, 0xE9, 0x10]);
        cpu.step(&mut bus); // SEC
        cpu.step(&mut bus); // LDA #$30
        cpu.step(&mut bus); // SBC #$10
        assert_eq!(cpu.a8(), 0x20);
        assert!(cpu.p.contains(bit::C), "no borrow → C stays set");
    }

    #[test]
    fn sbc_imm_borrows() {
        // SEC, LDA #$10, SBC #$20 → A=$F0, C=0 (borrow occurred)
        let (mut cpu, mut bus) = run(&[0x38, 0xA9, 0x10, 0xE9, 0x20]);
        cpu.step(&mut bus); // SEC
        cpu.step(&mut bus); // LDA #$10
        cpu.step(&mut bus); // SBC #$20
        assert_eq!(cpu.a8(), 0xF0);
        assert!(!cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn adc_abs_reads_from_memory() {
        let (mut cpu, mut bus) = run(&[0x18, 0xA9, 0x10, 0x6D, 0x00, 0x20]); // CLC, LDA #$10, ADC $2000
        cpu.db = 0;
        bus.poke(0x00_2000, 0x25);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // LDA #$10
        cpu.step(&mut bus); // ADC $2000
        assert_eq!(cpu.a8(), 0x35);
    }

    // -------------------------------------------------------------------
    // CMP / CPX / CPY
    // -------------------------------------------------------------------

    #[test]
    fn cmp_imm_equal_sets_zero_and_carry() {
        // LDA #$42, CMP #$42 → Z=1, C=1, N=0
        let (mut cpu, mut bus) = run(&[0xA9, 0x42, 0xC9, 0x42]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.a8(), 0x42, "CMP must not modify A");
        assert!(cpu.p.contains(bit::Z));
        assert!(cpu.p.contains(bit::C));
        assert!(!cpu.p.contains(bit::N));
    }

    #[test]
    fn cmp_imm_greater_clears_zero_keeps_carry() {
        // LDA #$50, CMP #$30 → Z=0, C=1 (no borrow), N=0
        let (mut cpu, mut bus) = run(&[0xA9, 0x50, 0xC9, 0x30]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert!(!cpu.p.contains(bit::Z));
        assert!(cpu.p.contains(bit::C));
        assert!(!cpu.p.contains(bit::N));
    }

    #[test]
    fn cmp_imm_less_clears_carry() {
        // LDA #$20, CMP #$50 → Z=0, C=0 (borrow), N=1 ($D0 sign bit)
        let (mut cpu, mut bus) = run(&[0xA9, 0x20, 0xC9, 0x50]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert!(!cpu.p.contains(bit::Z));
        assert!(!cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::N));
    }

    #[test]
    fn cpx_compares_against_x() {
        // LDX #$10, CPX #$10 → Z=1, C=1
        let (mut cpu, mut bus) = run(&[0xA2, 0x10, 0xE0, 0x10]);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::Z));
        assert!(cpu.p.contains(bit::C));
    }

    #[test]
    fn cpy_compares_against_y_reading_memory() {
        // LDY #$05, CPY $2000 (memory contains $05) → Z=1
        let (mut cpu, mut bus) = run(&[0xA0, 0x05, 0xCC, 0x00, 0x20]);
        cpu.db = 0;
        bus.poke(0x00_2000, 0x05);
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert!(cpu.p.contains(bit::Z));
    }

    #[test]
    fn cmp_16bit_compares_full_word() {
        // CLC, XCE, REP #$20, LDA #$1234, CMP #$1234
        let prog = &[0x18, 0xFB, 0xC2, 0x20, 0xA9, 0x34, 0x12, 0xC9, 0x34, 0x12];
        let (mut cpu, mut bus) = run(prog);
        for _ in 0..4 {
            cpu.step(&mut bus);
        } // through LDA
        cpu.step(&mut bus); // CMP #$1234
        assert!(cpu.p.contains(bit::Z));
        assert!(cpu.p.contains(bit::C));
    }

    // -------------------------------------------------------------------
    // BCD mode (ADC/SBC with D=1)
    // -------------------------------------------------------------------

    #[test]
    fn adc_bcd_simple() {
        // SED, CLC, LDA #$19, ADC #$25 → A=$44 (BCD), C=0
        let (mut cpu, mut bus) = run(&[0xF8, 0x18, 0xA9, 0x19, 0x69, 0x25]);
        cpu.step(&mut bus); // SED
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // LDA #$19
        cpu.step(&mut bus); // ADC #$25
        assert_eq!(cpu.a8(), 0x44);
        assert!(!cpu.p.contains(bit::C));
    }

    #[test]
    fn adc_bcd_carries_at_99_plus_01() {
        // SED, CLC, LDA #$99, ADC #$01 → A=$00, C=1
        let (mut cpu, mut bus) = run(&[0xF8, 0x18, 0xA9, 0x99, 0x69, 0x01]);
        for _ in 0..4 {
            cpu.step(&mut bus);
        }
        assert_eq!(cpu.a8(), 0x00);
        assert!(cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::Z));
    }

    #[test]
    fn adc_bcd_low_nibble_carries() {
        // SED, CLC, LDA #$08, ADC #$05 → low nibble 8+5=13 → +6 = $13
        let (mut cpu, mut bus) = run(&[0xF8, 0x18, 0xA9, 0x08, 0x69, 0x05]);
        for _ in 0..4 {
            cpu.step(&mut bus);
        }
        assert_eq!(cpu.a8(), 0x13);
        assert!(!cpu.p.contains(bit::C));
    }

    #[test]
    fn sbc_bcd_simple() {
        // SED, SEC (no borrow), LDA #$45, SBC #$19 → A=$26
        let (mut cpu, mut bus) = run(&[0xF8, 0x38, 0xA9, 0x45, 0xE9, 0x19]);
        cpu.step(&mut bus); // SED
        cpu.step(&mut bus); // SEC
        cpu.step(&mut bus); // LDA #$45
        cpu.step(&mut bus); // SBC #$19
        assert_eq!(cpu.a8(), 0x26);
        assert!(cpu.p.contains(bit::C), "no borrow → C stays set");
    }

    #[test]
    fn sbc_bcd_borrows() {
        // SED, SEC, LDA #$10, SBC #$25 → A=$85, C=0 (borrow into 100s)
        let (mut cpu, mut bus) = run(&[0xF8, 0x38, 0xA9, 0x10, 0xE9, 0x25]);
        for _ in 0..4 {
            cpu.step(&mut bus);
        }
        assert_eq!(cpu.a8(), 0x85);
        assert!(!cpu.p.contains(bit::C), "borrow → C clear");
    }

    #[test]
    fn adc_bcd_16bit() {
        // CLC, XCE, REP #$20, SED, CLC, LDA #$1234, ADC #$5678 → A=$6912
        let prog = &[
            0x18, 0xFB, 0xC2, 0x20, 0xF8, 0x18, 0xA9, 0x34, 0x12, 0x69, 0x78, 0x56,
        ];
        let (mut cpu, mut bus) = run(prog);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE → native
        cpu.step(&mut bus); // REP #$20 → M cleared
        cpu.step(&mut bus); // SED
        cpu.step(&mut bus); // CLC (XCE may have changed C)
        cpu.step(&mut bus); // LDA #$1234
        cpu.step(&mut bus); // ADC #$5678
        assert_eq!(cpu.a, 0x6912);
    }

    #[test]
    fn adc_16bit_wraps_at_10000() {
        // CLC, XCE, REP #$20, LDA #$FFFF, ADC #$0001
        let prog = &[0x18, 0xFB, 0xC2, 0x20, 0xA9, 0xFF, 0xFF, 0x69, 0x01, 0x00];
        let (mut cpu, mut bus) = run(prog);
        cpu.step(&mut bus); // CLC
        cpu.step(&mut bus); // XCE
        cpu.step(&mut bus); // REP #$20
        // Carry is set as a side-effect of XCE — need another CLC.
        // Easier: just verify the wrap behavior in a focused way.
        cpu.p.remove(bit::C);
        cpu.step(&mut bus); // LDA #$FFFF
        cpu.step(&mut bus); // ADC #$0001
        assert_eq!(cpu.a, 0);
        assert!(cpu.p.contains(bit::C));
        assert!(cpu.p.contains(bit::Z));
    }

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
