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
    /// A disabled breakpoint stays registered but never fires (issue #176).
    pub enabled: bool,
    /// Times this breakpoint has fired since creation. Memory watchpoints
    /// count at most one hit per instruction (the first access wins, same
    /// rule as [`BreakpointSet::pending_hit`]) — a multi-access instruction
    /// undercounts by design.
    pub hit_count: u64,
    /// Mem watchpoints: whether mirror folding is active (issue #91).
    pub mirror: bool,
    /// Display name (e.g. the symbol it was created from).
    pub name: Option<String>,
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

/// Fold an address to a canonical form so a watchpoint matches accesses that
/// reach the same physical location through an address mirror (issue #91):
///
/// - the **WRAM low 8 KB** — `$00-$3F:0000-1FFF` and `$80-$BF:0000-1FFF` all
///   alias `$7E:0000-1FFF` — folds to `$7E:off`;
/// - the **MMIO windows** — `$2100-$21FF`, `$4016-$4017`, `$4200-$43FF` appear
///   in every system bank (`$00-$3F`/`$80-$BF`) — fold to bank `$00`.
///
/// Everything else (WRAM high `$7E:2000-` / `$7F`, cartridge space) passes
/// through unchanged.
const fn fold_mirror(addr: u32) -> u32 {
    let bank = (addr >> 16) & 0xFF;
    let off = addr & 0xFFFF;
    let system = bank <= 0x3F || (bank >= 0x80 && bank <= 0xBF);
    if system {
        if off < 0x2000 {
            return 0x7E_0000 | off; // WRAM low → canonical $7E
        }
        if (off >= 0x2100 && off <= 0x21FF)
            || (off >= 0x4200 && off <= 0x43FF)
            || off == 0x4016
            || off == 0x4017
        {
            return off; // MMIO → canonical bank $00
        }
    }
    addr
}

/// A registered exec breakpoint.
#[derive(Debug)]
struct ExecBp {
    id: u32,
    pc: u32,
    enabled: bool,
    name: Option<String>,
    /// `Cell` because [`BreakpointSet::check_exec`] is deliberately
    /// `&self` — the run loop holds the registry by shared reference
    /// while stepping the machine mutably elsewhere.
    hits: std::cell::Cell<u64>,
}

/// A registered memory watchpoint (inclusive 24-bit address range).
#[derive(Debug, Clone)]
struct MemWatch {
    id: u32,
    lo: u32,
    hi: u32,
    on_read: bool,
    on_write: bool,
    /// Also match accesses that reach this range through an address mirror
    /// (issue #91). `clo`/`chi` are the folded endpoints, valid as a range
    /// only when both fold into the same region (`clo <= chi`).
    mirror: bool,
    clo: u32,
    chi: u32,
    enabled: bool,
    name: Option<String>,
    /// Plain counter — [`BreakpointSet::check_mem`] is already `&mut`.
    hits: u64,
}

/// The registry. Installed on [`crate::Snes`] as an `Option<Box<_>>` so
/// the no-debugger fast path costs one pointer-sized check.
#[derive(Debug, Default)]
pub struct BreakpointSet {
    next_id: u32,
    /// Exec breakpoints. Linear scan — debugger registries hold a
    /// handful of entries; a hash set would cost more in the common
    /// 1-3 entry case.
    exec: Vec<ExecBp>,
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

    /// Register an exec breakpoint at a 24-bit `PB:PC`, optionally named
    /// (e.g. after the symbol it was created from). Returns its id.
    pub fn add_exec(&mut self, pc: u32, name: Option<String>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.exec.push(ExecBp {
            id,
            pc: pc & 0x00FF_FFFF,
            enabled: true,
            name,
            hits: std::cell::Cell::new(0),
        });
        id
    }

    /// Register a memory watchpoint over the inclusive 24-bit bus range
    /// `lo..=hi`, firing on reads and/or writes. Returns its id. With
    /// `mirror`, also matches accesses that reach the range through a WRAM /
    /// MMIO address mirror (issue #91) — e.g. a watch on `$7E:0500` fires on
    /// `$00:0500`, and a watch on `$00:2100` fires on `$80:2100`.
    pub fn add_mem(
        &mut self,
        lo: u32,
        hi: u32,
        on_read: bool,
        on_write: bool,
        mirror: bool,
        name: Option<String>,
    ) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let lo = lo & 0x00FF_FFFF;
        let hi = hi & 0x00FF_FFFF;
        self.mem.push(MemWatch {
            id,
            lo,
            hi,
            on_read,
            on_write,
            mirror,
            clo: fold_mirror(lo),
            chi: fold_mirror(hi),
            enabled: true,
            name,
            hits: 0,
        });
        id
    }

    /// Enable / disable a breakpoint without removing it (issue #176) —
    /// keeps its id, name and hit count. Returns `true` if the id exists.
    pub fn set_enabled(&mut self, id: u32, enabled: bool) -> bool {
        if let Some(bp) = self.exec.iter_mut().find(|b| b.id == id) {
            bp.enabled = enabled;
            return true;
        }
        if let Some(w) = self.mem.iter_mut().find(|w| w.id == id) {
            w.enabled = enabled;
            return true;
        }
        false
    }

    /// Remove a breakpoint by id. Returns `true` if it existed.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.exec.len() + self.mem.len();
        self.exec.retain(|b| b.id != id);
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
    pub const fn is_empty(&self) -> bool {
        self.exec.is_empty() && self.mem.is_empty()
    }

    /// Snapshot the registry for a debugger UI / MCP client.
    #[must_use]
    pub fn list(&self) -> Vec<BreakpointInfo> {
        let mut out: Vec<BreakpointInfo> = self
            .exec
            .iter()
            .map(|b| BreakpointInfo {
                id: b.id,
                exec: true,
                lo: b.pc,
                hi: b.pc,
                on_read: false,
                on_write: false,
                enabled: b.enabled,
                hit_count: b.hits.get(),
                mirror: false,
                name: b.name.clone(),
            })
            .chain(self.mem.iter().map(|w| BreakpointInfo {
                id: w.id,
                exec: false,
                lo: w.lo,
                hi: w.hi,
                on_read: w.on_read,
                on_write: w.on_write,
                enabled: w.enabled,
                hit_count: w.hits,
                mirror: w.mirror,
                name: w.name.clone(),
            }))
            .collect();
        out.sort_by_key(|b| b.id);
        out
    }

    /// Exec check — called with the live `PB:PC` before an instruction
    /// executes.
    #[must_use]
    pub fn check_exec(&self, pc: u32) -> Option<BreakHit> {
        self.exec.iter().find(|b| b.enabled && b.pc == pc).map(|b| {
            b.hits.set(b.hits.get() + 1);
            BreakHit {
                id: b.id,
                kind: BreakKind::Exec,
                pc,
                addr: None,
                value: None,
            }
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
        let faddr = fold_mirror(addr);
        if let Some(w) = self.mem.iter_mut().find(|w| {
            let kind_ok = if is_read { w.on_read } else { w.on_write };
            w.enabled
                && kind_ok
                && ((w.lo..=w.hi).contains(&addr)
                    || (w.mirror && w.clo <= w.chi && (w.clo..=w.chi).contains(&faddr)))
        }) {
            w.hits += 1;
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
        let id = bp.add_exec(0x00_8012, None);
        assert!(bp.check_exec(0x00_8011).is_none());
        let hit = bp.check_exec(0x00_8012).unwrap();
        assert_eq!((hit.id, hit.kind), (id, BreakKind::Exec));
        assert_eq!(hit.pc, 0x00_8012);
    }

    #[test]
    fn mem_watchpoint_matches_range_and_kind() {
        let mut bp = BreakpointSet::new();
        let id = bp.add_mem(0x7E_1000, 0x7E_10FF, false, true, false, None); // write-only
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
        bp.add_mem(0x7E_0000, 0x7E_FFFF, true, true, false, None);
        bp.check_mem(0x7E_0001, MemEventKind::Write, 1, 0);
        bp.check_mem(0x7E_0002, MemEventKind::Write, 2, 0);
        assert_eq!(bp.take_pending().unwrap().addr, Some(0x7E_0001));
    }

    #[test]
    fn interrupt_markers_never_trip_watchpoints() {
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x00_0000, 0xFF_FFFF, true, true, false, None);
        bp.check_mem(0x00_FFEA, MemEventKind::NmiSignal, 0, 0);
        bp.check_mem(0x00_FFEE, MemEventKind::IrqSignal, 0, 0);
        assert!(bp.pending_hit.is_none());
    }

    #[test]
    fn remove_clear_list_lifecycle() {
        let mut bp = BreakpointSet::new();
        let a = bp.add_exec(0x00_8000, None);
        let b = bp.add_mem(0x7E_0000, 0x7E_00FF, true, false, false, None);
        assert_eq!(bp.list().len(), 2);
        assert!(bp.remove(a));
        assert!(!bp.remove(a), "double-remove is false");
        assert_eq!(bp.list().len(), 1);
        assert_eq!(bp.list()[0].id, b);
        let c = bp.add_exec(0x00_9000, None);
        assert!(c > b, "ids are never reused");
        bp.clear();
        assert!(bp.is_empty());
    }

    #[test]
    fn disabled_breakpoints_never_fire_and_keep_state() {
        let mut bp = BreakpointSet::new();
        let x = bp.add_exec(0x00_8000, Some("main".into()));
        let m = bp.add_mem(0x7E_0100, 0x7E_0100, false, true, false, None);

        // Hit both once.
        assert!(bp.check_exec(0x00_8000).is_some());
        bp.check_mem(0x7E_0100, MemEventKind::Write, 1, 0);
        assert!(bp.take_pending().is_some());

        // Disable: neither fires, both stay listed with state intact.
        assert!(bp.set_enabled(x, false));
        assert!(bp.set_enabled(m, false));
        assert!(bp.check_exec(0x00_8000).is_none());
        bp.check_mem(0x7E_0100, MemEventKind::Write, 2, 0);
        assert!(bp.pending_hit.is_none());
        let list = bp.list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|b| !b.enabled));
        assert_eq!(list[0].hit_count, 1);
        assert_eq!(list[0].name.as_deref(), Some("main"));

        // Re-enable: fires again, counts resume.
        assert!(bp.set_enabled(x, true));
        assert!(bp.check_exec(0x00_8000).is_some());
        assert_eq!(bp.list()[0].hit_count, 2);

        // Unknown id.
        assert!(!bp.set_enabled(999, true));
    }

    #[test]
    fn hit_counts_accumulate_per_breakpoint() {
        let mut bp = BreakpointSet::new();
        let a = bp.add_exec(0x00_8000, None);
        let b = bp.add_exec(0x00_9000, None);
        for _ in 0..3 {
            let _ = bp.check_exec(0x00_8000);
        }
        let _ = bp.check_exec(0x00_9000);
        let list = bp.list();
        assert_eq!(list.iter().find(|e| e.id == a).unwrap().hit_count, 3);
        assert_eq!(list.iter().find(|e| e.id == b).unwrap().hit_count, 1);
    }

    #[test]
    fn mirror_watch_folds_wram_low_page() {
        // Watch a WRAM-low variable via $7E; a mirror access through $00/$80
        // must fire, but $7F (different WRAM half) and WRAM-high must not.
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x7E_0500, 0x7E_0500, false, true, true, None);
        bp.check_mem(0x00_0500, MemEventKind::Write, 0x11, 0x00_8000);
        assert_eq!(
            bp.take_pending().unwrap().addr,
            Some(0x00_0500),
            "$00 mirror"
        );
        bp.check_mem(0x80_0500, MemEventKind::Write, 0x22, 0x00_8000);
        assert_eq!(
            bp.take_pending().unwrap().addr,
            Some(0x80_0500),
            "$80 mirror"
        );
        // $7F:0500 is a different physical byte (WRAM high half) — no hit.
        bp.check_mem(0x7F_0500, MemEventKind::Write, 0x33, 0x00_8000);
        assert!(bp.pending_hit.is_none(), "$7F is not a mirror of $7E-low");
    }

    #[test]
    fn mirror_watch_folds_mmio_across_banks() {
        // Watch $00:2100 (INIDISP); a FastROM access via $80:2100 must fire.
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x00_2100, 0x00_2100, false, true, true, None);
        bp.check_mem(0x80_2100, MemEventKind::Write, 0x8F, 0x80_8000);
        assert_eq!(bp.take_pending().unwrap().addr, Some(0x80_2100));
    }

    #[test]
    fn mirror_off_stays_bank_exact() {
        // Without mirroring, a $7E watch does NOT fire on the $00 mirror.
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x7E_0500, 0x7E_0500, false, true, false, None);
        bp.check_mem(0x00_0500, MemEventKind::Write, 0x11, 0x00_8000);
        assert!(bp.pending_hit.is_none(), "bank-exact: no mirror match");
        bp.check_mem(0x7E_0500, MemEventKind::Write, 0x11, 0x00_8000);
        assert_eq!(
            bp.take_pending().unwrap().addr,
            Some(0x7E_0500),
            "exact still hits"
        );
    }
}
