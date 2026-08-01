//! `luna state` — the diagnostic run: traces, dumps, asserts,
//! screenshots, JSON state.

use std::process::ExitCode;

use crate::csv::{
    write_cpu_trace_csv, write_dma_trace_csv, write_mailbox_log_csv, write_mem_trace_csv,
    write_sa1_log_csv, write_sa1_side_log_csv, write_sa1_trace_csv, write_spc_trace_csv,
    write_superfx_trace_csv,
};
use crate::fmt::hex_str;
use crate::output::{print_hex_dump, write_wav};
use crate::parsers::{
    parse_addr_range, parse_assert_spec, parse_assert_spec_no_bank, parse_assert_spec_sym,
    parse_hex_u8, parse_input_script, parse_mouse_script, parse_peek_spec, parse_peek_spec_sym,
};
use crate::rom::load_rom_into;

/// `luna state` — exercise the public `luna-api` surface end-to-end.
pub(crate) fn run_state(
    rom: &std::path::Path,
    steps: u64,
    force_mapper: Option<&str>,
    force_region: Option<&str>,
    dump_vram_path: Option<&std::path::Path>,
    dump_coproc_ram_path: Option<&std::path::Path>,
    dump_aram_path: Option<&std::path::Path>,
    out: &std::path::Path,
    screenshot: Option<&std::path::Path>,
    audio_out: Option<&std::path::Path>,
    input_script: Option<&str>,
    port1: &str,
    port2: &str,
    mouse_script: Option<&str>,
    superscope_script: Option<&str>,
    peek_specs: &[String],
    assert_specs: &[String],
    assert_aram_specs: &[String],
    assert_vram_specs: &[String],
    assert_cgram_specs: &[String],
    until_frame: Option<u64>,
    srm_in: Option<&std::path::Path>,
    srm_out: Option<&std::path::Path>,
    apu_log_path: Option<&std::path::Path>,
    sa1_log_path: Option<&std::path::Path>,
    sa1_side_log_path: Option<&std::path::Path>,
    sa1_trace_path: Option<&std::path::Path>,
    sa1_trace_max: usize,
    superfx_trace_path: Option<&std::path::Path>,
    superfx_trace_max: usize,
    spc_trace_path: Option<&std::path::Path>,
    spc_trace_max: usize,
    cpu_trace_path: Option<&std::path::Path>,
    cpu_trace_from: u64,
    cpu_trace_max: usize,
    mem_trace_path: Option<&std::path::Path>,
    mem_trace_from: u64,
    mem_trace_max: usize,
    mem_trace_bank: Option<&str>,
    mem_trace_addr: Option<&str>,
    dma_trace_path: Option<&std::path::Path>,
    dma_trace_from: u64,
    dma_trace_max: usize,
    dsp1_rom: Option<&std::path::Path>,
    sym: Option<&std::path::Path>,
    load_state_path: Option<&std::path::Path>,
    wdm_out: Option<&std::path::Path>,
    print_fbhash: bool,
    native_res: bool,
) -> ExitCode {
    let mut em = luna_api::Emulator::new();
    if let Err(e) = load_rom_into(&mut em, rom, force_mapper, force_region, dsp1_rom) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    if native_res {
        // Issue #115: keep the un-collapsed 512×448 pixels alongside the
        // displayed frame; --screenshot / --print-fbhash emit them below.
        em.set_native_capture(true).expect("rom just loaded");
    }
    // Explicit `--sym` overrides the `<rom>.sym` auto-detection done by
    // `load_rom_into` (issue #67).
    if let Some(sym_path) = sym {
        match em.load_symbols(sym_path) {
            Ok(n) => eprintln!("loaded {n} symbols from {}", sym_path.display()),
            Err(e) => {
                eprintln!("error: --sym {}: {e}", sym_path.display());
                return ExitCode::from(1);
            }
        }
    }
    // Controller port device selection (RFE-3): a `mouse` answers the
    // auto-read / serial path with the SNES Mouse protocol so the game's
    // DETECT succeeds (the signature) and `inputGetMouse` reads its deltas.
    for (port, dev) in [(0u8, port1), (1u8, port2)] {
        match dev {
            "pad" => {}
            "mouse" => {
                if let Err(e) = em.set_port_mouse(port, true) {
                    eprintln!("error: --port{}: {e}", port + 1);
                    return ExitCode::from(1);
                }
            }
            "superscope" => {
                if let Err(e) = em.set_port_device(port, luna_api::PortDevice::SuperScope) {
                    eprintln!("error: --port{}: {e}", port + 1);
                    return ExitCode::from(1);
                }
            }
            other => {
                eprintln!(
                    "error: --port{} `{other}`: expected `pad`, `mouse`, or `superscope`",
                    port + 1
                );
                return ExitCode::from(1);
            }
        }
    }
    let mouse_checkpoints = match mouse_script.map(parse_mouse_script) {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            eprintln!("error: --mouse: {e}");
            return ExitCode::from(1);
        }
        None => Vec::new(),
    };
    // `--superscope` shares the `frame:a,b,c` triplet grammar with `--mouse`
    // (here a,b = absolute aim x,y; c = the button mask).
    let scope_checkpoints = match superscope_script.map(parse_mouse_script) {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            eprintln!("error: --superscope: {e}");
            return ExitCode::from(1);
        }
        None => Vec::new(),
    };
    // --srm-in: seed battery SRAM from a `.srm` file (the read half of a
    // cross-run power-cycle test), before the warm-up runs.
    if let Some(path) = srm_in {
        match std::fs::read(path) {
            Ok(data) => {
                if let Err(e) = em.load_sram(&data) {
                    eprintln!("error: --srm-in: {e}");
                    return ExitCode::from(1);
                }
                eprintln!(
                    "loaded {} bytes of SRAM from {}",
                    data.len(),
                    path.display()
                );
            }
            Err(e) => {
                eprintln!("error: --srm-in {}: {e}", path.display());
                return ExitCode::from(1);
            }
        }
    }
    // Optionally resume from a GUI-captured save state (api-first:
    // `Emulator::load_state`). Done right after `load_rom` so the `-n`
    // warm-up runs forward from the captured scene.
    if let Some(sp) = load_state_path {
        match std::fs::read(sp) {
            Ok(bytes) => {
                if let Err(e) = em.load_state(&bytes) {
                    eprintln!("error: --load-state {}: {e}", sp.display());
                    return ExitCode::from(1);
                }
                eprintln!("loaded save state from {}", sp.display());
            }
            Err(e) => {
                eprintln!("error: --load-state {}: {e}", sp.display());
                return ExitCode::from(1);
            }
        }
    }
    if apu_log_path.is_some()
        && let Err(e) = em.enable_mailbox_log()
    {
        eprintln!("error: enable_mailbox_log: {e}");
        return ExitCode::from(1);
    }
    if sa1_log_path.is_some()
        && let Err(e) = em.enable_sa1_log()
    {
        eprintln!("error: enable_sa1_log: {e}");
        return ExitCode::from(1);
    }
    if sa1_side_log_path.is_some()
        && let Err(e) = em.enable_sa1_side_log()
    {
        eprintln!("error: enable_sa1_side_log: {e}");
        return ExitCode::from(1);
    }
    if sa1_trace_path.is_some()
        && let Err(e) = em.enable_sa1_trace(sa1_trace_max)
    {
        eprintln!("error: enable_sa1_trace: {e}");
        return ExitCode::from(1);
    }
    if superfx_trace_path.is_some()
        && let Err(e) = em.enable_superfx_trace(superfx_trace_max)
    {
        eprintln!("error: enable_superfx_trace: {e}");
        return ExitCode::from(1);
    }
    if spc_trace_path.is_some()
        && let Err(e) = em.enable_spc_trace(spc_trace_max)
    {
        eprintln!("error: enable_spc_trace: {e}");
        return ExitCode::from(1);
    }
    // Arm the WDM ($42) capture before stepping so the ROM's SNES_ASSERT /
    // breakpoint channel is recorded (issue #85 — parity with `luna run`).
    if wdm_out.is_some()
        && let Err(e) = em.enable_wdm_log()
    {
        eprintln!("error: enable_wdm_log: {e}");
        return ExitCode::from(1);
    }
    // Parse `frame:hex` checkpoints into a sorted vector. We apply
    // them by stepping `step_until_frame` between checkpoints — a
    // ~30k-instruction budget per frame is enough for any real ROM,
    // including SA-1 carts.
    let checkpoints: Vec<(u64, u16)> = match input_script {
        None => Vec::new(),
        Some(script) => match parse_input_script(script) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: --input: {e}");
                return ExitCode::from(1);
            }
        },
    };
    let start_instructions = em.instructions_executed();
    if !checkpoints.is_empty() || !mouse_checkpoints.is_empty() || !scope_checkpoints.is_empty() {
        // Merge gamepad (`--input`), mouse (`--mouse`) and super-scope
        // (`--superscope`) checkpoints into one frame-sorted event stream so
        // they all apply at the right moment.
        enum Ev {
            Pad(u16),
            Mouse(i32, i32, u8),
            Scope(i32, i32, u8),
        }
        let mut events: Vec<(u64, Ev)> = checkpoints
            .iter()
            .map(|&(f, m)| (f, Ev::Pad(m)))
            .chain(
                mouse_checkpoints
                    .iter()
                    .map(|&(f, (dx, dy, b))| (f, Ev::Mouse(dx, dy, b))),
            )
            .chain(
                scope_checkpoints
                    .iter()
                    .map(|&(f, (x, y, b))| (f, Ev::Scope(x, y, b))),
            )
            .collect();
        events.sort_by_key(|(f, _)| *f);
        // Chasing checkpoint frames spends from the SAME `-n` budget as
        // the run itself (issue #126): a checkpoint scheduled beyond the
        // requested window must never fire, and the total run must be
        // `-n` instructions — not `-n` on top of an unbounded pre-roll.
        for (frame, ev) in &events {
            let spent = em
                .instructions_executed()
                .saturating_sub(start_instructions);
            let Some(left) = steps.checked_sub(spent).filter(|l| *l > 0) else {
                break; // budget exhausted — later checkpoints never happen
            };
            crate::parsers::step_to_frame_bounded(&mut em, *frame, left);
            if em.state().scheduler.frame_count < *frame {
                break; // ran out before reaching this checkpoint's frame
            }
            let applied = match ev {
                Ev::Pad(m) => em.set_joypad(0, *m),
                Ev::Mouse(dx, dy, b) => em.set_mouse(*dx, *dy, *b),
                Ev::Scope(x, y, b) => em.set_superscope(*x, *y, *b),
            };
            if let Err(e) = applied {
                eprintln!("error: applying scripted input: {e}");
                return ExitCode::from(1);
            }
        }
    }
    // Trace gating: bridge to the earliest of the two "trace-from"
    // targets, enable whichever crossed; bridge to the later one if
    // it's still ahead, enable that. Each trace caps itself at its
    // own --*-trace-max events regardless of remaining steps.
    // Whatever the checkpoint pre-roll consumed comes OUT of `-n`, so
    // `-n` is the total run length with or without `--input`.
    let mut remaining = steps.saturating_sub(
        em.instructions_executed()
            .saturating_sub(start_instructions),
    );
    let mut bridge_target = u64::MAX;
    if cpu_trace_path.is_some() {
        bridge_target = bridge_target.min(cpu_trace_from);
    }
    if mem_trace_path.is_some() {
        bridge_target = bridge_target.min(mem_trace_from);
    }
    let parsed_mem_bank: Option<u8> = match mem_trace_bank {
        None => None,
        Some(s) => match parse_hex_u8(s) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("error: --mem-trace-bank `{s}`: {e}");
                return ExitCode::from(1);
            }
        },
    };
    let parsed_mem_addr: Option<(u16, u16)> = match mem_trace_addr {
        None => None,
        Some(spec) => match parse_addr_range(spec) {
            Ok(range) => Some(range),
            Err(e) => {
                eprintln!("error: --mem-trace-addr {e}");
                return ExitCode::from(1);
            }
        },
    };
    if bridge_target != u64::MAX {
        let current = em.instructions_executed();
        if bridge_target > current {
            let bridge = (bridge_target - current).min(remaining);
            if let Err(e) = em.step(bridge) {
                eprintln!("step warning (pre-trace bridge 1): {e}");
            }
            remaining = remaining.saturating_sub(bridge);
        }
    }
    // Enable whichever traces are at or past their target now.
    if cpu_trace_path.is_some()
        && em.instructions_executed() >= cpu_trace_from
        && let Err(e) = em.enable_cpu_trace(cpu_trace_max)
    {
        eprintln!("error: enable_cpu_trace: {e}");
        return ExitCode::from(1);
    }
    if mem_trace_path.is_some()
        && em.instructions_executed() >= mem_trace_from
        && let Err(e) = em.enable_mem_trace(mem_trace_max, parsed_mem_bank, parsed_mem_addr)
    {
        eprintln!("error: enable_mem_trace: {e}");
        return ExitCode::from(1);
    }
    // If the two targets differ, bridge to the second one and enable.
    if cpu_trace_path.is_some() && mem_trace_path.is_some() && cpu_trace_from != mem_trace_from {
        let second_target = cpu_trace_from.max(mem_trace_from);
        let current = em.instructions_executed();
        if second_target > current {
            let bridge = (second_target - current).min(remaining);
            if let Err(e) = em.step(bridge) {
                eprintln!("step warning (pre-trace bridge 2): {e}");
            }
            remaining = remaining.saturating_sub(bridge);
            // Re-check enables (whichever wasn't enabled yet).
            if cpu_trace_path.is_some() && em.instructions_executed() >= cpu_trace_from {
                let _ = em.enable_cpu_trace(cpu_trace_max);
            }
            if mem_trace_path.is_some() && em.instructions_executed() >= mem_trace_from {
                let _ = em.enable_mem_trace(mem_trace_max, parsed_mem_bank, parsed_mem_addr);
            }
        }
    }
    // DMA→VRAM trace: bridge to its start instruction independently of the
    // cpu/mem traces, then enable. Capturing from boot would drown the
    // window of interest, so `--dma-trace-from` skips the early uploads.
    if dma_trace_path.is_some() {
        let current = em.instructions_executed();
        if dma_trace_from > current {
            let bridge = (dma_trace_from - current).min(remaining);
            if let Err(e) = em.step(bridge) {
                eprintln!("step warning (pre-dma-trace bridge): {e}");
            }
            remaining = remaining.saturating_sub(bridge);
        }
        if let Err(e) = em.enable_dma_trace(dma_trace_max) {
            eprintln!("error: enable_dma_trace: {e}");
            return ExitCode::from(1);
        }
    }
    // When --audio-out is set, step in chunks and drain the APU's
    // ~16k-sample bounded queue after each chunk; otherwise we'd lose
    // ~99% of audio on any run longer than ~0.5 s of emulated time.
    // The accumulated Vec is written to disk after the run.
    let mut audio_accum: Vec<(i16, i16)> = Vec::new();
    if let Some(target_frame) = until_frame {
        // RFE-5: run to a specific PPU frame instead of the `-n` instruction
        // count, draining audio along the way so `--audio-out` still works.
        const FRAME_BUDGET: u64 = 200_000;
        while em.state().scheduler.frame_count < target_frame {
            if em.step_until_frame(FRAME_BUDGET).unwrap_or(0) == 0 {
                break;
            }
            if audio_out.is_some()
                && let Ok(mut chunk) = em.drain_audio(usize::MAX)
            {
                audio_accum.append(&mut chunk);
            }
        }
    } else if audio_out.is_some() {
        // Chunk = 100k instructions ≈ ~10 ms of emulated SPC time
        // (well under the 512 ms queue capacity even at peak DSP
        // output rate). Drain the full queue after each chunk.
        const AUDIO_CHUNK: u64 = 100_000;
        let mut left = remaining;
        while left > 0 {
            let take = left.min(AUDIO_CHUNK);
            if let Err(e) = em.step(take) {
                eprintln!("step warning: {e}");
                break;
            }
            left -= take;
            match em.drain_audio(usize::MAX) {
                Ok(mut chunk) => audio_accum.append(&mut chunk),
                Err(e) => eprintln!("warning: drain_audio mid-run: {e}"),
            }
        }
    } else {
        match em.step(remaining) {
            Ok(_) => {}
            Err(e) => {
                // Step errors are informational — we still want a state
                // snapshot.
                eprintln!("step warning: {e}");
            }
        }
    }

    for spec in peek_specs {
        // Numeric `BANK:OFFSET:COUNT` first; else a WLA-DX label
        // `NAME[:COUNT]` resolved through the loaded .sym table (#77).
        let target = match parse_peek_spec(spec) {
            Ok(t) => Ok(t),
            Err(num_err) => match parse_peek_spec_sym(spec) {
                Ok((name, count)) => match em.resolve_symbol(&name) {
                    Some(addr) => Ok(((addr >> 16) as u8, addr as u16, count)),
                    None => Err(format!(
                        "unknown symbol `{name}` (and not BANK:OFFSET:COUNT: {num_err})"
                    )),
                },
                Err(_) => Err(num_err),
            },
        };
        match target {
            Ok((bank, offset, count)) => match em.peek_memory(bank, offset, count) {
                Ok(bytes) => {
                    eprintln!("peek ${:02X}:{:04X} +{:04X}:", bank, offset, bytes.len());
                    print_hex_dump(bank, offset, &bytes);
                }
                Err(e) => eprintln!("error: peek_memory `{spec}`: {e}"),
            },
            Err(e) => eprintln!("error: --peek `{spec}`: {e}"),
        }
    }

    // --assert / --assert-aram / --assert-vram: native pass/fail (exit 0/1) so
    // headless consumers don't have to parse the peek dump.
    let mut assert_failed = false;
    for spec in assert_specs {
        // Numeric `BANK:OFFSET=HEX` first; else a WLA-DX label `NAME=HEX`
        // resolved through the loaded .sym table (#77) — this is what lets
        // OpenSNES's probe harness drop its own .sym parser.
        let target = match parse_assert_spec(spec) {
            Ok(t) => Ok(t),
            Err(num_err) => match parse_assert_spec_sym(spec) {
                Ok((name, want)) => match em.resolve_symbol(&name) {
                    Some(addr) => Ok(((addr >> 16) as u8, addr as u16, want)),
                    None => Err(format!(
                        "unknown symbol `{name}` (and not BANK:OFFSET=HEX: {num_err})"
                    )),
                },
                Err(_) => Err(num_err),
            },
        };
        match target {
            Ok((bank, offset, want)) => match em.peek_memory(bank, offset, want.len() as u16) {
                Ok(got) if got == want => {
                    println!("PASS ${bank:02X}:{offset:04X}={}", hex_str(&want));
                }
                Ok(got) => {
                    println!(
                        "FAIL ${bank:02X}:{offset:04X} expected {} got {}",
                        hex_str(&want),
                        hex_str(&got)
                    );
                    assert_failed = true;
                }
                Err(e) => {
                    eprintln!("error: --assert `{spec}`: {e}");
                    assert_failed = true;
                }
            },
            Err(e) => {
                eprintln!("error: --assert `{spec}`: {e}");
                assert_failed = true;
            }
        }
    }
    for (specs, label, kind) in [
        (assert_aram_specs, "aram", 0u8),
        (assert_vram_specs, "vram", 1u8),
    ] {
        for spec in specs {
            match parse_assert_spec_no_bank(spec) {
                Ok((offset, want)) => {
                    let res = if kind == 0 {
                        em.peek_aram(offset, want.len() as u16)
                    } else {
                        em.peek_vram(offset, want.len() as u16)
                    };
                    match res {
                        Ok(got) if got == want => {
                            println!("PASS {label}:{offset:04X}={}", hex_str(&want));
                        }
                        Ok(got) => {
                            println!(
                                "FAIL {label}:{offset:04X} expected {} got {}",
                                hex_str(&want),
                                hex_str(&got)
                            );
                            assert_failed = true;
                        }
                        Err(e) => {
                            eprintln!("error: --assert-{label} `{spec}`: {e}");
                            assert_failed = true;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: --assert-{label} `{spec}`: {e}");
                    assert_failed = true;
                }
            }
        }
    }
    // CGRAM is 256 × 15-bit colours (512 bytes), low byte of each colour first.
    for spec in assert_cgram_specs {
        match parse_assert_spec_no_bank(spec) {
            Ok((offset, want)) => match em.peek_cgram() {
                Ok(colors) => {
                    let bytes: Vec<u8> = colors
                        .iter()
                        .flat_map(|c| [*c as u8, (*c >> 8) as u8])
                        .collect();
                    let start = offset as usize;
                    let end = start + want.len();
                    if end > bytes.len() {
                        eprintln!(
                            "error: --assert-cgram `{spec}`: offset {offset:#X}+{} exceeds the 512-byte CGRAM",
                            want.len()
                        );
                        assert_failed = true;
                    } else if bytes[start..end] == want[..] {
                        println!("PASS cgram:{offset:04X}={}", hex_str(&want));
                    } else {
                        println!(
                            "FAIL cgram:{offset:04X} expected {} got {}",
                            hex_str(&want),
                            hex_str(&bytes[start..end])
                        );
                        assert_failed = true;
                    }
                }
                Err(e) => {
                    eprintln!("error: --assert-cgram `{spec}`: {e}");
                    assert_failed = true;
                }
            },
            Err(e) => {
                eprintln!("error: --assert-cgram `{spec}`: {e}");
                assert_failed = true;
            }
        }
    }

    if let Some(path) = apu_log_path {
        match em.take_mailbox_log() {
            Ok(events) => match write_mailbox_log_csv(path, &events) {
                Ok(()) => eprintln!(
                    "APU mailbox log written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing APU log: {e}"),
            },
            Err(e) => eprintln!("error: take_mailbox_log: {e}"),
        }
    }
    if let Some(path) = sa1_log_path {
        match em.take_sa1_log() {
            Ok(events) => match write_sa1_log_csv(path, &events) {
                Ok(()) => eprintln!(
                    "SA-1 MMIO log written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing SA-1 log: {e}"),
            },
            Err(e) => eprintln!("error: take_sa1_log: {e}"),
        }
    }
    if let Some(path) = sa1_side_log_path {
        match em.take_sa1_side_log() {
            Ok(events) => match write_sa1_side_log_csv(path, &events) {
                Ok(()) => eprintln!(
                    "SA-1-side log written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing SA-1-side log: {e}"),
            },
            Err(e) => eprintln!("error: take_sa1_side_log: {e}"),
        }
    }
    if let Some(path) = sa1_trace_path {
        match em.take_sa1_trace() {
            Ok(events) => match write_sa1_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "SA-1 instruction trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing SA-1 trace: {e}"),
            },
            Err(e) => eprintln!("error: take_sa1_trace: {e}"),
        }
    }
    if let Some(path) = superfx_trace_path {
        match em.take_superfx_trace() {
            Ok(events) => match write_superfx_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "Super FX instruction trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing Super FX trace: {e}"),
            },
            Err(e) => eprintln!("error: take_superfx_trace: {e}"),
        }
    }
    if let Some(path) = spc_trace_path {
        match em.take_spc_trace() {
            Ok(events) => match write_spc_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "SPC700 instruction trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing SPC700 trace: {e}"),
            },
            Err(e) => eprintln!("error: take_spc_trace: {e}"),
        }
    }
    if let Some(path) = dump_vram_path {
        match em.vram_bytes() {
            Ok(bytes) => match std::fs::write(path, &bytes) {
                Ok(()) => eprintln!("VRAM ({} bytes) written to {}", bytes.len(), path.display()),
                Err(e) => eprintln!("error: writing VRAM dump: {e}"),
            },
            Err(e) => eprintln!("error: vram_bytes: {e}"),
        }
    }
    if let Some(path) = dump_coproc_ram_path {
        match em.coproc_ram() {
            Ok(Some(bytes)) => match std::fs::write(path, &bytes) {
                Ok(()) => eprintln!(
                    "coproc work RAM ({} bytes) written to {}",
                    bytes.len(),
                    path.display()
                ),
                Err(e) => eprintln!("error: writing coproc RAM dump: {e}"),
            },
            Ok(None) => eprintln!("note: cart has no coprocessor work RAM"),
            Err(e) => eprintln!("error: coproc_ram: {e}"),
        }
    }
    if let Some(path) = dump_aram_path {
        match em.aram_bytes() {
            Ok(bytes) => match std::fs::write(path, &bytes) {
                Ok(()) => eprintln!("ARAM ({} bytes) written to {}", bytes.len(), path.display()),
                Err(e) => eprintln!("error: writing ARAM dump: {e}"),
            },
            Err(e) => eprintln!("error: aram_bytes: {e}"),
        }
    }
    if let Some(path) = cpu_trace_path {
        match em.take_cpu_trace_log() {
            Ok(events) => match write_cpu_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "CPU trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing CPU trace: {e}"),
            },
            Err(e) => eprintln!("error: take_cpu_trace_log: {e}"),
        }
    }
    if let Some(path) = mem_trace_path {
        match em.take_mem_trace_log() {
            Ok(events) => match write_mem_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "Memory trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing memory trace: {e}"),
            },
            Err(e) => eprintln!("error: take_mem_trace_log: {e}"),
        }
    }
    if let Some(path) = dma_trace_path {
        match em.take_dma_trace() {
            Ok(events) => match write_dma_trace_csv(path, &events) {
                Ok(()) => eprintln!(
                    "DMA→VRAM trace written to {} ({} events)",
                    path.display(),
                    events.len()
                ),
                Err(e) => eprintln!("error: writing DMA trace: {e}"),
            },
            Err(e) => eprintln!("error: take_dma_trace: {e}"),
        }
    }

    let state = em.state();
    let json = match serde_json::to_string_pretty(&state) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: serialising state: {e}");
            return ExitCode::from(1);
        }
    };
    if out.as_os_str() == "-" {
        println!("{json}");
    } else if let Err(e) = std::fs::write(out, &json) {
        eprintln!("error: writing state JSON: {e}");
        return ExitCode::from(1);
    } else {
        eprintln!("State JSON written to {}", out.display());
    }

    if let Some(p) = screenshot {
        let png_result = if native_res {
            em.render_frame_png_native()
        } else {
            em.render_frame_png(false)
        };
        match png_result {
            Ok(png) => {
                if let Err(e) = std::fs::write(p, &png) {
                    eprintln!("error: writing screenshot: {e}");
                    return ExitCode::from(1);
                }
                eprintln!("Screenshot written to {}", p.display());
            }
            Err(e) => {
                eprintln!("error: render_frame_png: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Some(p) = audio_out {
        // Drain anything still queued at end-of-run, then write the
        // full accumulated stream.
        match em.drain_audio(usize::MAX) {
            Ok(mut tail) => audio_accum.append(&mut tail),
            Err(e) => eprintln!("warning: final drain_audio: {e}"),
        }
        if let Err(e) = write_wav(p, &audio_accum) {
            eprintln!("error: writing WAV: {e}");
            return ExitCode::from(1);
        }
        let secs = audio_accum.len() as f64 / 32_000.0;
        eprintln!(
            "Audio WAV written to {}  ({} samples = {:.2}s @ 32 kHz stereo)",
            p.display(),
            audio_accum.len(),
            secs
        );
    }
    // --srm-out: persist battery SRAM to a `.srm` file (the write half of a
    // cross-run power-cycle test).
    if let Some(path) = srm_out {
        let data = em.sram();
        match std::fs::write(path, &data) {
            Ok(()) => eprintln!("wrote {} bytes of SRAM to {}", data.len(), path.display()),
            Err(e) => eprintln!("error: --srm-out {}: {e}", path.display()),
        }
    }
    // WDM ($42) capture → file, and the cross-arch fbhash key (issue #85 —
    // parity with `luna run`, so an input-driven test can also emit a visual
    // baseline). Printed last so a harness can `grep '^fbhash='`.
    if let Some(path) = wdm_out {
        use std::fmt::Write as _;
        let hits = em.take_wdm_log().unwrap_or_default();
        let mut body = String::new();
        for (pc, op) in &hits {
            let _ = writeln!(body, "PC=${pc:06X} operand=${op:02X}");
        }
        match std::fs::write(path, &body) {
            Ok(()) => eprintln!(
                "WDM log written to {} ({} hit(s))",
                path.display(),
                hits.len()
            ),
            Err(e) => {
                eprintln!("error: could not write WDM log: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if print_fbhash {
        let hash = if native_res {
            em.frame_hash_native()
        } else {
            em.frame_hash(false)
        };
        match hash {
            Ok(h) => println!("fbhash={h:016x}"),
            Err(e) => {
                eprintln!("error: could not hash frame: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if assert_failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
