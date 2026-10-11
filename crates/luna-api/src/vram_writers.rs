//! Who wrote what into VRAM: every byte sent to `$2118/$2119` during a
//! run, folded by source — the label a DMA read its bytes from, or the
//! routine whose code wrote the port — into ranges of words, and the
//! ranges two different sources both wrote.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::{ApiError, Emulator};

/// Words in VRAM.
const WORDS: usize = 0x8000;
/// What the DMA trace may hold between two drains: more than a frame of
/// DMA can move.
const DMA_EVENTS_CAP: usize = 1 << 20;

/// How a source reached VRAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VramWriteKind {
    /// A DMA: the source is where the bytes were read from.
    Dma,
    /// A CPU write to the port: the source is the code that wrote.
    Cpu,
}

/// One range of contiguous VRAM words written by one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct VramWriterRow {
    /// First word of the range (`VMADD` address).
    pub first_word: u16,
    /// Last word of the range, included.
    pub last_word: u16,
    /// Bytes written into the range over the run.
    pub bytes: u64,
    /// The label holding the DMA's source address or the writing code;
    /// `$BB:AA00` (a 256-byte page) without one.
    pub source: String,
    /// DMA or CPU write.
    pub how: VramWriteKind,
    /// Frame of the first write into the range.
    pub first_frame: u64,
    /// Frame of the last write into the range.
    pub last_frame: u64,
}

/// The words two different sources both wrote: one entry per pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct VramOverlap {
    /// The lowest word both wrote.
    pub first_word: u16,
    /// The highest word both wrote.
    pub last_word: u16,
    /// How many words both wrote, between the two.
    pub words: u32,
    /// The source that wrote a shared word first.
    pub first: String,
    /// Frame of its first write into a shared word.
    pub first_frame: u64,
    /// The other source.
    pub then: String,
    /// Frame of its first write into a shared word.
    pub then_frame: u64,
}

/// The folded report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct VramWritersReport {
    /// Writes before this frame were left out.
    pub from_frame: u64,
    /// One row per range and source, by address.
    pub rows: Vec<VramWriterRow>,
    /// The pairs of different sources that wrote the same words, by
    /// address.
    pub overlaps: Vec<VramOverlap>,
}

/// What one source did to each word.
struct PerWord {
    /// Frame of the first write, plus one (`0` = never written).
    first: Vec<u32>,
    last: Vec<u32>,
    bytes: Vec<u32>,
}

impl PerWord {
    fn new() -> Self {
        Self {
            first: vec![0; WORDS],
            last: vec![0; WORDS],
            bytes: vec![0; WORDS],
        }
    }
}

/// Accumulates VRAM writes over a run; [`Self::report`] folds them.
#[derive(Default)]
pub struct VramWriters {
    from_frame: u64,
    /// Per source name: what it wrote by DMA, and by the port.
    sources: BTreeMap<String, [Option<PerWord>; 2]>,
}

impl VramWriters {
    /// An empty record that leaves out writes before `from_frame`.
    #[must_use]
    pub const fn new(from_frame: u64) -> Self {
        Self {
            from_frame,
            sources: BTreeMap::new(),
        }
    }

    /// One byte written to `word` by `source` during `frame`.
    pub fn record(&mut self, source: &str, how: VramWriteKind, word: u16, frame: u64) {
        if frame < self.from_frame {
            return;
        }
        if !self.sources.contains_key(source) {
            self.sources.insert(source.to_string(), [None, None]);
        }
        let Some(slots) = self.sources.get_mut(source) else {
            return;
        };
        let per_word =
            slots[usize::from(how == VramWriteKind::Cpu)].get_or_insert_with(PerWord::new);
        let w = usize::from(word) & (WORDS - 1);
        let stamp = u32::try_from(frame).unwrap_or(u32::MAX - 1) + 1;
        if per_word.first[w] == 0 {
            per_word.first[w] = stamp;
        }
        per_word.last[w] = stamp;
        per_word.bytes[w] = per_word.bytes[w].saturating_add(1);
    }

    /// Fold the record into rows and overlaps.
    #[must_use]
    pub fn report(&self) -> VramWritersReport {
        let mut rows = Vec::new();
        for (source, how, per_word) in self.per_source() {
            for (start, end) in runs(|w| per_word.first[w] != 0) {
                rows.push(VramWriterRow {
                    first_word: start as u16,
                    last_word: end as u16,
                    bytes: per_word.bytes[start..=end]
                        .iter()
                        .map(|&b| u64::from(b))
                        .sum(),
                    source: source.clone(),
                    how,
                    first_frame: u64::from(min_stamp(&per_word.first[start..=end]) - 1),
                    last_frame: u64::from(
                        per_word.last[start..=end].iter().max().map_or(1, |&m| m) - 1,
                    ),
                });
            }
        }
        rows.sort_by_key(|a| (a.first_word, a.first_frame));

        let mut overlaps = Vec::new();
        let sources: Vec<_> = self.per_source().collect();
        for (i, (name_a, _, a)) in sources.iter().enumerate() {
            for (name_b, _, b) in &sources[i + 1..] {
                if name_a == name_b {
                    continue; // one source, by DMA and by the port
                }
                let shared: Vec<usize> = (0..WORDS)
                    .filter(|&w| a.first[w] != 0 && b.first[w] != 0)
                    .collect();
                let (Some(&start), Some(&end)) = (shared.first(), shared.last()) else {
                    continue;
                };
                let first_stamp =
                    |p: &PerWord| shared.iter().map(|&w| p.first[w]).min().unwrap_or(1);
                let frame_a = u64::from(first_stamp(a) - 1);
                let frame_b = u64::from(first_stamp(b) - 1);
                let ((first, first_frame), (then, then_frame)) = if frame_a <= frame_b {
                    ((name_a, frame_a), (name_b, frame_b))
                } else {
                    ((name_b, frame_b), (name_a, frame_a))
                };
                overlaps.push(VramOverlap {
                    first_word: start as u16,
                    last_word: end as u16,
                    words: shared.len() as u32,
                    first: (*first).clone(),
                    first_frame,
                    then: (*then).clone(),
                    then_frame,
                });
            }
        }
        overlaps.sort_by_key(|a| (a.first_word, a.first_frame));
        VramWritersReport {
            from_frame: self.from_frame,
            rows,
            overlaps,
        }
    }
}

impl VramWriters {
    /// Every `(source, how)` that wrote, by name.
    fn per_source(&self) -> impl Iterator<Item = (&String, VramWriteKind, &PerWord)> {
        self.sources.iter().flat_map(|(name, slots)| {
            [VramWriteKind::Dma, VramWriteKind::Cpu]
                .into_iter()
                .zip(slots)
                .filter_map(move |(how, slot)| slot.as_ref().map(|p| (name, how, p)))
        })
    }
}

/// The smallest non-zero stamp of a range that holds one.
fn min_stamp(stamps: &[u32]) -> u32 {
    stamps
        .iter()
        .copied()
        .filter(|&s| s != 0)
        .min()
        .unwrap_or(1)
}

/// The maximal runs of consecutive words for which `written` holds, as
/// `(first, last)`.
fn runs(written: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for w in 0..WORDS {
        match (written(w), start) {
            (true, None) => start = Some(w),
            (false, Some(s)) => {
                out.push((s, w - 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, WORDS - 1));
    }
    out
}

impl Emulator {
    /// Start recording every byte written to the VRAM data ports, by
    /// DMA or by the CPU. Takes over the DMA trace: drain it through
    /// [`Self::collect_vram_writes`] only, at least once a frame.
    pub fn enable_vram_writers(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.dma.enable_dma_trace(DMA_EVENTS_CAP);
        snes.vram_cpu_writes = Some(Vec::new());
        Ok(())
    }

    /// Move what was written since the last call into `writers`, each
    /// byte under its source: the label that holds the DMA's source
    /// address, or the one that holds the writing instruction.
    pub fn collect_vram_writes(&mut self, writers: &mut VramWriters) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let dma = snes.dma.take_dma_trace();
        let cpu = snes
            .vram_cpu_writes
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let symbols = self.symbols.as_ref();
        let mut record = |addr: u32, how, word, frame| {
            if let Some((label, _)) = symbols.and_then(|t| t.nearest_label(addr)) {
                writers.record(label, how, word, frame);
            } else {
                let page = format!("${:02X}:{:04X}", (addr >> 16) & 0xFF, addr & 0xFF00);
                writers.record(&page, how, word, frame);
            }
        };
        for ev in dma.iter().filter(|ev| matches!(ev.b_offset, 0x18 | 0x19)) {
            record(ev.src_full, VramWriteKind::Dma, ev.vram_word, ev.frame);
        }
        for ev in &cpu {
            record(ev.pc_full, VramWriteKind::Cpu, ev.word, ev.frame);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_fold_into_ranges_by_source_and_overlaps_between_sources() {
        let mut w = VramWriters::new(10);
        w.record("early", VramWriteKind::Dma, 0x6800, 9); // before the window
        for word in 0x6800..0x6B00u16 {
            w.record("text_tiles", VramWriteKind::Dma, word, 318);
            w.record("text_tiles", VramWriteKind::Dma, word, 318);
        }
        // A column, one word in 32, then the rest of the map later: the
        // ring is rewritten by the same source and is no overlap.
        for word in (0x6800..0x6C00u16).step_by(32) {
            w.record("pitch_cols", VramWriteKind::Dma, word, 320);
        }
        for word in 0x6800..0x6C00u16 {
            w.record("pitch_cols", VramWriteKind::Dma, word, 400);
        }
        w.record("hudDraw", VramWriteKind::Cpu, 0x7340, 330);
        w.record("hudDraw", VramWriteKind::Cpu, 0x7340, 331);

        let report = w.report();
        assert_eq!(report.rows.len(), 3, "{report:#?}");
        let text = &report.rows[0];
        assert_eq!((text.first_word, text.last_word), (0x6800, 0x6AFF));
        assert_eq!(
            (text.bytes, text.first_frame, text.last_frame),
            (1536, 318, 318)
        );
        let cols = &report.rows[1];
        assert_eq!((cols.first_word, cols.last_word), (0x6800, 0x6BFF));
        assert_eq!((cols.first_frame, cols.last_frame), (320, 400));
        let hud = &report.rows[2];
        assert_eq!(
            (hud.first_word, hud.bytes, hud.how),
            (0x7340, 2, VramWriteKind::Cpu)
        );

        assert_eq!(
            report.overlaps,
            [VramOverlap {
                first_word: 0x6800,
                last_word: 0x6AFF,
                words: 0x300,
                first: "text_tiles".into(),
                first_frame: 318,
                then: "pitch_cols".into(),
                then_frame: 320,
            }]
        );
    }
}
