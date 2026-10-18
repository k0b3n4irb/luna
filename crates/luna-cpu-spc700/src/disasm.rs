//! Standalone SPC700 disassembler.
//!
//! No SNES glue: [`disassemble`] takes a `read(addr) -> u8` closure and a
//! program counter, decodes one instruction, and returns its canonical
//! text + byte length. The opcode → (mnemonic, addressing-mode) table is
//! authored from the ares-cross-checked per-opcode reference in
//! [`crate::cycles`]; instruction length is fully determined by the mode.
//!
//! Operand order matches the execution handlers in [`crate::opcodes`]:
//! `dd,ds` is `[op][src][dst]`, `d,#i` is `[op][imm][dp]`, and a membit
//! operand is a little-endian 16-bit word split `addr = w & 0x1FFF`,
//! `bit = w >> 13` (see `Spc700::fetch_mem_bit`).

/// Addressing mode — selects how many operand bytes follow the opcode and
/// how they are formatted.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Mode {
    /// No operand bytes (the template is the whole instruction).
    Imp,
    /// `#$ii` immediate.
    Imm,
    /// `$dd` direct page.
    Dp,
    /// `$dd+X`.
    DpX,
    /// `$dd+Y`.
    DpY,
    /// `[$dd+X]` indexed-indirect.
    IndIdxX,
    /// `[$dd]+Y` indirect-indexed.
    IdxIndY,
    /// `$aaaa` absolute.
    Abs,
    /// `$aaaa+X`.
    AbsX,
    /// `$aaaa+Y`.
    AbsY,
    /// `[$aaaa+X]` absolute indexed-indirect (JMP).
    AbsIndX,
    /// `$dst, $src` — two direct-page bytes, encoded `[op][src][dst]`.
    DpDp,
    /// `$dp, #$ii` — encoded `[op][imm][dp]`.
    DpImm,
    /// Relative branch; operand is the resolved target `$aaaa`.
    Rel,
    /// `$dp, $target` — direct byte then relative (CBNE/DBNZ d).
    DpRel,
    /// `$dp+X, $target` (CBNE d+X).
    DpXRel,
    /// `$dp, $target` — direct then relative (BBS/BBC; the bit is in the
    /// mnemonic).
    DpBitRel,
    /// `$dp.b` — direct byte, bit number from the opcode (SET1/CLR1).
    DpBit,
    /// `$aaaa.b` membit (13-bit address + 3-bit bit).
    MemBit,
    /// `$FFuu` PCALL target.
    PCall,
    /// `n` TCALL vector, from the opcode.
    TCall,
}

impl Mode {
    const fn operand_bytes(self) -> u8 {
        match self {
            Self::Imp | Self::TCall => 0,
            Self::Imm
            | Self::Dp
            | Self::DpX
            | Self::DpY
            | Self::IndIdxX
            | Self::IdxIndY
            | Self::Rel
            | Self::DpBit
            | Self::PCall => 1,
            Self::Abs
            | Self::AbsX
            | Self::AbsY
            | Self::AbsIndX
            | Self::DpDp
            | Self::DpImm
            | Self::DpRel
            | Self::DpXRel
            | Self::DpBitRel
            | Self::MemBit => 2,
        }
    }
}

/// A decoded instruction: canonical assembly text and its byte length.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Instruction {
    /// Canonical mnemonic + operands, e.g. `"MOV A, #$12"`.
    pub text: String,
    /// Total instruction length in bytes (1..=3).
    pub length: u8,
}

/// Disassemble the single instruction at `pc`, reading bytes through
/// `read`. Never panics; unknown encodings cannot occur (all 256 opcodes
/// are tabulated).
pub fn disassemble(read: impl Fn(u16) -> u8, pc: u16) -> Instruction {
    decode(&read, pc, &|_| None)
}

/// [`disassemble`] as a debugger's listing shows it: a 16-bit absolute
/// operand and a branch's target as the name `name` gives that address,
/// when it gives one.
pub fn disassemble_named(
    read: impl Fn(u16) -> u8,
    pc: u16,
    name: &dyn Fn(u16) -> Option<String>,
) -> Instruction {
    decode(&read, pc, name)
}

fn decode(read: &dyn Fn(u16) -> u8, pc: u16, name: &dyn Fn(u16) -> Option<String>) -> Instruction {
    let op = read(pc);
    let (tmpl, mode) = OPCODES[op as usize];
    let length = 1 + mode.operand_bytes();
    let b1 = read(pc.wrapping_add(1));
    let b2 = read(pc.wrapping_add(2));
    let word = u16::from(b1) | (u16::from(b2) << 8);
    let target = |rel: u8| {
        pc.wrapping_add(u16::from(length))
            .wrapping_add(rel as i8 as u16)
    };

    let addr = |a: u16| name(a).unwrap_or_else(|| format!("${a:04X}"));
    let operand = match mode {
        Mode::Imp => String::new(),
        Mode::Imm => format!("#${b1:02X}"),
        Mode::Dp => format!("${b1:02X}"),
        Mode::DpX => format!("${b1:02X}+X"),
        Mode::DpY => format!("${b1:02X}+Y"),
        Mode::IndIdxX => format!("[${b1:02X}+X]"),
        Mode::IdxIndY => format!("[${b1:02X}]+Y"),
        Mode::Abs => addr(word),
        Mode::AbsX => format!("{}+X", addr(word)),
        Mode::AbsY => format!("{}+Y", addr(word)),
        Mode::AbsIndX => format!("[{}+X]", addr(word)),
        Mode::DpDp => format!("${b2:02X}, ${b1:02X}"),
        Mode::DpImm => format!("${b2:02X}, #${b1:02X}"),
        Mode::Rel => addr(target(b1)),
        Mode::DpRel => format!("${b1:02X}, {}", addr(target(b2))),
        Mode::DpXRel => format!("${b1:02X}+X, {}", addr(target(b2))),
        Mode::DpBitRel => format!("${b1:02X}, {}", addr(target(b2))),
        Mode::DpBit => format!("${b1:02X}.{}", (op >> 5) & 7),
        Mode::MemBit => format!("${:04X}.{}", word & 0x1FFF, word >> 13),
        Mode::PCall => addr(0xFF00 | u16::from(b1)),
        Mode::TCall => format!("{}", op >> 4),
    };

    let text = if tmpl.contains("{}") {
        tmpl.replace("{}", &operand)
    } else {
        tmpl.to_string()
    };
    Instruction { text, length }
}

use Mode::{
    Abs, AbsIndX, AbsX, AbsY, Dp, DpBit, DpBitRel, DpDp, DpImm, DpRel, DpX, DpXRel, DpY, IdxIndY,
    Imm, Imp, IndIdxX, MemBit, PCall, Rel, TCall,
};

/// Opcode → (text template with a single `{}` operand slot, mode), all 256
/// entries. Mnemonics follow the per-opcode reference in [`crate::cycles`].
#[rustfmt::skip]
const OPCODES: [(&str, Mode); 256] = [
    // 0x00
    ("NOP", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS0 {}", DpBitRel),
    ("OR A, {}", Dp), ("OR A, {}", Abs), ("OR A, (X)", Imp), ("OR A, {}", IndIdxX),
    ("OR A, {}", Imm), ("OR {}", DpDp), ("OR1 C, {}", MemBit), ("ASL {}", Dp),
    ("ASL {}", Abs), ("PUSH PSW", Imp), ("TSET1 {}", Abs), ("BRK", Imp),
    // 0x10
    ("BPL {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC0 {}", DpBitRel),
    ("OR A, {}", DpX), ("OR A, {}", AbsX), ("OR A, {}", AbsY), ("OR A, {}", IdxIndY),
    ("OR {}", DpImm), ("OR (X), (Y)", Imp), ("DECW {}", Dp), ("ASL {}", DpX),
    ("ASL A", Imp), ("DEC X", Imp), ("CMP X, {}", Abs), ("JMP {}", AbsIndX),
    // 0x20
    ("CLRP", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS1 {}", DpBitRel),
    ("AND A, {}", Dp), ("AND A, {}", Abs), ("AND A, (X)", Imp), ("AND A, {}", IndIdxX),
    ("AND A, {}", Imm), ("AND {}", DpDp), ("OR1 C, /{}", MemBit), ("ROL {}", Dp),
    ("ROL {}", Abs), ("PUSH A", Imp), ("CBNE {}", DpRel), ("BRA {}", Rel),
    // 0x30
    ("BMI {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC1 {}", DpBitRel),
    ("AND A, {}", DpX), ("AND A, {}", AbsX), ("AND A, {}", AbsY), ("AND A, {}", IdxIndY),
    ("AND {}", DpImm), ("AND (X), (Y)", Imp), ("INCW {}", Dp), ("ROL {}", DpX),
    ("ROL A", Imp), ("INC X", Imp), ("CMP X, {}", Dp), ("CALL {}", Abs),
    // 0x40
    ("SETP", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS2 {}", DpBitRel),
    ("EOR A, {}", Dp), ("EOR A, {}", Abs), ("EOR A, (X)", Imp), ("EOR A, {}", IndIdxX),
    ("EOR A, {}", Imm), ("EOR {}", DpDp), ("AND1 C, {}", MemBit), ("LSR {}", Dp),
    ("LSR {}", Abs), ("PUSH X", Imp), ("TCLR1 {}", Abs), ("PCALL {}", PCall),
    // 0x50
    ("BVC {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC2 {}", DpBitRel),
    ("EOR A, {}", DpX), ("EOR A, {}", AbsX), ("EOR A, {}", AbsY), ("EOR A, {}", IdxIndY),
    ("EOR {}", DpImm), ("EOR (X), (Y)", Imp), ("CMPW YA, {}", Dp), ("LSR {}", DpX),
    ("LSR A", Imp), ("MOV X, A", Imp), ("CMP Y, {}", Abs), ("JMP {}", Abs),
    // 0x60
    ("CLRC", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS3 {}", DpBitRel),
    ("CMP A, {}", Dp), ("CMP A, {}", Abs), ("CMP A, (X)", Imp), ("CMP A, {}", IndIdxX),
    ("CMP A, {}", Imm), ("CMP {}", DpDp), ("AND1 C, /{}", MemBit), ("ROR {}", Dp),
    ("ROR {}", Abs), ("PUSH Y", Imp), ("DBNZ {}", DpRel), ("RET", Imp),
    // 0x70
    ("BVS {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC3 {}", DpBitRel),
    ("CMP A, {}", DpX), ("CMP A, {}", AbsX), ("CMP A, {}", AbsY), ("CMP A, {}", IdxIndY),
    ("CMP {}", DpImm), ("CMP (X), (Y)", Imp), ("ADDW YA, {}", Dp), ("ROR {}", DpX),
    ("ROR A", Imp), ("MOV A, X", Imp), ("CMP Y, {}", Dp), ("RETI", Imp),
    // 0x80
    ("SETC", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS4 {}", DpBitRel),
    ("ADC A, {}", Dp), ("ADC A, {}", Abs), ("ADC A, (X)", Imp), ("ADC A, {}", IndIdxX),
    ("ADC A, {}", Imm), ("ADC {}", DpDp), ("EOR1 C, {}", MemBit), ("DEC {}", Dp),
    ("DEC {}", Abs), ("MOV Y, {}", Imm), ("POP PSW", Imp), ("MOV {}", DpImm),
    // 0x90
    ("BCC {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC4 {}", DpBitRel),
    ("ADC A, {}", DpX), ("ADC A, {}", AbsX), ("ADC A, {}", AbsY), ("ADC A, {}", IdxIndY),
    ("ADC {}", DpImm), ("ADC (X), (Y)", Imp), ("SUBW YA, {}", Dp), ("DEC {}", DpX),
    ("DEC A", Imp), ("MOV X, SP", Imp), ("DIV YA, X", Imp), ("XCN A", Imp),
    // 0xA0
    ("EI", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS5 {}", DpBitRel),
    ("SBC A, {}", Dp), ("SBC A, {}", Abs), ("SBC A, (X)", Imp), ("SBC A, {}", IndIdxX),
    ("SBC A, {}", Imm), ("SBC {}", DpDp), ("MOV1 C, {}", MemBit), ("INC {}", Dp),
    ("INC {}", Abs), ("CMP Y, {}", Imm), ("POP A", Imp), ("MOV (X)+, A", Imp),
    // 0xB0
    ("BCS {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC5 {}", DpBitRel),
    ("SBC A, {}", DpX), ("SBC A, {}", AbsX), ("SBC A, {}", AbsY), ("SBC A, {}", IdxIndY),
    ("SBC {}", DpImm), ("SBC (X), (Y)", Imp), ("MOVW YA, {}", Dp), ("INC {}", DpX),
    ("INC A", Imp), ("MOV SP, X", Imp), ("DAS A", Imp), ("MOV A, (X)+", Imp),
    // 0xC0
    ("DI", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS6 {}", DpBitRel),
    ("MOV {}, A", Dp), ("MOV {}, A", Abs), ("MOV (X), A", Imp), ("MOV {}, A", IndIdxX),
    ("CMP X, {}", Imm), ("MOV {}, X", Abs), ("MOV1 {}, C", MemBit), ("MOV {}, Y", Dp),
    ("MOV {}, Y", Abs), ("MOV X, {}", Imm), ("POP X", Imp), ("MUL YA", Imp),
    // 0xD0
    ("BNE {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC6 {}", DpBitRel),
    ("MOV {}, A", DpX), ("MOV {}, A", AbsX), ("MOV {}, A", AbsY), ("MOV {}, A", IdxIndY),
    ("MOV {}, X", Dp), ("MOV {}, X", DpY), ("MOVW {}, YA", Dp), ("MOV {}, Y", DpX),
    ("DEC Y", Imp), ("MOV A, Y", Imp), ("CBNE {}", DpXRel), ("DAA A", Imp),
    // 0xE0
    ("CLRV", Imp), ("TCALL {}", TCall), ("SET1 {}", DpBit), ("BBS7 {}", DpBitRel),
    ("MOV A, {}", Dp), ("MOV A, {}", Abs), ("MOV A, (X)", Imp), ("MOV A, {}", IndIdxX),
    ("MOV A, {}", Imm), ("MOV X, {}", Abs), ("NOT1 {}", MemBit), ("MOV Y, {}", Dp),
    ("MOV Y, {}", Abs), ("NOTC", Imp), ("POP Y", Imp), ("SLEEP", Imp),
    // 0xF0
    ("BEQ {}", Rel), ("TCALL {}", TCall), ("CLR1 {}", DpBit), ("BBC7 {}", DpBitRel),
    ("MOV A, {}", DpX), ("MOV A, {}", AbsX), ("MOV A, {}", AbsY), ("MOV A, {}", IdxIndY),
    ("MOV X, {}", Dp), ("MOV X, {}", DpY), ("MOV {}", DpDp), ("MOV Y, {}", DpX),
    ("INC Y", Imp), ("MOV Y, A", Imp), ("DBNZ Y, {}", Rel), ("STOP", Imp),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Disassemble a fixed byte slice starting at `pc`.
    fn disasm(bytes: &[u8], pc: u16) -> Instruction {
        disassemble(
            |a| {
                bytes
                    .get(usize::from(a.wrapping_sub(pc)))
                    .copied()
                    .unwrap_or(0)
            },
            pc,
        )
    }

    #[test]
    fn one_per_addressing_mode() {
        let cases: &[(&[u8], u16, &str, u8)] = &[
            (&[0x00], 0, "NOP", 1),
            (&[0xE8, 0x12], 0, "MOV A, #$12", 2),
            (&[0xE4, 0x10], 0, "MOV A, $10", 2),
            (&[0xF5, 0x34, 0x12], 0, "MOV A, $1234+X", 3),
            (&[0x5F, 0x34, 0x12], 0, "JMP $1234", 3),
            (&[0x1F, 0x34, 0x12], 0, "JMP [$1234+X]", 3),
            (&[0xE6], 0, "MOV A, (X)", 1),
            (&[0xBF], 0, "MOV A, (X)+", 1),
            (&[0x07, 0x10], 0, "OR A, [$10+X]", 2),
            (&[0x17, 0x10], 0, "OR A, [$10]+Y", 2),
            (&[0x09, 0x10, 0x20], 0, "OR $20, $10", 3), // dst=$20, src=$10
            (&[0x18, 0x05, 0x10], 0, "OR $10, #$05", 3), // dp=$10, imm=$05
            (&[0x0A, 0x00, 0x28], 0, "OR1 C, $0800.1", 3), // word 0x2800
            (&[0x2A, 0x00, 0x28], 0, "OR1 C, /$0800.1", 3),
            (&[0x02, 0x10], 0, "SET1 $10.0", 2),
            (&[0x22, 0x10], 0, "SET1 $10.1", 2),
            (&[0x12, 0x10], 0, "CLR1 $10.0", 2),
            (&[0x01], 0, "TCALL 0", 1),
            (&[0xF1], 0, "TCALL 15", 1),
            (&[0x4F, 0x30], 0, "PCALL $FF30", 2),
            (&[0xCF], 0, "MUL YA", 1),
            (&[0x9E], 0, "DIV YA, X", 1),
            (&[0xC4, 0x10], 0, "MOV $10, A", 2),
        ];
        for (bytes, pc, want, len) in cases {
            let got = disasm(bytes, *pc);
            assert_eq!(got.text, *want, "opcode {:02X}", bytes[0]);
            assert_eq!(got.length, *len, "len for {want}");
        }
    }

    #[test]
    fn branch_targets_are_resolved() {
        // BNE $rel at PC=0x0100, rel = -2 → target = 0x0100+2-2 = 0x0100.
        let i = disasm(&[0xD0, 0xFE], 0x0100);
        assert_eq!(i.text, "BNE $0100");
        assert_eq!(i.length, 2);
        // Forward: BRA at 0x0200, rel = +0x10 → 0x0200+2+0x10 = 0x0212.
        let i = disasm(&[0x2F, 0x10], 0x0200);
        assert_eq!(i.text, "BRA $0212");
        // BBS0 $10, rel at 0x0100, rel=-3 → 0x0100+3-3 = 0x0100.
        let i = disasm(&[0x03, 0x10, 0xFD], 0x0100);
        assert_eq!(i.text, "BBS0 $10, $0100");
        assert_eq!(i.length, 3);
        // CBNE d+X (0xDE).
        let i = disasm(&[0xDE, 0x10, 0x00], 0x0100);
        assert_eq!(i.text, "CBNE $10+X, $0103");
        assert_eq!(i.length, 3);
        // DBNZ Y, rel (0xFE) — 2 bytes.
        let i = disasm(&[0xFE, 0x00], 0x0100);
        assert_eq!(i.text, "DBNZ Y, $0102");
        assert_eq!(i.length, 2);
    }

    #[test]
    fn every_opcode_decodes_without_panic_and_has_sane_length() {
        for op in 0u16..=255 {
            let bytes = [op as u8, 0x55, 0xAA];
            let i = disassemble(|a| bytes[usize::from(a) % 3], a_pc(op));
            assert!((1..=3).contains(&i.length));
            assert!(!i.text.is_empty());
        }
    }
    const fn a_pc(_op: u16) -> u16 {
        0x1000
    }
}

/// The registers an SPC700 effective address depends on.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectiveRegs {
    /// Index X.
    pub x: u8,
    /// Index Y.
    pub y: u8,
    /// The `P` flag: direct page at `$0100` instead of `$0000`.
    pub p: bool,
}

/// Where an SPC700 instruction reads or writes memory if it ran now.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    /// The 16-bit address.
    pub addr: u16,
    /// Bytes it reads or writes there: 2 for the word instructions.
    pub width: u8,
}

/// The memory address the instruction at `pc` accesses, from the
/// registers `r` as they are now. `peek` reads ARAM without side effects.
/// `None` for an instruction that accesses no memory operand, a jump or
/// a call included; for a two-address one, the destination.
pub fn effective_address(peek: impl Fn(u16) -> u8, pc: u16, r: EffectiveRegs) -> Option<Effective> {
    let op = peek(pc);
    let (tmpl, mode) = OPCODES[op as usize];
    let b1 = peek(pc.wrapping_add(1));
    let b2 = peek(pc.wrapping_add(2));
    let word = u16::from(b1) | (u16::from(b2) << 8);
    let page = if r.p { 0x0100 } else { 0 };
    let dp = |d: u8| page | u16::from(d);
    let ptr = |a: u16| u16::from(peek(a)) | (u16::from(peek(dp((a as u8).wrapping_add(1)))) << 8);
    let mnemonic = tmpl.split(' ').next().unwrap_or("");
    if matches!(mnemonic, "JMP" | "CALL" | "PCALL" | "TCALL" | "BRK") {
        return None;
    }
    let width = if mnemonic.ends_with('W') { 2 } else { 1 };
    let addr = match mode {
        Mode::Dp | Mode::DpRel | Mode::DpBit | Mode::DpBitRel => dp(b1),
        Mode::DpX | Mode::DpXRel => dp(b1.wrapping_add(r.x)),
        Mode::DpY => dp(b1.wrapping_add(r.y)),
        Mode::IndIdxX => ptr(dp(b1.wrapping_add(r.x))),
        Mode::IdxIndY => ptr(dp(b1)).wrapping_add(u16::from(r.y)),
        Mode::Abs => word,
        Mode::AbsX => word.wrapping_add(u16::from(r.x)),
        Mode::AbsY => word.wrapping_add(u16::from(r.y)),
        Mode::DpDp => dp(b2),
        Mode::DpImm => dp(b2),
        Mode::MemBit => word & 0x1FFF,
        // `(X)`, `(X)+`: the direct-page byte X points at.
        Mode::Imp if tmpl.contains("(X)") => dp(r.x),
        _ => return None,
    };
    Some(Effective { addr, width })
}

#[cfg(test)]
mod effective_tests {
    use super::*;

    fn ea(code: &[u8], mem: &[(u16, u8)], r: EffectiveRegs) -> Option<(u16, u8)> {
        let peek = |a: u16| {
            if a >= 0x0400 && usize::from(a - 0x0400) < code.len() {
                return code[usize::from(a - 0x0400)];
            }
            mem.iter().find(|&&(m, _)| m == a).map_or(0, |&(_, v)| v)
        };
        effective_address(peek, 0x0400, r).map(|e| (e.addr, e.width))
    }

    #[test]
    fn memory_operands_and_nothing_for_jumps() {
        let r = EffectiveRegs {
            x: 2,
            y: 3,
            p: false,
        };
        assert_eq!(ea(&[0xE4, 0xF4], &[], r), Some((0x00F4, 1)), "MOV A, $F4");
        assert_eq!(ea(&[0xF4, 0x10], &[], r), Some((0x0012, 1)), "MOV A, $10+X");
        assert_eq!(
            ea(&[0xE5, 0x00, 0x02], &[], r),
            Some((0x0200, 1)),
            "MOV A, $0200"
        );
        assert_eq!(
            ea(&[0xF7, 0x20], &[(0x20, 0x00), (0x21, 0x03)], r),
            Some((0x0303, 1)),
            "MOV A, [$20]+Y"
        );
        assert_eq!(ea(&[0xBA, 0x30], &[], r), Some((0x0030, 2)), "MOVW YA, $30");
        assert_eq!(ea(&[0xE6], &[], r), Some((0x0002, 1)), "MOV A, (X)");
        assert_eq!(
            ea(&[0x8F, 0xAA, 0xF4], &[], r),
            Some((0x00F4, 1)),
            "MOV $F4, #$AA"
        );
        let rp = EffectiveRegs { p: true, ..r };
        assert_eq!(ea(&[0xE4, 0x10], &[], rp), Some((0x0110, 1)), "P = 1");
        for none in [
            &[0x3F, 0x00, 0x08][..],
            &[0x5F, 0x00, 0x08],
            &[0x2F, 0x02],
            &[0xE8, 0x01],
        ] {
            assert_eq!(ea(none, &[], r), None, "{none:02X?}");
        }
    }

    #[test]
    fn the_named_form_names_absolute_operands_and_targets() {
        let code = [0x3F, 0x00, 0x08, 0xD0, 0xFB];
        let read = |a: u16| {
            code.get(usize::from(a.wrapping_sub(0x0400)))
                .copied()
                .unwrap_or(0)
        };
        let name = |a: u16| (a == 0x0800).then(|| "Play".to_string());
        assert_eq!(disassemble_named(read, 0x0400, &name).text, "CALL Play");
        assert_eq!(disassemble_named(read, 0x0403, &name).text, "BNE $0400");
        assert_eq!(disassemble(read, 0x0400).text, "CALL $0800", "unchanged");
    }
}
