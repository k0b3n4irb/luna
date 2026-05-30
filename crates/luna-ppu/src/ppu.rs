//! [`Ppu`] register dispatch.
//!
//! The PPU is memory-mapped at `$00-$3F:$2100-$213F` (and the mirror in
//! `$80-$BF`). This module exposes [`Ppu::write`] / [`Ppu::read`]
//! taking the **low 6 bits of the offset** (`0x00`-`0x3F`) — the bus
//! is responsible for the bank/region routing.

use crate::memory::{Cgram, Oam, VmainSettings, Vram};
use crate::renderer::{FRAME_H, FRAME_W, RenderOptions, render_scanline_partial_into};
// `render_scanline_into` is no longer used directly here — full-line
// renders go through `flush_partial_scanline`.

/// PPU register offsets (relative to `$2100`).
pub mod register {
    /// `$2100` — INIDISP: forced blank + master brightness.
    pub const INIDISP: u8 = 0x00;
    /// `$2101` — OBSEL: sprite OAM size + base.
    pub const OBSEL: u8 = 0x01;
    /// `$2102/$2103` — OAMADDL/H.
    pub const OAMADDL: u8 = 0x02;
    /// `$2102/$2103` — OAMADDL/H.
    pub const OAMADDH: u8 = 0x03;
    /// `$2104` — OAMDATA (write).
    pub const OAMDATA: u8 = 0x04;
    /// `$2105` — BGMODE.
    pub const BGMODE: u8 = 0x05;
    /// `$2106` — MOSAIC.
    pub const MOSAIC: u8 = 0x06;
    /// `$2107` — BG1SC: BG1 tilemap address & size.
    pub const BG1SC: u8 = 0x07;
    /// `$2108` — BG2SC.
    pub const BG2SC: u8 = 0x08;
    /// `$2109` — BG3SC.
    pub const BG3SC: u8 = 0x09;
    /// `$210A` — BG4SC.
    pub const BG4SC: u8 = 0x0A;
    /// `$210B` — BG12NBA: BG1/2 character base.
    pub const BG12NBA: u8 = 0x0B;
    /// `$210C` — BG34NBA: BG3/4 character base.
    pub const BG34NBA: u8 = 0x0C;
    /// `$210D` — BG1HOFS (write-twice latch).
    pub const BG1HOFS: u8 = 0x0D;
    /// `$210E` — BG1VOFS.
    pub const BG1VOFS: u8 = 0x0E;
    /// `$210F` — BG2HOFS.
    pub const BG2HOFS: u8 = 0x0F;
    /// `$2110` — BG2VOFS.
    pub const BG2VOFS: u8 = 0x10;
    /// `$2111` — BG3HOFS.
    pub const BG3HOFS: u8 = 0x11;
    /// `$2112` — BG3VOFS.
    pub const BG3VOFS: u8 = 0x12;
    /// `$2113` — BG4HOFS.
    pub const BG4HOFS: u8 = 0x13;
    /// `$2114` — BG4VOFS.
    pub const BG4VOFS: u8 = 0x14;
    /// `$2115` — VMAIN: VRAM increment behaviour.
    pub const VMAIN: u8 = 0x15;
    /// `$2116/$2117` — VMADDL/H: VRAM word address.
    pub const VMADDL: u8 = 0x16;
    /// `$2116/$2117` — VMADDL/H: VRAM word address.
    pub const VMADDH: u8 = 0x17;
    /// `$2118/$2119` — VMDATAL/H: VRAM data (write).
    pub const VMDATAL: u8 = 0x18;
    /// `$2118/$2119` — VMDATAL/H: VRAM data (write).
    pub const VMDATAH: u8 = 0x19;
    /// `$2121` — CGADD: CGRAM word address.
    pub const CGADD: u8 = 0x21;
    /// `$2122` — CGDATA (write).
    pub const CGDATA: u8 = 0x22;
    /// `$2138` — OAMDATAREAD.
    pub const OAMDATAREAD: u8 = 0x38;
    /// `$2139/$213A` — VMDATALREAD/HREAD.
    pub const VMDATALREAD: u8 = 0x39;
    /// `$2139/$213A` — VMDATALREAD/HREAD.
    pub const VMDATAHREAD: u8 = 0x3A;
    /// `$213B` — CGDATAREAD.
    pub const CGDATAREAD: u8 = 0x3B;
    /// `$211A` M7SEL — Mode-7 H/V flip + screen-over mode.
    pub const M7SEL: u8 = 0x1A;
    /// `$211B` M7A — Mode-7 matrix element A (signed 8.8, write-twice).
    pub const M7A: u8 = 0x1B;
    /// `$211C` M7B — matrix B. Writing also triggers the M7A×M7B
    /// multiplication and updates MPYL/M/H.
    pub const M7B: u8 = 0x1C;
    /// `$211D` M7C — matrix C.
    pub const M7C: u8 = 0x1D;
    /// `$211E` M7D — matrix D.
    pub const M7D: u8 = 0x1E;
    /// `$211F` M7X — Mode-7 centre X (signed 13-bit, write-twice).
    pub const M7X: u8 = 0x1F;
    /// `$2120` M7Y — Mode-7 centre Y.
    pub const M7Y: u8 = 0x20;
    /// `$2134` MPYL — low byte of the Mode-7 hardware multiplier
    /// result (signed-16 M7A × signed-8 M7B[high], 24-bit total).
    pub const MPYL: u8 = 0x34;
    /// `$2135` MPYM — middle byte.
    pub const MPYM: u8 = 0x35;
    /// `$2136` MPYH — high byte.
    pub const MPYH: u8 = 0x36;
    /// `$2123` W12SEL — windows-1/2 enable + invert for BG1 and BG2.
    pub const W12SEL: u8 = 0x23;
    /// `$2124` W34SEL — same for BG3 and BG4.
    pub const W34SEL: u8 = 0x24;
    /// `$2125` WOBJSEL — same for OBJ and the color-math window.
    pub const WOBJSEL: u8 = 0x25;
    /// `$2126` WH0 — window 1 left X coordinate.
    pub const WH0: u8 = 0x26;
    /// `$2127` WH1 — window 1 right X coordinate.
    pub const WH1: u8 = 0x27;
    /// `$2128` WH2 — window 2 left X coordinate.
    pub const WH2: u8 = 0x28;
    /// `$2129` WH3 — window 2 right X coordinate.
    pub const WH3: u8 = 0x29;
    /// `$212A` WBGLOG — 2-bit window-combine logic for each BG layer.
    pub const WBGLOG: u8 = 0x2A;
    /// `$212B` WOBJLOG — combine logic for OBJ and math windows.
    pub const WOBJLOG: u8 = 0x2B;
    /// `$212C` TM — main-screen layer enable mask.
    pub const TM: u8 = 0x2C;
    /// `$212D` TS — sub-screen layer enable mask.
    pub const TS: u8 = 0x2D;
    /// `$212E` TMW — main-screen window mask (1 = disable layer
    /// inside the combined window region).
    pub const TMW: u8 = 0x2E;
    /// `$212F` TSW — sub-screen window mask.
    pub const TSW: u8 = 0x2F;
    /// `$2130` CGWSEL — color-math window / sub-screen-mix flags.
    pub const CGWSEL: u8 = 0x30;
    /// `$2131` CGADSUB — color-math operator + per-layer enable mask.
    pub const CGADSUB: u8 = 0x31;
    /// `$2132` COLDATA — fixed sub-screen colour, written one (R/G/B)
    /// channel at a time via the top 3 bits.
    pub const COLDATA: u8 = 0x32;
    /// `$2133` SETINI — interlace / hi-res / overscan flags.
    pub const SETINI: u8 = 0x33;
    /// `$213E` STAT77 — PPU1 status: interlace field, OBJ range/time
    /// over, chip ID.
    pub const STAT77: u8 = 0x3E;
    /// `$213F` STAT78 — PPU2 status: interlace odd-field, region
    /// bit (PAL/NTSC), chip rev. Reads ALSO reset the BG scroll
    /// write-twice latch (a documented hardware side effect).
    pub const STAT78: u8 = 0x3F;
    /// `$2137` SLHV — latches the current H/V counters when read,
    /// returning open-bus.
    pub const SLHV: u8 = 0x37;
    /// `$213C` OPHCT — latched H counter (9-bit, read low then high).
    pub const OPHCT: u8 = 0x3C;
    /// `$213D` OPVCT — latched V counter (9-bit, read low then high).
    pub const OPVCT: u8 = 0x3D;
}

/// Per-layer state derived from `$2107-$2114`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BgState {
    /// VRAM word address of the tilemap base.
    pub tilemap_addr_words: u16,
    /// VRAM word address of the layer's character (tile) base.
    pub char_addr_words: u16,
    /// 10-bit horizontal scroll.
    pub h_scroll: u16,
    /// 10-bit vertical scroll.
    pub v_scroll: u16,
    /// Tilemap SC bits 0-1 from `BG*SC`: 0=32x32, 1=64x32, 2=32x64, 3=64x64.
    /// Not yet honoured by the renderer; stored for forward compat.
    pub tilemap_size: u8,
}

/// Convenience accessor for the renderer.
#[must_use]
pub const fn bg_state(ppu: &Ppu, idx: usize) -> BgState {
    ppu.bg[idx]
}

/// The SNES Picture Processing Unit.
///
/// P1.1 scope: the data-flow plumbing — VRAM, CGRAM, OAM and the
/// minimum register subset to upload data to them. The rendering side
/// (modes, scroll, sprites on screen) lands in P1.4+.
pub struct Ppu {
    /// 64 KB tile and tilemap memory.
    pub vram: Vram,
    /// 512 B palette memory.
    pub cgram: Cgram,
    /// 544 B object attribute memory.
    pub oam: Oam,

    // ---------- Visual registers (stored but not yet rendered) ----------
    /// `$2100` INIDISP — bit 7 = forced blank, bits 0-3 = brightness.
    pub inidisp: u8,
    /// `$2101` OBSEL — sprite OAM size & character base.
    pub obsel: u8,
    /// `$2105` BGMODE — mode (bits 0-2) + BG3 priority + tile sizes.
    pub bgmode: u8,
    /// `$2106` MOSAIC.
    pub mosaic: u8,
    /// `$2130` CGWSEL — bit 7:6 force-main-black region, bit 5:4
    /// math-enable region (both reference the colour-math window),
    /// bit 1 sub-BG/OBJ enable, bit 0 direct-colour mode for 8bpp.
    pub cgwsel: u8,
    /// `$2131` CGADSUB — bit 7 add/subtract, bit 6 half-color, bit 5
    /// backdrop, bit 4 OBJ palettes 4-7, bits 3:0 BG4..BG1 enables.
    pub cgadsub: u8,
    /// `$2132` COLDATA: red channel of the fixed sub-screen colour
    /// (5 bits, 0-31). Updated by writes whose bit 5 is set.
    pub coldata_r: u8,
    /// `$2132` COLDATA: green channel (5 bits). Bit 6 selects.
    pub coldata_g: u8,
    /// `$2132` COLDATA: blue channel (5 bits). Bit 7 selects.
    pub coldata_b: u8,
    /// `$2123-$2125` — per-region window enable / invert bits.
    /// Layout, per layer (BG1, BG2, BG3, BG4, OBJ, math):
    ///   bit 0 = window-1 invert (1 = use "outside" semantics)
    ///   bit 1 = window-1 enable
    ///   bit 2 = window-2 invert
    ///   bit 3 = window-2 enable
    /// `w12sel` packs BG1 (low nibble) + BG2 (high nibble),
    /// `w34sel` packs BG3 + BG4, `wobjsel` packs OBJ + math.
    pub w12sel: u8,
    /// `$2124` W34SEL — same control word for BG3 (low nibble) and
    /// BG4 (high nibble). See [`Self::w12sel`] for bit semantics.
    pub w34sel: u8,
    /// `$2125` WOBJSEL — OBJ (low nibble) and the dedicated colour-
    /// math window (high nibble). Same per-nibble layout.
    pub wobjsel: u8,
    /// `$2126` WH0 — window 1 left X (inclusive).
    pub wh0: u8,
    /// `$2127` WH1 — window 1 right X (inclusive).
    pub wh1: u8,
    /// `$2128` WH2 — window 2 left X.
    pub wh2: u8,
    /// `$2129` WH3 — window 2 right X.
    pub wh3: u8,
    /// `$212A` WBGLOG — 2-bit window-combine logic per BG.
    /// Layout: bits 0-1 = BG1, 2-3 = BG2, 4-5 = BG3, 6-7 = BG4.
    /// Values: 0 = OR, 1 = AND, 2 = XOR, 3 = XNOR.
    pub wbglog: u8,
    /// `$212B` WOBJLOG — bits 0-1 = OBJ logic, bits 2-3 = math.
    pub wobjlog: u8,
    /// `$212C` TM — main-screen layer enable mask
    /// (bit 0..4 = BG1..BG4, OBJ).
    pub tm: u8,
    /// `$212D` TS — sub-screen layer enable mask.
    pub ts: u8,
    /// `$212E` TMW — main-screen window mask
    /// (1 bit = disable that layer inside the combined window).
    pub tmw: u8,
    /// `$212F` TSW — sub-screen window mask.
    pub tsw: u8,
    /// BG1-4 derived state ($2107-$2114).
    pub bg: [BgState; 4],
    /// `$211A` M7SEL: bit 0 = H-flip, bit 1 = V-flip, bits 7:6 =
    /// screen-over mode (00/01 wrap, 10 transparent, 11 use-tile-0).
    /// (ares io.cpp:411-414 — NOT the bit 7/6/1:0 layout some docs imply.)
    pub m7sel: u8,
    /// Mode-7 matrix element A — signed 8.8, scales the horizontal
    /// component of the projected X axis.
    pub m7a: i16,
    /// Mode-7 matrix element B — signed 8.8, scales the vertical
    /// component of the projected X axis. Writes also retrigger the
    /// hardware multiplier (see [`Self::mpy_result`]).
    pub m7b: i16,
    /// Mode-7 matrix element C — horizontal component of Y axis.
    pub m7c: i16,
    /// Mode-7 matrix element D — vertical component of Y axis.
    pub m7d: i16,
    /// Mode-7 centre X (signed 13-bit, sign-extended to i16).
    pub m7x: i16,
    /// Mode-7 centre Y.
    pub m7y: i16,
    /// Mode-7 horizontal scroll `M7HOFS` (`$210D`), signed 13-bit,
    /// sign-extended to i16. Distinct from `bg[0].h_scroll`: `$210D`
    /// feeds *both* the 10-bit BG1 scroll and this 13-bit Mode-7
    /// scroll, which uses the shared Mode-7 latch (ares io.cpp:308).
    pub m7_hofs: i16,
    /// Mode-7 vertical scroll `M7VOFS` (`$210E`), signed 13-bit.
    pub m7_vofs: i16,
    /// `$2133` SETINI — bit 7: external sync, bit 6: EXTBG (Mode-7
    /// BG2 overlay), bit 5: hi-res 512×448, bit 4: overscan,
    /// bit 3: pseudo-512, bit 2: V-mosaic disable, bit 1: interlace.
    pub setini: u8,
    /// `$213E` STAT77 — read-side latch for the PPU1 status byte.
    /// Bit 7 toggles per field in interlace mode; bits 0-3 = chip ID
    /// (we return 1 — model 5C77).
    pub stat77: u8,
    /// `$213F` STAT78 — read-side latch for the PPU2 status byte.
    /// Bit 7 = interlace odd-field flag, bit 4 = region (1 = PAL,
    /// 0 = NTSC), bits 0-3 = chip revision (= 2).
    pub stat78: u8,
    /// Latched H counter (9-bit) from the last $2137/$4201-bit7
    /// trigger. Exposed at OPHCT.
    pub ophct: u16,
    /// Latched V counter (9-bit). Exposed at OPVCT.
    pub opvct: u16,
    /// "High-byte pending" flag for OPHCT — first read returns the
    /// low byte (and sets this), second returns the high byte (and
    /// clears it).
    pub ophct_hi_pending: bool,
    /// Same for OPVCT.
    pub opvct_hi_pending: bool,
    /// "Latch hit" bit: set when the H/V counters were latched
    /// since the last STAT78 read. Exposed at STAT78 bit 6.
    pub external_latch_hit: bool,
    /// `$2134-$2136 MPYL/M/H` — 24-bit hardware multiplier result.
    /// Updated whenever M7A or M7B's high byte is written:
    /// `M7A (signed 16) × M7B_high (signed 8) → 24-bit signed`.
    pub mpy_result: i32,
    /// Shared latch for the Mode-7 write-twice registers
    /// (\$211B-\$2120). Each write `value` shifts the latch and
    /// stores the 16-bit pair `(latch_low, value_high)`.
    pub m7_latch: u8,
    /// Latch for the BG H/V scroll write-twice protocol.
    /// `$210D-$2114` are written low byte first, then high byte (bits
    /// 0-1 land as the top 2 bits of the 10-bit scroll). The PPU keeps
    /// a single shared latch across all eight registers — every write
    /// to any of them updates that latch.
    bg_scroll_latch: u8,

    // ---------- Open-bus tracking ----------
    /// Last value seen on the PPU data bus — returned for reads of
    /// write-only registers.
    pub open_bus: u8,

    // ---------- Diagnostic counters (debug-only, not on hot path) ----------
    /// How many times `$2100` (INIDISP) has been written since reset.
    /// Used by the GUI's Stubs panel to detect "the game never touched
    /// INIDISP again after init" (= NMI handler not running).
    pub inidisp_write_count: u64,

    /// Persistent framebuffer, written one scanline at a time by the
    /// scheduler via [`Ppu::render_current_scanline`]. Consumers
    /// (`luna-api::Emulator::render_frame_png`, the GUI, etc.) read
    /// this directly for zero-cost frame access — they no longer need
    /// to re-render the whole frame on every request.
    ///
    /// Tests that hand-build a `Ppu` and call
    /// [`render_frame_with`](crate::render_frame_with) bypass this
    /// buffer entirely and get a freshly-computed `Vec`.
    pub framebuffer: Vec<[u8; 3]>,

    /// How far the current scanline has been rendered into the
    /// framebuffer. Phase 2 of gap G6: a `$21xx` write that lands
    /// mid-scanline calls [`Ppu::flush_partial_scanline`] to commit
    /// pixels `last_flushed_dot..current_dot` with the OLD state
    /// BEFORE the write takes effect. Reset to 0 by
    /// [`Ppu::scanline_reset`] at every scanline boundary.
    pub last_flushed_dot: u16,

    /// `true` when the screen is currently being scanned out — i.e.
    /// not in forced blank (`INIDISP.7 == 0`) AND the current scanline
    /// is visible (`< vblank_start_line`). Updated by the bus layer
    /// before every PPU register write. Gap G7: writes to VRAM
    /// (`$2118/$2119`), CGRAM (`$2122`), and OAM (`$2104`) during
    /// active display silently DROP the data byte on real hardware —
    /// the address/latch counter still advances. ares ppu_io.cpp:19-45,
    /// Mesen2 SnesPpu.cpp:2046-2057 (`CanAccessVram`).
    pub active_display: bool,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    /// Build a powered-on PPU (all RAM zeroed, registers reset).
    #[must_use]
    pub fn new() -> Self {
        Self {
            vram: Vram::new(),
            cgram: Cgram::new(),
            oam: Oam::new(),
            inidisp: 0x80, // post-reset: forced blank
            obsel: 0,
            bgmode: 0,
            mosaic: 0,
            cgwsel: 0,
            cgadsub: 0,
            coldata_r: 0,
            coldata_g: 0,
            coldata_b: 0,
            w12sel: 0,
            w34sel: 0,
            wobjsel: 0,
            wh0: 0,
            wh1: 0,
            wh2: 0,
            wh3: 0,
            wbglog: 0,
            wobjlog: 0,
            // TM default to "all main layers on" matches the most
            // common driver-init pattern (drivers explicitly write
            // their final mask before un-blanking, so the default
            // mostly affects bring-up correctness; "all on" gives
            // us back-compat with the pre-window-aware renderer).
            tm: 0x1F,
            ts: 0,
            tmw: 0,
            tsw: 0,
            bg: [BgState::default(); 4],
            m7sel: 0,
            m7a: 0,
            m7b: 0,
            m7c: 0,
            m7d: 0,
            m7x: 0,
            m7y: 0,
            m7_hofs: 0,
            m7_vofs: 0,
            mpy_result: 0,
            m7_latch: 0,
            setini: 0,
            // Initial PPU1 chip-ID = 1, no over flags, interlace field 0.
            stat77: 0x01,
            // Initial PPU2 chip rev = 2; region bit (4) defaults to 0
            // (NTSC). PAL emulation can flip this on cart load.
            stat78: 0x02,
            ophct: 0,
            opvct: 0,
            ophct_hi_pending: false,
            opvct_hi_pending: false,
            external_latch_hit: false,
            bg_scroll_latch: 0,
            open_bus: 0,
            inidisp_write_count: 0,
            framebuffer: vec![[0u8; 3]; FRAME_W * FRAME_H],
            last_flushed_dot: 0,
            // Post-reset state is forced-blanked (INIDISP=$80), so
            // active_display starts false.
            active_display: false,
        }
    }

    /// Render the current visible scanline `y` into the persistent
    /// framebuffer, using the live PPU register state. Called by the
    /// scheduler at the end of every visible scanline (gap G6 Phase 1).
    ///
    /// Equivalent to a full-line flush: renders pixels
    /// `last_flushed_dot..FRAME_W` and resets the partial-flush cursor.
    /// Out-of-range `y` (≥ `FRAME_H`) is a no-op.
    pub fn render_current_scanline(&mut self, y: u16, opts: RenderOptions) {
        self.flush_partial_scanline(y, FRAME_W as u16, opts);
        self.scanline_reset();
    }

    /// Render pixels `last_flushed_dot..end_x` of scanline `y` into the
    /// persistent framebuffer using the current PPU register state, and
    /// advance `last_flushed_dot` to `end_x`. Phase 2 of gap G6 — the
    /// bus layer calls this BEFORE applying a `$21xx` write so the
    /// in-progress scanline gets the pre-write pixels committed.
    ///
    /// Out-of-range `y` (≥ `FRAME_H`) or `end_x <= last_flushed_dot`
    /// is a no-op.
    pub fn flush_partial_scanline(&mut self, y: u16, end_x: u16, opts: RenderOptions) {
        let yi = usize::from(y);
        if yi >= FRAME_H {
            return;
        }
        let start = self.last_flushed_dot.min(FRAME_W as u16);
        let end = end_x.min(FRAME_W as u16);
        if start >= end {
            return;
        }
        // Stack scratch row → partial render → copy into framebuffer.
        let mut row = [[0u8; 3]; FRAME_W];
        render_scanline_partial_into(self, y, start, end, opts, &mut row);
        let off = yi * FRAME_W;
        let si = usize::from(start);
        let ei = usize::from(end);
        self.framebuffer[off + si..off + ei].copy_from_slice(&row[si..ei]);
        self.last_flushed_dot = end;
    }

    /// Reset the partial-flush cursor to 0. Called by the scheduler at
    /// every scanline boundary so the next line starts from dot 0.
    pub const fn scanline_reset(&mut self) {
        self.last_flushed_dot = 0;
    }

    /// Borrow the current persistent framebuffer (256 × 224 BGR888
    /// pixels). Cheap accessor — no rendering happens here.
    #[must_use]
    pub fn framebuffer(&self) -> &[[u8; 3]] {
        &self.framebuffer
    }

    /// Read a PPU register. `offset` is the byte offset from `$2100`
    /// (`0x00..=0x3F`).
    ///
    /// Write-only registers return the open-bus value; we model open
    /// bus minimally by returning the last byte seen on the PPU bus.
    pub fn read(&mut self, offset: u8) -> u8 {
        let value = match offset {
            register::OAMDATAREAD => self.oam.read(),
            register::VMDATALREAD => self.vram.read_lo(),
            register::VMDATAHREAD => self.vram.read_hi(),
            register::CGDATAREAD => self.cgram.read(),
            // Mode-7 hardware-multiplier result (also driven by
            // M7A / M7B writes outside Mode 7 — used as a fast
            // multiplier).
            register::MPYL => self.mpy_result as u8,
            register::MPYM => (self.mpy_result >> 8) as u8,
            register::MPYH => (self.mpy_result >> 16) as u8,
            register::STAT77 => self.stat77,
            register::STAT78 => {
                // Reading $213F is documented to clear the shared
                // BG-scroll write-twice latch AND the "latch hit"
                // status bit as side effects.
                let v = self.stat78 | if self.external_latch_hit { 0x40 } else { 0 };
                self.bg_scroll_latch = 0;
                self.external_latch_hit = false;
                v
            }
            register::OPHCT => {
                // Read low byte first, then high byte (only bit 0
                // of the high byte is meaningful — H is 9-bit).
                if self.ophct_hi_pending {
                    self.ophct_hi_pending = false;
                    ((self.ophct >> 8) & 0x01) as u8
                } else {
                    self.ophct_hi_pending = true;
                    self.ophct as u8
                }
            }
            register::OPVCT => {
                if self.opvct_hi_pending {
                    self.opvct_hi_pending = false;
                    ((self.opvct >> 8) & 0x01) as u8
                } else {
                    self.opvct_hi_pending = true;
                    self.opvct as u8
                }
            }
            // Everything else: open bus. The renderer's status registers
            // ($2134-$213F apart from those above) will be implemented
            // alongside the renderer.
            _ => self.open_bus,
        };
        self.open_bus = value;
        value
    }

    /// Decode `$2107-$210A BGxSC` (tilemap address + size bits).
    fn set_bg_tilemap(&mut self, bg_idx: usize, value: u8) {
        // bits 2-7: tilemap base in 1 KB units (i.e. 0x400 byte
        // increments = 0x200 word increments).
        let base_words = u16::from(value & 0xFC) << 8;
        self.bg[bg_idx].tilemap_addr_words = base_words;
        self.bg[bg_idx].tilemap_size = value & 0x03;
    }

    /// `$210D/$210F/$2111/$2113` H-scroll write-twice protocol.
    fn write_bg_h_scroll(&mut self, bg_idx: usize, value: u8) {
        // New H scroll = (value << 8) | (prev_latch & ~7) | (h_scroll & 7).
        // Per fullsnes: the bottom 3 bits of the *previous* H scroll
        // value are OR'd back in, and the bottom 5 bits of the latch
        // contribute to bits 0-2 ... actually this is one of the more
        // notoriously buggy SNES PPU behaviours. We model the canonical
        // form used by most games: prev = (value as low) | (latch as high).
        let lo = self.bg_scroll_latch;
        let hi = value;
        self.bg[bg_idx].h_scroll = (u16::from(hi) << 8 | u16::from(lo)) & 0x03FF;
        self.bg_scroll_latch = value;
    }

    /// `$210E/$2110/$2112/$2114` V-scroll write-twice protocol.
    fn write_bg_v_scroll(&mut self, bg_idx: usize, value: u8) {
        let lo = self.bg_scroll_latch;
        let hi = value;
        self.bg[bg_idx].v_scroll = (u16::from(hi) << 8 | u16::from(lo)) & 0x03FF;
        self.bg_scroll_latch = value;
    }

    /// Mode-7 write-twice helper. Returns the new 16-bit value
    /// composed of `(latch_low, value_high)` and advances the
    /// shared latch. The Mode-7 latch is *separate* from the BG
    /// scroll latch — per fullsnes, every M7A-M7D / M7X / M7Y
    /// write feeds (and reads from) this single 8-bit register.
    fn write_m7_pair(&mut self, value: u8) -> u16 {
        let composed = (u16::from(value) << 8) | u16::from(self.m7_latch);
        self.m7_latch = value;
        composed
    }

    /// `$210D` also writes the 13-bit Mode-7 H scroll `M7HOFS`, which
    /// uses the *shared Mode-7 latch* — NOT the BG scroll latch
    /// (ares io.cpp:308-310). The value is `data << 8 | latch`, masked
    /// to 13 bits and sign-extended.
    fn write_m7_hofs(&mut self, value: u8) {
        let raw = self.write_m7_pair(value) & 0x1FFF;
        self.m7_hofs = ((raw as i16) << 3) >> 3;
    }

    /// `$210E` Mode-7 V scroll `M7VOFS` — see [`Self::write_m7_hofs`].
    fn write_m7_vofs(&mut self, value: u8) {
        let raw = self.write_m7_pair(value) & 0x1FFF;
        self.m7_vofs = ((raw as i16) << 3) >> 3;
    }

    /// Latch the current PPU H/V counters into OPHCT/OPVCT. Called
    /// by the `SnesBus` on:
    ///   * a WRIO (\$4201) write whose bit 7 transitions from 0 to 1
    ///   * a read of SLHV (\$2137) — also returns open bus
    ///
    /// Both paths feed the SAME pair of latched values; the read
    /// protocol on OPHCT/OPVCT is separately tracked (low-then-high).
    pub const fn latch_counters(&mut self, h: u16, v: u16) {
        self.ophct = h & 0x1FF;
        self.opvct = v & 0x1FF;
        self.external_latch_hit = true;
    }

    /// Re-run the Mode-7 hardware multiplier. Triggered by writes
    /// to M7A or the high byte of M7B (i.e. the second of the two
    /// M7B writes). Computes `signed(M7A) × signed(M7B_high) →
    /// 24-bit signed result`; reads at MPYL/M/H expose it.
    fn update_mpy(&mut self) {
        // `M7B`'s upper byte is what the hardware uses; that's
        // bits 8..15 of our stored i16, i.e. (self.m7b >> 8) as i8.
        let a = i32::from(self.m7a);
        let b = i32::from((self.m7b >> 8) as i8);
        self.mpy_result = a * b;
    }

    /// Write a PPU register. `offset` is the byte offset from `$2100`.
    pub fn write(&mut self, offset: u8, value: u8) {
        self.open_bus = value;
        match offset {
            register::INIDISP => {
                self.inidisp = value;
                self.inidisp_write_count = self.inidisp_write_count.saturating_add(1);
            }
            register::OBSEL => self.obsel = value,
            register::OAMADDL => self.oam.set_address_low(value),
            register::OAMADDH => self.oam.set_address_high(value),
            register::OAMDATA => self.oam.write_gated(value, !self.active_display),
            register::BGMODE => self.bgmode = value,
            register::MOSAIC => self.mosaic = value,
            register::BG1SC => self.set_bg_tilemap(0, value),
            register::BG2SC => self.set_bg_tilemap(1, value),
            register::BG3SC => self.set_bg_tilemap(2, value),
            register::BG4SC => self.set_bg_tilemap(3, value),
            register::BG12NBA => {
                self.bg[0].char_addr_words = u16::from(value & 0x0F) << 12;
                self.bg[1].char_addr_words = u16::from((value >> 4) & 0x0F) << 12;
            }
            register::BG34NBA => {
                self.bg[2].char_addr_words = u16::from(value & 0x0F) << 12;
                self.bg[3].char_addr_words = u16::from((value >> 4) & 0x0F) << 12;
            }
            register::BG1HOFS => {
                // $210D feeds both the BG1 (10-bit) and Mode-7 (13-bit)
                // scrolls, each with its own latch.
                self.write_bg_h_scroll(0, value);
                self.write_m7_hofs(value);
            }
            register::BG1VOFS => {
                self.write_bg_v_scroll(0, value);
                self.write_m7_vofs(value);
            }
            register::BG2HOFS => self.write_bg_h_scroll(1, value),
            register::BG2VOFS => self.write_bg_v_scroll(1, value),
            register::BG3HOFS => self.write_bg_h_scroll(2, value),
            register::BG3VOFS => self.write_bg_v_scroll(2, value),
            register::BG4HOFS => self.write_bg_h_scroll(3, value),
            register::BG4VOFS => self.write_bg_v_scroll(3, value),
            register::M7SEL => self.m7sel = value,
            register::M7A => {
                self.m7a = self.write_m7_pair(value) as i16;
                self.update_mpy();
            }
            register::M7B => {
                self.m7b = self.write_m7_pair(value) as i16;
                self.update_mpy();
            }
            register::M7C => self.m7c = self.write_m7_pair(value) as i16,
            register::M7D => self.m7d = self.write_m7_pair(value) as i16,
            register::M7X => {
                // 13-bit signed value, sign-extended for arithmetic.
                let raw = self.write_m7_pair(value) & 0x1FFF;
                self.m7x = ((raw as i16) << 3) >> 3;
            }
            register::M7Y => {
                let raw = self.write_m7_pair(value) & 0x1FFF;
                self.m7y = ((raw as i16) << 3) >> 3;
            }
            register::SETINI => self.setini = value,
            register::VMAIN => self.vram.vmain = VmainSettings::from_byte(value),
            register::VMADDL => {
                let hi = (self.vram.address >> 8) as u8;
                self.vram.set_address(value, hi);
            }
            register::VMADDH => {
                let lo = self.vram.address as u8;
                self.vram.set_address(lo, value);
            }
            register::VMDATAL => self.vram.write_lo_gated(value, !self.active_display),
            register::VMDATAH => self.vram.write_hi_gated(value, !self.active_display),
            register::CGADD => self.cgram.set_address(value),
            register::CGDATA => self.cgram.write_gated(value, !self.active_display),
            register::W12SEL => self.w12sel = value,
            register::W34SEL => self.w34sel = value,
            register::WOBJSEL => self.wobjsel = value,
            register::WH0 => self.wh0 = value,
            register::WH1 => self.wh1 = value,
            register::WH2 => self.wh2 = value,
            register::WH3 => self.wh3 = value,
            register::WBGLOG => self.wbglog = value,
            register::WOBJLOG => self.wobjlog = value,
            register::TM => self.tm = value,
            register::TS => self.ts = value,
            register::TMW => self.tmw = value,
            register::TSW => self.tsw = value,
            register::CGWSEL => self.cgwsel = value,
            register::CGADSUB => self.cgadsub = value,
            register::COLDATA => {
                // Multiple channels can be selected per write (the
                // high 3 bits act as a per-channel write-enable mask),
                // and writes ACCUMULATE — drivers set R then G then B
                // with three sequential writes, and reads of any
                // unaddressed channel are preserved.
                let intensity = value & 0x1F;
                if value & 0x20 != 0 {
                    self.coldata_r = intensity;
                }
                if value & 0x40 != 0 {
                    self.coldata_g = intensity;
                }
                if value & 0x80 != 0 {
                    self.coldata_b = intensity;
                }
            }
            // FALLTHROUGH for unmodelled registers.
            // Other registers are stored as raw bytes for now (BG
            // scroll, window state, etc. — wired here in P1.4+).
            _ => {
                // Drop silently; we'll wire each register as the
                // renderer needs it.
            }
        }
    }
}

#[cfg(test)]
mod stat_tests {
    use super::*;

    #[test]
    fn stat77_returns_chip_id_1() {
        let mut p = Ppu::new();
        assert_eq!(p.read(register::STAT77) & 0x0F, 0x01);
    }

    #[test]
    fn stat78_returns_chip_rev_2_and_ntsc_by_default() {
        let mut p = Ppu::new();
        let v = p.read(register::STAT78);
        assert_eq!(v & 0x0F, 0x02, "chip rev = 2");
        assert_eq!(v & 0x10, 0x00, "region bit clear = NTSC");
    }

    #[test]
    fn stat78_read_clears_bg_scroll_latch() {
        let mut p = Ppu::new();
        // Push a value into the BG scroll latch by writing BG1HOFS once.
        p.write(register::BG1HOFS, 0x42);
        // Reading STAT78 should reset the latch — verified
        // indirectly by checking the next BG scroll write doesn't
        // see the stale 0x42 in the high byte.
        let _ = p.read(register::STAT78);
        // Write a single BG2HOFS value; if the latch was cleared,
        // BG2 H scroll's high byte sees 0.
        p.write(register::BG2HOFS, 0x10);
        // h_scroll = (value << 8 | latch) & 0x3FF.
        // latch was cleared → expect 0x1000 & 0x3FF = 0.
        // (The 0x10 byte becomes the high byte of a 10-bit value
        // → bits 8-9 of that byte land in the scroll.)
        assert_eq!(p.bg[1].h_scroll & 0xFF, 0x00, "low byte = cleared latch");
    }

    #[test]
    fn setini_stores_byte_verbatim() {
        let mut p = Ppu::new();
        p.write(register::SETINI, 0x55);
        assert_eq!(p.setini, 0x55);
    }

    #[test]
    fn ophct_opvct_use_write_twice_read_protocol() {
        let mut p = Ppu::new();
        p.latch_counters(0x123, 0x0AB);
        // First read = low byte; second = bit 0 of high byte.
        assert_eq!(p.read(register::OPHCT), 0x23);
        assert_eq!(p.read(register::OPHCT), 0x01); // high bit 0 of 0x123
        // Third read cycles back to low again.
        assert_eq!(p.read(register::OPHCT), 0x23);
        // OPVCT independent.
        assert_eq!(p.read(register::OPVCT), 0xAB);
        assert_eq!(p.read(register::OPVCT), 0x00); // 0x0AB high bit 0
    }

    #[test]
    fn latch_counters_sets_latch_hit_bit_visible_via_stat78() {
        let mut p = Ppu::new();
        // Before any latch the hit bit is clear.
        assert_eq!(p.read(register::STAT78) & 0x40, 0);
        p.latch_counters(100, 50);
        let v = p.read(register::STAT78);
        assert_eq!(v & 0x40, 0x40, "latch hit should be set");
        // Reading STAT78 clears the bit.
        assert_eq!(p.read(register::STAT78) & 0x40, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vram_upload_round_trip_via_registers() {
        let mut p = Ppu::new();
        // Set VRAM address to 0x1000 (word address), increment on
        // high-byte write.
        p.write(register::VMAIN, 0x80);
        p.write(register::VMADDL, 0x00);
        p.write(register::VMADDH, 0x10);
        // Write a 16-bit word: $BBAA → byte $AA at $2000, $BB at $2001.
        p.write(register::VMDATAL, 0xAA);
        p.write(register::VMDATAH, 0xBB);
        // Verify direct VRAM contents (byte address = word << 1).
        assert_eq!(p.vram.peek(0x2000), 0xAA);
        assert_eq!(p.vram.peek(0x2001), 0xBB);
        // Verify the word address advanced.
        assert_eq!(p.vram.address, 0x1001);
    }

    #[test]
    fn cgram_palette_upload_via_registers() {
        let mut p = Ppu::new();
        p.write(register::CGADD, 0x00);
        p.write(register::CGDATA, 0x1F); // low byte of color 0
        p.write(register::CGDATA, 0x00); // high byte → commit + advance
        assert_eq!(p.cgram.color(0), 0x001F);
        assert_eq!(p.cgram.address, 1);
    }

    #[test]
    fn oam_low_table_via_registers() {
        let mut p = Ppu::new();
        p.write(register::OAMADDL, 0x00);
        p.write(register::OAMADDH, 0x00);
        p.write(register::OAMDATA, 0x10);
        p.write(register::OAMDATA, 0x20);
        assert_eq!(p.oam.peek(0), 0x10);
        assert_eq!(p.oam.peek(1), 0x20);
    }

    #[test]
    fn inidisp_write_count_tracks_writes() {
        let mut p = Ppu::new();
        assert_eq!(p.inidisp_write_count, 0);
        p.write(register::INIDISP, 0x80);
        p.write(register::INIDISP, 0x0F);
        p.write(register::INIDISP, 0x00);
        assert_eq!(p.inidisp_write_count, 3);
        // Writes to *other* registers don't bump the counter.
        p.write(register::BGMODE, 0xAB);
        assert_eq!(p.inidisp_write_count, 3);
    }

    #[test]
    fn read_write_only_register_returns_open_bus() {
        let mut p = Ppu::new();
        // $2100 (INIDISP) is write-only. We write a known value and
        // then read another write-only address — it should return that
        // open-bus value.
        p.write(register::BGMODE, 0xAB);
        assert_eq!(p.read(register::INIDISP), 0xAB);
    }

    #[test]
    fn vram_read_register_round_trip() {
        let mut p = Ppu::new();
        // Seed VRAM directly.
        p.vram.poke(0x0000, 0x42);
        p.vram.poke(0x0001, 0x84);
        // Address & VMAIN.
        p.write(register::VMAIN, 0x00); // step 1, inc on low
        p.write(register::VMADDL, 0x00);
        p.write(register::VMADDH, 0x00); // triggers prefetch
        // First reads return the pre-fetched bytes.
        assert_eq!(p.read(register::VMDATALREAD), 0x42);
        assert_eq!(p.read(register::VMDATAHREAD), 0x84);
    }

    /// Stand up a PPU rendering a solid-red BG1 tile across the
    /// whole screen — used by several G6 per-scanline tests.
    fn ppu_with_solid_bg1_tile() -> Ppu {
        let mut p = Ppu::new();
        p.write(register::INIDISP, 0x0F); // unblanked, full brightness
        p.write(register::BGMODE, 0x01); // mode 1
        p.write(register::TM, 0x01); // BG1 on main
        p.write(0x07, 0x00); // BG1SC: tilemap at word $0000
        p.write(0x0B, 0x01); // BG12NBA: BG1 char base = word $1000
        // Tilemap entry 0 at byte $0000: tile 0, palette 0.
        p.vram.poke(0x0000, 0x00);
        p.vram.poke(0x0001, 0x00);
        // Tile 0 at VRAM byte $2000: each row = all colour 1 (2bpp).
        //   pix:  1 1 1 1 1 1 1 1
        //   bit0: 1 1 1 1 1 1 1 1 → 0xFF
        //   bit1: 0 0 0 0 0 0 0 0 → 0x00
        for row in 0..8 {
            p.vram.poke(0x2000 + row * 2, 0xFF);
            p.vram.poke(0x2001 + row * 2, 0x00);
        }
        // CGRAM[1] = red ($001F).
        p.cgram.poke(2, 0x1F);
        p.cgram.poke(3, 0x00);
        // CGRAM[0] = backdrop = black ($0000), default.
        p
    }

    #[test]
    fn partial_flush_splits_one_scanline_at_dot_x() {
        // Gap G6 Phase 2 — intra-line partial flush. Build a BG1 red
        // line. Render the first 128 dots with COLDATA=0, then turn
        // on CGADSUB+BG1 math + COLDATA blue and finish the line.
        // Result: left half pure red, right half red + COLDATA blue.
        let mut p = ppu_with_solid_bg1_tile();
        // First half — no math.
        p.flush_partial_scanline(10, 128, RenderOptions::default());
        assert_eq!(p.last_flushed_dot, 128);
        // Enable math + add blue.
        p.write(register::CGADSUB, 0x01); // BG1 add, no halve
        p.write(register::COLDATA, 0x9F); // B = max
        // Finish the line.
        p.render_current_scanline(10, RenderOptions::default());
        assert_eq!(p.last_flushed_dot, 0, "scanline_reset clears cursor");
        // Left half (dot 0): no blue. Right half (dot 200): blue.
        let row_start = 10 * crate::FRAME_W;
        assert_eq!(
            p.framebuffer()[row_start][2],
            0,
            "left half rendered before COLDATA write should have no blue"
        );
        assert!(
            p.framebuffer()[row_start + 200][2] > 0,
            "right half rendered after COLDATA write should have COLDATA blue"
        );
    }

    #[test]
    fn partial_flush_clamps_end_x_and_is_idempotent() {
        // end_x > FRAME_W is clamped; end_x <= last_flushed_dot is a no-op.
        let mut p = ppu_with_solid_bg1_tile();
        p.flush_partial_scanline(20, 50, RenderOptions::default());
        assert_eq!(p.last_flushed_dot, 50);
        // Same end_x → no-op.
        p.flush_partial_scanline(20, 50, RenderOptions::default());
        assert_eq!(p.last_flushed_dot, 50);
        // Smaller end_x → no-op.
        p.flush_partial_scanline(20, 20, RenderOptions::default());
        assert_eq!(p.last_flushed_dot, 50);
        // end_x > FRAME_W clamps to FRAME_W.
        p.flush_partial_scanline(20, 1000, RenderOptions::default());
        assert_eq!(p.last_flushed_dot, crate::FRAME_W as u16);
    }

    #[test]
    fn per_scanline_picks_up_mid_frame_tm_change() {
        // Render lines 0..100 with TM=BG1 on, then disable BG1, then
        // render 100..224. The persistent framebuffer should show BG1
        // red on the top, backdrop black on the bottom — proving that
        // a TM change between scanlines is honoured.
        let mut p = ppu_with_solid_bg1_tile();
        for y in 0..100u16 {
            p.render_current_scanline(y, RenderOptions::default());
        }
        p.write(register::TM, 0x00); // BG1 off
        for y in 100..crate::FRAME_H as u16 {
            p.render_current_scanline(y, RenderOptions::default());
        }
        // Top half: BG1 red (non-black).
        assert_ne!(p.framebuffer()[0], [0, 0, 0], "line 0 should show BG1");
        assert_ne!(
            p.framebuffer()[99 * crate::FRAME_W],
            [0, 0, 0],
            "line 99 should still show BG1"
        );
        // Bottom half: backdrop black.
        assert_eq!(
            p.framebuffer()[100 * crate::FRAME_W],
            [0, 0, 0],
            "line 100 should be backdrop after TM=0"
        );
        assert_eq!(
            p.framebuffer()[(crate::FRAME_H - 1) * crate::FRAME_W],
            [0, 0, 0],
            "last line should be backdrop"
        );
    }

    #[test]
    fn per_scanline_cgwsel_change_splits_color_math() {
        // With BG1 on both main and sub, CGADSUB add-no-halve and
        // COLDATA = max blue: cgwsel bit 1 = 0 adds blue COLDATA;
        // cgwsel bit 1 = 1 uses the sub winner (BG1 red) so blue
        // stays 0. Toggle at line 100 and verify the split.
        let mut p = ppu_with_solid_bg1_tile();
        p.write(register::TS, 0x01); // BG1 on sub too
        p.write(register::CGADSUB, 0x01); // BG1 math add, no halve
        p.write(register::COLDATA, 0x9F); // B = max
        p.write(register::CGWSEL, 0x00); // bit 1 = 0 → operand = COLDATA blue
        for y in 0..100u16 {
            p.render_current_scanline(y, RenderOptions::default());
        }
        p.write(register::CGWSEL, 0x02); // bit 1 = 1 → operand = sub pixel (red)
        for y in 100..crate::FRAME_H as u16 {
            p.render_current_scanline(y, RenderOptions::default());
        }
        // Line 0..99: blue channel non-zero (COLDATA blue added).
        // Line 100..: blue channel zero (operand was red, not blue).
        let top_blue = p.framebuffer()[0][2];
        let bottom_blue = p.framebuffer()[150 * crate::FRAME_W][2];
        assert!(
            top_blue > 0,
            "top half should have COLDATA blue, got {top_blue}"
        );
        assert_eq!(
            bottom_blue, 0,
            "bottom half should have zero blue (sub pixel was red)"
        );
    }

    #[test]
    fn framebuffer_persists_force_blank_lines_as_black() {
        // Render line 50 with display on, then force-blank, then
        // render line 51. Line 50 should be non-black, line 51 black.
        let mut p = ppu_with_solid_bg1_tile();
        p.render_current_scanline(50, RenderOptions::default());
        assert_ne!(
            p.framebuffer()[50 * crate::FRAME_W],
            [0, 0, 0],
            "line 50 rendered with display on should be visible"
        );
        p.write(register::INIDISP, 0x80); // force-blank on
        p.render_current_scanline(51, RenderOptions::default());
        assert_eq!(
            p.framebuffer()[51 * crate::FRAME_W],
            [0, 0, 0],
            "line 51 rendered during force-blank must be black"
        );
        // Line 50 must still be visible (the force-blank only affects
        // lines rendered after it landed).
        assert_ne!(
            p.framebuffer()[50 * crate::FRAME_W],
            [0, 0, 0],
            "line 50 must persist across the force-blank toggle"
        );
    }

    #[test]
    fn framebuffer_default_is_all_zero_and_render_scanline_writes_into_it() {
        // Powered-on PPU starts with forced blank ($2100 bit 7 = 1)
        // and an all-zero framebuffer.
        let mut p = Ppu::new();
        assert_eq!(p.framebuffer().len(), crate::FRAME_W * crate::FRAME_H);
        assert!(p.framebuffer().iter().all(|px| *px == [0, 0, 0]));

        // Forced-blank scanline render still produces all-zero output,
        // and the framebuffer remains all-zero.
        p.render_current_scanline(0, RenderOptions::default());
        assert!(
            p.framebuffer()[..crate::FRAME_W]
                .iter()
                .all(|px| *px == [0, 0, 0]),
            "force-blanked scanline must write black into the framebuffer"
        );

        // Disable forced blank, max brightness, put a non-zero backdrop
        // colour (CGRAM[0]) so the scanline renders a visible value.
        p.write(register::INIDISP, 0x0F);
        p.cgram.poke(0, 0xFF); // CGRAM[0] = $00FF → BGR555 R = 31
        p.cgram.poke(1, 0x7F);
        p.render_current_scanline(42, RenderOptions::default());
        // Line 42 now holds the backdrop colour; lines 0..41 are still
        // black (forced-blank render written above) / never touched.
        let off = 42 * crate::FRAME_W;
        let pixel = p.framebuffer()[off];
        assert_ne!(pixel, [0, 0, 0], "scanline 42 should now be non-zero");
        // Out-of-range y is a no-op (doesn't panic).
        p.render_current_scanline(crate::FRAME_H as u16, RenderOptions::default());
    }
}
