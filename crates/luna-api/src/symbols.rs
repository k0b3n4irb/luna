//! WLA-DX `.sym` symbol tables (issue #67, epic #63).
//!
//! Parses the `wlalink` symbol-file format — the one every WLA-DX-built
//! SNES homebrew (including the `OpenSNES` SDK) emits next to its ROM:
//!
//! ```text
//! ; this file was created with wlalink
//! [labels]
//! 00:8000 main
//! 7e:0100 monster_x
//! ```
//!
//! Only the label sections carry addresses we care about (`[labels]`,
//! `[symbols]`, `[exports]` — the format grew aliases over the years);
//! every other section is skipped. Lines are `BB:AAAA name` with hex
//! bank/offset.
//!
//! The table answers both directions:
//! - name → 24-bit address ([`SymbolTable::resolve`])
//! - address → nearest label at or below, same bank, as `name` or
//!   `name+0xNN` ([`SymbolTable::nearest`]) — the annotation used by the
//!   disassembler and the trace tools.

use std::path::Path;

/// A loaded symbol table: name ↔ 24-bit `bank:offset` addresses.
#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    /// `(name, addr)` pairs in file order (duplicates keep the first).
    by_name: Vec<(String, u32)>,
    /// `(addr, index into by_name)` sorted by address, for nearest lookup.
    by_addr: Vec<(u32, usize)>,
}

impl SymbolTable {
    /// Parse the `wlalink` `.sym` text format. Never fails on unknown
    /// sections or malformed lines — they are skipped (the format has
    /// grown many optional sections; a debugger table should be liberal).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut by_name: Vec<(String, u32)> = Vec::new();
        let mut in_labels = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                in_labels = matches!(
                    section.to_ascii_lowercase().as_str(),
                    "labels" | "symbols" | "exports"
                );
                continue;
            }
            if !in_labels {
                continue;
            }
            // `BB:AAAA name` — hex bank, hex offset, then the label.
            let Some((addr_part, name)) = line.split_once(' ') else {
                continue;
            };
            let Some((bank_s, off_s)) = addr_part.split_once(':') else {
                continue;
            };
            let (Ok(bank), Ok(off)) = (
                u8::from_str_radix(bank_s, 16),
                u16::from_str_radix(off_s, 16),
            ) else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let addr = (u32::from(bank) << 16) | u32::from(off);
            // First definition wins (wlalink can emit duplicates for
            // section-local labels).
            if !by_name.iter().any(|(n, _)| n == name) {
                by_name.push((name.to_string(), addr));
            }
        }
        let mut by_addr: Vec<(u32, usize)> = by_name
            .iter()
            .enumerate()
            .map(|(i, &(_, addr))| (addr, i))
            .collect();
        by_addr.sort_by_key(|&(addr, _)| addr);
        Self { by_name, by_addr }
    }

    /// Load and parse a `.sym` file from disk.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        Ok(Self::parse(&std::fs::read_to_string(path)?))
    }

    /// Number of labels in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// `true` when no labels were parsed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Resolve a label name to its 24-bit `bank:offset` address.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<u32> {
        self.by_name
            .iter()
            .find(|(n, _)| n == name)
            .map(|&(_, addr)| addr)
    }

    /// Nearest label at or below `addr` **within the same bank**,
    /// rendered as `name` (exact) or `name+0xNN`. The same-bank guard
    /// keeps a `$00`-bank label from claiming a `$7E` WRAM address.
    #[must_use]
    pub fn nearest(&self, addr: u32) -> Option<String> {
        let addr = addr & 0x00FF_FFFF;
        let idx = self.by_addr.partition_point(|&(a, _)| a <= addr);
        let (label_addr, name_idx) = self.by_addr.get(idx.checked_sub(1)?)?;
        if label_addr >> 16 != addr >> 16 {
            return None;
        }
        let name = &self.by_name[*name_idx].0;
        let off = addr - label_addr;
        Some(if off == 0 {
            name.clone()
        } else {
            format!("{name}+{off:#04X}")
        })
    }

    /// Iterate `(name, addr)` pairs in file order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.by_name.iter().map(|(n, a)| (n.as_str(), *a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYM: &str = "\
; this file was created with wlalink by ville helin
[information]
version 1

[labels]
00:8000 main
00:8012 main_loop
7e:0100 monster_x
7e:0102 monster_y

[definitions]
00000010 SOME_CONSTANT

[symbols]
00:9000 irq_handler
";

    #[test]
    fn parses_labels_and_symbols_sections_only() {
        let t = SymbolTable::parse(SYM);
        assert_eq!(t.len(), 5);
        assert_eq!(t.resolve("main"), Some(0x00_8000));
        assert_eq!(t.resolve("monster_x"), Some(0x7E_0100));
        assert_eq!(t.resolve("irq_handler"), Some(0x00_9000));
        // [definitions] lines don't match BB:AAAA and are skipped.
        assert_eq!(t.resolve("SOME_CONSTANT"), None);
        assert_eq!(t.resolve("nope"), None);
    }

    #[test]
    fn nearest_annotates_with_offset_within_the_bank() {
        let t = SymbolTable::parse(SYM);
        assert_eq!(t.nearest(0x00_8000).as_deref(), Some("main"));
        assert_eq!(t.nearest(0x00_8005).as_deref(), Some("main+0x05"));
        assert_eq!(t.nearest(0x00_8012).as_deref(), Some("main_loop"));
        assert_eq!(t.nearest(0x7E_0101).as_deref(), Some("monster_x+0x01"));
        // Below the first label of a bank / different bank: no annotation.
        assert_eq!(t.nearest(0x00_7FFF), None);
        assert_eq!(t.nearest(0x7F_0000), None);
    }

    #[test]
    fn malformed_lines_and_unknown_sections_are_skipped() {
        let t = SymbolTable::parse(
            "[labels]\nbogus\nzz:0000 bad\n00:zzzz bad2\n00:1234\n[weird]\n00:9999 not_a_label_section\n",
        );
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn duplicate_names_keep_the_first_definition() {
        let t = SymbolTable::parse("[labels]\n00:8000 twice\n00:9000 twice\n");
        assert_eq!(t.len(), 1);
        assert_eq!(t.resolve("twice"), Some(0x00_8000));
    }
}
