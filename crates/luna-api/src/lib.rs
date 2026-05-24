//! Luna's public, transport-agnostic API.
//!
//! Every feature the emulator exposes — load a ROM, step the CPU,
//! peek memory, render a frame, drain audio — lives here as a method
//! on [`Emulator`]. The CLI, GUI, MCP server and any future
//! transport (HTTP, WebSocket, FFI) all call through this surface so
//! the emulator stays drivable from outside the binary that built it.
//!
//! ## Stability
//!
//! From V1 onward this crate carries strict SemVer guarantees: new
//! methods are additive, breaking changes bump the major version.
//! Today (P3.3) we're still pre-V1 and the surface is allowed to
//! churn freely.

use std::path::Path;

use luna_cartridge::{CartError, Cartridge};
use luna_core::Snes;
use luna_ppu::FRAME_H;
use luna_ppu::FRAME_W;
use serde::Serialize;
use thiserror::Error;

/// Errors surfaced from [`Emulator`] methods.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Caller asked for a ROM-dependent operation before
    /// [`Emulator::load_rom`] succeeded.
    #[error("no ROM loaded — call load_rom first")]
    NoRom,
    /// Cartridge parsing or layout detection failed.
    #[error("cartridge: {0}")]
    Cart(#[from] CartError),
    /// `from_cartridge` / `step` / etc. panicked inside the core.
    #[error("emulator panicked: {0}")]
    Panic(String),
    /// PNG encoding failed during `render_frame`.
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
    /// Generic I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Cartridge metadata returned by [`Emulator::load_rom`].
#[derive(Debug, Clone, Serialize)]
pub struct RomInfo {
    /// Title field from the internal SNES header.
    pub title: String,
    /// Detected mapper kind, e.g. `"LoRom"`, `"HiRom"`, `"ExHiRom"`.
    pub mapper: String,
    /// Cartridge ROM size in bytes.
    pub rom_bytes: usize,
    /// Battery-backed SRAM size in kilobytes.
    pub sram_kb: u32,
    /// `Ntsc`, `Pal`, or `Unknown`.
    pub region: String,
    /// FastROM (`MEMSEL`) eligibility from the header.
    pub fast_rom: bool,
}

/// Snapshot of the emulator's observable state. Every field maps to
/// something the GUI / debugger / tests want to inspect.
#[derive(Debug, Clone, Serialize)]
pub struct EmulatorState {
    /// `null` until a ROM is loaded.
    pub rom: Option<RomInfo>,
    /// CPU 65C816 registers.
    pub cpu: CpuState,
    /// PPU registers + occupancy.
    pub ppu: PpuState,
    /// CPU-system registers ($4200-$421F).
    pub cpu_regs: CpuRegsState,
    /// Scanline scheduler.
    pub scheduler: SchedulerState,
    /// APU / SPC700 / DSP.
    pub apu: ApuState,
    /// Cumulative metrics.
    pub stats: Stats,
}

/// 65C816 register snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct CpuState {
    /// Accumulator (16-bit, low byte active in 8-bit M mode).
    pub a: u16,
    /// X index register.
    pub x: u16,
    /// Y index register.
    pub y: u16,
    /// Stack pointer.
    pub sp: u16,
    /// Program counter (within the program bank).
    pub pc: u16,
    /// Program bank.
    pub pb: u8,
    /// Data bank.
    pub db: u8,
    /// Direct page register.
    pub dp: u16,
    /// Processor status flags (N V M X D I Z C).
    pub p: u8,
    /// Emulation mode flag.
    pub e: bool,
    /// `true` after STP.
    pub stopped: bool,
    /// `true` after WAI, until an interrupt arrives.
    pub waiting: bool,
}

/// PPU register snapshot + memory occupancy stats.
#[derive(Debug, Clone, Serialize)]
pub struct PpuState {
    /// `$2100` — bit 7 forced-blank, bits 0-3 brightness.
    pub inidisp: u8,
    /// `$2105` — bits 0-2 BG mode, bit 3 BG3 priority, bits 4-7 tile sizes.
    pub bgmode: u8,
    /// Current VRAM word address.
    pub vram_addr_words: u16,
    /// Count of writes to INIDISP since reset.
    pub inidisp_write_count: u64,
    /// CGRAM colour 0 (backdrop).
    pub backdrop: u16,
    /// `$2101` — sprite size + base.
    pub obsel: u8,
    /// How many of VRAM's 65 536 bytes are non-zero.
    pub vram_non_zero: usize,
    /// How many of CGRAM's 256 colours are non-zero.
    pub cgram_non_zero: usize,
    /// How many of OAM's 544 bytes are non-zero.
    pub oam_non_zero: usize,
}

/// CPU-system register snapshot (`$4200-$421F`).
#[derive(Debug, Clone, Serialize)]
pub struct CpuRegsState {
    /// `$4200`.
    pub nmitimen: u8,
    /// `$4212`.
    pub hvbjoy: u8,
    /// Latched NMI line (read of `$4210` clears it).
    pub nmi_flag: bool,
    /// Latched IRQ line.
    pub irq_flag: bool,
}

/// Scanline scheduler snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct SchedulerState {
    /// 0..=261 on NTSC; 225..=261 is VBlank.
    pub ppu_line: u16,
    /// Master cycles consumed within the current scanline.
    pub mcycles_in_line: u32,
    /// Frames completed since reset.
    pub frame_count: u64,
    /// Number of NMIs delivered to the CPU.
    pub nmis_serviced: u64,
}

/// APU / SPC700 / DSP snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct ApuState {
    /// SPC700 program counter.
    pub spc_pc: u16,
    /// `true` if the SPC700 has stopped (STP or unimplemented op).
    pub spc_stopped: bool,
    /// `true` once the SPC has left the IPL ROM area at `$FFC0`.
    pub past_iplrom: bool,
    /// `Some((opcode, pc))` if the SPC stopped on an unimplemented op.
    pub unimplemented_opcode: Option<UnimplementedOp>,
    /// `$2140-$2143` bytes the SPC has placed on the CPU side.
    pub to_cpu_ports: [u8; 4],
    /// `$F4-$F7` bytes the CPU has placed on the SPC side.
    pub to_spc_ports: [u8; 4],
    /// Master volume left (signed 7-bit).
    pub mvol_l: i8,
    /// Master volume right.
    pub mvol_r: i8,
    /// KON register (`$4C`) most-recent write.
    pub kon: u8,
    /// ENDX register (`$7C`).
    pub endx: u8,
    /// Number of DSP voices currently playing.
    pub active_voices: u8,
    /// How many stereo samples are buffered in `audio_queue`.
    pub audio_queue_len: usize,
    /// The most recent mixed stereo sample.
    pub last_audio_sample: (i16, i16),
}

/// Diagnostic info for an SPC700 opcode that the emulator hasn't
/// implemented yet.
#[derive(Debug, Clone, Serialize)]
pub struct UnimplementedOp {
    /// Offending opcode byte.
    pub opcode: u8,
    /// Address (in SPC700 memory) where the opcode lives.
    pub pc: u16,
}

/// Cumulative metrics since reset.
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    /// Total CPU instructions executed.
    pub instructions_executed: u64,
    /// Total master cycles consumed.
    pub total_mclk: u64,
}

/// The public emulator handle. Owns at most one cartridge + Snes
/// machine at a time.
pub struct Emulator {
    snes: Option<Snes>,
    rom_info: Option<RomInfo>,
    instructions_executed: u64,
}

impl Default for Emulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Emulator {
    /// Build a fresh emulator with no ROM loaded yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            snes: None,
            rom_info: None,
            instructions_executed: 0,
        }
    }

    /// Whether a ROM is currently loaded.
    #[must_use]
    pub fn has_rom(&self) -> bool {
        self.snes.is_some()
    }

    /// Load a ROM file. On success, the emulator is ready for
    /// stepping. Returns the parsed cartridge metadata for callers
    /// that want to surface it (window title, MCP `load_rom` tool).
    pub fn load_rom(&mut self, path: &Path) -> Result<RomInfo, ApiError> {
        let cart = Cartridge::load(path)?;
        self.load_cartridge(cart)
    }

    /// Lower-level entry point used by tests: load a ROM blob
    /// already parsed into bytes (skips filesystem I/O).
    pub fn load_rom_bytes(&mut self, bytes: Vec<u8>) -> Result<RomInfo, ApiError> {
        let cart = Cartridge::from_bytes(bytes)?;
        self.load_cartridge(cart)
    }

    fn load_cartridge(&mut self, cart: Cartridge) -> Result<RomInfo, ApiError> {
        let info = RomInfo {
            title: cart.header.title.clone(),
            mapper: format!("{:?}", cart.header.mapper_kind),
            rom_bytes: cart.rom.len(),
            sram_kb: cart.header.sram_size_kb,
            region: format!("{:?}", cart.header.region),
            fast_rom: cart.header.fast_rom,
        };
        // `Snes::from_cartridge` panics on coprocessor cart types;
        // surface that as a clean error rather than tearing down the
        // whole transport.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut snes = Snes::from_cartridge(cart);
            snes.reset();
            snes
        }));
        match result {
            Ok(snes) => {
                self.snes = Some(snes);
                self.rom_info = Some(info.clone());
                self.instructions_executed = 0;
                Ok(info)
            }
            Err(payload) => Err(ApiError::Panic(panic_message(payload))),
        }
    }

    /// Reset the loaded emulator to its power-on state. Equivalent
    /// to running the SNES reset vector. Errors if no ROM is loaded.
    pub fn reset(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.reset();
        self.instructions_executed = 0;
        Ok(())
    }

    /// Step the CPU `count` instructions (or stop early if the CPU
    /// halts or panics). Returns the number actually executed.
    pub fn step(&mut self, count: u64) -> Result<u64, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let mut executed = 0u64;
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while executed < count {
                if snes.cpu.stopped {
                    break;
                }
                snes.step();
                executed += 1;
            }
            executed
        }));
        std::panic::set_hook(prev_hook);
        match result {
            Ok(n) => {
                self.instructions_executed += n;
                Ok(n)
            }
            Err(payload) => {
                self.instructions_executed += executed;
                Err(ApiError::Panic(panic_message(payload)))
            }
        }
    }

    /// Run instructions until the PPU completes one frame (i.e.
    /// `frame_count` advances). Returns the number of instructions
    /// executed. Bounded by `max_steps` as a safety belt.
    pub fn step_until_frame(&mut self, max_steps: u64) -> Result<u64, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let start_frame = snes.frame_count;
        let mut executed = 0u64;
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while executed < max_steps && !snes.cpu.stopped && snes.frame_count == start_frame {
                snes.step();
                executed += 1;
            }
            executed
        }));
        std::panic::set_hook(prev_hook);
        match result {
            Ok(n) => {
                self.instructions_executed += n;
                Ok(n)
            }
            Err(payload) => {
                self.instructions_executed += executed;
                Err(ApiError::Panic(panic_message(payload)))
            }
        }
    }

    /// Take a JSON-serialisable snapshot of the entire observable
    /// emulator state.
    pub fn state(&mut self) -> EmulatorState {
        let cpu = self
            .snes
            .as_ref()
            .map_or_else(default_cpu_state, |s| CpuState {
                a: s.cpu.a,
                x: s.cpu.x,
                y: s.cpu.y,
                sp: s.cpu.sp,
                pc: s.cpu.pc,
                pb: s.cpu.pb,
                db: s.cpu.db,
                dp: s.cpu.dp,
                p: s.cpu.p.bits(),
                e: s.cpu.e,
                stopped: s.cpu.stopped,
                waiting: s.cpu.waiting,
            });
        let ppu = self.snes.as_ref().map_or_else(default_ppu_state, |s| {
            let mut vram_nz = 0;
            for off in 0..0x10000u32 {
                if s.ppu.vram.peek(off as u16) != 0 {
                    vram_nz += 1;
                }
            }
            let mut cgram_nz = 0;
            for idx in 0..256u16 {
                if s.ppu.cgram.color(idx as u8) != 0 {
                    cgram_nz += 1;
                }
            }
            let mut oam_nz = 0;
            for off in 0..0x220u16 {
                if s.ppu.oam.peek(off) != 0 {
                    oam_nz += 1;
                }
            }
            PpuState {
                inidisp: s.ppu.inidisp,
                bgmode: s.ppu.bgmode,
                vram_addr_words: s.ppu.vram.address,
                inidisp_write_count: s.ppu.inidisp_write_count,
                backdrop: s.ppu.cgram.color(0),
                obsel: s.ppu.obsel,
                vram_non_zero: vram_nz,
                cgram_non_zero: cgram_nz,
                oam_non_zero: oam_nz,
            }
        });
        let cpu_regs = self
            .snes
            .as_ref()
            .map_or_else(default_cpu_regs_state, |s| CpuRegsState {
                nmitimen: s.cpu_regs.nmitimen,
                hvbjoy: s.cpu_regs.hvbjoy,
                nmi_flag: s.cpu_regs.nmi_flag,
                irq_flag: s.cpu_regs.irq_flag,
            });
        let scheduler = self
            .snes
            .as_ref()
            .map_or_else(default_scheduler_state, |s| SchedulerState {
                ppu_line: s.ppu_line,
                mcycles_in_line: s.mcycles_in_line,
                frame_count: s.frame_count,
                nmis_serviced: s.nmis_serviced,
            });
        let apu = self
            .snes
            .as_ref()
            .map_or_else(default_apu_state, |s| ApuState {
                spc_pc: s.apu_real.cpu.pc,
                spc_stopped: s.apu_real.cpu.stopped,
                past_iplrom: s.apu_real.past_iplrom,
                unimplemented_opcode: s
                    .apu_real
                    .cpu
                    .unimplemented_opcode
                    .map(|(o, p)| UnimplementedOp { opcode: o, pc: p }),
                to_cpu_ports: s.apu_real.to_cpu_ports,
                to_spc_ports: s.apu_real.to_spc_ports,
                mvol_l: s.apu_real.dsp_regs[0x0C] as i8,
                mvol_r: s.apu_real.dsp_regs[0x1C] as i8,
                kon: s.apu_real.dsp_regs[0x4C],
                endx: s.apu_real.dsp_regs[0x7C],
                active_voices: s.apu_real.voice_active.iter().filter(|a| **a).count() as u8,
                audio_queue_len: s.apu_real.audio_queue.len(),
                last_audio_sample: s.apu_real.audio_sample(),
            });
        let stats = Stats {
            instructions_executed: self.instructions_executed,
            total_mclk: self.snes.as_ref().map_or(0, |s| s.total_mclk),
        };
        EmulatorState {
            rom: self.rom_info.clone(),
            cpu,
            ppu,
            cpu_regs,
            scheduler,
            apu,
            stats,
        }
    }

    /// Render the current PPU framebuffer (256×224, composited
    /// BG3-over-BG1-over-BG2 + sprites) as a PNG-encoded byte vector.
    pub fn render_frame_png(&self, force_display: bool) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let opts = luna_ppu::RenderOptions {
            bypass_forced_blank: force_display,
        };
        let frame = luna_ppu::render_frame_with(&snes.ppu, opts);
        let mut buf = Vec::with_capacity(FRAME_W * FRAME_H * 3);
        for px in frame {
            buf.extend_from_slice(&px);
        }
        let img =
            image::RgbImage::from_raw(FRAME_W as u32, FRAME_H as u32, buf).expect("size matches");
        let mut out = Vec::with_capacity(FRAME_W * FRAME_H);
        let dyn_image: image::DynamicImage = img.into();
        dyn_image.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
        Ok(out)
    }

    /// Drain up to `max` stereo (i16, i16) audio samples from the
    /// APU's output queue. Returns the actual samples consumed.
    pub fn drain_audio(&mut self, max: usize) -> Result<Vec<(i16, i16)>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(max.min(snes.apu_real.audio_queue.len()));
        snes.apu_real.drain_audio(&mut out, max);
        Ok(out)
    }

    /// Read `count` bytes starting at the 24-bit CPU address
    /// `bank:offset`. Reads go through the real bus, so MMIO reads
    /// have their normal side effects — pass non-MMIO ranges if you
    /// just want a memory dump.
    pub fn peek_memory(&mut self, bank: u8, offset: u16, count: u16) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(usize::from(count));
        let saved_pc = snes.cpu.pc;
        let saved_pb = snes.cpu.pb;
        snes.cpu.pc = offset;
        snes.cpu.pb = bank;
        let bytes = snes.peek_pc_bytes(count as usize);
        snes.cpu.pc = saved_pc;
        snes.cpu.pb = saved_pb;
        out.extend_from_slice(&bytes);
        Ok(out)
    }

    /// Direct read of the SPC700's ARAM. Read-only, no bus side
    /// effects.
    pub fn peek_aram(&self, offset: u16, count: u16) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(usize::from(count));
        for i in 0..count {
            out.push(snes.apu_real.aram[offset.wrapping_add(i) as usize]);
        }
        Ok(out)
    }
}

fn default_cpu_state() -> CpuState {
    CpuState {
        a: 0,
        x: 0,
        y: 0,
        sp: 0,
        pc: 0,
        pb: 0,
        db: 0,
        dp: 0,
        p: 0,
        e: false,
        stopped: false,
        waiting: false,
    }
}

fn default_ppu_state() -> PpuState {
    PpuState {
        inidisp: 0,
        bgmode: 0,
        vram_addr_words: 0,
        inidisp_write_count: 0,
        backdrop: 0,
        obsel: 0,
        vram_non_zero: 0,
        cgram_non_zero: 0,
        oam_non_zero: 0,
    }
}

fn default_cpu_regs_state() -> CpuRegsState {
    CpuRegsState {
        nmitimen: 0,
        hvbjoy: 0,
        nmi_flag: false,
        irq_flag: false,
    }
}

fn default_scheduler_state() -> SchedulerState {
    SchedulerState {
        ppu_line: 0,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
    }
}

fn default_apu_state() -> ApuState {
    ApuState {
        spc_pc: 0,
        spc_stopped: false,
        past_iplrom: false,
        unimplemented_opcode: None,
        to_cpu_ports: [0; 4],
        to_spc_ports: [0; 4],
        mvol_l: 0,
        mvol_r: 0,
        kon: 0,
        endx: 0,
        active_voices: 0,
        audio_queue_len: 0,
        last_audio_sample: (0, 0),
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(unknown panic payload)".to_string()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 32 KB LoROM cart for tests. Has a valid reset
    /// vector + cartridge-checksum so the parser accepts it.
    fn demo_lorom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        // Reset vector at LoROM $00:FFFC = ROM offset $7FFC → $8000.
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        // Title at $7FC0..$7FD4 (21 bytes ASCII, space-padded).
        let title = b"LUNA API TEST DEMO   ";
        rom[0x7FC0..0x7FC0 + title.len()].copy_from_slice(title);
        // Map mode byte at $7FD5 = $20 (LoROM, slow).
        rom[0x7FD5] = 0x20;
        // ROM size byte at $7FD7 = $07 (1 << 7 = 128 KB).
        rom[0x7FD7] = 0x07;
        // SRAM byte at $7FD8 = 0 (no SRAM).
        rom[0x7FD8] = 0x00;
        // Compute checksum + complement.
        let mut sum = 0u32;
        for (i, b) in rom.iter().enumerate() {
            if !(0x7FDC..=0x7FDF).contains(&i) {
                sum += u32::from(*b);
            }
        }
        let checksum = (sum & 0xFFFF) as u16;
        let complement = !checksum;
        rom[0x7FDC] = complement as u8;
        rom[0x7FDD] = (complement >> 8) as u8;
        rom[0x7FDE] = checksum as u8;
        rom[0x7FDF] = (checksum >> 8) as u8;
        rom
    }

    #[test]
    fn fresh_emulator_has_no_rom() {
        let e = Emulator::new();
        assert!(!e.has_rom());
    }

    #[test]
    fn load_rom_bytes_populates_rom_info() {
        let mut e = Emulator::new();
        let info = e.load_rom_bytes(demo_lorom()).expect("load");
        assert_eq!(info.title.trim_end(), "LUNA API TEST DEMO");
        assert_eq!(info.mapper, "LoRom");
        assert!(e.has_rom());
    }

    #[test]
    fn step_advances_instruction_count() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let n = e.step(50).expect("step");
        assert!(n > 0, "should execute at least one instruction");
        assert_eq!(e.state().stats.instructions_executed, n);
    }

    #[test]
    fn step_until_frame_returns_when_frame_count_changes_or_caps() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let n = e.step_until_frame(1_000_000).expect("step_until_frame");
        let s = e.state();
        assert!(n > 0);
        assert!(s.scheduler.frame_count >= 1 || n == 1_000_000);
    }

    #[test]
    fn state_serialises_to_json() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let s = e.state();
        let json = serde_json::to_string(&s).expect("serialise");
        assert!(json.contains("\"rom\""));
        assert!(json.contains("\"cpu\""));
        assert!(json.contains("\"apu\""));
    }

    #[test]
    fn no_rom_returns_no_rom_error() {
        let mut e = Emulator::new();
        let err = e.step(1).unwrap_err();
        assert!(matches!(err, ApiError::NoRom));
    }

    #[test]
    fn peek_memory_reads_through_the_bus() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        // Reset vector at $00:FFFC..$FFFD should map to ROM bytes
        // $00, $80.
        let bytes = e.peek_memory(0x00, 0xFFFC, 2).unwrap();
        assert_eq!(bytes, vec![0x00, 0x80]);
    }

    #[test]
    fn render_frame_png_round_trips_via_image_crate() {
        let mut e = Emulator::new();
        e.load_rom_bytes(demo_lorom()).unwrap();
        let png = e.render_frame_png(false).expect("png");
        assert!(png.starts_with(b"\x89PNG"), "header should be PNG magic");
    }
}
