//! A per-tick state record for differential runs against a reference: the
//! `tick-record` JSONL format, version 1 ([`Emulator::record_ticks`]).
//!
//! A game advances by ticks, not by frames: a tick may span several frames,
//! and a frame boundary may fall in the middle of one. The record reads
//! named memory zones at each arrival on the routine that starts a tick,
//! so two runs — two builds, or a port and the program it reproduces on
//! another machine — line up tick by tick whatever their speed.
//!
//! The record is raw bytes in memory order, with the byte order in the
//! header: decoding belongs to whoever compares, once, for both sides.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::Digest;

use crate::{Emulator, Expression, InputScript, ScriptBound, WatchEvent};

/// The `version` this build writes.
pub const TICK_RECORD_VERSION: u32 = 1;

/// Frames a record may run when the spec gives no `until_frame`: ten
/// minutes of NTSC.
const FRAME_CAP: u64 = 36_000;

// ---- The spec -----------------------------------------------------------

/// What to record: the routine whose arrivals are the ticks, the zones to
/// read, and optionally a start, a limit, input, pokes and pictures. The
/// CLI reads it from TOML, the MCP tool from JSON; every key is checked.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickSpec {
    /// Must be [`TICK_RECORD_VERSION`].
    pub format: u32,
    /// The routine whose arrivals are the ticks.
    pub tick: TickPoint,
    /// Where tick 1 may start.
    #[serde(default)]
    pub start: Option<TickStart>,
    /// Stop after this many ticks.
    #[serde(default)]
    pub ticks: Option<u64>,
    /// Stop at this frame (default: ten minutes after the call).
    #[serde(default)]
    pub until_frame: Option<u64>,
    /// A joypad script.
    #[serde(default)]
    pub input: Option<TickInput>,
    /// Bytes written into WRAM at a tick, or at an arrival on a routine.
    #[serde(default)]
    pub poke: Vec<TickPoke>,
    /// The named zones read at every tick.
    #[serde(default)]
    pub zone: Vec<TickZone>,
    /// Pictures at some ticks.
    #[serde(default)]
    pub shots: Option<TickShots>,
}

/// The routine whose arrivals are the ticks.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickPoint {
    /// A `.sym` label, `label+N` or `BANK:OFFSET`.
    pub symbol: String,
    /// Only arrivals where this condition holds are ticks (see
    /// [`check_tick_condition`]).
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub cond: Option<String>,
}

/// Where tick 1 may start: the `hit`-th arrival on `symbol` where `if`
/// holds. An arrival that is both the start and a tick is tick 1.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickStart {
    /// A `.sym` label, `label+N` or `BANK:OFFSET`.
    pub symbol: String,
    /// Which arrival (1 = the first).
    #[serde(default = "one")]
    pub hit: u64,
    /// Only arrivals where this condition holds are counted.
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub cond: Option<String>,
}

const fn one() -> u64 {
    1
}

/// A joypad script: a file, or a table naming the port and the clock.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum TickInput {
    /// The script's file, for port 1, numbered by arrivals on the tick's
    /// routine.
    File(PathBuf),
    /// The full form.
    Table(TickInputTable),
}

/// The full form of [`TickInput`].
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickInputTable {
    /// The script's file (relative to the spec). Or `script`.
    #[serde(default)]
    pub file: Option<PathBuf>,
    /// The script itself. Or `file`.
    #[serde(default)]
    pub script: Option<String>,
    /// The port, 1 to 5.
    #[serde(default = "port_one")]
    pub port: u8,
    /// The routine whose arrivals (counted from the start of the run)
    /// number the entries; `"frame"` numbers them by frame. Default: the
    /// tick's routine.
    #[serde(default)]
    pub at: Option<String>,
}

const fn port_one() -> u8 {
    1
}

/// Bytes written into WRAM at a tick or at an arrival on a routine.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickPoke {
    /// Written at this tick, before its zones are read. Or `on`.
    #[serde(default)]
    pub tick: Option<u64>,
    /// Written at an arrival on a routine, counted from the start of the
    /// run, before it runs. Or `tick`.
    #[serde(default)]
    pub on: Option<TickPokeOn>,
    /// Where: a symbol, `symbol+N` or `BANK:OFFSET`, in WRAM.
    pub at: String,
    /// Hex bytes in memory order.
    pub bytes: String,
}

/// The `hit`-th arrival on `symbol`.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickPokeOn {
    /// A `.sym` label, `label+N` or `BANK:OFFSET`.
    pub symbol: String,
    /// Which arrival (1 = the first).
    #[serde(default = "one")]
    pub hit: u64,
}

/// A named memory zone, read at every tick.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickZone {
    /// The key a comparison pairs.
    pub name: String,
    /// A symbol, `symbol+N`, `BANK:OFFSET`, or `APU:`, `VRAM:` (word),
    /// `CGRAM:` (colour), `OAM:` followed by a hex offset.
    pub at: String,
    /// Bytes, in decimal.
    pub length: u32,
}

/// Pictures to write at some ticks.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TickShots {
    /// The ticks.
    pub ticks: Vec<u64>,
    /// The 512-wide native capture instead of the 256-wide frame.
    #[serde(default)]
    pub native: bool,
}

/// Where [`Emulator::record_ticks`] finds and puts files.
#[derive(Debug, Clone, Copy)]
pub struct TickRecordOptions<'a> {
    /// What an input `file` is relative to.
    pub base_dir: &'a Path,
    /// Where the `[shots]` pictures go. Required with `shots`.
    pub shots_dir: Option<&'a Path>,
}

/// How a record ended.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct TickOutcome {
    /// Ticks recorded.
    pub ticks: u64,
    /// The end line's reason: `limit` (the ticks asked for), `until_frame`,
    /// `frame_cap`, `halted` or `error`.
    pub reason: &'static str,
    /// What fell short; empty when the record is complete.
    pub failures: Vec<String>,
}

impl TickOutcome {
    /// Every tick asked for, every poke written, nothing failed.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.failures.is_empty()
    }
}

// ---- The record ---------------------------------------------------------

/// Line 1 of the record.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct Header {
    /// Always `"tick-record"`.
    format: &'static str,
    /// Raised by any change that breaks reading; an added field that a
    /// reader may ignore does not raise it.
    version: u32,
    producer: Producer,
    subject: Subject,
    /// The byte order of the machine the zones were read on.
    endian: &'static str,
    tick: HeaderTick,
    clock: Clock,
    zones: Vec<ZoneOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inputs: Option<InputOut>,
    /// Where the `screen` files of the tick lines are.
    #[serde(skip_serializing_if = "Option::is_none")]
    shots_dir: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct Producer {
    name: &'static str,
    version: &'static str,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct Subject {
    machine: &'static str,
    /// SHA-256 of the ROM file (absent for a ROM loaded from bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    /// The ROM's file name.
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct HeaderTick {
    symbol: String,
    #[serde(rename = "if", skip_serializing_if = "Option::is_none")]
    cond: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<TickStart>,
}

/// What the informative time fields of a tick line count.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct Clock {
    /// `frame` counts completed frames.
    frame: &'static str,
    /// `cycles` counts master cycles since power-on.
    cycles: &'static str,
    /// Master cycles per second.
    hz: u64,
    /// `beam` is `[scanline, master cycles into the line]`.
    beam: &'static str,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct ZoneOut {
    name: String,
    at: String,
    length: u32,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct InputOut {
    /// The script's file name (absent for an inline script).
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    port: u8,
    /// The routine whose arrivals number the entries, or `"frame"`.
    at: String,
}

/// One line per tick.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct TickLine {
    /// 1 for the first tick.
    t: u64,
    frame: u64,
    cycles: u64,
    beam: [u16; 2],
    /// Each zone's bytes, lowercase hex in memory order, in spec order.
    #[schemars(with = "BTreeMap<String, String>")]
    z: Zones,
    /// The controller masks that changed since the previous tick line
    /// (on the first, those that are not zero), by port.
    #[serde(rename = "in", skip_serializing_if = "Option::is_none")]
    input: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screen: Option<Screen>,
}

/// The zones of a tick line, kept in the spec's order.
struct Zones(Vec<(String, String)>);

impl serde::Serialize for Zones {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct Screen {
    /// FNV-1a 64 of the picture's pixels, as `frame_hash` computes it.
    fnv1a64: String,
    /// The PNG, in `shots_dir`.
    file: String,
}

/// The last line: a record without it was cut short.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct EndLine {
    end: End,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct End {
    ticks: u64,
    /// `limit`, `until_frame`, `frame_cap`, `halted` or `error`.
    reason: &'static str,
}

/// The JSON Schema of each kind of line of a record, under `header`,
/// `tick` and `end`.
#[must_use]
pub fn tick_record_schema() -> serde_json::Value {
    serde_json::json!({
        "header": schemars::schema_for!(Header),
        "tick": schemars::schema_for!(TickLine),
        "end": schemars::schema_for!(EndLine),
    })
}

// ---- The `if` subset ----------------------------------------------------

/// Check a condition against the portable subset every producer of the
/// format evaluates alike, and return the names it reads.
///
/// ```text
/// cond = conj { "||" conj } ;   conj = cmp { "&&" cmp } ;
/// cmp  = term [ relop term ] ;  relop = "==" | "!=" | "<" | "<=" | ">" | ">=" ;
/// term = read | number | "(" read "&" number ")" ;
/// read = "[" ref "]" | "{" ref "}" ;   ref = name [ "+" decimal ] ;
/// number = decimal | "$" hex | "0x" hex ;
/// ```
///
/// `[x]` reads a byte, `{x}` a word in the machine's order; values are
/// unsigned. Every text of the subset means the same in Mesen2's language,
/// which evaluates it here.
pub fn check_tick_condition(text: &str) -> Result<Vec<String>, String> {
    let s: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut p = CondParser {
        s: &s,
        i: 0,
        names: Vec::new(),
    };
    p.cond()?;
    if p.i != s.len() {
        return Err(p.err("an operator or the end"));
    }
    Ok(p.names)
}

struct CondParser<'a> {
    s: &'a [u8],
    i: usize,
    names: Vec<String>,
}

impl CondParser<'_> {
    fn err(&self, want: &str) -> String {
        let at = String::from_utf8_lossy(&self.s[self.i..]);
        if at.is_empty() {
            format!("expected {want} at the end")
        } else {
            format!("expected {want} at `{at}`")
        }
    }

    fn eat(&mut self, tok: &str) -> bool {
        if self.s[self.i..].starts_with(tok.as_bytes()) {
            self.i += tok.len();
            true
        } else {
            false
        }
    }

    fn cond(&mut self) -> Result<(), String> {
        self.conj()?;
        while self.eat("||") {
            self.conj()?;
        }
        Ok(())
    }

    fn conj(&mut self) -> Result<(), String> {
        self.cmp()?;
        while self.eat("&&") {
            self.cmp()?;
        }
        Ok(())
    }

    fn cmp(&mut self) -> Result<(), String> {
        self.term()?;
        // Two-character operators first: `<=` is not `<` then `=`.
        if ["==", "!=", "<=", ">=", "<", ">"]
            .iter()
            .any(|op| self.eat(op))
        {
            self.term()?;
        }
        Ok(())
    }

    fn term(&mut self) -> Result<(), String> {
        if self.eat("(") {
            if !self.read()? {
                return Err(self.err("`[name]` or `{name}` after `(`"));
            }
            if !self.eat("&") {
                return Err(self.err("`&` (a parenthesis holds `read & mask` only)"));
            }
            self.number()?;
            if !self.eat(")") {
                return Err(self.err("`)`"));
            }
            return Ok(());
        }
        if self.read()? {
            return Ok(());
        }
        self.number()
    }

    /// `[ref]` or `{ref}`; `false` when neither opens here.
    fn read(&mut self) -> Result<bool, String> {
        let close = if self.eat("[") {
            "]"
        } else if self.eat("{") {
            "}"
        } else {
            return Ok(false);
        };
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
        {
            self.i += 1;
        }
        let name = &self.s[start..self.i];
        if name.is_empty() || name[0].is_ascii_digit() || name[0] == b'.' {
            self.i = start;
            return Err(self.err("a name"));
        }
        self.names.push(String::from_utf8_lossy(name).into_owned());
        if self.eat("+") && self.digits(|c| c.is_ascii_digit()) == 0 {
            return Err(self.err("a decimal offset"));
        }
        if !self.eat(close) {
            return Err(self.err(&format!("`{close}`")));
        }
        Ok(true)
    }

    fn number(&mut self) -> Result<(), String> {
        let hex = self.eat("$") || self.eat("0x");
        let n = if hex {
            self.digits(|c| c.is_ascii_hexdigit())
        } else {
            self.digits(|c| c.is_ascii_digit())
        };
        if n == 0 {
            return Err(self.err("a number, `[name]` or `{name}`"));
        }
        Ok(())
    }

    fn digits(&mut self, ok: impl Fn(u8) -> bool) -> usize {
        let start = self.i;
        while self.s.get(self.i).is_some_and(|&c| ok(c)) {
            self.i += 1;
        }
        self.i - start
    }
}

// ---- Addresses ----------------------------------------------------------

/// The `N` of `symbol+N`: **decimal** unless prefixed `0x` or `$`, as an
/// offset reads in assembly (`results+16` is sixteen bytes in).
///
/// Deliberately not hex by default, even though `BANK:OFFSET` and the
/// `--peek` count are: an element offset is arithmetic on an address, and
/// `+16` meaning twenty-two bytes would be a trap no one would ever
/// suspect. The prefix is there for anyone who wants hex.
fn parse_key_offset(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (digits, radix) = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .or_else(|| s.strip_prefix('$'))
        .map_or((s, 10), |h| (h, 16));
    if digits.is_empty() {
        return Err("empty".to_string());
    }
    i64::from_str_radix(digits, radix).map_err(|e| e.to_string())
}

impl Emulator {
    /// Resolve an address key — a loaded symbol, `BANK:OFFSET` in hex, or
    /// either followed by `+N` / `-N` (decimal unless `0x` / `$`) — to a
    /// 24-bit address. The error says why, and lists the candidates of an
    /// ambiguous bare name.
    pub fn resolve_address(&self, key: &str) -> Result<u32, String> {
        // The key exactly as written first, so a symbol whose name happens
        // to contain `+` or `-` still resolves as itself.
        if let Some(a) = self.resolve_address_base(key) {
            return Ok(a);
        }
        // `symbol+N` / `symbol-N` / `BANK:OFFSET+N`: an array element named
        // relative to its array, so a variable that moves in RAM no longer
        // breaks everything that pointed into it.
        if let Some(i) = key.rfind(['+', '-'])
            && i > 0
        {
            let (base, rest) = key.split_at(i);
            let n =
                parse_key_offset(&rest[1..]).map_err(|e| format!("bad offset in `{key}`: {e}"))?;
            let base = base.trim();
            let a =
                self.resolve_address_base(base)
                    .ok_or_else(|| match self.lookup_symbol(base) {
                        Err(e @ crate::SymbolError::Ambiguous { .. }) => e.to_string(),
                        _ => format!("`{base}`: not a loaded symbol and not BANK:OFFSET"),
                    })?;
            let a = i64::from(a) + if rest.starts_with('-') { -n } else { n };
            return u32::try_from(a)
                .ok()
                .filter(|&a| a <= 0xFF_FFFF)
                .ok_or_else(|| format!("`{key}` lands outside the 24-bit address space"));
        }
        Err(match self.lookup_symbol(key) {
            Err(e @ crate::SymbolError::Ambiguous { .. }) => e.to_string(),
            _ => "not a loaded symbol and not BANK:OFFSET".to_string(),
        })
    }

    /// A key without an offset: a loaded symbol, or `BANK:OFFSET` in hex.
    fn resolve_address_base(&self, key: &str) -> Option<u32> {
        if let Some(a) = self.resolve_symbol(key) {
            return Some(a);
        }
        let (bank_s, off_s) = key.split_once(':')?;
        let bank = u8::from_str_radix(bank_s, 16).ok()?;
        let off = u16::from_str_radix(off_s, 16).ok()?;
        Some((u32::from(bank) << 16) | u32::from(off))
    }

    /// Whether `condition` holds at an arrival on `addr`, evaluated as an
    /// exec breakpoint's (`value` is the opcode, `address` the PC).
    #[must_use]
    pub fn arrival_holds(&self, condition: &Expression, addr: u32) -> bool {
        let at = self
            .cpu_state()
            .map_or(addr, |c| (u32::from(c.pb) << 16) | u32::from(c.pc));
        let opcode = self
            .read_memory(crate::MemoryKind::SnesMemory, at, 1)
            .map_or(0, |b| b[0]);
        let access = crate::ExprAccess {
            value: i64::from(opcode),
            address: i64::from(at),
            memaddress: self
                .absolute_address(at)
                .ok()
                .flatten()
                .map_or(-1, |l| i64::from(l.offset)),
            write: false,
            dma: false,
            dummy: false,
            oppc: i64::from(at),
        };
        self.evaluate_expression(condition, &access)
            .is_ok_and(crate::ExprResult::holds)
    }

    /// A condition checked against the portable subset, its names
    /// resolved, and compiled.
    fn compile_tick_condition(&self, what: &str, text: &str) -> Result<Expression, String> {
        let names = check_tick_condition(text).map_err(|e| format!("{what} `{text}`: {e}"))?;
        for name in names {
            self.lookup_symbol(&name)
                .map_err(|e| format!("{what} `{text}`: `{name}`: {e}"))?;
        }
        self.compile_expression(text, crate::ExprCpu::Snes)
            .ok_or_else(|| format!("{what} `{text}`: not a valid expression"))
    }
}

// ---- Zones and pokes ----------------------------------------------------

/// Where a zone lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Space {
    /// The 24-bit CPU bus.
    Cpu(u32),
    /// SPC700 RAM, by offset.
    Apu(u16),
    /// VRAM, by word address.
    Vram(u16),
    /// CGRAM, by colour index.
    Cgram(u16),
    /// OAM, by byte offset.
    Oam(u16),
}

/// A memory's `PREFIX:HEX` form, or `None` for a CPU-bus key.
fn prefixed(at: &str) -> Option<(&'static str, Result<u32, String>)> {
    let (prefix, rest) = at.split_once(':')?;
    let kind = ["APU", "VRAM", "CGRAM", "OAM"]
        .into_iter()
        .find(|k| k.eq_ignore_ascii_case(prefix.trim()))?;
    let digits = rest.trim();
    let digits = digits
        .strip_prefix('$')
        .or_else(|| digits.strip_prefix("0x"))
        .unwrap_or(digits);
    Some((
        kind,
        u32::from_str_radix(digits, 16).map_err(|e| format!("bad hex `{rest}`: {e}")),
    ))
}

/// Whether `offset` of `bank` is an I/O register: a read there is not a
/// memory read and may have effects.
const fn is_io(bank: u32, offset: u32) -> bool {
    (bank <= 0x3F || (bank >= 0x80 && bank <= 0xBF)) && offset >= 0x2000 && offset <= 0x5FFF
}

/// Resolve a zone and check its whole range is memory.
fn resolve_zone(em: &mut Emulator, z: &TickZone) -> Result<Space, String> {
    let what = || format!("zone `{}` at `{}`", z.name, z.at);
    let len = z.length;
    if len == 0 {
        return Err(format!("{}: length is 0", what()));
    }
    let fits = |start: u32, size: u32| -> Result<(), String> {
        if start + len > size {
            Err(format!(
                "{}: {len} bytes from ${start:X} run past the end (${size:X} bytes)",
                what()
            ))
        } else {
            Ok(())
        }
    };
    let space = if let Some((kind, n)) = prefixed(&z.at) {
        let n = n.map_err(|e| format!("{}: {e}", what()))?;
        let small = |limit: u32| {
            u16::try_from(n)
                .ok()
                .filter(|&v| u32::from(v) < limit)
                .ok_or_else(|| format!("{}: ${n:X} is out of range", what()))
        };
        match kind {
            "APU" => {
                let o = small(0x1_0000)?;
                fits(u32::from(o), 0x1_0000)?;
                Space::Apu(o)
            }
            "VRAM" => {
                let w = small(0x8000)?;
                fits(u32::from(w) * 2, 0x1_0000)?;
                Space::Vram(w)
            }
            "CGRAM" => {
                let c = small(0x100)?;
                fits(u32::from(c) * 2, 0x200)?;
                Space::Cgram(c)
            }
            _ => {
                let o = small(0x220)?;
                fits(u32::from(o), 0x220)?;
                Space::Oam(o)
            }
        }
    } else {
        let a = em
            .resolve_address(&z.at)
            .map_err(|e| format!("{}: {e}", what()))?;
        let (bank, off) = (a >> 16, a & 0xFFFF);
        fits(off, 0x1_0000)?;
        if len > 0xFFFF {
            return Err(format!("{}: at most 65535 bytes", what()));
        }
        if (off..off + len).any(|o| is_io(bank, o)) {
            return Err(format!(
                "{}: ${bank:02X}:{off:04X}+{len} reaches the I/O registers ($2000-$5FFF), \
                 which are not memory",
                what()
            ));
        }
        Space::Cpu(a)
    };
    // A first read now: nothing mapped behind the range is an error, not a
    // stream of `$FF` in the record.
    if let Space::Cpu(a) = space {
        let peek = em
            .peek_memory_checked((a >> 16) as u8, a as u16, len as u16)
            .map_err(|e| format!("{}: {e}", what()))?;
        if peek.unmapped > 0 {
            return Err(format!(
                "{}: {} of its {len} bytes are unmapped (open bus)",
                what(),
                peek.unmapped
            ));
        }
    }
    Ok(space)
}

/// Read a resolved zone.
fn read_zone(em: &mut Emulator, space: Space, len: u32) -> Result<Vec<u8>, String> {
    let n = len as usize;
    match space {
        Space::Cpu(a) => em
            .peek_memory_checked((a >> 16) as u8, a as u16, len as u16)
            .map(|p| p.bytes),
        Space::Apu(o) => em.peek_aram(o, len),
        Space::Vram(w) => em.peek_vram(w * 2, len),
        Space::Cgram(c) => em.peek_cgram().map(|colors| {
            colors[usize::from(c)..]
                .iter()
                .flat_map(|c| c.to_le_bytes())
                .take(n)
                .collect()
        }),
        Space::Oam(o) => em
            .peek_oam()
            .map(|oam| oam[usize::from(o)..usize::from(o) + n].to_vec()),
    }
    .map_err(|e| e.to_string())
}

/// Lowercase hex, two characters per byte.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Hex bytes (`7f`, `0x7f01`, spaces allowed) in memory order.
fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.split_whitespace().collect();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(&s);
    if s.is_empty() || !s.len().is_multiple_of(2) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("`{s}` is not an even run of hex digits"));
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("checked hex"))
        .collect())
}

/// A poke, resolved.
struct Poke {
    when: PokeWhen,
    at: String,
    addr: u32,
    bytes: Vec<u8>,
    done: bool,
}

/// When a poke is written.
enum PokeWhen {
    /// At this tick, before its zones are read.
    Tick(u64),
    /// At the `hit`-th arrival on `pc` (`seen` so far).
    Arrival { pc: u32, hit: u64, seen: u64 },
}

impl Poke {
    fn write(&mut self, em: &mut Emulator) -> Result<(), String> {
        let written = em
            .poke_memory((self.addr >> 16) as u8, self.addr as u16, &self.bytes)
            .map_err(|e| format!("poke at `{}`: {e}", self.at))?;
        if written != self.bytes.len() {
            return Err(format!(
                "poke at `{}`: {written} of {} bytes written",
                self.at,
                self.bytes.len()
            ));
        }
        self.done = true;
        Ok(())
    }
}

/// Resolve a poke and check it lands in WRAM, the only memory a debug
/// write reaches.
fn resolve_poke(em: &Emulator, p: &TickPoke) -> Result<Poke, String> {
    let what = || format!("poke at `{}`", p.at);
    let when = match (&p.tick, &p.on) {
        (Some(0), None) => return Err(format!("{}: ticks count from 1", what())),
        (Some(t), None) => PokeWhen::Tick(*t),
        (None, Some(on)) if on.hit == 0 => {
            return Err(format!("{}: on.hit counts from 1", what()));
        }
        (None, Some(on)) => PokeWhen::Arrival {
            pc: em
                .resolve_address(&on.symbol)
                .map_err(|e| format!("{}: on.symbol `{}`: {e}", what(), on.symbol))?,
            hit: on.hit,
            seen: 0,
        },
        _ => return Err(format!("{}: give either `tick` or `on`", what())),
    };
    let addr = em
        .resolve_address(&p.at)
        .map_err(|e| format!("{}: {e}", what()))?;
    let bytes = parse_hex(&p.bytes).map_err(|e| format!("{}: {e}", what()))?;
    let (bank, off) = (addr >> 16, addr & 0xFFFF);
    let end = off + bytes.len() as u32;
    let wram = match bank {
        0x7E | 0x7F => end <= 0x1_0000,
        _ if bank <= 0x3F || (0x80..=0xBF).contains(&bank) => end <= 0x2000,
        _ => false,
    };
    if !wram {
        return Err(format!(
            "{}: ${bank:02X}:{off:04X}+{} is not all WRAM",
            what(),
            bytes.len()
        ));
    }
    Ok(Poke {
        when,
        at: p.at.clone(),
        addr,
        bytes,
        done: false,
    })
}

// ---- The run ------------------------------------------------------------

/// The pictures asked for: the ticks, native or not, the folder.
type Shots = (Vec<u64>, bool, PathBuf);

/// What the watching run tracks.
struct Recorder<'w> {
    tick_pc: u32,
    tick_cond: Option<Expression>,
    start: Option<(u32, u64, Option<Expression>)>,
    start_matches: u64,
    started: bool,
    zones: Vec<(String, Space, u32)>,
    pokes: Vec<Poke>,
    shots: Option<Shots>,
    port: Option<u8>,
    last_pad: Option<u16>,
    limit: Option<u64>,
    t: u64,
    out: &'w mut dyn Write,
    error: Option<String>,
}

impl Recorder<'_> {
    /// Execution reached `pc`. Returns `true` to end the run.
    fn on_pc(&mut self, em: &mut Emulator, pc: u32) -> bool {
        // Writes first: a tick on the same routine reads them.
        for p in &mut self.pokes {
            if let PokeWhen::Arrival { pc: at, hit, seen } = &mut p.when
                && *at == pc
            {
                *seen += 1;
                if *seen == *hit
                    && let Err(e) = p.write(em)
                {
                    self.error = Some(e);
                    return true;
                }
            }
        }
        if !self.started
            && let Some((start_pc, hit, cond)) = &self.start
            && *start_pc == pc
            && cond.as_ref().is_none_or(|c| em.arrival_holds(c, pc))
        {
            self.start_matches += 1;
            self.started = self.start_matches == *hit;
        }
        // The arrival that starts the record may itself be tick 1.
        if !self.started
            || pc != self.tick_pc
            || !self
                .tick_cond
                .as_ref()
                .is_none_or(|c| em.arrival_holds(c, pc))
        {
            return false;
        }
        self.t += 1;
        if let Err(e) = self.write_tick(em) {
            self.error = Some(e);
            return true;
        }
        self.limit == Some(self.t)
    }

    fn write_tick(&mut self, em: &mut Emulator) -> Result<(), String> {
        let t = self.t;
        for p in &mut self.pokes {
            if matches!(p.when, PokeWhen::Tick(n) if n == t) {
                p.write(em)?;
            }
        }
        let mut z = Vec::with_capacity(self.zones.len());
        for (name, space, len) in &self.zones {
            let bytes = read_zone(em, *space, *len).map_err(|e| format!("zone `{name}`: {e}"))?;
            z.push((name.clone(), hex(&bytes)));
        }
        let input = self.port.and_then(|port| {
            let mask = em.joypad(port - 1).ok()?;
            let changed = self.last_pad.map_or(mask != 0, |last| last != mask);
            self.last_pad = Some(mask);
            changed.then(|| BTreeMap::from([(port.to_string(), format!("0x{mask:04x}"))]))
        });
        let screen = match &self.shots {
            Some((ticks, native, dir)) if ticks.contains(&t) => {
                let (png, hash) = if *native {
                    (em.render_frame_png_native(), em.frame_hash_native())
                } else {
                    (em.render_frame_png(false), em.frame_hash(false))
                };
                let file = format!("t{t:06}.png");
                let png = png.map_err(|e| format!("tick {t}: picture: {e}"))?;
                let path = dir.join(&file);
                std::fs::write(&path, png)
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
                Some(Screen {
                    fnv1a64: format!("{:016x}", hash.map_err(|e| e.to_string())?),
                    file,
                })
            }
            _ => None,
        };
        let row = TickLine {
            t,
            frame: em.frame_count().map_err(|e| e.to_string())?,
            cycles: em.master_clock().map_err(|e| e.to_string())?,
            beam: em.beam().map_err(|e| e.to_string())?.into(),
            z: Zones(z),
            input,
            screen,
        };
        write_line(self.out, &row)
    }
}

fn write_line(out: &mut dyn Write, value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
    writeln!(out, "{text}").map_err(|e| format!("writing the record: {e}"))
}

/// The spec's own checks, before anything is resolved.
fn check_spec(spec: &TickSpec) -> Result<(), String> {
    if spec.format != TICK_RECORD_VERSION {
        return Err(format!(
            "format {} (this luna writes format {TICK_RECORD_VERSION})",
            spec.format
        ));
    }
    if spec.ticks == Some(0) {
        return Err("ticks = 0: nothing to record".to_string());
    }
    if spec.start.as_ref().is_some_and(|s| s.hit == 0) {
        return Err("start.hit counts from 1".to_string());
    }
    let mut names: Vec<&str> = Vec::new();
    for z in &spec.zone {
        if z.name.is_empty() || names.contains(&z.name.as_str()) {
            return Err(format!("zone name `{}` is empty or given twice", z.name));
        }
        names.push(&z.name);
    }
    if spec.shots.as_ref().is_some_and(|s| s.ticks.contains(&0)) {
        return Err("shots.ticks count from 1".to_string());
    }
    Ok(())
}

impl Emulator {
    /// Record `spec` from the current state, as `tick-record` JSONL into
    /// `out`: a header, one line per tick, an end line.
    ///
    /// `Err` means the spec was refused — an unknown symbol, a zone on the
    /// I/O registers or on unmapped memory, a condition outside the portable
    /// subset ([`check_tick_condition`]), a poke outside WRAM — and nothing
    /// was run or written. Otherwise the record always ends with its end
    /// line, and the outcome says whether it is complete. Arrivals (for
    /// `start`, `on` pokes and the input) count from the call: load or
    /// reset the ROM first for a record from power-on.
    pub fn record_ticks(
        &mut self,
        spec: &TickSpec,
        options: &TickRecordOptions<'_>,
        out: &mut dyn Write,
    ) -> Result<TickOutcome, String> {
        check_spec(spec)?;
        if self.snes.is_none() {
            return Err("no ROM loaded".to_string());
        }
        let tick_pc = self
            .resolve_address(&spec.tick.symbol)
            .map_err(|e| format!("tick.symbol `{}`: {e}", spec.tick.symbol))?;
        let tick_cond = spec
            .tick
            .cond
            .as_deref()
            .map(|c| self.compile_tick_condition("tick.if", c))
            .transpose()?;
        let start = spec
            .start
            .as_ref()
            .map(|s| -> Result<_, String> {
                let pc = self
                    .resolve_address(&s.symbol)
                    .map_err(|e| format!("start.symbol `{}`: {e}", s.symbol))?;
                let cond = s
                    .cond
                    .as_deref()
                    .map(|c| self.compile_tick_condition("start.if", c))
                    .transpose()?;
                Ok((pc, s.hit, cond))
            })
            .transpose()?;
        let mut zones = Vec::with_capacity(spec.zone.len());
        for z in &spec.zone {
            zones.push((z.name.clone(), resolve_zone(self, z)?, z.length));
        }
        let pokes = spec
            .poke
            .iter()
            .map(|p| resolve_poke(self, p))
            .collect::<Result<Vec<_>, _>>()?;
        let (mut script, inputs) = self.tick_input(spec, options.base_dir)?;
        let shots = match &spec.shots {
            Some(s) => {
                let dir = options
                    .shots_dir
                    .ok_or("[shots] needs a folder for its pictures")?;
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                Some((s.ticks.clone(), s.native, dir.to_path_buf()))
            }
            None => None,
        };

        let (subject_sha256, subject_file) = match &self.rom_file {
            Some(path) => (
                std::fs::read(path)
                    .ok()
                    .map(|b| hex(&sha2::Sha256::digest(&b))),
                path.file_name().map(|n| n.to_string_lossy().into_owned()),
            ),
            None => (None, None),
        };
        let header = Header {
            format: "tick-record",
            version: TICK_RECORD_VERSION,
            producer: Producer {
                name: "luna",
                version: crate::VERSION,
            },
            subject: Subject {
                machine: "snes",
                sha256: subject_sha256,
                file: subject_file,
            },
            endian: "little",
            tick: HeaderTick {
                symbol: spec.tick.symbol.clone(),
                cond: spec.tick.cond.clone(),
                start: spec.start.clone(),
            },
            clock: Clock {
                frame: "frame_end",
                cycles: "snes_master",
                hz: match self.region() {
                    crate::Region::Pal => 21_281_370,
                    _ => 21_477_272,
                },
                beam: "line,mclk",
            },
            zones: spec
                .zone
                .iter()
                .map(|z| ZoneOut {
                    name: z.name.clone(),
                    at: z.at.clone(),
                    length: z.length,
                })
                .collect(),
            inputs,
            shots_dir: shots
                .as_ref()
                .map(|(_, _, d)| d.to_string_lossy().into_owned()),
        };
        write_line(out, &header)?;

        // Pictures are composed only when a tick asks for one: the rest of
        // the time that is over half the work of a run for nothing. The
        // setting comes back as it was.
        let snes = self.snes.as_mut().expect("checked above");
        let was_skipping = snes.ppu.skip_pixels;
        let was_native = !snes.ppu.native_framebuffer.is_empty();
        snes.ppu.skip_pixels = shots.is_none();
        if shots.as_ref().is_some_and(|s| s.1) {
            snes.ppu.set_native_capture(true);
        }

        // The start routine and the poke routines are watched first, so an
        // arrival shared with a tick writes and opens the record before it
        // is read.
        let mut pcs = Vec::new();
        if let Some((pc, _, _)) = start {
            pcs.push(pc);
        }
        for pc in std::iter::once(tick_pc).chain(pokes.iter().filter_map(|p| match p.when {
            PokeWhen::Arrival { pc, .. } => Some(pc),
            PokeWhen::Tick(_) => None,
        })) {
            if !pcs.contains(&pc) {
                pcs.push(pc);
            }
        }
        let mut rec = Recorder {
            tick_pc,
            tick_cond,
            start_matches: 0,
            started: start.is_none(),
            start,
            zones,
            pokes,
            shots,
            port: spec.input.as_ref().map(|i| match i {
                TickInput::File(_) => 1,
                TickInput::Table(t) => t.port,
            }),
            last_pad: None,
            limit: spec.ticks,
            t: 0,
            out,
            error: None,
        };
        let frame_bound = spec
            .until_frame
            .unwrap_or_else(|| self.frame_count().unwrap_or(0) + FRAME_CAP);
        let run = self.run_script_watching(
            &mut script,
            ScriptBound::Frame(frame_bound),
            &pcs,
            |em, ev| match ev {
                WatchEvent::Pc(i) => rec.on_pc(em, pcs[i]),
                WatchEvent::Frame => false,
            },
        );
        if let Some(snes) = self.snes.as_mut() {
            snes.ppu.skip_pixels = was_skipping;
            if !was_native {
                snes.ppu.set_native_capture(false);
            }
        }
        self.finish_record(spec, &mut rec, &run, frame_bound)
    }

    /// The input script, and what the header says of it.
    fn tick_input(
        &self,
        spec: &TickSpec,
        base_dir: &Path,
    ) -> Result<(InputScript, Option<InputOut>), String> {
        let mut script = InputScript::new();
        let Some(input) = &spec.input else {
            return Ok((script, None));
        };
        let (file, inline, port, at) = match input {
            TickInput::File(f) => (Some(f.clone()), None, 1, None),
            TickInput::Table(t) => (t.file.clone(), t.script.clone(), t.port, t.at.clone()),
        };
        if !(1..=5).contains(&port) {
            return Err(format!("input.port {port}: ports are 1 to 5"));
        }
        let body = match (&file, inline) {
            (Some(f), None) => {
                let path = base_dir.join(f);
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("input `{}`: {e}", path.display()))?
            }
            (None, Some(s)) => s,
            _ => return Err("input: give either `file` or `script`".to_string()),
        };
        script
            .add_pad(port - 1, &body)
            .map_err(|e| format!("input: {e}"))?;
        let at = at.unwrap_or_else(|| spec.tick.symbol.clone());
        if at == "frame" {
            if script.has_after() {
                return Err(
                    "input: an `N+K` entry counts frames after an arrival; it needs `at` \
                     to name a routine"
                        .to_string(),
                );
            }
        } else {
            let pc = self
                .resolve_address(&at)
                .map_err(|e| format!("input.at `{at}`: {e}"))?;
            script.set_clock_pc(pc);
        }
        let out = InputOut {
            file: file
                .as_ref()
                .and_then(|f| f.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            port,
            at,
        };
        Ok((script, Some(out)))
    }

    /// Write the end line and say what fell short.
    fn finish_record(
        &self,
        spec: &TickSpec,
        rec: &mut Recorder<'_>,
        run: &Result<bool, crate::ApiError>,
        frame_bound: u64,
    ) -> Result<TickOutcome, String> {
        let mut failures = Vec::new();
        let reason = match (run, &rec.error) {
            (_, Some(e)) => {
                failures.push(e.clone());
                "error"
            }
            (Err(e), None) => {
                failures.push(e.to_string());
                "error"
            }
            (Ok(true), None) => "limit",
            (Ok(false), None) => {
                if self.frame_count().unwrap_or(0) < frame_bound {
                    failures.push("the CPU halted".to_string());
                    "halted"
                } else if spec.until_frame.is_some() {
                    "until_frame"
                } else {
                    "frame_cap"
                }
            }
        };
        let t = rec.t;
        write_line(
            rec.out,
            &EndLine {
                end: End { ticks: t, reason },
            },
        )?;
        rec.out
            .flush()
            .map_err(|e| format!("writing the record: {e}"))?;
        if !rec.started {
            failures.push(format!(
                "start `{}` was not reached",
                spec.start.as_ref().map_or("", |s| s.symbol.as_str())
            ));
        } else if t == 0 {
            failures.push(format!("tick `{}` was not reached", spec.tick.symbol));
        } else if let Some(limit) = spec.ticks.filter(|&l| t < l) {
            failures.push(format!("{t} of the {limit} ticks asked for ({reason})"));
        } else if spec.ticks.is_none() && reason == "frame_cap" {
            failures.push(format!(
                "no `ticks` nor `until_frame`: stopped after {FRAME_CAP} frames"
            ));
        }
        for p in rec.pokes.iter().filter(|p| !p.done) {
            let when = match p.when {
                PokeWhen::Tick(n) => format!("tick {n}"),
                PokeWhen::Arrival { hit, seen, .. } => {
                    format!("arrival {hit} on its routine (reached {seen} time(s))")
                }
            };
            failures.push(format!("poke at `{}`: {when} not reached", p.at));
        }
        Ok(TickOutcome {
            ticks: t,
            reason,
            failures,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{check_tick_condition, parse_hex, parse_key_offset};

    #[test]
    fn a_key_offset_is_decimal_unless_prefixed() {
        assert_eq!(parse_key_offset("16"), Ok(16));
        assert_eq!(parse_key_offset("0x10"), Ok(16));
        assert_eq!(parse_key_offset("$10"), Ok(16));
        assert!(parse_key_offset("").is_err());
        assert!(parse_key_offset("0x").is_err());
        assert!(parse_key_offset("ten").is_err());
    }

    #[test]
    fn the_subset_takes_its_forms() {
        for ok in [
            "[mode] == 2",
            "{timer} >= $0100 && [paused] == 0",
            "([flags] & $80) != 0 || [state+3] == 7",
            "[a.b_c]",
            "{x} < 0x10",
            " [ mode ]  ==  2 ",
        ] {
            assert!(
                check_tick_condition(ok).is_ok(),
                "{ok}: {:?}",
                check_tick_condition(ok)
            );
        }
        assert_eq!(
            check_tick_condition("[a] == 1 || {b+2} != [c]").unwrap(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn the_subset_refuses_the_rest() {
        for bad in [
            "",
            "[mode] = 2",
            "[flags] & 4 == 4",
            "!([a])",
            "[a] + 1 == 2",
            "a == 1",
            "[1a]",
            "[a+$10]",
            "(1 & [a])",
            "#a == 1",
            "[a] == 2)",
            "[a",
        ] {
            assert!(check_tick_condition(bad).is_err(), "`{bad}` was accepted");
        }
    }

    #[test]
    fn poke_bytes_are_hex_in_memory_order() {
        assert_eq!(parse_hex("7f").unwrap(), [0x7F]);
        assert_eq!(parse_hex("0x7801 ff").unwrap(), [0x78, 0x01, 0xFF]);
        for bad in ["", "7", "zz", "0x"] {
            assert!(parse_hex(bad).is_err(), "`{bad}`");
        }
    }
}
