//! Debugger expressions in Mesen2's language: breakpoint conditions,
//! watches, trace filters. The same text gives the same value in both
//! emulators.
//!
//! The language, briefly:
//! - **Values.** Decimal, `$hex` / `0xhex`, `%binary` literals; registers
//!   and flags of the CPU the expression is for; `value`, `address`,
//!   `isread`… of the access being checked; labels, which stand for their
//!   address.
//! - **Operators.** C's operators with C's precedence, plus `[a]` / `{a}`
//!   / `#a`, which read a byte, word or long at `a`, and `:a`, which gives
//!   the offset of the memory behind `a`.
//! - **Arithmetic and kinds.** Arithmetic is signed 64-bit, and the result
//!   is clamped to −2³¹ … 2³²−1. Comparisons, `&&` and `||` give a Boolean.
//! - **Quirks kept from Mesen2.** Spaces are removed first. `&&` and `||`
//!   evaluate both sides. A binary operator missing its left operand takes
//!   the last one used, so `1+` is 1. Right after `[` or `{` the operator
//!   is binary, so `[-1]` reads `[0-1]` and `[~1]` is invalid.
//! - **Departures.** A closing bracket must match the innermost open one
//!   (Mesen2 accepts some mismatched pairs, which only typos produce), and
//!   a label may contain `.`, as WLA-DX's do.

use crate::memory::View;
use crate::{ApiError, Emulator};

/// The CPU an expression is evaluated for: it picks the registers and the
/// address space `[ ]`, `{ }`, `#` and `:` work in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExprCpu {
    /// The 65C816 on the 24-bit bus.
    Snes,
    /// The SPC700 on its 16-bit bus.
    Spc,
}

/// What an evaluation produced. Every kind but `Numeric` and `Boolean`
/// comes with the value 0.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ExprKind {
    /// A number.
    Numeric,
    /// A truth value (1 or 0), from a comparison, `&&`, `||` or a flag.
    Boolean,
    /// The expression does not parse, or an operator lacked its operand.
    Invalid,
    /// A division or modulo by zero.
    DivideBy0,
    /// A label this CPU cannot see.
    OutOfScope,
}

/// An evaluation's value and kind.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExprResult {
    /// The value, clamped to −2 147 483 648 … 4 294 967 295.
    pub value: i64,
    /// What it is.
    pub kind: ExprKind,
}

impl ExprResult {
    const fn fail(kind: ExprKind) -> Self {
        Self { value: 0, kind }
    }

    /// Whether a condition holds: a non-zero value of a valid evaluation.
    #[must_use]
    pub const fn holds(self) -> bool {
        matches!(self.kind, ExprKind::Numeric | ExprKind::Boolean) && self.value != 0
    }
}

/// The memory access an expression is checked against: what `value`,
/// `address`, `memaddress`, `isread`… stand for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExprAccess {
    /// The byte read or written, or the opcode fetched.
    pub value: i64,
    /// The CPU address of the access.
    pub address: i64,
    /// The offset of the memory behind it (−1 for none).
    pub memaddress: i64,
    /// A write (else a read, a fetch or an execution).
    pub write: bool,
    /// A DMA cycle.
    pub dma: bool,
    /// A dummy (dead) bus cycle.
    pub dummy: bool,
    /// The address of the instruction's first byte.
    pub oppc: i64,
}

/// The tokens every CPU shares, about the access.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Special {
    Value,
    Address,
    MemAddress,
    OpPc,
    IsRead,
    IsWrite,
    IsDma,
    IsDummy,
}

/// A register or state token of one CPU.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Reg {
    A,
    X,
    Y,
    Sp,
    D,
    Db,
    Pc,
    Ps,
    /// A bit of the 65C816's P.
    Flag(u8),
    Nmi,
    Irq,
    Frame,
    Scanline,
    HClock,
    Cycle,
    DspIndex,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Op {
    Mul,
    Div,
    Mod,
    Add,
    Sub,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Or,
    Plus,
    Neg,
    BitNot,
    Not,
    Absolute,
    Read32,
    Read8,
    Read16,
}

impl Op {
    const fn precedence(self) -> u8 {
        match self {
            Self::Plus | Self::Neg | Self::BitNot | Self::Not | Self::Absolute | Self::Read32 => 11,
            Self::Mul | Self::Div | Self::Mod => 10,
            Self::Add | Self::Sub => 9,
            Self::Shl | Self::Shr => 8,
            Self::Lt | Self::Le | Self::Gt | Self::Ge => 7,
            Self::Eq | Self::Ne => 6,
            Self::BitAnd => 5,
            Self::BitXor => 4,
            Self::BitOr => 3,
            Self::And => 2,
            Self::Or => 1,
            Self::Read8 | Self::Read16 => 0,
        }
    }

    const fn unary(self) -> bool {
        self.precedence() == 11 || matches!(self, Self::Read8 | Self::Read16)
    }
}

/// Where a label points, resolved when the expression is compiled.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LabelAt {
    Address(i64),
    OutOfScope,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Item {
    Num(i64),
    Reg(Reg),
    Special(Special),
    Label(LabelAt),
    Op(Op),
}

/// A compiled expression, ready to evaluate many times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expression {
    cpu: ExprCpu,
    program: Vec<Item>,
}

/// An opening bracket on the operator stack.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Pending {
    Op(Op),
    Paren,
    Byte,
    Word,
}

const fn reg_named(cpu: ExprCpu, name: &str) -> Option<Reg> {
    let snes: &[(&str, Reg)] = &[
        ("a", Reg::A),
        ("x", Reg::X),
        ("y", Reg::Y),
        ("sp", Reg::Sp),
        ("d", Reg::D),
        ("db", Reg::Db),
        ("pc", Reg::Pc),
        ("ps", Reg::Ps),
        ("pscarry", Reg::Flag(0)),
        ("pszero", Reg::Flag(1)),
        ("psinterrupt", Reg::Flag(2)),
        ("psdecimal", Reg::Flag(3)),
        ("psindex", Reg::Flag(4)),
        ("psmemory", Reg::Flag(5)),
        ("psoverflow", Reg::Flag(6)),
        ("psnegative", Reg::Flag(7)),
        ("nmi", Reg::Nmi),
        ("irq", Reg::Irq),
        ("frame", Reg::Frame),
        ("scanline", Reg::Scanline),
        ("hclock", Reg::HClock),
        ("cycle", Reg::Cycle),
    ];
    let spc: &[(&str, Reg)] = &[
        ("a", Reg::A),
        ("x", Reg::X),
        ("y", Reg::Y),
        ("sp", Reg::Sp),
        ("ps", Reg::Ps),
        ("pc", Reg::Pc),
        ("dspreg", Reg::DspIndex),
    ];
    let table = match cpu {
        ExprCpu::Snes => snes,
        ExprCpu::Spc => spc,
    };
    let mut i = 0;
    while i < table.len() {
        if table[i].0.eq_ignore_ascii_case(name) {
            return Some(table[i].1);
        }
        i += 1;
    }
    None
}

const fn special_named(name: &str) -> Option<Special> {
    let table: [(&str, Special); 8] = [
        ("value", Special::Value),
        ("address", Special::Address),
        ("memaddress", Special::MemAddress),
        ("oppc", Special::OpPc),
        ("isread", Special::IsRead),
        ("iswrite", Special::IsWrite),
        ("isdma", Special::IsDma),
        ("isdummy", Special::IsDummy),
    ];
    let mut i = 0;
    while i < table.len() {
        if table[i].0.eq_ignore_ascii_case(name) {
            return Some(table[i].1);
        }
        i += 1;
    }
    None
}

/// The operator spelled at the start of `s` when an operand is expected
/// (`unary`) or not, and its length.
fn read_op(s: &[u8], unary: bool) -> Option<(Op, usize)> {
    let two = s.get(..2).unwrap_or(&[]);
    if !unary {
        let op = match two {
            b"<<" => Some(Op::Shl),
            b">>" => Some(Op::Shr),
            b"<=" => Some(Op::Le),
            b">=" => Some(Op::Ge),
            b"==" => Some(Op::Eq),
            b"!=" => Some(Op::Ne),
            b"&&" => Some(Op::And),
            b"||" => Some(Op::Or),
            _ => None,
        };
        if let Some(op) = op {
            return Some((op, 2));
        }
    }
    let op = match (s[0], unary) {
        (b'+', true) => Op::Plus,
        (b'-', true) => Op::Neg,
        (b'~', true) => Op::BitNot,
        (b'!', true) => Op::Not,
        (b':', true) => Op::Absolute,
        (b'#', true) => Op::Read32,
        (b'*', false) => Op::Mul,
        (b'/', false) => Op::Div,
        (b'%', false) => Op::Mod,
        (b'+', false) => Op::Add,
        (b'-', false) => Op::Sub,
        (b'<', false) => Op::Lt,
        (b'>', false) => Op::Gt,
        (b'&', false) => Op::BitAnd,
        (b'^', false) => Op::BitXor,
        (b'|', false) => Op::BitOr,
        _ => return None,
    };
    Some((op, 1))
}

const fn ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || matches!(c, b'_' | b'@' | b'\'')
}

const fn ident_char(c: u8) -> bool {
    ident_start(c) || c.is_ascii_digit() || c == b'.'
}

/// Literal digits in `radix` accumulated in 32 bits (only the last 8 hex
/// or 32 binary digits count), or `None` if there are none.
fn read_radix(s: &[u8], radix: u32) -> Option<(i64, usize)> {
    let n = s
        .iter()
        .take_while(|c| char::from(**c).is_digit(radix))
        .count();
    if n == 0 {
        return None;
    }
    let v = s[..n].iter().fold(0u32, |v, &c| {
        v.wrapping_mul(radix)
            .wrapping_add(char::from(c).to_digit(radix).unwrap_or(0))
    });
    Some((i64::from(v), n))
}

impl Expression {
    /// Compile `text` for `cpu`; `label` resolves a name to where it
    /// points. `None` when the expression is invalid.
    fn compile(text: &str, cpu: ExprCpu, label: &dyn Fn(&str) -> Option<LabelAt>) -> Option<Self> {
        let text: Vec<u8> = text.bytes().filter(|&c| c != b' ').collect();
        if text.is_empty() {
            return None;
        }
        let mut out: Vec<Item> = Vec::new();
        let mut stack: Vec<Pending> = Vec::new();
        let (mut expect, mut after_value) = (true, false);
        let mut i = 0;
        // Move pending operators that bind at least as tightly as `op`
        // (prefix operators are right-associative, so never) to the output.
        let push_op = |op: Op, stack: &mut Vec<Pending>, out: &mut Vec<Item>| {
            while let Some(&Pending::Op(top)) = stack.last() {
                if op.unary() || top.precedence() < op.precedence() {
                    break;
                }
                out.push(Item::Op(top));
                stack.pop();
            }
            stack.push(Pending::Op(op));
        };
        while i < text.len() {
            let s = &text[i..];
            let c = s[0];
            let value = |out: &mut Vec<Item>, item: Item| {
                out.push(item);
            };
            // A value, or an opening bracket, may not follow a value.
            let starts_value = c.is_ascii_digit()
                || ident_start(c)
                || c == b'$'
                || matches!(c, b'(' | b'[' | b'{')
                || (c == b'%' && expect && s.get(1).is_some_and(|d| matches!(d, b'0' | b'1')));
            if starts_value && after_value {
                return None;
            }
            match c {
                b'(' | b'[' | b'{' => {
                    stack.push(match c {
                        b'(' => Pending::Paren,
                        b'[' => Pending::Byte,
                        _ => Pending::Word,
                    });
                    // Mesen2: an operand is expected after `(` only.
                    expect = c == b'(';
                    after_value = false;
                    i += 1;
                }
                b')' | b']' | b'}' => {
                    let want = match c {
                        b')' => Pending::Paren,
                        b']' => Pending::Byte,
                        _ => Pending::Word,
                    };
                    loop {
                        match stack.pop() {
                            Some(Pending::Op(op)) => out.push(Item::Op(op)),
                            Some(p) if p == want => break,
                            _ => return None,
                        }
                    }
                    match want {
                        Pending::Byte => out.push(Item::Op(Op::Read8)),
                        Pending::Word => out.push(Item::Op(Op::Read16)),
                        _ => {}
                    }
                    expect = false;
                    after_value = true;
                    i += 1;
                }
                b'$' => {
                    let (v, n) = read_radix(&s[1..], 16)?;
                    value(&mut out, Item::Num(v));
                    (expect, after_value) = (false, true);
                    i += 1 + n;
                }
                b'0' if matches!(s.get(1), Some(b'x' | b'X')) => {
                    let (v, n) = read_radix(&s[2..], 16)?;
                    value(&mut out, Item::Num(v));
                    (expect, after_value) = (false, true);
                    i += 2 + n;
                }
                b'%' if expect => {
                    let (v, n) = read_radix(&s[1..], 2)?;
                    value(&mut out, Item::Num(v));
                    (expect, after_value) = (false, true);
                    i += 1 + n;
                }
                b'0'..=b'9' => {
                    let n = s.iter().take_while(|c| c.is_ascii_digit()).count();
                    let v: i64 = std::str::from_utf8(&s[..n]).ok()?.parse().ok()?;
                    value(&mut out, Item::Num(v));
                    (expect, after_value) = (false, true);
                    i += n;
                }
                _ if ident_start(c) => {
                    let n = s.iter().take_while(|c| ident_char(**c)).count();
                    let name = std::str::from_utf8(&s[..n]).ok()?;
                    let item = if let Some(r) = reg_named(cpu, name) {
                        Item::Reg(r)
                    } else if let Some(sp) = special_named(name) {
                        Item::Special(sp)
                    } else {
                        Item::Label(label(name)?)
                    };
                    value(&mut out, item);
                    (expect, after_value) = (false, true);
                    i += n;
                }
                _ => {
                    let (op, n) = read_op(s, expect)?;
                    push_op(op, &mut stack, &mut out);
                    (expect, after_value) = (true, false);
                    i += n;
                }
            }
        }
        while let Some(p) = stack.pop() {
            match p {
                Pending::Op(op) => out.push(Item::Op(op)),
                _ => return None,
            }
        }
        if out.is_empty() {
            return None;
        }
        Some(Self { cpu, program: out })
    }

    /// The CPU this expression was compiled for.
    #[must_use]
    pub const fn cpu(&self) -> ExprCpu {
        self.cpu
    }
}

/// What an expression reads from the machine.
trait ExprEnv {
    fn reg(&self, cpu: ExprCpu, reg: Reg) -> i64;
    fn byte(&self, cpu: ExprCpu, addr: u32) -> u8;
    fn space(&self, cpu: ExprCpu) -> u32;
    fn absolute(&self, cpu: ExprCpu, addr: u32) -> i64;
}

/// The values of the boolean register tokens.
const fn boolean_reg(r: Reg) -> bool {
    matches!(r, Reg::Flag(_) | Reg::Nmi | Reg::Irq)
}

fn evaluate(expr: &Expression, access: &ExprAccess, env: &dyn ExprEnv) -> ExprResult {
    let cpu = expr.cpu;
    let mut stack: Vec<i64> = Vec::new();
    let mut kind = ExprKind::Numeric;
    let mut last_left = 0i64;
    let read = |addr: i64, n: u32| -> i64 {
        let size = env.space(cpu);
        let base = addr as u32;
        (0..n).fold(0i64, |v, k| {
            let a = base.wrapping_add(k);
            // A byte past the end comes from address 0, as in Mesen2.
            let a = if k > 0 && a >= size { 0 } else { a };
            let b = if a >= size { 0 } else { env.byte(cpu, a) };
            v | (i64::from(b) << (8 * k))
        })
    };
    for item in &expr.program {
        let v = match *item {
            Item::Num(v) => v,
            Item::Reg(r) => {
                if boolean_reg(r) {
                    kind = ExprKind::Boolean;
                }
                env.reg(cpu, r)
            }
            Item::Special(s) => match s {
                Special::Value => access.value,
                Special::Address => access.address,
                Special::MemAddress => access.memaddress,
                Special::OpPc => access.oppc,
                Special::IsRead => i64::from(!access.write),
                Special::IsWrite => i64::from(access.write),
                Special::IsDma => i64::from(access.dma),
                Special::IsDummy => i64::from(access.dummy),
            },
            Item::Label(LabelAt::Address(a)) => a,
            Item::Label(LabelAt::OutOfScope) => return ExprResult::fail(ExprKind::OutOfScope),
            Item::Op(op) if op.unary() => {
                let Some(x) = stack.pop() else {
                    return ExprResult::fail(ExprKind::Invalid);
                };
                kind = ExprKind::Numeric;
                match op {
                    Op::Plus => x,
                    Op::Neg => x.wrapping_neg(),
                    Op::BitNot => !x,
                    Op::Not => i64::from(x == 0),
                    Op::Absolute if x < 0 => -1,
                    Op::Absolute => env.absolute(cpu, x as i32 as u32),
                    Op::Read8 => read(x, 1),
                    Op::Read16 => read(x, 2),
                    _ => read(x, 4),
                }
            }
            Item::Op(op) => {
                let Some(right) = stack.pop() else {
                    return ExprResult::fail(ExprKind::Invalid);
                };
                let left = match stack.pop() {
                    Some(l) => {
                        last_left = l;
                        l
                    }
                    None => last_left,
                };
                kind = if matches!(
                    op,
                    Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne | Op::And | Op::Or
                ) {
                    ExprKind::Boolean
                } else {
                    ExprKind::Numeric
                };
                match op {
                    Op::Mul => left.wrapping_mul(right),
                    Op::Div | Op::Mod if right == 0 => {
                        return ExprResult::fail(ExprKind::DivideBy0);
                    }
                    Op::Div => left.wrapping_div(right),
                    Op::Mod => left.wrapping_rem(right),
                    Op::Add => left.wrapping_add(right),
                    Op::Sub => left.wrapping_sub(right),
                    Op::Shl => left.wrapping_shl(right as u32 & 63),
                    Op::Shr => left.wrapping_shr(right as u32 & 63),
                    Op::Lt => i64::from(left < right),
                    Op::Le => i64::from(left <= right),
                    Op::Gt => i64::from(left > right),
                    Op::Ge => i64::from(left >= right),
                    Op::Eq => i64::from(left == right),
                    Op::Ne => i64::from(left != right),
                    Op::BitAnd => left & right,
                    Op::BitXor => left ^ right,
                    Op::BitOr => left | right,
                    Op::And => i64::from(left != 0 && right != 0),
                    _ => i64::from(left != 0 || right != 0),
                }
            }
        };
        if stack.len() >= 99 {
            return ExprResult::fail(ExprKind::Invalid);
        }
        stack.push(v);
    }
    match stack.first() {
        Some(&v) => ExprResult {
            value: v.clamp(-0x8000_0000, 0xFFFF_FFFF),
            kind,
        },
        None => ExprResult::fail(ExprKind::Invalid),
    }
}

/// The PPU dot of master clock `hclock` in a line: four clocks each, but
/// dots 323 and 327 last six.
const fn dot_of(hclock: i64) -> i64 {
    if hclock < 1296 {
        hclock / 4
    } else if hclock < 1314 {
        (hclock - 2) / 4
    } else {
        (hclock - 4) / 4
    }
}

impl ExprEnv for View<'_> {
    fn reg(&self, cpu: ExprCpu, reg: Reg) -> i64 {
        let snes = self.0;
        match cpu {
            ExprCpu::Snes => {
                let c = &snes.cpu;
                let hclock = i64::from(snes.mcycles_in_line.min(u32::from(u16::MAX)));
                match reg {
                    Reg::A => i64::from(c.a),
                    Reg::X => i64::from(c.x),
                    Reg::Y => i64::from(c.y),
                    Reg::Sp => i64::from(c.sp),
                    Reg::D => i64::from(c.dp),
                    Reg::Db => i64::from(c.db),
                    Reg::Pc => (i64::from(c.pb) << 16) | i64::from(c.pc),
                    Reg::Ps => i64::from(c.p.bits()),
                    Reg::Flag(b) => i64::from((c.p.bits() >> b) & 1),
                    Reg::Nmi => i64::from(snes.nmi_pending),
                    Reg::Irq => {
                        i64::from(snes.cpu_regs.irq_flag || snes.mapper.coproc_main_irq_pending())
                    }
                    Reg::Frame => i64::from(snes.frame_count as u32),
                    Reg::Scanline => i64::from(snes.ppu_line),
                    Reg::HClock => hclock,
                    Reg::Cycle => dot_of(hclock),
                    Reg::DspIndex => 0,
                }
            }
            ExprCpu::Spc => spc_reg(&snes.apu_real, reg),
        }
    }

    fn byte(&self, cpu: ExprCpu, addr: u32) -> u8 {
        match cpu {
            // Mesen2 reads 0 where nothing answers.
            ExprCpu::Snes => self.bus_peek(addr, 0),
            ExprCpu::Spc => spc_byte(&self.0.apu_real, addr),
        }
    }

    fn space(&self, cpu: ExprCpu) -> u32 {
        match cpu {
            ExprCpu::Snes => 0x100_0000,
            ExprCpu::Spc => 0x1_0000,
        }
    }

    fn absolute(&self, cpu: ExprCpu, addr: u32) -> i64 {
        match cpu {
            ExprCpu::Snes => {
                let addr = addr & 0x00FF_FFFF;
                if let Some(l) = View::absolute(*self, addr) {
                    return i64::from(l.offset);
                }
                let (bank, off) = (addr >> 16, addr & 0xFFFF);
                let system = bank <= 0x3F || (0x80..=0xBF).contains(&bank);
                if system && (0x2000..=0x5FFF).contains(&off) {
                    i64::from(off)
                } else {
                    -1
                }
            }
            ExprCpu::Spc => spc_absolute(&self.0.apu_real, addr),
        }
    }
}

/// An SPC700 register token's value.
fn spc_reg(apu: &luna_apu::Apu, reg: Reg) -> i64 {
    let c = &apu.cpu;
    match reg {
        Reg::A => i64::from(c.a),
        Reg::X => i64::from(c.x),
        Reg::Y => i64::from(c.y),
        Reg::Sp => i64::from(c.sp),
        Reg::Ps => i64::from(c.psw.0),
        Reg::Pc => i64::from(c.pc),
        Reg::DspIndex => i64::from(apu.dsp_index),
        _ => 0,
    }
}

/// An SPC700 bus byte as Mesen2 peeks it: `$F0` / `$F1` read 0, `$F2` the
/// DSP index, `$F3` the DSP register it selects, the IPL ROM over the top
/// page while it is enabled.
fn spc_byte(apu: &luna_apu::Apu, addr: u32) -> u8 {
    match addr {
        0xF0 | 0xF1 => 0,
        0xF2 => apu.dsp_index,
        0xF3 => apu.dsp.registers[usize::from(apu.dsp_index & 0x7F)],
        _ => apu.peek(addr as u16),
    }
}

/// `:addr` on the SPC700: an IPL ROM offset over the top page while it is
/// enabled, else the ARAM offset.
fn spc_absolute(apu: &luna_apu::Apu, addr: u32) -> i64 {
    let a = addr & 0xFFFF;
    if a >= 0xFFC0 && apu.control & 0x80 != 0 {
        i64::from(a & 0x3F)
    } else {
        i64::from(a)
    }
}

/// The APU alone, for an SPC700 breakpoint's condition (checked inside
/// the APU, at the start of an instruction).
struct ApuView<'a>(&'a luna_apu::Apu);

impl ExprEnv for ApuView<'_> {
    fn reg(&self, _cpu: ExprCpu, reg: Reg) -> i64 {
        spc_reg(self.0, reg)
    }

    fn byte(&self, _cpu: ExprCpu, addr: u32) -> u8 {
        spc_byte(self.0, addr)
    }

    fn space(&self, _cpu: ExprCpu) -> u32 {
        0x1_0000
    }

    fn absolute(&self, _cpu: ExprCpu, addr: u32) -> i64 {
        spc_absolute(self.0, addr)
    }
}

/// Whether an SPC700 exec breakpoint's condition holds: evaluated as
/// Mesen2 does at the instruction's start (`value` the opcode, `address`
/// the PC).
pub(crate) fn spc_holds(
    expr: &Expression,
    apu: &luna_apu::Apu,
    access: Option<luna_apu::SpcAccess>,
) -> bool {
    let pc = apu.cpu.pc;
    let access = match access {
        // A watchpoint's: mid-instruction, `pc` has moved past the opcode.
        Some(a) => ExprAccess {
            value: i64::from(a.value),
            address: i64::from(a.addr),
            memaddress: if a.write {
                i64::from(a.addr)
            } else {
                spc_absolute(apu, u32::from(a.addr))
            },
            write: a.write,
            dma: false,
            dummy: false,
            oppc: i64::from(apu.instruction_pc()),
        },
        None => ExprAccess {
            value: i64::from(apu.peek(pc)),
            address: i64::from(pc),
            memaddress: spc_absolute(apu, u32::from(pc)),
            write: false,
            dma: false,
            dummy: false,
            oppc: i64::from(pc),
        },
    };
    evaluate(expr, &access, &ApuView(apu)).holds()
}

/// Evaluate `expr` on a machine the caller only holds as its core.
pub(crate) fn evaluate_on(expr: &Expression, access: &ExprAccess, view: View<'_>) -> ExprResult {
    evaluate(expr, access, &view)
}

impl Emulator {
    /// Compile `text` for `cpu`, or `None` if it is invalid. Labels are
    /// resolved now from the loaded `.sym`: in the CPU's own table first;
    /// a CPU-bus label in an SPC700 expression is out of scope.
    pub fn compile_expression(&self, text: &str, cpu: ExprCpu) -> Option<Expression> {
        let label = |name: &str| -> Option<LabelAt> {
            let bus = self.lookup_symbol(name).ok();
            let spc = self.resolve_symbol_spc(name);
            match (cpu, bus, spc) {
                (ExprCpu::Snes, Some(a), _) => Some(LabelAt::Address(i64::from(a))),
                (_, _, Some(a)) => Some(LabelAt::Address(i64::from(a))),
                (ExprCpu::Spc, Some(_), None) => Some(LabelAt::OutOfScope),
                _ => None,
            }
        };
        Expression::compile(text, cpu, &label)
    }

    /// Evaluate a compiled expression against `access`.
    pub fn evaluate_expression(
        &self,
        expr: &Expression,
        access: &ExprAccess,
    ) -> Result<ExprResult, ApiError> {
        Ok(evaluate(expr, access, &self.view()?))
    }

    /// Evaluate `text` for `cpu` as a watch does: no access (`value` and
    /// `address` 0, `isread` 1), `oppc` the instruction about to run.
    pub fn evaluate(&self, text: &str, cpu: ExprCpu) -> Result<ExprResult, ApiError> {
        self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let oppc = match cpu {
            ExprCpu::Snes => {
                let c = self.cpu_state()?;
                (i64::from(c.pb) << 16) | i64::from(c.pc)
            }
            ExprCpu::Spc => i64::from(self.spc700_state()?.pc),
        };
        let access = ExprAccess {
            value: 0,
            address: 0,
            memaddress: 0,
            write: false,
            dma: false,
            dummy: false,
            oppc,
        };
        let view = self.view()?;
        Ok(self
            .compile_expression(text, cpu)
            .map_or(ExprResult::fail(ExprKind::Invalid), |e| {
                evaluate(&e, &access, &view)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine with fixed registers and memory, for the language alone.
    struct Fixed;

    impl ExprEnv for Fixed {
        fn reg(&self, _cpu: ExprCpu, reg: Reg) -> i64 {
            match reg {
                Reg::A => 0x1234,
                Reg::X => 5,
                Reg::Y => 0xFF,
                Reg::Sp => 0x01F0,
                Reg::Db => 0x7E,
                Reg::Pc => 0x80_8123,
                Reg::Ps => 0x31,
                Reg::Flag(b) => i64::from((0x31u8 >> b) & 1),
                Reg::Scanline => 10,
                Reg::HClock => 400,
                Reg::Cycle => 100,
                Reg::Frame => 1000,
                _ => 0,
            }
        }

        fn byte(&self, _cpu: ExprCpu, addr: u32) -> u8 {
            match addr {
                0x05 => 0xAA,
                0x10 => 0x34,
                0x11 => 0x12,
                0x12 => 0x78,
                0x13 => 0x56,
                0x14 => 0x9A,
                0xFF_FFFF => 0x11,
                0xFF_FFFE => 0x77,
                0x00 => 0x22,
                0x01 => 0x33,
                0x04 => 0x44,
                0x33 => 0x66,
                _ => 0,
            }
        }

        fn space(&self, _cpu: ExprCpu) -> u32 {
            0x100_0000
        }

        fn absolute(&self, _cpu: ExprCpu, addr: u32) -> i64 {
            match addr {
                0x00_8000 => 0,
                0x7E_0010 => 0x10,
                0x00_2100 => 0x2100,
                0x7F_0000 => 0x1_0000,
                _ => -1,
            }
        }
    }

    fn eval(text: &str) -> (i64, ExprKind) {
        let label = |name: &str| match name {
            "Player" => Some(LabelAt::Address(0x7E_0100)),
            "Reset" => Some(LabelAt::Address(0x80_8000)),
            "INIDISP" => Some(LabelAt::Address(0x2100)),
            _ => None,
        };
        let watch = ExprAccess {
            value: 0,
            address: 0,
            memaddress: 0,
            write: false,
            dma: false,
            dummy: false,
            oppc: 0x80_8120,
        };
        match Expression::compile(text, ExprCpu::Snes, &label) {
            Some(e) => {
                let r = evaluate(&e, &watch, &Fixed);
                (r.value, r.kind)
            }
            None => (0, ExprKind::Invalid),
        }
    }

    use ExprKind::{Boolean as B, DivideBy0, Invalid, Numeric as N};

    #[test]
    fn the_reference_examples_give_mesen2_results() {
        let cases: &[(&str, i64, ExprKind)] = &[
            ("1+3*3+10/(3+4)", 11, N),
            ("(1+3*3+10)/3+4", 10, N),
            ("1 - -1", 2, N),
            ("1 - -(-1)", 0, N),
            ("-2*-3", 6, N),
            ("--1", 1, N),
            ("~0", -1, N),
            ("~0 & $FFFF", 65535, N),
            ("(~0 & ~1) & $FFF == $FFE", 0, N),
            ("((~0 & ~1) & $FFF) == $FFE", 1, B),
            ("10 == $A", 1, B),
            ("0x1F + $1f", 62, N),
            ("%1011", 11, N),
            ("%12", 0, Invalid),
            ("10%4*10", 20, N),
            ("11%%10", 1, N),
            ("%100%5", 4, N),
            ("1 2", 12, N),
            ("1 = = 1", 1, B),
            ("1=1", 0, Invalid),
            ("10 / 0", 0, DivideBy0),
            ("0 && 1/0", 0, DivideBy0),
            ("-7/2", -3, N),
            ("-7%2", -1, N),
            ("7%-2", 1, N),
            ("1<<32", 4_294_967_295, N),
            ("(1<<32)>>4", 268_435_456, N),
            ("1<<64", 1, N),
            ("-16>>2", -4, N),
            ("-1 == $FFFFFFFF", 0, B),
            ("$FFFFFFFF + 1", 4_294_967_295, N),
            ("-3000000000", -2_147_483_648, N),
            ("$123456789", 0x2345_6789, N),
            ("9223372036854775808", 0, Invalid),
            ("1<2<3", 1, B),
            ("3>2>1", 0, B),
            ("!5", 0, N),
            ("!0", 1, N),
            ("!x == 0", 1, B),
            ("a", 0x1234, N),
            ("A + X", 0x1239, N),
            ("pc", 0x80_8123, N),
            ("ps", 0x31, N),
            ("pscarry", 1, B),
            ("pszero", 0, B),
            ("psindex", 1, B),
            ("pscarry + 0", 1, N),
            ("x == y == scanline", 0, B),
            ("scanline == 10 && (cycle >= 55 && cycle <= 100)", 1, B),
            ("[$10]", 0x34, N),
            ("{$10}", 0x1234, N),
            ("#$10", 0x5678_1234, N),
            ("#$10+1", 0x5678_1235, N),
            ("#($10+1)", 0x9A56_7812, N),
            ("[x]", 0xAA, N),
            ("[[$12] - $73]", 0xAA, N),
            ("{$FFFFFF}", 0x2211, N),
            ("#$FFFFFE", 0x2222_1177, N),
            ("[$1000000]", 0, N),
            ("[-1]", 0, N),
            ("5+[-1]", 5 + 0x44, N),
            ("[%101]", 0x22, N),
            ("[~1]", 0, Invalid),
            ("[:$8000]", 0, Invalid),
            ("[(~1)]", 0, N),
            (":$008000", 0, N),
            (":$7E0010", 16, N),
            (":$2100", 0x2100, N),
            (":$7F0000", 0x1_0000, N),
            (":(-5)", -1, N),
            ("1+", 1, N),
            ("5*", 0, N),
            ("*5", 0, Invalid),
            ("==5", 0, Invalid),
            ("()", 0, Invalid),
            ("1+()", 1, N),
            ("(3)4", 0, Invalid),
            ("[$45]3", 0, Invalid),
            ("($45]", 0, Invalid),
            ("(1", 0, Invalid),
            ("$", 0, Invalid),
            ("0x", 0, Invalid),
            ("0xg", 0, Invalid),
            ("1.5", 0, Invalid),
            ("a?1:2", 0, Invalid),
            ("x y", 0, Invalid),
            ("Player", 0x7E_0100, N),
            ("player", 0, Invalid),
            ("Reset", 0x80_8000, N),
            ("INIDISP", 0x2100, N),
            ("value", 0, N),
            ("address", 0, N),
            ("isread", 1, N),
            ("iswrite", 0, N),
            ("oppc", 0x80_8120, N),
            ("1+(-)", -1, N),
            ("-", 0, Invalid),
            ("[1]\t", 0, Invalid),
        ];
        for &(text, value, kind) in cases {
            assert_eq!(eval(text), (value, kind), "`{text}`");
        }
        // The operand stack holds 99 values.
        let deep = format!("{}1{}", "1+(".repeat(100), ")".repeat(100));
        assert_eq!(eval(&deep).1, Invalid);
    }

    #[test]
    fn dots_323_and_327_last_six_clocks() {
        assert_eq!(dot_of(1291), 322);
        assert_eq!((1292..1298).map(dot_of).collect::<Vec<_>>(), [323; 6]);
        assert_eq!(dot_of(1298), 324);
        assert_eq!((1310..1316).map(dot_of).collect::<Vec<_>>(), [327; 6]);
        assert_eq!(dot_of(1316), 328);
    }
}
