//! SA-1 chip — own 65C816 + cross-CPU IRQ / timer / DMA wiring.
//!
//! Wraps [`luna_bus::sa1::Sa1Mapper`] (which owns the shared memory
//! state — ROM, I-RAM, BW-RAM, MMIO regs, hardware multiplier, IRQ
//! latches, timer, DMA engine) and adds the SA-1's own 65C816 CPU
//! plus a `running` flag controlled by the main-CPU's writes to
//! `$2200 CCNT`:
//!
//!   * On reset, the SA-1 CPU is held in reset (CCNT.7 = 1 default).
//!   * When the main CPU clears CCNT.7 (1 → 0 edge), the SA-1 CPU
//!     loads its PC from `$2203/$2204 CRV` and starts executing.
//!   * Setting CCNT.7 back to 1 halts the SA-1 again.
//!
//! Each main-CPU master cycle, [`Sa1Chip::step_coproc`] runs roughly
//! `mclk / 6` SA-1 instructions and ticks the SA-1 timer at full
//! main-cycle resolution so timer IRQs fire even while the SA-1 CPU
//! is held in reset. The chip's [`Sa1Chip::coproc_main_irq_pending`]
//! implementation surfaces the `Sa1Mapper`'s `main_irq_line()` to
//! the host bus so the main CPU can be IRQ'd by the SA-1 directly.
//!
//! Character-conversion DMA (Type-1 / Type-2) remains the one
//! larger piece deferred — non-CC bulk DMA already works.

use luna_bus::sa1::Sa1Mapper;
use luna_bus::{
    Addr24, Bus, MCycles, Mapper, MapperKind, Sa1SideEvent, Sa1Snapshot, Sa1TraceEvent, make_addr,
};
use luna_cpu_65c816::Cpu;

/// MUTABLE save-state of a [`Sa1Chip`]: the shared-memory mapper state
/// (as its own bincode blob, ROM excluded) plus the SA-1's CPU and run
/// accounting. Trace/log fields are not part of the state.
#[derive(serde::Serialize, serde::Deserialize)]
struct Sa1ChipState {
    inner: Vec<u8>,
    cpu: Cpu,
    running: bool,
    // `deficit` is intentionally not serialized — it is a transient
    // ≤1-instruction sub-clock budget; resetting it to 0 on load costs at
    // most one SA-1 instruction of drift and keeps the save-state format
    // stable across the u32→i32 change.
}

/// SA-1 chip — a `Sa1Mapper` (shared cart memory) wrapped with its
/// own 65C816 core.
pub struct Sa1Chip {
    /// Shared memory state — also delegated through the `Mapper`
    /// trait so the main CPU can read / write it through the
    /// regular bus dispatch.
    inner: Sa1Mapper,
    /// The SA-1's own 65C816 instance.
    pub cpu: Cpu,
    /// `false` while the SA-1 is held in reset (CCNT.7 = 1).
    /// Default after construction is `false`; the main CPU starts it
    /// by clearing CCNT.7.
    pub running: bool,
    /// Signed sub-master-clock budget (mclk). Each main-CPU advance adds to
    /// it; each SA-1 instruction subtracts its **real** cost (per-access +
    /// idle SA-1 steps × 2 mclk/step). It goes slightly negative when an
    /// instruction overshoots; the overshoot carries to the next call. Not
    /// serialized — a ≤1-instruction transient, reset to 0 on load.
    deficit: i32,
    /// Optional SA-1-side execution log: when `Some`, the SA-1's own
    /// accesses to its MMIO window (`$2200-$23FF`) are recorded with the
    /// SA-1 PC. Enabled via [`Mapper::enable_sa1_side_log`].
    sa1_side_log: Option<Vec<Sa1SideEvent>>,
    /// Optional full SA-1 instruction trace: `(events, max_events)`. A
    /// pre-instruction register snapshot per SA-1 opcode, capped at
    /// `max_events`. Enabled via [`Mapper::enable_sa1_trace`].
    sa1_trace: Option<(Vec<Sa1TraceEvent>, usize)>,
}

/// One SA-1 step = 2 master clocks (SA-1 @ 10.74 MHz; ares
/// `SA1::step()` = `Thread::step(2)`).
const MCLK_PER_SA1_STEP: i32 = 2;

/// Upper bound on the carried budget (mclk). Bounds the catch-up burst if
/// a large `main_mclk` ever leaks in (the per-byte DMA tick keeps the
/// normal cadence fine-grained). ~a handful of SA-1 instructions.
const DEFICIT_CAP: i32 = 120;

impl Sa1Chip {
    /// Build a new SA-1 chip wrapping the given mapper.
    #[must_use]
    pub const fn new(inner: Sa1Mapper) -> Self {
        Self {
            inner,
            cpu: Cpu::new(),
            running: false,
            deficit: 0,
            sa1_side_log: None,
            sa1_trace: None,
        }
    }

    /// Pull the SA-1's reset vector out of the CRV register at
    /// `$2203/$2204` of the MMIO file and load it into PC.
    fn load_reset_vector(&mut self) {
        let lo = self.inner.read(make_addr(0x00, 0x2203)).unwrap_or(0);
        let hi = self.inner.read(make_addr(0x00, 0x2204)).unwrap_or(0);
        self.cpu.pc = u16::from(lo) | (u16::from(hi) << 8);
        self.cpu.pb = 0;
        self.cpu.stopped = false;
        self.cpu.waiting = false;
    }
}

impl Mapper for Sa1Chip {
    fn kind(&self) -> MapperKind {
        self.inner.kind()
    }

    /// Re-power the SA-1 on a system reset (ares `SA1::power()`): the
    /// SA-1's own 65C816 returns to power-on, the chip is held in reset
    /// again (`running = false`, CCNT.5 set by the inner mapper's
    /// `power_reset`), and the sub-clock budget is cleared. ROM and
    /// BW-RAM persist (handled by the inner mapper).
    fn reset(&mut self) {
        self.inner.power_reset();
        self.cpu = Cpu::new();
        self.running = false;
        self.deficit = 0;
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        self.inner.read(addr)
    }

    /// Writes go through to the inner mapper. We additionally watch
    /// `$2200 CCNT` for the bit-7 1 → 0 edge that releases the SA-1
    /// CPU from reset.
    fn write(&mut self, addr: Addr24, value: u8) -> bool {
        let bank = (addr >> 16) as u8;
        let offset = (addr & 0xFFFF) as u16;
        let is_ccnt = matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset == 0x2200;
        let prev_ccnt = if is_ccnt {
            self.inner.read(make_addr(0x00, 0x2200)).unwrap_or(0)
        } else {
            0
        };
        let claimed = self.inner.write(addr, value);
        if is_ccnt {
            // Per ares + Mesen2: CCNT bit 5 is the SA-1 reset bit.
            // 1 = held in reset, 0 = released. The 1 → 0 edge starts
            // the SA-1 at CRV; the 0 → 1 edge re-asserts reset.
            let was_reset = prev_ccnt & 0x20 != 0;
            let now_reset = value & 0x20 != 0;
            if was_reset && !now_reset {
                self.load_reset_vector();
                self.running = true;
                // NOTE: ares io.cpp:113 also clears CIWP=0 here. luna does
                // NOT — it deliberately deviates on the I-RAM write-
                // protection model (CIWP/SIWP default 0xFF; see
                // docs/sa1_status.md). Clearing CIWP here breaks SA-1 code
                // that doesn't pre-arm it (and is the fragile, GUI-blackout-
                // prone area flagged in that doc). Deferred until the
                // protection model is revisited holistically. (#4)
            } else if !was_reset && now_reset {
                self.running = false;
            }
        }
        claimed
    }

    fn rom_size(&self) -> usize {
        self.inner.rom_size()
    }

    fn sram_size(&self) -> usize {
        self.inner.sram_size()
    }

    fn save_state(&self) -> Vec<u8> {
        let st = Sa1ChipState {
            inner: self.inner.save_state(),
            cpu: self.cpu.clone(),
            running: self.running,
        };
        bincode::serialize(&st).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) {
        if let Ok(st) = bincode::deserialize::<Sa1ChipState>(data) {
            self.inner.load_state(&st.inner);
            self.cpu = st.cpu;
            self.running = st.running;
            self.deficit = 0; // transient; re-accrues on the next step
        }
    }

    fn step_coproc(&mut self, main_mclk: u32, scpu_mar: u32) {
        // Timer ticks even while the SA-1 CPU is held in reset — that's
        // how games can sit in CCNT.7-asserted "wait" mode and still
        // generate timer IRQs.
        self.inner.tick_timer(main_mclk);
        if !self.running {
            return;
        }
        // Add this advance to the budget, clamped so a stray large lump
        // can't trigger a runaway catch-up burst.
        self.deficit = (self.deficit.saturating_add(main_mclk as i32)).min(DEFICIT_CAP);
        while self.deficit > 0 && !self.cpu.stopped {
            // Self-referential bus borrow: `cpu`, `inner` and
            // `sa1_side_log` are disjoint fields of `Sa1Chip`, so we can
            // lend `inner`/`sa1_side_log` to the bus view while `cpu`
            // mutates itself. Snapshot the SA-1 PC before the step so the
            // optional trace can attribute each MMIO access to its code.
            let sa1_pc = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
            // Full instruction tracer: pre-opcode register snapshot. Ring
            // buffer — keeps the most recent `max` events (drops the oldest
            // half when full, amortised O(1)), so a long run captures the
            // SA-1's *current* loop (the hang) rather than early boot.
            if let Some((events, max)) = self.sa1_trace.as_mut() {
                if *max > 0 {
                    if events.len() >= *max {
                        events.drain(0..*max / 2);
                    }
                    events.push(Sa1TraceEvent {
                        pc_full: sa1_pc,
                        a: self.cpu.a,
                        x: self.cpu.x,
                        y: self.cpu.y,
                        sp: self.cpu.sp,
                        p: self.cpu.p.bits(),
                        db: self.cpu.db,
                        dp: self.cpu.dp,
                        e: self.cpu.e,
                    });
                }
            }
            // Count this instruction's real SA-1 steps: `Sa1Bus` adds the
            // per-access region cost (1, or 2 for BWRAM) on every read/write
            // and 1 per internal/idle `io_cycle`. Charge `steps × 2` mclk.
            let mut steps: u32 = 0;
            let mut bus = Sa1Bus {
                mapper: &mut self.inner,
                log: self.sa1_side_log.as_mut(),
                sa1_pc,
                steps: &mut steps,
                scpu_mar,
            };
            self.cpu.step(&mut bus);
            // Floor at 1 step so a zero-cost path can never stall the loop.
            self.deficit -= steps.max(1) as i32 * MCLK_PER_SA1_STEP;
        }
    }

    fn coproc_main_irq_pending(&self) -> bool {
        self.inner.main_irq_line()
    }

    fn sa1_snapshot(&self) -> Option<Sa1Snapshot> {
        Some(Sa1Snapshot {
            pc: self.cpu.pc,
            pb: self.cpu.pb,
            p: self.cpu.p.bits(),
            running: self.running,
        })
    }

    fn enable_sa1_side_log(&mut self) {
        if self.sa1_side_log.is_none() {
            self.sa1_side_log = Some(Vec::new());
        }
    }

    fn take_sa1_side_log(&mut self) -> Vec<Sa1SideEvent> {
        match self.sa1_side_log.as_mut() {
            Some(log) => std::mem::take(log),
            None => Vec::new(),
        }
    }

    fn enable_sa1_trace(&mut self, max_events: usize) {
        self.sa1_trace = Some((Vec::new(), max_events));
    }

    fn take_sa1_trace(&mut self) -> Vec<Sa1TraceEvent> {
        match self.sa1_trace.as_mut() {
            Some((events, _)) => std::mem::take(events),
            None => Vec::new(),
        }
    }
}

/// Bus exposed to the SA-1 CPU during one of its instruction steps.
/// Routes all accesses through the shared `Sa1Mapper` so I-RAM,
/// BW-RAM, ROM and MMIO are mutually-coherent with the main CPU's
/// view.
struct Sa1Bus<'a> {
    mapper: &'a mut Sa1Mapper,
    /// Optional SA-1-side trace sink (the chip's `sa1_side_log`).
    log: Option<&'a mut Vec<Sa1SideEvent>>,
    /// SA-1 PC at the start of the executing instruction, for the trace.
    sa1_pc: u32,
    /// Per-instruction SA-1-step accumulator (Phase 5b): each access adds
    /// its region cost, each internal `io_cycle` adds 1. `step_coproc`
    /// reads it after the step to charge the real cycle count.
    steps: &'a mut u32,
    /// S-CPU's last bus-access address (ares `cpu.r.mar`) for the duration
    /// of this SA-1 batch, used to add `conflict()` contention steps when
    /// both chips touch the same shared resource (Increment B).
    scpu_mar: u32,
}

/// SA-1 MMIO register (`$2200-$23FF`) if `addr` hits the register window.
const fn sa1_reg(addr: Addr24) -> Option<u16> {
    let off = addr as u16;
    if off >= 0x2200 && off <= 0x23FF {
        Some(off)
    } else {
        None
    }
}

/// Canonical I-RAM address (`$3000-$37FF`) if `addr` hits the SA-1's
/// view of I-RAM — either the `$3000-$37FF` window or the `$0000-$07FF`
/// direct-page mirror, in banks `$00-$3F` / `$80-$BF` (mirrors
/// [`Sa1Mapper::iram_offset_sa1`]). Used by the SA-1-side log so a trace
/// shows the SA-1's *I-RAM* writes (the cross-CPU handshake flags like
/// `$300A`/`$300E`), not just its `$2200-23FF` MMIO. Reported under the
/// `$3000+offset` alias regardless of which mirror the access used.
const fn sa1_iram_addr(addr: Addr24) -> Option<u16> {
    let bank = (addr >> 16) as u8;
    let off = addr as u16;
    let bank_ok = matches!(bank, 0x00..=0x3F | 0x80..=0xBF);
    if !bank_ok {
        return None;
    }
    if off >= 0x3000 && off <= 0x37FF {
        Some(off)
    } else if off < 0x0800 {
        Some(0x3000 + off)
    } else {
        None
    }
}

impl Bus for Sa1Bus<'_> {
    fn read(&mut self, addr: Addr24) -> u8 {
        // Charge this access's SA-1 cycle cost (Phase 5b): ROM/IRAM/IO = 1
        // step, BWRAM = 2. Covers opcode fetch + operand + data reads (the
        // core fetches via `bus.read`), mirroring ares `memory.cpp`.
        // Plus the shared-bus `conflict()` contention steps (Increment B)
        // when the S-CPU holds the same resource.
        *self.steps += u32::from(self.mapper.sa1_region_steps(addr))
            + u32::from(self.mapper.sa1_conflict_steps(addr, self.scpu_mar));
        let bank = (addr >> 16) as u8;
        let offset = (addr & 0xFFFF) as u16;
        // SA-1 vector fetches at bank 0 redirect through CRV/CNV/CIV.
        if let Some(v) = self.mapper.sa1_vector_override(bank, offset) {
            return v;
        }
        // Use the SA-1-side read path so the I-RAM mirror at
        // $00-3F/$80-BF:$0000-07FF resolves.
        let value = self.mapper.read_from_sa1(addr).unwrap_or(0xFF);
        if let (Some(log), Some(reg)) = (self.log.as_deref_mut(), sa1_reg(addr)) {
            log.push(Sa1SideEvent {
                sa1_pc: self.sa1_pc,
                write: false,
                reg,
                value,
            });
        }
        value
    }

    fn write(&mut self, addr: Addr24, value: u8) {
        // Charge this access's SA-1 cycle cost (Phase 5b) + `conflict()`
        // contention (Increment B), as in `read`.
        *self.steps += u32::from(self.mapper.sa1_region_steps(addr))
            + u32::from(self.mapper.sa1_conflict_steps(addr, self.scpu_mar));
        // Log SA-1-side writes to MMIO ($2200-23FF) AND to I-RAM
        // ($3000-37FF / $0000-07FF mirror). The I-RAM writes are the
        // cross-CPU handshake flags (e.g. Kirby's $300A/$300E) that the
        // MMIO-only log could never show — only writes are traced (reads
        // would flood the log when the SA-1 spins polling a flag).
        if let Some(log) = self.log.as_deref_mut() {
            if let Some(reg) = sa1_reg(addr).or_else(|| sa1_iram_addr(addr)) {
                log.push(Sa1SideEvent {
                    sa1_pc: self.sa1_pc,
                    write: true,
                    reg,
                    value,
                });
            }
        }
        // Route through the SA-1-side entry so I-RAM / BW-RAM
        // protection consults CIWP / CBWE instead of SIWP / SBWE.
        let _ = self.mapper.write_from_sa1(addr, value);
    }

    fn io_cycle(&mut self, _mcycles: MCycles) {
        // One internal/idle SA-1 step per call (Phase 5b). The 65c816 core
        // emits exactly one `io_cycle` per ares `idle()` step (and per WAI
        // poll), so we count calls — the passed `_mcycles` is the main-CPU
        // mclk value, irrelevant to the SA-1's own clock.
        *self.steps += 1;
    }

    fn nmi_pending(&self) -> bool {
        self.mapper.sa1_nmi_line()
    }

    fn irq_pending(&self) -> bool {
        self.mapper.sa1_irq_line()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_bus::types::make_addr;

    fn ramp_rom(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    fn sa1_chip() -> Sa1Chip {
        Sa1Chip::new(Sa1Mapper::new(ramp_rom(0x20_0000), 0x10000))
    }

    #[test]
    fn sa1_starts_reset_then_writes_to_ccnt_release_it() {
        let mut chip = sa1_chip();
        assert!(!chip.running, "default: held in reset");
        // The Sa1Mapper seeds CCNT to $80 at construction (matching
        // the real-hardware power-on state), so writing $00 produces
        // the 1→0 edge that releases the SA-1.
        chip.write(make_addr(0x00, 0x2200), 0x00);
        assert!(chip.running);
    }

    #[test]
    fn sa1_release_works_when_main_only_writes_zero_to_ccnt() {
        // Real-world boot path used by opensnes test ROMs: the main
        // CPU sets CRV via $2203/$2204, then stores `$00` to CCNT
        // *without* setting bit 7 first. Real HW boots with CCNT=$80
        // so this is the 1→0 release edge; luna must match.
        let mut chip = sa1_chip();
        chip.write(make_addr(0x00, 0x2203), 0x34);
        chip.write(make_addr(0x00, 0x2204), 0x12);
        chip.write(make_addr(0x00, 0x2200), 0x00);
        assert!(chip.running, "SA-1 must release on the first $00 write");
        assert_eq!(chip.cpu.pc, 0x1234);
    }

    #[test]
    fn sa1_reset_loads_pc_from_crv() {
        let mut chip = sa1_chip();
        // Set CRV = $1234 by writing $2203/$2204.
        chip.write(make_addr(0x00, 0x2203), 0x34);
        chip.write(make_addr(0x00, 0x2204), 0x12);
        // Now toggle CCNT.7 1→0.
        chip.write(make_addr(0x00, 0x2200), 0x80);
        chip.write(make_addr(0x00, 0x2200), 0x00);
        assert_eq!(chip.cpu.pc, 0x1234, "PC should reload from CRV");
        assert_eq!(chip.cpu.pb, 0);
    }

    #[test]
    fn sa1_step_advances_pc_when_running() {
        let mut chip = sa1_chip();
        // Place a small NOP-only program in I-RAM so we don't have
        // to fight the ROM ramp.
        chip.write(make_addr(0x00, 0x3000), 0xEA); // NOP
        chip.write(make_addr(0x00, 0x3001), 0xEA); // NOP
        chip.write(make_addr(0x00, 0x3002), 0xEA);
        chip.write(make_addr(0x00, 0x3003), 0xEA);
        chip.write(make_addr(0x00, 0x2203), 0x00); // CRV low = 0x00
        chip.write(make_addr(0x00, 0x2204), 0x30); // CRV high = 0x30 → PC = 0x3000
        chip.write(make_addr(0x00, 0x2200), 0x80); // arm reset
        chip.write(make_addr(0x00, 0x2200), 0x00); // release
        assert_eq!(chip.cpu.pc, 0x3000);
        // Run a couple of SA-1 instructions' worth of mclk — enough to
        // advance through the I-RAM NOPs without overrunning into the
        // unwritten bytes past $3003.
        chip.step_coproc(8, 0);
        assert!(chip.cpu.pc > 0x3000, "PC should have advanced past 0x3000");
    }

    #[test]
    fn sa1_step_does_nothing_while_reset_held() {
        let mut chip = sa1_chip();
        let pc_before = chip.cpu.pc;
        chip.step_coproc(1000, 0);
        assert_eq!(chip.cpu.pc, pc_before);
    }

    #[test]
    fn re_asserting_ccnt_5_halts_the_cpu() {
        // Per ares + Mesen2, CCNT bit 5 (not bit 7) is the reset bit.
        let mut chip = sa1_chip();
        // Default mmio[$2200] = $20 (bit 5 set). Clear it → release.
        chip.write(make_addr(0x00, 0x2200), 0x00);
        assert!(chip.running);
        // Set bit 5 again → SA-1 back into reset.
        chip.write(make_addr(0x00, 0x2200), 0x20);
        assert!(!chip.running);
    }

    #[test]
    fn mapper_delegation_reads_rom_through_the_inner_mapper() {
        let mut chip = sa1_chip();
        // Default CXB = 0 → $00:8000 → ROM[0].
        assert_eq!(chip.read(make_addr(0x00, 0x8000)), Some(0));
    }

    #[test]
    fn sa1_raises_main_irq_line_through_chip() {
        // SA-1 writes SCNT bit 7 (after the main CPU has enabled SIE.7);
        // `coproc_main_irq_pending()` on the chip must reflect that so
        // the SnesBus ORs it into the main CPU's IRQ line.
        let mut chip = sa1_chip();
        // S-CPU side: enable SA-1 → S-CPU IRQ.
        chip.write(make_addr(0x00, 0x2201), 0x80);
        assert!(!chip.coproc_main_irq_pending());
        // SA-1 side: assert SCNT bit 7.
        chip.write(make_addr(0x00, 0x2209), 0x80);
        assert!(
            chip.coproc_main_irq_pending(),
            "chip should expose the SA-1 → S-CPU IRQ line"
        );
        // S-CPU side: SIC bit 7 clears the latch.
        chip.write(make_addr(0x00, 0x2202), 0x80);
        assert!(!chip.coproc_main_irq_pending());
    }

    #[test]
    fn sa1_bus_irq_pending_reflects_main_to_sa1_latch() {
        let mut chip = sa1_chip();
        // SA-1 enables S-CPU → SA-1 IRQ (CIE bit 7).
        chip.write(make_addr(0x00, 0x220A), 0x80);
        // Main side asserts CCNT bit 7 (0→1 IRQ trigger). Keep bit 5
        // set so the SA-1 stays in reset for this isolated test.
        chip.write(make_addr(0x00, 0x2200), 0xA0);
        let mut sa1_steps = 0u32;
        let bus = Sa1Bus {
            mapper: &mut chip.inner,
            log: None,
            sa1_pc: 0,
            steps: &mut sa1_steps,
            scpu_mar: 0,
        };
        assert!(bus.irq_pending());
        assert!(!bus.nmi_pending());
    }

    #[test]
    fn step_coproc_advances_timer_even_with_sa1_in_reset() {
        // Even before the SA-1 CPU is released, the timer should run
        // and fire IRQs — games rely on this to gate work outside the
        // SA-1's reset window.
        let mut chip = sa1_chip();
        chip.write(make_addr(0x00, 0x220A), 0x40); // CIE bit 6 = timer
        chip.write(make_addr(0x00, 0x2210), 0x81); // linear mode, H enable
        chip.write(make_addr(0x00, 0x2212), 100); // compare lo
        chip.write(make_addr(0x00, 0x2213), 0);
        chip.write(make_addr(0x00, 0x2214), 0);
        assert!(!chip.running, "still in reset");
        // Step enough to cross 100.
        chip.step_coproc(200, 0);
        assert!(chip.inner.sa1_irq_line(), "timer should fire during reset");
    }

    #[test]
    fn dma_complete_raises_sa1_irq_via_inner_path() {
        let mut chip = sa1_chip();
        chip.write(make_addr(0x00, 0x220A), 0x20); // CIE bit 5 = DMA
        // Seed I-RAM source.
        chip.write(make_addr(0x00, 0x3100), 0x77);
        // SDA = $00:3100
        chip.write(make_addr(0x00, 0x2232), 0x00);
        chip.write(make_addr(0x00, 0x2233), 0x31);
        chip.write(make_addr(0x00, 0x2234), 0x00);
        chip.write(make_addr(0x00, 0x2238), 1);
        chip.write(make_addr(0x00, 0x2239), 0);
        // Configure DCNT first: enable + dest = BW-RAM (bit 2 = dd = 1).
        chip.write(make_addr(0x00, 0x2230), 0x84);
        // DDA = $40:0000 — the $2237 write fires the burst.
        chip.write(make_addr(0x00, 0x2235), 0x00);
        chip.write(make_addr(0x00, 0x2236), 0x00);
        chip.write(make_addr(0x00, 0x2237), 0x40);
        assert_eq!(chip.read(make_addr(0x40, 0)), Some(0x77));
        let mut sa1_steps = 0u32;
        let bus = Sa1Bus {
            mapper: &mut chip.inner,
            log: None,
            sa1_pc: 0,
            steps: &mut sa1_steps,
            scpu_mar: 0,
        };
        assert!(bus.irq_pending());
    }
}
