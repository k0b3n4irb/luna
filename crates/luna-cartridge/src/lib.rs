//! SNES ROM file parsing.
//!
//! Detects an optional 512-byte SMC "copier" header, infers the internal
//! header location (`$7FC0` for `LoROM`, `$FFC0` for `HiROM`), parses the
//! title / mapper / sizes / region, and builds a [`Cartridge`] ready to
//! be wrapped in a `luna-bus` mapper.
//!
//! See `ARCHITECTURE.md` §5.

use luna_bus::MapperKind;
use std::fs;
use std::path::Path;
use thiserror::Error;

// =============================================================================
// Errors
// =============================================================================

/// Errors that may surface while loading or parsing a SNES ROM.
#[derive(Debug, Error)]
pub enum CartError {
    /// Underlying filesystem error.
    #[error("I/O error reading ROM: {0}")]
    Io(#[from] std::io::Error),
    /// File is smaller than the minimum SNES ROM page (32 KB).
    #[error("ROM is too small ({0} bytes); minimum is 32 KB")]
    TooSmall(usize),
    /// No internal header at the expected offsets passed the
    /// checksum-complement validation.
    #[error(
        "could not detect cartridge layout (LoROM / HiROM): both internal headers fail the checksum complement check"
    )]
    LayoutUnknown,
}

// =============================================================================
// Region & header
// =============================================================================

/// Cartridge region / video standard derived from the country byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Region {
    /// NTSC (Japan / North America).
    Ntsc,
    /// PAL (Europe / Australia).
    Pal,
    /// Unknown country code.
    Unknown,
}

impl Region {
    /// Decode region from the SNES country byte at offset `$xxD9`.
    #[must_use]
    pub const fn from_country(byte: u8) -> Self {
        match byte {
            // NTSC: Japan, USA, Canada, South Korea, Brazil
            0x00 | 0x01 | 0x0D | 0x0F | 0x10 => Self::Ntsc,
            // PAL: Europe and friends
            0x02..=0x0C | 0x11 => Self::Pal,
            _ => Self::Unknown,
        }
    }
}

/// A coprocessor / add-on board the header identifies but luna does not
/// emulate. Detected the way ares does (`board()` + `firmwareNEC()` in
/// mia/medium/super-famicom.cpp): chipset byte `$FFD6`, sub-type `$FFBF`,
/// and — for the NEC DSP revisions, which share one chipset code — the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedChip {
    /// NEC DSP-2 (Dungeon Master).
    Dsp2,
    /// NEC DSP-3 (SD Gundam GX).
    Dsp3,
    /// NEC DSP-4 (Top Gear 3000).
    Dsp4,
    /// OBC1 (Metal Combat).
    Obc1,
    /// Sharp S-RTC real-time clock (Daikaijuu Monogatari II).
    SharpRtc,
    /// Super Game Boy.
    SuperGameBoy,
    /// Seta ST-010 / ST-011 (NEC uPD96050).
    SetaSt01x,
    /// Seta ST-018 (ARM).
    SetaSt018,
    /// Hitachi Cx4 (Mega Man X2 / X3).
    Cx4,
}

impl std::fmt::Display for UnsupportedChip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Dsp2 => "DSP-2",
            Self::Dsp3 => "DSP-3",
            Self::Dsp4 => "DSP-4",
            Self::Obc1 => "OBC1",
            Self::SharpRtc => "S-RTC",
            Self::SuperGameBoy => "Super Game Boy",
            Self::SetaSt01x => "ST-010/ST-011",
            Self::SetaSt018 => "ST-018",
            Self::Cx4 => "Cx4",
        })
    }
}

/// Decoded SNES internal header.
#[derive(Debug, Clone)]
pub struct Header {
    /// ROM title, ASCII-decoded (Japanese ROMs use Shift-JIS — best-effort).
    pub title: String,
    /// Cartridge mapping mode.
    pub mapper_kind: MapperKind,
    /// For a `Dsp1` cartridge, whether the base ROM layout is `HiROM`
    /// (`true`, e.g. Super Mario Kart) or `LoROM` (`false`). Ignored for
    /// non-DSP mappers.
    pub dsp_hirom: bool,
    /// A coprocessor the header names but luna does not emulate. When set,
    /// `mapper_kind` is only the bare base layout and the system refuses to
    /// build (a forced mapper clears it — the explicit "try it anyway").
    pub unsupported_chip: Option<UnsupportedChip>,
    /// `true` if the `FastROM` bit is set in the mapping byte.
    pub fast_rom: bool,
    /// ROM size in kilobytes (advertised by the cartridge, may exceed the
    /// actual file size for over-dumped or padded ROMs).
    pub rom_size_kb: u32,
    /// SRAM size in kilobytes (0 = no SRAM).
    pub sram_size_kb: u32,
    /// Expansion (coprocessor work) RAM size in kilobytes, from the
    /// extended-header `$FFBD` byte (`1024 << n`). `0` if the byte is not a
    /// valid `1..=7` exponent. This is the Super FX Game Pak work RAM size —
    /// distinct from `sram_size_kb` (battery save RAM, `$FFD8`). See Mesen2
    /// `BaseCartridge.cpp` (`ExpansionRamSize`).
    pub expansion_ram_kb: u32,
    /// Region / video standard.
    pub region: Region,
    /// Maker code (old-style single byte).
    pub maker: u8,
    /// Mask ROM revision.
    pub version: u8,
    /// 16-bit checksum claimed by the header.
    pub checksum: u16,
    /// 16-bit checksum complement (should be `!checksum`).
    pub checksum_complement: u16,
}

impl Header {
    /// `true` iff `checksum ^ complement == 0xFFFF`. Used as the primary
    /// signal to disambiguate `LoROM` vs `HiROM`.
    #[must_use]
    pub const fn checksum_valid(&self) -> bool {
        self.checksum ^ self.checksum_complement == 0xFFFF
    }
}

// =============================================================================
// Cartridge
// =============================================================================

/// A parsed SNES cartridge.
#[derive(Debug, Clone)]
pub struct Cartridge {
    /// Pure ROM bytes (SMC copier header stripped if present).
    pub rom: Vec<u8>,
    /// Decoded header.
    pub header: Header,
    /// Coprocessor microcode (e.g. an 8 KB DSP-1 `dsp1b.rom`), if the cart
    /// needs one and it was found embedded in the dump or supplied
    /// externally. `None` for non-coprocessor carts or until resolved.
    coprocessor_firmware: Option<Vec<u8>>,
}

/// Combined DSP-1 firmware size (program `0x1800` + data `0x800`).
/// Size of a combined DSP-1 / DSP-1B microcode dump: 2048 24-bit program
/// words (`0x1800`) followed by 1024 16-bit data words (`0x800`). A blob of
/// any other size cannot be that firmware, so it is refused rather than
/// stored — a truncated file that *looks* installed leaves the chip inert
/// while [`Cartridge::needs_coprocessor_firmware`] reports all is well.
pub const DSP1_FIRMWARE_LEN: usize = 0x2000;

impl Cartridge {
    /// Load and parse a ROM file from disk. For a DSP game with no firmware
    /// embedded in the dump, auto-discovers `dsp1b.rom` next to the ROM file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CartError> {
        let path = path.as_ref();
        let mut cart = Self::from_bytes(fs::read(path)?)?;
        if cart.needs_coprocessor_firmware()
            && let Some(dir) = path.parent()
            && let Ok(bytes) = fs::read(dir.join("dsp1b.rom"))
        {
            cart.set_coprocessor_firmware(bytes);
        }
        Ok(cart)
    }

    /// Parse a ROM image from bytes already in memory. Strips a 512-byte
    /// SMC copier header if present, and (DSP games) extracts firmware
    /// appended to the dump.
    pub fn from_bytes(mut rom: Vec<u8>) -> Result<Self, CartError> {
        // SMC copier prepends 512 bytes if `(rom.len() % 1024) == 512`.
        if rom.len() % 1024 == 512 {
            rom.drain(..512);
        }
        if rom.len() < 0x8000 {
            return Err(CartError::TooSmall(rom.len()));
        }
        let header = detect_and_parse(&rom).ok_or(CartError::LayoutUnknown)?;
        // Some DSP-1 dumps append the chip's 8 KB firmware (Mesen2
        // `BaseCartridge.cpp`: ROM length is `32KB·n + 0x2000`). Strip it
        // off the ROM and keep it as the coprocessor firmware.
        let coprocessor_firmware = if matches!(header.mapper_kind, MapperKind::Dsp1)
            && rom.len() & 0x7FFF == DSP1_FIRMWARE_LEN
        {
            Some(rom.split_off(rom.len() - DSP1_FIRMWARE_LEN))
        } else {
            None
        };
        Ok(Self {
            rom,
            header,
            coprocessor_firmware,
        })
    }

    /// Parse a ROM image but **force** the mapper layout and skip the
    /// checksum-complement validation that [`Self::from_bytes`] requires.
    ///
    /// For headerless / homebrew / hardware-test ROMs (e.g. the Peter
    /// Lemon SNES suite) whose internal checksum is blank or wrong, where
    /// layout auto-detection would reject them. The header fields are
    /// still parsed at the forced mapper's offset (best effort), but
    /// `mapper_kind` is overridden to `mapper`.
    pub fn from_bytes_forced(mut rom: Vec<u8>, mapper: MapperKind) -> Result<Self, CartError> {
        if rom.len() % 1024 == 512 {
            rom.drain(..512);
        }
        if rom.len() < 0x8000 {
            return Err(CartError::TooSmall(rom.len()));
        }
        let off = match mapper {
            // LoROM-region layouts (header at $7FC0).
            MapperKind::LoRom | MapperKind::Sa1 | MapperKind::SuperFx => HEADER_OFFSET_LOROM,
            // HiROM-region layouts (header at $FFC0). Forced DSP-1 assumes
            // the HiROM board (Super Mario Kart); LoROM DSP-1 isn't forced.
            MapperKind::HiRom | MapperKind::Sdd1 | MapperKind::Spc7110 | MapperKind::Dsp1 => {
                HEADER_OFFSET_HIROM
            }
            MapperKind::ExHiRom => HEADER_OFFSET_EXHIROM,
        };
        if off + 0x20 > rom.len() {
            return Err(CartError::LayoutUnknown);
        }
        let mut header = parse_at(&rom, off);
        header.mapper_kind = mapper;
        header.unsupported_chip = None;
        Ok(Self {
            rom,
            header,
            coprocessor_firmware: None,
        })
    }

    /// `true` when this cartridge needs an external coprocessor firmware
    /// image that hasn't been supplied yet (a DSP game with no `dsp1b.rom`
    /// embedded in the dump or loaded beside it).
    #[must_use]
    pub const fn needs_coprocessor_firmware(&self) -> bool {
        matches!(self.header.mapper_kind, MapperKind::Dsp1) && self.coprocessor_firmware.is_none()
    }

    /// Supply the coprocessor firmware (e.g. an 8 KB `dsp1b.rom`). Used by
    /// front-ends that resolve the file via a CLI flag / firmware folder.
    ///
    /// Returns `false` — leaving the cartridge untouched — when `bytes` is
    /// not a plausible dump for this cartridge's coprocessor. Accepting a
    /// short blob here is what made a truncated `dsp1b.rom` *look*
    /// installed: [`Self::needs_coprocessor_firmware`] then answered
    /// "nothing missing" while the DSP core, which checks the length
    /// itself, silently refused to run. The caller can surface the refusal;
    /// ignoring it leaves the firmware reported as missing, which is the
    /// honest state.
    pub fn set_coprocessor_firmware(&mut self, bytes: Vec<u8>) -> bool {
        if !matches!(self.header.mapper_kind, MapperKind::Dsp1) || bytes.len() != DSP1_FIRMWARE_LEN
        {
            return false;
        }
        self.coprocessor_firmware = Some(bytes);
        true
    }

    /// The loaded coprocessor firmware, if any.
    #[must_use]
    pub fn coprocessor_firmware(&self) -> Option<&[u8]> {
        self.coprocessor_firmware.as_deref()
    }

    /// The external firmware file this cartridge needs (e.g. `dsp1b.rom`
    /// for a DSP-1 game), or `None` if it needs none. Front-ends use this
    /// to name the file in a prompt / error and to look it up by name.
    #[must_use]
    pub const fn required_firmware_filename(&self) -> Option<&'static str> {
        match self.header.mapper_kind {
            MapperKind::Dsp1 => Some("dsp1b.rom"),
            _ => None,
        }
    }
}

// =============================================================================
// Layout detection
// =============================================================================

const HEADER_OFFSET_LOROM: usize = 0x7FC0;
const HEADER_OFFSET_HIROM: usize = 0xFFC0;
const HEADER_OFFSET_EXHIROM: usize = 0x40_FFC0;

/// Detect the cartridge layout by examining each plausible internal-header
/// offset.
///
/// The checksum-complement test (`!ck == ckcomp`) is kept as the strict
/// **acceptance gate** — it reliably rejects all-zero / non-ROM input,
/// which is luna's deliberate trade-off (forced-mapper loading handles
/// unlicensed dumps with bogus checksums). Among the offsets that pass
/// the gate, we no longer take the *first* one: we pick the
/// highest-[`score_header`] candidate (ties → the earlier offset, i.e.
/// `LoROM`), a port of ares' `SuperFamicom::scoreHeader`
/// (mia/medium/super-famicom.cpp). This disambiguates the rare case where
/// more than one offset's checksum coincidentally validates.
fn detect_and_parse(rom: &[u8]) -> Option<Header> {
    let mut best: Option<(i32, usize)> = None;
    for off in [
        HEADER_OFFSET_LOROM,
        HEADER_OFFSET_HIROM,
        HEADER_OFFSET_EXHIROM,
    ] {
        if off + 0x20 > rom.len() {
            continue;
        }
        if !parse_at(rom, off).checksum_valid() {
            continue;
        }
        let score = score_header(rom, off);
        if best.is_none_or(|(bs, _)| score > bs) {
            best = Some((score, off));
        }
    }
    best.map(|(_, off)| parse_at(rom, off))
}

/// Heuristic confidence that the internal header at `off` is the real one,
/// a faithful port of ares' `SuperFamicom::scoreHeader`
/// (mia/medium/super-famicom.cpp). ares' header base is `off - 0x10`, so
/// its `address + 0x25` map-mode byte is luna's `off + 0x15`, etc. Scores
/// the reset-vector validity, the plausibility of the first opcode the CPU
/// would execute, the checksum complement, and the map-mode/offset match.
fn score_header(rom: &[u8], off: usize) -> i32 {
    // ares requires `address + 0x50` bytes (= luna `off + 0x40`): the
    // header plus the native reset vector at `off + 0x3C`.
    if off + 0x40 > rom.len() {
        return 0;
    }
    let map_mode = rom[off + 0x15] & !0x10; // ignore the FastROM bit
    let complement = u16::from_le_bytes([rom[off + 0x1C], rom[off + 0x1D]]);
    let checksum = u16::from_le_bytes([rom[off + 0x1E], rom[off + 0x1F]]);
    let reset_vector = u16::from_le_bytes([rom[off + 0x3C], rom[off + 0x3D]]);
    if reset_vector < 0x8000 {
        // $00:0000-7FFF is never ROM data — this offset can't be a header.
        return 0;
    }

    // The first instruction the CPU would execute at the reset vector.
    let ares_base = off.wrapping_sub(0x10);
    let opcode_off = (ares_base & !0x7FFF) | (reset_vector as usize & 0x7FFF);
    let opcode = rom.get(opcode_off).copied().unwrap_or(0);

    let mut score: i32 = 0;
    match opcode {
        // most likely: sei / clc / sec / stz $nnnn / jmp / jml
        0x78 | 0x18 | 0x38 | 0x9C | 0x4C | 0x5C => score += 8,
        // plausible: rep/sep/lda/ldx/ldy/jsr/jsl
        0xC2 | 0xE2 | 0xAD | 0xAE | 0xAC | 0xAF | 0xA9 | 0xA2 | 0xA0 | 0x20 | 0x22 => score += 4,
        // implausible: rti/rts/rtl/cmp/cpx/cpy
        0x40 | 0x60 | 0x6B | 0xCD | 0xEC | 0xCC => score -= 4,
        // least likely: brk/cop/stp/wdm/sbc $nnnnnn,x
        0x00 | 0x02 | 0xDB | 0x42 | 0xFF => score -= 8,
        _ => {}
    }
    if checksum.wrapping_add(complement) == 0xFFFF {
        score += 4;
    }
    if off == HEADER_OFFSET_LOROM && map_mode == 0x20 {
        score += 2;
    }
    if off == HEADER_OFFSET_HIROM && map_mode == 0x21 {
        score += 2;
    }
    score.max(0)
}

fn parse_at(rom: &[u8], off: usize) -> Header {
    let mut title_bytes = [0u8; 21];
    title_bytes.copy_from_slice(&rom[off..off + 21]);
    let title = decode_title(&title_bytes);

    let map_byte = rom[off + 0x15];
    let chipset = rom[off + 0x16];
    // Coprocessor override from the chipset byte ($FFD6): when the low
    // nibble flags a coprocessor (>= 3) the high nibble selects which.
    // Super FX games (Star Fox = $13, Yoshi's Island = $15) carry a LoROM
    // map mode ($20), so the GSU is only visible via this byte — high
    // nibble 1 = GSU. (Empirically verified against both ROMs' headers.)
    // Coprocessor overrides keyed on the chipset byte: low nibble >= 3 flags
    // a coprocessor, high nibble selects which (1 = Super FX, 0 = NEC DSP).
    let is_superfx = (chipset & 0x0F) >= 0x03 && (chipset & 0xF0) == 0x10;
    let is_nec = (chipset & 0x0F) >= 0x03 && (chipset & 0xF0) == 0x00;
    // Every NEC DSP revision shares that chipset code; ares `firmwareNEC()`
    // (and Mesen2) tell them apart by title. Only DSP-1/1B is emulated.
    let nec_other = match trimmed_title(&title_bytes) {
        b"DUNGEON MASTER" => Some(UnsupportedChip::Dsp2),
        // "SDガンダムGX" in half-width katakana.
        b"SD\xB6\xDE\xDD\xC0\xDE\xD1GX" => Some(UnsupportedChip::Dsp3),
        b"PLANETS CHAMP TG3000" | b"TOP GEAR 3000" => Some(UnsupportedChip::Dsp4),
        _ => None,
    };
    let is_dsp = is_nec && nec_other.is_none();
    // The remaining coprocessor codes of ares `board()`. `$Fx` boards are
    // selected by the sub-type byte `$FFBF` (one below the title block).
    let subtype = off.checked_sub(1).and_then(|i| rom.get(i)).copied();
    let is_spc7110 = matches!(chipset, 0xF5 | 0xF9) && subtype == Some(0x00);
    let unsupported_chip = if is_nec {
        nec_other
    } else if (chipset & 0x0F) < 0x03 {
        None
    } else {
        match (chipset >> 4, subtype) {
            (0x2, _) => Some(UnsupportedChip::Obc1),
            (0x5, _) => Some(UnsupportedChip::SharpRtc),
            (0xE, _) if chipset == 0xE3 => Some(UnsupportedChip::SuperGameBoy),
            (0xF, Some(0x01)) => Some(UnsupportedChip::SetaSt01x),
            (0xF, Some(0x02)) => Some(UnsupportedChip::SetaSt018),
            (0xF, Some(0x10)) => Some(UnsupportedChip::Cx4),
            _ => None,
        }
    };
    // SA-1's canonical signal is the chipset/RomType byte ($FFD6): low
    // nibble >= 3 (coprocessor present) + high nibble 3 (= SA-1) — e.g.
    // SMRPG / Kirby Super Star carry $34/$35. The MapMode byte's low
    // nibble 3 is a weaker secondary signal (kept via `mapper_from_byte`),
    // but RomType is what hardware/ares key on.
    let is_sa1 = (chipset & 0x0F) >= 0x03 && (chipset & 0xF0) == 0x30;
    // S-DD1 (graphics decompression — Star Ocean, Street Fighter Alpha 2):
    // chipset high nibble 4. LoROM-based; the chip is a `Sdd1Mapper` shim.
    let is_sdd1 = (chipset & 0x0F) >= 0x03 && (chipset & 0xF0) == 0x40;
    // ares `super-famicom.cpp` board(): the map mode only counts when it
    // is one of the exact documented values; otherwise (many titles let
    // an extra title character overwrite it — Contra III carries `$53`)
    // the layout comes from where the header was found. One title
    // overwrites it with a plausible-looking `!` (`$21`) on a LoROM board.
    let base_kind = if title == "YUYU NO QUIZ DE GO!GO" {
        MapperKind::LoRom
    } else {
        mapper_from_byte(map_byte).unwrap_or_else(|| mapper_from_offset(off))
    };
    // DSP-1 boards exist in both LoROM (DR/SR at $8000) and HiROM (DR/SR at
    // $6000) flavours — the base layout follows the map byte.
    let dsp_hirom = matches!(base_kind, MapperKind::HiRom | MapperKind::ExHiRom);
    let mapper_kind = if is_superfx {
        MapperKind::SuperFx
    } else if is_dsp {
        MapperKind::Dsp1
    } else if is_sa1 {
        MapperKind::Sa1
    } else if is_sdd1 {
        MapperKind::Sdd1
    } else if is_spc7110 {
        // Recognised so the system can refuse it by name (not emulated yet).
        MapperKind::Spc7110
    } else {
        base_kind
    };
    let fast_rom = (map_byte & 0x10) != 0;
    // The size bytes are exponents (KB = 1 << byte). Garbage cartridges
    // (or our wrong-offset probing) can produce arbitrary byte values
    // which would propagate downstream into multi-terabyte allocation
    // requests. Clamp the exponents to ranges that span the real SNES
    // catalogue: ROM up to 64 MB (1 << 16 KB) and SRAM up to 128 KB
    // (1 << 7 KB). Larger advertised values are saturated, not trusted.
    let rom_size_kb = 1u32 << u32::from(rom[off + 0x17]).min(16);
    let sram_byte = rom[off + 0x18];
    let sram_size_kb = if sram_byte == 0 {
        0
    } else {
        1u32 << u32::from(sram_byte).min(7)
    };
    // Expansion (coprocessor) RAM size: extended-header byte $FFBD, three
    // bytes before the standard header (`off - 3`). Only a `1..=7` exponent
    // is a valid size (`1024 << n`); anything else (incl. the 0xFF that
    // non-extended-header carts like Star Fox carry there) reads as "absent".
    let expansion_ram_kb = off
        .checked_sub(3)
        .and_then(|i| rom.get(i))
        .copied()
        .filter(|&b| (1..=7).contains(&b))
        .map_or(0, |b| 1u32 << b);

    Header {
        title,
        mapper_kind,
        dsp_hirom,
        unsupported_chip,
        fast_rom,
        rom_size_kb,
        sram_size_kb,
        expansion_ram_kb,
        region: Region::from_country(rom[off + 0x19]),
        maker: rom[off + 0x1A],
        version: rom[off + 0x1B],
        checksum_complement: u16::from_le_bytes([rom[off + 0x1C], rom[off + 0x1D]]),
        checksum: u16::from_le_bytes([rom[off + 0x1E], rom[off + 0x1F]]),
    }
}

/// The header title with its `$00` / `$20` / `$FF` padding trimmed, as raw
/// bytes — what ares `label()` compares before Shift-JIS decoding.
fn trimmed_title(title: &[u8; 21]) -> &[u8] {
    let end = title
        .iter()
        .rposition(|&b| !matches!(b, 0x00 | 0x20 | 0xFF))
        .map_or(0, |i| i + 1);
    &title[..end]
}

/// Map mode byte (`$FFD5`) → layout, accepting only the exact values ares
/// recognises (`board()` in mia/medium/super-famicom.cpp), `FastROM` bit
/// (`$10`) either way. `$22/$32` is the S-DD1 board, LoROM-based in luna.
/// Any other value (`$2A/$3A` SPC7110 included) is `None`: the caller falls
/// back to the header location.
const fn mapper_from_byte(byte: u8) -> Option<MapperKind> {
    match byte {
        0x20 | 0x30 | 0x22 | 0x32 => Some(MapperKind::LoRom),
        0x21 | 0x31 => Some(MapperKind::HiRom),
        0x23 | 0x33 => Some(MapperKind::Sa1),
        0x25 | 0x35 => Some(MapperKind::ExHiRom),
        _ => None,
    }
}

/// Layout implied by where the internal header was found — ares' fallback
/// when the map mode byte is not a recognised value.
const fn mapper_from_offset(off: usize) -> MapperKind {
    match off {
        HEADER_OFFSET_HIROM => MapperKind::HiRom,
        HEADER_OFFSET_EXHIROM => MapperKind::ExHiRom,
        _ => MapperKind::LoRom,
    }
}

fn decode_title(bytes: &[u8; 21]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 32 KB synthetic `LoROM` with a valid header.
    fn synth_lorom(title: &str, sram_kb_log2: u8) -> Vec<u8> {
        let mut rom = vec![0xEA; 32 * 1024]; // NOP-padded
        let header_off = HEADER_OFFSET_LOROM;
        // Title (21 bytes, space-padded).
        let title_bytes: Vec<u8> = title
            .bytes()
            .chain(std::iter::repeat(b' '))
            .take(21)
            .collect();
        rom[header_off..header_off + 21].copy_from_slice(&title_bytes);
        rom[header_off + 0x15] = 0x20; // LoROM, slow
        rom[header_off + 0x16] = 0x00; // ROM only
        rom[header_off + 0x17] = 0x05; // 32 KB
        rom[header_off + 0x18] = sram_kb_log2;
        rom[header_off + 0x19] = 0x01; // USA (NTSC)
        rom[header_off + 0x1A] = 0x33;
        rom[header_off + 0x1B] = 0x00;
        // Checksum complement = 0x1234, checksum = !0x1234 = 0xEDCB
        rom[header_off + 0x1C] = 0x34;
        rom[header_off + 0x1D] = 0x12;
        rom[header_off + 0x1E] = 0xCB;
        rom[header_off + 0x1F] = 0xED;
        rom
    }

    /// A synthetic `LoROM` whose chipset (`$FFD6`) / sub-type (`$FFBF`)
    /// bytes name a coprocessor board.
    fn synth_chip(title: &[u8], chipset: u8, subtype: u8) -> Header {
        let mut rom = synth_lorom("", 0);
        rom[HEADER_OFFSET_LOROM..HEADER_OFFSET_LOROM + 21].fill(b' ');
        rom[HEADER_OFFSET_LOROM..HEADER_OFFSET_LOROM + title.len()].copy_from_slice(title);
        rom[HEADER_OFFSET_LOROM + 0x16] = chipset;
        rom[HEADER_OFFSET_LOROM - 1] = subtype;
        Cartridge::from_bytes(rom).unwrap().header
    }

    /// Build a synthetic DSP-1 cartridge (chipset `$05`, SMK's title picks
    /// the DSP-1 revision) with no firmware supplied.
    fn synth_dsp1_cart() -> Cartridge {
        let mut rom = synth_lorom("", 0);
        rom[HEADER_OFFSET_LOROM..HEADER_OFFSET_LOROM + 21].fill(b' ');
        let title = b"SUPER MARIO KART";
        rom[HEADER_OFFSET_LOROM..HEADER_OFFSET_LOROM + title.len()].copy_from_slice(title);
        rom[HEADER_OFFSET_LOROM + 0x16] = 0x05;
        Cartridge::from_bytes(rom).unwrap()
    }

    #[test]
    fn a_firmware_blob_of_the_wrong_size_is_refused_not_stored() {
        // A truncated `dsp1b.rom` used to be accepted here, which made
        // `needs_coprocessor_firmware()` answer "nothing missing" while the
        // DSP core — which checks the length itself — quietly refused to
        // run. The cartridge must keep reporting the firmware as missing.
        let mut cart = synth_dsp1_cart();
        assert!(cart.needs_coprocessor_firmware());

        for bad in [vec![], vec![0u8; 1], vec![0u8; DSP1_FIRMWARE_LEN - 1]] {
            let n = bad.len();
            assert!(
                !cart.set_coprocessor_firmware(bad),
                "{n} bytes was accepted"
            );
            assert!(
                cart.needs_coprocessor_firmware(),
                "{n} bytes left the cart claiming it has firmware"
            );
            assert!(cart.coprocessor_firmware().is_none());
        }

        assert!(cart.set_coprocessor_firmware(vec![0xAB; DSP1_FIRMWARE_LEN]));
        assert!(!cart.needs_coprocessor_firmware());
        assert_eq!(
            cart.coprocessor_firmware().map(<[u8]>::len),
            Some(DSP1_FIRMWARE_LEN)
        );
    }

    #[test]
    fn a_cart_with_no_coprocessor_takes_no_firmware() {
        let mut cart = Cartridge::from_bytes(synth_lorom("PLAIN", 0)).unwrap();
        assert!(!cart.needs_coprocessor_firmware());
        assert!(!cart.set_coprocessor_firmware(vec![0xAB; DSP1_FIRMWARE_LEN]));
        assert!(cart.coprocessor_firmware().is_none());
    }

    #[test]
    fn nec_dsp_revisions_are_told_apart_by_title() {
        // ares firmwareNEC(): one chipset code, the title picks the revision.
        let dsp1 = synth_chip(b"SUPER MARIO KART", 0x05, 0x00);
        assert_eq!(dsp1.mapper_kind, MapperKind::Dsp1);
        assert_eq!(dsp1.unsupported_chip, None);
        for (title, chip) in [
            (&b"DUNGEON MASTER"[..], UnsupportedChip::Dsp2),
            (&b"SD\xB6\xDE\xDD\xC0\xDE\xD1GX"[..], UnsupportedChip::Dsp3),
            (&b"TOP GEAR 3000"[..], UnsupportedChip::Dsp4),
            (&b"PLANETS CHAMP TG3000"[..], UnsupportedChip::Dsp4),
        ] {
            let h = synth_chip(title, 0x05, 0x00);
            assert_eq!(h.unsupported_chip, Some(chip), "{title:?}");
            assert_ne!(
                h.mapper_kind,
                MapperKind::Dsp1,
                "{title:?} must not run as DSP-1"
            );
        }
    }

    #[test]
    fn unemulated_coprocessor_boards_are_named() {
        // ares board(): chipset high nibble, `$Fx` split by the sub-type.
        for (chipset, subtype, chip) in [
            (0x25, 0x00, UnsupportedChip::Obc1),
            (0x55, 0x00, UnsupportedChip::SharpRtc),
            (0xE3, 0x00, UnsupportedChip::SuperGameBoy),
            (0xF6, 0x01, UnsupportedChip::SetaSt01x),
            (0xF5, 0x02, UnsupportedChip::SetaSt018),
            (0xF3, 0x10, UnsupportedChip::Cx4),
        ] {
            let h = synth_chip(b"CHIP TEST", chipset, subtype);
            assert_eq!(h.unsupported_chip, Some(chip), "chipset ${chipset:02X}");
            assert_eq!(h.mapper_kind, MapperKind::LoRom);
        }
        let spc = synth_chip(b"CHIP TEST", 0xF5, 0x00);
        assert_eq!(spc.mapper_kind, MapperKind::Spc7110);
        // Supported boards and plain carts stay clean.
        for chipset in [0x00, 0x02, 0x13, 0x15, 0x34, 0x35, 0x43, 0x45] {
            let h = synth_chip(b"CHIP TEST", chipset, 0x00);
            assert_eq!(h.unsupported_chip, None, "chipset ${chipset:02X}");
        }
    }

    #[test]
    fn forcing_a_mapper_clears_the_unsupported_chip() {
        let mut rom = synth_lorom("CHIP TEST", 0);
        rom[HEADER_OFFSET_LOROM + 0x16] = 0xF3;
        rom[HEADER_OFFSET_LOROM - 1] = 0x10;
        let cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).unwrap();
        assert_eq!(cart.header.unsupported_chip, None);
    }

    #[test]
    fn round_trip_synth_lorom() {
        let rom = synth_lorom("LUNA DEMO", 3);
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert_eq!(cart.header.title, "LUNA DEMO");
        assert_eq!(cart.header.mapper_kind, MapperKind::LoRom);
        assert_eq!(cart.header.rom_size_kb, 32);
        assert_eq!(cart.header.sram_size_kb, 8); // 1 << 3
        assert_eq!(cart.header.region, Region::Ntsc);
        assert!(cart.header.checksum_valid());
    }

    #[test]
    fn sa1_detected_via_chipset_romtype_not_mapmode() {
        let mut rom = synth_lorom("SA1 TEST", 0);
        // Keep the plain LoROM map mode ($20) — i.e. NOT the $23 SA-1
        // map-mode. SA-1 must be recognised purely from the RomType byte
        // ($FFD6 high nibble 3); SMRPG/Kirby carry $34/$35.
        rom[HEADER_OFFSET_LOROM + 0x15] = 0x20;
        rom[HEADER_OFFSET_LOROM + 0x16] = 0x34;
        assert_eq!(
            parse_at(&rom, HEADER_OFFSET_LOROM).mapper_kind,
            MapperKind::Sa1
        );
    }

    #[test]
    fn unrecognised_map_mode_falls_back_to_header_location() {
        // Contra III (USA) carries map byte $53: its low nibble (3) used to
        // select SA-1. ares only accepts exact values, then falls back to
        // the header location — here LoROM.
        let mut rom = synth_lorom("CONTRA3", 0);
        rom[HEADER_OFFSET_LOROM + 0x15] = 0x53;
        assert_eq!(
            parse_at(&rom, HEADER_OFFSET_LOROM).mapper_kind,
            MapperKind::LoRom
        );
        // Same unrecognised byte in a header found at $FFC0 → HiROM.
        let mut rom = vec![0xEA; 64 * 1024];
        rom[HEADER_OFFSET_HIROM + 0x15] = 0x53;
        assert_eq!(
            parse_at(&rom, HEADER_OFFSET_HIROM).mapper_kind,
            MapperKind::HiRom
        );
    }

    #[test]
    fn exact_map_modes_select_their_layout() {
        for (byte, kind) in [
            (0x20, MapperKind::LoRom),
            (0x30, MapperKind::LoRom),
            (0x21, MapperKind::HiRom),
            (0x31, MapperKind::HiRom),
            (0x23, MapperKind::Sa1),
            (0x25, MapperKind::ExHiRom),
            (0x35, MapperKind::ExHiRom),
        ] {
            assert_eq!(mapper_from_byte(byte), Some(kind), "map byte {byte:#04x}");
        }
        assert_eq!(mapper_from_byte(0x53), None);
        assert_eq!(mapper_from_byte(0x3A), None);
    }

    #[test]
    fn score_header_prefers_plausible_reset_opcode() {
        // Identical headers; only the byte at the reset-vector target
        // differs. A plausible first opcode (sei) must outscore an
        // implausible one (brk).
        let mut good = synth_lorom("SCORE", 0);
        // Point the reset vector at $8100 → file offset $0100.
        good[HEADER_OFFSET_LOROM + 0x3C] = 0x00;
        good[HEADER_OFFSET_LOROM + 0x3D] = 0x81;
        let mut bad = good.clone();
        good[0x0100] = 0x78; // sei  → +8
        bad[0x0100] = 0x00; // brk  → -8
        assert!(score_header(&good, HEADER_OFFSET_LOROM) > score_header(&bad, HEADER_OFFSET_LOROM));
    }

    #[test]
    fn expansion_ram_byte_sizes_superfx_work_ram() {
        // Extended-header $FFBD (off-3) = exponent n → 1024<<n KB. Yoshi's
        // Island carries 5 (= 32 KB); Doom/Stunt Race carry 6 (= 64 KB).
        let mut rom = synth_lorom("EXP RAM", 0);
        rom[HEADER_OFFSET_LOROM - 3] = 5;
        assert_eq!(
            Cartridge::from_bytes(rom).unwrap().header.expansion_ram_kb,
            32
        );

        // An out-of-range byte (e.g. the 0xFF a non-extended-header cart
        // like Star Fox carries there) reads as "absent" → 0; the Super FX
        // builder then defaults GSU work RAM to 64 KB.
        let mut rom = synth_lorom("NO EXP", 0);
        rom[HEADER_OFFSET_LOROM - 3] = 0xFF;
        assert_eq!(
            Cartridge::from_bytes(rom).unwrap().header.expansion_ram_kb,
            0
        );
    }

    #[test]
    fn smc_header_is_stripped() {
        let mut rom = vec![0xCC; 512]; // SMC copier header garbage
        rom.extend(synth_lorom("STRIP TEST", 0));
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert_eq!(cart.header.title, "STRIP TEST");
        assert_eq!(cart.rom.len(), 32 * 1024);
    }

    #[test]
    fn too_small_rejected() {
        let rom = vec![0u8; 0x1000];
        assert!(matches!(
            Cartridge::from_bytes(rom),
            Err(CartError::TooSmall(_))
        ));
    }

    #[test]
    fn unknown_layout_rejected() {
        // 32 KB of zeros — no valid checksum complement at any offset.
        let rom = vec![0u8; 32 * 1024];
        assert!(matches!(
            Cartridge::from_bytes(rom),
            Err(CartError::LayoutUnknown)
        ));
    }

    #[test]
    fn region_decoding() {
        assert_eq!(Region::from_country(0x00), Region::Ntsc); // Japan
        assert_eq!(Region::from_country(0x01), Region::Ntsc); // USA
        assert_eq!(Region::from_country(0x02), Region::Pal); // EU (Australia)
        assert_eq!(Region::from_country(0x42), Region::Unknown);
    }

    #[test]
    fn decode_title_handles_garbage() {
        let bytes: [u8; 21] = [
            b'S', b'A', b'M', b'P', b'L', b'E', 0xFF, b'X', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b' ', b' ', b' ', b' ',
        ];
        // 0xFF is replaced by space, trimmed at the end. The 0xFF sits
        // between 'E' and 'X', giving one space — the trailing spaces
        // are stripped by trim_end.
        assert_eq!(decode_title(&bytes), "SAMPLE X");
    }
}
