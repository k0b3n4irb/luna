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
use luna_bus::{Addr24, Bus, MCycles, Mapper, MapperKind, Sa1SideEvent, Sa1Snapshot, make_addr};
use luna_cpu_65c816::Cpu;

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
    /// Sub-master-clock budget. SA-1 runs at roughly 6 mclk per
    /// instruction-equivalent (10.74 MHz × ~1.7 cycles/instr ≈ 6
    /// SNES master cycles). We accumulate and dispatch one
    /// instruction per `MCLK_PER_SA1_INSN` units.
    deficit: u32,
    /// Optional SA-1-side execution log: when `Some`, the SA-1's own
    /// accesses to its MMIO window (`$2200-$23FF`) are recorded with the
    /// SA-1 PC. Enabled via [`Mapper::enable_sa1_side_log`].
    sa1_side_log: Option<Vec<Sa1SideEvent>>,
}

const MCLK_PER_SA1_INSN: u32 = 6;

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

    fn step_coproc(&mut self, main_mclk: u32) {
        // Timer ticks even while the SA-1 CPU is held in reset — that's
        // how games can sit in CCNT.7-asserted "wait" mode and still
        // generate timer IRQs.
        self.inner.tick_timer(main_mclk);
        if !self.running {
            return;
        }
        self.deficit = self.deficit.saturating_add(main_mclk);
        while self.deficit >= MCLK_PER_SA1_INSN && !self.cpu.stopped {
            self.deficit -= MCLK_PER_SA1_INSN;
            // Self-referential bus borrow: `cpu`, `inner` and
            // `sa1_side_log` are disjoint fields of `Sa1Chip`, so we can
            // lend `inner`/`sa1_side_log` to the bus view while `cpu`
            // mutates itself. Snapshot the SA-1 PC before the step so the
            // optional trace can attribute each MMIO access to its code.
            let sa1_pc = (u32::from(self.cpu.pb) << 16) | u32::from(self.cpu.pc);
            let mut bus = Sa1Bus {
                mapper: &mut self.inner,
                log: self.sa1_side_log.as_mut(),
                sa1_pc,
            };
            self.cpu.step(&mut bus);
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

impl Bus for Sa1Bus<'_> {
    fn read(&mut self, addr: Addr24) -> u8 {
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
        if let (Some(log), Some(reg)) = (self.log.as_deref_mut(), sa1_reg(addr)) {
            log.push(Sa1SideEvent {
                sa1_pc: self.sa1_pc,
                write: true,
                reg,
                value,
            });
        }
        // Route through the SA-1-side entry so I-RAM / BW-RAM
        // protection consults CIWP / CBWE instead of SIWP / SBWE.
        let _ = self.mapper.write_from_sa1(addr, value);
    }

    fn io_cycle(&mut self, _mcycles: MCycles) {
        // No scheduler hookup on the SA-1 side yet — internal cycles
        // are accounted via the main-CPU's mclk budget in
        // `Sa1Chip::step_coproc`.
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
        // Run enough mclk for at least one instruction.
        chip.step_coproc(MCLK_PER_SA1_INSN * 4);
        assert!(chip.cpu.pc > 0x3000, "PC should have advanced past 0x3000");
    }

    #[test]
    fn sa1_step_does_nothing_while_reset_held() {
        let mut chip = sa1_chip();
        let pc_before = chip.cpu.pc;
        chip.step_coproc(1000);
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
        let bus = Sa1Bus {
            mapper: &mut chip.inner,
            log: None,
            sa1_pc: 0,
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
        chip.step_coproc(200);
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
        let bus = Sa1Bus {
            mapper: &mut chip.inner,
            log: None,
            sa1_pc: 0,
        };
        assert!(bus.irq_pending());
    }
}
