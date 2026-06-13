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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// `true` if the `FastROM` bit is set in the mapping byte.
    pub fast_rom: bool,
    /// ROM size in kilobytes (advertised by the cartridge, may exceed the
    /// actual file size for over-dumped or padded ROMs).
    pub rom_size_kb: u32,
    /// SRAM size in kilobytes (0 = no SRAM).
    pub sram_size_kb: u32,
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
const DSP1_FIRMWARE_LEN: usize = 0x2000;

impl Cartridge {
    /// Load and parse a ROM file from disk. For a DSP game with no firmware
    /// embedded in the dump, auto-discovers `dsp1b.rom` next to the ROM file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CartError> {
        let path = path.as_ref();
        let mut cart = Self::from_bytes(fs::read(path)?)?;
        if cart.needs_coprocessor_firmware() {
            if let Some(dir) = path.parent() {
                if let Ok(bytes) = fs::read(dir.join("dsp1b.rom")) {
                    cart.set_coprocessor_firmware(bytes);
                }
            }
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
    pub fn set_coprocessor_firmware(&mut self, bytes: Vec<u8>) {
        self.coprocessor_firmware = Some(bytes);
    }

    /// The loaded coprocessor firmware, if any.
    #[must_use]
    pub fn coprocessor_firmware(&self) -> Option<&[u8]> {
        self.coprocessor_firmware.as_deref()
    }
}

// =============================================================================
// Layout detection
// =============================================================================

const HEADER_OFFSET_LOROM: usize = 0x7FC0;
const HEADER_OFFSET_HIROM: usize = 0xFFC0;
const HEADER_OFFSET_EXHIROM: usize = 0x40_FFC0;

/// Try every plausible internal-header offset and return the first whose
/// checksum complement matches.
///
/// We only use the checksum complement test (`!ck == ckcomp`) as the
/// signal — it's strict but reliable. Unlicensed dumps with bogus
/// checksums will be rejected; that's an acceptable trade-off for
/// avoiding false positives on all-zero or non-ROM input.
fn detect_and_parse(rom: &[u8]) -> Option<Header> {
    for off in [
        HEADER_OFFSET_LOROM,
        HEADER_OFFSET_HIROM,
        HEADER_OFFSET_EXHIROM,
    ] {
        if off + 0x20 > rom.len() {
            continue;
        }
        let h = parse_at(rom, off);
        if h.checksum_valid() {
            return Some(h);
        }
    }
    None
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
    let is_dsp = (chipset & 0x0F) >= 0x03 && (chipset & 0xF0) == 0x00;
    let base_kind = mapper_from_byte(map_byte).unwrap_or(MapperKind::LoRom);
    // DSP-1 boards exist in both LoROM (DR/SR at $8000) and HiROM (DR/SR at
    // $6000) flavours — the base layout follows the map byte.
    let dsp_hirom = matches!(base_kind, MapperKind::HiRom | MapperKind::ExHiRom);
    let mapper_kind = if is_superfx {
        MapperKind::SuperFx
    } else if is_dsp {
        MapperKind::Dsp1
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

    Header {
        title,
        mapper_kind,
        dsp_hirom,
        fast_rom,
        rom_size_kb,
        sram_size_kb,
        region: Region::from_country(rom[off + 0x19]),
        maker: rom[off + 0x1A],
        version: rom[off + 0x1B],
        checksum_complement: u16::from_le_bytes([rom[off + 0x1C], rom[off + 0x1D]]),
        checksum: u16::from_le_bytes([rom[off + 0x1E], rom[off + 0x1F]]),
    }
}

const fn mapper_from_byte(byte: u8) -> Option<MapperKind> {
    match byte & 0x0F {
        0 => Some(MapperKind::LoRom),
        1 => Some(MapperKind::HiRom),
        3 => Some(MapperKind::Sa1),
        5 => Some(MapperKind::ExHiRom),
        _ => None,
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
