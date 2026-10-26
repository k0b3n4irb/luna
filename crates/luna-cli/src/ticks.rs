//! `luna ticks`: a per-tick state record for differential runs against a
//! reference — the `tick-record` JSONL format, version 1.
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
use std::process::ExitCode;

use sha2::Digest;

/// The `version` this build writes.
const FORMAT_VERSION: u32 = 1;

/// Frames a record may run when the spec gives no `until_frame`: ten
/// minutes of NTSC, as `luna state --until-pc`.
const FRAME_CAP: u64 = 36_000;

// ---- The spec file ------------------------------------------------------

/// The `--spec` TOML file.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    /// Must be [`FORMAT_VERSION`].
    format: u32,
    tick: TickPoint,
    start: Option<StartPoint>,
    /// Stop after this many ticks.
    ticks: Option<u64>,
    /// Stop at this frame.
    until_frame: Option<u64>,
    input: Option<InputSpec>,
    #[serde(default)]
    poke: Vec<PokeSpec>,
    #[serde(default)]
    zone: Vec<ZoneSpec>,
    shots: Option<ShotsSpec>,
}

/// The routine whose arrivals are the ticks.
#[derive(Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TickPoint {
    /// A `.sym` label, `label+N` or `BANK:OFFSET`.
    symbol: String,
    /// Only arrivals where this condition holds are ticks.
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    cond: Option<String>,
}

/// Where tick 1 may start: the `hit`-th arrival on `symbol` where `if` holds.
#[derive(Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct StartPoint {
    /// A `.sym` label, `label+N` or `BANK:OFFSET`.
    symbol: String,
    /// Which arrival (1 = the first).
    #[serde(default = "one")]
    hit: u64,
    /// Only arrivals where this condition holds are counted.
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    cond: Option<String>,
}

const fn one() -> u64 {
    1
}

/// `input = "pad.txt"`, or a table naming the port and the clock.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum InputSpec {
    File(PathBuf),
    Table(InputTable),
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InputTable {
    file: PathBuf,
    #[serde(default = "port_one")]
    port: u8,
    /// The routine whose arrivals number the entries; `"frame"` numbers
    /// them by frame. Default: the tick's routine.
    at: Option<String>,
}

const fn port_one() -> u8 {
    1
}

/// Bytes written into WRAM at a tick, before its zones are read.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PokeSpec {
    /// Written at this tick, before its zones are read.
    tick: Option<u64>,
    /// Or at an arrival on a routine, counted from power-on.
    on: Option<PokeOn>,
    at: String,
    /// Hex bytes in memory order.
    bytes: String,
}

/// The `hit`-th arrival on `symbol` since power-on.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PokeOn {
    symbol: String,
    #[serde(default = "one")]
    hit: u64,
}

/// A named memory zone, read at every tick.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ZoneSpec {
    name: String,
    at: String,
    /// Bytes, in decimal.
    length: u32,
}

/// Pictures to write at some ticks.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShotsSpec {
    ticks: Vec<u64>,
    /// The 512-wide native capture instead of the 256-wide frame.
    #[serde(default)]
    native: bool,
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
    /// SHA-256 of the ROM file.
    sha256: String,
    /// The ROM's file name.
    file: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct HeaderTick {
    symbol: String,
    #[serde(rename = "if", skip_serializing_if = "Option::is_none")]
    cond: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<StartPoint>,
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
    file: String,
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
    /// FNV-1a 64 of the picture's pixels, as `luna state` hashes them.
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
    /// `limit` (the `ticks` asked for), `until_frame`, `frame_cap`,
    /// `halted` or `error`.
    reason: &'static str,
}

/// `luna ticks --schema`: the JSON Schema of each kind of line.
pub(crate) fn schema_json() -> String {
    let schema = serde_json::json!({
        "header": schemars::schema_for!(Header),
        "tick": schemars::schema_for!(TickLine),
        "end": schemars::schema_for!(EndLine),
    });
    serde_json::to_string_pretty(&schema).expect("schema serialises")
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
pub(crate) fn check_condition(text: &str) -> Result<Vec<String>, String> {
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

/// A condition checked, its names resolved, and compiled.
fn compile_condition(
    em: &luna_api::Emulator,
    what: &str,
    text: &str,
) -> Result<luna_api::Expression, String> {
    let names = check_condition(text).map_err(|e| format!("{what} `{text}`: {e}"))?;
    for name in names {
        em.lookup_symbol(&name)
            .map_err(|e| format!("{what} `{text}`: `{name}`: {e}"))?;
    }
    em.compile_expression(text, luna_api::ExprCpu::Snes)
        .ok_or_else(|| format!("{what} `{text}`: not a valid expression"))
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
fn resolve_zone(em: &mut luna_api::Emulator, z: &ZoneSpec) -> Result<Space, String> {
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
        let a = crate::test_cmd::resolve_key(em, &z.at).map_err(|e| format!("{}: {e}", what()))?;
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
fn read_zone(em: &mut luna_api::Emulator, space: Space, len: u32) -> Result<Vec<u8>, String> {
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

/// A poke, resolved: written at tick `tick`.
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
    /// At the `hit`-th arrival on `pc` since power-on (`seen` so far).
    Arrival { pc: u32, hit: u64, seen: u64 },
}

impl Poke {
    fn write(&mut self, em: &mut luna_api::Emulator) -> Result<(), String> {
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
fn resolve_poke(em: &luna_api::Emulator, p: &PokeSpec) -> Result<Poke, String> {
    let what = || format!("poke at `{}`", p.at);
    let when = match (&p.tick, &p.on) {
        (Some(0), None) => return Err(format!("{}: ticks count from 1", what())),
        (Some(t), None) => PokeWhen::Tick(*t),
        (None, Some(on)) if on.hit == 0 => {
            return Err(format!("{}: on.hit counts from 1", what()));
        }
        (None, Some(on)) => PokeWhen::Arrival {
            pc: crate::test_cmd::resolve_key(em, &on.symbol)
                .map_err(|e| format!("{}: on.symbol `{}`: {e}", what(), on.symbol))?,
            hit: on.hit,
            seen: 0,
        },
        _ => return Err(format!("{}: give either `tick` or `on`", what())),
    };
    let addr = crate::test_cmd::resolve_key(em, &p.at).map_err(|e| format!("{}: {e}", what()))?;
    let bytes =
        crate::parsers::parse_hex_bytes(&p.bytes).map_err(|e| format!("{}: {e}", what()))?;
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

/// `luna ticks` options.
pub(crate) struct TicksOptions<'a> {
    pub spec: &'a Path,
    pub out: &'a Path,
    pub shots_dir: Option<&'a Path>,
    pub sym: Option<&'a Path>,
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
}

/// The pictures asked for: the ticks, native or not, the folder.
type Shots = (Vec<u64>, bool, PathBuf);

/// What the watching run tracks.
struct Recorder {
    tick_pc: u32,
    tick_cond: Option<luna_api::Expression>,
    start: Option<(u32, u64, Option<luna_api::Expression>)>,
    start_matches: u64,
    started: bool,
    zones: Vec<(String, Space, u32)>,
    pokes: Vec<Poke>,
    shots: Option<Shots>,
    port: Option<u8>,
    last_pad: Option<u16>,
    limit: Option<u64>,
    t: u64,
    out: Box<dyn Write>,
    error: Option<String>,
}

impl Recorder {
    /// Execution reached `pc`. Returns `true` to end the run.
    fn on_pc(&mut self, em: &mut luna_api::Emulator, pc: u32) -> bool {
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
            && cond
                .as_ref()
                .is_none_or(|c| crate::state::arrival_holds(em, c, pc))
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
                .is_none_or(|c| crate::state::arrival_holds(em, c, pc))
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

    fn write_tick(&mut self, em: &mut luna_api::Emulator) -> Result<(), String> {
        let t = self.t;
        for p in &mut self.pokes {
            if matches!(p.when, PokeWhen::Tick(n) if n == t) {
                p.write(em)?;
            }
        }
        let mut z = Vec::with_capacity(self.zones.len());
        for (name, space, len) in &self.zones {
            let bytes = read_zone(em, *space, *len).map_err(|e| format!("zone `{name}`: {e}"))?;
            z.push((name.clone(), crate::fmt::hex_str(&bytes)));
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
        write_line(&mut self.out, &row)
    }
}

fn write_line(out: &mut dyn Write, value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
    writeln!(out, "{text}").map_err(|e| format!("writing the record: {e}"))
}

/// Master cycles per second of the loaded machine.
fn master_hz(em: &luna_api::Emulator) -> u64 {
    match em.region() {
        luna_api::Region::Pal => 21_281_370,
        _ => 21_477_272,
    }
}

/// `luna ticks`: exit 0 when the record is complete, 1 when the run fell
/// short of it, 2 when the spec is refused (nothing is run).
pub(crate) fn run_ticks(rom: &Path, o: &TicksOptions<'_>) -> ExitCode {
    match record(rom, o) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

/// Read and check the spec file.
fn read_spec(path: &Path) -> Result<Spec, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let spec: Spec = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if spec.format != FORMAT_VERSION {
        return Err(format!(
            "{}: format {} (this luna writes format {FORMAT_VERSION})",
            path.display(),
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
    Ok(spec)
}

/// The input script, and what the header says of it: numbered by arrivals
/// on a routine (the tick's by default), as `luna state --input-at`, or by
/// frame.
fn input_script(
    em: &luna_api::Emulator,
    spec: &Spec,
    spec_dir: &Path,
) -> Result<(luna_api::InputScript, Option<InputOut>), String> {
    let mut script = luna_api::InputScript::new();
    let Some(input) = &spec.input else {
        return Ok((script, None));
    };
    let (file, port, at) = match input {
        InputSpec::File(f) => (f.clone(), 1, None),
        InputSpec::Table(t) => (t.file.clone(), t.port, t.at.clone()),
    };
    if !(1..=5).contains(&port) {
        return Err(format!("input.port {port}: ports are 1 to 5"));
    }
    let path = spec_dir.join(&file);
    let body =
        std::fs::read_to_string(&path).map_err(|e| format!("input `{}`: {e}", path.display()))?;
    let at = at.unwrap_or_else(|| spec.tick.symbol.clone());
    let by_frame = at == "frame";
    crate::parsers::add_pad_events(&mut script, &body, port - 1, !by_frame)
        .map_err(|e| format!("input `{}`: {e}", path.display()))?;
    if !by_frame {
        let pc =
            crate::test_cmd::resolve_key(em, &at).map_err(|e| format!("input.at `{at}`: {e}"))?;
        script.set_clock_pc(pc);
    }
    let out = InputOut {
        file: file
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        port,
        at,
    };
    Ok((script, Some(out)))
}

/// Everything before the run returns `Err` (exit 2); the run itself
/// reports through the exit code it returns.
fn record(rom: &Path, o: &TicksOptions<'_>) -> Result<ExitCode, String> {
    let spec = read_spec(o.spec)?;
    let spec_dir = o.spec.parent().unwrap_or_else(|| Path::new("."));

    let rom_bytes = std::fs::read(rom).map_err(|e| format!("{}: {e}", rom.display()))?;
    let mut em = luna_api::Emulator::new();
    crate::rom::load_rom_into(
        &mut em,
        rom,
        o.force_mapper,
        o.force_region,
        None,
        o.power_on,
    )?;
    if let Some(sym) = o.sym {
        em.load_symbols(sym)
            .map_err(|e| format!("--sym {}: {e}", sym.display()))?;
    }

    let tick_pc = crate::test_cmd::resolve_key(&em, &spec.tick.symbol)
        .map_err(|e| format!("tick.symbol `{}`: {e}", spec.tick.symbol))?;
    let tick_cond = spec
        .tick
        .cond
        .as_deref()
        .map(|c| compile_condition(&em, "tick.if", c))
        .transpose()?;
    let start = spec
        .start
        .as_ref()
        .map(|s| -> Result<_, String> {
            let pc = crate::test_cmd::resolve_key(&em, &s.symbol)
                .map_err(|e| format!("start.symbol `{}`: {e}", s.symbol))?;
            let cond = s
                .cond
                .as_deref()
                .map(|c| compile_condition(&em, "start.if", c))
                .transpose()?;
            Ok((pc, s.hit, cond))
        })
        .transpose()?;
    let mut zones = Vec::with_capacity(spec.zone.len());
    for z in &spec.zone {
        zones.push((z.name.clone(), resolve_zone(&mut em, z)?, z.length));
    }
    let pokes = spec
        .poke
        .iter()
        .map(|p| resolve_poke(&em, p))
        .collect::<Result<Vec<_>, _>>()?;
    let (mut script, inputs) = input_script(&em, &spec, spec_dir)?;

    let shots = if let Some(s) = &spec.shots {
        let dir = match o.shots_dir {
            Some(d) => d.to_path_buf(),
            None if o.out == Path::new("-") => {
                return Err("[shots] with --out -: give --shots-dir".to_string());
            }
            None => o.out.with_extension("shots"),
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if s.native {
            em.set_native_capture(true).map_err(|e| e.to_string())?;
        }
        Some((s.ticks.clone(), s.native, dir))
    } else {
        // No picture is read: skip composing them, which is over half the
        // work of a run and changes nothing else.
        em.set_rendering(false).map_err(|e| e.to_string())?;
        None
    };

    let mut out: Box<dyn Write> = if o.out == Path::new("-") {
        Box::new(std::io::BufWriter::new(std::io::stdout()))
    } else {
        Box::new(std::io::BufWriter::new(
            std::fs::File::create(o.out).map_err(|e| format!("{}: {e}", o.out.display()))?,
        ))
    };
    let header = Header {
        format: "tick-record",
        version: FORMAT_VERSION,
        producer: Producer {
            name: "luna",
            version: luna_api::VERSION,
        },
        subject: Subject {
            machine: "snes",
            sha256: crate::fmt::hex_str(&sha2::Sha256::digest(&rom_bytes)),
            file: rom
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
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
            hz: master_hz(&em),
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
    write_line(&mut out, &header)?;

    // The start routine is watched first, so an arrival that is both the
    // start and a tick opens the record before it is read.
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
        port: inputs_port(&spec),
        last_pad: None,
        limit: spec.ticks,
        t: 0,
        out,
        error: None,
    };
    let frame_bound = spec
        .until_frame
        .unwrap_or_else(|| em.frame_count().unwrap_or(0) + FRAME_CAP);
    let run = em.run_script_watching(
        &mut script,
        luna_api::ScriptBound::Frame(frame_bound),
        &pcs,
        |em, ev| match ev {
            luna_api::WatchEvent::Pc(i) => rec.on_pc(em, pcs[i]),
            luna_api::WatchEvent::Frame => false,
        },
    );
    finish(&em, &spec, &mut rec, &run, frame_bound)
}

/// The port the input script drives, if any.
fn inputs_port(spec: &Spec) -> Option<u8> {
    spec.input.as_ref().map(|i| match i {
        InputSpec::File(_) => 1,
        InputSpec::Table(t) => t.port,
    })
}

/// Write the end line, say what fell short, and pick the exit code.
fn finish(
    em: &luna_api::Emulator,
    spec: &Spec,
    rec: &mut Recorder,
    run: &Result<bool, luna_api::ApiError>,
    frame_bound: u64,
) -> Result<ExitCode, String> {
    let (reason, mut failed) = match (run, &rec.error) {
        (_, Some(e)) => {
            eprintln!("error: {e}");
            ("error", true)
        }
        (Err(e), None) => {
            eprintln!("error: {e}");
            ("error", true)
        }
        (Ok(true), None) => ("limit", false),
        (Ok(false), None) => {
            if em.frame_count().unwrap_or(0) < frame_bound {
                ("halted", true)
            } else if spec.until_frame.is_some() {
                ("until_frame", spec.ticks.is_some())
            } else {
                ("frame_cap", true)
            }
        }
    };
    let t = rec.t;
    write_line(
        &mut rec.out,
        &EndLine {
            end: End { ticks: t, reason },
        },
    )?;
    rec.out
        .flush()
        .map_err(|e| format!("writing the record: {e}"))?;
    if !rec.started {
        eprintln!(
            "error: start `{}` was not reached",
            spec.start.as_ref().map_or("", |s| s.symbol.as_str())
        );
        failed = true;
    } else if t == 0 {
        eprintln!("error: tick `{}` was not reached", spec.tick.symbol);
        failed = true;
    } else if let Some(limit) = spec.ticks.filter(|&l| t < l) {
        eprintln!("error: {t} of the {limit} ticks asked for ({reason})");
        failed = true;
    }
    for p in rec.pokes.iter().filter(|p| !p.done) {
        let when = match p.when {
            PokeWhen::Tick(n) => format!("tick {n}"),
            PokeWhen::Arrival { hit, seen, .. } => {
                format!("arrival {hit} on its routine (reached {seen} time(s))")
            }
        };
        eprintln!("error: poke at `{}`: {when} not reached", p.at);
        failed = true;
    }
    eprintln!("{t} tick(s) recorded ({reason})");
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
mod tests {
    use super::check_condition;

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
                check_condition(ok).is_ok(),
                "{ok}: {:?}",
                check_condition(ok)
            );
        }
        assert_eq!(
            check_condition("[a] == 1 || {b+2} != [c]").unwrap(),
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
            assert!(check_condition(bad).is_err(), "`{bad}` was accepted");
        }
    }
}
