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
    /// Exact horizontal master-clock (0..1363) at the transfer — Mesen2's
    /// `GetHClock` (the Event Viewer plots events at `(hclock, line)`).
    pub hclock: u16,
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
        // ares `hdmaReset` + Mesen2 `InitHdmaChannels`: every frame, reset the
        // per-channel run flags for ALL 8 channels — enabled or not. Mesen
        // notes NOT resetting `DoTransfer` glitches Aladdin / Super Ghouls'n
        // Ghosts. `hdma_active` is luna's `!HdmaFinished`.
        for ch in 0..8 {
            self.channels[ch].hdma_active = true;
            self.channels[ch].hdma_do_transfer = false;
        }
        if self.hdmaen == 0 {
            return 0;
        }
        let mut enabled = 0u32;
        for ch in 0..8 {
            // "Set DoTransfer to true for ALL channels if any HDMA channel is
            // enabled" (Mesen2 SnesDmaController.cpp:131, ares dma.cpp:143).
            // This is what lets a channel enabled MID-frame transfer from its
            // (stale) running table pointer — the references do NOT re-copy
            // the source address on a mid-frame enable.
            self.channels[ch].hdma_do_transfer = true;
            if self.hdmaen & (1 << ch) != 0 {
                // Enabled at V=0: copy source→pointer and read the first entry
                // (may terminate the channel if the header is 0).
                self.channels[ch].hdma_start_frame(bus);
                enabled += 1;
            }
        }
        HDMA_OVERHEAD_MCLK + 8 * enabled
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
                // Live HDMAEN gate (ares `hdmaActive()` / Mesen2 per-line
                // `HdmaChannels & (1<<i)`): a channel enabled mid-frame runs
                // from here using the `do_transfer`/pointer state left by
                // `hdma_init` — the references keep that (stale) state; they do
                // NOT re-copy the source on a mid-frame enable.
                // Tag B-bus writes this channel makes with its channel id so
                // the Event Viewer can plot them — faithful to Mesen2's
                // `_activeChannel = HdmaChannelFlag | i` (SnesDmaController.cpp
                // :264). The HDMA flag (bit 6) marks the event as HDMA-sourced;
                // consumers mask `& 7` for the channel number.
                bus.set_active_channel(HDMA_CHANNEL_FLAG | ch as u8);
                // A channel active at line start does work this line.
                any_active |= self.channels[ch].hdma_active;
                // ares `hdmaFinished()`: this is the last active HDMA channel
                // iff no higher-indexed enabled channel is still active. Snapshot
                // it before the step (later channels haven't advanced this line —
                // matching ares' transfer-all-then-advance-all ordering). Drives
                // the indirect-terminator 1-byte reload quirk (`hdma_step_line`).
                let last_active = !((ch + 1..8)
                    .any(|j| self.hdmaen & (1 << j) != 0 && self.channels[j].hdma_active));
                bytes += self.channels[ch].hdma_step_line(bus, last_active);
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

/// Marks an active-channel tag as HDMA-sourced (Mesen2's
/// `SnesDmaController::HdmaChannelFlag`, `SnesDmaController.h:12`). OR'd onto
/// the channel index for the Event Viewer; consumers take `& 7` for the
/// channel number.
const HDMA_CHANNEL_FLAG: u8 = 0x40;

#[cfg(test)]
mod tests {
    use super::super::bus::DmaBus;
    use super::super::channel::DmaParams;
    use super::*;

    struct MockBus {
        a: Vec<u8>,
        b: Vec<u8>,
        /// Active channel tag last set via `set_active_channel` (mirrors the
        /// real `DmaBusView::dma_channel`); recorded against each `write_b`.
        active: u8,
        /// `(active_channel, b_offset, value)` for every B-bus write, so a
        /// test can assert the channel tag carried by each transfer.
        tagged_writes: Vec<(u8, u8, u8)>,
    }

    impl MockBus {
        fn new() -> Self {
            Self {
                a: vec![0; 0x100_0000],
                b: vec![0; 0x100],
                active: 0,
                tagged_writes: Vec::new(),
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
            self.tagged_writes.push((self.active, b_offset, value));
        }
        fn set_active_channel(&mut self, channel: u8) {
            self.active = channel;
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
    fn hdma_mid_frame_enable_uses_stale_pointer_not_source() {
        // Faithful port (ares hdmaSetup / Mesen2 InitHdmaChannels, audit #9): a
        // channel enabled MID-frame does NOT re-copy its source address. When
        // any HDMA is enabled at V=0, hdma_init arms `DoTransfer=true` for ALL
        // channels, so a later-enabled channel runs from its (stale) running
        // table pointer. luna previously lazily re-init'd from source — an
        // invention absent from BOTH references (Yoshi's Island still renders
        // correctly under the faithful model; validated by screenshot).
        let mut bus = MockBus::new();
        // ch7 is enabled at V=0 → makes hdma_init run its full loop.
        bus.a[0x00_1000] = 0x01; // 1-line entry
        bus.a[0x00_1001] = 0x11;
        bus.a[0x00_1002] = 0x00; // terminator
        // ch0's STALE pointer points at $00:3000 ($DD); its SOURCE is a
        // different place ($00:2000 = $AA) that must NOT be read mid-frame.
        bus.a[0x00_3000] = 0xDD;
        bus.a[0x00_2000] = 0xAA;

        let mut dma = Dma::new();
        dma.channels[7].params = DmaParams::from_byte(0);
        dma.channels[7].bbad = 0x30;
        dma.channels[7].a_addr = 0x1000;
        dma.channels[0].params = DmaParams::from_byte(0);
        dma.channels[0].bbad = 0x22;
        dma.channels[0].a_addr = 0x2000; // source — must stay untouched
        dma.channels[0].a2a = 0x3000; // stale running pointer
        dma.channels[0].ntlr = 0x02; // stale line counter (non-repeat gap next)

        dma.hdmaen = 0x80; // only ch7 at V=0
        dma.hdma_init(&mut bus);
        assert!(
            dma.channels[0].hdma_do_transfer,
            "init armed DoTransfer for ALL channels"
        );
        assert_eq!(
            dma.channels[0].a2a, 0x3000,
            "source NOT copied into pointer"
        );

        // Mid-frame enable of ch0 → transfers from the stale pointer ($DD).
        dma.hdmaen = 0x81;
        dma.hdma_run_line(&mut bus);
        assert_eq!(
            bus.b[0x22], 0xDD,
            "ch0 ran from its stale pointer, not source"
        );
    }

    #[test]
    fn hdma_cold_mid_frame_enable_skips_transfer_first_line() {
        // Faithful: if NO channel is enabled at V=0, hdma_init resets
        // DoTransfer=false for all and early-returns (Mesen2 InitHdmaChannels
        // line 111 + the `!HdmaChannels` return). A channel enabled mid-frame
        // then has DoTransfer=false → NO transfer on its first active line; it
        // only advances the (stale) counter.
        let mut bus = MockBus::new();
        bus.a[0x00_3000] = 0xDD;
        let mut dma = Dma::new();
        dma.channels[0].params = DmaParams::from_byte(0);
        dma.channels[0].bbad = 0x22;
        dma.channels[0].a2a = 0x3000;
        dma.channels[0].ntlr = 0x02;

        dma.hdmaen = 0;
        assert_eq!(dma.hdma_init(&mut bus), 0);
        assert!(
            !dma.channels[0].hdma_do_transfer,
            "cold init leaves DoTransfer false"
        );

        dma.hdmaen = 0x01;
        dma.hdma_run_line(&mut bus);
        assert_eq!(bus.b[0x22], 0x00, "no transfer on the first line");
    }

    #[test]
    fn hdma_transfer_tags_writes_with_the_hdma_channel_flag() {
        // Regression: HDMA per-scanline register writes must be tagged with
        // their channel so the Event Viewer can plot them — faithful to
        // Mesen2's `_activeChannel = HdmaChannelFlag | i`. The tag is
        // `0x40 | ch`; consumers mask `& 7` for the channel number. (Without
        // this, raster/gradient HDMA effects were invisible in the overlay.)
        let mut bus = MockBus::new();
        // Channel 3, mode 0, one transferred line of 0x5A → $2122.
        bus.a[0x00_2000] = 0x01; // 1-line, non-repeat
        bus.a[0x00_2001] = 0x5A; // data byte
        bus.a[0x00_2002] = 0x00; // terminator
        let mut dma = Dma::new();
        dma.channels[3].params = DmaParams::from_byte(0); // mode 0, direct
        dma.channels[3].bbad = 0x22;
        dma.channels[3].a_addr = 0x2000;
        dma.channels[3].a_bank = 0x00;
        dma.hdmaen = 0b0000_1000; // channel 3

        dma.hdma_init(&mut bus); // header reads only — no B-bus writes
        assert!(
            bus.tagged_writes.is_empty(),
            "hdma_init must not write B-bus registers"
        );

        dma.hdma_run_line(&mut bus);
        assert_eq!(
            bus.tagged_writes,
            vec![(HDMA_CHANNEL_FLAG | 3, 0x22, 0x5A)],
            "the transfer is tagged with HdmaChannelFlag | channel 3"
        );
        // Consumer view: the channel number is the low 3 bits.
        assert_eq!(bus.tagged_writes[0].0 & 7, 3);
    }

    #[test]
    fn hdma_indirect_terminator_1byte_quirk_tracks_the_last_active_channel() {
        // Row #10 integration: the "last active channel" that drives the
        // 1-byte indirect-terminator quirk (ares `hdmaFinished()`) is computed
        // across channels. Two indirect channels: ch0 terminates on line 1
        // while ch1 (higher index) is still active → ch0 is NOT last, reads 2
        // pointer bytes. ch1 terminates on line 2 as the sole survivor → it IS
        // last, reads only 1.
        let mut bus = MockBus::new();
        // ch0 table $00:1000 — 1-line entry → $3456, then 0 terminator whose
        // pointer bytes ($EE,$FF) sit at offsets 4/5.
        for (i, b) in [0x01u8, 0x56, 0x34, 0x00, 0xEE, 0xFF].iter().enumerate() {
            bus.a[0x00_1000 + i] = *b;
        }
        bus.a[0x7E_3456] = 0xAB;
        // ch1 table $00:2000 — 2-line entry → $5678, then 0 terminator ($AA,$BB).
        for (i, b) in [0x02u8, 0x78, 0x56, 0x00, 0xAA, 0xBB].iter().enumerate() {
            bus.a[0x00_2000 + i] = *b;
        }
        bus.a[0x7E_5678] = 0xCD;
        bus.a[0x7E_5679] = 0xEF;

        let mut dma = Dma::new();
        for (ch, addr) in [(0usize, 0x1000u16), (1, 0x2000)] {
            dma.channels[ch].params = DmaParams::from_byte(0x40); // indirect, mode 0
            dma.channels[ch].bbad = 0x22;
            dma.channels[ch].a_addr = addr;
            dma.channels[ch].a_bank = 0x00;
            dma.channels[ch].dasb = 0x7E;
        }
        dma.hdmaen = 0b0000_0011;
        dma.hdma_init(&mut bus);

        // Line 1: ch0 terminates but ch1 is still active → NOT last → 2 bytes.
        dma.hdma_run_line(&mut bus);
        assert!(!dma.channels[0].hdma_active);
        assert_eq!(
            dma.channels[0].a2a, 0x1006,
            "ch0 read header + 2 indirect bytes (a later channel is active)"
        );
        assert_eq!(dma.channels[0].das, 0xFFEE, "full 2-byte indirect pointer");
        assert!(
            dma.channels[1].hdma_active,
            "ch1 still inside its 2-line entry"
        );

        // Line 2: ch1 terminates as the sole remaining channel → last → 1 byte.
        dma.hdma_run_line(&mut bus);
        assert!(!dma.channels[1].hdma_active);
        assert_eq!(
            dma.channels[1].a2a, 0x2005,
            "ch1 read header + 1 indirect byte (last active channel quirk)"
        );
        assert_eq!(
            dma.channels[1].das, 0xAA00,
            "ares: firstByte << 8, one short"
        );
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
