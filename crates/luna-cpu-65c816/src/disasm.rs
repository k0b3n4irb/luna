//! Standalone 65C816 disassembler.
//!
//! No SNES glue: [`disassemble`] takes a `read(addr) -> u8` closure, a
//! 16-bit program counter (within the current program bank), and the
//! effective `m8` / `x8` accumulator/index widths, and decodes one
//! instruction to canonical text + byte length.
//!
//! The opcode → (mnemonic, addressing-mode) table is authored from the
//! dispatch in [`crate::opcodes`] (each handler name encodes the mnemonic
//! and mode). The 65C816 quirk vs the SPC700 is **flag-dependent immediate
//! widths**: an accumulator-immediate (`LDA #` etc.) is 1 byte when `m8`
//! else 2; an index-immediate (`LDX/LDY/CPX/CPY #`) is 1 byte when `x8`
//! else 2. `m8 = E || M-flag`, `x8 = E || X-flag`.

/// Addressing mode — selects operand length + formatting.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Mode {
    /// No operand (the mnemonic, incl. `A`-form ops, is complete).
    Imp,
    /// `#$ii` / `#$iiii` — width follows `m8` (accumulator ops).
    ImmM,
    /// `#$ii` / `#$iiii` — width follows `x8` (index ops).
    ImmX,
    /// `#$ii` — always one byte (REP/SEP/BRK/COP/WDM).
    Imm8,
    /// `$dd` direct page.
    Dp,
    /// `$dd,X`.
    DpX,
    /// `$dd,Y`.
    DpY,
    /// `($dd)` direct-page indirect.
    DpInd,
    /// `[$dd]` direct-page indirect long.
    DpIndLong,
    /// `($dd),Y`.
    DpIndY,
    /// `[$dd],Y`.
    DpIndLongY,
    /// `($dd,X)`.
    DpXInd,
    /// `$aaaa` absolute.
    Abs,
    /// `$aaaa,X`.
    AbsX,
    /// `$aaaa,Y`.
    AbsY,
    /// `($aaaa)` absolute indirect.
    AbsInd,
    /// `($aaaa,X)` absolute indexed indirect.
    AbsXInd,
    /// `[$aaaa]` absolute indirect long.
    AbsIndLong,
    /// `$aaaaaa` absolute long (24-bit).
    Long,
    /// `$aaaaaa,X`.
    LongX,
    /// 8-bit relative branch; operand is the resolved target.
    Rel,
    /// 16-bit relative (BRL / PER); operand is the resolved target.
    RelLong,
    /// `$dd,S` stack relative.
    Sr,
    /// `($dd,S),Y` stack relative indirect indexed.
    SrIndY,
    /// `$src, $dst` block move (operand bytes are `[dest][src]`).
    BlockMove,
}

impl Mode {
    const fn operand_bytes(self, m8: bool, x8: bool) -> u8 {
        match self {
            Self::Imp => 0,
            Self::ImmM => {
                if m8 {
                    1
                } else {
                    2
                }
            }
            Self::ImmX => {
                if x8 {
                    1
                } else {
                    2
                }
            }
            Self::Imm8
            | Self::Dp
            | Self::DpX
            | Self::DpY
            | Self::DpInd
            | Self::DpIndLong
            | Self::DpIndY
            | Self::DpIndLongY
            | Self::DpXInd
            | Self::Rel
            | Self::Sr
            | Self::SrIndY => 1,
            Self::Abs
            | Self::AbsX
            | Self::AbsY
            | Self::AbsInd
            | Self::AbsXInd
            | Self::AbsIndLong
            | Self::RelLong
            | Self::BlockMove => 2,
            Self::Long | Self::LongX => 3,
        }
    }
}

/// A decoded instruction: canonical assembly text and its byte length.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Instruction {
    /// Canonical mnemonic + operands, e.g. `"LDA #$1234"`.
    pub text: String,
    /// Total instruction length in bytes (1..=4).
    pub length: u8,
}

/// Disassemble the single instruction at `pc` (16-bit, within the program
/// bank), reading bytes through `read`. `m8` / `x8` are the effective
/// 8-bit accumulator / index widths. Never panics.
pub fn disassemble(read: impl Fn(u16) -> u8, pc: u16, m8: bool, x8: bool) -> Instruction {
    let op = read(pc);
    let (mnemonic, mode) = OPCODES[op as usize];
    let length = 1 + mode.operand_bytes(m8, x8);
    let b1 = read(pc.wrapping_add(1));
    let b2 = read(pc.wrapping_add(2));
    let b3 = read(pc.wrapping_add(3));
    let w16 = u16::from(b1) | (u16::from(b2) << 8);
    let w24 = u32::from(b1) | (u32::from(b2) << 8) | (u32::from(b3) << 16);
    let rel8 = pc
        .wrapping_add(u16::from(length))
        .wrapping_add(b1 as i8 as u16);
    let rel16 = pc.wrapping_add(u16::from(length)).wrapping_add(w16);

    let imm = |wide: bool| {
        if wide {
            format!("#${w16:04X}")
        } else {
            format!("#${b1:02X}")
        }
    };

    let operand = match mode {
        Mode::Imp => String::new(),
        Mode::ImmM => imm(!m8),
        Mode::ImmX => imm(!x8),
        Mode::Imm8 => format!("#${b1:02X}"),
        Mode::Dp => format!("${b1:02X}"),
        Mode::DpX => format!("${b1:02X},X"),
        Mode::DpY => format!("${b1:02X},Y"),
        Mode::DpInd => format!("(${b1:02X})"),
        Mode::DpIndLong => format!("[${b1:02X}]"),
        Mode::DpIndY => format!("(${b1:02X}),Y"),
        Mode::DpIndLongY => format!("[${b1:02X}],Y"),
        Mode::DpXInd => format!("(${b1:02X},X)"),
        Mode::Abs => format!("${w16:04X}"),
        Mode::AbsX => format!("${w16:04X},X"),
        Mode::AbsY => format!("${w16:04X},Y"),
        Mode::AbsInd => format!("(${w16:04X})"),
        Mode::AbsXInd => format!("(${w16:04X},X)"),
        Mode::AbsIndLong => format!("[${w16:04X}]"),
        Mode::Long => format!("${w24:06X}"),
        Mode::LongX => format!("${w24:06X},X"),
        Mode::Rel => format!("${rel8:04X}"),
        Mode::RelLong => format!("${rel16:04X}"),
        Mode::Sr => format!("${b1:02X},S"),
        Mode::SrIndY => format!("(${b1:02X},S),Y"),
        // Block-move operand bytes are [dest][src]; assembler syntax shows
        // source, dest.
        Mode::BlockMove => format!("${b2:02X}, ${b1:02X}"),
    };

    let text = if operand.is_empty() {
        mnemonic.to_string()
    } else {
        format!("{mnemonic} {operand}")
    };
    Instruction { text, length }
}

use Mode::{
    Abs, AbsInd, AbsIndLong, AbsX, AbsXInd, AbsY, BlockMove, Dp, DpInd, DpIndLong, DpIndLongY,
    DpIndY, DpX, DpXInd, DpY, Imm8, ImmM, ImmX, Imp, Long, LongX, Rel, RelLong, Sr, SrIndY,
};

/// Opcode → (mnemonic, mode), all 256 entries — authored from the dispatch
/// in [`crate::opcodes`] (`execute`). `A`-form / implied ops carry their
/// full text and use [`Mode::Imp`].
#[rustfmt::skip]
const OPCODES: [(&str, Mode); 256] = [
    // 0x00
    ("BRK", Imm8), ("ORA", DpXInd), ("COP", Imm8), ("ORA", Sr),
    ("TSB", Dp), ("ORA", Dp), ("ASL", Dp), ("ORA", DpIndLong),
    ("PHP", Imp), ("ORA", ImmM), ("ASL A", Imp), ("PHD", Imp),
    ("TSB", Abs), ("ORA", Abs), ("ASL", Abs), ("ORA", Long),
    // 0x10
    ("BPL", Rel), ("ORA", DpIndY), ("ORA", DpInd), ("ORA", SrIndY),
    ("TRB", Dp), ("ORA", DpX), ("ASL", DpX), ("ORA", DpIndLongY),
    ("CLC", Imp), ("ORA", AbsY), ("INC A", Imp), ("TCS", Imp),
    ("TRB", Abs), ("ORA", AbsX), ("ASL", AbsX), ("ORA", LongX),
    // 0x20
    ("JSR", Abs), ("AND", DpXInd), ("JSL", Long), ("AND", Sr),
    ("BIT", Dp), ("AND", Dp), ("ROL", Dp), ("AND", DpIndLong),
    ("PLP", Imp), ("AND", ImmM), ("ROL A", Imp), ("PLD", Imp),
    ("BIT", Abs), ("AND", Abs), ("ROL", Abs), ("AND", Long),
    // 0x30
    ("BMI", Rel), ("AND", DpIndY), ("AND", DpInd), ("AND", SrIndY),
    ("BIT", DpX), ("AND", DpX), ("ROL", DpX), ("AND", DpIndLongY),
    ("SEC", Imp), ("AND", AbsY), ("DEC A", Imp), ("TSC", Imp),
    ("BIT", AbsX), ("AND", AbsX), ("ROL", AbsX), ("AND", LongX),
    // 0x40
    ("RTI", Imp), ("EOR", DpXInd), ("WDM", Imm8), ("EOR", Sr),
    ("MVP", BlockMove), ("EOR", Dp), ("LSR", Dp), ("EOR", DpIndLong),
    ("PHA", Imp), ("EOR", ImmM), ("LSR A", Imp), ("PHK", Imp),
    ("JMP", Abs), ("EOR", Abs), ("LSR", Abs), ("EOR", Long),
    // 0x50
    ("BVC", Rel), ("EOR", DpIndY), ("EOR", DpInd), ("EOR", SrIndY),
    ("MVN", BlockMove), ("EOR", DpX), ("LSR", DpX), ("EOR", DpIndLongY),
    ("CLI", Imp), ("EOR", AbsY), ("PHY", Imp), ("TCD", Imp),
    ("JML", Long), ("EOR", AbsX), ("LSR", AbsX), ("EOR", LongX),
    // 0x60
    ("RTS", Imp), ("ADC", DpXInd), ("PER", RelLong), ("ADC", Sr),
    ("STZ", Dp), ("ADC", Dp), ("ROR", Dp), ("ADC", DpIndLong),
    ("PLA", Imp), ("ADC", ImmM), ("ROR A", Imp), ("RTL", Imp),
    ("JMP", AbsInd), ("ADC", Abs), ("ROR", Abs), ("ADC", Long),
    // 0x70
    ("BVS", Rel), ("ADC", DpIndY), ("ADC", DpInd), ("ADC", SrIndY),
    ("STZ", DpX), ("ADC", DpX), ("ROR", DpX), ("ADC", DpIndLongY),
    ("SEI", Imp), ("ADC", AbsY), ("PLY", Imp), ("TDC", Imp),
    ("JMP", AbsXInd), ("ADC", AbsX), ("ROR", AbsX), ("ADC", LongX),
    // 0x80
    ("BRA", Rel), ("STA", DpXInd), ("BRL", RelLong), ("STA", Sr),
    ("STY", Dp), ("STA", Dp), ("STX", Dp), ("STA", DpIndLong),
    ("DEY", Imp), ("BIT", ImmM), ("TXA", Imp), ("PHB", Imp),
    ("STY", Abs), ("STA", Abs), ("STX", Abs), ("STA", Long),
    // 0x90
    ("BCC", Rel), ("STA", DpIndY), ("STA", DpInd), ("STA", SrIndY),
    ("STY", DpX), ("STA", DpX), ("STX", DpY), ("STA", DpIndLongY),
    ("TYA", Imp), ("STA", AbsY), ("TXS", Imp), ("TXY", Imp),
    ("STZ", Abs), ("STA", AbsX), ("STZ", AbsX), ("STA", LongX),
    // 0xA0
    ("LDY", ImmX), ("LDA", DpXInd), ("LDX", ImmX), ("LDA", Sr),
    ("LDY", Dp), ("LDA", Dp), ("LDX", Dp), ("LDA", DpIndLong),
    ("TAY", Imp), ("LDA", ImmM), ("TAX", Imp), ("PLB", Imp),
    ("LDY", Abs), ("LDA", Abs), ("LDX", Abs), ("LDA", Long),
    // 0xB0
    ("BCS", Rel), ("LDA", DpIndY), ("LDA", DpInd), ("LDA", SrIndY),
    ("LDY", DpX), ("LDA", DpX), ("LDX", DpY), ("LDA", DpIndLongY),
    ("CLV", Imp), ("LDA", AbsY), ("TSX", Imp), ("TYX", Imp),
    ("LDY", AbsX), ("LDA", AbsX), ("LDX", AbsY), ("LDA", LongX),
    // 0xC0
    ("CPY", ImmX), ("CMP", DpXInd), ("REP", Imm8), ("CMP", Sr),
    ("CPY", Dp), ("CMP", Dp), ("DEC", Dp), ("CMP", DpIndLong),
    ("INY", Imp), ("CMP", ImmM), ("DEX", Imp), ("WAI", Imp),
    ("CPY", Abs), ("CMP", Abs), ("DEC", Abs), ("CMP", Long),
    // 0xD0
    ("BNE", Rel), ("CMP", DpIndY), ("CMP", DpInd), ("CMP", SrIndY),
    ("PEI", DpInd), ("CMP", DpX), ("DEC", DpX), ("CMP", DpIndLongY),
    ("CLD", Imp), ("CMP", AbsY), ("PHX", Imp), ("STP", Imp),
    ("JML", AbsIndLong), ("CMP", AbsX), ("DEC", AbsX), ("CMP", LongX),
    // 0xE0
    ("CPX", ImmX), ("SBC", DpXInd), ("SEP", Imm8), ("SBC", Sr),
    ("CPX", Dp), ("SBC", Dp), ("INC", Dp), ("SBC", DpIndLong),
    ("INX", Imp), ("SBC", ImmM), ("NOP", Imp), ("XBA", Imp),
    ("CPX", Abs), ("SBC", Abs), ("INC", Abs), ("SBC", Long),
    // 0xF0
    ("BEQ", Rel), ("SBC", DpIndY), ("SBC", DpInd), ("SBC", SrIndY),
    ("PEA", Abs), ("SBC", DpX), ("INC", DpX), ("SBC", DpIndLongY),
    ("SED", Imp), ("SBC", AbsY), ("PLX", Imp), ("XCE", Imp),
    ("JSR", AbsXInd), ("SBC", AbsX), ("INC", AbsX), ("SBC", LongX),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn disasm(bytes: &[u8], pc: u16, m8: bool, x8: bool) -> Instruction {
        disassemble(
            |a| {
                bytes
                    .get(usize::from(a.wrapping_sub(pc)))
                    .copied()
                    .unwrap_or(0)
            },
            pc,
            m8,
            x8,
        )
    }

    #[test]
    fn immediate_width_follows_flags() {
        // LDA # — 8-bit accumulator.
        let i = disasm(&[0xA9, 0x12], 0, true, true);
        assert_eq!(i.text, "LDA #$12");
        assert_eq!(i.length, 2);
        // LDA # — 16-bit accumulator (m8 = false).
        let i = disasm(&[0xA9, 0x34, 0x12], 0, false, true);
        assert_eq!(i.text, "LDA #$1234");
        assert_eq!(i.length, 3);
        // LDX # — follows the index flag, not M.
        let i = disasm(&[0xA2, 0x34, 0x12], 0, true, false);
        assert_eq!(i.text, "LDX #$1234");
        assert_eq!(i.length, 3);
        let i = disasm(&[0xA2, 0x12], 0, false, true);
        assert_eq!(i.text, "LDX #$12");
        assert_eq!(i.length, 2);
        // REP/SEP are always 1-byte immediates.
        assert_eq!(disasm(&[0xC2, 0x30], 0, false, false).text, "REP #$30");
        assert_eq!(disasm(&[0xE2, 0x30], 0, false, false).length, 2);
    }

    #[test]
    fn one_per_addressing_mode() {
        let f = (true, true);
        let cases: &[(&[u8], &str, u8)] = &[
            (&[0xEA], "NOP", 1),
            (&[0x0A], "ASL A", 1),
            (&[0xA5, 0x10], "LDA $10", 2),
            (&[0xB5, 0x10], "LDA $10,X", 2),
            (&[0xB6, 0x10], "LDX $10,Y", 2),
            (&[0xB2, 0x10], "LDA ($10)", 2),
            (&[0xA7, 0x10], "LDA [$10]", 2),
            (&[0xB1, 0x10], "LDA ($10),Y", 2),
            (&[0xB7, 0x10], "LDA [$10],Y", 2),
            (&[0xA1, 0x10], "LDA ($10,X)", 2),
            (&[0xAD, 0x34, 0x12], "LDA $1234", 3),
            (&[0xBD, 0x34, 0x12], "LDA $1234,X", 3),
            (&[0xB9, 0x34, 0x12], "LDA $1234,Y", 3),
            (&[0x6C, 0x34, 0x12], "JMP ($1234)", 3),
            (&[0x7C, 0x34, 0x12], "JMP ($1234,X)", 3),
            (&[0xDC, 0x34, 0x12], "JML [$1234]", 3),
            (&[0xAF, 0x56, 0x34, 0x12], "LDA $123456", 4),
            (&[0xBF, 0x56, 0x34, 0x12], "LDA $123456,X", 4),
            (&[0x5C, 0x56, 0x34, 0x12], "JML $123456", 4),
            (&[0xA3, 0x10], "LDA $10,S", 2),
            (&[0xB3, 0x10], "LDA ($10,S),Y", 2),
            (&[0xF4, 0x34, 0x12], "PEA $1234", 3),
            (&[0xD4, 0x10], "PEI ($10)", 2),
        ];
        for (bytes, want, len) in cases {
            let i = disasm(bytes, 0, f.0, f.1);
            assert_eq!(i.text, *want, "opcode {:02X}", bytes[0]);
            assert_eq!(i.length, *len, "len for {want}");
        }
    }

    #[test]
    fn block_move_operand_order_and_branches() {
        // MVN dest=$7E src=$00 (bytes [dest][src]) → "MVN $00, $7E".
        let i = disasm(&[0x54, 0x7E, 0x00], 0, true, true);
        assert_eq!(i.text, "MVN $00, $7E");
        assert_eq!(i.length, 3);
        // BNE rel at 0x8000, rel = -2 → target 0x8000.
        let i = disasm(&[0xD0, 0xFE], 0x8000, true, true);
        assert_eq!(i.text, "BNE $8000");
        // BRL (16-bit) at 0x8000, rel = +0x10 → 0x8000+3+0x10 = 0x8013.
        let i = disasm(&[0x82, 0x10, 0x00], 0x8000, true, true);
        assert_eq!(i.text, "BRL $8013");
        assert_eq!(i.length, 3);
    }

    #[test]
    fn every_opcode_decodes() {
        for op in 0u16..=255 {
            let bytes = [op as u8, 0x34, 0x12, 0x56];
            let i = disassemble(|a| bytes[usize::from(a) % 4], 0x1000, false, false);
            assert!((1..=4).contains(&i.length));
            assert!(!i.text.is_empty());
        }
    }
}
