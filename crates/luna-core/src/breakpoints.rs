//! [`BreakpointSet`] — a first-class breakpoint/watchpoint registry
//! (issue #66, epic #63).
//!
//! Replaces the single-step-and-scan emulation of breakpoints
//! (`run_until_pc` / `run_until_mem_access`) with a registry checked at
//! the two hook points the tracers already use:
//!
//! - **exec breakpoints** — the per-instruction capture point at the top
//!   of [`crate::Snes::step`] compares the live `PB:PC` against the set.
//! - **memory watchpoints** — `SnesBus::trace_mem_access` (inlined in
//!   every CPU bus access) matches the address/kind and parks a
//!   [`BreakHit`] in [`BreakpointSet::pending_hit`] for the driving loop
//!   to consume after the instruction completes (instruction-atomic, like
//!   luna's interrupt model).
//!
//! Overhead with no registry installed is a single `Option` check —
//! the same class as the existing trace hooks.

use serde::{Deserialize, Serialize};

use crate::snes::MemEventKind;

/// What kind of event a breakpoint fires on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakKind {
    /// The CPU is about to execute the instruction at the registered PC.
    Exec,
    /// An instruction read a watched address.
    Read,
    /// An instruction wrote a watched address.
    Write,
}

/// One registered breakpoint, as reported by [`BreakpointSet::list`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakpointInfo {
    /// Registry id (stable until removed).
    pub id: u32,
    /// `true` for an exec breakpoint, `false` for a memory watchpoint.
    pub exec: bool,
    /// Exec: the 24-bit `PB:PC`. Mem: the range start (24-bit bus address).
    pub lo: u32,
    /// Exec: same as `lo`. Mem: the inclusive range end.
    pub hi: u32,
    /// Mem watchpoints: fire on reads.
    pub on_read: bool,
    /// Mem watchpoints: fire on writes.
    pub on_write: bool,
}

/// A breakpoint/watchpoint hit, returned to the driving run loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BreakHit {
    /// Id of the breakpoint that fired.
    pub id: u32,
    /// What fired.
    pub kind: BreakKind,
    /// 24-bit PC — for exec, the about-to-execute instruction; for
    /// mem, the instruction that performed the access.
    pub pc: u32,
    /// Mem hits: the accessed 24-bit bus address.
    pub addr: Option<u32>,
    /// Mem hits: the byte transferred.
    pub value: Option<u8>,
}

/// A registered memory watchpoint (inclusive 24-bit address range).
#[derive(Debug, Clone, Copy)]
struct MemWatch {
    id: u32,
    lo: u32,
    hi: u32,
    on_read: bool,
    on_write: bool,
}

/// The registry. Installed on [`crate::Snes`] as an `Option<Box<_>>` so
/// the no-debugger fast path costs one pointer-sized check.
#[derive(Debug, Default)]
pub struct BreakpointSet {
    next_id: u32,
    /// Exec breakpoints: `(id, 24-bit PB:PC)`. Linear scan — debugger
    /// registries hold a handful of entries; a hash set would cost more
    /// in the common 1-3 entry case.
    exec: Vec<(u32, u32)>,
    /// Memory watchpoints.
    mem: Vec<MemWatch>,
    /// Watchpoint hit parked by the bus mid-instruction, consumed by the
    /// driving loop after the instruction completes. Only the FIRST hit
    /// of an instruction is kept (a 16-bit write may touch a range twice).
    pub pending_hit: Option<BreakHit>,
}

impl BreakpointSet {
    /// Fresh empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an exec breakpoint at a 24-bit `PB:PC`. Returns its id.
    pub fn add_exec(&mut self, pc: u32) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.exec.push((id, pc & 0x00FF_FFFF));
        id
    }

    /// Register a memory watchpoint over the inclusive 24-bit bus range
    /// `lo..=hi`, firing on reads and/or writes. Returns its id.
    pub fn add_mem(&mut self, lo: u32, hi: u32, on_read: bool, on_write: bool) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.mem.push(MemWatch {
            id,
            lo: lo & 0x00FF_FFFF,
            hi: hi & 0x00FF_FFFF,
            on_read,
            on_write,
        });
        id
    }

    /// Remove a breakpoint by id. Returns `true` if it existed.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.exec.len() + self.mem.len();
        self.exec.retain(|(i, _)| *i != id);
        self.mem.retain(|w| w.id != id);
        before != self.exec.len() + self.mem.len()
    }

    /// Remove every breakpoint (ids are not reused).
    pub fn clear(&mut self) {
        self.exec.clear();
        self.mem.clear();
        self.pending_hit = None;
    }

    /// `true` when nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exec.is_empty() && self.mem.is_empty()
    }

    /// Snapshot the registry for a debugger UI / MCP client.
    #[must_use]
    pub fn list(&self) -> Vec<BreakpointInfo> {
        let mut out: Vec<BreakpointInfo> = self
            .exec
            .iter()
            .map(|&(id, pc)| BreakpointInfo {
                id,
                exec: true,
                lo: pc,
                hi: pc,
                on_read: false,
                on_write: false,
            })
            .chain(self.mem.iter().map(|w| BreakpointInfo {
                id: w.id,
                exec: false,
                lo: w.lo,
                hi: w.hi,
                on_read: w.on_read,
                on_write: w.on_write,
            }))
            .collect();
        out.sort_by_key(|b| b.id);
        out
    }

    /// Exec check — called with the live `PB:PC` before an instruction
    /// executes.
    #[must_use]
    pub fn check_exec(&self, pc: u32) -> Option<BreakHit> {
        self.exec
            .iter()
            .find(|&&(_, bp)| bp == pc)
            .map(|&(id, _)| BreakHit {
                id,
                kind: BreakKind::Exec,
                pc,
                addr: None,
                value: None,
            })
    }

    /// Memory check — called from the bus on every access when a registry
    /// is installed. Parks the FIRST hit of the instruction in
    /// [`Self::pending_hit`].
    pub fn check_mem(&mut self, addr: u32, kind: MemEventKind, value: u8, pc: u32) {
        if self.pending_hit.is_some() || self.mem.is_empty() {
            return;
        }
        let (is_read, break_kind) = match kind {
            MemEventKind::Read => (true, BreakKind::Read),
            MemEventKind::Write => (false, BreakKind::Write),
            // Interrupt-delivery markers are trace annotations, not accesses.
            MemEventKind::NmiSignal | MemEventKind::IrqSignal => return,
        };
        if let Some(w) = self
            .mem
            .iter()
            .find(|w| (w.lo..=w.hi).contains(&addr) && if is_read { w.on_read } else { w.on_write })
        {
            self.pending_hit = Some(BreakHit {
                id: w.id,
                kind: break_kind,
                pc,
                addr: Some(addr),
                value: Some(value),
            });
        }
    }

    /// Consume the watchpoint hit parked by the bus, if any.
    pub const fn take_pending(&mut self) -> Option<BreakHit> {
        self.pending_hit.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_breakpoint_matches_exact_pc() {
        let mut bp = BreakpointSet::new();
        let id = bp.add_exec(0x00_8012);
        assert!(bp.check_exec(0x00_8011).is_none());
        let hit = bp.check_exec(0x00_8012).unwrap();
        assert_eq!((hit.id, hit.kind), (id, BreakKind::Exec));
        assert_eq!(hit.pc, 0x00_8012);
    }

    #[test]
    fn mem_watchpoint_matches_range_and_kind() {
        let mut bp = BreakpointSet::new();
        let id = bp.add_mem(0x7E_1000, 0x7E_10FF, false, true); // write-only
        // Read inside the range: no hit (write-only watch).
        bp.check_mem(0x7E_1080, MemEventKind::Read, 0xAA, 0x00_8000);
        assert!(bp.pending_hit.is_none());
        // Write outside the range: no hit.
        bp.check_mem(0x7E_1100, MemEventKind::Write, 0xAA, 0x00_8000);
        assert!(bp.pending_hit.is_none());
        // Write inside: hit with full context.
        bp.check_mem(0x7E_1080, MemEventKind::Write, 0xAA, 0x00_8000);
        let hit = bp.take_pending().unwrap();
        assert_eq!(hit.id, id);
        assert_eq!(hit.kind, BreakKind::Write);
        assert_eq!(hit.addr, Some(0x7E_1080));
        assert_eq!(hit.value, Some(0xAA));
        assert_eq!(hit.pc, 0x00_8000);
    }

    #[test]
    fn first_hit_of_an_instruction_wins() {
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x7E_0000, 0x7E_FFFF, true, true);
        bp.check_mem(0x7E_0001, MemEventKind::Write, 1, 0);
        bp.check_mem(0x7E_0002, MemEventKind::Write, 2, 0);
        assert_eq!(bp.take_pending().unwrap().addr, Some(0x7E_0001));
    }

    #[test]
    fn interrupt_markers_never_trip_watchpoints() {
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x00_0000, 0xFF_FFFF, true, true);
        bp.check_mem(0x00_FFEA, MemEventKind::NmiSignal, 0, 0);
        bp.check_mem(0x00_FFEE, MemEventKind::IrqSignal, 0, 0);
        assert!(bp.pending_hit.is_none());
    }

    #[test]
    fn remove_clear_list_lifecycle() {
        let mut bp = BreakpointSet::new();
        let a = bp.add_exec(0x00_8000);
        let b = bp.add_mem(0x7E_0000, 0x7E_00FF, true, false);
        assert_eq!(bp.list().len(), 2);
        assert!(bp.remove(a));
        assert!(!bp.remove(a), "double-remove is false");
        assert_eq!(bp.list().len(), 1);
        assert_eq!(bp.list()[0].id, b);
        let c = bp.add_exec(0x00_9000);
        assert!(c > b, "ids are never reused");
        bp.clear();
        assert!(bp.is_empty());
    }
}
