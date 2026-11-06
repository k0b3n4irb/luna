//! "Ready for a physical cartridge" header audit (`luna cart-check`).
//!
//! A faithful port ends on a repro board or a flash cart, not in an
//! emulator: a header an emulator forgives (a copier header, a wrong
//! checksum, a size byte that disagrees with the file) is a header a
//! programmer or a console may not. [`audit`] reads a ROM image the way the
//! loader does (same layout detection, same checksum) and returns one
//! [`Finding`] per thing worth fixing, each with a stable `code`.
//!
//! Severities: [`Severity::Error`] is a cartridge that will not boot or
//! cannot be built as is; [`Severity::Warn`] is a header that contradicts
//! itself or the file; [`Severity::Info`] is a fact to know (a rule with no
//! documented source is never more than Info). Sources: the SNES header
//! tables of fullsnes and the snesdev wiki (field layout, chipset and
//! country codes, "assume the vectors point into `$8000..$FFFE`"), the
//! snesdev wiki "Header Verification" list (the heuristics emulators use),
//! and ares `mia/medium/super-famicom.cpp` (`board()`, `scoreHeader`).

use crate::{
    HEADER_OFFSET_EXHIROM, HEADER_OFFSET_HIROM, HEADER_OFFSET_LOROM, computed_checksum, parse_at,
    score_header,
};
use luna_bus::MapperKind;

/// How bad a [`Finding`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The cartridge cannot boot or cannot be built as is.
    Error,
    /// The header contradicts itself or the file.
    Warn,
    /// A fact worth knowing; no action required.
    Info,
}

impl Severity {
    /// Upper-case label used by the text output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
        }
    }
}

/// One result of [`audit`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// How bad it is.
    pub severity: Severity,
    /// Stable identifier of the rule (never reworded once released).
    pub code: &'static str,
    /// What is wrong and what to do about it.
    pub message: String,
}

struct Out(Vec<Finding>);

impl Out {
    fn add(&mut self, severity: Severity, code: &'static str, message: String) {
        self.0.push(Finding {
            severity,
            code,
            message,
        });
    }
}

/// Project policy applied on top of the header rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditOptions {
    /// The cartridge must carry no save RAM: any declared SRAM (size byte
    /// or a RAM/battery chipset code) is an [`Severity::Error`].
    pub no_sram: bool,
}

/// Audit a ROM image (a file as read from disk, copier header included).
#[must_use]
pub fn audit(file: &[u8]) -> Vec<Finding> {
    audit_with(file, AuditOptions::default())
}

/// [`audit`] with a project policy ([`AuditOptions`]).
#[must_use]
pub fn audit_with(file: &[u8], opts: AuditOptions) -> Vec<Finding> {
    let mut out = Out(Vec::new());
    let copier = file.len() % 1024 == 512;
    if copier {
        out.add(
            Severity::Error,
            "SMC_COPIER_HEADER",
            "the file starts with a 512-byte copier (SMC) header (size % 1024 == 512); \
             a cartridge chip holds the bare ROM: strip the first 512 bytes before burning"
                .into(),
        );
    }
    let rom = if copier { &file[512..] } else { file };
    if rom.len() < 0x8000 {
        out.add(
            Severity::Error,
            "ROM_TOO_SMALL",
            format!(
                "the ROM is {} bytes; the smallest image holding an internal header is 32 KiB",
                rom.len()
            ),
        );
        return out.0;
    }

    // Header location: the offsets whose checksum pair validates, best
    // `score_header` first (ties: earliest); none validates -> best score of
    // all, and the complement rule below reports it.
    let fits = |off: usize| off + 0x40 <= rom.len();
    let offsets = [
        HEADER_OFFSET_LOROM,
        HEADER_OFFSET_HIROM,
        HEADER_OFFSET_EXHIROM,
    ];
    let pick = |gated: bool| {
        let mut best: Option<(i32, usize)> = None;
        for off in offsets.into_iter().filter(|&o| fits(o)) {
            if gated && !parse_at(rom, off).checksum_valid() {
                continue;
            }
            let s = score_header(rom, off);
            if best.is_none_or(|(bs, _)| s > bs) {
                best = Some((s, off));
            }
        }
        best.map(|(_, o)| o)
    };
    let off = pick(true)
        .or_else(|| pick(false))
        .unwrap_or(HEADER_OFFSET_LOROM);
    let h = parse_at(rom, off);
    let shown = off + (file.len() - rom.len());
    out.add(
        Severity::Info,
        "HEADER_FOUND",
        format!(
            "internal header read at ROM offset ${off:06X} ({}, file offset ${shown:06X}); \
             layout from the map byte: {:?}",
            match off {
                HEADER_OFFSET_LOROM => "LoROM position",
                HEADER_OFFSET_HIROM => "HiROM position",
                _ => "ExHiROM position",
            },
            h.mapper_kind
        ),
    );

    // Checksums.
    if h.checksum_valid() {
        let computed = computed_checksum(rom);
        if computed != h.checksum {
            out.add(
                Severity::Warn,
                "CHECKSUM_MISMATCH",
                format!(
                    "the header declares checksum ${:04X} but the ROM sums to ${computed:04X} \
                     (non-power-of-two images counted with their tail mirrored); \
                     recompute it after the last edit of the image",
                    h.checksum
                ),
            );
        }
    } else {
        out.add(
            Severity::Error,
            "CHECKSUM_COMPLEMENT_INVALID",
            format!(
                "checksum ${:04X} and complement ${:04X} do not XOR to $FFFF; no valid \
                 internal header was found (a flash tool and most emulators cannot place the ROM)",
                h.checksum, h.checksum_complement
            ),
        );
    }

    // File size.
    let mut len = rom.len();
    if h.mapper_kind == MapperKind::Dsp1 && len & 0x7FFF == crate::DSP1_FIRMWARE_LEN {
        len -= crate::DSP1_FIRMWARE_LEN; // firmware appended to the dump
    }
    if !len.is_multiple_of(0x8000) {
        out.add(
            Severity::Info,
            "SIZE_NOT_32K_MULTIPLE",
            format!("the ROM is {len} bytes, not a multiple of 32 KiB (a LoROM bank)"),
        );
    }
    if !len.is_power_of_two() {
        let up = len.next_power_of_two();
        let pieces = len.count_ones();
        let shape = if pieces == 2 {
            "the sum of two powers of two (a mirrored layout)"
        } else {
            "neither a power of two nor the sum of two (an emulator has to guess the padding)"
        };
        // A burned chip is a power of two: the board needs the completed
        // image, the smaller tail repeated as the board maps it (SFC dev
        // wiki "Non-Power-of-Two ROM Size": 20 Mbit = ABCDEEEE).
        out.add(
            Severity::Warn,
            "SIZE_NOT_POWER_OF_TWO",
            format!(
                "the ROM is {len} bytes ({} KiB), {shape}; the next power of two is {up} bytes                  ({} KiB, 2^{}): a repro board takes a power-of-two chip and requires the \
                 completed image, the tail mirrored as the board maps it (ABCDEEEE for \
                 2.5 MB in 4 MB), with the checksum recomputed on it",
                len / 1024,
                up / 1024,
                up.ilog2()
            ),
        );
    }
    let size_byte = rom[off + 0x17];
    let expected = len.next_power_of_two();
    let declared = (size_byte < 32)
        .then(|| 1usize << size_byte)
        .map(|kb| kb * 1024);
    if declared != Some(expected) {
        let said = declared.map_or_else(
            || format!("${size_byte:02X}"),
            |d| format!("${size_byte:02X} = {} KiB", d / 1024),
        );
        out.add(
            Severity::Warn,
            "ROM_SIZE_BYTE_MISMATCH",
            format!(
                "the ROM size byte says {said}; a {len}-byte image rounds up to {} KiB (${:02X}): \
                 the byte is 1 << n KiB, rounded up",
                expected / 1024,
                expected.ilog2() - 10
            ),
        );
    }

    // BigLoROM: a LoROM image above 2 MiB. Bank n contributes 32 KiB at
    // $8000-$FFFF; fullsnes (Cartridge ROM Capacity): plain LoROM is
    // "max 2MByte ROM (banks 00h-3Fh, with mirror at 80h-BFh)", larger games
    // "map additional LoROM banks into HiROM area (BigLoROM)". Luna's mapper
    // continues the linear 32 KiB banks through $40-$7D (verified in
    // luna-bus lorom.rs); that the lower halves of those banks are ROM too on
    // a real board (fullsnes: "mapped twice in the 64K hirom banks") is an
    // hypothesis luna does not model.
    if h.mapper_kind == MapperKind::LoRom && len > 0x20_0000 {
        out.add(
            Severity::Info,
            "LOROM_BIGLOROM",
            format!(
                "a LoROM image of {} KiB: the data above 2 MiB sits in banks $40-$7D / $C0-$FF \
                 (BigLoROM), never in $00-$3F, so the code must reach it there; the board must \
                 wire bank bit BA6 to the ROM (a plain 2 MiB LoROM board does not) and its size \
                 byte must read ${:02X}",
                len / 1024,
                len.next_power_of_two().ilog2() - 10
            ),
        );
        if rom[off + 0x18] != 0 && len > 0x30_0000 {
            out.add(
                Severity::Info,
                "LOROM_BIGLOROM_SRAM",
                "above 3 MiB of LoROM the SRAM must sit in the lower 32 KiB of banks \
                 $70-$7D only (fullsnes, LoROM with SRAM: newer boards free the upper halves \
                 of those banks as ROM); an older board maps it over whole 64 KiB banks"
                    .into(),
            );
        }
    }

    // Map mode.
    let map = rom[off + 0x15];
    let ok_modes: &[u8] = match off {
        HEADER_OFFSET_LOROM => &[0x0, 0x2, 0x3],
        HEADER_OFFSET_HIROM => &[0x1, 0x5, 0xA],
        _ => &[0x5],
    };
    if map & 0xE0 != 0x20 || !ok_modes.contains(&(map & 0x0F)) {
        out.add(
            Severity::Warn,
            "MAP_MODE_MISMATCH",
            format!(
                "the map byte ${map:02X} (001s mmmm: s = FastROM, m = layout) does not fit a \
                 header found at the {} position (expects $2m/$3m with m in {:?}); a loader \
                 that trusts the byte maps the ROM wrongly",
                match off {
                    HEADER_OFFSET_LOROM => "LoROM",
                    HEADER_OFFSET_HIROM => "HiROM",
                    _ => "ExHiROM",
                },
                ok_modes
            ),
        );
    }

    // Vectors ($FFE0..$FFFF is off + 0x20..).
    let vec_at = |ofs: usize| u16::from_le_bytes([rom[off + 0x20 + ofs], rom[off + 0x21 + ofs]]);
    for (code, name, ofs, sev) in [
        (
            "VECTOR_NMI_NATIVE",
            "native NMI ($FFEA)",
            0x0A,
            Severity::Warn,
        ),
        (
            "VECTOR_IRQ_NATIVE",
            "native IRQ ($FFEE)",
            0x0E,
            Severity::Warn,
        ),
        (
            "VECTOR_NMI_EMU",
            "emulation NMI ($FFFA)",
            0x1A,
            Severity::Warn,
        ),
        (
            "VECTOR_RESET_EMU",
            "emulation RESET ($FFFC)",
            0x1C,
            Severity::Error,
        ),
        (
            "VECTOR_IRQ_EMU",
            "emulation IRQ/BRK ($FFFE)",
            0x1E,
            Severity::Warn,
        ),
    ] {
        let v = vec_at(ofs);
        if v == 0x0000 || v == 0xFFFF || v < 0x8000 {
            out.add(
                sev,
                code,
                format!(
                    "the {name} vector is ${v:04X}; it must point into ROM ($8000..$FFFE), \
                     {}",
                    if sev == Severity::Error {
                        "the console boots there"
                    } else {
                        "an interrupt taken there runs garbage"
                    }
                ),
            );
        }
    }

    // Chipset / SRAM.
    let chipset = rom[off + 0x16];
    let (lo, hi) = (chipset & 0x0F, chipset >> 4);
    let subtype = rom[off - 1];
    let known_chip = matches!(lo, 0..=6 | 9 | 0xA) && matches!(hi, 0..=5 | 0xE | 0xF);
    if !known_chip {
        out.add(
            Severity::Warn,
            "CHIPSET_UNKNOWN",
            format!("the chipset byte ${chipset:02X} is not a documented ROM/RAM/battery/coprocessor code"),
        );
    } else if hi == 0xF && !matches!(subtype, 0x00..=0x03 | 0x10) {
        out.add(
            Severity::Warn,
            "CHIPSET_UNKNOWN",
            format!("chipset ${chipset:02X} selects a custom chip by the sub-type byte $FFBF, whose ${subtype:02X} is unknown"),
        );
    }
    if let Some(chip) = h.unsupported_chip {
        out.add(
            Severity::Warn,
            "CHIPSET_NOT_EMULATED",
            chip_msg(chipset, &chip.to_string()),
        );
    } else if hi == 0xF && lo >= 3 && subtype == 0x03 {
        out.add(
            Severity::Warn,
            "CHIPSET_NOT_EMULATED",
            chip_msg(chipset, "Cx4"),
        );
    } else if lo >= 3 && matches!(hi, 0 | 1 | 3 | 4) {
        out.add(
            Severity::Info,
            "CHIPSET_COPROCESSOR",
            format!(
                "chipset ${chipset:02X}: the board carries a coprocessor ({:?} in luna); the \
                 physical cartridge needs that chip (and its firmware, if any) fitted",
                h.mapper_kind
            ),
        );
    }
    let sram_byte = rom[off + 0x18];
    let ram_code = matches!(lo, 1 | 2 | 4 | 5 | 9 | 0xA);
    if known_chip {
        if matches!(lo, 2 | 5 | 9 | 0xA) && sram_byte == 0 && h.mapper_kind != MapperKind::SuperFx {
            out.add(
                Severity::Warn,
                "SRAM_BATTERY_WITHOUT_SRAM",
                format!(
                    "chipset ${chipset:02X} declares battery-backed RAM but the SRAM size byte is 0; \
                     set the size ($FFD8) or drop the battery from the chipset byte"
                ),
            );
        }
        if sram_byte != 0 && !ram_code {
            out.add(
                Severity::Warn,
                "SRAM_WITHOUT_RAM_CHIPSET",
                format!(
                    "the SRAM size byte is ${sram_byte:02X} but chipset ${chipset:02X} says no RAM; \
                     set the chipset RAM (and battery) code, or clear the size"
                ),
            );
        } else if sram_byte != 0 && matches!(lo, 1 | 4) {
            out.add(
                Severity::Info,
                "SRAM_NOT_BATTERY_BACKED",
                format!(
                    "{} KiB of RAM without a battery (chipset ${chipset:02X}): its contents are \
                     lost at power-off; a save game needs the battery code",
                    1u32 << sram_byte.min(7)
                ),
            );
        }
    }
    if opts.no_sram && (sram_byte != 0 || ram_code) {
        out.add(
            Severity::Error,
            "SRAM_DECLARED",
            format!(
                "the header declares save RAM (size byte ${sram_byte:02X} at $FFD8, chipset \
                 ${chipset:02X}) but this cartridge must save nothing: set $FFD8 to 0 and use a \
                 chipset byte without RAM or battery ($00)"
            ),
        );
    }
    if sram_byte > 7 {
        out.add(
            Severity::Info,
            "SRAM_SIZE_IMPLAUSIBLE",
            format!("the SRAM size byte ${sram_byte:02X} means more than 128 KiB; real boards stop at 1..=5 (2..32 KiB) and a few at 7"),
        );
    }

    // Country / video standard.
    let country = rom[off + 0x19];
    if country <= 0x14 {
        let video = match crate::Region::from_country(country) {
            crate::Region::Ntsc => "NTSC (60 Hz)",
            crate::Region::Pal => "PAL (50 Hz)",
            crate::Region::Unknown => "no video standard",
        };
        out.add(
            Severity::Info,
            "REGION_DECLARED",
            format!("country byte ${country:02X} at $FFD9: the cartridge declares {video}"),
        );
    }
    if country > 0x14 {
        out.add(
            Severity::Warn,
            "COUNTRY_UNKNOWN",
            format!("the country byte ${country:02X} is outside the documented codes $00..$14; the video standard cannot be derived"),
        );
    } else if country == 0x0E {
        out.add(
            Severity::Info,
            "COUNTRY_VIDEO_AMBIGUOUS",
            "the country byte $0E (Common) does not say NTSC or PAL; luna then keeps its default timing".into(),
        );
    } else if rom[off + 0x1A] == 0x33 {
        // Extended header: the last character of the game code ($FFB5)
        // repeats the destination (snesdev wiki table).
        let letter = rom[off - 0x0B];
        let expect = match country {
            0x00 => Some(b"J".as_slice()),
            0x01 => Some(b"E".as_slice()),
            0x02 => Some(b"P".as_slice()),
            0x03 => Some(b"W".as_slice()),
            0x06 => Some(b"F".as_slice()),
            0x07 => Some(b"H".as_slice()),
            0x08 => Some(b"S".as_slice()),
            0x09 => Some(b"D".as_slice()),
            0x0A => Some(b"I".as_slice()),
            0x0B => Some(b"C".as_slice()),
            0x0D => Some(b"K".as_slice()),
            0x0F => Some(b"N".as_slice()),
            0x10 => Some(b"BG".as_slice()),
            0x11 => Some(b"U".as_slice()),
            _ => None,
        };
        if let Some(e) = expect
            && letter.is_ascii_uppercase()
            && !e.contains(&letter)
        {
            out.add(
                Severity::Info,
                "COUNTRY_GAMECODE_MISMATCH",
                format!(
                    "country ${country:02X} expects game-code letter '{}' at $FFB5 but it is '{}'",
                    e[0] as char, letter as char
                ),
            );
        }
    }

    // Title.
    let title = &rom[off..off + 21];
    if let Some(i) = title.iter().position(|b| !(0x20..=0x7E).contains(b)) {
        let b = title[i];
        out.add(
            Severity::Warn,
            "TITLE_NOT_PRINTABLE",
            format!(
                "title byte {i} is ${b:02X}{}; the title is 21 bytes of ASCII $20..$7E, unused \
                 bytes being spaces",
                if matches!(b, 0x00 | 0xFF) {
                    " (zero/$FF padding instead of spaces)"
                } else {
                    ""
                }
            ),
        );
    } else if title.iter().all(|&b| b == b' ') {
        out.add(
            Severity::Info,
            "TITLE_EMPTY",
            "the title is 21 spaces".into(),
        );
    }

    out.0
}

fn chip_msg(chipset: u8, name: &str) -> String {
    format!(
        "chipset ${chipset:02X} names a {name}, which luna does not emulate: this cartridge \
         cannot be checked in luna"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A healthy 32 KiB `LoROM`: ROM only, no SRAM, NTSC, vectors in ROM,
    /// valid title, real checksum.
    fn healthy() -> Vec<u8> {
        let mut rom = vec![0xEA; 0x8000];
        let h = HEADER_OFFSET_LOROM;
        rom[h..h + 21].copy_from_slice(b"AUDIT TEST           ");
        rom[h + 0x15] = 0x20;
        rom[h + 0x16] = 0x00;
        rom[h + 0x17] = 0x05; // 32 KiB
        rom[h + 0x18] = 0x00;
        rom[h + 0x19] = 0x01;
        set_vectors(&mut rom, h);
        fix_checksum(&mut rom, h);
        rom
    }

    fn set_vectors(rom: &mut [u8], h: usize) {
        // header at $FFC0 -> vectors $FFE0 = h + 0x20
        for ofs in [0x0A, 0x0E, 0x1A, 0x1C, 0x1E] {
            rom[h + 0x20 + ofs] = 0x00;
            rom[h + 0x21 + ofs] = 0x80;
        }
    }

    fn fix_checksum(rom: &mut [u8], h: usize) {
        rom[h + 0x1C..h + 0x20].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00]);
        let sum = computed_checksum(rom);
        rom[h + 0x1C..h + 0x1E].copy_from_slice(&(!sum).to_le_bytes());
        rom[h + 0x1E..h + 0x20].copy_from_slice(&sum.to_le_bytes());
    }

    fn codes(rom: &[u8]) -> Vec<&'static str> {
        audit(rom).iter().map(|f| f.code).collect()
    }

    fn bad(rom: &[u8]) -> Vec<&'static str> {
        audit(rom)
            .iter()
            .filter(|f| f.severity != Severity::Info)
            .map(|f| f.code)
            .collect()
    }

    /// Apply `edit` to a healthy ROM, refresh the checksum, and expect the
    /// rule `code` (and only it, among Error/Warn) to fire.
    fn only(code: &str, edit: impl FnOnce(&mut Vec<u8>)) {
        let mut rom = healthy();
        edit(&mut rom);
        fix_checksum(&mut rom, HEADER_OFFSET_LOROM);
        assert_eq!(bad(&rom), vec![code], "{:#?}", audit(&rom));
    }

    #[test]
    fn a_healthy_rom_has_no_error_or_warning() {
        assert!(bad(&healthy()).is_empty(), "{:#?}", audit(&healthy()));
        assert!(codes(&healthy()).contains(&"HEADER_FOUND"));
    }

    #[test]
    fn copier_header() {
        let mut f = vec![0u8; 512];
        f.extend(healthy());
        let b = bad(&f);
        assert_eq!(b, vec!["SMC_COPIER_HEADER"]);
    }

    #[test]
    fn too_small() {
        assert_eq!(bad(&[0u8; 100]), vec!["ROM_TOO_SMALL"]);
    }

    #[test]
    fn complement_invalid() {
        let mut rom = healthy();
        rom[HEADER_OFFSET_LOROM + 0x1C] ^= 0x01;
        assert_eq!(bad(&rom), vec!["CHECKSUM_COMPLEMENT_INVALID"]);
    }

    #[test]
    fn checksum_mismatch() {
        let mut rom = healthy();
        rom[0x100] = 0x00; // data edit after the checksum was set
        assert_eq!(bad(&rom), vec!["CHECKSUM_MISMATCH"]);
    }

    #[test]
    fn size_rules() {
        // 48 KiB = 32 + 16: sum of two powers of two -> Warn as well (a
        // chip is a power of two), with the size to fill up to.
        let mut rom = healthy();
        rom.extend(vec![0xEA; 0x4000]);
        let h = HEADER_OFFSET_LOROM;
        rom[h + 0x17] = 0x06; // 64 KiB
        fix_checksum(&mut rom, h);
        assert_eq!(bad(&rom), vec!["SIZE_NOT_POWER_OF_TWO"]);
        assert!(codes(&rom).contains(&"SIZE_NOT_32K_MULTIPLE"));
        // 32 + 16 + 8 KiB: three bits -> Warn.
        let mut rom = healthy();
        rom.extend(vec![0xEA; 0x6000]);
        rom[h + 0x17] = 0x06;
        fix_checksum(&mut rom, h);
        assert_eq!(bad(&rom), vec!["SIZE_NOT_POWER_OF_TWO"]);
        // 64 KiB of data under a 32 KiB size byte, and the converse.
        let mut rom = healthy();
        rom.extend(vec![0xEA; 0x8000]);
        fix_checksum(&mut rom, h);
        assert_eq!(bad(&rom), vec!["ROM_SIZE_BYTE_MISMATCH"]);
        only("ROM_SIZE_BYTE_MISMATCH", |r| r[h + 0x17] = 0x07);
    }

    /// A `LoROM` of `len` bytes (a multiple of 32 KiB) with the size byte
    /// `size_byte`; the data past the first 2 MiB differs per bank so a
    /// completed image can be told from its parts.
    fn lorom_of(len: usize, size_byte: u8) -> Vec<u8> {
        let mut rom = healthy();
        let h = HEADER_OFFSET_LOROM;
        rom.resize(len, 0xEA);
        rom[h + 0x17] = size_byte;
        fix_checksum(&mut rom, h);
        rom
    }

    /// The image ABCDEEEE: 2.5 MB (A..D = 2 MiB, E = 512 KiB) completed to
    /// 4 MiB by repeating the last 512 KiB four times.
    fn completed_2_5_mb(size_byte: u8) -> Vec<u8> {
        let mut rom = healthy();
        rom.resize(0x28_0000, 0xEA);
        let tail = rom[0x20_0000..].to_vec();
        for _ in 0..3 {
            rom.extend_from_slice(&tail);
        }
        assert_eq!(rom.len(), 0x40_0000);
        rom[HEADER_OFFSET_LOROM + 0x17] = size_byte;
        fix_checksum(&mut rom, HEADER_OFFSET_LOROM);
        rom
    }

    #[test]
    fn a_non_power_of_two_size_names_the_fill_and_asks_for_the_completed_image() {
        let rom = lorom_of(0x28_0000, 0x0C); // 2.5 MB, the byte already says 4 MiB
        assert_eq!(bad(&rom), vec!["SIZE_NOT_POWER_OF_TWO"]);
        let f = audit(&rom)
            .into_iter()
            .find(|f| f.code == "SIZE_NOT_POWER_OF_TWO")
            .unwrap();
        assert_eq!(f.severity, Severity::Warn);
        for needle in [
            "2621440 bytes",
            "4194304 bytes",
            "4096 KiB",
            "completed image",
        ] {
            assert!(f.message.contains(needle), "{needle}: {}", f.message);
        }
    }

    #[test]
    fn a_2_5_mb_lorom_completed_to_4_mb_has_no_size_alert() {
        let rom = completed_2_5_mb(0x0C);
        assert!(bad(&rom).is_empty(), "{:#?}", audit(&rom));
        let c = codes(&rom);
        assert!(!c.contains(&"SIZE_NOT_POWER_OF_TWO"));
        assert!(c.contains(&"LOROM_BIGLOROM"));
        // The size byte must follow the real size: 4 MiB = $0C.
        let rom = completed_2_5_mb(0x0B); // says 2 MiB
        assert_eq!(bad(&rom), vec!["ROM_SIZE_BYTE_MISMATCH"]);
    }

    #[test]
    fn a_4_mb_lorom_is_biglorom_and_its_size_byte_is_checked() {
        let rom = lorom_of(0x40_0000, 0x0C);
        assert!(bad(&rom).is_empty(), "{:#?}", audit(&rom));
        let f = audit(&rom)
            .into_iter()
            .find(|f| f.code == "LOROM_BIGLOROM")
            .unwrap();
        assert!(f.message.contains("$40-$7D") && f.message.contains("BA6"));
        assert!(f.message.contains("$0C"), "{}", f.message);
        assert_eq!(
            bad(&lorom_of(0x40_0000, 0x0B)),
            vec!["ROM_SIZE_BYTE_MISMATCH"]
        );
        // 2 MiB exactly is plain LoROM: no BigLoROM note.
        assert!(!codes(&lorom_of(0x20_0000, 0x0B)).contains(&"LOROM_BIGLOROM"));
    }

    #[test]
    fn biglorom_above_3_mb_with_sram_notes_the_bank_halves() {
        let h = HEADER_OFFSET_LOROM;
        let mut rom = lorom_of(0x40_0000, 0x0C);
        rom[h + 0x16] = 0x02;
        rom[h + 0x18] = 0x05;
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).is_empty(), "{:#?}", audit(&rom));
        assert!(codes(&rom).contains(&"LOROM_BIGLOROM_SRAM"));
    }

    #[test]
    fn map_mode() {
        only("MAP_MODE_MISMATCH", |r| {
            r[HEADER_OFFSET_LOROM + 0x15] = 0x21;
        });
        only("MAP_MODE_MISMATCH", |r| {
            r[HEADER_OFFSET_LOROM + 0x15] = 0x00;
        });
        // FastROM LoROM is fine.
        let mut rom = healthy();
        rom[HEADER_OFFSET_LOROM + 0x15] = 0x30;
        fix_checksum(&mut rom, HEADER_OFFSET_LOROM);
        assert!(bad(&rom).is_empty());
    }

    #[test]
    fn vectors() {
        let h = HEADER_OFFSET_LOROM;
        for (ofs, code) in [
            (0x0A, "VECTOR_NMI_NATIVE"),
            (0x0E, "VECTOR_IRQ_NATIVE"),
            (0x1A, "VECTOR_NMI_EMU"),
            (0x1C, "VECTOR_RESET_EMU"),
            (0x1E, "VECTOR_IRQ_EMU"),
        ] {
            for v in [0x0000u16, 0xFFFF, 0x7FFF] {
                only(code, |r| {
                    r[h + 0x20 + ofs..h + 0x22 + ofs].copy_from_slice(&v.to_le_bytes());
                });
            }
        }
        let mut rom = healthy();
        rom[h + 0x3C..h + 0x3E].copy_from_slice(&[0, 0]);
        fix_checksum(&mut rom, h);
        assert!(
            audit(&rom)
                .iter()
                .any(|f| f.code == "VECTOR_RESET_EMU" && f.severity == Severity::Error)
        );
    }

    #[test]
    fn sram_and_chipset() {
        let h = HEADER_OFFSET_LOROM;
        only("SRAM_BATTERY_WITHOUT_SRAM", |r| r[h + 0x16] = 0x02);
        only("SRAM_WITHOUT_RAM_CHIPSET", |r| r[h + 0x18] = 0x03);
        // RAM, no battery: Info.
        let mut rom = healthy();
        rom[h + 0x16] = 0x01;
        rom[h + 0x18] = 0x03;
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).is_empty());
        assert!(codes(&rom).contains(&"SRAM_NOT_BATTERY_BACKED"));
        // Battery + SRAM: clean.
        rom[h + 0x16] = 0x02;
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).is_empty(), "{:#?}", audit(&rom));
        only("CHIPSET_UNKNOWN", |r| r[h + 0x16] = 0x7F);
    }

    #[test]
    fn coprocessors() {
        let h = HEADER_OFFSET_LOROM;
        // OBC1 (high nibble 2): recognised, not emulated.
        let mut rom = healthy();
        rom[h + 0x16] = 0x23;
        fix_checksum(&mut rom, h);
        assert_eq!(bad(&rom), vec!["CHIPSET_NOT_EMULATED"]);
        // Cx4 by sub-type.
        let mut rom = healthy();
        rom[h + 0x16] = 0xF3;
        rom[h - 1] = 0x10;
        fix_checksum(&mut rom, h);
        assert_eq!(bad(&rom), vec!["CHIPSET_NOT_EMULATED"]);
        // Unknown sub-type.
        let mut rom = healthy();
        rom[h + 0x16] = 0xF3;
        rom[h - 1] = 0x77;
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).contains(&"CHIPSET_UNKNOWN"));
        // Emulated coprocessor (SA-1 board): Info only.
        let mut rom = healthy();
        rom[h + 0x16] = 0x34;
        rom[h + 0x18] = 0x00;
        fix_checksum(&mut rom, h);
        assert!(codes(&rom).contains(&"CHIPSET_COPROCESSOR"));
    }

    #[test]
    fn country() {
        let h = HEADER_OFFSET_LOROM;
        only("COUNTRY_UNKNOWN", |r| r[h + 0x19] = 0x30);
        let mut rom = healthy();
        rom[h + 0x19] = 0x0E;
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).is_empty());
        assert!(codes(&rom).contains(&"COUNTRY_VIDEO_AMBIGUOUS"));
        // Extended header: game code ends in 'P' (Europe) under country USA.
        let mut rom = healthy();
        rom[h + 0x1A] = 0x33;
        rom[h - 0x0B] = b'P';
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).is_empty());
        assert!(codes(&rom).contains(&"COUNTRY_GAMECODE_MISMATCH"));
        rom[h - 0x0B] = b'E';
        fix_checksum(&mut rom, h);
        assert!(!codes(&rom).contains(&"COUNTRY_GAMECODE_MISMATCH"));
    }

    #[test]
    fn title() {
        let h = HEADER_OFFSET_LOROM;
        only("TITLE_NOT_PRINTABLE", |r| r[h + 10] = 0x00);
        only("TITLE_NOT_PRINTABLE", |r| r[h + 3] = 0xB6);
        let mut rom = healthy();
        rom[h..h + 21].fill(b' ');
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).is_empty());
        assert!(codes(&rom).contains(&"TITLE_EMPTY"));
    }

    #[test]
    fn a_hirom_header_is_found_and_judged_at_its_own_position() {
        let mut rom = vec![0xEA; 0x10000];
        let h = HEADER_OFFSET_HIROM;
        rom[h..h + 21].copy_from_slice(b"AUDIT HIROM          ");
        rom[h + 0x15] = 0x21;
        rom[h + 0x17] = 0x06; // 64 KiB
        rom[h + 0x19] = 0x01;
        set_vectors(&mut rom, h);
        fix_checksum(&mut rom, h);
        assert!(bad(&rom).is_empty(), "{:#?}", audit(&rom));
        rom[h + 0x15] = 0x20; // LoROM byte at the HiROM position
        fix_checksum(&mut rom, h);
        assert_eq!(bad(&rom), vec!["MAP_MODE_MISMATCH"]);
    }

    #[test]
    fn no_sram_refuses_a_declared_sram_and_names_the_region() {
        let h = HEADER_OFFSET_LOROM;
        let strict = AuditOptions { no_sram: true };
        // Healthy: no SRAM, so no error even under the policy.
        assert!(
            audit_with(&healthy(), strict)
                .iter()
                .all(|f| f.severity == Severity::Info)
        );
        // The region is always said.
        let f = audit(&healthy());
        let region = f.iter().find(|f| f.code == "REGION_DECLARED").unwrap();
        assert!(region.message.contains("NTSC"), "{}", region.message);
        let mut pal = healthy();
        pal[h + 0x19] = 0x02;
        fix_checksum(&mut pal, h);
        let f = audit(&pal);
        assert!(
            f.iter()
                .any(|f| f.code == "REGION_DECLARED" && f.message.contains("PAL"))
        );
        // SRAM declared: only an error under the policy.
        let mut sram = healthy();
        sram[h + 0x16] = 0x02;
        sram[h + 0x18] = 0x03;
        fix_checksum(&mut sram, h);
        assert!(!audit(&sram).iter().any(|f| f.code == "SRAM_DECLARED"));
        assert!(
            audit_with(&sram, strict)
                .iter()
                .any(|f| f.code == "SRAM_DECLARED" && f.severity == Severity::Error)
        );
    }
}
