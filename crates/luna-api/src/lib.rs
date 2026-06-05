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
//! From V1 onward this crate carries strict `SemVer` guarantees: new
//! methods are additive, breaking changes bump the major version.
//! Today (P3.3) we're still pre-V1 and the surface is allowed to
//! churn freely.

use std::path::Path;

use luna_cartridge::{CartError, Cartridge};
use luna_core::Snes;
pub use luna_core::{
    CpuTraceEvent, CpuTraceLog, DmaTraceEvent, DmaTraceLog, MailboxEvent, MailboxEventKind,
    MapperKind, MemEventKind, MemTraceEvent, MemTraceLog, Sa1LogEvent, Sa1SideEvent, Sa1TraceEvent,
    SuperFxTraceEvent,
};
/// Framebuffer dimensions (256×224), re-exported so front-ends size their
/// texture/window through `luna-api` rather than depending on `luna-ppu`.
pub use luna_ppu::{FRAME_H, FRAME_W};
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
    /// Cartridge needs a coprocessor luna does not yet emulate
    /// (S-DD1, SPC7110) — reachable via a forced mapper.
    #[error("{0}")]
    UnsupportedMapper(#[from] luna_core::UnsupportedMapper),
    /// `step` / etc. panicked inside the core.
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
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RomInfo {
    /// Title field from the internal SNES header.
    pub title: String,
    /// Detected mapper kind, e.g. `"LoRom"`, `"HiRom"`, `"ExHiRom"`.
    pub mapper: String,
    /// Cartridge ROM size in bytes (on disk).
    pub rom_bytes: usize,
    /// ROM size in KB as declared in the internal header (distinct from
    /// the on-disk byte count `rom_bytes`).
    pub header_rom_size_kb: u32,
    /// Battery-backed SRAM size in kilobytes.
    pub sram_kb: u32,
    /// `Ntsc`, `Pal`, or `Unknown`.
    pub region: String,
    /// `FastROM` (`MEMSEL`) eligibility from the header.
    pub fast_rom: bool,
    /// Mask-ROM version byte from the header.
    pub version: u8,
    /// 16-bit header checksum.
    pub checksum: u16,
    /// 16-bit header checksum complement.
    pub checksum_complement: u16,
    /// Whether `checksum` and `checksum_complement` are bitwise complements.
    pub checksum_valid: bool,
}

/// Snapshot of the emulator's observable state. Every field maps to
/// something the GUI / debugger / tests want to inspect.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
    /// SA-1 coprocessor CPU state, if the loaded cartridge hosts one.
    /// `None` for non-SA-1 carts. Diagnostic for main↔SA-1 mailbox
    /// debugging — lets you see at a glance whether the SA-1 PC is
    /// stuck in a polling loop, running random ROM bytes, or halted.
    pub sa1: Option<Sa1State>,
}

/// SA-1 coprocessor CPU snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Sa1State {
    /// Program counter within the program bank.
    pub pc: u16,
    /// Program bank.
    pub pb: u8,
    /// Processor status flags (N V M X D I Z C).
    pub p: u8,
    /// `true` while the SA-1 is released from reset (CCNT.5 clear).
    pub running: bool,
}

/// 65C816 register snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
    /// First 16 sprites' OAM low-table entries (64 bytes total, 4
    /// bytes per sprite: X.lo, Y, Tile.lo, Attrs). Lets debuggers
    /// see at a glance whether sprite 0 (e.g. Mario in SMW) has
    /// been written without dumping the full 544 bytes.
    pub oam_low_excerpt: Vec<u8>,
    /// High-table excerpt (first 4 bytes = sprites 0..15 size +
    /// X-bit-8). Together with `oam_low_excerpt` reconstructs the
    /// full state for the first 16 sprites.
    pub oam_high_excerpt: Vec<u8>,
    /// `$212C` TM — main-screen layer enable (bits 0..4 = BG1..BG4, OBJ).
    pub tm: u8,
    /// `$212D` TS — sub-screen layer enable.
    pub ts: u8,
    /// `$212E` TMW — main-screen window-clip mask.
    pub tmw: u8,
    /// `$212F` TSW — sub-screen window-clip mask.
    pub tsw: u8,
    /// `$2130` CGWSEL.
    pub cgwsel: u8,
    /// `$2131` CGADSUB.
    pub cgadsub: u8,
    /// `$2132` COLDATA red channel.
    pub coldata_r: u8,
    /// `$2132` COLDATA green channel.
    pub coldata_g: u8,
    /// `$2132` COLDATA blue channel.
    pub coldata_b: u8,
    /// `$2133` SETINI.
    pub setini: u8,
    /// `$2123` W12SEL.
    pub w12sel: u8,
    /// `$2124` W34SEL.
    pub w34sel: u8,
    /// `$2125` WOBJSEL.
    pub wobjsel: u8,
    /// `$212A` WBGLOG.
    pub wbglog: u8,
    /// `$212B` WOBJLOG.
    pub wobjlog: u8,
    /// `$2126..$2129` window left/right edges (WH0..WH3).
    pub windows: [u8; 4],
    /// Per-BG state: tilemap base, char base, scrolls, tilemap size.
    pub bgs: [BgInfo; 4],
    /// CGRAM dump — 256 BGR555 colours.
    pub cgram: Vec<u16>,
    /// Full OAM dump — 544 bytes (512 low table + 32 high table).
    pub oam_full: Vec<u8>,
}

/// Per-BG serialisable view.
#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
pub struct BgInfo {
    /// VRAM word address of the tilemap base ($2107..$210A).
    pub tilemap_addr_words: u16,
    /// VRAM word address of the character (tile) base.
    pub char_addr_words: u16,
    /// 10-bit horizontal scroll.
    pub h_scroll: u16,
    /// 10-bit vertical scroll.
    pub v_scroll: u16,
    /// BG*SC tilemap-size bits (0=32×32, 1=64×32, 2=32×64, 3=64×64).
    pub tilemap_size: u8,
}

/// CPU-system register snapshot (`$4200-$421F`).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SchedulerState {
    /// 0..=261 on NTSC; 225..=261 is `VBlank`.
    pub ppu_line: u16,
    /// Master cycles consumed within the current scanline.
    pub mcycles_in_line: u32,
    /// Frames completed since reset.
    pub frame_count: u64,
    /// Number of NMIs delivered to the CPU.
    pub nmis_serviced: u64,
}

/// APU / SPC700 / DSP snapshot.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
    /// Full 128-byte DSP register file (`$00..$7F`). Lets debuggers see
    /// per-voice volume/pitch/SRCN/ADSR/GAIN + global DIR/EVOL/EON/PMON
    /// without re-reading via the bus port.
    pub dsp_regs: Vec<u8>,
    /// First 64 bytes of ARAM at the BRR sample directory base
    /// (`DIR << 8`). Each 4-byte entry is `start_lo/start_hi/loop_lo`/
    /// `loop_hi`; 64 bytes covers 16 SRCN entries — enough to see what
    /// samples the driver has set up.
    pub dir_excerpt: Vec<u8>,
    /// Per-voice "is currently playing" (mirror of `voice_active`).
    pub voice_active: [bool; 8],
    /// Per-voice ADSR phase as a string for easy reading.
    pub voice_phase: [String; 8],
    /// Per-voice 11-bit envelope value (0..0x7FF). Independent of
    /// the `ENVX` register reading — this is the live value the DSP
    /// is using right now.
    pub voice_envelope: [u16; 8],
    /// Per-voice current BRR block address in ARAM. Set on KON from
    /// the sample directory and advanced by 9 bytes per consumed
    /// BRR block.
    pub voice_block_addr: [u16; 8],
    /// First 36 bytes of ARAM at each voice's current BRR block (4
    /// blocks of 9 bytes per voice) — lets debuggers spot whether the
    /// SPC has actually uploaded BRR data with sane headers (scale
    /// 0..12, varying filter, end/loop bits sparse).
    pub voice_brr_dump: Vec<Vec<u8>>,
    /// Per-voice 4-sample BRR history (newest first). Direct view of
    /// the post-IIR-decoded samples used as input to the Gaussian
    /// interpolator. If this is all zeros for an active voice with a
    /// non-zero envelope, decoding is failing somewhere upstream.
    pub voice_brr_history: Vec<Vec<i16>>,
    /// Per-voice 14-bit pitch accumulator. Threshold 0x4000 = 1 BRR
    /// sample per output tick.
    pub voice_pitch_acc: [u16; 8],
}

/// Diagnostic info for an SPC700 opcode that the emulator hasn't
/// implemented yet.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct UnimplementedOp {
    /// Offending opcode byte.
    pub opcode: u8,
    /// Address (in SPC700 memory) where the opcode lives.
    pub pc: u16,
}

/// One decoded OAM sprite (size, flips, and high-table bits resolved),
/// mirroring `luna_ppu::SpriteEntry` so front-ends can list sprites
/// without depending on `luna-ppu` directly.
#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
pub struct SpriteInfo {
    /// OAM index (0..=127).
    pub index: usize,
    /// Signed X position (-256..=255 after high-table sign extension).
    pub x: i16,
    /// 8-bit Y position.
    pub y: u8,
    /// 9-bit tile number.
    pub tile: u16,
    /// 3-bit palette index.
    pub palette: u8,
    /// 2-bit priority (0-3).
    pub priority: u8,
    /// Horizontal flip flag.
    pub h_flip: bool,
    /// Vertical flip flag.
    pub v_flip: bool,
    /// Sprite width in pixels.
    pub w: u16,
    /// Sprite height in pixels.
    pub h: u16,
}

/// Cumulative metrics since reset.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
    pub const fn new() -> Self {
        Self {
            snes: None,
            rom_info: None,
            instructions_executed: 0,
        }
    }

    /// Whether a ROM is currently loaded.
    #[must_use]
    pub const fn has_rom(&self) -> bool {
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

    /// Load a ROM blob with a **forced** mapper, bypassing header
    /// auto-detection. Needed for headerless homebrew test ROMs (e.g. the
    /// `PeterLemon` Super FX / GSU plot tests) that carry no chipset byte.
    pub fn load_rom_bytes_forced(
        &mut self,
        bytes: Vec<u8>,
        mapper: luna_core::MapperKind,
    ) -> Result<RomInfo, ApiError> {
        let cart = Cartridge::from_bytes_forced(bytes, mapper)?;
        self.load_cartridge(cart)
    }

    fn load_cartridge(&mut self, cart: Cartridge) -> Result<RomInfo, ApiError> {
        let info = RomInfo {
            title: cart.header.title.clone(),
            mapper: format!("{:?}", cart.header.mapper_kind),
            rom_bytes: cart.rom.len(),
            header_rom_size_kb: cart.header.rom_size_kb,
            sram_kb: cart.header.sram_size_kb,
            region: format!("{:?}", cart.header.region),
            fast_rom: cart.header.fast_rom,
            version: cart.header.version,
            checksum: cart.header.checksum,
            checksum_complement: cart.header.checksum_complement,
            checksum_valid: cart.header.checksum_valid(),
        };
        // Unsupported coprocessor carts surface as a typed
        // `UnsupportedMapper` error (no longer a panic). The
        // `catch_unwind` stays as a backstop for any *other* panic
        // during construction / `reset`, so a malformed ROM can't tear
        // down the whole transport.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut snes = Snes::try_from_cartridge(cart)?;
            snes.reset();
            Ok::<_, luna_core::UnsupportedMapper>(snes)
        }));
        match result {
            Ok(Ok(snes)) => {
                self.snes = Some(snes);
                self.rom_info = Some(info.clone());
                self.instructions_executed = 0;
                Ok(info)
            }
            Ok(Err(unsupported)) => Err(ApiError::UnsupportedMapper(unsupported)),
            Err(payload) => Err(ApiError::Panic(panic_message(&payload))),
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

    /// Set the joypad button bitmask for controller `port` (`0` or
    /// `1`). The mask is the SNES JOY1L/JOY1H layout — bit 15 = B,
    /// 14 = Y, 13 = Select, 12 = Start, 11..8 = Up/Down/Left/Right,
    /// 7..4 = A/X/L/R, 3..0 = 0 (signature). The mask is latched on
    /// the next auto-read pulse (`VBlank` entry when `NMITIMEN.0` is
    /// set), so callers should hold the mask for at least one frame
    /// before reading state.
    pub fn set_joypad(&mut self, port: u8, mask: u16) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.cpu_regs.set_joypad(usize::from(port), mask);
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
                Err(ApiError::Panic(panic_message(&payload)))
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
                Err(ApiError::Panic(panic_message(&payload)))
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
            // First 16 sprites = 64 bytes of low table + 4 bytes
            // of high table.
            let oam_low_excerpt: Vec<u8> = (0..64u16).map(|i| s.ppu.oam.peek(i)).collect();
            let oam_high_excerpt: Vec<u8> = (0..4u16).map(|i| s.ppu.oam.peek(0x200 + i)).collect();
            let cgram: Vec<u16> = (0..256u16).map(|i| s.ppu.cgram.color(i as u8)).collect();
            let oam_full: Vec<u8> = (0..0x220u16).map(|i| s.ppu.oam.peek(i)).collect();
            let bgs = std::array::from_fn(|i| BgInfo {
                tilemap_addr_words: s.ppu.bg[i].tilemap_addr_words,
                char_addr_words: s.ppu.bg[i].char_addr_words,
                h_scroll: s.ppu.bg[i].h_scroll,
                v_scroll: s.ppu.bg[i].v_scroll,
                tilemap_size: s.ppu.bg[i].tilemap_size,
            });
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
                oam_low_excerpt,
                oam_high_excerpt,
                tm: s.ppu.tm,
                ts: s.ppu.ts,
                tmw: s.ppu.tmw,
                tsw: s.ppu.tsw,
                cgwsel: s.ppu.cgwsel,
                cgadsub: s.ppu.cgadsub,
                coldata_r: s.ppu.coldata_r,
                coldata_g: s.ppu.coldata_g,
                coldata_b: s.ppu.coldata_b,
                setini: s.ppu.setini,
                w12sel: s.ppu.w12sel,
                w34sel: s.ppu.w34sel,
                wobjsel: s.ppu.wobjsel,
                wbglog: s.ppu.wbglog,
                wobjlog: s.ppu.wobjlog,
                windows: [s.ppu.wh0, s.ppu.wh1, s.ppu.wh2, s.ppu.wh3],
                bgs,
                cgram,
                oam_full,
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
                mvol_l: s.apu_real.dsp.registers[0x0C] as i8,
                mvol_r: s.apu_real.dsp.registers[0x1C] as i8,
                kon: s.apu_real.dsp.registers[0x4C],
                endx: s.apu_real.dsp.registers[0x7C],
                active_voices: s
                    .apu_real
                    .dsp
                    .voices
                    .iter()
                    .filter(|v| {
                        v.envelope_mode != luna_apu::dsp::EnvelopeMode::Release || v.envelope != 0
                    })
                    .count() as u8,
                audio_queue_len: s.apu_real.audio_queue.len(),
                last_audio_sample: s.apu_real.audio_sample(),
                dsp_regs: s.apu_real.dsp.registers.to_vec(),
                dir_excerpt: {
                    let base = (s.apu_real.dsp.registers[0x5D] as usize) << 8;
                    let mut v = Vec::with_capacity(64);
                    for i in 0..64 {
                        v.push(s.apu_real.aram[(base + i) & 0xFFFF]);
                    }
                    v
                },
                voice_active: std::array::from_fn(|i| {
                    let v = &s.apu_real.dsp.voices[i];
                    v.envelope_mode != luna_apu::dsp::EnvelopeMode::Release || v.envelope != 0
                }),
                voice_phase: std::array::from_fn(|i| {
                    format!("{:?}", s.apu_real.dsp.voices[i].envelope_mode)
                }),
                voice_envelope: std::array::from_fn(|i| s.apu_real.dsp.voices[i].envelope),
                voice_block_addr: std::array::from_fn(|i| s.apu_real.dsp.voices[i].brr_address),
                voice_brr_dump: (0..8)
                    .map(|v| {
                        let base = s.apu_real.dsp.voices[v].brr_address as usize;
                        (0..36)
                            .map(|i| s.apu_real.aram[(base + i) & 0xFFFF])
                            .collect()
                    })
                    .collect(),
                voice_brr_history: (0..8)
                    .map(|v| s.apu_real.dsp.voices[v].buffer.to_vec())
                    .collect(),
                voice_pitch_acc: std::array::from_fn(|i| s.apu_real.dsp.voices[i].gaussian_offset),
            });
        let stats = Stats {
            instructions_executed: self.instructions_executed,
            total_mclk: self.snes.as_ref().map_or(0, |s| s.total_mclk),
        };
        let sa1 = self
            .snes
            .as_ref()
            .and_then(|s| s.mapper.sa1_snapshot())
            .map(|snap| Sa1State {
                pc: snap.pc,
                pb: snap.pb,
                p: snap.p,
                running: snap.running,
            });
        EmulatorState {
            rom: self.rom_info.clone(),
            cpu,
            ppu,
            cpu_regs,
            scheduler,
            apu,
            stats,
            sa1,
        }
    }

    /// Render the current PPU framebuffer (256×224, composited
    /// BG3-over-BG1-over-BG2 + sprites) as a PNG-encoded byte vector.
    ///
    /// Default path (`force_display=false`) is zero-cost — it copies
    /// the persistent framebuffer that the scheduler has been
    /// populating one scanline at a time (gap G6 Phase 1). The
    /// force-display debug path still re-renders the whole frame
    /// via `render_frame_with` with `bypass_forced_blank: true`.
    pub fn render_frame_png(&self, force_display: bool) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut buf = Vec::with_capacity(FRAME_W * FRAME_H * 3);
        if force_display {
            // Debug-only path: rebuild the frame with forced-blank
            // bypass so the user can see VRAM contents even when the
            // game is keeping the screen blanked.
            let opts = luna_ppu::RenderOptions {
                bypass_forced_blank: true,
            };
            let frame = luna_ppu::render_frame_with(&snes.ppu, opts);
            for px in frame {
                buf.extend_from_slice(&px);
            }
        } else {
            for px in snes.ppu.framebuffer() {
                buf.extend_from_slice(px);
            }
        }
        let img =
            image::RgbImage::from_raw(FRAME_W as u32, FRAME_H as u32, buf).expect("size matches");
        let mut out = Vec::with_capacity(FRAME_W * FRAME_H);
        let dyn_image: image::DynamicImage = img.into();
        dyn_image.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
        Ok(out)
    }

    /// Render a single BG layer (`bg_idx` 0..=3) in isolation as a
    /// PNG-encoded byte vector — the `luna run --bg N` debug path.
    /// `force_display` bypasses INIDISP forced-blank.
    pub fn render_frame_bg_png(
        &self,
        bg_idx: usize,
        force_display: bool,
    ) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let opts = luna_ppu::RenderOptions {
            bypass_forced_blank: force_display,
        };
        let mut buf = Vec::with_capacity(FRAME_W * FRAME_H * 3);
        for px in luna_ppu::render_frame_bg_with(&snes.ppu, bg_idx, opts) {
            buf.extend_from_slice(&px);
        }
        let img =
            image::RgbImage::from_raw(FRAME_W as u32, FRAME_H as u32, buf).expect("size matches");
        let mut out = Vec::with_capacity(FRAME_W * FRAME_H);
        let dyn_image: image::DynamicImage = img.into();
        dyn_image.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
        Ok(out)
    }

    /// Peek `count` bytes starting at the current CPU program counter
    /// (`PB:PC`), through the CPU bus view. Diagnostic (needs `&mut` for
    /// the bus). Wraps the core's `peek_pc_bytes`.
    pub fn peek_pc_bytes(&mut self, count: usize) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.peek_pc_bytes(count))
    }

    /// Decode all 128 OAM sprites (size, flips, and high-table bits
    /// resolved). Diagnostic surface so front-ends needn't import
    /// `luna-ppu`'s renderer to list sprites.
    pub fn decode_sprites(&self) -> Result<Vec<SpriteInfo>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok(luna_ppu::decode_all_sprites(&snes.ppu)
            .iter()
            .enumerate()
            .map(|(index, s)| SpriteInfo {
                index,
                x: s.x,
                y: s.y,
                tile: s.tile,
                palette: s.palette,
                priority: s.priority,
                h_flip: s.h_flip,
                v_flip: s.v_flip,
                w: s.w,
                h: s.h,
            })
            .collect())
    }

    /// Render the current PPU framebuffer as raw **RGBA** bytes
    /// (`256 × 224 × 4`, row-major, alpha forced to `0xFF`) — the
    /// uncompressed form a GUI uploads straight to a texture, sharing the
    /// exact render path as [`Emulator::render_frame_png`] so the CLI and
    /// GUI cannot disagree on pixels. `force_display` bypasses INIDISP
    /// forced-blank (debug: see VRAM even while the game blanks).
    pub fn render_frame_rgba(&self, force_display: bool) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(FRAME_W * FRAME_H * 4);
        let mut push_rgb = |px: &[u8; 3]| {
            out.extend_from_slice(px);
            out.push(0xFF);
        };
        if force_display {
            let opts = luna_ppu::RenderOptions {
                bypass_forced_blank: true,
            };
            for px in luna_ppu::render_frame_with(&snes.ppu, opts) {
                push_rgb(&px);
            }
        } else {
            for px in snes.ppu.framebuffer() {
                push_rgb(px);
            }
        }
        Ok(out)
    }

    /// The emulated PPU frame counter — cheap, for a GUI's frame-boundary
    /// detection without a full [`Emulator::state`] snapshot.
    pub fn frame_count(&self) -> Result<u64, ApiError> {
        Ok(self.snes.as_ref().ok_or(ApiError::NoRom)?.frame_count)
    }

    /// Whether the screen is in INIDISP forced-blank (bit 7) right now —
    /// cheap accessor so a GUI can hold the last non-blank frame without
    /// snapshotting full state. The forced-blank *render* policy itself
    /// lives in [`Emulator::render_frame_rgba`]; this only reports it.
    pub fn forced_blank(&self) -> Result<bool, ApiError> {
        Ok(self.snes.as_ref().ok_or(ApiError::NoRom)?.ppu.inidisp & 0x80 != 0)
    }

    /// Number of stereo samples currently waiting in the APU output
    /// queue — cheap, lets an audio-paced GUI drain exactly the host
    /// ring's free space and tell whether the ring (not the queue) was
    /// the limiter, without re-queuing rejected samples.
    pub fn audio_queue_len(&self) -> Result<usize, ApiError> {
        Ok(self
            .snes
            .as_ref()
            .ok_or(ApiError::NoRom)?
            .apu_real
            .audio_queue
            .len())
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

    /// Enable APU mailbox (`$2140-$2143`) event logging. Every CPU
    /// read or write of those ports from this point onward is captured
    /// in an in-memory ring buffer that the caller can drain with
    /// [`Emulator::take_mailbox_log`]. Cheap when disabled.
    pub fn enable_mailbox_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_mailbox_log();
        Ok(())
    }

    /// Take ownership of the accumulated mailbox events, resetting the
    /// buffer to empty. Returns an empty `Vec` if logging is disabled
    /// or no events were captured.
    pub fn take_mailbox_log(&mut self) -> Result<Vec<MailboxEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_mailbox_log())
    }

    /// Enable SA-1 MMIO (`$2200-$23FF`) event logging. Every CPU read or
    /// write of an SA-1 register from this point is captured for draining
    /// with [`Emulator::take_sa1_log`]. Cheap when disabled.
    pub fn enable_sa1_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_sa1_log();
        Ok(())
    }

    /// Take ownership of the accumulated SA-1 MMIO events, resetting the
    /// buffer. Returns an empty `Vec` if logging is disabled.
    pub fn take_sa1_log(&mut self) -> Result<Vec<Sa1LogEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_sa1_log())
    }

    /// Enable SA-1-*side* execution logging (the SA-1's own `$2200-$23FF`
    /// accesses with its PC). Complements [`Emulator::enable_sa1_log`].
    /// No-op for non-SA-1 carts.
    pub fn enable_sa1_side_log(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_sa1_side_log();
        Ok(())
    }

    /// Drain the SA-1-side execution log (empty if disabled / not SA-1).
    pub fn take_sa1_side_log(&mut self) -> Result<Vec<Sa1SideEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_sa1_side_log())
    }

    /// Enable a full SA-1 instruction trace (pre-opcode register snapshot
    /// per SA-1 instruction, capped at `max_events`). No-op for non-SA-1.
    pub fn enable_sa1_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_sa1_trace(max_events);
        Ok(())
    }

    /// Drain the SA-1 instruction trace (empty if disabled / not SA-1).
    pub fn take_sa1_trace(&mut self) -> Result<Vec<Sa1TraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_sa1_trace())
    }

    /// Enable a per-opcode Super FX (GSU) instruction trace (PC + register
    /// file per opcode), for diffing the GSU stream against a reference.
    pub fn enable_superfx_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_superfx_trace(max_events);
        Ok(())
    }

    /// Drain the Super FX instruction trace (empty if disabled / not GSU).
    pub fn take_superfx_trace(&mut self) -> Result<Vec<SuperFxTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_superfx_trace())
    }

    /// Enable the DMA→VRAM transfer-time trace: every byte an MDMA writes
    /// to `$2118/$2119` is captured as (source A-bus address → VMADD word
    /// → byte) AT transfer time. Lets a coprocessor framebuffer
    /// (Super FX) be checked against the VRAM it produced without the
    /// double-buffer confound of a post-hoc source dump.
    pub fn enable_dma_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.dma.enable_dma_trace(max_events);
        Ok(())
    }

    /// Drain the DMA→VRAM trace (empty if disabled).
    pub fn take_dma_trace(&mut self) -> Result<Vec<DmaTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.dma.take_dma_trace())
    }

    /// Dump all 64 KB of PPU VRAM (byte-addressed). For diagnosing the
    /// framebuffer DMA → VRAM → display path of coprocessor renderers.
    pub fn vram_bytes(&self) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        Ok((0u32..0x1_0000)
            .map(|a| snes.ppu.vram.peek(a as u16))
            .collect())
    }

    /// Enable per-instruction CPU tracing. Every subsequent
    /// [`Emulator::step`] / [`Emulator::step_until_frame`] tick
    /// captures a register-file snapshot until `max_events` events
    /// have been recorded. Drain with [`Emulator::take_cpu_trace_log`].
    pub fn enable_cpu_trace(&mut self, max_events: usize) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_cpu_trace(max_events);
        Ok(())
    }

    /// Take ownership of the accumulated CPU trace events.
    pub fn take_cpu_trace_log(&mut self) -> Result<Vec<CpuTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_cpu_trace_log())
    }

    /// Current cumulative instruction count since the last reset /
    /// rom-load. Used by the CLI to gate "start tracing at instr N".
    #[must_use]
    pub const fn instructions_executed(&self) -> u64 {
        self.instructions_executed
    }

    /// Enable per-access memory tracing. Every CPU bus read/write
    /// from this point matching `bank_filter` (or every access when
    /// `None`) is captured into the log until `max_events` is
    /// reached. Drain with [`Emulator::take_mem_trace_log`].
    pub fn enable_mem_trace(
        &mut self,
        max_events: usize,
        bank_filter: Option<u8>,
    ) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_mem_trace(max_events, bank_filter);
        Ok(())
    }

    /// Take ownership of the accumulated memory access events.
    pub fn take_mem_trace_log(&mut self) -> Result<Vec<MemTraceEvent>, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        Ok(snes.take_mem_trace_log())
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

    /// Direct read of PPU VRAM. `offset` is a *byte* address (0..0xFFFF
    /// — VRAM is 64 KB), `count` how many consecutive bytes to read.
    /// Read-only, no bus side effects.
    pub fn peek_vram(&self, offset: u16, count: u16) -> Result<Vec<u8>, ApiError> {
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let mut out = Vec::with_capacity(usize::from(count));
        for i in 0..count {
            out.push(snes.ppu.vram.peek(offset.wrapping_add(i)));
        }
        Ok(out)
    }
}

const fn default_cpu_state() -> CpuState {
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

const fn default_ppu_state() -> PpuState {
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
        oam_low_excerpt: Vec::new(),
        oam_high_excerpt: Vec::new(),
        tm: 0,
        ts: 0,
        tmw: 0,
        tsw: 0,
        cgwsel: 0,
        cgadsub: 0,
        coldata_r: 0,
        coldata_g: 0,
        coldata_b: 0,
        setini: 0,
        w12sel: 0,
        w34sel: 0,
        wobjsel: 0,
        wbglog: 0,
        wobjlog: 0,
        windows: [0; 4],
        bgs: [BgInfo {
            tilemap_addr_words: 0,
            char_addr_words: 0,
            h_scroll: 0,
            v_scroll: 0,
            tilemap_size: 0,
        }; 4],
        cgram: Vec::new(),
        oam_full: Vec::new(),
    }
}

const fn default_cpu_regs_state() -> CpuRegsState {
    CpuRegsState {
        nmitimen: 0,
        hvbjoy: 0,
        nmi_flag: false,
        irq_flag: false,
    }
}

const fn default_scheduler_state() -> SchedulerState {
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
        dsp_regs: vec![0; 128],
        dir_excerpt: vec![0; 64],
        voice_active: [false; 8],
        voice_phase: std::array::from_fn(|_| "Off".to_string()),
        voice_envelope: [0; 8],
        voice_block_addr: [0; 8],
        voice_brr_dump: vec![vec![0; 36]; 8],
        voice_brr_history: vec![vec![0; 4]; 8],
        voice_pitch_acc: [0; 8],
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
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

    /// Build a minimal 32 KB `LoROM` cart for tests. Has a valid reset
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
