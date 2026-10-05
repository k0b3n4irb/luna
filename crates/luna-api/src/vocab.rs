//! Names the front-ends share: the video standard, the `--force-mapper`
//! list and the S-DSP registers. One copy here, so the CLI, `luna test`
//! manifests and the MCP server cannot drift apart. Each caller keeps its
//! own error wording — it names the flag, manifest key or tool parameter
//! its user typed.

use crate::{MapperKind, Region};

/// Parse a video-standard name — the CLI `--force-region`, a manifest
/// `region`, the MCP `force_region`: `ntsc` or `pal`, case-insensitive.
/// `None` for anything else.
#[must_use]
pub fn parse_region(name: &str) -> Option<Region> {
    match name.to_ascii_lowercase().as_str() {
        "ntsc" => Some(Region::Ntsc),
        "pal" => Some(Region::Pal),
        _ => None,
    }
}

/// The `--force-mapper` names as one phrase for a help text or an error,
/// built from [`MapperKind::CLI_NAMES`]: the mappers luna emulates, then
/// the names that parse but that the load refuses.
#[must_use]
pub fn force_mapper_names() -> String {
    let refused = MapperKind::CLI_NAMES_NOT_EMULATED;
    let emulated: Vec<&str> = MapperKind::CLI_NAMES
        .into_iter()
        .filter(|n| !refused.contains(n))
        .collect();
    let mut out = emulated.join(", ");
    if !refused.is_empty() {
        out.push_str("; ");
        out.push_str(&refused.join(", "));
        out.push_str(" is recognised but not emulated, the load refuses it");
    }
    out
}

/// The ten per-voice S-DSP registers (`$x0`-`$x9`): the name the
/// `--dsp-trace` CSV prints, then the other spellings a name lookup takes.
const DSP_VOICE_REGS: [(&str, &[&str]); 10] = [
    ("VOLL", &[]),
    ("VOLR", &[]),
    ("PL", &["PITCHL"]),
    ("PH", &["PITCHH"]),
    ("SRCN", &[]),
    ("ADSR1", &[]),
    ("ADSR2", &[]),
    ("GAIN", &[]),
    // The two read-back registers: ENVX is the envelope's top 7 bits, OUTX
    // the voice's last output >> 8 (ares `dsp/voice.cpp`, Mesen2 `SnesDsp`).
    ("ENVX", &[]),
    ("OUTX", &[]),
];

/// The global S-DSP registers: index, printed name, other spellings.
const DSP_GLOBAL_REGS: [(u8, &str, &[&str]); 15] = [
    (0x0C, "MVOLL", &["MVOL_L"]),
    (0x1C, "MVOLR", &["MVOL_R"]),
    (0x2C, "EVOLL", &["EVOL_L"]),
    (0x3C, "EVOLR", &["EVOL_R"]),
    (0x4C, "KON", &[]),
    (0x5C, "KOFF", &["KOF"]),
    (0x6C, "FLG", &[]),
    (0x7C, "ENDX", &[]),
    (0x0D, "EFB", &[]),
    (0x2D, "PMON", &[]),
    (0x3D, "NON", &[]),
    (0x4D, "EON", &[]),
    (0x5D, "DIR", &[]),
    (0x6D, "ESA", &[]),
    (0x7D, "EDL", &[]),
];

/// The name of S-DSP register `reg` (`$00-$7F`) as the `--dsp-trace` CSV
/// prints it: `V3_PL`, `KOFF`, `MVOLL`, `FIR5`. An index with no register
/// behind it reads as `$xx`.
#[must_use]
pub fn dsp_register_name(reg: u8) -> String {
    let voice = reg >> 4;
    if reg < 0x80 {
        match reg & 0x0F {
            0xF => return format!("FIR{voice}"),
            lo => {
                if let Some((name, _)) = DSP_VOICE_REGS.get(usize::from(lo)) {
                    return format!("V{voice}_{name}");
                }
            }
        }
        if let Some((_, name, _)) = DSP_GLOBAL_REGS.iter().find(|(i, ..)| *i == reg) {
            return (*name).to_string();
        }
    }
    format!("${reg:02X}")
}

/// The S-DSP registers a debugger lists, in display order, each with the
/// name [`dsp_register_name`] gives it: the eighty per-voice registers
/// (`V0_VOLL` … `V7_OUTX`), the fifteen globals in Mesen2's order
/// (`MVOLL`, `MVOLR`, `EVOLL`, `EVOLR`, `KON`, `KOFF`, `FLG`, `ENDX`,
/// `EFB`, `PMON`, `NON`, `EON`, `DIR`, `ESA`, `EDL`), then `FIR0` … `FIR7`.
/// The 25 indices with no register behind them are left out. The GUI's
/// register viewer is this table, row for row.
#[must_use]
pub fn dsp_register_table() -> Vec<(u8, String)> {
    let voices = (0..8u8).flat_map(|v| (0..10u8).map(move |lo| (v << 4) | lo));
    let globals = DSP_GLOBAL_REGS.iter().map(|(i, ..)| *i);
    let fir = (0..8u8).map(|n| (n << 4) | 0x0F);
    voices
        .chain(globals)
        .chain(fir)
        .map(|i| (i, dsp_register_name(i)))
        .collect()
}

/// The index of the S-DSP register called `name`, case-insensitive: any
/// name [`dsp_register_name`] prints, the longer spellings (`V0_PITCHL`,
/// `MVOL_L`, `KOF`), or a raw hex index below `80`.
#[must_use]
pub fn dsp_register_index(name: &str) -> Option<u8> {
    let upper = name.to_ascii_uppercase();
    let is =
        |canonical: &str, others: &[&str]| canonical == upper || others.contains(&upper.as_str());
    if let Some((index, ..)) = DSP_GLOBAL_REGS.iter().find(|(_, n, alt)| is(n, alt)) {
        return Some(*index);
    }
    // FIR0..FIR7 at $x0F.
    if let Some(n) = upper.strip_prefix("FIR")
        && let Ok(i) = n.parse::<u8>()
        && i < 8
    {
        return Some((i << 4) | 0x0F);
    }
    // Per-voice: V<n>_<register>.
    if let Some(rest) = upper.strip_prefix('V')
        && let Some((v, reg)) = rest.split_once('_')
        && let Ok(v) = v.parse::<u8>()
        && v < 8
    {
        let lo = DSP_VOICE_REGS
            .iter()
            .position(|(n, alt)| *n == reg || alt.contains(&reg))?;
        return Some((v << 4) | u8::try_from(lo).ok()?);
    }
    // Raw hex index.
    u8::from_str_radix(name, 16).ok().filter(|&i| i < 0x80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_names_parse_whatever_their_case() {
        assert_eq!(parse_region("ntsc"), Some(Region::Ntsc));
        assert_eq!(parse_region("PAL"), Some(Region::Pal));
        assert_eq!(parse_region("Pal"), Some(Region::Pal));
        assert_eq!(parse_region("secam"), None);
        assert_eq!(parse_region(""), None);
        assert_eq!(parse_region(" pal"), None, "no trimming, as before");
    }

    /// Every mapper, spelled so that a new variant fails to compile here
    /// until it is added.
    const ALL_KINDS: [MapperKind; 8] = [
        MapperKind::LoRom,
        MapperKind::HiRom,
        MapperKind::ExHiRom,
        MapperKind::Sa1,
        MapperKind::SuperFx,
        MapperKind::Dsp1,
        MapperKind::Sdd1,
        MapperKind::Spc7110,
    ];

    const fn listed(kind: MapperKind) -> bool {
        match kind {
            MapperKind::LoRom
            | MapperKind::HiRom
            | MapperKind::ExHiRom
            | MapperKind::Sa1
            | MapperKind::SuperFx
            | MapperKind::Dsp1
            | MapperKind::Sdd1
            | MapperKind::Spc7110 => true,
        }
    }

    #[test]
    fn the_mapper_list_is_exactly_what_parses() {
        // Every listed name parses, to a different mapper each.
        let parsed: Vec<MapperKind> = MapperKind::CLI_NAMES
            .iter()
            .map(|n| {
                MapperKind::from_cli_str(n).unwrap_or_else(|| panic!("`{n}` is listed, not parsed"))
            })
            .collect();
        for (i, kind) in parsed.iter().enumerate() {
            assert!(!parsed[..i].contains(kind), "two listed names for {kind:?}");
        }
        // Every mapper that can be parsed is reached by a listed name: the
        // parser has one name per mapper, so nothing parses outside the list.
        for kind in ALL_KINDS {
            assert!(listed(kind));
            assert!(parsed.contains(&kind), "{kind:?} has no listed name");
        }
        assert_eq!(MapperKind::CLI_NAMES.len(), ALL_KINDS.len());
        // The names are the lower-case spelling a help text shows.
        for n in MapperKind::CLI_NAMES {
            assert_eq!(n, n.to_ascii_lowercase());
        }
        for n in MapperKind::CLI_NAMES_NOT_EMULATED {
            assert!(MapperKind::CLI_NAMES.contains(&n), "`{n}` is not listed");
        }
    }

    #[test]
    fn the_mapper_phrase_names_every_mapper_and_sets_the_refused_apart() {
        let phrase = force_mapper_names();
        assert_eq!(
            phrase,
            "lorom, hirom, exhirom, sa1, superfx, dsp1, sdd1; spc7110 is recognised \
             but not emulated, the load refuses it"
        );
        for n in MapperKind::CLI_NAMES {
            assert!(phrase.contains(n), "`{n}` missing from: {phrase}");
        }
    }

    #[test]
    fn dsp_names_are_the_csv_spelling() {
        assert_eq!(dsp_register_name(0x00), "V0_VOLL");
        assert_eq!(dsp_register_name(0x32), "V3_PL");
        assert_eq!(dsp_register_name(0x73), "V7_PH");
        assert_eq!(dsp_register_name(0x48), "V4_ENVX");
        assert_eq!(dsp_register_name(0x0C), "MVOLL");
        assert_eq!(dsp_register_name(0x5C), "KOFF");
        assert_eq!(dsp_register_name(0x7C), "ENDX");
        assert_eq!(dsp_register_name(0x7D), "EDL");
        assert_eq!(dsp_register_name(0x5F), "FIR5");
        // No register behind these.
        assert_eq!(dsp_register_name(0x1D), "$1D");
        assert_eq!(dsp_register_name(0x0A), "$0A");
        assert_eq!(dsp_register_name(0x7E), "$7E");
    }

    #[test]
    fn every_printed_dsp_name_reads_back_as_its_index() {
        for reg in 0u8..0x80 {
            let name = dsp_register_name(reg);
            if name.starts_with('$') {
                continue;
            }
            assert_eq!(dsp_register_index(&name), Some(reg), "{name}");
            assert_eq!(
                dsp_register_index(&name.to_ascii_lowercase()),
                Some(reg),
                "{name}, lower case"
            );
        }
    }

    #[test]
    fn dsp_index_takes_the_longer_spellings_and_raw_hex() {
        for (name, index) in [
            ("MVOL_L", 0x0C),
            ("MVOL_R", 0x1C),
            ("EVOL_L", 0x2C),
            ("EVOL_R", 0x3C),
            ("KOF", 0x5C),
            ("V0_PITCHL", 0x02),
            ("V7_PITCHH", 0x73),
            ("v3_gain", 0x37),
            ("FIR7", 0x7F),
            ("7D", 0x7D),
            ("08", 0x08),
        ] {
            assert_eq!(dsp_register_index(name), Some(index), "{name}");
        }
        for name in ["V8_GAIN", "V0_NOPE", "FIR8", "80", "BOGUS", ""] {
            assert_eq!(dsp_register_index(name), None, "{name}");
        }
    }

    #[test]
    fn the_register_table_is_every_named_register_under_the_csv_name() {
        let table = dsp_register_table();
        assert_eq!(table.len(), 103);
        // Every index exactly once, no `$xx` placeholder, and each name
        // resolves back to its index: a row pasted into `[asserts.dsp]`
        // or read off a `--dsp-trace` CSV names the same register.
        let mut seen = std::collections::BTreeSet::new();
        for (index, name) in &table {
            assert!(seen.insert(*index), "{index:#04x} listed twice");
            assert!(!name.starts_with('$'), "{index:#04x} has no name");
            assert_eq!(dsp_register_index(name), Some(*index), "{name}");
        }
        // The named registers are exactly those with a name.
        let named: Vec<u8> = (0..0x80u8)
            .filter(|&i| !dsp_register_name(i).starts_with('$'))
            .collect();
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), named);
        // Display order and the spellings the viewer shows.
        let row = |n: usize| (table[n].0, table[n].1.as_str());
        assert_eq!(row(0), (0x00, "V0_VOLL"));
        assert_eq!(row(2), (0x02, "V0_PL"));
        assert_eq!(row(79), (0x79, "V7_OUTX"));
        assert_eq!(row(80), (0x0C, "MVOLL"));
        assert_eq!(row(85), (0x5C, "KOFF"));
        assert_eq!(row(94), (0x7D, "EDL"));
        assert_eq!(row(95), (0x0F, "FIR0"));
        assert_eq!(row(102), (0x7F, "FIR7"));
    }
}
