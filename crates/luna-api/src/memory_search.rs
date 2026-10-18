//! Memory Search, Mesen2's: a cheat finder over any memory that narrows a
//! set of candidate addresses filter after filter, with an undo.
//!
//! Every address of the memory starts as a candidate. A value is read
//! little-endian from each address on 1, 2 or 4 bytes (windows overlap and
//! need no alignment; an address whose value would run past the end is
//! left out). A filter compares each candidate's value now with a target —
//! its value at the previous filter (*previous search*), at the previous
//! refresh, a given value, or the value at a given address — and keeps the
//! ones that pass. The values compare signed or unsigned. After a filter,
//! the previous-search values are the current ones.
//!
//! The older WRAM-only `search_begin` / `search_refine` session is
//! separate and unchanged.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::AccessCounter;
use crate::memory::MemoryKind;
use crate::{ApiError, Emulator};

/// What a candidate's value is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum SearchTarget {
    /// Its value at the last filter (or the start).
    PreviousSearch,
    /// Its value at the last [`Emulator::mem_search_refresh`].
    PreviousRefresh,
    /// A value.
    Value(i64),
    /// The value now at this offset of the same memory.
    Address(u32),
}

/// How a value compares with its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchOperator {
    /// Equal.
    Equal,
    /// Not equal.
    NotEqual,
    /// Less than.
    LessThan,
    /// Greater than.
    GreaterThan,
    /// Less than or equal.
    LessThanOrEqual,
    /// Greater than or equal.
    GreaterThanOrEqual,
}

/// How a value is read: its size and its sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchFormat {
    /// 1, 2 or 4 bytes, little-endian.
    pub size: u8,
    /// Read as a signed number.
    pub signed: bool,
}

impl Default for SearchFormat {
    fn default() -> Self {
        Self {
            size: 1,
            signed: false,
        }
    }
}

impl SearchFormat {
    fn check(self) -> Result<Self, ApiError> {
        if matches!(self.size, 1 | 2 | 4) {
            Ok(self)
        } else {
            Err(ApiError::BadArg(format!(
                "value size {}: 1, 2 or 4 bytes",
                self.size
            )))
        }
    }

    /// The value at `at` in `mem`, `None` past the end.
    fn read(self, mem: &[u8], at: usize) -> Option<i64> {
        let n = usize::from(self.size);
        let b = mem.get(at..at + n)?;
        let mut raw = [0u8; 4];
        raw[..n].copy_from_slice(b);
        let u = u32::from_le_bytes(raw);
        Some(match (self.size, self.signed) {
            (1, true) => i64::from(u as u8 as i8),
            (2, true) => i64::from(u as u16 as i16),
            (_, true) => i64::from(u as i32),
            _ => i64::from(u),
        })
    }
}

/// One filter: a target and an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchFilter {
    /// What the value is compared with.
    pub target: SearchTarget,
    /// How.
    pub operator: SearchOperator,
}

/// One candidate, as Memory Search's table shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SearchRow {
    /// Its offset in the memory.
    pub offset: u32,
    /// Its value now.
    pub value: i64,
    /// Its value at the previous refresh.
    pub last_value: i64,
    /// Whether it passes the filter asked about (`None` when none was).
    pub matches: Option<bool>,
    /// Its access counters, when they are on.
    pub counters: Option<AccessCounter>,
}

/// A live search.
#[derive(Debug, Clone)]
pub(crate) struct MemSearch {
    kind: MemoryKind,
    candidates: Vec<u32>,
    /// The memory at the last filter (or the start).
    searched: Vec<u8>,
    /// The memory at the last refresh.
    refreshed: Vec<u8>,
    /// Candidates and previous-search values before each filter.
    history: Vec<(Vec<u32>, Vec<u8>)>,
}

/// Whether `value` passes `filter` in `ctx` (the memories it reads).
fn passes(
    format: SearchFormat,
    filter: SearchFilter,
    at: usize,
    value: i64,
    searched: &[u8],
    refreshed: &[u8],
    now: &[u8],
) -> bool {
    let target = match filter.target {
        SearchTarget::PreviousSearch => format.read(searched, at),
        SearchTarget::PreviousRefresh => format.read(refreshed, at),
        SearchTarget::Value(v) => Some(v),
        SearchTarget::Address(a) => format.read(now, a as usize),
    };
    let Some(t) = target else {
        return false;
    };
    match filter.operator {
        SearchOperator::Equal => value == t,
        SearchOperator::NotEqual => value != t,
        SearchOperator::LessThan => value < t,
        SearchOperator::GreaterThan => value > t,
        SearchOperator::LessThanOrEqual => value <= t,
        SearchOperator::GreaterThanOrEqual => value >= t,
    }
}

impl Emulator {
    /// The whole of `kind`, without side effects.
    fn search_snapshot(&self, kind: MemoryKind) -> Result<Vec<u8>, ApiError> {
        let size = self.memory_size(kind)?;
        self.read_memory(kind, 0, size)
    }

    fn mem_search(&self) -> Result<&MemSearch, ApiError> {
        self.mem_search
            .as_ref()
            .ok_or_else(|| ApiError::BadArg("no memory search — mem_search_start first".into()))
    }

    /// Start a search over a memory (Mesen2's *Reset*): every address a
    /// candidate, the previous-search and previous-refresh values the
    /// current ones, no history. A memory, not a bus: `SnesMemory`,
    /// `SpcMemory`, `Sa1Memory` and `GsuMemory` are refused. Returns the
    /// candidate count.
    pub fn mem_search_start(&mut self, kind: MemoryKind) -> Result<usize, ApiError> {
        if matches!(
            kind,
            MemoryKind::SnesMemory
                | MemoryKind::SpcMemory
                | MemoryKind::Sa1Memory
                | MemoryKind::GsuMemory
        ) {
            return Err(ApiError::BadArg(format!(
                "{}: Memory Search runs on a memory, not a bus (SnesWorkRam, SpcRam…)",
                kind.name()
            )));
        }
        let mem = self.search_snapshot(kind)?;
        let candidates: Vec<u32> = (0..mem.len() as u32).collect();
        let n = candidates.len();
        self.mem_search = Some(MemSearch {
            kind,
            candidates,
            refreshed: mem.clone(),
            searched: mem,
            history: Vec::new(),
        });
        Ok(n)
    }

    /// The memory the search runs on, `None` without one.
    #[must_use]
    pub fn mem_search_kind(&self) -> Option<MemoryKind> {
        self.mem_search.as_ref().map(|s| s.kind)
    }

    /// Take the current values as the previous-refresh ones (Mesen2 does
    /// it at each refresh of its window, about 30 times a second).
    pub fn mem_search_refresh(&mut self) -> Result<(), ApiError> {
        let kind = self.mem_search()?.kind;
        let mem = self.search_snapshot(kind)?;
        if let Some(s) = self.mem_search.as_mut() {
            s.refreshed = mem;
        }
        Ok(())
    }

    /// Keep the candidates that pass `filter` with values read as
    /// `format` (Mesen2's *Apply filter*); the previous-search values
    /// become the current ones. Returns the candidates left.
    pub fn mem_search_filter(
        &mut self,
        format: SearchFormat,
        filter: SearchFilter,
    ) -> Result<usize, ApiError> {
        let format = format.check()?;
        let kind = self.mem_search()?.kind;
        let now = self.search_snapshot(kind)?;
        let s = self.mem_search.as_mut().ok_or(ApiError::NoRom)?;
        let kept: Vec<u32> = s
            .candidates
            .iter()
            .copied()
            .filter(|&a| {
                format.read(&now, a as usize).is_some_and(|v| {
                    passes(
                        format,
                        filter,
                        a as usize,
                        v,
                        &s.searched,
                        &s.refreshed,
                        &now,
                    )
                })
            })
            .collect();
        let before = std::mem::replace(&mut s.candidates, kept);
        let searched = std::mem::replace(&mut s.searched, now);
        s.history.push((before, searched));
        Ok(s.candidates.len())
    }

    /// Undo the last filter (no limit). Returns the candidates, or an
    /// error when there is nothing to undo.
    pub fn mem_search_undo(&mut self) -> Result<usize, ApiError> {
        let s = self
            .mem_search
            .as_mut()
            .ok_or_else(|| ApiError::BadArg("no memory search — mem_search_start first".into()))?;
        let (candidates, searched) = s
            .history
            .pop()
            .ok_or_else(|| ApiError::BadArg("no filter to undo".into()))?;
        s.candidates = candidates;
        s.searched = searched;
        Ok(s.candidates.len())
    }

    /// How many candidates are left, and how many filters can be undone.
    pub fn mem_search_count(&self) -> Result<(usize, usize), ApiError> {
        let s = self.mem_search()?;
        Ok((s.candidates.len(), s.history.len()))
    }

    /// Candidates `first..first + count`, in address order, with values
    /// read as `format`; `matches` says whether each passes `preview`
    /// (Mesen2's live *Match* column) when one is given.
    pub fn mem_search_rows(
        &self,
        first: usize,
        count: usize,
        format: SearchFormat,
        preview: Option<SearchFilter>,
    ) -> Result<Vec<SearchRow>, ApiError> {
        let format = format.check()?;
        let s = self.mem_search()?;
        let now = self.search_snapshot(s.kind)?;
        let counted = self.access_counters_enabled();
        let rows = s.candidates.iter().skip(first).take(count);
        Ok(rows
            .map(|&a| {
                let at = a as usize;
                let value = format.read(&now, at).unwrap_or_default();
                SearchRow {
                    offset: a,
                    value,
                    last_value: format.read(&s.refreshed, at).unwrap_or_default(),
                    matches: preview.map(|f| {
                        format.read(&now, at).is_some()
                            && passes(format, f, at, value, &s.searched, &s.refreshed, &now)
                    }),
                    counters: counted
                        .then(|| self.access_counters(s.kind, a, 1).ok())
                        .flatten()
                        .and_then(|c| c.first().copied()),
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_read_little_endian_with_their_sign() {
        let mem = [0xFE, 0xFF, 0x7F, 0x80];
        let f = |size, signed| SearchFormat { size, signed };
        assert_eq!(f(1, false).read(&mem, 0), Some(0xFE));
        assert_eq!(f(1, true).read(&mem, 0), Some(-2));
        assert_eq!(f(2, true).read(&mem, 0), Some(-2));
        assert_eq!(f(2, false).read(&mem, 2), Some(0x807F));
        assert_eq!(f(4, true).read(&mem, 0), Some(-2_139_095_042));
        assert_eq!(f(2, false).read(&mem, 3), None);
    }
}
