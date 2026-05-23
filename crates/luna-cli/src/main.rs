//! Luna SNES emulator — command-line entry point.
//!
//! Dispatches between execution modes (run / mcp / replay).
//! See `ARCHITECTURE.md` §3.2.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use luna_cartridge::Cartridge;
use luna_core::Snes;

#[derive(Parser, Debug)]
#[command(
    name = "luna",
    version,
    about = "SNES emulator with introspection API",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Load a ROM, step the CPU N instructions, optionally dump a
    /// screenshot of the resulting PPU state.
    ///
    /// Phase 1.7: no APU yet, so real ROMs that handshake with the
    /// SPC700 will eventually hang. We still render whatever the PPU
    /// produced via direct CPU writes / DMA. unimplemented opcodes
    /// panic and are caught — partial state is still dumped.
    Run {
        /// Path to the .sfc / .smc ROM file.
        rom: PathBuf,
        /// Maximum number of CPU instructions to execute before dumping.
        #[arg(short = 'n', long, default_value_t = 64)]
        steps: u64,
        /// If set, render a 256×224 PNG of the framebuffer (BG1 Mode 0
        /// only at this point) and write it to the given path.
        #[arg(long)]
        screenshot: Option<PathBuf>,
    },
    /// MCP server stub (real implementation lands in Phase 3).
    Mcp,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            rom,
            steps,
            screenshot,
        } => run(&rom, steps, screenshot.as_deref()),
        Command::Mcp => {
            eprintln!("MCP server not implemented yet — see ARCHITECTURE.md §14 (Phase 3).");
            ExitCode::from(2)
        }
    }
}

fn run(rom_path: &std::path::Path, steps: u64, screenshot: Option<&std::path::Path>) -> ExitCode {
    let cart = match Cartridge::load(rom_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    print_header(&cart);

    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    println!("After reset: PC=${:02X}:{:04X}", snes.cpu.pb, snes.cpu.pc);
    println!();

    // Silence the default panic printer so we own the panic message
    // output (catch_unwind doesn't suppress the hook by itself).
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut executed: u64 = 0;
    let mut panic_msg: Option<String> = None;
    while executed < steps {
        if snes.cpu.stopped {
            println!("CPU halted by STP after {executed} instructions.");
            break;
        }
        match catch_unwind(AssertUnwindSafe(|| snes.step())) {
            Ok(_) => executed += 1,
            Err(payload) => {
                panic_msg = Some(payload_to_string(payload));
                break;
            }
        }
    }

    std::panic::set_hook(prev_hook);

    println!("--- final state ---");
    print_cpu_state(&snes);
    println!("Instructions executed: {executed}");
    println!("Total master cycles:   {}", snes.total_mclk);
    if let Some(msg) = panic_msg {
        println!();
        println!("Stopped on CPU panic:");
        println!("  {msg}");
        // Returning success here: hitting an unimplemented opcode is the
        // expected state of P0.6, not a CLI failure.
    }

    // Screenshot dump: render whatever the PPU has accumulated.
    if let Some(out_path) = screenshot {
        match save_screenshot(&snes, out_path) {
            Ok(()) => println!("\nScreenshot written to {}", out_path.display()),
            Err(e) => {
                eprintln!("\nerror: could not write screenshot: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}

fn print_header(cart: &Cartridge) {
    let h = &cart.header;
    println!("=== ROM ===");
    println!("Title:       {:?}", h.title);
    println!(
        "Mapper:      {:?}{}",
        h.mapper_kind,
        if h.fast_rom { " (FastROM)" } else { "" }
    );
    println!(
        "ROM size:    {} KB ({} bytes on disk)",
        h.rom_size_kb,
        cart.rom.len()
    );
    println!("SRAM size:   {} KB", h.sram_size_kb);
    println!("Region:      {:?}", h.region);
    println!("Version:     v{}", h.version);
    println!(
        "Checksum:    ${:04X} / complement ${:04X} (valid: {})",
        h.checksum,
        h.checksum_complement,
        h.checksum_valid()
    );
}

fn print_cpu_state(snes: &Snes) {
    let cpu = &snes.cpu;
    println!(
        "A=${:04X}  X=${:04X}  Y=${:04X}  SP=${:04X}  DP=${:04X}",
        cpu.a, cpu.x, cpu.y, cpu.sp, cpu.dp
    );
    println!(
        "PC=${:02X}:{:04X}  DB=${:02X}  P=${:02X}  E={}",
        cpu.pb,
        cpu.pc,
        cpu.db,
        cpu.p.bits(),
        u8::from(cpu.e)
    );
    println!("flags: {}", flag_string(cpu.p.bits(), cpu.e));
}

fn flag_string(p: u8, e: bool) -> String {
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

fn save_screenshot(snes: &Snes, path: &std::path::Path) -> Result<(), image::ImageError> {
    let frame = luna_ppu::render_frame_bg1(&snes.ppu);
    let mut buf = Vec::with_capacity(luna_ppu::FRAME_W * luna_ppu::FRAME_H * 3);
    for px in frame {
        buf.extend_from_slice(&px);
    }
    let img = image::RgbImage::from_raw(luna_ppu::FRAME_W as u32, luna_ppu::FRAME_H as u32, buf)
        .expect("frame buffer size matches dims");
    img.save(path)
}

fn payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(unknown panic payload)".to_string()
    }
}
