//! SPC700 per-opcode cycle counts (canonical Sony / Anomie / ares
//! reference).
//!
//! Each entry is the total bus-cycle cost of the opcode INCLUDING the
//! initial opcode fetch. Branches list the not-taken cost; the
//! dispatcher should add the per-instruction "taken" penalty
//! separately if it cares (BRA family = +2 when taken, BBC/BBS = +2,
//! CBNE = +2, DBNZ = +2).
//!
//! Cross-checked against ares' `processor/spc700/instructions.cpp`
//! addressing-mode helpers (per the spec in
//! `docs/apu_dsp_reference.md` §2): each `fetch()` / `read()` /
//! `write()` / `idle()` / `push()` / `pull()` / `load()` / `store()`
//! call inside ares costs exactly one bus cycle, and the table here
//! is the sum for each opcode.
//!
//! Replaces the previous "flat 4 cycles per opcode" approximation
//! that desynchronised music tempo (T0/T1/T2 timers) AND playback
//! pitch (DSP sample tick) by the same factor of ~10-50% (gap A1 in
//! `docs/luna_apu_gaps.md`).

/// Per-opcode cycle counts for the SPC700, in opcode order
/// (`SPC700_CYCLES[opcode] = cycles`).
///
/// Values follow the canonical SPC700 cycle reference (see
/// `docs/apu_dsp_reference.md` §2). Branches list the **not-taken**
/// cost; `Spc700::step` adds the +2 taken-branch penalty
/// ([`SPC700_BRANCH_TAKEN_PENALTY`]) when a branch / `CBNE` / `DBNZ`
/// is taken.
#[rustfmt::skip]
pub const SPC700_CYCLES: [u8; 256] = [
    // 0x00-0x0F
    //  00:NOP 01:TCALL0 02:SET1 03:BBS0 04:OR_d 05:OR_!a 06:OR_(X) 07:OR_[d+X]
    //  08:OR_#i 09:OR_dd,ds 0A:OR1 0B:ASL_d 0C:ASL_!a 0D:PUSH_P 0E:TSET1 0F:BRK
        2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 4, 6, 8,
    // 0x10-0x1F
    //  10:BPL 11:TCALL1 12:CLR1 13:BBC0 14:OR_d+X 15:OR_!a+X 16:OR_!a+Y 17:OR_[d]+Y
    //  18:OR_d,#i 19:OR_(X)(Y) 1A:DECW_d 1B:ASL_d+X 1C:ASL_A 1D:DEC_X 1E:CMP_X_!a 1F:JMP_[!a+X]
        2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 6, 5, 2, 2, 4, 6,
    // 0x20-0x2F
    //  20:CLRP 21:TCALL2 22:SET1 23:BBS1 24:AND_d 25:AND_!a 26:AND_(X) 27:AND_[d+X]
    //  28:AND_#i 29:AND_dd,ds 2A:OR1_/m 2B:ROL_d 2C:ROL_!a 2D:PUSH_A 2E:CBNE_d 2F:BRA
        2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 4, 5, 2,
    // 0x30-0x3F
    //  30:BMI 31:TCALL3 32:CLR1 33:BBC1 34:AND_d+X 35:AND_!a+X 36:AND_!a+Y 37:AND_[d]+Y
    //  38:AND_d,#i 39:AND_(X)(Y) 3A:INCW_d 3B:ROL_d+X 3C:ROL_A 3D:INC_X 3E:CMP_X_d 3F:CALL_!a
        2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 6, 5, 2, 2, 3, 8,
    // 0x40-0x4F
    //  40:SETP 41:TCALL4 42:SET1 43:BBS2 44:EOR_d 45:EOR_!a 46:EOR_(X) 47:EOR_[d+X]
    //  48:EOR_#i 49:EOR_dd,ds 4A:AND1 4B:LSR_d 4C:LSR_!a 4D:PUSH_X 4E:TCLR1 4F:PCALL_#u
        2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 4, 6, 6,
    // 0x50-0x5F
    //  50:BVC 51:TCALL5 52:CLR1 53:BBC2 54:EOR_d+X 55:EOR_!a+X 56:EOR_!a+Y 57:EOR_[d]+Y
    //  58:EOR_d,#i 59:EOR_(X)(Y) 5A:CMPW_d 5B:LSR_d+X 5C:LSR_A 5D:MOV_X,A 5E:CMP_Y_!a 5F:JMP_!a
        2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 4, 5, 2, 2, 4, 3,
    // 0x60-0x6F
    //  60:CLRC 61:TCALL6 62:SET1 63:BBS3 64:CMP_d 65:CMP_!a 66:CMP_(X) 67:CMP_[d+X]
    //  68:CMP_#i 69:CMP_dd,ds 6A:AND1_/m 6B:ROR_d 6C:ROR_!a 6D:PUSH_Y 6E:DBNZ_d 6F:RET
        2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 4, 5, 5,
    // 0x70-0x7F
    //  70:BVS 71:TCALL7 72:CLR1 73:BBC3 74:CMP_d+X 75:CMP_!a+X 76:CMP_!a+Y 77:CMP_[d]+Y
    //  78:CMP_d,#i 79:CMP_(X)(Y) 7A:ADDW_d 7B:ROR_d+X 7C:ROR_A 7D:MOV_A,X 7E:CMP_Y_d 7F:RETI
        2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 3, 6,
    // 0x80-0x8F
    //  80:SETC 81:TCALL8 82:SET1 83:BBS4 84:ADC_d 85:ADC_!a 86:ADC_(X) 87:ADC_[d+X]
    //  88:ADC_#i 89:ADC_dd,ds 8A:EOR1 8B:DEC_d 8C:DEC_!a 8D:MOV_Y,#i 8E:POP_P 8F:MOV_d,#i
        2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 2, 4, 5,
    // 0x90-0x9F
    //  90:BCC 91:TCALL9 92:CLR1 93:BBC4 94:ADC_d+X 95:ADC_!a+X 96:ADC_!a+Y 97:ADC_[d]+Y
    //  98:ADC_d,#i 99:ADC_(X)(Y) 9A:SUBW_d 9B:DEC_d+X 9C:DEC_A 9D:MOV_X,SP 9E:DIV 9F:XCN
        2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2,12, 5,
    // 0xA0-0xAF
    //  A0:EI A1:TCALL10 A2:SET1 A3:BBS5 A4:SBC_d A5:SBC_!a A6:SBC_(X) A7:SBC_[d+X]
    //  A8:SBC_#i A9:SBC_dd,ds AA:LDC_C,m AB:INC_d AC:INC_!a AD:CMP_Y,#i AE:POP_A AF:MOV_(X)+,A
        3, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 2, 4, 4,
    // 0xB0-0xBF
    //  B0:BCS B1:TCALL11 B2:CLR1 B3:BBC5 B4:SBC_d+X B5:SBC_!a+X B6:SBC_!a+Y B7:SBC_[d]+Y
    //  B8:SBC_d,#i B9:SBC_(X)(Y) BA:MOVW_YA,d BB:INC_d+X BC:INC_A BD:MOV_SP,X BE:DAS BF:MOV_A,(X)+
        2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 3, 4,
    // 0xC0-0xCF
    //  C0:DI C1:TCALL12 C2:SET1 C3:BBS6 C4:MOV_d,A C5:MOV_!a,A C6:MOV_(X),A C7:MOV_[d+X],A
    //  C8:CMP_X,#i C9:MOV_!a,X CA:STC_m,C CB:MOV_d,Y CC:MOV_!a,Y CD:MOV_X,#i CE:POP_X CF:MUL
        3, 8, 4, 5, 4, 5, 4, 7, 2, 5, 6, 4, 5, 2, 4, 9,
    // 0xD0-0xDF
    //  D0:BNE D1:TCALL13 D2:CLR1 D3:BBC6 D4:MOV_d+X,A D5:MOV_!a+X,A D6:MOV_!a+Y,A D7:MOV_[d]+Y,A
    //  D8:MOV_d,X D9:MOV_d+Y,X DA:MOVW_d,YA DB:MOV_d+X,Y DC:DEC_Y DD:MOV_A,Y DE:CBNE_d+X DF:DAA
        2, 8, 4, 5, 5, 6, 6, 7, 4, 5, 5, 5, 2, 2, 6, 3,
    // 0xE0-0xEF
    //  E0:CLRV E1:TCALL14 E2:SET1 E3:BBS7 E4:MOV_A,d E5:MOV_A,!a E6:MOV_A,(X) E7:MOV_A,[d+X]
    //  E8:MOV_A,#i E9:MOV_X,!a EA:NOT1 EB:MOV_Y,d EC:MOV_Y,!a ED:NOTC EE:POP_Y EF:SLEEP
        2, 8, 4, 5, 3, 4, 3, 6, 2, 4, 5, 3, 4, 3, 4, 2,
    // 0xF0-0xFF
    //  F0:BEQ F1:TCALL15 F2:CLR1 F3:BBC7 F4:MOV_A,d+X F5:MOV_A,!a+X F6:MOV_A,!a+Y F7:MOV_A,[d]+Y
    //  F8:MOV_X,d F9:MOV_X,d+Y FA:MOV_dd,ds FB:MOV_Y,d+X FC:INC_Y FD:MOV_Y,A FE:DBNZ_Y FF:STOP
        2, 8, 4, 5, 4, 5, 5, 6, 3, 4, 5, 4, 2, 2, 4, 2,
];

/// Extra SPC cycles a branch-family opcode costs when the branch is
/// **taken** — two pipeline idles on real hardware (ares
/// `instructionBranch`: two `idle()` calls past the condition). Added
/// by [`crate::Spc700::step`] on top of the not-taken base in
/// [`SPC700_CYCLES`] for BRA / Bcc / CBNE / DBNZ / BBS / BBC.
pub const SPC700_BRANCH_TAKEN_PENALTY: u8 = 2;

/// Quick sanity check: every entry must be in [2, 12]. Used by the
/// build-time test below.
#[cfg(test)]
const _: () = {
    let mut i = 0;
    while i < 256 {
        let c = SPC700_CYCLES[i];
        assert!(c >= 2 && c <= 12, "cycle table entry out of range");
        i += 1;
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_opcodes_have_canonical_cycle_counts() {
        // Spot-check against the agent's ares research notes.
        assert_eq!(SPC700_CYCLES[0x00], 2, "NOP");
        assert_eq!(SPC700_CYCLES[0x01], 8, "TCALL 0");
        assert_eq!(SPC700_CYCLES[0x08], 2, "OR A,#i (immediate)");
        assert_eq!(SPC700_CYCLES[0x05], 4, "OR A,!a (absolute)");
        assert_eq!(SPC700_CYCLES[0x04], 3, "OR A,d (direct page)");
        assert_eq!(SPC700_CYCLES[0x3F], 8, "CALL !a");
        assert_eq!(SPC700_CYCLES[0x6F], 5, "RET");
        assert_eq!(SPC700_CYCLES[0x7F], 6, "RETI");
        assert_eq!(SPC700_CYCLES[0x0F], 8, "BRK");
        assert_eq!(SPC700_CYCLES[0x10], 2, "BPL not-taken");
        assert_eq!(
            SPC700_CYCLES[0x2F], 2,
            "BRA not-taken base; step() adds +2 taken = 4"
        );
        assert_eq!(SPC700_CYCLES[0xCF], 9, "MUL YA");
        assert_eq!(SPC700_CYCLES[0x9E], 12, "DIV YA,X");
        assert_eq!(SPC700_CYCLES[0xDF], 3, "DAA");
        assert_eq!(SPC700_CYCLES[0xBE], 3, "DAS");
        assert_eq!(SPC700_CYCLES[0x9F], 5, "XCN");
        assert_eq!(SPC700_CYCLES[0xED], 3, "NOTC");
        // SLEEP/STOP halt the core; luna charges the fetch+execute (2) and
        // then returns 2 per step while halted. The Tom Harte trace length
        // (7) is a fixed halt-window artifact, not a completing instruction
        // cost, so the cycle backstop excludes these two opcodes.
        assert_eq!(SPC700_CYCLES[0xEF], 2, "SLEEP");
        assert_eq!(SPC700_CYCLES[0xFF], 2, "STOP");
        assert_eq!(SPC700_CYCLES[0x8F], 5, "MOV d,#i (direct-immediate write)");
        assert_eq!(SPC700_CYCLES[0xFA], 5, "MOV dd,ds");

        // The carry/bit group + MOV A,(X)+ — corrected after the Tom Harte
        // cycle backstop caught luna charging each one cycle too many
        // (tests/tom_harte.rs validates step() == bus-cycle trace length).
        assert_eq!(SPC700_CYCLES[0x0A], 5, "OR1 C,m.b");
        assert_eq!(SPC700_CYCLES[0x2A], 5, "OR1 C,/m.b");
        assert_eq!(SPC700_CYCLES[0x4A], 4, "AND1 C,m.b");
        assert_eq!(SPC700_CYCLES[0x6A], 4, "AND1 C,/m.b");
        assert_eq!(SPC700_CYCLES[0x8A], 5, "EOR1 C,m.b");
        assert_eq!(SPC700_CYCLES[0xAA], 4, "MOV1 C,m.b");
        assert_eq!(SPC700_CYCLES[0xCA], 6, "MOV1 m.b,C");
        assert_eq!(SPC700_CYCLES[0xEA], 5, "NOT1 m.b");
        assert_eq!(SPC700_CYCLES[0xBF], 4, "MOV A,(X)+");
    }

    #[test]
    fn cycle_table_average_is_in_realistic_range() {
        // Sanity: the avg cycle count across all 256 opcodes should
        // land between 3.5 and 5.5 — anything outside indicates I
        // mistyped a row.
        let sum: u32 = SPC700_CYCLES.iter().map(|&c| c as u32).sum();
        let avg = sum as f64 / 256.0;
        assert!(
            (3.5..=5.5).contains(&avg),
            "avg cycles per opcode out of plausible range: {avg}"
        );
    }
}
