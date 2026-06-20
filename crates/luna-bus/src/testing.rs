//! Test-only [`Bus`] implementations.
//!
//! Gated behind the `test-utils` feature so downstream crates' tests can
//! use [`RamBus`] without paying its (tiny) cost in production builds.

use crate::bus::Bus;
use crate::types::{Addr24, MCycles};

/// Flat 16 MB RAM bus, for unit-testing CPUs and individual components.
///
/// All reads return whatever was last written (defaulting to `0`). The
/// `io_cycles_paid` field accumulates the total master cycles requested
/// via [`Bus::io_cycle`] (directly or through reads/writes), so tests can
/// verify that a CPU pays the right number of cycles for an instruction.
pub struct RamBus {
    mem: Box<[u8; 1 << 24]>,
    io_cycles_paid: MCycles,
    io_cycle_calls: u64,
    nmi: bool,
    irq: bool,
    /// Optional per-cycle bus-activity trace (opt-in via [`RamBus::enable_trace`]),
    /// one entry per cycle in execution order: `(kind, addr, value)` where
    /// `kind` is [`TraceKind`]. Reads/writes carry `addr`+`value`; internal
    /// (`io_cycle`) cycles carry `None`/`None`. Used by the Tom Harte
    /// `cycles[]` entry-for-entry oracle.
    trace: Vec<(TraceKind, Option<Addr24>, Option<u8>)>,
    record_trace: bool,
}

/// Kind of one recorded [`RamBus`] bus cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    /// A memory read (`addr`, `value` valid).
    Read,
    /// A memory write (`addr`, `value` valid).
    Write,
    /// An internal / idle cycle (`io_cycle`; no bus access).
    Internal,
}

impl Default for RamBus {
    fn default() -> Self {
        Self::new()
    }
}

impl RamBus {
    /// Build an empty RAM bus (16 MB zeroed).
    #[must_use]
    pub fn new() -> Self {
        // Box::new([0; 1 << 24]) would build the array on the stack first
        // (16 MB) and overflow it. The vec→box dance heap-allocates.
        let v = vec![0u8; 1 << 24].into_boxed_slice();
        let mem: Box<[u8; 1 << 24]> = v.try_into().expect("16 MB slice → fixed-size array");
        Self {
            mem,
            io_cycles_paid: 0,
            io_cycle_calls: 0,
            nmi: false,
            irq: false,
            trace: Vec::new(),
            record_trace: false,
        }
    }

    /// Begin recording the per-cycle bus-activity trace (cleared first).
    pub fn enable_trace(&mut self) {
        self.record_trace = true;
        self.trace.clear();
    }

    /// Take the recorded per-cycle trace, clearing it (recording continues).
    #[must_use]
    pub fn take_trace(&mut self) -> Vec<(TraceKind, Option<Addr24>, Option<u8>)> {
        std::mem::take(&mut self.trace)
    }

    /// Total master cycles paid via [`Bus::io_cycle`] since the bus was
    /// created (or the last `reset_cycle_counter` call).
    #[must_use]
    pub const fn io_cycles_paid(&self) -> MCycles {
        self.io_cycles_paid
    }

    /// Number of [`Bus::io_cycle`] invocations since the bus was created
    /// (or the last `reset_cycle_counter` call). Because the CPU cores
    /// call `io_cycle` exactly once per bus cycle (each read, each write,
    /// and each internal/idle cycle), this is the instruction's hardware
    /// cycle count — the quantity a Tom Harte `cycles[]` trace length
    /// encodes — independent of per-access mclk speed.
    #[must_use]
    pub const fn io_cycle_calls(&self) -> u64 {
        self.io_cycle_calls
    }

    /// Reset both cycle counters (mclk total and invocation count) to
    /// zero. Useful between assertions.
    pub const fn reset_cycle_counter(&mut self) {
        self.io_cycles_paid = 0;
        self.io_cycle_calls = 0;
    }

    /// Mark the NMI line as asserted (latched until cleared by the CPU
    /// via a real bus implementation reading `$4210`). In this test bus,
    /// we expose it as a simple field for explicit control.
    pub const fn set_nmi(&mut self, pending: bool) {
        self.nmi = pending;
    }

    /// Mark the IRQ line as asserted.
    pub const fn set_irq(&mut self, pending: bool) {
        self.irq = pending;
    }

    /// Direct (cost-free) read for test assertions.
    #[must_use]
    pub fn peek(&self, addr: Addr24) -> u8 {
        self.mem[addr as usize & 0x00FF_FFFF]
    }

    /// Direct (cost-free) write for test setup.
    pub fn poke(&mut self, addr: Addr24, value: u8) {
        self.mem[addr as usize & 0x00FF_FFFF] = value;
    }

    /// Bulk-load bytes at a given address. Wraps the 24-bit address space.
    pub fn poke_slice(&mut self, addr: Addr24, bytes: &[u8]) {
        let base = addr as usize & 0x00FF_FFFF;
        for (i, &b) in bytes.iter().enumerate() {
            self.mem[(base + i) & 0x00FF_FFFF] = b;
        }
    }
}

impl Bus for RamBus {
    fn read(&mut self, addr: Addr24) -> u8 {
        // Default cost in tests: 8 mclk per access (SLOW). Tests that care
        // about FAST / XSLOW should call `io_cycle` directly instead. The
        // cycle count is inlined (not via `io_cycle`) so the per-cycle trace
        // records a `Read` here and reserves `Internal` for direct
        // `io_cycle` calls only.
        self.io_cycles_paid = self.io_cycles_paid.saturating_add(8);
        self.io_cycle_calls = self.io_cycle_calls.saturating_add(1);
        let value = self.mem[addr as usize & 0x00FF_FFFF];
        if self.record_trace {
            self.trace.push((TraceKind::Read, Some(addr), Some(value)));
        }
        value
    }

    fn write(&mut self, addr: Addr24, value: u8) {
        self.io_cycles_paid = self.io_cycles_paid.saturating_add(8);
        self.io_cycle_calls = self.io_cycle_calls.saturating_add(1);
        self.mem[addr as usize & 0x00FF_FFFF] = value;
        if self.record_trace {
            self.trace.push((TraceKind::Write, Some(addr), Some(value)));
        }
    }

    fn io_cycle(&mut self, mcycles: MCycles) {
        self.io_cycles_paid = self.io_cycles_paid.saturating_add(mcycles);
        self.io_cycle_calls = self.io_cycle_calls.saturating_add(1);
        if self.record_trace {
            self.trace.push((TraceKind::Internal, None, None));
        }
    }

    fn nmi_pending(&self) -> bool {
        self.nmi
    }

    fn irq_pending(&self) -> bool {
        self.irq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_round_trip() {
        let mut bus = RamBus::new();
        bus.write(0x7E_1234, 0x42);
        assert_eq!(bus.read(0x7E_1234), 0x42);
    }

    #[test]
    fn read_pays_8_mclk() {
        let mut bus = RamBus::new();
        bus.read(0x00_0000);
        assert_eq!(bus.io_cycles_paid(), 8);
        bus.read(0x00_0001);
        assert_eq!(bus.io_cycles_paid(), 16);
    }

    #[test]
    fn io_cycle_accumulates() {
        let mut bus = RamBus::new();
        bus.io_cycle(6);
        bus.io_cycle(12);
        assert_eq!(bus.io_cycles_paid(), 18);
    }

    #[test]
    fn nmi_and_irq_lines() {
        let mut bus = RamBus::new();
        assert!(!bus.nmi_pending());
        assert!(!bus.irq_pending());
        bus.set_nmi(true);
        bus.set_irq(true);
        assert!(bus.nmi_pending());
        assert!(bus.irq_pending());
    }

    #[test]
    fn peek_does_not_charge_cycles() {
        let mut bus = RamBus::new();
        bus.poke(0x01_0000, 0xAA);
        assert_eq!(bus.peek(0x01_0000), 0xAA);
        assert_eq!(bus.io_cycles_paid(), 0);
    }

    #[test]
    fn poke_slice_writes_consecutive_bytes() {
        let mut bus = RamBus::new();
        bus.poke_slice(0x80_8000, &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(bus.peek(0x80_8000), 0xCA);
        assert_eq!(bus.peek(0x80_8001), 0xFE);
        assert_eq!(bus.peek(0x80_8002), 0xBA);
        assert_eq!(bus.peek(0x80_8003), 0xBE);
    }
}
