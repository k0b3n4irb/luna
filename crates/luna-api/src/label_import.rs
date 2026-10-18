//! Labels from an assembler's symbol files, read as Mesen2 reads them for
//! a SNES cartridge: ca65 / cc65 `.dbg` debug info and bass `.sym`.
//!
//! - **`.dbg`**: a symbol becomes a label when it lies in a known segment,
//!   has a valid name, and is a `lab` (or has a `size`). A segment with an
//!   output offset (`ooffs`) is ROM — the label sits at `val - start +
//!   ooffs` in the program ROM — unless it is `rw`, which Mesen2 takes for
//!   SPC700 code (`SpcRam` at `val`); a segment without one is RAM, `val` a
//!   CPU address placed through the memory map, kept only in work RAM, save
//!   RAM or ROM. Names stay as they are (no scope prefix); a name met again
//!   becomes `name_2`, `name_3`. The length follows the symbol's size
//!   (1 for code and for a scope's own symbol). The comments come from the
//!   source files the `.dbg` names: the text after `;` on the line and the
//!   comment lines just above it (`//` lines and the whole line for C).
//! - **bass `.sym`**: `AAAAAAAA name` lines, the address a CPU address
//!   placed through the memory map (registers included), every character
//!   a label cannot hold turned into `_`.
//!
//! luna differs on one point: a `.dbg` whose segments do not parse is read
//! for what it has, where Mesen2 stops with an error.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::labels::{Label, MlbImport, valid_name};
use crate::memory::{MemoryKind, MemoryLocation, View};
use crate::{ApiError, Emulator};

/// One `key=value` list of a `.dbg` record.
fn attributes(rest: &str) -> BTreeMap<&str, &str> {
    let mut out = BTreeMap::new();
    let mut s = rest;
    while !s.is_empty() {
        s = s.trim_start_matches([',', ' ', '\t']);
        let Some(eq) = s.find('=') else { break };
        let key = &s[..eq];
        let after = &s[eq + 1..];
        let (value, next) = if let Some(q) = after.strip_prefix('"') {
            let end = q.find('"').unwrap_or(q.len());
            (&q[..end], q.get(end + 1..).unwrap_or(""))
        } else {
            let end = after.find([',', ' ', '\t']).unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        out.insert(key, value);
        s = next;
    }
    out
}

fn hex(v: &str) -> Option<u32> {
    let h = v
        .strip_prefix("0x")
        .or_else(|| v.strip_prefix("0X"))
        .unwrap_or(v);
    u32::from_str_radix(h, 16).ok()
}

/// A `.dbg` segment.
struct Seg {
    start: u32,
    ooffs: Option<u32>,
    rw: bool,
}

/// A `.dbg` span.
struct Span {
    seg: u32,
    start: u32,
    data: bool,
}

/// A `.dbg` line record.
struct Line {
    file: u32,
    number: u32,
    spans: Vec<u32>,
}

/// What a `.dbg` holds that the labels need.
#[derive(Default)]
struct Dbg<'a> {
    segs: BTreeMap<u32, Seg>,
    spans: BTreeMap<u32, Span>,
    lines: BTreeMap<u32, Line>,
    files: BTreeMap<u32, &'a str>,
    scope_syms: BTreeSet<u32>,
    syms: Vec<BTreeMap<&'a str, &'a str>>,
    errors: usize,
}

fn parse_dbg(text: &str) -> Dbg<'_> {
    let mut d = Dbg::default();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.len() < 3 {
            d.errors += 1;
            continue;
        }
        let (kind, rest) = line.split_at(line.find([' ', '\t']).unwrap_or(line.len()));
        let a = attributes(rest);
        let id = a.get("id").and_then(|v| v.parse::<u32>().ok());
        let num = |k: &str| a.get(k).and_then(|v| v.parse::<u32>().ok());
        let ok = match kind {
            "seg" => id
                .zip(a.get("start").and_then(|v| hex(v)))
                .map(|(id, start)| {
                    d.segs.insert(
                        id,
                        Seg {
                            start,
                            ooffs: num("ooffs"),
                            rw: a.get("type") == Some(&"rw"),
                        },
                    );
                })
                .is_some(),
            "span" => id
                .zip(num("seg"))
                .map(|(id, seg)| {
                    d.spans.insert(
                        id,
                        Span {
                            seg,
                            start: num("start").unwrap_or(0),
                            data: a.contains_key("type"),
                        },
                    );
                })
                .is_some(),
            "line" => id
                .zip(num("file"))
                .map(|(id, file)| {
                    let spans = a
                        .get("span")
                        .map(|s| s.split('+').filter_map(|x| x.parse().ok()).collect())
                        .unwrap_or_default();
                    d.lines.insert(
                        id,
                        Line {
                            file,
                            number: num("line").unwrap_or(0),
                            spans,
                        },
                    );
                })
                .is_some(),
            "file" => id
                .zip(a.get("name").copied())
                .map(|(id, name)| {
                    d.files.insert(id, name);
                })
                .is_some(),
            "scope" => {
                if let Some(s) = num("sym") {
                    d.scope_syms.insert(s);
                }
                id.is_some()
            }
            "sym" => {
                d.syms.push(a);
                true
            }
            _ => true,
        };
        if !ok {
            d.errors += 1;
        }
    }
    d
}

impl Dbg<'_> {
    /// Where `val` (or a span start) of segment `seg` lives.
    fn place(view: View<'_>, seg: &Seg, val: u32) -> Option<MemoryLocation> {
        let loc = match (seg.ooffs, seg.rw) {
            (Some(o), false) => MemoryLocation {
                kind: MemoryKind::SnesPrgRom,
                offset: val.checked_sub(seg.start)? + o,
            },
            (Some(_), true) => MemoryLocation {
                kind: MemoryKind::SpcRam,
                offset: val & 0xFFFF,
            },
            (None, _) => view.absolute(val & 0xFF_FFFF)?,
        };
        matches!(
            loc.kind,
            MemoryKind::SnesPrgRom
                | MemoryKind::SnesWorkRam
                | MemoryKind::SnesSaveRam
                | MemoryKind::SpcRam
        )
        .then_some(loc)
    }

    /// The labels, in file order, and the names they claimed.
    fn labels(&self, view: View<'_>) -> Vec<Label> {
        let mut used: BTreeMap<String, u32> = BTreeMap::new();
        let mut out = Vec::new();
        for (i, a) in self.syms.iter().enumerate() {
            let Some(seg) = a
                .get("seg")
                .and_then(|s| s.parse::<u32>().ok())
                .and_then(|s| self.segs.get(&s))
            else {
                continue;
            };
            let Some(&raw) = a.get("name").filter(|n| valid_name(n)) else {
                continue;
            };
            let n = used.entry(raw.to_string()).or_insert(0);
            *n += 1;
            let name = if *n == 1 {
                raw.to_string()
            } else {
                format!("{raw}_{n}")
            };
            let lab = a.get("type") == Some(&"lab");
            let size = a.get("size").and_then(|s| s.parse::<u32>().ok());
            if !lab && size.is_none() {
                continue;
            }
            let Some(val) = a
                .get("val")
                .and_then(|v| hex(v))
                .filter(|&v| v < 0x8000_0000)
            else {
                continue;
            };
            let Some(loc) = Self::place(view, seg, val) else {
                continue;
            };
            let id = a
                .get("id")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(i as u32);
            let length = if self.scope_syms.contains(&id) {
                1
            } else if seg.ooffs.is_none() || seg.rw {
                size.unwrap_or(1)
            } else {
                // ROM: data spans keep the size, code is one byte.
                let span = a
                    .get("def")
                    .and_then(|d| d.split('+').next())
                    .and_then(|d| d.parse::<u32>().ok())
                    .and_then(|l| self.lines.get(&l))
                    .and_then(|l| l.spans.first())
                    .and_then(|s| self.spans.get(s));
                match span {
                    Some(s) if !s.data => 1,
                    _ => size.unwrap_or(1),
                }
            };
            out.push(Label {
                memory: loc.kind,
                address: loc.offset,
                length: length.clamp(1, 0x1_0000),
                name,
                comment: String::new(),
            });
        }
        out
    }

    /// The comments the source files hold, by where their line's code is.
    fn comments(&self, view: View<'_>, dir: &Path) -> Vec<(MemoryLocation, String)> {
        let mut sources: BTreeMap<u32, Option<Vec<String>>> = BTreeMap::new();
        let mut out = Vec::new();
        for l in self.lines.values() {
            let Some(span) = l.spans.first().and_then(|s| self.spans.get(s)) else {
                continue;
            };
            let Some(seg) = self.segs.get(&span.seg) else {
                continue;
            };
            let text = sources.entry(l.file).or_insert_with(|| {
                let name = self.files.get(&l.file)?;
                find_source(dir, name)
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .map(|t| t.lines().map(|s| s.replace('\t', " ")).collect())
            });
            let Some(text) = text.as_ref() else { continue };
            let is_c = self
                .files
                .get(&l.file)
                .and_then(|n| Path::new(n).extension())
                .is_some_and(|e| e.eq_ignore_ascii_case("c") || e.eq_ignore_ascii_case("h"));
            let Some(idx) = (l.number as usize).checked_sub(1) else {
                continue;
            };
            let Some(here) = text.get(idx) else { continue };
            let mut parts = Vec::new();
            let mut j = idx;
            while j > 0 {
                let above = text[j - 1].trim();
                let c = if is_c {
                    above.strip_prefix("//")
                } else {
                    above.strip_prefix(';')
                };
                match c {
                    Some(c) => parts.push(c.trim().to_string()),
                    None => break,
                }
                j -= 1;
            }
            parts.reverse();
            if is_c {
                parts.push(here.trim().to_string());
            } else if let Some((_, c)) = here.split_once(';') {
                parts.push(c.trim().to_string());
            }
            parts.retain(|p| !p.is_empty());
            if parts.is_empty() {
                continue;
            }
            let at = match seg.ooffs {
                Some(o) if !seg.rw => Some(MemoryLocation {
                    kind: MemoryKind::SnesPrgRom,
                    offset: o + span.start,
                }),
                _ => Self::place(view, seg, seg.start + span.start),
            };
            if let Some(at) = at {
                out.push((at, parts.join("\n")));
            }
        }
        out
    }
}

/// A source file a `.dbg` names: next to it, else in each folder above.
fn find_source(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut d = Some(dir);
    while let Some(here) = d {
        let p = here.join(name);
        if p.is_file() {
            return Some(p);
        }
        d = here.parent();
    }
    None
}

/// Whether a `.sym` is WLA-DX's (else bass's).
#[must_use]
pub fn is_wla_sym(text: &str) -> bool {
    text.contains("[labels]") || text.contains("this file was created with wlalink")
}

/// One bass `.sym` line: the CPU address and the name, or `None` for a
/// line that does not parse.
fn bass_line(line: &str) -> Option<(u32, String)> {
    let (addr, name) = line.trim().split_once(' ')?;
    if addr.is_empty() || addr.len() > 8 {
        return None;
    }
    let addr = u32::from_str_radix(addr, 16).ok()?;
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '@' {
                c
            } else {
                '_'
            }
        })
        .collect();
    valid_name(&name).then_some((addr, name))
}

impl Emulator {
    /// Import a ca65 / cc65 `.dbg` (see the module doc): the labels go back
    /// to the defaults first, then the file's labels, then the comments of
    /// the source files found from `dir` (the `.dbg`'s folder).
    pub fn import_labels_dbg(&mut self, text: &str, dir: &Path) -> Result<MlbImport, ApiError> {
        let d = parse_dbg(text);
        let view = self.view()?;
        let labels = d.labels(view);
        let comments = d.comments(view, dir);
        self.reset_labels();
        let mut r = MlbImport {
            errors: d.errors,
            ..MlbImport::default()
        };
        for l in labels {
            if self.view()?.size(l.memory) == 0 {
                r.skipped += 1;
            } else if self.set_label(l).is_ok() {
                r.imported += 1;
            }
        }
        for (at, text) in comments {
            let mut l = self
                .labels
                .at(at.kind, at.offset)
                .filter(|(l, byte)| *byte == 0 && l.address == at.offset)
                .map_or_else(
                    || Label {
                        memory: at.kind,
                        address: at.offset,
                        length: 1,
                        name: String::new(),
                        comment: String::new(),
                    },
                    |(l, _)| l.clone(),
                );
            l.comment = text;
            let _ = self.set_label(l);
        }
        Ok(r)
    }

    /// Import a bass `.sym` (see the module doc), merged into the labels
    /// there.
    pub fn import_labels_bass(&mut self, text: &str) -> Result<MlbImport, ApiError> {
        let view = self.view()?;
        let mut r = MlbImport::default();
        let mut found = Vec::new();
        for line in text.lines() {
            match bass_line(line) {
                None => r.errors += 1,
                Some((addr, name)) => match view.absolute(addr) {
                    Some(loc) => found.push(Label {
                        memory: loc.kind,
                        address: loc.offset,
                        length: 1,
                        name,
                        comment: String::new(),
                    }),
                    None => r.skipped += 1,
                },
            }
        }
        for l in found {
            if self.set_label(l).is_ok() {
                r.imported += 1;
            }
        }
        Ok(r)
    }

    /// Import a label file by its extension: `.mlb` (Mesen2's), `.dbg`
    /// (ca65), `.sym` — WLA-DX's into the symbol table, else bass's into the
    /// labels. `reset` applies to `.mlb` (a `.dbg` always starts from the
    /// defaults, a bass `.sym` always merges, as in Mesen2).
    pub fn load_label_file(&mut self, path: &Path, reset: bool) -> Result<MlbImport, ApiError> {
        let text = std::fs::read_to_string(path)?;
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "dbg" => {
                let dir = path.parent().unwrap_or_else(|| Path::new("."));
                self.import_labels_dbg(&text, dir)
            }
            "sym" if is_wla_sym(&text) => {
                let n = self.load_symbols_str(&text);
                Ok(MlbImport {
                    imported: n,
                    ..MlbImport::default()
                })
            }
            "sym" => self.import_labels_bass(&text),
            _ => self.import_labels_mlb(&text, reset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bass_lines_read_as_mesen2_reads_them() {
        assert_eq!(
            bass_line("00808000 Reset"),
            Some((0x80_8000, "Reset".into()))
        );
        assert_eq!(
            bass_line("00808003 Reset.loop"),
            Some((0x80_8003, "Reset_loop".into()))
        );
        assert_eq!(
            bass_line("8010 ShortAddr"),
            Some((0x8010, "ShortAddr".into()))
        );
        assert_eq!(
            bass_line("00808040 name with spaces ; comment"),
            Some((0x80_8040, "name_with_spaces___comment".into()))
        );
        assert_eq!(
            bass_line("  00808090   _under"),
            Some((0x80_8090, "___under".into()))
        );
        assert_eq!(bass_line("0x8020 HexPrefix"), None);
        assert_eq!(bass_line("00808050 1digit"), None);
        assert_eq!(bass_line("00808080\tTabbed"), None);
        assert_eq!(bass_line("NoSpaceLine"), None);
        assert_eq!(bass_line(""), None);
        assert!(is_wla_sym(
            "; this file was created with wlalink\n[labels]\n"
        ));
        assert!(!is_wla_sym("00808000 Reset\n"));
    }

    #[test]
    fn dbg_attributes_split_on_commas_and_keep_quoted_names() {
        let a = attributes("\tid=0,name=\"lo.s\",size=600,mtime=0x66000000,mod=0");
        assert_eq!(a.get("name"), Some(&"lo.s"));
        assert_eq!(a.get("mtime"), Some(&"0x66000000"));
        let a = attributes(" id=3,file=0,line=8,span=3+7");
        assert_eq!(a.get("span"), Some(&"3+7"));
    }
}
