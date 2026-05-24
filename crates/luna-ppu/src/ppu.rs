//! [`Ppu`] register dispatch.
//!
//! The PPU is memory-mapped at `$00-$3F:$2100-$213F` (and the mirror in
//! `$80-$BF`). This module exposes [`Ppu::write`] / [`Ppu::read`]
//! taking the **low 6 bits of the offset** (`0x00`-`0x3F`) — the bus
//! is responsible for the bank/region routing.

use crate::memory::{Cgram, Oam, VmainSettings, Vram};

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
pub fn bg_state(ppu: &Ppu, idx: usize) -> BgState {
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
    pub w34sel: u8,
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
            bg_scroll_latch: 0,
            open_bus: 0,
            inidisp_write_count: 0,
        }
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
            register::OAMDATA => self.oam.write(value),
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
            register::BG1HOFS => self.write_bg_h_scroll(0, value),
            register::BG1VOFS => self.write_bg_v_scroll(0, value),
            register::BG2HOFS => self.write_bg_h_scroll(1, value),
            register::BG2VOFS => self.write_bg_v_scroll(1, value),
            register::BG3HOFS => self.write_bg_h_scroll(2, value),
            register::BG3VOFS => self.write_bg_v_scroll(2, value),
            register::BG4HOFS => self.write_bg_h_scroll(3, value),
            register::BG4VOFS => self.write_bg_v_scroll(3, value),
            register::VMAIN => self.vram.vmain = VmainSettings::from_byte(value),
            register::VMADDL => {
                let hi = (self.vram.address >> 8) as u8;
                self.vram.set_address(value, hi);
            }
            register::VMADDH => {
                let lo = self.vram.address as u8;
                self.vram.set_address(lo, value);
            }
            register::VMDATAL => self.vram.write_lo(value),
            register::VMDATAH => self.vram.write_hi(value),
            register::CGADD => self.cgram.set_address(value),
            register::CGDATA => self.cgram.write(value),
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
}
