//! `luna run` — plain headless execution with input scripting.

use std::process::ExitCode;

use crate::output::{print_cpu_state, print_diag_state, print_header, save_screenshot, write_wav};
use crate::rom::load_rom_into;

pub(crate) fn run(
    rom_path: &std::path::Path,
    steps: u64,
    screenshot: Option<&std::path::Path>,
    force_display: bool,
    bg: Option<u8>,
    audio_out: Option<&std::path::Path>,
    nocash_out: Option<&std::path::Path>,
    wdm_out: Option<&std::path::Path>,
    print_fbhash: bool,
    native_res: bool,
    force_mapper: Option<&str>,
    force_region: Option<&str>,
) -> ExitCode {
    let mut em = luna_api::Emulator::new();
    // Shared loader (issue #95): honours `--force-mapper` so a bad-checksum
    // reference ROM (PeterLemon corpus) can reach `--print-fbhash` too.
    if let Err(e) = load_rom_into(&mut em, rom_path, force_mapper, force_region, None) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    if native_res {
        em.set_native_capture(true).expect("rom just loaded");
    }

    if let Some(info) = em.rom_info() {
        print_header(info);
    }

    // Arm the Nocash ($21FC) capture before stepping so the program's
    // `SNES_NOCASH`/`SNES_ASSERT` output is recorded during the run.
    if nocash_out.is_some() {
        if let Err(e) = em.enable_nocash_log() {
            eprintln!("error: enable_nocash_log: {e}");
            return ExitCode::from(1);
        }
    }
    if wdm_out.is_some() {
        if let Err(e) = em.enable_wdm_log() {
            eprintln!("error: enable_wdm_log: {e}");
            return ExitCode::from(1);
        }
    }

    // `load_rom` already runs the reset vector; report where it landed.
    {
        let cpu = em.state().cpu;
        println!("After reset: PC=${:02X}:{:04X}", cpu.pb, cpu.pc);
    }
    println!();

    let mut panic_msg: Option<String> = None;
    let mut stopped = false;
    // If we're recording audio, drain the APU queue every batch of
    // steps so it doesn't overflow (capped at 16 384 samples).
    let mut audio_samples: Vec<(i16, i16)> = if audio_out.is_some() {
        Vec::with_capacity(1 << 20)
    } else {
        Vec::new()
    };
    if audio_out.is_some() {
        // Step in 4 096-instruction batches, draining the APU's bounded
        // queue after each so it never saturates over a long run. A
        // short batch (`ran < want`) means the CPU hit STP.
        const BATCH: u64 = 4_096;
        loop {
            let done = em.instructions_executed();
            if done >= steps {
                break;
            }
            let want = (steps - done).min(BATCH);
            match em.step(want) {
                Ok(ran) => {
                    match em.drain_audio(8_192) {
                        Ok(mut chunk) => audio_samples.append(&mut chunk),
                        Err(e) => eprintln!("warning: drain_audio mid-run: {e}"),
                    }
                    if ran < want {
                        stopped = true;
                        break;
                    }
                }
                Err(luna_api::ApiError::Panic(msg)) => {
                    panic_msg = Some(msg);
                    break;
                }
                Err(e) => {
                    eprintln!("error: step: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        // Final drain.
        match em.drain_audio(usize::MAX) {
            Ok(mut chunk) => audio_samples.append(&mut chunk),
            Err(e) => eprintln!("warning: drain_audio final: {e}"),
        }
    } else {
        match em.step(steps) {
            Ok(ran) => stopped = ran < steps,
            Err(luna_api::ApiError::Panic(msg)) => panic_msg = Some(msg),
            Err(e) => {
                eprintln!("error: step: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if stopped {
        println!(
            "CPU halted by STP after {} instructions.",
            em.instructions_executed()
        );
    }

    println!("--- final state ---");
    print_cpu_state(&em.state().cpu);
    print_diag_state(&mut em);
    println!("Instructions executed: {}", em.instructions_executed());
    println!("Total master cycles:   {}", em.state().stats.total_mclk);
    if let Some(msg) = panic_msg {
        println!();
        println!("Stopped on CPU panic:");
        println!("  {msg}");
        // Returning success here: hitting an unimplemented opcode is the
        // expected state of P0.6, not a CLI failure.
    }

    // Screenshot dump: render whatever the PPU has accumulated.
    if let Some(out_path) = screenshot {
        match save_screenshot(&em, out_path, force_display, bg) {
            Ok(()) => println!("\nScreenshot written to {}", out_path.display()),
            Err(e) => {
                eprintln!("\nerror: could not write screenshot: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Some(out_path) = audio_out {
        match write_wav(out_path, &audio_samples) {
            Ok(()) => println!(
                "Audio WAV written to {}  ({} samples @ 32 kHz stereo, {} s)",
                out_path.display(),
                audio_samples.len(),
                audio_samples.len() as f64 / 32_000.0,
            ),
            Err(e) => {
                eprintln!("\nerror: could not write audio WAV: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Some(path) = nocash_out {
        let bytes = em.take_nocash_log().unwrap_or_default();
        match std::fs::write(path, &bytes) {
            Ok(()) => println!(
                "Nocash ($21FC) log written to {}  ({} bytes)",
                path.display(),
                bytes.len(),
            ),
            Err(e) => {
                eprintln!("\nerror: could not write Nocash log: {e}");
                return ExitCode::from(1);
            }
        }
    }
    if let Some(path) = wdm_out {
        use std::fmt::Write as _;
        let hits = em.take_wdm_log().unwrap_or_default();
        let mut body = String::new();
        for (pc, op) in &hits {
            let _ = writeln!(body, "PC=${pc:06X} operand=${op:02X}");
        }
        match std::fs::write(path, &body) {
            Ok(()) => println!(
                "WDM log written to {}  ({} hit(s))",
                path.display(),
                hits.len(),
            ),
            Err(e) => {
                eprintln!("\nerror: could not write WDM log: {e}");
                return ExitCode::from(1);
            }
        }
    }
    // Stable, cross-arch visual-regression key (hashes the displayed RGBA,
    // pre-PNG). Printed last so a harness can `grep '^fbhash='`.
    if print_fbhash {
        let hash = if native_res {
            em.frame_hash_native()
        } else {
            em.frame_hash(force_display)
        };
        match hash {
            Ok(h) => println!("fbhash={h:016x}"),
            Err(e) => {
                eprintln!("\nerror: could not hash frame: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}
