//! String→value parsers for the CLI's flag mini-languages
//! (`--input`, `--mouse`, `--peek`, `--assert*`, `--mem-trace-*`).
//!
//! The parsing bodies are pure free functions with no I/O, so they are
//! unit-testable — these parsers are the front door of the
//! differential-debugging toolkit and a silent mis-parse corrupts a
//! debugging session, so they get pinned by tests. The one exception is
//! [`parse_input_script`], which resolves a leading `@` as a file path
//! (so an exported recording replays directly with `--input @file`); the
//! pure body it delegates to is [`parse_input_script_body`].

/// Parse a `--input` script into `(frame, mask)` checkpoints sorted by
/// ascending frame.
///
/// The script is comma- **and** newline-separated `frame:hex` entries; a
/// `#` starts a comment to end-of-line. The frame is decimal; the hex mask
/// accepts an optional `0x` prefix and is a 16-bit value. If the whole
/// argument starts with `@`, the rest is a **file path** whose contents are
/// parsed instead — so a recording exported by the GUI (a commented,
/// multi-line file) replays directly via `--input @recording.input`.
pub(crate) fn parse_input_script(script: &str) -> Result<Vec<(u64, u16)>, String> {
    let script = script.trim();
    if let Some(path) = script.strip_prefix('@') {
        let path = path.trim();
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("reading input file `{path}`: {e}"))?;
        parse_input_script_body(&contents)
    } else {
        parse_input_script_body(script)
    }
}

/// Pure `frame:hex` body parser (no I/O): comma/newline-separated entries,
/// `#` line comments, sorted by frame. See [`parse_input_script`].
pub(crate) fn parse_input_script_body(script: &str) -> Result<Vec<(u64, u16)>, String> {
    let mut out: Vec<(u64, u16)> = Vec::new();
    for line in script.lines() {
        // Drop a `#` comment (whole-line or trailing) before splitting.
        let line = line.split('#').next().unwrap_or("");
        for entry in line.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (frame_str, mask_str) = entry
                .split_once(':')
                .ok_or_else(|| format!("missing ':' in entry `{entry}`"))?;
            let frame: u64 = frame_str
                .trim()
                .parse()
                .map_err(|e| format!("bad frame `{frame_str}`: {e}"))?;
            let mask_str = mask_str
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            let mask: u16 = u16::from_str_radix(mask_str, 16)
                .map_err(|e| format!("bad hex mask `{mask_str}`: {e}"))?;
            out.push((frame, mask));
        }
    }
    out.sort_by_key(|(f, _)| *f);
    Ok(out)
}

/// A scripted mouse checkpoint: `(frame, (dx, dy, buttons))`.
pub(crate) type MouseCheckpoint = (u64, (i32, i32, u8));

/// Parse a `--mouse` script: `;`-separated `frame:dx,dy,buttons` entries
/// (signed `dx`/`dy`; `buttons` bit0 = left, bit1 = right). Returns the
/// checkpoints sorted by frame.
pub(crate) fn parse_mouse_script(script: &str) -> Result<Vec<MouseCheckpoint>, String> {
    let mut out = Vec::new();
    for entry in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (frame, rest) = entry
            .split_once(':')
            .ok_or_else(|| format!("`{entry}`: expected `frame:dx,dy,buttons`"))?;
        let frame: u64 = frame
            .trim()
            .parse()
            .map_err(|_| format!("`{entry}`: bad frame"))?;
        let p: Vec<&str> = rest.split(',').map(str::trim).collect();
        if p.len() != 3 {
            return Err(format!("`{entry}`: expected `dx,dy,buttons` (3 values)"));
        }
        let dx: i32 = p[0].parse().map_err(|_| format!("`{entry}`: bad dx"))?;
        let dy: i32 = p[1].parse().map_err(|_| format!("`{entry}`: bad dy"))?;
        let buttons: u8 = p[2]
            .parse()
            .map_err(|_| format!("`{entry}`: bad buttons"))?;
        out.push((frame, (dx, dy, buttons)));
    }
    out.sort_by_key(|(f, _)| *f);
    Ok(out)
}

/// Parse a `BANK:OFFSET:COUNT` peek spec (all hex, no `0x` prefix).
pub(crate) fn parse_peek_spec(spec: &str) -> Result<(u8, u16, u16), String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected BANK:OFFSET:COUNT, got `{spec}`"));
    }
    let bank = u8::from_str_radix(parts[0].trim(), 16)
        .map_err(|e| format!("bad bank `{}`: {e}", parts[0]))?;
    let offset = u16::from_str_radix(parts[1].trim(), 16)
        .map_err(|e| format!("bad offset `{}`: {e}", parts[1]))?;
    let count = u16::from_str_radix(parts[2].trim(), 16)
        .map_err(|e| format!("bad count `{}`: {e}", parts[2]))?;
    Ok((bank, offset, count))
}

/// Parse an even-length hex string (no `0x`) into bytes.
pub(crate) fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.is_empty() || !s.len().is_multiple_of(2) {
        return Err(format!("expected an even-length hex value, got `{s}`"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| format!("bad hex `{}`: {e}", &s[i..i + 2]))
        })
        .collect()
}

/// Parse a `BANK:OFFSET=HEXBYTES` assertion (all hex, no `0x`).
pub(crate) fn parse_assert_spec(spec: &str) -> Result<(u8, u16, Vec<u8>), String> {
    let (loc, val) = spec
        .split_once('=')
        .ok_or_else(|| format!("expected BANK:OFFSET=HEX, got `{spec}`"))?;
    let (bank_s, off_s) = loc
        .split_once(':')
        .ok_or_else(|| format!("expected BANK:OFFSET=HEX, got `{spec}`"))?;
    let bank =
        u8::from_str_radix(bank_s.trim(), 16).map_err(|e| format!("bad bank `{bank_s}`: {e}"))?;
    let offset =
        u16::from_str_radix(off_s.trim(), 16).map_err(|e| format!("bad offset `{off_s}`: {e}"))?;
    Ok((bank, offset, parse_hex_bytes(val)?))
}

/// Parse an `OFFSET=HEXBYTES` assertion (for ARAM / VRAM — no bank).
pub(crate) fn parse_assert_spec_no_bank(spec: &str) -> Result<(u16, Vec<u8>), String> {
    let (off_s, val) = spec
        .split_once('=')
        .ok_or_else(|| format!("expected OFFSET=HEX, got `{spec}`"))?;
    let offset =
        u16::from_str_radix(off_s.trim(), 16).map_err(|e| format!("bad offset `{off_s}`: {e}"))?;
    Ok((offset, parse_hex_bytes(val)?))
}

/// Parse a symbolic assert `NAME=HEXBYTES` (issue #77 — the WLA-DX
/// label form). Returns the raw name; resolution against the loaded
/// `.sym` table happens at the use site. The numeric `BANK:OFFSET=HEX`
/// form is tried FIRST by the caller, so this only sees specs whose
/// left side is not a bank:offset pair.
pub(crate) fn parse_assert_spec_sym(spec: &str) -> Result<(String, Vec<u8>), String> {
    let (name, val) = spec
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=HEX, got `{spec}`"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("empty symbol name in `{spec}`"));
    }
    Ok((name.to_string(), parse_hex_bytes(val)?))
}

/// Parse a symbolic peek `NAME[:COUNT]` (issue #77): a WLA-DX label,
/// optionally followed by a hex byte count (default 1). Splits on the
/// LAST `:` and only treats the tail as a count when it parses as hex —
/// so a label that itself contains `:` still resolves whole.
pub(crate) fn parse_peek_spec_sym(spec: &str) -> Result<(String, u16), String> {
    let spec = spec.trim();
    if let Some((name, count_s)) = spec.rsplit_once(':')
        && let Ok(count) = u16::from_str_radix(count_s.trim(), 16)
    {
        let name = name.trim();
        if name.is_empty() {
            return Err(format!("empty symbol name in `{spec}`"));
        }
        return Ok((name.to_string(), count));
    }
    if spec.is_empty() {
        return Err("empty --peek spec".into());
    }
    Ok((spec.to_string(), 1))
}

/// Parse a hex byte with optional `0x` prefix (the `--mem-trace-bank`
/// form). Extracted from the former inline match in `run_state` so it is
/// unit-testable; the error text feeds the same `eprintln!` as before.
pub(crate) fn parse_hex_u8(s: &str) -> Result<u8, String> {
    u8::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

/// Parse a `LO:HI` 16-bit hex address range with optional `0x` prefixes
/// (the `--mem-trace-addr` form, e.g. `2100:21FF`). Extracted from the
/// former inline closure in `run_state`; error strings preserve the
/// original diagnostics.
pub(crate) fn parse_addr_range(spec: &str) -> Result<(u16, u16), String> {
    let parse_hex16 = |h: &str| u16::from_str_radix(h.trim().trim_start_matches("0x"), 16);
    let Some((lo, hi)) = spec.split_once(':') else {
        return Err(format!("`{spec}`: expected `LO:HI` (e.g. 2100:21FF)"));
    };
    match (parse_hex16(lo), parse_hex16(hi)) {
        (Ok(lo), Ok(hi)) if lo <= hi => Ok((lo, hi)),
        (Ok(lo), Ok(hi)) => Err(format!("`{spec}`: LO ({lo:#06X}) > HI ({hi:#06X})")),
        _ => Err(format!("`{spec}`: expected hex `LO:HI`")),
    }
}

// =============================================================================
// Tests — these parsers front the differential-debugging toolkit; a silent
// mis-parse corrupts a debugging session, so every accepted/rejected form
// is pinned here.
// =============================================================================

/// Step `em` until PPU frame `frame` is reached, spending **at most**
/// `budget` instructions. Returns the instructions consumed.
///
/// This bound is what makes a scripted `--input` checkpoint honour
/// `-n` (issue #126): the pre-roll used to step to each checkpoint's
/// frame unconditionally and only THEN spend the requested budget, so
/// `-n 100000 --input "900:0x8000"` ran to frame 910 instead of frame
/// 12 — a run 75x longer than asked, in which a press scheduled far
/// beyond the requested window still reached the ROM. A checkpoint the
/// run never reaches must simply not fire.
///
/// Stops early when the emulator makes no progress (halted core), so a
/// dead ROM cannot spin here.
pub(crate) fn step_to_frame_bounded(em: &mut luna_api::Emulator, frame: u64, budget: u64) -> u64 {
    let start = em.instructions_executed();
    while em.state().scheduler.frame_count < frame {
        let spent = em.instructions_executed().saturating_sub(start);
        let left = budget.saturating_sub(spent);
        if left == 0 {
            break;
        }
        // Never overshoot the budget: cap the per-frame step by what is
        // left, so the last partial frame stops exactly on the limit.
        if em
            .step_until_frame(left.min(FRAME_STEP_BUDGET))
            .unwrap_or(0)
            == 0
        {
            break;
        }
    }
    em.instructions_executed().saturating_sub(start)
}

/// Per-call instruction cap while chasing a checkpoint frame. Large
/// enough for any real frame (a slow one is ~40k instructions), small
/// enough that the budget check above stays responsive.
const FRAME_STEP_BUDGET: u64 = 200_000;

/// An `APU:OFFSET:COUNT` peek target (issue #122): `OFFSET`/`COUNT` are
/// hex, and the read goes to **ARAM** (the SPC700's 64 KB address
/// space) rather than the CPU bus — so `APU:0200:16` dumps the uploaded
/// driver image and `APU:00F0:10` the `$F0-$FF` register page.
///
/// Returns `None` when the spec is not APU-prefixed, so the caller can
/// fall through to the CPU-bus and symbol forms.
pub(crate) fn parse_apu_peek_spec(spec: &str) -> Option<Result<(u16, u32), String>> {
    let rest = spec
        .trim()
        .strip_prefix("APU:")
        .or_else(|| spec.trim().strip_prefix("apu:"))?;
    let Some((off_s, count_s)) = rest.split_once(':') else {
        return Some(Err(format!(
            "expected APU:OFFSET:COUNT (hex), got `{spec}`"
        )));
    };
    let offset = match u16::from_str_radix(off_s.trim(), 16) {
        Ok(v) => v,
        Err(e) => return Some(Err(format!("bad ARAM offset `{off_s}`: {e}"))),
    };
    let count = match u32::from_str_radix(count_s.trim(), 16) {
        Ok(v) => v,
        Err(e) => return Some(Err(format!("bad count `{count_s}`: {e}"))),
    };
    Some(Ok((offset, count)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn apu_peek_spec_parses_and_is_opt_in() {
        use super::parse_apu_peek_spec;
        assert_eq!(parse_apu_peek_spec("APU:0200:16"), Some(Ok((0x0200, 0x16))));
        assert_eq!(parse_apu_peek_spec("apu:00F0:10"), Some(Ok((0x00F0, 0x10))));
        // Not APU-prefixed → None, so the CPU-bus / symbol forms still win.
        assert!(parse_apu_peek_spec("7E:0200:20").is_none());
        assert!(parse_apu_peek_spec("current_song:1").is_none());
        // Malformed APU spec reports rather than silently falling through.
        assert!(matches!(parse_apu_peek_spec("APU:0200"), Some(Err(_))));
        assert!(matches!(parse_apu_peek_spec("APU:zz:10"), Some(Err(_))));
    }

    use super::*;

    // --- parse_input_script -------------------------------------------------

    #[test]
    fn input_script_parses_and_sorts_by_frame() {
        let v = parse_input_script("1610:0,1600:0x1000").unwrap();
        assert_eq!(v, vec![(1600, 0x1000), (1610, 0)]);
    }

    #[test]
    fn input_script_accepts_0x_and_bare_hex_and_whitespace() {
        let v = parse_input_script(" 5 : 0X80 , 6:c0 ").unwrap();
        assert_eq!(v, vec![(5, 0x80), (6, 0xC0)]);
    }

    #[test]
    fn input_script_skips_empty_entries() {
        let v = parse_input_script("1:1,,2:2,").unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn input_script_rejects_missing_colon_and_bad_values() {
        assert!(
            parse_input_script("1600")
                .unwrap_err()
                .contains("missing ':'")
        );
        assert!(parse_input_script("x:1").unwrap_err().contains("bad frame"));
        assert!(
            parse_input_script("1:zz")
                .unwrap_err()
                .contains("bad hex mask")
        );
    }

    #[test]
    fn input_script_empty_string_is_empty_ok() {
        assert_eq!(parse_input_script("").unwrap(), vec![]);
    }

    #[test]
    fn input_script_handles_comments_and_newlines() {
        // Exactly the shape the GUI recording export writes (issue #83):
        // a `#` header, the P1 line, and a fully-commented P2 section.
        let file = "\
# luna input recording — player 1
# replay: luna state --input @file rom.sfc
1600:0x1000,1610:0x0000
# player 2 (not replayable via --input):
# 8000:0x8000
";
        let v = parse_input_script(file).unwrap();
        assert_eq!(v, vec![(1600, 0x1000), (1610, 0)], "P2 comment ignored");
    }

    #[test]
    fn input_script_strips_trailing_comment() {
        let v = parse_input_script("5:0x80  # press A").unwrap();
        assert_eq!(v, vec![(5, 0x80)]);
    }

    #[test]
    fn input_script_reads_from_at_file() {
        // `@path` loads and parses the file — the `--input @recording` path.
        let dir = std::env::temp_dir();
        let path = dir.join("luna_test_input_at_file.input");
        std::fs::write(&path, "# hdr\n3:0x1000\n7:0\n").unwrap();
        let arg = format!("@{}", path.display());
        let v = parse_input_script(&arg).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(v, vec![(3, 0x1000), (7, 0)]);
    }

    #[test]
    fn input_script_at_missing_file_errors() {
        let err = parse_input_script("@/no/such/luna_input_file.input").unwrap_err();
        assert!(err.contains("reading input file"), "got: {err}");
    }

    // --- parse_mouse_script -------------------------------------------------

    #[test]
    fn mouse_script_parses_signed_deltas_and_sorts() {
        let v = parse_mouse_script("20:0,0,0;10:-3,4,1").unwrap();
        assert_eq!(v, vec![(10, (-3, 4, 1)), (20, (0, 0, 0))]);
    }

    #[test]
    fn mouse_script_rejects_wrong_arity_and_bad_fields() {
        assert!(
            parse_mouse_script("1:2,3")
                .unwrap_err()
                .contains("3 values")
        );
        assert!(
            parse_mouse_script("1:a,0,0")
                .unwrap_err()
                .contains("bad dx")
        );
        assert!(
            parse_mouse_script("1:0,b,0")
                .unwrap_err()
                .contains("bad dy")
        );
        assert!(
            parse_mouse_script("1:0,0,9c")
                .unwrap_err()
                .contains("bad buttons")
        );
        assert!(
            parse_mouse_script("nocolon")
                .unwrap_err()
                .contains("expected")
        );
    }

    // --- parse_peek_spec ----------------------------------------------------

    #[test]
    fn peek_spec_parses_bank_offset_count_hex() {
        assert_eq!(parse_peek_spec("7E:2000:40").unwrap(), (0x7E, 0x2000, 0x40));
    }

    #[test]
    fn peek_spec_rejects_wrong_arity_and_bad_hex() {
        assert!(
            parse_peek_spec("7E:2000")
                .unwrap_err()
                .contains("BANK:OFFSET:COUNT")
        );
        assert!(parse_peek_spec("zz:0:1").unwrap_err().contains("bad bank"));
        assert!(
            parse_peek_spec("0:zz:1")
                .unwrap_err()
                .contains("bad offset")
        );
        assert!(parse_peek_spec("0:0:zz").unwrap_err().contains("bad count"));
    }

    // --- parse_hex_bytes ----------------------------------------------------

    #[test]
    fn hex_bytes_parses_pairs() {
        assert_eq!(parse_hex_bytes("00ffA5").unwrap(), vec![0x00, 0xFF, 0xA5]);
    }

    #[test]
    fn hex_bytes_rejects_odd_length_empty_and_bad_digits() {
        assert!(parse_hex_bytes("abc").unwrap_err().contains("even-length"));
        assert!(parse_hex_bytes("").unwrap_err().contains("even-length"));
        assert!(parse_hex_bytes("zz").unwrap_err().contains("bad hex"));
    }

    // --- parse_assert_spec / parse_assert_spec_no_bank ----------------------

    #[test]
    fn assert_spec_parses_bank_offset_bytes() {
        assert_eq!(
            parse_assert_spec("7E:0100=DEAD").unwrap(),
            (0x7E, 0x0100, vec![0xDE, 0xAD])
        );
    }

    #[test]
    fn assert_spec_rejects_missing_equals_or_colon() {
        assert!(
            parse_assert_spec("7E:0100")
                .unwrap_err()
                .contains("BANK:OFFSET=HEX")
        );
        assert!(
            parse_assert_spec("7E0100=00")
                .unwrap_err()
                .contains("BANK:OFFSET=HEX")
        );
    }

    #[test]
    fn assert_spec_no_bank_parses_offset_bytes() {
        assert_eq!(
            parse_assert_spec_no_bank("2140=81").unwrap(),
            (0x2140, vec![0x81])
        );
        assert!(
            parse_assert_spec_no_bank("2140")
                .unwrap_err()
                .contains("OFFSET=HEX")
        );
    }

    // --- symbolic forms (issue #77) ------------------------------------------

    #[test]
    fn assert_spec_sym_parses_name_and_bytes() {
        assert_eq!(
            parse_assert_spec_sym("r_done=EFBE").unwrap(),
            ("r_done".into(), vec![0xEF, 0xBE])
        );
        assert!(
            parse_assert_spec_sym("r_done")
                .unwrap_err()
                .contains("NAME=HEX")
        );
        assert!(
            parse_assert_spec_sym("=00")
                .unwrap_err()
                .contains("empty symbol")
        );
        assert!(
            parse_assert_spec_sym("x=zz")
                .unwrap_err()
                .contains("bad hex")
        );
    }

    #[test]
    fn peek_spec_sym_parses_name_and_optional_count() {
        assert_eq!(
            parse_peek_spec_sym("monster_x").unwrap(),
            ("monster_x".into(), 1)
        );
        assert_eq!(
            parse_peek_spec_sym("monster_x:10").unwrap(),
            ("monster_x".into(), 0x10)
        );
        // A non-hex tail is part of the name, not a count.
        assert_eq!(
            parse_peek_spec_sym("ns:label").unwrap(),
            ("ns:label".into(), 1)
        );
        assert!(parse_peek_spec_sym("").is_err());
        assert!(
            parse_peek_spec_sym(":10")
                .unwrap_err()
                .contains("empty symbol")
        );
    }

    // --- parse_hex_u8 / parse_addr_range (extracted from run_state) ---------

    #[test]
    fn hex_u8_accepts_bare_and_0x_forms() {
        assert_eq!(parse_hex_u8("7E").unwrap(), 0x7E);
        assert_eq!(parse_hex_u8("0x7e").unwrap(), 0x7E);
        assert!(parse_hex_u8("zz").is_err());
    }

    #[test]
    fn addr_range_parses_lo_hi_and_enforces_order() {
        assert_eq!(parse_addr_range("2100:21FF").unwrap(), (0x2100, 0x21FF));
        assert_eq!(parse_addr_range("0xFFEA:0xffea").unwrap(), (0xFFEA, 0xFFEA));
        assert!(parse_addr_range("21FF:2100").unwrap_err().contains('>'));
        assert!(parse_addr_range("2100").unwrap_err().contains("LO:HI"));
        assert!(
            parse_addr_range("zz:00")
                .unwrap_err()
                .contains("expected hex")
        );
    }
}
