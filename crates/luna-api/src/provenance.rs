//! VRAM provenance (`luna strict` rules S1 and S4, MCP
//! `take_vram_provenance`): which sprite tiles the picture draws that no
//! write ever filled (S1) or that were uploaded under another `OBSEL`
//! layout (S4), with the CPU / DMA write that last touched the tile.
//! Observation only: switching it on never changes a frame. The tagging
//! and the check live in `luna_ppu`.

use luna_ppu::{VramProvReason, VramWriterKind};
use schemars::JsonSchema;
use serde::Serialize;

use crate::{ApiError, Emulator};

/// The last write that touched a tile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VramLastWrite {
    /// `cpu`, `dma<n>`, `hdma<n>` or `poke`.
    pub origin: String,
    /// Instruction behind the write (`$bb:aaaa`); for a DMA, the one that
    /// started the transfer.
    pub pc: String,
    /// A-bus address of the byte a DMA moved (`$bb:aaaa`), if any.
    pub source: Option<String>,
    /// Frame of the write.
    pub frame: u64,
    /// `OBSEL` when the write was made.
    pub obsel: u8,
    /// How many of the tile's 32 bytes were ever written (32 = whole).
    pub bytes_written: u8,
}

/// One distinct (rule, tile) finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VramProvEntry {
    /// `S1` or `S4`.
    pub rule: &'static str,
    /// One-line statement of the rule.
    pub what: &'static str,
    /// VRAM byte address of the tile (`$aaaa`).
    pub tile_addr: String,
    /// OAM index of the first sprite that drew it.
    pub sprite: u8,
    /// That sprite's 9-bit OAM character number.
    pub sprite_tile: u16,
    /// Frame of the check.
    pub frame: u64,
    /// `OBSEL` at the check.
    pub obsel: u8,
    /// The tile's last write; absent when it was never written (S1).
    pub last_write: Option<VramLastWrite>,
}

/// What [`Emulator::take_vram_provenance`] returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VramProvReport {
    /// The distinct findings, in order of first occurrence.
    pub entries: Vec<VramProvEntry>,
    /// New distinct findings dropped because the journal was full.
    pub overflow: u64,
}

fn long_address(a: u32) -> String {
    format!("${:02X}:{:04X}", a >> 16, a & 0xFFFF)
}

impl Emulator {
    /// Start tagging VRAM writes and checking the sprite tiles each frame
    /// (`true`), or stop and drop the journal (`false`). Switch it on
    /// before the ROM runs: a tile written earlier reads as never written.
    /// Read-only: every frame hash is unchanged. A later `load_rom` builds
    /// a new machine with the journal off.
    pub fn enable_vram_provenance(&mut self, on: bool) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.ppu.enable_vram_provenance(on);
        Ok(())
    }

    /// Is the VRAM provenance journal on?
    pub fn vram_provenance_enabled(&self) -> Result<bool, ApiError> {
        Ok(self
            .snes
            .as_ref()
            .ok_or(ApiError::NoRom)?
            .ppu
            .vram_provenance_enabled())
    }

    /// Drain the journal (empty when it is off). A (rule, tile) pair is
    /// reported once, not again after a drain.
    pub fn take_vram_provenance(&mut self) -> Result<VramProvReport, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let (reports, overflow) = snes.ppu.take_vram_provenance();
        Ok(VramProvReport {
            entries: reports
                .into_iter()
                .map(|r| VramProvEntry {
                    rule: r.reason.code(),
                    what: r.reason.summary(),
                    tile_addr: format!("${:04X}", r.vram_addr),
                    sprite: r.sprite,
                    sprite_tile: r.sprite_tile,
                    frame: r.frame,
                    obsel: r.obsel,
                    last_write: r.last_write.map(|t| {
                        let w = t.writer;
                        VramLastWrite {
                            origin: match w.kind {
                                VramWriterKind::Cpu => "cpu".to_string(),
                                VramWriterKind::Dma => format!("dma{}", w.channel),
                                VramWriterKind::Hdma => format!("hdma{}", w.channel),
                                VramWriterKind::Poke => "poke".to_string(),
                            },
                            pc: long_address(w.pc),
                            source: matches!(w.kind, VramWriterKind::Dma | VramWriterKind::Hdma)
                                .then(|| long_address(w.source)),
                            frame: w.frame,
                            obsel: t.obsel,
                            bytes_written: t.written.count_ones() as u8,
                        }
                    }),
                })
                .collect(),
            overflow,
        })
    }
}

/// The signals the check knows, `(code, statement)`.
#[must_use]
pub fn vram_provenance_rules() -> Vec<(&'static str, &'static str)> {
    [VramProvReason::NeverWritten, VramProvReason::ObselChanged]
        .into_iter()
        .map(|r| (r.code(), r.summary()))
        .collect()
}
