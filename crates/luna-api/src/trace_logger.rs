//! The Trace Logger, Mesen2's: one text row per instruction a CPU runs,
//! laid out by a format string of tags.
//!
//! A format is text and tags: `[A,4h]` writes the accumulator in
//! hexadecimal on at least 4 digits, `[Scanline,3]` the scanline in decimal
//! padded to 3 characters, `[Align,38]` pads the row to column 38. The
//! format a row gets by default is the one Mesen2 builds from its options
//! ([`auto_format`]); any other is given as text. A condition, in the
//! debugger's expression language, keeps only the rows it holds for:
//! checked at the opcode fetch, then — when it failed there — on each
//! memory access the instruction makes, the row being logged at the first
//! access it holds for (Mesen2's `LogNonExec`).
//!
//! A row is taken where Mesen2 takes it: the registers before the
//! instruction, the beam once its opcode fetch has ended. Its effective
//! address and memory value come from running the instruction on a copy
//! of the CPU against a bus that only peeks — the last data access it
//! makes, as Mesen2's dummy CPU finds it. Each CPU keeps its last 30 000
//! rows; a file, when one is open, receives every row prefixed by the
//! program counter.
//!
//! Rows are formatted when they are taken, memory values included: the
//! value is the one before the instruction, in the window as in the file.
//! (Mesen2 formats its window's rows when it draws them, from memory as it
//! is then.) A memory value in decimal is written as such; Mesen2 writes
//! `= ` alone there.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;

use luna_bus::{Addr24, Bus, MCycles};
use luna_core::{FetchPoint, Snes, StepAccess};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::expr::{ExprAccess, ExprCpu, Expression, evaluate_on};
use crate::labels::LabelStore;
use crate::listing::{ListingCpu, bus_name};
use crate::memory::View;
use crate::{ApiError, Emulator, SymbolTable};

/// Rows each CPU keeps (Mesen2's `ExecutionLogSize`).
pub const TRACE_ROWS: usize = 30_000;

/// A CPU the trace logger follows.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TraceCpu {
    /// The 65C816.
    #[default]
    Snes,
}

/// How the auto format writes the status flags.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusFormat {
    /// Every flag, upper case when set (`nvMXdIzc`).
    #[default]
    Text,
    /// The set flags only (`MXI`).
    CompactText,
    /// The byte in hexadecimal.
    Hexadecimal,
}

/// The options Mesen2 builds a CPU's format from (its defaults).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TraceFormatOptions {
    /// The registers.
    pub show_registers: bool,
    /// The status flags.
    pub show_status_flags: bool,
    /// How the flags are written.
    pub status_format: StatusFormat,
    /// The effective address after the instruction.
    pub show_effective_addresses: bool,
    /// The value at the effective address.
    pub show_memory_values: bool,
    /// The instruction's bytes.
    pub show_byte_code: bool,
    /// The CPU cycle count.
    pub show_clock_counter: bool,
    /// The frame count.
    pub show_frame_counter: bool,
    /// The scanline and the dot.
    pub show_frame_position: bool,
}

impl Default for TraceFormatOptions {
    fn default() -> Self {
        Self {
            show_registers: true,
            show_status_flags: true,
            status_format: StatusFormat::Text,
            show_effective_addresses: true,
            show_memory_values: true,
            show_byte_code: false,
            show_clock_counter: false,
            show_frame_counter: false,
            show_frame_position: true,
        }
    }
}

/// The format Mesen2 builds for `cpu` from `o` (its "auto format").
#[must_use]
pub fn auto_format(cpu: TraceCpu, o: &TraceFormatOptions) -> String {
    let mut format = String::from("[Disassembly]");
    let mut align = 24;
    if o.show_effective_addresses {
        format.push_str("[EffectiveAddress]");
        align += 8;
    }
    if o.show_memory_values {
        format.push_str(" [MemoryValue,h]");
        align += 6;
    }
    let _ = write!(format, "[Align,{align}] ");
    match cpu {
        TraceCpu::Snes => {
            if o.show_registers {
                format.push_str("A:[A,4h] X:[X,4h] Y:[Y,4h] S:[SP,4h] D:[D,4h] DB:[DB,2h] ");
            }
            if o.show_status_flags {
                format.push_str(match o.status_format {
                    StatusFormat::Hexadecimal => "P:[P,h] ",
                    StatusFormat::CompactText => "P:[P] ",
                    StatusFormat::Text => "P:[P,8] ",
                });
            }
        }
    }
    if o.show_frame_position {
        format.push_str("V:[Scanline,3] H:[Cycle,3] ");
    }
    if o.show_frame_counter {
        format.push_str("Fr:[FrameCount] ");
    }
    if o.show_clock_counter {
        format.push_str("Cycle:[CycleCount] ");
    }
    if o.show_byte_code {
        format.push_str("BC:[ByteCode]");
    }
    format.trim().to_string()
}

/// What a CPU's trace logger writes, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TraceOptions {
    /// The format; `None` is [`auto_format`] with Mesen2's defaults.
    pub format: Option<String>,
    /// A condition in the debugger's expression language; empty logs
    /// every instruction.
    pub condition: String,
    /// Names from the labels and the loaded symbols in the disassembly
    /// and the effective address.
    pub use_labels: bool,
    /// Indents the disassembly by the stack depth, half a column per
    /// byte below `$xxFF`.
    pub indent_code: bool,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            format: None,
            condition: String::new(),
            use_labels: true,
            indent_code: false,
        }
    }
}

/// A logged row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TraceRow {
    /// Rows are numbered across the CPUs, in the order they were logged.
    pub row_id: u64,
    /// The CPU that ran the instruction.
    pub cpu: TraceCpu,
    /// The instruction's address.
    pub pc: u32,
    /// Its bytes.
    pub byte_code: Vec<u8>,
    /// The row, as the format lays it out.
    pub text: String,
}

/// A format tag.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Tag {
    ByteCode,
    Disassembly,
    EffectiveAddress,
    MemoryValue,
    Align,
    Pc,
    Cycle,
    HClock,
    Scanline,
    FrameCount,
    CycleCount,
    A,
    X,
    Y,
    D,
    Db,
    Sp,
    Ps,
}

/// A piece of a parsed format.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Text(String),
    Tag { tag: Tag, width: usize, hex: bool },
}

/// The tag named `name` on `cpu`, or `None`.
fn tag_named(cpu: TraceCpu, name: &str) -> Option<Tag> {
    Some(match name {
        "ByteCode" => Tag::ByteCode,
        "Disassembly" => Tag::Disassembly,
        "EffectiveAddress" => Tag::EffectiveAddress,
        "MemoryValue" => Tag::MemoryValue,
        "Align" => Tag::Align,
        "PC" => Tag::Pc,
        "Cycle" => Tag::Cycle,
        "HClock" => Tag::HClock,
        "Scanline" => Tag::Scanline,
        "FrameCount" => Tag::FrameCount,
        "CycleCount" => Tag::CycleCount,
        _ => match (cpu, name) {
            (TraceCpu::Snes, "A") => Tag::A,
            (TraceCpu::Snes, "X") => Tag::X,
            (TraceCpu::Snes, "Y") => Tag::Y,
            (TraceCpu::Snes, "D") => Tag::D,
            (TraceCpu::Snes, "DB") => Tag::Db,
            (TraceCpu::Snes, "SP") => Tag::Sp,
            (TraceCpu::Snes, "P") => Tag::Ps,
            _ => return None,
        },
    })
}

/// The inside of a `[…]`: the tag's name, its width and whether it is in
/// hexadecimal. The name runs up to the first point from which the rest
/// reads as `,` + digits + an optional `h` (Mesen2's lazy match).
fn parse_tag(inner: &str) -> (&str, usize, bool) {
    let inner = inner.trim_start();
    let suffix = |rest: &str| -> Option<(usize, bool)> {
        let rest = rest.trim();
        if rest.is_empty() {
            return Some((0, false));
        }
        let rest = rest.strip_prefix(',')?.trim();
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let (num, tail) = rest.split_at(digits);
        let tail = tail.trim();
        let hex = match tail {
            "" => false,
            // The pattern ignores case, but the flag is compared with `h`.
            "h" => true,
            "H" => false,
            _ => return None,
        };
        // A width that does not fit is dropped, as `stoi` throwing.
        Some((num.parse().unwrap_or(0), hex))
    };
    for (k, _) in inner.char_indices().chain([(inner.len(), ' ')]) {
        if let Some((width, hex)) = suffix(&inner[k..]) {
            return (inner[..k].trim_end(), width, hex);
        }
    }
    (inner.trim_end(), 0, false)
}

/// Parse a format into its parts. Text runs up to a `[`; a `[` with no
/// `]` before the next `[` is dropped; an unknown tag reads
/// `[Invalid tag]`.
fn parse_format(cpu: TraceCpu, format: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut rest = format;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let close = after.find(']');
            let open = after.find('[');
            match close {
                Some(c) if open.is_none_or(|o| o > c) => {
                    let (name, width, hex) = parse_tag(&after[..c]);
                    parts.push(tag_named(cpu, name).map_or_else(
                        || Part::Text("[Invalid tag]".into()),
                        |tag| Part::Tag { tag, width, hex },
                    ));
                    rest = &after[c + 1..];
                }
                _ => rest = after,
            }
        } else {
            let end = rest.find('[').unwrap_or(rest.len());
            parts.push(Part::Text(rest[..end].to_string()));
            rest = &rest[end..];
        }
    }
    parts
}

/// A number as Mesen2 prints it: hexadecimal on the digits of its C++
/// type, or decimal.
#[derive(Copy, Clone, Debug)]
enum Num {
    U8(u8),
    U16(u16),
    /// A 32-bit value: 2, 4, 6 or 8 hex digits as it needs.
    U32(u32),
    U64(u64),
}

impl Num {
    fn hex(self) -> String {
        match self {
            Self::U8(v) => format!("{v:02X}"),
            Self::U16(v) => format!("{v:04X}"),
            Self::U32(v) if v <= 0xFF => format!("{v:02X}"),
            Self::U32(v) if v <= 0xFFFF => format!("{v:04X}"),
            Self::U32(v) if v <= 0xFF_FFFF => format!("{v:06X}"),
            Self::U32(v) => format!("{v:08X}"),
            Self::U64(v) => format!("{v:016X}"),
        }
    }

    fn dec(self) -> String {
        match self {
            Self::U8(v) => v.to_string(),
            Self::U16(v) => v.to_string(),
            Self::U32(v) => v.to_string(),
            Self::U64(v) => v.to_string(),
        }
    }
}

/// Mesen2's `WriteIntValue`: hexadecimal padded with zeros on the left,
/// decimal padded with spaces on the right.
fn write_int(out: &mut String, v: Num, width: usize, hex: bool) {
    if hex {
        let s = v.hex();
        out.extend(std::iter::repeat_n('0', width.saturating_sub(s.len())));
        out.push_str(&s);
    } else {
        write_str(out, &v.dec(), width);
    }
}

/// Mesen2's `WriteStringValue`: the text, padded with spaces on the right.
fn write_str(out: &mut String, s: &str, width: usize) {
    out.push_str(s);
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(s.len())));
}

/// The effective address Mesen2 shows after an instruction.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct Effective {
    /// The address, if there is one.
    addr: Option<u32>,
    /// Bytes its value has (0 for a jump's target).
    size: u8,
    /// Whether the address itself is shown (the value is, regardless).
    show: bool,
}

/// Per opcode, what Mesen2's addressing-mode table makes of its effective
/// address: `s` a stack operation (nothing shown), `n` a mode without one
/// (its last data access still gives a value), `e` a mode that shows it.
const EA_CLASS: &[u8; 256] = b"\
neneeeeesnnseeen\
neeeeeeenenneeee\
eeneeeeesnnseeen\
neeeeeeenenneeee\
seneneeesnnseeen\
neeeneeenesnneee\
seneeeeesnnseeen\
neeeeeeenesneeee\
neneeeeennnseeen\
neeeeeeenenneeee\
neneeeeennnseeen\
neeeeeeenenneeee\
neneeeeennnneeen\
neeeeeeenesneeee\
neneeeeennnneeen\
neeeneeenesneeee";

/// `BRK`, `COP`, `JSR`, `JSL`, `RTI`, `JMP`, `JML`, `RTS`, `RTL`, `BRA`,
/// `BRL`: Mesen2's unconditional jumps.
const fn unconditional_jump(op: u8) -> bool {
    matches!(
        op,
        0x00 | 0x02
            | 0x20
            | 0x22
            | 0x40
            | 0x4C
            | 0x5C
            | 0x60
            | 0x6B
            | 0x6C
            | 0x7C
            | 0x80
            | 0x82
            | 0xDC
            | 0xFC
    )
}

/// The conditional branches.
const fn conditional_jump(op: u8) -> bool {
    matches!(op, 0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0)
}

/// What a bus access of the dummy CPU was.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum OpKind {
    Opcode,
    Operand,
    Read,
    Write,
}

/// The bus Mesen2's dummy CPU runs against: reads peek, writes are only
/// noted, time does not pass.
struct DummyBus<'a> {
    view: View<'a>,
    ops: Vec<(u32, OpKind)>,
}

impl Bus for DummyBus<'_> {
    fn read(&mut self, addr: Addr24) -> u8 {
        self.ops.push((addr, OpKind::Read));
        self.view.bus_peek(addr, 0)
    }

    fn write(&mut self, addr: Addr24, _value: u8) {
        self.ops.push((addr, OpKind::Write));
    }

    fn fetch(&mut self, addr: Addr24) -> u8 {
        let kind = if self.ops.is_empty() {
            OpKind::Opcode
        } else {
            OpKind::Operand
        };
        self.ops.push((addr, kind));
        self.view.bus_peek(addr, 0)
    }

    fn io_cycle(&mut self, _mcycles: MCycles) {}
}

/// The effective address of the instruction the CPU of `snes` is about to
/// run, as Mesen2 finds it: run it on a copy against a peeking bus, take
/// the last data access (both bytes of a 16-bit one), or a 3-byte jump's
/// destination.
fn effective(view: View<'_>, op: u8, len: u8) -> Effective {
    let class = EA_CLASS[op as usize];
    if class == b's' {
        return Effective::default();
    }
    let c = &view.0.cpu;
    let mut cpu = luna_cpu_65c816::Cpu::new();
    cpu.a = c.a;
    cpu.x = c.x;
    cpu.y = c.y;
    cpu.sp = c.sp;
    cpu.dp = c.dp;
    cpu.pc = c.pc;
    cpu.pb = c.pb;
    cpu.db = c.db;
    cpu.p = c.p;
    cpu.e = c.e;
    let mut bus = DummyBus {
        view,
        ops: Vec::with_capacity(16),
    };
    cpu.step(&mut bus);
    if unconditional_jump(op) || conditional_jump(op) {
        return if len == 3 {
            Effective {
                addr: Some((u32::from(cpu.pb) << 16) | u32::from(cpu.pc)),
                size: 0,
                show: true,
            }
        } else {
            Effective::default()
        };
    }
    let ops = &bus.ops;
    for i in (1..ops.len()).rev() {
        let (addr, kind) = ops[i];
        if kind == OpKind::Operand {
            continue;
        }
        let (prev, prev_kind) = ops[i - 1];
        let (addr, size) = if prev_kind == kind && prev == addr.wrapping_sub(1) {
            (prev, 2)
        } else if kind == OpKind::Write && prev_kind == kind && prev == addr.wrapping_add(1) {
            // A 16-bit read-modify-write writes its high byte first.
            (addr, 2)
        } else {
            (addr, 1)
        };
        return Effective {
            addr: Some(addr),
            size,
            show: class == b'e',
        };
    }
    Effective::default()
}

/// What a row is made of, taken before the instruction runs.
#[derive(Clone, Debug)]
struct Capture {
    pc: u32,
    a: u16,
    x: u16,
    y: u16,
    d: u16,
    db: u8,
    sp: u16,
    p: u8,
    bytes: Vec<u8>,
    disassembly: String,
    effective: Effective,
    /// The effective address's name, when labels are on.
    effective_name: Option<String>,
    /// The value there before the instruction.
    value: Option<Value>,
    /// Whether the condition held at the opcode fetch.
    holds: bool,
}

/// A memory value, read before the instruction, or — for `$4210-$4212`,
/// whose flags move with the beam — read at the row's moment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Value {
    Memory(u16),
    /// The register's offset (two for a 16-bit value) and the opcode, on
    /// the data bus once it has been fetched.
    Io {
        lo: u16,
        hi: Option<u16>,
        opcode: u8,
    },
}

impl Value {
    /// The value at `at`.
    fn resolve(self, snes: &Snes, at: &FetchPoint) -> u16 {
        match self {
            Self::Memory(v) => v,
            Self::Io { lo, hi, opcode } => {
                let byte = |o: u16| u16::from(snes.peek_io_at(o, opcode, at).unwrap_or(0));
                byte(lo) | hi.map_or(0, |h| byte(h) << 8)
            }
        }
    }
}

/// What the formatter needs besides the row.
struct Names<'a> {
    labels: &'a LabelStore,
    symbols: Option<&'a SymbolTable>,
}

impl Names<'_> {
    fn name(&self, view: View<'_>, addr: u32) -> Option<String> {
        bus_name(self.labels, self.symbols, view, ListingCpu::Snes, addr)
    }
}

/// One CPU's trace logger.
#[derive(Debug)]
pub(crate) struct Logger {
    parts: Vec<Part>,
    condition: Option<Expression>,
    use_labels: bool,
    indent_code: bool,
    rows: VecDeque<TraceRow>,
    /// The last instruction whose condition failed at its opcode fetch:
    /// logged at the first later access the condition holds for, until
    /// the next instruction.
    pending: Option<Capture>,
}

/// Every CPU's trace logger, and the file they write to.
#[derive(Debug, Default)]
pub(crate) struct TraceLogs {
    snes: Option<Logger>,
    file: Option<std::io::BufWriter<std::fs::File>>,
    next_row_id: u64,
}

impl TraceLogs {
    /// No CPU followed, no file.
    pub(crate) const fn new() -> Self {
        Self {
            snes: None,
            file: None,
            next_row_id: 0,
        }
    }

    /// Whether any CPU is followed.
    pub(crate) const fn active(&self) -> bool {
        self.snes.is_some()
    }

    /// Whether the 65C816's accesses are needed (a condition is set).
    fn wants_accesses(&self) -> bool {
        self.snes.as_ref().is_some_and(|l| l.condition.is_some())
    }

    /// Make sure the core records what the loggers need (a new machine
    /// after a load starts disarmed).
    pub(crate) fn arm(&self, snes: &mut Snes) {
        if self.active() && snes.opcode_fetch.is_none() {
            snes.arm_opcode_fetch(true);
        }
        if self.wants_accesses() && snes.step_accesses.is_none() {
            snes.arm_step_accesses(true);
        }
    }
}

/// What the run loop keeps from before a step to after it.
pub(crate) struct TracePre {
    /// The row, when the step will run an instruction.
    capture: Option<Capture>,
    /// The CPU was parked (`WAI` / `STP`).
    idle: bool,
    interrupts: u64,
}

/// The row of the instruction at the 65C816's program counter.
fn capture(logger: &Logger, names: &Names<'_>, view: View<'_>) -> Capture {
    let c = &view.0.cpu;
    let pc = (u32::from(c.pb) << 16) | u32::from(c.pc);
    let p = c.p.bits();
    let m8 = c.e || p & 0x20 != 0;
    let x8 = c.e || p & 0x10 != 0;
    let bank = pc & 0xFF_0000;
    let read = |a: u16| view.bus_peek(bank | u32::from(a), 0);
    let name = |a: u32| {
        if logger.use_labels {
            names.name(view, a)
        } else {
            None
        }
    };
    let ins = luna_cpu_65c816::disassemble_named(read, c.pb, c.pc, m8, x8, &name);
    let bytes: Vec<u8> = (0..u32::from(ins.length))
        .map(|i| view.bus_peek((pc + i) & 0xFF_FFFF, 0))
        .collect();
    let op = bytes.first().copied().unwrap_or(0);
    let effective = effective(view, op, ins.length);
    // Mesen2 reads the value once the opcode fetch has ended: the opcode
    // is then on the data bus (what a write-only register reads), and the
    // flags of `$4210-$4212` are those of that moment (see `Value::Io`).
    let now = view.0.point_now();
    let register = |a: u32| match view.absolute(a) {
        Some(l) if l.kind == crate::MemoryKind::SnesRegister && l.offset >= 0x4000 => {
            Some(l.offset as u16)
        }
        _ => None,
    };
    let byte = |a: u32| match register(a) {
        Some(o) => view.0.peek_io_at(o, op, &now).unwrap_or(0),
        None => view.bus_peek(a, 0),
    };
    let value = effective.addr.filter(|_| effective.size > 0).map(|a| {
        let next = (a + 1) & 0xFF_FFFF;
        let wide = effective.size == 2;
        let moving = |a: u32| register(a).filter(|o| (0x4210..=0x4212).contains(o));
        if let Some(lo) = moving(a) {
            Value::Io {
                lo,
                hi: wide.then(|| moving(next)).flatten(),
                opcode: op,
            }
        } else {
            let lo = u16::from(byte(a));
            Value::Memory(if wide {
                lo | (u16::from(byte(next)) << 8)
            } else {
                lo
            })
        }
    });
    let effective_name = effective.addr.filter(|_| effective.show).and_then(name);
    let holds = logger.condition.as_ref().is_none_or(|e| {
        let access = ExprAccess {
            value: i64::from(op),
            address: i64::from(pc),
            memaddress: view.absolute(pc).map_or(-1, |l| i64::from(l.offset)),
            write: false,
            dma: false,
            dummy: false,
            oppc: i64::from(pc),
        };
        evaluate_on(e, &access, view).holds()
    });
    Capture {
        pc,
        a: c.a,
        x: c.x,
        y: c.y,
        d: c.dp,
        db: c.db,
        sp: c.sp,
        p,
        bytes,
        disassembly: ins.text,
        effective,
        effective_name,
        value,
        holds,
    }
}

/// Lay a row out after `out` (Mesen2 aligns on the whole line, so a file
/// row's program counter counts).
fn format_row(
    out: &mut String,
    parts: &[Part],
    indent: bool,
    c: &Capture,
    at: &FetchPoint,
    value: Option<u16>,
) {
    const ACTIVE: &[u8; 8] = b"NVMXDIZC";
    const INACTIVE: &[u8; 8] = b"nvmxdizc";
    for part in parts {
        let (tag, width, hex) = match part {
            Part::Text(t) => {
                out.push_str(t);
                continue;
            }
            Part::Tag { tag, width, hex } => (*tag, *width, *hex),
        };
        match tag {
            Tag::A => write_int(out, Num::U16(c.a), width, hex),
            Tag::X => write_int(out, Num::U16(c.x), width, hex),
            Tag::Y => write_int(out, Num::U16(c.y), width, hex),
            Tag::D => write_int(out, Num::U16(c.d), width, hex),
            Tag::Db => write_int(out, Num::U8(c.db), width, hex),
            Tag::Sp => write_int(out, Num::U16(c.sp), width, hex),
            Tag::Ps => {
                if hex {
                    write_int(out, Num::U32(u32::from(c.p)), width, true);
                } else {
                    let flags: String = (0..8)
                        .filter_map(|i| {
                            if c.p & (0x80 >> i) != 0 {
                                Some(char::from(ACTIVE[i]))
                            } else if width >= 8 {
                                Some(char::from(INACTIVE[i]))
                            } else {
                                None
                            }
                        })
                        .collect();
                    write_str(out, &flags, width);
                }
            }
            Tag::ByteCode => {
                let s = c
                    .bytes
                    .iter()
                    .map(|b| {
                        if hex {
                            format!("${b:02X}")
                        } else {
                            format!("{b:02X}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                write_str(out, &s, width);
            }
            Tag::Disassembly => {
                let start = out.len();
                if indent {
                    let level = 0xFF - usize::from(c.sp as u8);
                    out.extend(std::iter::repeat_n(' ', level / 2));
                }
                out.push_str(&c.disassembly);
                // Mesen2 writes a space after the mnemonic, operand or not.
                if !c.disassembly.contains(' ') {
                    out.push(' ');
                }
                let len = out.len() - start;
                out.extend(std::iter::repeat_n(' ', width.saturating_sub(len)));
            }
            Tag::EffectiveAddress => {
                if let (Some(a), true) = (c.effective.addr, c.effective.show) {
                    let s = c
                        .effective_name
                        .as_ref()
                        .map_or_else(|| format!(" [${a:06X}]"), |n| format!(" [{n}]"));
                    write_str(out, &s, width);
                }
            }
            Tag::MemoryValue => {
                if let Some(v) = value {
                    if hex {
                        out.push_str("= $");
                        let n = if c.effective.size == 2 {
                            Num::U16(v)
                        } else {
                            Num::U8(v as u8)
                        };
                        write_int(out, n, width, true);
                    } else {
                        out.push_str("= ");
                        write_int(out, Num::U16(v), width, false);
                    }
                }
            }
            Tag::Align => {
                out.extend(std::iter::repeat_n(' ', width.saturating_sub(out.len())));
            }
            Tag::Pc => write_str(out, &Num::U32(c.pc).hex(), width),
            Tag::Cycle => write_int(out, Num::U32(u32::from(dot(at.hclock))), width, hex),
            Tag::HClock => write_int(out, Num::U32(u32::from(at.hclock)), width, hex),
            Tag::Scanline => write_int(out, Num::U32(u32::from(at.line)), width, hex),
            Tag::FrameCount => write_int(out, Num::U32(at.frame as u32), width, hex),
            Tag::CycleCount => write_int(out, Num::U64(at.cpu_cycles), width, hex),
        }
    }
}

/// The dot (Mesen2's `Cycle`) at master clock `hclock` of a line: four
/// clocks a dot, but dots 323 and 327 take six.
const fn dot(hclock: u16) -> u16 {
    if hclock <= 1292 {
        hclock >> 2
    } else if hclock <= 1310 {
        (hclock - 2) >> 2
    } else {
        (hclock - 4) >> 2
    }
}

impl TraceLogs {
    /// Before a step: what the row would be, if an instruction runs.
    pub(crate) fn pre(
        &self,
        labels: &LabelStore,
        symbols: Option<&SymbolTable>,
        snes: &Snes,
    ) -> Option<TracePre> {
        let logger = self.snes.as_ref()?;
        let c = &snes.cpu;
        let idle = c.stopped || c.waiting;
        let names = Names { labels, symbols };
        let capture = (!idle && !c.pending_nmi && !c.pending_irq)
            .then(|| capture(logger, &names, View(snes)));
        Some(TracePre {
            capture,
            idle,
            interrupts: c.interrupts_serviced,
        })
    }

    /// After a step: log its instruction's row, or a waiting row on one of
    /// the step's accesses.
    pub(crate) fn post(
        &mut self,
        labels: &LabelStore,
        symbols: Option<&SymbolTable>,
        snes: &mut Snes,
        pre: TracePre,
    ) {
        let accesses = snes
            .step_accesses
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let Some(logger) = self.snes.as_mut() else {
            return;
        };
        let c = &snes.cpu;
        let ran =
            c.interrupts_serviced == pre.interrupts && !(pre.idle && (c.waiting || c.stopped));
        let mut accesses: &[StepAccess] = &accesses;
        let mut logged: Option<(Capture, FetchPoint)> = None;
        if ran {
            // A parked CPU that woke without taking the interrupt ran an
            // instruction this very step: its row comes from now.
            let cap = pre.capture.unwrap_or_else(|| {
                let names = Names { labels, symbols };
                capture(logger, &names, View(snes))
            });
            let at = snes.opcode_fetch.flatten().unwrap_or(FetchPoint {
                line: 0,
                hclock: 0,
                frame: snes.frame_count,
                mclk: snes.total_mclk,
                cpu_cycles: snes.cpu_cycles,
                nmi_flag: false,
                irq_flag: false,
                hvbjoy: 0,
            });
            // The opcode fetch itself was the exec check.
            if let Some(i) = accesses.iter().position(|a| a.code) {
                accesses = &accesses[i + 1..];
            }
            if cap.holds {
                logger.pending = None;
                logged = Some((cap, at));
            } else {
                logger.pending = Some(cap);
            }
        }
        if logged.is_none()
            && let (Some(cond), Some(cap)) = (logger.condition.as_ref(), logger.pending.as_ref())
        {
            let view = View(snes);
            let hit = accesses.iter().find(|a| {
                let access = ExprAccess {
                    value: i64::from(a.value),
                    address: i64::from(a.addr),
                    memaddress: view.absolute(a.addr).map_or(-1, |l| i64::from(l.offset)),
                    write: a.write,
                    dma: false,
                    dummy: false,
                    oppc: i64::from(cap.pc),
                };
                evaluate_on(cond, &access, view).holds()
            });
            if let Some(at) = hit.map(|a| a.at) {
                logged = logger.pending.take().map(|cap| (cap, at));
            }
        }
        if let Some((cap, at)) = logged {
            self.log(TraceCpu::Snes, &cap, &at, snes);
        }
    }

    /// Add a row to its CPU's window and to the file.
    fn log(&mut self, cpu: TraceCpu, cap: &Capture, at: &FetchPoint, snes: &Snes) {
        let Some(logger) = (match cpu {
            TraceCpu::Snes => self.snes.as_mut(),
        }) else {
            return;
        };
        let value = cap.value.map(|v| v.resolve(snes, at));
        let mut text = String::with_capacity(128);
        format_row(&mut text, &logger.parts, logger.indent_code, cap, at, value);
        if let Some(file) = self.file.as_mut() {
            let mut line = String::with_capacity(136);
            write_int(&mut line, Num::U32(cap.pc), 6, true);
            line.push_str("  ");
            format_row(&mut line, &logger.parts, logger.indent_code, cap, at, value);
            line.push('\n');
            // A failed write closes the file rather than stop the run.
            if file.write_all(line.as_bytes()).is_err() {
                self.file = None;
            }
        }
        if logger.rows.len() == TRACE_ROWS {
            logger.rows.pop_front();
        }
        logger.rows.push_back(TraceRow {
            row_id: self.next_row_id,
            cpu,
            pc: cap.pc,
            byte_code: cap.bytes.clone(),
            text,
        });
        self.next_row_id += 1;
    }
}

impl Emulator {
    /// Start (or restart, with new options) the trace logger of `cpu`. An
    /// invalid condition or format width is refused; the window's rows are
    /// kept.
    pub fn trace_logger_start(
        &mut self,
        cpu: TraceCpu,
        options: &TraceOptions,
    ) -> Result<(), ApiError> {
        self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let condition = if options.condition.trim().is_empty() {
            None
        } else {
            let expr_cpu = match cpu {
                TraceCpu::Snes => ExprCpu::Snes,
            };
            Some(
                self.compile_expression(&options.condition, expr_cpu)
                    .ok_or_else(|| {
                        ApiError::BadArg(format!("invalid condition: {}", options.condition))
                    })?,
            )
        };
        let format = options
            .format
            .clone()
            .unwrap_or_else(|| auto_format(cpu, &TraceFormatOptions::default()));
        let rows = match cpu {
            TraceCpu::Snes => self.trace.snes.take().map(|l| l.rows).unwrap_or_default(),
        };
        let logger = Logger {
            parts: parse_format(cpu, &format),
            condition,
            use_labels: options.use_labels,
            indent_code: options.indent_code,
            rows,
            pending: None,
        };
        match cpu {
            TraceCpu::Snes => self.trace.snes = Some(logger),
        }
        if let Some(snes) = self.snes.as_mut() {
            snes.arm_opcode_fetch(false);
            snes.arm_step_accesses(false);
            self.trace.arm(snes);
        }
        Ok(())
    }

    /// Stop the trace logger of `cpu`; its rows are dropped.
    pub fn trace_logger_stop(&mut self, cpu: TraceCpu) {
        match cpu {
            TraceCpu::Snes => self.trace.snes = None,
        }
        if let Some(snes) = self.snes.as_mut()
            && !self.trace.active()
        {
            snes.arm_opcode_fetch(false);
            snes.arm_step_accesses(false);
        }
    }

    /// Whether the trace logger of `cpu` is on.
    #[must_use]
    pub const fn trace_logger_running(&self, cpu: TraceCpu) -> bool {
        match cpu {
            TraceCpu::Snes => self.trace.snes.is_some(),
        }
    }

    /// The last `count` rows logged, all CPUs together, oldest first.
    #[must_use]
    pub fn trace_logger_rows(&self, count: usize) -> Vec<TraceRow> {
        let mut rows: Vec<&TraceRow> = self.trace.snes.iter().flat_map(|l| l.rows.iter()).collect();
        rows.sort_by_key(|r| r.row_id);
        let skip = rows.len().saturating_sub(count);
        rows.into_iter().skip(skip).cloned().collect()
    }

    /// Empty every CPU's window.
    pub fn trace_logger_clear(&mut self) {
        if let Some(l) = self.trace.snes.as_mut() {
            l.rows.clear();
        }
    }

    /// Write every row logged from now on to `path` (created or
    /// truncated), as well as to the windows.
    pub fn trace_file_start(&mut self, path: &std::path::Path) -> Result<(), ApiError> {
        let file = std::fs::File::create(path)?;
        self.trace.file = Some(std::io::BufWriter::with_capacity(1 << 16, file));
        Ok(())
    }

    /// Close the trace file, writing what is still buffered.
    pub fn trace_file_stop(&mut self) -> Result<(), ApiError> {
        if let Some(mut file) = self.trace.file.take() {
            file.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_format_matches_mesen2_defaults() {
        assert_eq!(
            auto_format(TraceCpu::Snes, &TraceFormatOptions::default()),
            "[Disassembly][EffectiveAddress] [MemoryValue,h][Align,38] A:[A,4h] X:[X,4h] \
             Y:[Y,4h] S:[SP,4h] D:[D,4h] DB:[DB,2h] P:[P,8] V:[Scanline,3] H:[Cycle,3]"
        );
        let all = TraceFormatOptions {
            show_effective_addresses: false,
            show_memory_values: false,
            status_format: StatusFormat::Hexadecimal,
            show_byte_code: true,
            show_clock_counter: true,
            show_frame_counter: true,
            ..TraceFormatOptions::default()
        };
        assert_eq!(
            auto_format(TraceCpu::Snes, &all),
            "[Disassembly][Align,24] A:[A,4h] X:[X,4h] Y:[Y,4h] S:[SP,4h] D:[D,4h] \
             DB:[DB,2h] P:[P,h] V:[Scanline,3] H:[Cycle,3] Fr:[FrameCount] \
             Cycle:[CycleCount] BC:[ByteCode]"
        );
    }

    #[test]
    fn format_parsing_follows_mesen2() {
        let p = parse_format(TraceCpu::Snes, "A:[A,4h] [ Scanline , 3 ][P][Bogus,2] [x");
        assert_eq!(
            p,
            vec![
                Part::Text("A:".into()),
                Part::Tag {
                    tag: Tag::A,
                    width: 4,
                    hex: true
                },
                Part::Text(" ".into()),
                Part::Tag {
                    tag: Tag::Scanline,
                    width: 3,
                    hex: false
                },
                Part::Tag {
                    tag: Tag::Ps,
                    width: 0,
                    hex: false
                },
                Part::Text("[Invalid tag]".into()),
                Part::Text(" ".into()),
                // A `[` that never closes is dropped.
                Part::Text("x".into()),
            ]
        );
        // The flag is compared with a lower-case `h`.
        assert_eq!(
            parse_format(TraceCpu::Snes, "[A,4H]"),
            vec![Part::Tag {
                tag: Tag::A,
                width: 4,
                hex: false
            }]
        );
    }

    #[test]
    fn numbers_are_written_as_mesen2_does() {
        let mut s = String::new();
        write_int(&mut s, Num::U16(0x12), 0, true);
        s.push('|');
        write_int(&mut s, Num::U32(0x8000), 6, true);
        s.push('|');
        write_int(&mut s, Num::U32(5), 3, false);
        s.push('|');
        write_int(&mut s, Num::U32(0x7E_1234), 0, true);
        assert_eq!(s, "0012|008000|5  |7E1234");
        assert_eq!(dot(1292), 323);
        assert_eq!(dot(1296), 323);
        assert_eq!(dot(1300), 324);
        assert_eq!(dot(1360), 339);
    }

    fn capture_for(p: u8) -> Capture {
        Capture {
            pc: 0x00_8000,
            a: 0x1234,
            x: 0x0001,
            y: 0x00FF,
            d: 0,
            db: 0x7E,
            sp: 0x01F0,
            p,
            bytes: vec![0xAD, 0x00, 0x20],
            disassembly: "LDA $2000".into(),
            effective: Effective {
                addr: Some(0x7E_2000),
                size: 1,
                show: true,
            },
            effective_name: None,
            value: Some(Value::Memory(0x42)),
            holds: true,
        }
    }

    #[test]
    fn a_row_is_laid_out_like_mesen2s() {
        let parts = parse_format(
            TraceCpu::Snes,
            &auto_format(TraceCpu::Snes, &TraceFormatOptions::default()),
        );
        let at = FetchPoint {
            line: 12,
            hclock: 100,
            frame: 3,
            mclk: 0,
            cpu_cycles: 0,
            nmi_flag: false,
            irq_flag: false,
            hvbjoy: 0,
        };
        let mut s = String::new();
        format_row(&mut s, &parts, false, &capture_for(0x24), &at, Some(0x42));
        assert_eq!(
            s,
            "LDA $2000 [$7E2000] = $42              A:1234 X:0001 Y:00FF S:01F0 D:0000 DB:7E \
             P:nvMxdIzc V:12  H:25 "
        );
        // In the file, the program counter counts towards the alignment.
        let mut f = String::from("008000  ");
        format_row(&mut f, &parts, false, &capture_for(0x24), &at, Some(0x42));
        assert!(f.starts_with("008000  LDA $2000 [$7E2000] = $42      A:1234"));
        let mut c = String::new();
        format_row(
            &mut c,
            &parse_format(TraceCpu::Snes, "[P]|[P,h]|[ByteCode]|[ByteCode,h]"),
            false,
            &capture_for(0x24),
            &at,
            None,
        );
        assert_eq!(c, "MI|24|AD 00 20|$AD $00 $20");
    }
}
