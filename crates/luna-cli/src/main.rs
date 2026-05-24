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
        /// Bypass INIDISP forced-blank when rendering. Lets you see
        /// whatever the game has uploaded to VRAM/CGRAM even if its
        /// init left the screen blanked (typical when waiting on the
        /// SPC700 we don't fully emulate yet).
        #[arg(long)]
        force_display: bool,
        /// If set (1..=4), render ONLY that BG layer. Default is the
        /// composited BG3-over-BG1-over-BG2 frame (right for most
        /// Mode-1 title screens).
        #[arg(long)]
        bg: Option<u8>,
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
            force_display,
            bg,
        } => run(&rom, steps, screenshot.as_deref(), force_display, bg),
        Command::Mcp => {
            eprintln!("MCP server not implemented yet — see ARCHITECTURE.md §14 (Phase 3).");
            ExitCode::from(2)
        }
    }
}

fn run(
    rom_path: &std::path::Path,
    steps: u64,
    screenshot: Option<&std::path::Path>,
    force_display: bool,
    bg: Option<u8>,
) -> ExitCode {
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
    print_diag_state(&mut snes);
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
        match save_screenshot(&snes, out_path, force_display, bg) {
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

fn print_diag_state(snes: &mut Snes) {
    let p = &snes.ppu;
    println!(
        "PPU:  INIDISP=${:02X} (blanked={}, brightness={})  BGMODE=${:02X}  VRAM_addr=${:04X}",
        p.inidisp,
        p.inidisp & 0x80 != 0,
        p.inidisp & 0x0F,
        p.bgmode,
        p.vram.address
    );
    println!(
        "PPU:  INIDISP_writes={}  Backdrop=${:04X}",
        p.inidisp_write_count,
        p.cgram.color(0)
    );
    for (i, bg) in p.bg.iter().enumerate() {
        let base = (bg.tilemap_addr_words as usize) << 1;
        let mut nonzero = 0usize;
        for off in 0..(32 * 32 * 2) {
            let a = (base + off) & 0xFFFF;
            if p.vram.peek(a as u16) != 0 {
                nonzero += 1;
            }
        }
        println!(
            "BG{}:  tile=${:04X} (byte ${:04X})  char=${:04X} (byte ${:04X})  hscroll={} vscroll={}  tilemap_nonzero={}/{}",
            i + 1,
            bg.tilemap_addr_words,
            base,
            bg.char_addr_words,
            (bg.char_addr_words as usize) << 1,
            bg.h_scroll,
            bg.v_scroll,
            nonzero,
            32 * 32 * 2,
        );
    }
    println!(
        "CPU regs:  NMITIMEN=${:02X}  HVBJOY=${:02X}  fake_frames={}  NMIs_served={}",
        snes.cpu_regs.nmitimen, snes.cpu_regs.hvbjoy, snes.fake_frame_count, snes.nmis_serviced
    );
    let ports = snes.apu.ports();
    println!(
        "APU stub:  phase={:?}  $2140=${:02X} $2141=${:02X} $2142=${:02X} $2143=${:02X}",
        snes.apu.phase(),
        ports[0],
        ports[1],
        ports[2],
        ports[3]
    );
    // OAM occupancy + first few sprite entries (uses immutable `p`).
    let mut oam_non_zero = 0usize;
    for off in 0..0x220u16 {
        if p.oam.peek(off) != 0 {
            oam_non_zero += 1;
        }
    }
    println!(
        "OAM:   {oam_non_zero}/544 non-zero  |  OBSEL=${:02X}",
        p.obsel
    );
    let all_sprites = luna_ppu::decode_all_sprites(p);
    let visible_count = all_sprites.iter().filter(|sp| sp.y < 224).count();
    println!("  visible sprites (y<224): {visible_count}");
    let mut shown = 0;
    for (i, sp) in all_sprites.iter().enumerate() {
        if sp.y >= 224 {
            continue;
        }
        if shown >= 12 {
            break;
        }
        shown += 1;
        println!(
            "  sprite #{i:>3}: x={:>4} y={:>3} tile=${:03X} pal={} pri={} {}x{} {}{}",
            sp.x,
            sp.y,
            sp.tile,
            sp.palette,
            sp.priority,
            sp.w,
            sp.h,
            if sp.h_flip { "H" } else { "-" },
            if sp.v_flip { "V" } else { "-" },
        );
    }

    // VRAM / CGRAM occupancy digest: how many non-zero bytes in each.
    // Lets us tell "the game has uploaded graphics" from "VRAM is
    // empty" — important for diagnosing why the screen stays black.
    let mut vram_non_zero = 0usize;
    for off in 0..0x10000u32 {
        if snes.ppu.vram.peek(off as u16) != 0 {
            vram_non_zero += 1;
        }
    }
    let mut cgram_non_zero = 0usize;
    for idx in 0..256u16 {
        if snes.ppu.cgram.color(idx as u8) != 0 {
            cgram_non_zero += 1;
        }
    }
    println!(
        "VRAM:  {vram_non_zero}/65536 non-zero bytes  |  CGRAM: {cgram_non_zero}/256 non-zero colours"
    );

    // @PC bytes need mutable bus access — run after all immutable
    // PPU diagnostics are done.
    let pc_bytes = snes.peek_pc_bytes(8);
    print!("@PC bytes:");
    for b in &pc_bytes {
        print!(" {b:02X}");
    }
    println!();
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

fn save_screenshot(
    snes: &Snes,
    path: &std::path::Path,
    force_display: bool,
    bg: Option<u8>,
) -> Result<(), image::ImageError> {
    let opts = luna_ppu::RenderOptions {
        bypass_forced_blank: force_display,
    };
    let frame = match bg {
        Some(n) => {
            let idx = (n.saturating_sub(1).min(3)) as usize;
            luna_ppu::render_frame_bg_with(&snes.ppu, idx, opts)
        }
        None => luna_ppu::render_frame_with(&snes.ppu, opts),
    };
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
