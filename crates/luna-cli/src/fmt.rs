//! Small pure formatting helpers shared by the trace writers and the
//! human-readable state printers.

/// Lower-case hex of a byte slice (for assert PASS/FAIL output).
pub(crate) fn hex_str(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Format a 24-bit program counter / bus address as `$BB:OOOO`
/// (bank:offset) — the canonical PC column shared by the trace writers.
pub(crate) fn fmt_pc(pc_full: u32) -> String {
    format!("${:02X}:{:04X}", (pc_full >> 16) & 0xFF, pc_full & 0xFFFF)
}

/// Render the 65C816 P register as the canonical `NVMXDIZC` flag string
/// (upper-case = set), with the emulation bit appended.
pub(crate) fn flag_string(p: u8, e: bool) -> String {
    let bit = |mask: u8, c: char, fallback: char| if p & mask != 0 { c } else { fallback };
    format!(
        "{}{}{}{}{}{}{}{} (e={})",
        bit(0b1000_0000, 'N', 'n'),
        bit(0b0100_0000, 'V', 'v'),
        bit(0b0010_0000, 'M', 'm'),
        bit(0b0001_0000, 'X', 'x'),
        bit(0b0000_1000, 'D', 'd'),
        bit(0b0000_0100, 'I', 'i'),
        bit(0b0000_0010, 'Z', 'z'),
        bit(0b0000_0001, 'C', 'c'),
        u8::from(e),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_str_is_lowercase_pairs() {
        assert_eq!(hex_str(&[0xDE, 0xAD, 0x00]), "dead00");
        assert_eq!(hex_str(&[]), "");
    }

    #[test]
    fn fmt_pc_renders_bank_colon_offset() {
        assert_eq!(fmt_pc(0x7E_1234), "$7E:1234");
        assert_eq!(fmt_pc(0x00_000E), "$00:000E");
    }

    #[test]
    fn flag_string_uppercase_means_set() {
        assert_eq!(flag_string(0xFF, true), "NVMXDIZC (e=1)");
        assert_eq!(flag_string(0x00, false), "nvmxdizc (e=0)");
        assert_eq!(flag_string(0b1000_0001, false), "NvmxdizC (e=0)");
        assert_eq!(flag_string(0b0010_0100, true), "nvMxdIzc (e=1)");
    }
}
