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
    /// B-bus register offset: the byte targets `$2100 + b_offset` (e.g.
    /// `0x18`/`0x19` = `$2118`/`$2119` VRAM; `0x04` = `$2104` OAM).
    pub b_offset: u8,
    /// The transferred byte.
    pub value: u8,
    /// DMA channel (0-7) that performed this transfer — Mesen2's
    /// `DebugEventInfo::DmaChannel` (read from `dma->GetActiveChannel()`),
    /// driving the Event Viewer's per-channel filter.
    pub channel: u8,
    /// Completed-frame counter at the start of the owning DMA burst — lets a
    /// consumer bucket DMA→VRAM bytes by frame (the per-VBlank budget check).
    pub frame: u64,
    /// PPU scanline at the start of the owning burst.
    pub line: u16,
    /// PPU dot — the H position at the transfer, derived from the master
    /// clock (the Event Viewer plots events at `(dot, line)`).
    pub dot: u16,
    /// `true` if the burst started in the vertical-blank window
    /// (`line >= vblank_start`).
    pub blank: bool,
    /// `true` if INIDISP (`$2100`) forced-blank (bit 7) was set at the write.
    /// A VRAM write is safe iff `blank || force_blank` (V-blank *or* forced
    /// blank); otherwise it races active display and the PPU drops it.
    pub force_blank: bool,
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
#[derive(Default, serde::Serialize, serde::Deserialize)]
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
    ///
    /// Diagnostic only — not part of the save-state (`serde(skip)` →
    /// defaults to `None` on restore).
    #[serde(skip)]
    pub dma_trace: Option<DmaTraceLog>,
    /// In-progress sync-DMA cursor (Phase 5): the masked channel set and
    /// the channel currently mid-transfer, so a burst can be driven in
    /// scanline-bounded segments. `None` = no sync DMA in flight.
    /// Transient run-state — never saved (a state is only taken at a
    /// frame boundary, never mid-DMA).
    #[serde(skip)]
    pub mdma_cursor: Option<MdmaCursor>,
}

/// Resume state for a segmented sync DMA (Phase 5). The per-channel
/// progress lives on each [`DmaChannel`]; this only tracks which channels
/// were triggered and which one the driver is currently servicing.
#[derive(Debug, Clone, Copy)]
pub struct MdmaCursor {
    /// The `$420B` channel mask for this burst.
    pub mask: u8,
    /// Next channel index (0..=8) to service; `8` = burst complete.
    pub current_ch: u8,
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
        // One-shot wrapper: an unbounded budget runs the whole burst in a
        // single call and clears the cursor (the legacy lump behaviour).
        self.run_mdma_segment(bus, mask, u32::MAX)
    }

    /// Transfer at most `max_bytes` of the sync DMA triggered by `mask`,
    /// resuming across calls (Phase 5). The first call on an idle
    /// controller latches the cursor from `mask`; subsequent calls
    /// continue the same burst (the passed `mask` is ignored while a
    /// cursor is live). Channels run in ascending order; a channel is
    /// serviced to completion before advancing. Returns the bytes
    /// transferred *this* call; when the burst finishes,
    /// [`Self::mdma_cursor`] is cleared back to `None`.
    pub fn run_mdma_segment<B: DmaBus>(&mut self, bus: &mut B, mask: u8, max_bytes: u32) -> u32 {
        if self.mdma_cursor.is_none() {
            self.mdmaen = mask;
            self.mdma_cursor = Some(MdmaCursor {
                mask,
                current_ch: 0,
            });
        }
        let mut budget = max_bytes;
        let mut total = 0u32;
        // Work the masked channels in order, within this call's budget.
        loop {
            let cur = self.mdma_cursor.expect("cursor set above");
            if cur.current_ch >= 8 || budget == 0 {
                break;
            }
            let ch = cur.current_ch as usize;
            if cur.mask & (1 << ch) != 0 {
                // Tag the bus view with the active channel so its B-bus
                // writes carry the channel (Mesen2 `dma->GetActiveChannel()`).
                bus.set_active_channel(ch as u8);
                let done = self.channels[ch].run_segment(bus, budget);
                total += done;
                budget -= done;
                if self.channels[ch].seg_running {
                    // Hit the budget mid-channel — resume this channel on
                    // the next call (don't advance the cursor).
                    break;
                }
            }
            // Channel finished (or not masked): advance to the next.
            self.mdma_cursor.as_mut().expect("cursor").current_ch += 1;
        }
        if self.mdma_cursor.is_some_and(|c| c.current_ch >= 8) {
            self.mdma_cursor = None;
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
            // Re-arm the lazy-start latch each frame; a channel enabled
            // *mid-frame* (HDMAEN bit set after this init) is set up on its
            // first active line in `hdma_run_line`.
            self.channels[ch].hdma_started = false;
            if self.hdmaen & (1 << ch) != 0 {
                self.channels[ch].hdma_start_frame(bus);
                self.channels[ch].hdma_started = true;
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
                // Mid-frame enable (HDMAEN bit set after `hdma_init`, e.g.
                // Yoshi's Island's text-band split at scanline ~12): set the
                // channel up now so it begins from its source address — ares
                // gates `hdmaRun` on the live `hdmaActive()`, not a V=0 latch.
                if !self.channels[ch].hdma_started {
                    self.channels[ch].hdma_start_frame(bus);
                    self.channels[ch].hdma_started = true;
                }
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
    fn run_mdma_segment_chunked_matches_one_shot() {
        // Increment 0 invariant: splitting a sync DMA into arbitrary
        // byte-bounded segments produces byte-identical B-bus output and
        // identical channel end-state vs. running it in one shot.
        // Two masked channels with odd sizes + different modes, so chunk
        // boundaries fall mid-pattern and mid-channel.
        fn configure(dma: &mut Dma, bus: &mut MockBus) {
            for i in 0..256u32 {
                bus.a[(0x7E_0000 + i) as usize] = i as u8;
                bus.a[(0x7F_0000 + i) as usize] = 0x80u8.wrapping_add(i as u8);
            }
            // Ch0: mode 1 (2 regs, pattern [0,1]), BBAD $18, 70 bytes.
            dma.channels[0].params = DmaParams::from_byte(0x01);
            dma.channels[0].bbad = 0x18;
            dma.channels[0].a_addr = 0x0000;
            dma.channels[0].a_bank = 0x7E;
            dma.channels[0].das = 70;
            // Ch2: mode 0 (1 reg), BBAD $22, 37 bytes.
            dma.channels[2].params = DmaParams::from_byte(0x00);
            dma.channels[2].bbad = 0x22;
            dma.channels[2].a_addr = 0x0000;
            dma.channels[2].a_bank = 0x7F;
            dma.channels[2].das = 37;
        }
        let mask = 0b0000_0101; // channels 0 and 2

        // (a) one shot
        let mut bus_ref = MockBus::new();
        let mut dma_ref = Dma::new();
        configure(&mut dma_ref, &mut bus_ref);
        let n_ref = dma_ref.run_mdma(&mut bus_ref, mask);

        // (b) segmented in cycling chunk sizes until the cursor clears
        let mut bus_seg = MockBus::new();
        let mut dma_seg = Dma::new();
        configure(&mut dma_seg, &mut bus_seg);
        let chunks = [1u32, 7, 8, 9, 170, 65535];
        let mut n_seg = 0u32;
        let mut i = 0;
        while dma_seg.mdma_cursor.is_some() || n_seg == 0 {
            n_seg += dma_seg.run_mdma_segment(&mut bus_seg, mask, chunks[i % chunks.len()]);
            i += 1;
            assert!(i < 1000, "segmented run did not terminate");
        }

        assert_eq!(n_ref, 70 + 37);
        assert_eq!(n_seg, n_ref, "same total bytes");
        assert_eq!(bus_seg.b, bus_ref.b, "byte-identical B-bus output");
        assert!(
            dma_seg.mdma_cursor.is_none(),
            "cursor cleared at completion"
        );
        for ch in [0usize, 2] {
            assert_eq!(dma_seg.channels[ch].a_addr, dma_ref.channels[ch].a_addr);
            assert_eq!(dma_seg.channels[ch].das, dma_ref.channels[ch].das);
            assert!(!dma_seg.channels[ch].seg_running);
        }
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
    fn hdma_enabled_mid_frame_starts_from_source() {
        // Regression: a channel whose HDMAEN bit is set AFTER `hdma_init`
        // (Yoshi's Island enables its text-band split at scanline ~12)
        // must still run, lazily setting up from its source address on the
        // first active line — not stay dormant until the next frame's init.
        let mut bus = MockBus::new();
        bus.a[0x00_2000] = 0x02; // line count
        bus.a[0x00_2001] = 0xAB; // data byte
        bus.a[0x00_2002] = 0x00; // terminator
        let mut dma = Dma::new();
        dma.channels[0].params = DmaParams::from_byte(0); // mode 0, direct
        dma.channels[0].bbad = 0x22;
        dma.channels[0].a_addr = 0x2000;
        dma.channels[0].a_bank = 0x00;

        // Frame init with HDMA disabled → channel does not set up.
        dma.hdmaen = 0;
        assert_eq!(dma.hdma_init(&mut bus), 0);
        assert!(!dma.channels[0].hdma_active);

        // Mid-frame: the game enables the channel. The next scanline must
        // set it up from $00:2000 and transfer the data byte to $2122.
        dma.hdmaen = 0b0000_0001;
        assert_eq!(dma.hdma_run_line(&mut bus), HDMA_OVERHEAD_MCLK + 8);
        assert_eq!(bus.b[0x22], 0xAB, "mid-frame-enabled channel transferred");
        assert!(dma.channels[0].hdma_started);
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
