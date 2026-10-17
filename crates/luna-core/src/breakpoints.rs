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
    /// A break source (not a registry entry): the CPU is about to run a
    /// `BRK`.
    Brk,
    /// About to run a `COP`.
    Cop,
    /// About to run a `WDM`.
    Wdm,
    /// About to run a `STP`.
    Stp,
    /// The CPU has just entered its NMI handler.
    Nmi,
    /// The CPU has just entered its IRQ handler.
    Irq,
    /// The PPU has just reached the chosen scanline.
    Scanline,
    /// The step asked for (over, out, a scanline, N frames…) completed.
    Step,
}

/// The memory a watchpoint watches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatchSpace {
    /// The 65C816's 24-bit bus.
    #[default]
    Bus,
    /// Video RAM, byte-addressed, through `$2118`-`$2119` / `$2139`-`$213A`.
    Vram,
    /// Palette RAM, byte-addressed, through `$2122` / `$213B`.
    Cgram,
    /// Sprite memory, byte-addressed, through `$2104` / `$2138`.
    Oam,
    /// The SPC700's 64 KB of RAM (its `$F0-$FF` registers excluded).
    Aram,
    /// The SPC700's 64-byte IPL ROM, read at `$FFC0-$FFFF`.
    Ipl,
}

/// The CPU a breakpoint watches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BreakCpu {
    /// The 65C816.
    #[default]
    Snes,
    /// The SPC700.
    Spc,
    /// The SA-1's 65C816.
    Sa1,
    /// The Super FX's GSU.
    Gsu,
}

impl BreakCpu {
    /// The mask of its addresses (24-bit buses, the SPC700's 16).
    const fn mask(self) -> u32 {
        match self {
            Self::Spc => 0xFFFF,
            Self::Snes | Self::Sa1 | Self::Gsu => 0x00FF_FFFF,
        }
    }
}

/// The id a [`BreakHit`] carries when a break source, not a registered
/// breakpoint, stopped the run.
pub const BREAK_SOURCE_ID: u32 = u32::MAX;

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
    /// The CPU it watches.
    pub cpu: BreakCpu,
    /// Mem watchpoints: the memory the range is in.
    pub space: WatchSpace,
    /// The condition (an expression in Mesen2's language) that must hold
    /// for the breakpoint to fire; `None` fires on every match. The
    /// driving loop evaluates it.
    pub condition: Option<String>,
    /// A *mark*: when it fires it records an event instead of halting.
    pub mark: bool,
}

/// A breakpoint/watchpoint hit, returned to the driving run loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BreakHit {
    /// Id of the breakpoint that fired.
    pub id: u32,
    /// What fired.
    pub kind: BreakKind,
    /// The CPU it fired on: `pc` is that CPU's, `addr` on its bus or in
    /// `space`.
    #[serde(default)]
    pub cpu: BreakCpu,
    /// PC — for exec, the about-to-execute instruction; for mem, the
    /// instruction that performed the access.
    pub pc: u32,
    /// Mem hits: the accessed 24-bit bus address.
    pub addr: Option<u32>,
    /// Mem hits: the byte transferred.
    pub value: Option<u8>,
    /// Mem hits: the access was a DMA cycle.
    pub dma: bool,
    /// Mem hits: the memory `addr` is in (VRAM addresses for a VRAM
    /// watchpoint, …).
    pub space: WatchSpace,
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

/// What every breakpoint carries besides where it matches.
#[derive(Debug, Clone, Default)]
struct Extra {
    enabled: bool,
    name: Option<String>,
    condition: Option<String>,
    mark: bool,
    hits: u64,
}

/// A registered exec breakpoint.
#[derive(Debug, Clone)]
struct ExecBp {
    id: u32,
    pc: u32,
    cpu: BreakCpu,
    extra: Extra,
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
    cpu: BreakCpu,
    space: WatchSpace,
    extra: Extra,
}

/// Most candidate hits one instruction parks (a block move touches many
/// bytes; the first ones are what a debugger reports).
const PENDING_MAX: usize = 64;

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
    /// Watchpoint matches parked by the bus during an instruction, in
    /// access order, for the driving loop to check (conditions) after the
    /// instruction completes.
    pending: Vec<BreakHit>,
}

impl BreakpointSet {
    /// Fresh empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    const fn extra(name: Option<String>) -> Extra {
        Extra {
            enabled: true,
            name,
            condition: None,
            mark: false,
            hits: 0,
        }
    }

    /// Register an exec breakpoint at a 24-bit `PB:PC`, optionally named
    /// (e.g. after the symbol it was created from). Returns its id.
    pub fn add_exec(&mut self, pc: u32, name: Option<String>) -> u32 {
        self.add_exec_on(BreakCpu::Snes, pc, name)
    }

    /// Register an exec breakpoint on `cpu` (a 24-bit address for the
    /// 65C816, 16-bit for the SPC700). Returns its id.
    pub fn add_exec_on(&mut self, cpu: BreakCpu, pc: u32, name: Option<String>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.exec.push(ExecBp {
            id,
            pc: pc & cpu.mask(),
            cpu,
            extra: Self::extra(name),
        });
        id
    }

    /// The CPU breakpoint `id` watches, `None` for an unknown id.
    #[must_use]
    pub fn cpu_of(&self, id: u32) -> Option<BreakCpu> {
        if let Some(w) = self.mem.iter().find(|w| w.id == id) {
            return Some(w.cpu);
        }
        self.exec.iter().find(|b| b.id == id).map(|b| b.cpu)
    }

    /// The enabled exec breakpoints of `cpu`, as `(id, address)`.
    #[must_use]
    pub fn exec_on(&self, cpu: BreakCpu) -> Vec<(u32, u32)> {
        self.exec
            .iter()
            .filter(|b| b.cpu == cpu && b.extra.enabled)
            .map(|b| (b.id, b.pc))
            .collect()
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
        self.add_mem_in(WatchSpace::Bus, lo, hi, on_read, on_write, mirror, name)
    }

    /// [`Self::add_mem`] over a range of `space` (bus addresses for
    /// [`WatchSpace::Bus`], byte addresses in that memory otherwise).
    #[allow(clippy::too_many_arguments)]
    pub fn add_mem_in(
        &mut self,
        space: WatchSpace,
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
        let mirror = mirror && space == WatchSpace::Bus;
        self.mem.push(MemWatch {
            id,
            lo,
            hi,
            on_read,
            on_write,
            mirror,
            clo: fold_mirror(lo),
            chi: fold_mirror(hi),
            cpu: BreakCpu::Snes,
            space,
            extra: Self::extra(name),
        });
        id
    }

    /// Register a memory watchpoint of a CPU other than the 65C816 over
    /// `lo..=hi` of `space` (its bus as [`WatchSpace::Bus`]; for the
    /// SPC700 also [`WatchSpace::Aram`], [`WatchSpace::Ipl`]). The chip
    /// checks it; the registry keeps it.
    #[allow(clippy::too_many_arguments)]
    pub fn add_mem_on(
        &mut self,
        cpu: BreakCpu,
        space: WatchSpace,
        lo: u32,
        hi: u32,
        on_read: bool,
        on_write: bool,
        name: Option<String>,
    ) -> u32 {
        let id = self.add_mem_in(space, lo, hi, on_read, on_write, false, name);
        if let Some(w) = self.mem.last_mut() {
            w.cpu = cpu;
        }
        id
    }

    /// Whether any enabled watchpoint watches a PPU memory: the PPU then
    /// has to report its writes.
    #[must_use]
    pub fn has_ppu_watches(&self) -> bool {
        self.mem
            .iter()
            .any(|w| w.cpu == BreakCpu::Snes && w.space != WatchSpace::Bus && w.extra.enabled)
    }

    /// Park every enabled watchpoint a PPU memory access through a port
    /// matches (`dma` marks a DMA cycle).
    pub fn check_ppu(&mut self, access: luna_ppu::PpuAccess, pc: u32, dma: bool) {
        let space = match access.memory {
            luna_ppu::PpuMemory::Vram => WatchSpace::Vram,
            luna_ppu::PpuMemory::Cgram => WatchSpace::Cgram,
            luna_ppu::PpuMemory::Oam => WatchSpace::Oam,
        };
        let addr = u32::from(access.addr);
        let kind = if access.write {
            BreakKind::Write
        } else {
            BreakKind::Read
        };
        for w in &self.mem {
            if w.cpu == BreakCpu::Snes
                && w.space == space
                && w.extra.enabled
                && (if access.write { w.on_write } else { w.on_read })
                && (w.lo..=w.hi).contains(&addr)
                && self.pending.len() < PENDING_MAX
            {
                self.pending.push(BreakHit {
                    id: w.id,
                    kind,
                    cpu: BreakCpu::Snes,
                    pc,
                    addr: Some(addr),
                    value: Some(access.value),
                    dma,
                    space,
                });
            }
        }
    }

    fn extra_mut(&mut self, id: u32) -> Option<&mut Extra> {
        self.exec
            .iter_mut()
            .find(|b| b.id == id)
            .map(|b| &mut b.extra)
            .or_else(|| {
                self.mem
                    .iter_mut()
                    .find(|w| w.id == id)
                    .map(|w| &mut w.extra)
            })
    }

    /// Enable / disable a breakpoint without removing it (issue #176).
    /// Returns `true` if the id exists.
    pub fn set_enabled(&mut self, id: u32, enabled: bool) -> bool {
        self.extra_mut(id).map(|e| e.enabled = enabled).is_some()
    }

    /// Set or clear a breakpoint's condition. Returns `true` if the id
    /// exists.
    pub fn set_condition(&mut self, id: u32, condition: Option<String>) -> bool {
        self.extra_mut(id)
            .map(|e| e.condition = condition)
            .is_some()
    }

    /// Make a breakpoint a mark (it records an event instead of halting)
    /// or a plain one again. Returns `true` if the id exists.
    pub fn set_mark(&mut self, id: u32, mark: bool) -> bool {
        self.extra_mut(id).map(|e| e.mark = mark).is_some()
    }

    /// Whether breakpoint `id` is a mark.
    #[must_use]
    pub fn is_mark(&self, id: u32) -> bool {
        self.exec
            .iter()
            .find(|b| b.id == id)
            .map(|b| &b.extra)
            .or_else(|| self.mem.iter().find(|w| w.id == id).map(|w| &w.extra))
            .is_some_and(|e| e.mark)
    }

    /// Count one firing of breakpoint `id` (the driving loop calls it once
    /// a hit passed its condition).
    pub fn count_hit(&mut self, id: u32) {
        if let Some(e) = self.extra_mut(id) {
            e.hits += 1;
        }
    }

    /// Remove a breakpoint by id. Returns `true` if it existed.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.exec.len() + self.mem.len();
        self.exec.retain(|b| b.id != id);
        self.mem.retain(|w| w.id != id);
        before != self.exec.len() + self.mem.len()
    }

    /// Remove every breakpoint and any parked hit.
    pub fn clear(&mut self) {
        self.exec.clear();
        self.mem.clear();
        self.pending.clear();
    }

    /// No breakpoint registered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.exec.is_empty() && self.mem.is_empty()
    }

    /// Every registered breakpoint, by id.
    #[must_use]
    pub fn list(&self) -> Vec<BreakpointInfo> {
        let info =
            |id, cpu, space, exec, lo, hi, on_read, on_write, mirror, e: &Extra| BreakpointInfo {
                id,
                cpu,
                space,
                exec,
                lo,
                hi,
                on_read,
                on_write,
                enabled: e.enabled,
                hit_count: e.hits,
                mirror,
                name: e.name.clone(),
                condition: e.condition.clone(),
                mark: e.mark,
            };
        let mut out: Vec<BreakpointInfo> = self
            .exec
            .iter()
            .map(|b| {
                info(
                    b.id,
                    b.cpu,
                    WatchSpace::Bus,
                    true,
                    b.pc,
                    b.pc,
                    false,
                    false,
                    false,
                    &b.extra,
                )
            })
            .chain(self.mem.iter().map(|w| {
                let (lo, hi) = (w.lo, w.hi);
                info(
                    w.id, w.cpu, w.space, false, lo, hi, w.on_read, w.on_write, w.mirror, &w.extra,
                )
            }))
            .collect();
        out.sort_by_key(|b| b.id);
        out
    }

    /// The enabled exec breakpoints at `pc`, in registration order (empty,
    /// without allocating, when none matches).
    #[must_use]
    pub fn check_exec(&self, pc: u32) -> Vec<BreakHit> {
        self.exec
            .iter()
            .filter(|b| b.cpu == BreakCpu::Snes && b.extra.enabled && b.pc == pc)
            .map(|b| BreakHit {
                id: b.id,
                kind: BreakKind::Exec,
                cpu: BreakCpu::Snes,
                pc,
                addr: None,
                value: None,
                dma: false,
                space: WatchSpace::Bus,
            })
            .collect()
    }

    /// Park every enabled watchpoint this access matches. Called by the
    /// bus on each access; `dma` marks a DMA cycle.
    pub fn check_mem(&mut self, addr: u32, kind: MemEventKind, value: u8, pc: u32, dma: bool) {
        if self.mem.is_empty() || self.pending.len() >= PENDING_MAX {
            return;
        }
        let (is_read, break_kind) = match kind {
            MemEventKind::Read => (true, BreakKind::Read),
            MemEventKind::Write => (false, BreakKind::Write),
            // Interrupt-delivery markers are trace annotations, not accesses.
            MemEventKind::NmiSignal | MemEventKind::IrqSignal => return,
        };
        let faddr = fold_mirror(addr);
        for w in &self.mem {
            let kind_ok = if is_read { w.on_read } else { w.on_write };
            if w.cpu == BreakCpu::Snes
                && w.space == WatchSpace::Bus
                && w.extra.enabled
                && kind_ok
                && ((w.lo..=w.hi).contains(&addr)
                    || (w.mirror && w.clo <= w.chi && (w.clo..=w.chi).contains(&faddr)))
                && self.pending.len() < PENDING_MAX
            {
                self.pending.push(BreakHit {
                    id: w.id,
                    kind: break_kind,
                    cpu: BreakCpu::Snes,
                    pc,
                    addr: Some(addr),
                    value: Some(value),
                    dma,
                    space: WatchSpace::Bus,
                });
            }
        }
    }

    /// The watchpoint matches parked since the last call, in access order.
    pub fn take_pending(&mut self) -> Vec<BreakHit> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one parked hit of an access.
    fn one(bp: &mut BreakpointSet) -> Option<BreakHit> {
        let v = bp.take_pending();
        assert!(v.len() <= 1, "{v:?}");
        v.into_iter().next()
    }

    #[test]
    fn exec_breakpoint_matches_exact_pc() {
        let mut bp = BreakpointSet::new();
        let id = bp.add_exec(0x00_8012, None);
        assert!(bp.check_exec(0x00_8011).is_empty());
        let hit = bp.check_exec(0x00_8012)[0];
        assert_eq!((hit.id, hit.kind), (id, BreakKind::Exec));
        assert_eq!(hit.pc, 0x00_8012);
    }

    #[test]
    fn mem_watchpoint_matches_range_and_kind() {
        let mut bp = BreakpointSet::new();
        let id = bp.add_mem(0x7E_1000, 0x7E_10FF, false, true, false, None); // write-only
        bp.check_mem(0x7E_1080, MemEventKind::Read, 0xAA, 0x00_8000, false);
        assert!(one(&mut bp).is_none(), "a read does not trip a write watch");
        bp.check_mem(0x7E_1100, MemEventKind::Write, 0xAA, 0x00_8000, false);
        assert!(one(&mut bp).is_none(), "outside the range");
        bp.check_mem(0x7E_1080, MemEventKind::Write, 0xAA, 0x00_8000, true);
        let hit = one(&mut bp).unwrap();
        assert_eq!(hit.id, id);
        assert_eq!(hit.kind, BreakKind::Write);
        assert_eq!(hit.addr, Some(0x7E_1080));
        assert_eq!(hit.value, Some(0xAA));
        assert_eq!(hit.pc, 0x00_8000);
        assert!(hit.dma);
    }

    #[test]
    fn every_match_of_an_instruction_is_parked_in_order() {
        let mut bp = BreakpointSet::new();
        let a = bp.add_mem(0x7E_0000, 0x7E_FFFF, true, true, false, None);
        let b = bp.add_mem(0x7E_0002, 0x7E_0002, false, true, false, None);
        bp.check_mem(0x7E_0001, MemEventKind::Write, 1, 0, false);
        bp.check_mem(0x7E_0002, MemEventKind::Write, 2, 0, false);
        let v: Vec<(u32, Option<u32>)> = bp.take_pending().iter().map(|h| (h.id, h.addr)).collect();
        assert_eq!(
            v,
            [
                (a, Some(0x7E_0001)),
                (a, Some(0x7E_0002)),
                (b, Some(0x7E_0002))
            ]
        );
        assert!(bp.take_pending().is_empty());
        // A block move cannot park without bound.
        for i in 0..1000 {
            bp.check_mem(0x7E_1000 + i, MemEventKind::Write, 0, 0, false);
        }
        assert_eq!(bp.take_pending().len(), PENDING_MAX);
    }

    #[test]
    fn interrupt_markers_never_trip_watchpoints() {
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x00_0000, 0xFF_FFFF, true, true, false, None);
        bp.check_mem(0x00_FFEA, MemEventKind::NmiSignal, 0, 0, false);
        bp.check_mem(0x00_FFEE, MemEventKind::IrqSignal, 0, 0, false);
        assert!(one(&mut bp).is_none());
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
    fn disabled_breakpoints_never_match_and_keep_state() {
        let mut bp = BreakpointSet::new();
        let x = bp.add_exec(0x00_8000, Some("main".into()));
        let m = bp.add_mem(0x7E_0100, 0x7E_0100, false, true, false, None);
        assert!(bp.set_condition(x, Some("a == 1".into())));
        assert!(bp.set_mark(m, true));
        bp.count_hit(x);
        assert!(bp.set_enabled(x, false));
        assert!(bp.set_enabled(m, false));
        assert!(bp.check_exec(0x00_8000).is_empty());
        bp.check_mem(0x7E_0100, MemEventKind::Write, 2, 0, false);
        assert!(one(&mut bp).is_none());
        let list = bp.list();
        assert!(list.iter().all(|b| !b.enabled));
        assert_eq!(list[0].hit_count, 1);
        assert_eq!(list[0].name.as_deref(), Some("main"));
        assert_eq!(list[0].condition.as_deref(), Some("a == 1"));
        assert!(list[1].mark && !list[0].mark);
        assert!(bp.set_enabled(x, true));
        assert_eq!(bp.check_exec(0x00_8000).len(), 1);
        assert!(!bp.set_enabled(999, true), "unknown id");
    }

    #[test]
    fn hit_counts_are_what_the_driver_counts() {
        let mut bp = BreakpointSet::new();
        let a = bp.add_exec(0x00_8000, None);
        let b = bp.add_exec(0x00_8000, None);
        assert_eq!(bp.check_exec(0x00_8000).len(), 2, "both match");
        for _ in 0..3 {
            bp.count_hit(a);
        }
        let list = bp.list();
        assert_eq!(list.iter().find(|e| e.id == a).unwrap().hit_count, 3);
        assert_eq!(
            list.iter().find(|e| e.id == b).unwrap().hit_count,
            0,
            "matching is not firing"
        );
    }

    #[test]
    fn mirror_watch_folds_wram_low_page() {
        // Watch a WRAM-low variable via $7E; a mirror access through $00/$80
        // must fire, but $7F (different WRAM half) and WRAM-high must not.
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x7E_0500, 0x7E_0500, false, true, true, None);
        bp.check_mem(0x00_0500, MemEventKind::Write, 0x11, 0x00_8000, false);
        assert_eq!(one(&mut bp).unwrap().addr, Some(0x00_0500), "$00 mirror");
        bp.check_mem(0x80_0500, MemEventKind::Write, 0x22, 0x00_8000, false);
        assert_eq!(one(&mut bp).unwrap().addr, Some(0x80_0500), "$80 mirror");
        bp.check_mem(0x7F_0500, MemEventKind::Write, 0x33, 0x00_8000, false);
        assert!(one(&mut bp).is_none(), "$7F is not a mirror of $7E-low");
    }

    #[test]
    fn mirror_watch_folds_mmio_across_banks() {
        // Watch $00:2100 (INIDISP); a FastROM access via $80:2100 must fire.
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x00_2100, 0x00_2100, false, true, true, None);
        bp.check_mem(0x80_2100, MemEventKind::Write, 0x8F, 0x80_8000, false);
        assert_eq!(one(&mut bp).unwrap().addr, Some(0x80_2100));
    }

    #[test]
    fn mirror_off_stays_bank_exact() {
        let mut bp = BreakpointSet::new();
        bp.add_mem(0x7E_0500, 0x7E_0500, false, true, false, None);
        bp.check_mem(0x00_0500, MemEventKind::Write, 0x11, 0x00_8000, false);
        assert!(one(&mut bp).is_none(), "bank-exact: no mirror match");
        bp.check_mem(0x7E_0500, MemEventKind::Write, 0x11, 0x00_8000, false);
        assert_eq!(
            one(&mut bp).unwrap().addr,
            Some(0x7E_0500),
            "exact still hits"
        );
    }
}
