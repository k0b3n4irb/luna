//! VRAM provenance: who last wrote each 32-byte tile of VRAM, and a
//! per-frame check of the sprite tiles the picture is about to draw.
//!
//! Observation only: turning it on never changes a pixel. Off by default
//! ([`Ppu::enable_vram_provenance`]), one `Option` test on the write path.
//!
//! The tag is set at the `$2118` / `$2119` arm, from the byte the gated
//! write really committed: a write the active display dropped tags nothing
//! (a real console loses it too). The writer's identity (CPU or DMA, PC,
//! source, channel) is set by the system glue just before the write
//! ([`Ppu::set_vram_writer`]).
//!
//! The check runs once per frame, at the end of the first visible line
//! ([`Ppu::vram_provenance_check`]). Two signals, both about sprite
//! (OBJ) tiles, the 4 bpp tiles `OBSEL` addresses:
//!
//! * **S1** — the sprite draws a tile no write ever touched since the
//!   journal was switched on, and the tile holds a non-zero pixel. The
//!   contents come from the power-on state of VRAM, which a console does
//!   not define.
//! * **S4** — the tile was last written while `OBSEL` addressed OBJ
//!   character data differently (name base or name select, bits 0-4) than
//!   it does now: the upload was made for another layout.
//!
//! Approximations of the check, on purpose: the 32-sprites / 34-slivers
//! per-line limits are not modelled (a tile of a sprite the line drops
//! still counts), and a vertically flipped sprite taller than wide is
//! judged visible as a whole. Start the journal before the ROM runs: a
//! tile written before it was on reads as never written.

use std::collections::HashSet;

use crate::ppu::Ppu;
use crate::renderer::{decode_all_sprites, sprite_tile_byte_offset};

/// Distinct reports kept before further new ones are only counted.
pub const VRAM_PROV_MAX_REPORTS: usize = 1024;

/// Bytes of one 4 bpp tile.
const TILE_BYTES: usize = 32;
/// Tiles in 64 KB of VRAM.
const TILES: usize = 0x1_0000 / TILE_BYTES;
/// `OBSEL` bits that decide where OBJ character data is (name base and
/// name select); the size bits (5-7) do not move any tile.
const OBSEL_ADDRESS_BITS: u8 = 0x1F;

/// Who made a VRAM write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VramWriterKind {
    /// An instruction's own `$2118` / `$2119` write.
    #[default]
    Cpu,
    /// A general-purpose DMA burst.
    Dma,
    /// An HDMA transfer.
    Hdma,
    /// A debugger poke (`poke_vram`), not a bus write.
    Poke,
}

/// The identity behind a VRAM write, set by the system glue before it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VramWriter {
    /// CPU, DMA, HDMA or a poke.
    pub kind: VramWriterKind,
    /// 24-bit address of the instruction (for a DMA, the one that started
    /// the transfer).
    pub pc: u32,
    /// A DMA's A-bus source address of the byte; 0 for a CPU write.
    pub source: u32,
    /// The DMA channel (0-7); 0 for a CPU write.
    pub channel: u8,
    /// Frame of the write.
    pub frame: u64,
}

/// What is known of the last write that touched a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VramTileTag {
    /// The writer of the last write.
    pub writer: VramWriter,
    /// `OBSEL` when that write was made.
    pub obsel: u8,
    /// Which of the tile's 32 bytes were ever written (bit n = byte n).
    pub written: u32,
}

/// Why a sprite tile is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VramProvReason {
    /// S1 — drawn, non-zero, never written.
    NeverWritten,
    /// S4 — written under another `OBSEL` address layout.
    ObselChanged,
}

impl VramProvReason {
    /// Short stable code, `S1` / `S4`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NeverWritten => "S1",
            Self::ObselChanged => "S4",
        }
    }

    /// One-line statement of the signal.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::NeverWritten => {
                "sprite tile drawn with non-zero pixels but never written: its content is the power-on VRAM"
            }
            Self::ObselChanged => {
                "sprite tile written under another OBSEL character layout than the one that draws it"
            }
        }
    }
}

/// One distinct report: a (reason, tile) pair, with its first sprite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VramProvReport {
    /// The signal.
    pub reason: VramProvReason,
    /// Byte address in VRAM of the tile (a multiple of 32).
    pub vram_addr: u16,
    /// OAM index of the first sprite that drew it.
    pub sprite: u8,
    /// That sprite's 9-bit OAM character number.
    pub sprite_tile: u16,
    /// Frame of the check.
    pub frame: u64,
    /// `OBSEL` at the check.
    pub obsel: u8,
    /// The tile's last write, `None` when it was never written.
    pub last_write: Option<VramTileTag>,
}

/// The journal: a tag per tile and the reports.
pub(crate) struct VramProv {
    tags: Vec<Option<VramTileTag>>,
    seen: HashSet<(VramProvReason, u16)>,
    reports: Vec<VramProvReport>,
    /// New distinct reports not kept because the journal was full.
    overflow: u64,
}

impl VramProv {
    fn new() -> Self {
        Self {
            tags: vec![None; TILES],
            seen: HashSet::new(),
            reports: Vec::new(),
            overflow: 0,
        }
    }

    /// Tag the byte at VRAM byte address `addr`.
    fn tag(&mut self, addr: u16, writer: VramWriter, obsel: u8) {
        let tile = usize::from(addr) / TILE_BYTES;
        let bit = 1u32 << (usize::from(addr) % TILE_BYTES);
        let written = self.tags[tile].map_or(0, |t| t.written) | bit;
        self.tags[tile] = Some(VramTileTag {
            writer,
            obsel,
            written,
        });
    }

    fn report(&mut self, r: VramProvReport) {
        if !self.seen.insert((r.reason, r.vram_addr)) {
            return;
        }
        if self.reports.len() >= VRAM_PROV_MAX_REPORTS {
            self.overflow += 1;
        } else {
            self.reports.push(r);
        }
    }
}

impl Ppu {
    /// Start tagging VRAM writes and checking sprite tiles (`true`, with
    /// every tile untagged), or stop and drop the journal (`false`).
    pub fn enable_vram_provenance(&mut self, on: bool) {
        self.prov = on.then(|| Box::new(VramProv::new()));
    }

    /// Is the provenance journal on?
    #[must_use]
    pub const fn vram_provenance_enabled(&self) -> bool {
        self.prov.is_some()
    }

    /// Name the writer of the VRAM writes that follow (the system glue
    /// calls this before a bus write to `$2118` / `$2119`).
    pub const fn set_vram_writer(&mut self, writer: VramWriter) {
        self.prov_writer = writer;
    }

    /// Tag the VRAM byte at `addr` as written by the current writer; the
    /// caller passes the byte the gated write committed.
    pub(crate) fn note_vram_write(&mut self, addr: usize) {
        let (writer, obsel) = (self.prov_writer, self.obsel);
        if let Some(p) = self.prov.as_deref_mut() {
            p.tag(addr as u16, writer, obsel);
        }
    }

    /// Tag a direct poke of `addr` (a debugger write, not a bus write).
    pub fn note_vram_poke(&mut self, addr: u16, frame: u64) {
        let (obsel, writer) = (
            self.obsel,
            VramWriter {
                kind: VramWriterKind::Poke,
                frame,
                ..VramWriter::default()
            },
        );
        if let Some(p) = self.prov.as_deref_mut() {
            p.tag(addr, writer, obsel);
        }
    }

    /// Drain the reports (distinct (reason, tile) pairs, in order of first
    /// occurrence) and how many new ones were dropped for lack of room. A
    /// pair reported once is not reported again after a drain.
    pub fn take_vram_provenance(&mut self) -> (Vec<VramProvReport>, u64) {
        self.prov.as_deref_mut().map_or_else(
            || (Vec::new(), 0),
            |p| {
                (
                    std::mem::take(&mut p.reports),
                    std::mem::take(&mut p.overflow),
                )
            },
        )
    }

    /// The per-frame check: judge the tiles of the sprites the picture is
    /// about to draw. `frame` stamps the reports. Reads only.
    pub fn vram_provenance_check(&mut self, frame: u64) {
        let Some(mut prov) = self.prov.take() else {
            return;
        };
        if self.inidisp & 0x80 == 0 {
            self.check_sprites(&mut prov, frame);
        }
        self.prov = Some(prov);
    }

    fn check_sprites(&self, prov: &mut VramProv, frame: u64) {
        let height = self.frame_height() as u16;
        for (idx, sp) in decode_all_sprites(self).iter().enumerate() {
            let (cols, rows) = (sp.w / 8, sp.h / 8);
            if sp.x + sp.w as i16 <= 0 || sp.x >= 256 {
                continue;
            }
            // Screen line of the sprite's row `r`: Y + r, lines 1..=height.
            let line_visible = |r: u16| {
                let l = (u16::from(sp.y) + r) & 0xFF;
                (1..=height).contains(&l)
            };
            if !(0..sp.h).any(line_visible) {
                continue;
            }
            for ty in 0..rows {
                for tx in 0..cols {
                    // Where the tile lands once flipped. The vertical flip
                    // of a taller-than-wide sprite is irregular: judged
                    // visible as a whole.
                    let sx =
                        i32::from(sp.x) + 8 * i32::from(if sp.h_flip { cols - 1 - tx } else { tx });
                    if sx + 8 <= 0 || sx >= 256 {
                        continue;
                    }
                    if !(sp.v_flip && sp.w != sp.h) {
                        let ry = if sp.v_flip { rows - 1 - ty } else { ty };
                        if !(0..8).any(|i| line_visible(ry * 8 + i)) {
                            continue;
                        }
                    }
                    // Nibble-wrapped component tile, name page held
                    // (renderer: ares object.cpp:130-146).
                    let char_byte = sp.tile & 0xFF;
                    let col_nib = ((char_byte & 0x0F) + tx) & 0x0F;
                    let row_nib = ((char_byte >> 4) + ty) & 0x0F;
                    let id = (sp.tile & 0x100) | (row_nib << 4) | col_nib;
                    let off = sprite_tile_byte_offset(self.obsel, id);
                    self.judge_tile(prov, off, idx as u8, sp.tile, frame);
                }
            }
        }
    }

    fn judge_tile(
        &self,
        prov: &mut VramProv,
        off: usize,
        sprite: u8,
        sprite_tile: u16,
        frame: u64,
    ) {
        let tag = prov.tags[off / TILE_BYTES];
        // A zero tile draws nothing: no pixel can be wrong.
        if (0..TILE_BYTES).all(|i| self.vram.peek(((off + i) & 0xFFFF) as u16) == 0) {
            return;
        }
        let reason = match tag {
            None => VramProvReason::NeverWritten,
            Some(t) if (t.obsel ^ self.obsel) & OBSEL_ADDRESS_BITS != 0 => {
                VramProvReason::ObselChanged
            }
            Some(_) => return,
        };
        prov.report(VramProvReport {
            reason,
            vram_addr: off as u16,
            sprite,
            sprite_tile,
            frame,
            obsel: self.obsel,
            last_write: tag,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::register;

    /// A PPU with every sprite parked off-screen except #0 at (10, 20)
    /// with character `tile`, the screen on, the journal on. `fill` is
    /// poked into VRAM at `at` BEFORE the journal starts (power-on
    /// content, which no write explains).
    fn ppu_with_sprite(tile: u8, at: u16, fill: u8) -> Ppu {
        let mut p = Ppu::new();
        p.inidisp = 0x0F;
        for i in 0..128u16 {
            p.oam.poke(i * 4 + 1, 0xF0);
        }
        p.oam.poke(0, 10);
        p.oam.poke(1, 20);
        p.oam.poke(2, tile);
        for i in 0..32 {
            p.vram.poke(at + i, fill);
        }
        p.enable_vram_provenance(true);
        p
    }

    fn upload(p: &mut Ppu, word: u16, bytes: usize) {
        p.write(register::VMAIN, 0x80);
        p.write(register::VMADDL, word as u8);
        p.write(register::VMADDH, (word >> 8) as u8);
        for _ in 0..bytes / 2 {
            p.write(register::VMDATAL, 0xFF);
            p.write(register::VMDATAH, 0xFF);
        }
    }

    fn reports(p: &mut Ppu, frame: u64) -> Vec<VramProvReport> {
        p.vram_provenance_check(frame);
        p.take_vram_provenance().0
    }

    #[test]
    fn a_never_written_tile_is_flagged_and_a_dma_d_one_is_clean() {
        let mut p = ppu_with_sprite(0, 0, 0xAA);
        let r = reports(&mut p, 3);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(
            (r[0].reason, r[0].vram_addr, r[0].sprite, r[0].frame),
            (VramProvReason::NeverWritten, 0, 0, 3)
        );
        assert!(r[0].last_write.is_none());
        // Reported once: neither the next frame nor a drain repeats it.
        assert!(reports(&mut p, 4).is_empty());

        let mut q = ppu_with_sprite(0, 0, 0xAA);
        q.set_vram_writer(VramWriter {
            kind: VramWriterKind::Dma,
            pc: 0x00_8123,
            source: 0x7E_0200,
            channel: 3,
            frame: 1,
        });
        upload(&mut q, 0, 32);
        assert!(reports(&mut q, 2).is_empty());
        let tag = q.prov.as_ref().unwrap().tags[0].unwrap();
        assert_eq!(tag.written, u32::MAX);
        assert_eq!(tag.writer.channel, 3);
        assert_eq!(tag.writer.source, 0x7E_0200);
    }

    #[test]
    fn a_blank_tile_never_written_draws_nothing_wrong() {
        let mut p = ppu_with_sprite(0, 0, 0);
        assert!(reports(&mut p, 1).is_empty());
    }

    #[test]
    fn a_write_dropped_by_the_active_display_tags_nothing() {
        let mut p = ppu_with_sprite(0, 0, 0xAA);
        p.active_display = true;
        upload(&mut p, 0, 32);
        assert!(p.prov.as_ref().unwrap().tags.iter().all(Option::is_none));
        assert_eq!(p.vram.peek(0), 0xAA, "the write was dropped");
        let r = reports(&mut p, 1);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].reason, VramProvReason::NeverWritten);
        // Allowed again (VBlank): the same writes tag.
        p.active_display = false;
        upload(&mut p, 0, 32);
        assert_eq!(p.prov.as_ref().unwrap().tags[0].unwrap().written, u32::MAX);
    }

    #[test]
    fn vmain_remap_and_increment_modes_tag_the_byte_written() {
        let mut p = Ppu::new();
        p.enable_vram_provenance(true);
        // Remap mode 1, increment on the low byte; word 3 -> word 0x18
        // -> byte 0x30 (tile 1, byte 16); then word 4 -> byte 0x40.
        p.write(register::VMAIN, 0x04);
        p.write(register::VMADDL, 3);
        p.write(register::VMADDH, 0);
        p.write(register::VMDATAL, 1);
        p.write(register::VMDATAL, 2);
        let tags = &p.prov.as_ref().unwrap().tags;
        assert_eq!(tags[1].unwrap().written, 1 << 16);
        assert_eq!(tags[2].unwrap().written, 1);
        // Increment on the high byte: the high write lands on byte + 1.
        p.write(register::VMAIN, 0x84);
        p.write(register::VMADDL, 3);
        p.write(register::VMADDH, 0);
        p.write(register::VMDATAH, 9);
        let tags = &p.prov.as_ref().unwrap().tags;
        assert_eq!(tags[1].unwrap().written, (1 << 16) | (1 << 17));
        assert_eq!(p.vram.peek(0x31), 9);
    }

    #[test]
    fn a_tile_written_under_another_obsel_layout_is_flagged_s4() {
        // Tile at byte $4000 uploaded while OBSEL = 0 ...
        let mut p = ppu_with_sprite(0, 0x4000, 0);
        upload(&mut p, 0x2000, 32);
        // ... then OBSEL points the sprite's page 0 there.
        p.write(register::OBSEL, 0x01);
        let r = reports(&mut p, 5);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(
            (r[0].reason, r[0].vram_addr, r[0].obsel),
            (VramProvReason::ObselChanged, 0x4000, 0x01)
        );
        assert_eq!(r[0].last_write.unwrap().obsel, 0);
    }

    #[test]
    fn only_the_address_bits_of_obsel_count_for_s4() {
        let mut p = ppu_with_sprite(0, 0, 0);
        upload(&mut p, 0, 32);
        p.write(register::OBSEL, 0x60); // sprite size only
        assert!(reports(&mut p, 1).is_empty());
    }

    #[test]
    fn nothing_is_judged_in_forced_blank_or_when_off() {
        let mut p = ppu_with_sprite(0, 0, 0xAA);
        p.inidisp = 0x80;
        assert!(reports(&mut p, 1).is_empty());
        p.inidisp = 0x0F;
        p.enable_vram_provenance(false);
        p.vram_provenance_check(1);
        assert!(!p.vram_provenance_enabled());
        assert!(p.take_vram_provenance().0.is_empty());
    }

    #[test]
    fn a_sprite_off_screen_is_not_judged() {
        let mut p = ppu_with_sprite(0, 0, 0xAA);
        p.oam.poke(1, 0xF0); // parked
        assert!(reports(&mut p, 1).is_empty());
    }
}
