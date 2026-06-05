//! [`Dma`] — the 8-channel SNES DMA controller.
//!
//! Owns the array of channels and the `MDMAEN` / `HDMAEN` register
//! semantics. `Dma::run_mdma` is the entry point invoked by the bus
//! when software writes `$420B`.

use super::bus::DmaBus;
use super::channel::DmaChannel;

/// One DMA byte landing in VRAM (`$2118`/`$2119`), captured at transfer
/// time. The byte value is the one the DMA actually read from the
/// source this instant — immune to a coprocessor (e.g. Super FX)
/// overwriting its source buffer between the transfer and a later VRAM
/// dump (the double-buffer confound).
#[derive(Debug, Clone, Copy)]
pub struct DmaTraceEvent {
    /// 24-bit A-bus source address of this byte.
    pub src_full: u32,
    /// PPU VRAM word address (`$2116/7` VMADD) the byte targets.
    pub vram_word: u16,
    /// B-bus register: `0x18` (`$2118`, low byte) or `0x19` (`$2119`, high).
    pub b_offset: u8,
    /// The transferred byte.
    pub value: u8,
}

/// Bounded ring for the DMA→VRAM transfer-time tracer.
#[derive(Default)]
pub struct DmaTraceLog {
    /// Recorded VRAM-write events, in transfer order.
    pub events: Vec<DmaTraceEvent>,
    /// Hard cap on event count.
    pub max_events: usize,
}

/// The SNES DMA controller — 8 channels + a pair of global registers.
#[derive(Default)]
pub struct Dma {
    /// The eight DMA channels (indexed 0-7).
    pub channels: [DmaChannel; 8],
    /// `$420B MDMAEN` — last written value. Reading it returns 0 (write
    /// only). The fast path is to call [`Dma::run_mdma`] directly with
    /// the value being written.
    pub mdmaen: u8,
    /// `$420C HDMAEN` — HDMA enable mask. Stored but not yet acted upon
    /// (HDMA is in a later phase).
    pub hdmaen: u8,
    /// Optional DMA→VRAM transfer-time trace. `None` = disabled. The
    /// bus moves this into the per-transfer [`DmaBus`] view so the
    /// view's `$2118/9` writes can record (source → VMADD → byte).
    pub dma_trace: Option<DmaTraceLog>,
}

impl Dma {
    /// Build a fresh controller (all channels zeroed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable the DMA→VRAM transfer-time trace, capped at `max_events`.
    pub fn enable_dma_trace(&mut self, max_events: usize) {
        self.dma_trace = Some(DmaTraceLog {
            events: Vec::new(),
            max_events,
        });
    }

    /// Drain the captured DMA→VRAM trace (leaves tracing enabled but
    /// empty). Returns an empty vec if tracing was never enabled.
    pub fn take_dma_trace(&mut self) -> Vec<DmaTraceEvent> {
        match self.dma_trace.as_mut() {
            Some(log) => std::mem::take(&mut log.events),
            None => Vec::new(),
        }
    }

    /// Read a channel register at the 24-bit B-bus(-style) offset
    /// inside `$4300-$437F`. Returns `None` if the address is outside
    /// the DMA register window.
    #[must_use]
    pub fn read_register(&self, offset: u16) -> Option<u8> {
        if !(0x4300..=0x437F).contains(&offset) {
            return None;
        }
        let channel = ((offset >> 4) & 0x07) as usize;
        let reg = (offset & 0x0F) as u8;
        Some(self.channels[channel].read(reg))
    }

    /// Write a channel register.
    pub fn write_register(&mut self, offset: u16, value: u8) -> bool {
        if !(0x4300..=0x437F).contains(&offset) {
            return false;
        }
        let channel = ((offset >> 4) & 0x07) as usize;
        let reg = (offset & 0x0F) as u8;
        self.channels[channel].write(reg, value);
        true
    }

    /// Execute the sync-DMA pass triggered by a write of `mask` to
    /// `$420B MDMAEN`. Channels with their bit set in `mask` run in
    /// ascending order; each transfers `das` bytes (or 64 KB if
    /// `das == 0`). Returns the total number of bytes transferred
    /// across all triggered channels (useful for cycle counting in
    /// later phases).
    pub fn run_mdma<B: DmaBus>(&mut self, bus: &mut B, mask: u8) -> u32 {
        self.mdmaen = mask;
        let mut total = 0u32;
        for ch in 0..8 {
            if mask & (1 << ch) != 0 {
                total += self.channels[ch].run(bus);
            }
        }
        total
    }

    /// Frame-start HDMA initialisation. Called once per frame
    /// (typically at the entry of the pre-render scanline). For each
    /// channel whose bit is set in `$420C HDMAEN`, copies the table
    /// start pointer, reads the first header byte, and in indirect
    /// mode the first data pointer. Channels not enabled in
    /// [`Self::hdmaen`] are left untouched.
    /// Returns the master-cycle cost of the frame-start setup so the CPU
    /// can be charged the stall (Phase 4). Per ares `cpu/dma.cpp`
    /// `hdmaSetup`: `step(8)` overhead + one header read per enabled
    /// channel; folded here into the canonical `18 + 8·channels` figure.
    pub fn hdma_init<B: DmaBus>(&mut self, bus: &mut B) -> u32 {
        let mut enabled = 0u32;
        for ch in 0..8 {
            if self.hdmaen & (1 << ch) != 0 {
                self.channels[ch].hdma_start_frame(bus);
                enabled += 1;
            } else {
                self.channels[ch].hdma_active = false;
                self.channels[ch].hdma_do_transfer = false;
            }
        }
        if enabled > 0 {
            HDMA_OVERHEAD_MCLK + 8 * enabled
        } else {
            0
        }
    }

    /// Per-scanline HDMA step. Called once per visible scanline
    /// (lines 0..=224 NTSC). Each enabled, still-active channel fires up
    /// to one mode-pattern's worth of bytes through its configured B-bus
    /// offset. Returns the **master-cycle cost** of the line's HDMA so the
    /// CPU can be charged the stall (Phase 4): `18 + 8·bytes` when any
    /// channel was active, else 0 (ares `cpu/dma.cpp` `hdmaRun`: `step(8)`
    /// overhead + 8 mclk per transferred byte, folded into the canonical
    /// 18-mclk per-scanline overhead).
    pub fn hdma_run_line<B: DmaBus>(&mut self, bus: &mut B) -> u32 {
        let mut bytes = 0u32;
        let mut any_active = false;
        for ch in 0..8 {
            if self.hdmaen & (1 << ch) != 0 {
                // A channel active at line start does work this line.
                any_active |= self.channels[ch].hdma_active;
                bytes += self.channels[ch].hdma_step_line(bus);
            }
        }
        if any_active {
            HDMA_OVERHEAD_MCLK + 8 * bytes
        } else {
            0
        }
    }
}

/// Fixed per-scanline HDMA overhead in master cycles when ≥1 channel is
/// active — the canonical hardware figure (anomie / bsnes / higan),
/// folding ares' `step(8)` + DMA-clock alignment + reload reads.
const HDMA_OVERHEAD_MCLK: u32 = 18;

#[cfg(test)]
mod tests {
    use super::super::bus::DmaBus;
    use super::super::channel::DmaParams;
    use super::*;

    struct MockBus {
        a: Vec<u8>,
        b: Vec<u8>,
    }

    impl MockBus {
        fn new() -> Self {
            Self {
                a: vec![0; 0x100_0000],
                b: vec![0; 0x100],
            }
        }
    }

    impl DmaBus for MockBus {
        fn read_a(&mut self, addr: u32) -> u8 {
            self.a[(addr as usize) & 0xFF_FFFF]
        }
        fn write_a(&mut self, addr: u32, value: u8) {
            self.a[(addr as usize) & 0xFF_FFFF] = value;
        }
        fn read_b(&mut self, b_offset: u8) -> u8 {
            self.b[b_offset as usize]
        }
        fn write_b(&mut self, b_offset: u8, value: u8) {
            self.b[b_offset as usize] = value;
        }
    }

    #[test]
    fn register_write_routes_to_correct_channel() {
        let mut dma = Dma::new();
        // Write 0x42 to $4302 (channel 0, register 2 = A1TL) and 0x84
        // to $4312 (channel 1, register 2 = A1TL).
        dma.write_register(0x4302, 0x42);
        dma.write_register(0x4312, 0x84);
        assert_eq!(dma.channels[0].a_addr & 0xFF, 0x42);
        assert_eq!(dma.channels[1].a_addr & 0xFF, 0x84);
    }

    #[test]
    fn read_register_outside_range_returns_none() {
        let dma = Dma::new();
        assert!(dma.read_register(0x4200).is_none());
        assert!(dma.read_register(0x4380).is_none());
    }

    #[test]
    fn mdma_runs_only_the_masked_channels() {
        let mut bus = MockBus::new();
        // Channel 0 will copy from $7E:1000 (4 bytes) → $2122.
        bus.a[0x7E_1000] = 0x11;
        bus.a[0x7E_1001] = 0x22;
        bus.a[0x7E_1002] = 0x33;
        bus.a[0x7E_1003] = 0x44;
        // Channel 1 should NOT run — we'll leave it pointing at junk
        // and verify the masked-out bit is honoured.
        let mut dma = Dma::new();
        dma.channels[0].params = DmaParams::from_byte(0); // mode 0, +1, A→B
        dma.channels[0].bbad = 0x22;
        dma.channels[0].a_addr = 0x1000;
        dma.channels[0].a_bank = 0x7E;
        dma.channels[0].das = 4;
        dma.channels[1].bbad = 0xFF; // would write to $21FF if it ran

        let n = dma.run_mdma(&mut bus, 0b0000_0001);
        assert_eq!(n, 4, "only channel 0 transferred");
        assert_eq!(bus.b[0x22], 0x44, "last byte landed at $2122");
        assert_eq!(bus.b[0xFF], 0x00, "channel 1 did not run");
    }

    #[test]
    fn hdma_charges_overhead_plus_per_byte_stall() {
        // Phase 4 HDMA time cost: 18 mclk/scanline overhead when any
        // channel is active, + 8 mclk per transferred byte. Table
        // `02 11 00` = non-repeat 2-line entry (transfer line 1, gap
        // line 2), then terminator. Mode 0 (1 byte/line) → BBAD $22.
        let mut bus = MockBus::new();
        bus.a[0x00_2000] = 0x02; // line count
        bus.a[0x00_2001] = 0x11; // data byte
        bus.a[0x00_2002] = 0x00; // terminator
        let mut dma = Dma::new();
        dma.channels[0].params = DmaParams::from_byte(0); // mode 0, direct
        dma.channels[0].bbad = 0x22;
        dma.channels[0].a_addr = 0x2000;
        dma.channels[0].a_bank = 0x00;
        dma.hdmaen = 0b0000_0001; // channel 0 HDMA enabled

        // Frame setup: 18 overhead + 8 per enabled channel (1).
        assert_eq!(dma.hdma_init(&mut bus), HDMA_OVERHEAD_MCLK + 8);

        // Line 1 transfers 1 byte → 18 + 8.
        assert_eq!(dma.hdma_run_line(&mut bus), HDMA_OVERHEAD_MCLK + 8);
        assert_eq!(bus.b[0x22], 0x11);

        // Line 2 is an active gap (still active at line start, 0 bytes,
        // reads the terminator) → overhead only.
        assert_eq!(dma.hdma_run_line(&mut bus), HDMA_OVERHEAD_MCLK);

        // Channel terminated → no cost on subsequent lines.
        assert_eq!(dma.hdma_run_line(&mut bus), 0);
    }

    #[test]
    fn hdma_with_no_enabled_channels_costs_nothing() {
        let mut bus = MockBus::new();
        let mut dma = Dma::new(); // hdmaen == 0
        assert_eq!(dma.hdma_init(&mut bus), 0);
        assert_eq!(dma.hdma_run_line(&mut bus), 0);
    }

    #[test]
    fn mdma_runs_channels_in_ascending_order() {
        // Two channels write to the SAME B-bus address. Whichever runs
        // LAST wins. With mask = 0b11, we expect channel 1 to overwrite
        // channel 0's result.
        let mut bus = MockBus::new();
        bus.a[0x7E_1000] = 0xAA;
        bus.a[0x7E_2000] = 0xBB;
        let mut dma = Dma::new();
        for ch in &mut [0usize, 1] {
            dma.channels[*ch].params = DmaParams::from_byte(0);
            dma.channels[*ch].bbad = 0x22;
            dma.channels[*ch].a_bank = 0x7E;
            dma.channels[*ch].das = 1;
        }
        dma.channels[0].a_addr = 0x1000;
        dma.channels[1].a_addr = 0x2000;

        dma.run_mdma(&mut bus, 0b11);
        assert_eq!(
            bus.b[0x22], 0xBB,
            "channel 1 ran after channel 0 and overwrote its byte"
        );
    }
}
