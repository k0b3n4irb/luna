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
