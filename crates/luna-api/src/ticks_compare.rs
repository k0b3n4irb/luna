//! Comparing two `tick-record` files ([`compare_tick_records`]): the first
//! tick where the zones of one differ from the other's.
//!
//! The records are paired by `t`, never by frame or cycles (which only say
//! where a tick fell), and their zones are compared as the raw bytes the
//! format stores. Nothing here needs an emulator: the two records may have
//! been written by luna or by any other program that writes the format.

use std::collections::BTreeSet;
use std::io::{BufRead, Lines, Write};

use crate::TICK_RECORD_VERSION;

/// What to compare.
#[derive(Debug, Clone, Default)]
pub struct CompareOptions {
    /// Compare only these zones (by name; each must be in both records).
    /// Empty: every zone, which both headers must then name alike.
    pub zones: Vec<String>,
}

/// The end line of one record.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct CompareEnd {
    /// `end.ticks`.
    pub ticks: u64,
    /// `end.reason`.
    pub reason: String,
}

/// What the comparison learned about one record.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct CompareSide {
    /// Tick lines read.
    pub ticks: u64,
    /// The end line; absent when the record was cut short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<CompareEnd>,
    /// The record has no end line.
    pub truncated: bool,
    /// The header's byte order. The bytes are compared as stored: records
    /// of different orders differ wherever a word does.
    pub endian: String,
}

/// A zone that differs at one tick.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct ZoneDiff {
    /// The tick.
    pub t: u64,
    /// The zone's name.
    pub zone: String,
    /// Offset of the first differing byte within the zone.
    pub offset: usize,
    /// That byte in the first record, two hex digits.
    pub a: String,
    /// That byte in the second record.
    pub b: String,
    /// How many bytes of the zone differ.
    pub differing_bytes: usize,
}

/// A tick one record has and the other lacks.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct UnpairedTick {
    /// The tick.
    pub t: u64,
    /// The record that has it: `a` or `b`.
    pub only_in: &'static str,
}

/// The first divergence, in tick order.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FirstDivergence {
    /// A zone's bytes differ.
    Bytes(ZoneDiff),
    /// One record has a tick the other lacks (it is shorter, or skips it).
    Length(UnpairedTick),
}

impl FirstDivergence {
    /// The tick.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        match self {
            Self::Bytes(d) => d.t,
            Self::Length(u) => u.t,
        }
    }
}

/// The verdict of [`compare_tick_records`].
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct TickComparison {
    /// No zone differs at a tick both have, and no tick is in one only.
    pub identical: bool,
    /// The zones compared, in the first record's order.
    pub zones: Vec<String>,
    /// Ticks both records have.
    pub ticks_compared: u64,
    /// Of those, the ticks where at least one zone differs.
    pub ticks_diverging: u64,
    /// Ticks in one record only.
    pub ticks_unpaired: u64,
    /// The first divergence, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first: Option<FirstDivergence>,
    /// The first record.
    pub a: CompareSide,
    /// The second record.
    pub b: CompareSide,
    /// Every differing zone of every tick, in tick order (the CSV's rows).
    #[serde(skip)]
    pub diffs: Vec<ZoneDiff>,
}

impl TickComparison {
    /// One line saying what was found.
    #[must_use]
    pub fn summary(&self) -> String {
        let note = |s: &CompareSide, n: &str| {
            if s.truncated {
                format!(" ({n} has no end line: cut short)")
            } else {
                String::new()
            }
        };
        let tail = format!("{}{}", note(&self.a, "A"), note(&self.b, "B"));
        match &self.first {
            None => format!(
                "identical: {} tick(s) compared over {} zone(s){tail}",
                self.ticks_compared,
                self.zones.len()
            ),
            Some(FirstDivergence::Bytes(d)) => format!(
                "first divergence at tick {}, zone `{}`, byte {}: A={} B={} ({} of {} tick(s) \
                 differ{}){tail}",
                d.t,
                d.zone,
                d.offset,
                d.a,
                d.b,
                self.ticks_diverging,
                self.ticks_compared,
                if self.ticks_unpaired > 0 {
                    format!("; {} tick(s) in one record only", self.ticks_unpaired)
                } else {
                    String::new()
                }
            ),
            Some(FirstDivergence::Length(u)) => format!(
                "first divergence at tick {}: only record {} has it ({}; {} tick(s) in one \
                 record only; {} of {} paired tick(s) differ){tail}",
                u.t,
                u.only_in.to_uppercase(),
                self.end_reasons(),
                self.ticks_unpaired,
                self.ticks_diverging,
                self.ticks_compared,
            ),
        }
    }

    fn end_reasons(&self) -> String {
        let one = |s: &CompareSide| {
            s.end
                .as_ref()
                .map_or_else(|| "no end line".to_string(), |e| e.reason.clone())
        };
        format!("A ended: {}, B ended: {}", one(&self.a), one(&self.b))
    }

    /// The differing zones as CSV, one row per zone per tick, in tick
    /// order: `t,zone,first_offset,a,b,differing_bytes`. `a` and `b` are the
    /// bytes at `first_offset`. When one record is shorter, a last row for
    /// the first tick it lacks: zone `-`, `a` / `b` = `present` / `absent`.
    pub fn write_csv(&self, out: &mut dyn Write) -> std::io::Result<()> {
        writeln!(out, "t,zone,first_offset,a,b,differing_bytes")?;
        let unpaired = match &self.first {
            Some(FirstDivergence::Length(u)) => Some(u),
            _ => None,
        };
        // A length divergence can come before later byte differences: keep
        // tick order, with the unpaired row among them.
        let mut pending = unpaired;
        for d in &self.diffs {
            if let Some(u) = pending.filter(|u| u.t <= d.t) {
                write_unpaired(out, u)?;
                pending = None;
            }
            writeln!(
                out,
                "{},{},{},{},{},{}",
                d.t,
                csv_field(&d.zone),
                d.offset,
                d.a,
                d.b,
                d.differing_bytes
            )?;
        }
        if let Some(u) = pending {
            write_unpaired(out, u)?;
        }
        Ok(())
    }
}

fn write_unpaired(out: &mut dyn Write, u: &UnpairedTick) -> std::io::Result<()> {
    let (a, b) = if u.only_in == "a" {
        ("present", "absent")
    } else {
        ("absent", "present")
    };
    writeln!(out, "{},-,,{a},{b},", u.t)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

// ---- Reading a record ---------------------------------------------------

/// One record being read line by line.
struct Rec<R: BufRead> {
    label: &'static str,
    lines: Lines<R>,
    line_no: usize,
    endian: String,
    /// `(name, length)` of the header's zones.
    zones: Vec<(String, usize)>,
    end: Option<CompareEnd>,
    done: bool,
    last_t: u64,
    ticks: u64,
}

type TickRow = (u64, Vec<String>);

fn bad(label: &str, line_no: usize, what: impl std::fmt::Display) -> String {
    format!("record {label}, line {line_no}: {what}")
}

impl<R: BufRead> Rec<R> {
    /// Read and check the header.
    fn open(label: &'static str, reader: R) -> Result<Self, String> {
        let mut lines = reader.lines();
        let first = lines
            .next()
            .ok_or_else(|| format!("record {label}: empty"))?
            .map_err(|e| format!("record {label}: {e}"))?;
        let h: serde_json::Value =
            serde_json::from_str(&first).map_err(|e| bad(label, 1, format!("not JSON: {e}")))?;
        if h["format"] != "tick-record" {
            return Err(bad(label, 1, "not a tick-record header (`format`)"));
        }
        if h["version"] != TICK_RECORD_VERSION {
            return Err(bad(
                label,
                1,
                format!(
                    "version {} (this luna reads version {TICK_RECORD_VERSION})",
                    h["version"]
                ),
            ));
        }
        let zones = h["zones"]
            .as_array()
            .ok_or_else(|| bad(label, 1, "no `zones` array"))?
            .iter()
            .map(|z| {
                let name = z["name"].as_str();
                let len = z["length"].as_u64().and_then(|l| usize::try_from(l).ok());
                name.zip(len)
                    .map(|(n, l)| (n.to_string(), l))
                    .ok_or_else(|| bad(label, 1, "a zone without `name` or `length`"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            label,
            lines,
            line_no: 1,
            endian: h["endian"].as_str().unwrap_or("").to_string(),
            zones,
            end: None,
            done: false,
            last_t: 0,
            ticks: 0,
        })
    }

    fn zone_len(&self, name: &str) -> Option<usize> {
        self.zones.iter().find(|z| z.0 == name).map(|z| z.1)
    }

    /// The next tick line with the hex of `zones`, or `None` at the end line
    /// or the end of the file.
    fn next_tick(&mut self, zones: &[String]) -> Result<Option<TickRow>, String> {
        while !self.done {
            let Some(line) = self.lines.next() else {
                self.done = true;
                break;
            };
            self.line_no += 1;
            let line = line.map_err(|e| bad(self.label, self.line_no, e))?;
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&line)
                .map_err(|e| bad(self.label, self.line_no, format!("not JSON: {e}")))?;
            if let Some(end) = v.get("end") {
                self.end = Some(CompareEnd {
                    ticks: end["ticks"].as_u64().unwrap_or(0),
                    reason: end["reason"].as_str().unwrap_or("").to_string(),
                });
                self.done = true;
                break;
            }
            let Some(t) = v["t"].as_u64() else {
                continue;
            };
            if t <= self.last_t {
                return Err(bad(
                    self.label,
                    self.line_no,
                    format!("tick {t} after tick {}: `t` must increase", self.last_t),
                ));
            }
            self.last_t = t;
            self.ticks += 1;
            let mut row = Vec::with_capacity(zones.len());
            for name in zones {
                let hex = v["z"][name.as_str()].as_str().ok_or_else(|| {
                    bad(
                        self.label,
                        self.line_no,
                        format!("tick {t}: no zone `{name}`"),
                    )
                })?;
                let want = self.zone_len(name).unwrap_or(0) * 2;
                if hex.len() != want || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return Err(bad(
                        self.label,
                        self.line_no,
                        format!("tick {t}: zone `{name}` is not {} hex byte(s)", want / 2),
                    ));
                }
                row.push(hex.to_ascii_lowercase());
            }
            return Ok(Some((t, row)));
        }
        Ok(None)
    }

    fn side(&self) -> CompareSide {
        CompareSide {
            ticks: self.ticks,
            end: self.end.clone(),
            truncated: self.end.is_none(),
            endian: self.endian.clone(),
        }
    }
}

/// Which zones to compare: the common ones, checked.
fn pick_zones<R: BufRead, S: BufRead>(
    a: &Rec<R>,
    b: &Rec<S>,
    opts: &CompareOptions,
) -> Result<Vec<String>, String> {
    let names = |r: &[(String, usize)]| r.iter().map(|z| z.0.clone()).collect::<BTreeSet<_>>();
    let zones: Vec<String> = if opts.zones.is_empty() {
        let (na, nb) = (names(&a.zones), names(&b.zones));
        if na != nb {
            let only = |x: &BTreeSet<String>, y: &BTreeSet<String>| {
                x.difference(y).cloned().collect::<Vec<_>>().join(", ")
            };
            return Err(format!(
                "the records name different zones (only in A: [{}]; only in B: [{}]): \
                 restrict the comparison with --zone",
                only(&na, &nb),
                only(&nb, &na)
            ));
        }
        a.zones.iter().map(|z| z.0.clone()).collect()
    } else {
        for z in &opts.zones {
            for r in [a.zone_len(z).map(|_| ()), b.zone_len(z).map(|_| ())] {
                r.ok_or_else(|| format!("zone `{z}` is not in both records"))?;
            }
        }
        opts.zones.clone()
    };
    for z in &zones {
        let (la, lb) = (a.zone_len(z), b.zone_len(z));
        if la != lb {
            return Err(format!(
                "zone `{z}` is {} byte(s) in A and {} in B",
                la.unwrap_or(0),
                lb.unwrap_or(0)
            ));
        }
    }
    Ok(zones)
}

/// The zones of one paired tick that differ.
fn diff_tick(
    t: u64,
    zones: &[String],
    a: &[String],
    b: &[String],
    out: &mut Vec<ZoneDiff>,
) -> bool {
    let before = out.len();
    for ((zone, ha), hb) in zones.iter().zip(a).zip(b) {
        if ha == hb {
            continue;
        }
        let (pa, pb) = (ha.as_bytes().chunks(2), hb.as_bytes().chunks(2));
        let mut first = None;
        let mut count = 0;
        for (i, (x, y)) in pa.zip(pb).enumerate() {
            if x != y {
                count += 1;
                first.get_or_insert((i, x, y));
            }
        }
        if let Some((offset, x, y)) = first {
            out.push(ZoneDiff {
                t,
                zone: zone.clone(),
                offset,
                a: String::from_utf8_lossy(x).into_owned(),
                b: String::from_utf8_lossy(y).into_owned(),
                differing_bytes: count,
            });
        }
    }
    out.len() > before
}

/// Compare two `tick-record` files and find the first tick where a zone
/// differs. `Err` means the records cannot be compared (unreadable, not a
/// version-1 `tick-record`, different zones, a malformed line).
///
/// Records are paired by `t`. A tick in one record only is a divergence of
/// kind `length`; a record without its end line is compared as far as it
/// goes and flagged `truncated`.
pub fn compare_tick_records(
    a: impl BufRead,
    b: impl BufRead,
    opts: &CompareOptions,
) -> Result<TickComparison, String> {
    let mut ra = Rec::open("A", a)?;
    let mut rb = Rec::open("B", b)?;
    let zones = pick_zones(&ra, &rb, opts)?;

    let mut diffs = Vec::new();
    let (mut compared, mut diverging, mut unpaired) = (0u64, 0u64, 0u64);
    let mut first: Option<FirstDivergence> = None;
    let (mut ca, mut cb): (Option<TickRow>, Option<TickRow>) = (None, None);
    loop {
        if ca.is_none() {
            ca = ra.next_tick(&zones)?;
        }
        if cb.is_none() {
            cb = rb.next_tick(&zones)?;
        }
        let only = |t: u64, in_a: bool, first: &mut Option<FirstDivergence>, n: &mut u64| {
            *n += 1;
            first.get_or_insert(FirstDivergence::Length(UnpairedTick {
                t,
                only_in: if in_a { "a" } else { "b" },
            }));
        };
        match (&ca, &cb) {
            (None, None) => break,
            (Some((t, _)), None) => {
                only(*t, true, &mut first, &mut unpaired);
                ca = None;
            }
            (None, Some((t, _))) => {
                only(*t, false, &mut first, &mut unpaired);
                cb = None;
            }
            (Some((ta, za)), Some((tb, zb))) => match ta.cmp(tb) {
                std::cmp::Ordering::Less => {
                    only(*ta, true, &mut first, &mut unpaired);
                    ca = None;
                }
                std::cmp::Ordering::Greater => {
                    only(*tb, false, &mut first, &mut unpaired);
                    cb = None;
                }
                std::cmp::Ordering::Equal => {
                    compared += 1;
                    let at = diffs.len();
                    if diff_tick(*ta, &zones, za, zb, &mut diffs) {
                        diverging += 1;
                        if first.is_none() {
                            first = Some(FirstDivergence::Bytes(diffs[at].clone()));
                        }
                    }
                    ca = None;
                    cb = None;
                }
            },
        }
    }
    Ok(TickComparison {
        identical: first.is_none(),
        zones,
        ticks_compared: compared,
        ticks_diverging: diverging,
        ticks_unpaired: unpaired,
        first,
        a: ra.side(),
        b: rb.side(),
        diffs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn rec(zones: &[(&str, usize)], ticks: &[(u64, &[&str])], end: Option<(u64, &str)>) -> String {
        let z: Vec<String> = zones
            .iter()
            .map(|(n, l)| format!(r#"{{"name":"{n}","at":"x","length":{l}}}"#))
            .collect();
        let mut s = format!(
            r#"{{"format":"tick-record","version":1,"endian":"little","zones":[{}]}}"#,
            z.join(",")
        );
        s.push('\n');
        for (t, vals) in ticks {
            let zz: Vec<String> = zones
                .iter()
                .zip(*vals)
                .map(|((n, _), v)| format!(r#""{n}":"{v}""#))
                .collect();
            let _ = writeln!(
                s,
                r#"{{"t":{t},"frame":0,"cycles":0,"beam":[0,0],"z":{{{}}}}}"#,
                zz.join(",")
            );
        }
        if let Some((n, r)) = end {
            let _ = writeln!(s, r#"{{"end":{{"ticks":{n},"reason":"{r}"}}}}"#);
        }
        s
    }

    fn cmp(a: &str, b: &str, zones: &[&str]) -> Result<TickComparison, String> {
        compare_tick_records(
            a.as_bytes(),
            b.as_bytes(),
            &CompareOptions {
                zones: zones.iter().map(ToString::to_string).collect(),
            },
        )
    }

    const Z: &[(&str, usize)] = &[("pos", 2), ("hp", 1)];

    #[test]
    fn identical_records() {
        let r = rec(
            Z,
            &[(1, &["0000", "10"]), (2, &["0100", "10"])],
            Some((2, "limit")),
        );
        let c = cmp(&r, &r, &[]).unwrap();
        assert!(c.identical, "{c:?}");
        assert_eq!((c.ticks_compared, c.ticks_diverging), (2, 0));
        assert!(c.summary().starts_with("identical"));
    }

    #[test]
    fn one_byte_difference_is_located() {
        let a = rec(
            Z,
            &[
                (1, &["0000", "10"]),
                (2, &["0100", "10"]),
                (3, &["0200", "10"]),
            ],
            Some((3, "limit")),
        );
        let b = rec(
            Z,
            &[
                (1, &["0000", "10"]),
                (2, &["0100", "10"]),
                (3, &["02ff", "0f"]),
            ],
            Some((3, "limit")),
        );
        let c = cmp(&a, &b, &[]).unwrap();
        assert!(!c.identical);
        let Some(FirstDivergence::Bytes(d)) = &c.first else {
            panic!("{c:?}")
        };
        assert_eq!((d.t, d.zone.as_str(), d.offset), (3, "pos", 1));
        assert_eq!((d.a.as_str(), d.b.as_str()), ("00", "ff"));
        assert_eq!((c.ticks_compared, c.ticks_diverging), (3, 1));
        assert_eq!(c.diffs.len(), 2);
        let mut csv = Vec::new();
        c.write_csv(&mut csv).unwrap();
        assert_eq!(
            String::from_utf8(csv).unwrap(),
            "t,zone,first_offset,a,b,differing_bytes\n3,pos,1,00,ff,1\n3,hp,0,10,0f,1\n"
        );
    }

    #[test]
    fn zones_are_paired_by_name_not_order() {
        let a = rec(
            &[("pos", 1), ("hp", 1)],
            &[(1, &["01", "02"])],
            Some((1, "limit")),
        );
        let b = rec(
            &[("hp", 1), ("pos", 1)],
            &[(1, &["02", "01"])],
            Some((1, "limit")),
        );
        assert!(cmp(&a, &b, &[]).unwrap().identical);
    }

    #[test]
    fn a_shorter_record_diverges_by_length() {
        let a = rec(
            Z,
            &[(1, &["0000", "10"]), (2, &["0100", "10"])],
            Some((2, "limit")),
        );
        let b = rec(Z, &[(1, &["0000", "10"])], Some((1, "halted")));
        let c = cmp(&a, &b, &[]).unwrap();
        assert!(!c.identical);
        let Some(FirstDivergence::Length(u)) = &c.first else {
            panic!("{c:?}")
        };
        assert_eq!((u.t, u.only_in), (2, "a"));
        assert_eq!((c.ticks_compared, c.ticks_unpaired), (1, 1));
        assert_eq!(c.b.end.as_ref().unwrap().reason, "halted");
        assert!(c.summary().contains("B ended: halted"), "{}", c.summary());
        let mut csv = Vec::new();
        c.write_csv(&mut csv).unwrap();
        assert!(
            String::from_utf8(csv)
                .unwrap()
                .ends_with("2,-,,present,absent,\n")
        );
    }

    #[test]
    fn zone_mismatch_is_refused_unless_restricted() {
        let a = rec(
            &[("pos", 2), ("hp", 1)],
            &[(1, &["0000", "10"])],
            Some((1, "limit")),
        );
        let b = rec(
            &[("pos", 2), ("mp", 1)],
            &[(1, &["0000", "99"])],
            Some((1, "limit")),
        );
        let e = cmp(&a, &b, &[]).unwrap_err();
        assert!(
            e.contains("different zones") && e.contains("hp") && e.contains("mp"),
            "{e}"
        );
        assert!(cmp(&a, &b, &["pos"]).unwrap().identical);
        assert!(cmp(&a, &b, &["hp"]).unwrap_err().contains("not in both"));
        let c = rec(&[("pos", 3)], &[(1, &["000000"])], Some((1, "limit")));
        assert!(
            cmp(&a, &c, &["pos"])
                .unwrap_err()
                .contains("2 byte(s) in A and 3 in B")
        );
    }

    #[test]
    fn a_truncated_record_is_compared_and_flagged() {
        let a = rec(Z, &[(1, &["0000", "10"]), (2, &["0100", "10"])], None);
        let b = rec(
            Z,
            &[(1, &["0000", "10"]), (2, &["0100", "10"])],
            Some((2, "limit")),
        );
        let c = cmp(&a, &b, &[]).unwrap();
        assert!(c.identical && c.a.truncated && !c.b.truncated, "{c:?}");
        assert!(c.summary().contains("A has no end line"));
    }

    #[test]
    fn unreadable_records_are_refused() {
        let ok = rec(Z, &[], Some((0, "limit")));
        assert!(cmp("", &ok, &[]).unwrap_err().contains("empty"));
        assert!(cmp("nope\n", &ok, &[]).is_err());
        let v2 = ok.replace("\"version\":1", "\"version\":2");
        assert!(cmp(&v2, &ok, &[]).unwrap_err().contains("version"));
        let other = ok.replace("tick-record", "other");
        assert!(cmp(&other, &ok, &[]).is_err());
        let backwards = rec(Z, &[(2, &["0000", "10"]), (1, &["0000", "10"])], None);
        assert!(
            cmp(&backwards, &ok, &[])
                .unwrap_err()
                .contains("must increase")
        );
    }
}
