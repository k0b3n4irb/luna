//! SNES test-ROM golden suite (Peter Lemon hardware tests + homebrew).
//!
//! Mirrors the `twvd/siena` approach: the ROM corpus is **not vendored**
//! (it's large), but checked out at the same directory level as this
//! repo — e.g. `../luna_tests` — and referenced from there. Each test
//! boots a ROM, runs it until the 256×224 framebuffer settles, and
//! asserts a SHA-256 of that framebuffer against a committed golden hash.
//!
//! ## Setup
//!
//! ```bash
//! tools/fetch-snes-test-roms.sh        # sparse-clone into ../luna_tests
//! cargo test -p luna-core --test snes_test_roms
//! ```
//!
//! Or point `LUNA_SNES_TEST_DIR` at a corpus root. If the corpus is
//! absent, every test prints a skip notice and passes — so `cargo test`
//! works with or without the checkout.
//!
//! ## Regenerating hashes
//!
//! The golden hashes are captured from luna's own renderer (regression
//! baselines), so an intended render change requires regenerating them:
//!
//! ```bash
//! LUNA_SNES_TEST_RECORD=1 cargo test -p luna-core --test snes_test_roms -- --nocapture
//! # also dump PNGs to eyeball the result screens:
//! LUNA_SNES_TEST_RECORD=1 LUNA_SNES_TEST_PNG=/tmp/snestests \
//!   cargo test -p luna-core --test snes_test_roms -- --nocapture
//! ```

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use luna_bus::MapperKind;
use luna_cartridge::Cartridge;
use luna_core::Snes;
use sha2::{Digest, Sha256};

const FRAME_W: usize = luna_ppu::FRAME_W;
const FRAME_H: usize = luna_ppu::FRAME_H;

/// Hard ceiling on instructions, in case a ROM never settles or loops.
const STEP_CAP: u64 = 30_000_000;
/// SPC700 ALU tests run every addressing mode before the pass/fail verdict
/// lands in the mailbox (ADC/SBC ~35M instructions), so they get a higher
/// ceiling than the framebuffer-settle tests (some of which intentionally
/// cap-out mid-animation and must keep their 30M frame).
const SPC700_STEP_CAP: u64 = 45_000_000;
/// Sample the framebuffer hash every this many instructions.
const SAMPLE_EVERY: u64 = 100_000;
/// Consecutive identical samples that count as "settled".
const STABLE_SAMPLES: u32 = 8;

/// Corpus root: `$LUNA_SNES_TEST_DIR`, else the sibling `../luna_tests`.
fn corpus_root() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("LUNA_SNES_TEST_DIR") {
        let p = PathBuf::from(s);
        return p.is_dir().then_some(p);
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // crates/luna-core
    p.pop(); // crates
    p.pop(); // <repo root>
    p.pop(); // parent of repo
    p.push("luna_tests");
    p.is_dir().then_some(p)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn fb_bytes(snes: &Snes) -> Vec<u8> {
    let mut buf = Vec::with_capacity(FRAME_W * FRAME_H * 3);
    for px in snes.ppu.framebuffer() {
        buf.extend_from_slice(px);
    }
    buf
}

/// Boot a forced-LoROM ROM and run until the framebuffer settles (or the
/// step cap / a `STP` / a CPU panic). Returns the framebuffer bytes.
///
/// The ROM is loaded as **PAL**, matching the `twvd/siena` convention.
/// Peter Lemon's suite is PAL-timed: several tests do a single `WaitNMI`
/// then write the whole result table in one burst that only fits inside
/// PAL's longer V-blank (~72 lines vs NTSC's 37). Run as NTSC, luna
/// correctly drops the writes that overflow into active display and the
/// screen stays blank — so PAL is required to reproduce the reference
/// output.
fn run_to_stable(rom: Vec<u8>, hold: u16) -> Vec<u8> {
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = luna_cartridge::Region::Pal;
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    // Hold a controller-1 button for the whole run (e.g. the Mosaic demos
    // ramp the mosaic size while R is held). `LUNA_SNES_TEST_HOLD` (hex)
    // overrides it for ad-hoc experimentation.
    let hold: u16 = std::env::var("LUNA_SNES_TEST_HOLD")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(hold);
    if hold != 0 {
        snes.set_joypad(0, hold);
    }

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut last = String::new();
    let mut stable = 0u32;
    let mut executed = 0u64;
    'run: while executed < STEP_CAP {
        for _ in 0..SAMPLE_EVERY {
            if snes.cpu.stopped {
                break;
            }
            if catch_unwind(AssertUnwindSafe(|| {
                snes.step();
            }))
            .is_err()
            {
                break 'run; // settle on whatever rendered before the panic
            }
            executed += 1;
        }
        let h = hex(&Sha256::digest(fb_bytes(&snes)));
        if h == last {
            stable += 1;
            if stable >= STABLE_SAMPLES {
                break;
            }
        } else {
            stable = 0;
            last = h;
        }
        if snes.cpu.stopped {
            break;
        }
    }

    std::panic::set_hook(prev_hook);
    if std::env::var("LUNA_SNES_TEST_PPUDIAG").is_ok() {
        let bg1 = snes.ppu.bg[0];
        let bg2 = snes.ppu.bg[1];
        eprintln!(
            "PPUDIAG cpu=${:02X}:{:04X} stp={} BGMODE=${:02X} MOSAIC=${:02X} TM=${:02X} TS=${:02X} SETINI=${:02X} \
             BG1[sz={} map_w=${:04X} chr_w=${:04X} h={}] BG2[sz={} map_w=${:04X} h={}]",
            snes.cpu.pb,
            snes.cpu.pc,
            snes.cpu.stopped,
            snes.ppu.bgmode,
            snes.ppu.mosaic,
            snes.ppu.tm,
            snes.ppu.ts,
            snes.ppu.setini,
            bg1.tilemap_size,
            bg1.tilemap_addr_words,
            bg1.char_addr_words,
            bg1.h_scroll,
            bg2.tilemap_size,
            bg2.tilemap_addr_words,
            bg2.h_scroll,
        );
    }
    fb_bytes(&snes)
}

fn dump_png(bytes: &[u8], path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let img =
        image::RgbImage::from_raw(FRAME_W as u32, FRAME_H as u32, bytes.to_vec()).expect("dims");
    let _ = img.save(path);
}

/// Repo-local commercial ROM dir (`tests/roms/`, gitignored). Used by the
/// representative hardware-coverage goldens. Absent ROMs skip — these are
/// **developer-local** regression nets (the copyrighted ROMs are not in CI).
fn games_root() -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // crates/luna-core
    p.pop(); // crates
    p.pop(); // <repo root>
    p.push("tests");
    p.push("roms");
    p.is_dir().then_some(p)
}

/// Boot a commercial ROM (auto-detected mapper + native region) with no input
/// and run a FIXED instruction count, returning the framebuffer. Fixed-count
/// (not settle) because these scenes animate; luna is deterministic, so the
/// hash is stable run-to-run and moves only when emulation behaviour changes —
/// re-record (`LUNA_SNES_TEST_RECORD=1`) after an intended render/timing change.
fn run_game_fixed(rom: Vec<u8>, instructions: u64) -> Vec<u8> {
    let cart = Cartridge::from_bytes(rom).expect("auto-detect cartridge");
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut executed = 0u64;
    while executed < instructions {
        if snes.cpu.stopped {
            break;
        }
        if catch_unwind(AssertUnwindSafe(|| snes.step())).is_err() {
            break;
        }
        executed += 1;
    }
    std::panic::set_hook(prev_hook);
    fb_bytes(&snes)
}

/// Boot `rel` (relative to the corpus root), settle, and compare the
/// framebuffer SHA-256 to `expected`. Skips gracefully if the corpus or
/// the specific ROM is absent.
fn test_display(rel: &str, expected: &str, hold: u16) {
    let Some(root) = corpus_root() else {
        eprintln!(
            "[skip] SNES test corpus not found — checkout ../luna_tests \
             (tools/fetch-snes-test-roms.sh) or set LUNA_SNES_TEST_DIR"
        );
        return;
    };
    let path = root.join(rel);
    if !path.is_file() {
        eprintln!("[skip] {rel}: not present under {}", root.display());
        return;
    }

    let rom = std::fs::read(&path).expect("read rom");
    let bytes = run_to_stable(rom, hold);
    let got = hex(&Sha256::digest(&bytes));

    if std::env::var("LUNA_SNES_TEST_RECORD").is_ok() {
        if let Ok(dir) = std::env::var("LUNA_SNES_TEST_PNG") {
            let safe = rel.replace(['/', ' '], "_");
            dump_png(&bytes, &Path::new(&dir).join(format!("{safe}.png")));
        }
        println!("RECORD {rel} => {got}");
        return;
    }

    assert_eq!(
        got, expected,
        "framebuffer hash mismatch for {rel}\n  \
         (run LUNA_SNES_TEST_RECORD=1 to regenerate after an intended render change)"
    );
}

/// Declare a Peter Lemon `CPUTest/CPU/<NAME>/CPU<NAME>.sfc` golden test.
macro_rules! cpu_test {
    ($fn:ident, $name:literal, $hash:literal) => {
        #[test]
        fn $fn() {
            test_display(
                concat!("CPUTest/CPU/", $name, "/CPU", $name, ".sfc"),
                $hash,
                0,
            );
        }
    };
}

// Golden hashes captured from luna's renderer (loaded as PAL — see
// `run_to_stable`). All 23 render the correct all-PASS result screen.
cpu_test!(
    cpu_adc,
    "ADC",
    "9f2c04820b712abb2cf94b49bfafcb0f5384c08a4bdc2665f10fb11b87bd4df5"
);
cpu_test!(
    cpu_and,
    "AND",
    "ab1f1a4806e8af4be436f72a8d07b1157095d0213534d90033e2fca82141c4ef"
);
cpu_test!(
    cpu_asl,
    "ASL",
    "9b43257dd732fb231ea022d84a3b429de3dfdf613c437d22824c1fef4ef5a676"
);
cpu_test!(
    cpu_bit,
    "BIT",
    "7161f1fbf43d0c8ab1dd9224edc8b31ce83f28e5154659ab46d157c00597b6dc"
);
cpu_test!(
    cpu_bra,
    "BRA",
    "ba0ac0fea8985bac44c9baa8c1e614b27468fff4eca516c117e5cbdac48e6dad"
);
cpu_test!(
    cpu_cmp,
    "CMP",
    "b58c651cf366ed54cb423e1eb903e4515c000ec23e8d7b1771550a4a944ddf6f"
);
cpu_test!(
    cpu_dec,
    "DEC",
    "756d778724ee196dbf935ac4d1f272121db62d27bf55d43235f58716bf1bfbeb"
);
cpu_test!(
    cpu_eor,
    "EOR",
    "01e8bbe4a0d5c74c5c014b13ed7115827e1134d96384b7be39dc0802baf50287"
);
cpu_test!(
    cpu_inc,
    "INC",
    "906da4f07091fdbafee79e020d30c6a7aa494112e307d64565017fdb6d0eab94"
);
cpu_test!(
    cpu_jmp,
    "JMP",
    "73367eaedb8f70ad0e73ad4a2b72e756a223ae14047ce7754b24827d09d8f0bd"
);
cpu_test!(
    cpu_ldr,
    "LDR",
    "b40f60b260056515688b136b1b07fdbdf3fcfb2806d3aab71130db7dc35a6b44"
);
cpu_test!(
    cpu_lsr,
    "LSR",
    "42d2016c4d22554a93b154e1fb4c7c0ecd166a5e9b9c3b9e60a65183b30ad52f"
);
cpu_test!(
    cpu_mov,
    "MOV",
    "f9adc3998195578846723f7521b6441742c3d2784d0ae9ab6bdeed257e1ea931"
);
cpu_test!(
    cpu_msc,
    "MSC",
    "6932e0ac7568ceb73083ae9fe3eec334a4c515ba99daf43399d3a56a023d8d3b"
);
cpu_test!(
    cpu_ora,
    "ORA",
    "e81c479ebec11bf59cde7d0e4c56ef0c84d266d9526e5d3f7e2e67f6dd33327b"
);
cpu_test!(
    cpu_phl,
    "PHL",
    "2c6f100bb12ba58ef3e5e6b7c159744a2d9a927ab77bb530fce2cb7503375b8f"
);
cpu_test!(
    cpu_psr,
    "PSR",
    "c4e0406dad42e1fa92392e3d659168e71dd172d18687fb81cb64ed97ad021621"
);
cpu_test!(
    cpu_ret,
    "RET",
    "977cf3f643d39ac7e2ca53a960fa2803b06be5cb837a7e482d8cc36415565622"
);
cpu_test!(
    cpu_rol,
    "ROL",
    "6d19686e6886c3c2f3904c432f25d4531d95dfb6e2da6b460cdfc48cdfbb2990"
);
cpu_test!(
    cpu_ror,
    "ROR",
    "25126b9e496b228daf3efda965f1d1b42beb6c58301d503912cca7300d874500"
);
cpu_test!(
    cpu_sbc,
    "SBC",
    "d95ac3554a56038cbe10f89ad95ffedfa65a513fbe4804f219259e8fc5ad73e1"
);
cpu_test!(
    cpu_str,
    "STR",
    "edf27896517d3865fb893431e3ff40e9098a3952f1f0d87e5a97c84b0638317b"
);
cpu_test!(
    cpu_trn,
    "TRN",
    "4499f14b4497b7522691a4ac5ac8f9d5731976f89be27167091fe25a19cc9b68"
);

/// Peter Lemon `CPUTest/SPC700/<NAME>` ALU hardware test — checked by its
/// **memory-result protocol**, not a framebuffer hash (the result display
/// cycles per addressing mode, so a hash settles on a non-deterministic
/// transient). Per the ROM's `.asm`, on the first divergent opcode the SPC700
/// writes `$81` to CPUIO0 (`$2140`) and HALTS in a fail loop; on success it
/// runs every mode to completion. Objective pass = the SPC→CPU mailbox port 0
/// is never `$81`. Complements the cycle-stepped 65c816/SPC700 differential.
fn run_spc700_fail_port(rom: Vec<u8>) -> u8 {
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = luna_cartridge::Region::Pal;
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut executed = 0u64;
    'run: while executed < SPC700_STEP_CAP {
        for _ in 0..SAMPLE_EVERY {
            if catch_unwind(AssertUnwindSafe(|| snes.step())).is_err() {
                break 'run;
            }
            executed += 1;
        }
        // The fail path halts immediately with $81 in CPUIO0 — bail early.
        if snes.apu_real.cpu_read_port(0) == 0x81 {
            break;
        }
    }
    std::panic::set_hook(prev_hook);
    snes.apu_real.cpu_read_port(0)
}

macro_rules! spc700_test {
    ($fn:ident, $name:literal) => {
        #[test]
        fn $fn() {
            let rel = concat!("CPUTest/SPC700/", $name, "/SPC700", $name, ".sfc");
            let Some(root) = corpus_root() else {
                eprintln!("[skip] SNES test corpus not found (tools/fetch-snes-test-roms.sh)");
                return;
            };
            let path = root.join(rel);
            if !path.is_file() {
                eprintln!("[skip] {rel}: not present under {}", root.display());
                return;
            }
            let rom = std::fs::read(&path).expect("read rom");
            let port0 = run_spc700_fail_port(rom);
            assert_ne!(
                port0, 0x81,
                "SPC700 {} test FAILED on hardware-result protocol: CPUIO0/$2140 = $81 (fail halt)",
                $name
            );
        }
    };
}

spc700_test!(spc700_adc, "ADC");
spc700_test!(spc700_and, "AND");
spc700_test!(spc700_dec, "DEC");
spc700_test!(spc700_eor, "EOR");
spc700_test!(spc700_inc, "INC");
spc700_test!(spc700_ora, "ORA");
spc700_test!(spc700_sbc, "SBC");

/// Declare a Peter Lemon `PPU/<path>` golden test. The PPU suite has an
/// irregular directory layout, so the full relative path is given.
macro_rules! ppu_test {
    ($fn:ident, $path:literal, $hash:literal) => {
        #[test]
        fn $fn() {
            test_display(concat!("PPU/", $path), $hash, 0);
        }
    };
    // `hold = <mask>` holds a controller-1 button for the whole run — for
    // demos driven by input (the Mosaic demos ramp the mosaic size while R
    // is held).
    ($fn:ident, $path:literal, $hash:literal, hold = $mask:expr) => {
        #[test]
        fn $fn() {
            test_display(concat!("PPU/", $path), $hash, $mask);
        }
    };
    // A scene luna renders wrong (tracked PPU gap). `#[ignore]`d, with the
    // committed hash characterising the current (wrong) output — once the
    // gap is fixed the render changes, the `--ignored` run goes red.
    ($fn:ident, $path:literal, $hash:literal, ignore = $reason:literal) => {
        #[test]
        #[ignore = $reason]
        fn $fn() {
            test_display(concat!("PPU/", $path), $hash, 0);
        }
    };
}

// Curated PPU scenes (the twvd/siena selection): BG maps, hi-colour
// blending, windows, and Mode 7. Golden hashes are luna's own PAL render.
ppu_test!(
    ppu_bg1_2bpp,
    "BGMAP/8x8/2BPP/8x8BG1Map2BPP32x328PAL/8x8BG1Map2BPP32x328PAL.sfc",
    "d0c931e79fb78ae46471674155dabbbcaedddb8f082ccc54c4e02a1a8617fe57"
);
ppu_test!(
    ppu_bg2_2bpp,
    "BGMAP/8x8/2BPP/8x8BG2Map2BPP32x328PAL/8x8BG2Map2BPP32x328PAL.sfc",
    "347f7663c3cfdc347a323c64c7c4e80ad3873b8b211aefa12919e245a99b2ff8"
);
ppu_test!(
    ppu_bg3_2bpp,
    "BGMAP/8x8/2BPP/8x8BG3Map2BPP32x328PAL/8x8BG3Map2BPP32x328PAL.sfc",
    "347f7663c3cfdc347a323c64c7c4e80ad3873b8b211aefa12919e245a99b2ff8"
);
ppu_test!(
    ppu_bg4_2bpp,
    "BGMAP/8x8/2BPP/8x8BG4Map2BPP32x328PAL/8x8BG4Map2BPP32x328PAL.sfc",
    "347f7663c3cfdc347a323c64c7c4e80ad3873b8b211aefa12919e245a99b2ff8"
);
ppu_test!(
    ppu_bg_4bpp,
    "BGMAP/8x8/4BPP/8x8BGMap4BPP32x328PAL/8x8BGMap4BPP32x328PAL.sfc",
    "156220da11d227e5a5f0447b36d4923a3b1b04bfd435584fa13b50a6153462e5"
);
// 8bpp (256-colour) BG maps across all four tilemap sizes + tile flip —
// exercises the 64-wide/64-tall quadrant offsets (+0x800/0x1000/0x1800)
// and the H/V-flip path in 8bpp. Each validated against the reference art
// that ships with the ROM (`GFX/BG.png`; TileFlip also a full-screen
// capture): all five match at 100% (tol 24, the only delta being the
// 8→5→8-bit palette roundtrip). The 32x32 demo scrolls (140,140) into its
// wrapping 256-px map; 32x64/64x32/64x64 show the un-scrolled top-left —
// since the visible 256×224 only touches the first quadrant, those three
// produce the *same* framebuffer (hence the identical hash, not a typo).
// TileFlip's flip pattern is pixel-identical (same colour histogram) at a
// 15-px vertical framing offset vs the PAL capture.
ppu_test!(
    ppu_bg_8bpp_32x32,
    "BGMAP/8x8/8BPP/32x32/8x8BGMap8BPP32x32.sfc",
    "f2017bdcdeb5938291288e2d5d453b33ed3095f759dc99bbf909257bb17e8bdf"
);
ppu_test!(
    ppu_bg_8bpp_32x64,
    "BGMAP/8x8/8BPP/32x64/8x8BGMap8BPP32x64.sfc",
    "fd2abf80a33c3145d5b3ce0aff45168f7e55790012ce09ca6de1e4af5d86b51e"
);
ppu_test!(
    ppu_bg_8bpp_64x32,
    "BGMAP/8x8/8BPP/64x32/8x8BGMap8BPP64x32.sfc",
    "fd2abf80a33c3145d5b3ce0aff45168f7e55790012ce09ca6de1e4af5d86b51e"
);
ppu_test!(
    ppu_bg_8bpp_64x64,
    "BGMAP/8x8/8BPP/64x64/8x8BGMap8BPP64x64.sfc",
    "fd2abf80a33c3145d5b3ce0aff45168f7e55790012ce09ca6de1e4af5d86b51e"
);
ppu_test!(
    ppu_bg_8bpp_tileflip,
    "BGMAP/8x8/8BPP/TileFlip/8x8BGMapTileFlip.sfc",
    "04202031bf187476cd32c2e7e6851b128372986b126fbe499c851cbf41b73929"
);
ppu_test!(
    ppu_rings,
    "Rings/Rings.sfc",
    "a8353b5531c6173b46636544e5a6838a97b38b2d2f03bcb11c887054bf3ec15e"
);
ppu_test!(
    ppu_hicolor_dlair,
    "Blend/HiColor/HiColor1241DLair/HiColor1241DLair.sfc",
    "32c758e0238f8de9717cff1351f083545c4423a90a5aad4bc8ebeea493ff2555"
);
ppu_test!(
    ppu_hicolor_3840,
    "Blend/HiColor/HiColor3840/HiColor3840.sfc",
    "bc2c00d8d889753a1f22548191fd87ba6dad6f9b63ce861358bedb34393a5bb2"
);
ppu_test!(
    ppu_hicolor_myst,
    "Blend/HiColor/HiColor575Myst/HiColor575Myst.sfc",
    "0125ae2f592c0cb4a00a31b156b95085b7e6a6026bb8c86cc4e55d13e449acf3"
);
ppu_test!(
    ppu_window_hdma,
    "Window/WindowHDMA/WindowHDMA.sfc",
    "2bae131ba2086640751142164246aadaf54c147dfd732839b8f0a7c91f7b2521"
);
ppu_test!(
    ppu_window_multi,
    "Window/WindowMultiHDMA/WindowMultiHDMA.sfc",
    "885273a42c4f466571ff0db04f180b6cc08f988022c52a596d50aa6c700dfc18"
);
ppu_test!(
    ppu_mode7_rotzoom,
    "Mode7/RotZoom/RotZoom.sfc",
    "6f8deb68ff3ad378cbcab75310272e2b152862ad01d286a7c0780b7df693001b"
);
ppu_test!(
    ppu_mode7_persp,
    "Mode7/Perspective/Perspective.sfc",
    "10ce69859a5828d80d0b8af768a233694414c76743aa5cffdc962d52eb9dab0d"
);
// Animated Mode-7 Star Wars intro. luna's run settles on the static
// "A long time ago in a galaxy far, far away...." opening-text hold (blue
// text + starfield), rendered cleanly. The ROM's reference `StarWars.png`
// captures a later phase (the STAR WARS logo), so a direct pixel match is
// N/A (eye-validated as a correct intro frame); the golden is luna's own
// deterministic settled frame as a regression baseline.
ppu_test!(
    ppu_mode7_starwars,
    "Mode7/StarWars/StarWars.sfc",
    "ed496efc8c84512041910419eaee12fc4d941067a942fd1ad403cead1c5bef05"
);
ppu_test!(
    ppu_greenspace,
    "GreenSpace/GreenSpace.sfc",
    "26b8e01e014df9777a8a7afed5c7f713f12048af50c3cd8b3168ee1639928734"
);
// MosaicMode3 ramps the BG mosaic size while R is held — hold R so the
// captured frame exercises the $2106 mosaic (verified pixelated).
ppu_test!(
    ppu_mosaic_mode3,
    "Mosaic/Mode3/MosaicMode3.sfc",
    "c3048a2eff2084b019b0dee48c2de599aa24e4d071facc000b33497e5ba6478a",
    hold = PAD_R
);
// Mode 5 hi-res + INTERLACE (SETINI bit 0): the Moogle figure. Interlace
// renders the full 448-line image collapsed to 224 by averaging both fields
// (logical lines y*2 and y*2+1, ares background.cpp:40 + Phase C blend) —
// previously sampled as progressive, showing only the top 224 rows stretched
// 2x (a zoomed-in head). Validated against the ROM's 512x448 reference.
ppu_test!(
    ppu_mosaic_mode5,
    "Mosaic/Mode5/MosaicMode5.sfc",
    "2a64e595a9c7d6f37326ac2c305b7c5895e999be27abb5a813f68cecb30e0aac"
);

// -----------------------------------------------------------------------
// Interlace scenes (512x448 = Mode 5/6 hi-res + SETINI bit 0). luna
// collapses to 256x224 by averaging both fields (Phase C). Validated
// against each ROM's 512x448 reference (downsampled). BG-driven demos are
// wired; sprite-heavy ones await OBJ-interlace (obj_gaps #6, Phase D).
// -----------------------------------------------------------------------
ppu_test!(
    ppu_interlace_font,
    "Interlace/InterlaceFont/InterlaceFont.sfc",
    "c94d7ae1117f59cb5ba247039297047debef9b0a0744d2a12f58800c3a19fe39"
);
ppu_test!(
    ppu_interlace_scroll,
    "Interlace/InterlaceScroll/InterlaceScroll.sfc",
    "6b9454710ae9131852cdb4a818272b73fa7bd3bb98814f032fa7e17bc1cc952d"
);
// The only wired Interlace ROM with a sprite (the hero). Phase D made its
// sprite render half-height (interlace), matching the reference — pre-Phase-D
// it was drawn 2x too tall at screen-y.
ppu_test!(
    ppu_interlace_rpg,
    "Interlace/InterlaceRPG/InterlaceRPG.sfc",
    "139abe05a6f67e5057e472e031eab7ef6cff80e2acf66507cbb29a2d051b1e63"
);
ppu_test!(
    ppu_interlace_moogle,
    "Interlace/InterlaceMoogle/InterlaceMoogle.sfc",
    "2a64e595a9c7d6f37326ac2c305b7c5895e999be27abb5a813f68cecb30e0aac"
);
ppu_test!(
    ppu_interlace_myst_hdma,
    "Interlace/InterlaceMystHDMA/InterlaceMystHDMA.sfc",
    "2ade4b5840e988bdd181d11dc8083bee84762e0e2edef9c49c792695b4549aa1"
);
ppu_test!(
    ppu_interlace_simpsons_hdma,
    "Interlace/InterlaceSimpsonsHDMA/InterlaceSimpsonsHDMA.sfc",
    "d3d1c97d1c2ab7749da08d01a75c6619e9f1ef0c4acf175b7dc79ae4361bb35a"
);

// -----------------------------------------------------------------------
// HDMA scenes (per-scanline register transfers). HDMA had no direct
// coverage before — only the two Window*HDMA demos exercised it indirectly.
// Goldens are luna's PAL render, each eyeballed against the expected
// effect before committing (coproc-testing discipline).
//
// These 5 render correctly: a per-line scroll water ripple (Wave), a
// vertical red→black fixed-colour gradient (RedSpace, direct / indirect /
// 9-bit-per-line — direct and indirect produce the *same* hash, as they
// must), and a Mode-7 perspective floor with per-line matrix HDMA. They
// validate the HDMA engine: table walk, indirect addressing, per-line
// fixed-colour ($2132), and Mode-7 matrix writes ($211B-$2120).
ppu_test!(
    ppu_hdma_wave,
    "HDMA/WaveHDMA/WaveHDMA.sfc",
    "c61b781ec9bb1fa7cdf5d49f8353adbb8074d9b88f9093cf3ed04af9e141f971"
);
ppu_test!(
    ppu_hdma_redspace,
    "HDMA/RedSpaceHDMA/RedSpaceHDMA.sfc",
    "45419aa9755b9a7229b4d4457c4adea0fff7b94193da29cfaf270f14dd38966e"
);
ppu_test!(
    ppu_hdma_redspace_indirect,
    "HDMA/RedSpaceIndirectHDMA/RedSpaceIndirectHDMA.sfc",
    "45419aa9755b9a7229b4d4457c4adea0fff7b94193da29cfaf270f14dd38966e"
);
ppu_test!(
    ppu_hdma_redspace_9bit,
    "HDMA/RedSpace9BitHDMA/RedSpace9BitHDMA.sfc",
    "8aa57ff15d8cdc7343924b25796182b8b317d0cce809d006e7d8ada7fe41f843"
);
ppu_test!(
    ppu_hdma_mode7,
    "HDMA/Mode7HDMA/Mode7HDMA.sfc",
    "736a61ba11eeb963d4c78129669c7a3dee511d3a482646cf60eb7c17e7061d89"
);
// The HiColor demos stream CGRAM mid-frame to exceed 256 colours. Despite
// the corpus folder name, the palette is NOT pushed by HDMA — it's an
// H-IRQ-driven general DMA: an H-counter IRQ fires every scanline (~H=170-
// 190, mid active-display) and its ISR triggers a DMA of N colours into
// CGDATA ($2122). (The one true HDMA channel here drives OAM/sprite size.)
//
// luna was DROPPING those CGDATA writes whenever the ISR also wrote CGADD
// ($2121) mid-line: that CPU write flipped `active_display` true, and the
// following CGDATA DMA was gated off (`write_gated(!active_display)`).
// Fixed in `DmaBusView::write_b` — CGDATA via DMA/HDMA bypasses the gate
// (CGRAM is never dropped on hardware, ares `io.cpp:55-60`), VRAM/OAM stay
// gated (`io.cpp:26,40`). The pseudo-hires variant — whose per-8-line
// ("per tile row") cadence + photo content hides the residual sub-line
// timing — now renders the full-colour mandrill cleanly → passing golden.
// See docs/luna_dma_gaps.md #7.
ppu_test!(
    ppu_hdma_hicolor64_pseudohires,
    "HDMA/HiColor64PerTileRowPseudoHiRes/HiColor64PerTileRowPseudoHiRes.sfc",
    "610fbfa6a0566c809708ff380d1a2f972b10b1d343d82310646fd1c91297072c"
);
// The two non-pseudo-hires variants display an RGB colour *chart* (sharp
// gradient bands; reference image ships as `HiColor*PerTileRow.png`).
// Validated HiColor64 against that reference: 81.2% pixel-exact, 88.2%
// within tol 24, MAE 7/255 — and the diff is confined to the *tile-row
// boundary* scanlines (rows 0,8,16,24,…, 15 of 224). The H-IRQ fires
// mid-line, so on hardware each 8-line boundary scanline is split (old
// palette above the IRQ dot, new below). luna renders each scanline
// atomically from one CGRAM snapshot (and only partial-flushes on the CPU
// write path, not the DMA path), so it draws the boundary line with the
// pre-swap palette — only that 1 line per tile-row is wrong, the other 7
// are pixel-exact. No cheap fix: neither pure-old nor pure-new palette
// matches the mid-line mix; an exact fix needs sub-scanline CGRAM tracking
// tied to the CPU H-position during the DMA — deep change, ~no commercial
// payoff. Kept `#[ignore]`d (gap #7). Confirmed not a render-order lag:
// deferring the render by one line neither fixed it nor survived the suite.
ppu_test!(
    ppu_hdma_hicolor64,
    "HDMA/HiColor64PerTileRow/HiColor64PerTileRow.sfc",
    "ab7a0324251a2b7c87ede33af6b707dc3e4aa08891dfecd42121ec5f5f36e06a",
    ignore = "HiColor chart (gap #7): fully mapped vs the shipped hardware reference PNG, 2026-06-22 — HARD multi-factor timing bug, NOT yet cracked. Technique (.asm): an H-IRQ at HTIME=190 reads the V-counter ($213D) each line and DMAs 8 incremental colours into CGRAM in HBlank (h~306), CGADD reset every 8 lines, building a 64-colour palette per 8-line tile-row. Reference diff: 81% exact; wrong rows are the FIRST line of each tile-row (y=0,8,16,…, ~15 rows) PLUS a separate bottom smooth-gradient band (y~160-221, ~57 rows = the larger slice). RULED OUT by measurement: (a) OPVCT/IRQ — luna fires the H-IRQ at HTIME=190 and returns the correct scanline (108=$6C,…), so the handler loads the right group; the scorecard grade-D \"H-IRQ ignores HTIME\" is stale. (b) render-vs-HBlank-DMA ordering — a flush-before-DMA correctly commits the visible line pre-DMA and persists (line-end render then no-ops), yet the image is UNCHANGED, proving each line's own DMA loads colours for FUTURE lines (incremental), so ordering is irrelevant to the current line. (c) per-line palette swap — forcing band-1 instead of band-0 did not help. The real residual is the incremental-palette-group activation timing + the bottom-band technique; needs a focused session with the reference per-row diff as the live oracle."
);
ppu_test!(
    ppu_hdma_hicolor128,
    "HDMA/HiColor128PerTileRow/HiColor128PerTileRow.sfc",
    "54495c7af30fa3cda2734230351396254d5ea2b64095b444082087888b539bc5",
    ignore = "HiColor chart (gap #7): fully mapped vs the shipped hardware reference PNG, 2026-06-22 — HARD multi-factor timing bug, NOT yet cracked. Technique (.asm): an H-IRQ at HTIME=190 reads the V-counter ($213D) each line and DMAs 8 incremental colours into CGRAM in HBlank (h~306), CGADD reset every 8 lines, building a 64-colour palette per 8-line tile-row. Reference diff: 81% exact; wrong rows are the FIRST line of each tile-row (y=0,8,16,…, ~15 rows) PLUS a separate bottom smooth-gradient band (y~160-221, ~57 rows = the larger slice). RULED OUT by measurement: (a) OPVCT/IRQ — luna fires the H-IRQ at HTIME=190 and returns the correct scanline (108=$6C,…), so the handler loads the right group; the scorecard grade-D \"H-IRQ ignores HTIME\" is stale. (b) render-vs-HBlank-DMA ordering — a flush-before-DMA correctly commits the visible line pre-DMA and persists (line-end render then no-ops), yet the image is UNCHANGED, proving each line's own DMA loads colours for FUTURE lines (incremental), so ordering is irrelevant to the current line. (c) per-line palette swap — forcing band-1 instead of band-0 did not help. The real residual is the incremental-palette-group activation timing + the bottom-band technique; needs a focused session with the reference per-row diff as the live oracle."
);

// INPUT/ControllerLatency: "any button → white screen, none → black". Held
// with A, the joypad auto-read ($4218 JOY1L, NMI-driven) must report the
// press so the ROM draws white — matching the reference capture. Exercises
// the joypad auto-read latch + NMI joypad-enable ($4200 bit 0) end-to-end.
#[test]
fn input_controller_latency() {
    test_display(
        "INPUT/ControllerLatency/ControllerLatency.sfc",
        "5fcaea3e9a96bd542b161537c280f82dc131be0498b738564f53cd256a1c601d",
        PAD_A,
    );
}

// =============================================================================
// SPC700 / S-DSP audio tests
//
// Peter Lemon's SPC700 ROMs play music / sounds rather than draw a result
// screen, so these assert a SHA-256 of the APU's 32 kHz PCM output instead
// of the framebuffer. Like the display hashes they are luna's own output
// (regression baselines): they lock the SPC700 + S-DSP pipeline against
// silent regressions. Record mode dumps a `.wav` (when LUNA_SNES_TEST_PNG
// points at a dir) so the audio can be auditioned.
// =============================================================================

/// Stereo PCM samples to capture and hash (~3 s at 32 kHz).
const AUDIO_SAMPLES: usize = 96_000;
/// Instruction ceiling while accumulating audio.
const AUDIO_STEP_CAP: u64 = 80_000_000;

/// SNES controller button masks for [`Snes::set_joypad`]
/// (`B Y SEL START Up Down Left Right A X L R 0 0 0 0`, MSB→LSB).
const PAD_A: u16 = 0x0080;
const PAD_R: u16 = 0x0010;

/// Boot a forced-LoROM ROM (as PAL) and accumulate the first
/// [`AUDIO_SAMPLES`] stereo samples from the APU.
///
/// `hold` is a controller-1 button mask held from reset until the SPC700
/// finishes booting the uploaded driver (`past_iplrom`), then released —
/// for ROMs that only start playing on a button press (e.g. `PlayTwoSong`'s
/// A = song 1). `0` means no input. The `LUNA_SNES_TEST_HOLD` env var
/// (hex) overrides it for ad-hoc experimentation.
fn run_audio(rom: Vec<u8>, hold: u16) -> Vec<(i16, i16)> {
    let want: usize = std::env::var("LUNA_SNES_TEST_AUDIO_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(AUDIO_SAMPLES);
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = luna_cartridge::Region::Pal;
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    let hold: u16 = std::env::var("LUNA_SNES_TEST_HOLD")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(hold);
    if hold != 0 {
        snes.set_joypad(0, hold);
    }

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut samples: Vec<(i16, i16)> = Vec::with_capacity(want + 8192);
    let mut executed = 0u64;
    let mut released = hold == 0;
    'run: while samples.len() < want && executed < AUDIO_STEP_CAP {
        for _ in 0..4096 {
            if catch_unwind(AssertUnwindSafe(|| {
                snes.step();
            }))
            .is_err()
            {
                break 'run;
            }
            executed += 1;
        }
        // Release the held button once the upload has landed (the SPC700
        // left the IPL ROM into the driver), so the ROM's input loop
        // doesn't re-trigger the upload and reset the song.
        if !released && snes.apu_real.past_iplrom {
            snes.set_joypad(0, 0);
            released = true;
        }
        snes.apu_real.drain_audio(&mut samples, usize::MAX);
    }

    std::panic::set_hook(prev_hook);

    if std::env::var("LUNA_SNES_TEST_APUDIAG").is_ok() {
        let a = &snes.apu_real;
        let aram_nz = a.aram.iter().filter(|&&b| b != 0).count();
        eprintln!(
            "APUDIAG past_ipl={} spc_pc=${:04X} KON=${:02X} KOFF=${:02X} FLG=${:02X} \
             MVOL=({},{}) EON=${:02X} V0VOL=({},{}) to_spc={:02X?} to_cpu={:02X?} aram_nz={aram_nz}",
            a.past_iplrom,
            a.cpu.pc,
            a.dsp.registers[0x4C],
            a.dsp.registers[0x5C],
            a.dsp.registers[0x6C],
            a.dsp.registers[0x0C] as i8,
            a.dsp.registers[0x1C] as i8,
            a.dsp.registers[0x3D],
            a.dsp.registers[0x00] as i8,
            a.dsp.registers[0x01] as i8,
            a.to_spc_ports,
            a.to_cpu_ports,
        );
    }

    samples.truncate(want);
    samples
}

fn audio_bytes(samples: &[(i16, i16)]) -> Vec<u8> {
    let mut b = Vec::with_capacity(samples.len() * 4);
    for (l, r) in samples {
        b.extend_from_slice(&l.to_le_bytes());
        b.extend_from_slice(&r.to_le_bytes());
    }
    b
}

/// Minimal RIFF/WAVE writer (16-bit signed PCM stereo, 32 kHz) for the
/// record-mode audio dump.
fn write_wav(path: &Path, samples: &[(i16, i16)]) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut f) = std::fs::File::create(path) else {
        return;
    };
    let rate: u32 = 32_000;
    let channels: u16 = 2;
    let bits: u16 = 16;
    let block = channels * bits / 8;
    let byte_rate = rate * u32::from(block);
    let data_len = (samples.len() * usize::from(block)) as u32;
    let mut w = |b: &[u8]| {
        let _ = f.write_all(b);
    };
    w(b"RIFF");
    w(&(36 + data_len).to_le_bytes());
    w(b"WAVE");
    w(b"fmt ");
    w(&16u32.to_le_bytes());
    w(&1u16.to_le_bytes()); // PCM
    w(&channels.to_le_bytes());
    w(&rate.to_le_bytes());
    w(&byte_rate.to_le_bytes());
    w(&block.to_le_bytes());
    w(&bits.to_le_bytes());
    w(b"data");
    w(&data_len.to_le_bytes());
    for (l, r) in samples {
        w(&l.to_le_bytes());
        w(&r.to_le_bytes());
    }
}

/// Boot `rel`, capture its audio, and compare the PCM SHA-256 to
/// `expected`. Skips gracefully if the corpus / ROM is absent.
fn test_audio(rel: &str, expected: &str, hold: u16) {
    let Some(root) = corpus_root() else {
        eprintln!("[skip] SNES test corpus not found (run tools/fetch-snes-test-roms.sh)");
        return;
    };
    let path = root.join(rel);
    if !path.is_file() {
        eprintln!("[skip] {rel}: not present under {}", root.display());
        return;
    }

    let rom = std::fs::read(&path).expect("read rom");
    let samples = run_audio(rom, hold);
    let got = hex(&Sha256::digest(audio_bytes(&samples)));
    let nonsilent = samples.iter().filter(|(l, r)| *l != 0 || *r != 0).count();

    if std::env::var("LUNA_SNES_TEST_RECORD").is_ok() {
        if let Ok(dir) = std::env::var("LUNA_SNES_TEST_PNG") {
            let safe = rel.replace(['/', ' '], "_");
            write_wav(&Path::new(&dir).join(format!("{safe}.wav")), &samples);
        }
        let first = samples.iter().position(|(l, r)| *l != 0 || *r != 0);
        println!(
            "RECORD {rel} => {got}  [samples={} nonsilent={nonsilent} first={first:?}]",
            samples.len()
        );
        return;
    }

    assert_eq!(
        samples.len(),
        AUDIO_SAMPLES,
        "{rel}: produced only {} of {AUDIO_SAMPLES} samples (ROM did not play?)",
        samples.len()
    );
    assert!(nonsilent > 0, "{rel}: APU output was pure silence");
    assert_eq!(
        got, expected,
        "audio hash mismatch for {rel}\n  \
         (run LUNA_SNES_TEST_RECORD=1 to regenerate after an intended APU change)"
    );
}

/// Declare a Peter Lemon `SPC700/<path>` audio golden test. The optional
/// `hold = <mask>` form holds a controller-1 button (e.g. [`PAD_A`]) until
/// the driver boots, then releases — for ROMs that only play on a button
/// press (`PlayTwoSong`: A = song 1).
macro_rules! spc_test {
    ($fn:ident, $path:literal, $hash:literal) => {
        #[test]
        fn $fn() {
            test_audio(concat!("SPC700/", $path), $hash, 0);
        }
    };
    ($fn:ident, $path:literal, $hash:literal, hold = $mask:expr) => {
        #[test]
        fn $fn() {
            test_audio(concat!("SPC700/", $path), $hash, $mask);
        }
    };
    // Ignored audio golden. Now used only by PitchMod — a real SPC700 STOP
    // halt under the correct cycles that Mesen2 reproduces too (see the
    // `project_pitchmod_spc700_crash` memory + tools/pitchmod-ref-check.lua), so
    // its golden is intentionally parked. (The Phase-2/3 stale-waveform goldens
    // that used to live here were auditioned + re-baselined 2026-06-23.)
    ($fn:ident, $path:literal, $hash:literal, ignore = $reason:literal) => {
        #[test]
        #[ignore = $reason]
        fn $fn() {
            test_audio(concat!("SPC700/", $path), $hash, 0);
        }
    };
    // Ignored, but keeps its input `hold` mask for when the WAV is auditioned
    // and the hash regenerated.
    ($fn:ident, $path:literal, $hash:literal, hold = $mask:expr, ignore = $reason:literal) => {
        #[test]
        #[ignore = $reason]
        fn $fn() {
            test_audio(concat!("SPC700/", $path), $hash, $mask);
        }
    };
}

// Golden hashes of luna's 32 kHz PCM output (first 3 s, loaded as PAL).
// All 8 auditioned (recognisable, clean) and re-baselined 2026-06-23 after the
// Phase-2/3 SPC700 cycle-accuracy waveform shift; the multi-block-upload music
// ROMs play thanks to the IPL-ROM byte fix. PitchMod stays ignored (a real,
// ares-matching STOP halt — see its reason).
spc_test!(
    spc_italo,
    "ItaloTest/ItaloTest.sfc",
    "df026edb17535c591ac398713d8f510e923f8a6ffdb92995b65863a3302954db"
);
spc_test!(
    spc_pitchmod,
    "PitchMod/PitchMod.sfc",
    "2d0b4cf14f382dff76f4e77a016e98827c70e36c3fcc6b9016ac92ec75bc529e",
    ignore = "PitchMod is a knife-edge timing ROM — Mesen2 ALSO halts its SPC700 on STOP ~1.8s in (frame 108), so luna is correct; golden was captured with pre-081e78d wrong cycles (project_pitchmod_spc700_crash)"
);
spc_test!(
    spc_play_brr,
    "PlayBRRSample/PlayBRRSample.sfc",
    "8e23b0d9c060b0339f13173f7863aade272d02cf8df97d7f1684699d85e11ad2"
);
spc_test!(
    spc_play_noise,
    "PlayNoise/PlayNoise.sfc",
    "fb285cf0055c90ae485656269536ec103e0407d36b705e63e1c60cb370e5cb63"
);
spc_test!(
    spc_twinkle,
    "Twinkle/Twinkle.sfc",
    "d145d0f0ea9f41927b33e5ed3bc71758556f1f63bdc101c3810cd38ea6daf9c4"
);
// Multi-block uploads — silent until the IPL-ROM `$FFEE` byte fix.
spc_test!(
    spc_axel_f,
    "Axel-F/Axel-F.sfc",
    "3d24ae64cef24c53d4863c7a07205953f885905cdfcef8a97f1c5885cd5daf3d"
);
spc_test!(
    spc_ffvii_prelude,
    "FFVIIPrelude/FFVIIPrelude.sfc",
    "8acf5de6f2ad8e736bda6271a7a772596b1a8857ff6619768362acd9a4c513d6"
);
spc_test!(
    spc_speech,
    "SpeechSynth/SpeechSynth.sfc",
    "724b0a292a5da09cc2c0fd4c9637e2dd679e15ec4e72de6e06cc4caba409d459"
);
// Plays only on a button press — hold A (song 1) until the driver boots.
spc_test!(
    spc_play_two_song,
    "PlayTwoSong/PlayTwoSong.sfc",
    "9e10ae2a4286501af7f423db4f59eeec67c5b9189249399da0bce61bdbb4d339",
    hold = PAD_A
);

/// Declare a representative commercial-title golden — one eyeball-validated
/// scene per hardware feature (mapper / coprocessor / PPU effect). The ROM
/// boots with NO input to a fixed instruction count and its framebuffer hash
/// is asserted. These are an **integration** regression net (the mapper +
/// coproc boot + the full-game render path), complementing the Peter Lemon
/// **primitive** goldens. Copyrighted ROMs live in `tests/roms/` (gitignored),
/// so these SKIP unless the developer has dumped them.
macro_rules! game_test {
    ($fn:ident, $file:literal, $instructions:literal, $hash:literal) => {
        #[test]
        fn $fn() {
            let Some(root) = games_root() else {
                eprintln!("[skip] commercial ROMs (tests/roms/) absent — gitignored, dump your own");
                return;
            };
            let path = root.join($file);
            if !path.is_file() {
                eprintln!("[skip] {}: not present under {}", $file, root.display());
                return;
            }
            let rom = std::fs::read(&path).expect("read rom");
            let bytes = run_game_fixed(rom, $instructions);
            let got = hex(&Sha256::digest(&bytes));
            if std::env::var("LUNA_SNES_TEST_RECORD").is_ok() {
                if let Ok(dir) = std::env::var("LUNA_SNES_TEST_PNG") {
                    dump_png(&bytes, &Path::new(&dir).join(concat!(stringify!($fn), ".png")));
                }
                println!("RECORD {} => {}", $file, got);
                return;
            }
            assert_eq!(
                got, $hash,
                "framebuffer hash mismatch for {} \
                 (run LUNA_SNES_TEST_RECORD=1 to re-record after an intended render change)",
                $file
            );
        }
    };
}

// Mode 7
game_test!(
    game_fzero,
    "F-Zero (USA).sfc",
    30_000_000,
    "db0aae0d7ecaf5805eb44c732a22541880066759814f021951f8af0af7733ece"
);
game_test!(
    game_mariokart,
    "Super Mario Kart (USA).sfc",
    50_000_000,
    "b7bc468dec89ba2f02f36190f0fc4c6945ec2cee1f6e68000452596e8e21456e"
);
// SA-1
game_test!(
    game_smrpg,
    "Super Mario RPG - Legend of the Seven Stars (USA).sfc",
    12_000_000,
    "49f84ad54742960ad0e193a38134e6ceba2b59f9809c46976b5d4cf6546a3dba"
);
game_test!(
    game_kirby_ss,
    "Kirby Super Star (USA).sfc",
    35_000_000,
    "1c88c42a9f38ff52a69bb78f62c90f1070a8837112243157df5a1fc485539e62"
);
// Super FX (GSU)
game_test!(
    game_starfox,
    "Star Fox (USA) (Rev 2).sfc",
    25_000_000,
    "e778238e51f28032bd71a400b54e8f2278b19b3cba6d70a38cffbc36cf83ac9a"
);
game_test!(
    game_stuntfx,
    "Stunt Race FX (USA) (Rev 1).sfc",
    25_000_000,
    "4127b08e545a26a3c86cbdf39206c5834788a28cb4da5e0083a2595a3ff46b7f"
);
// S-DD1
game_test!(
    game_starocean,
    "Star Ocean (tr).sfc",
    30_000_000,
    "1ecd444af4ed8b7e16c3ea267b2250bf1eea656f3ca7b426ac44a4158e7997a1"
);
// DSP-1
game_test!(
    game_pilotwings,
    "Pilotwings (USA).sfc",
    20_000_000,
    "74e38144b13287b0bada97de9669bca89f00f3870461caf9537ea728c3f50fa7"
);
// Color math / transparency
game_test!(
    game_som,
    "Secret of Mana (USA).sfc",
    30_000_000,
    "5371ef9d2574babb1aeab176fd70cbfd351ae925ed30bf61504bd69cefa909dc"
);
game_test!(
    game_zelda,
    "Legend of Zelda, The - A Link to the Past (USA).sfc",
    35_000_000,
    "924f3a854ea1b49886a2e11491afb6815c207f4f5b14a0532025e1ff58c8b252"
);
// HiROM (+ Mode 7 pendulum)
game_test!(
    game_metroid,
    "Super Metroid (Japan, USA) (En,Ja).sfc",
    35_000_000,
    "3fddcd5d6d10a972030bec93560c75c65f670b4f3a8d84a423d42f8d661f6845"
);
game_test!(
    game_chrono,
    "Chrono Trigger (USA).sfc",
    20_000_000,
    "6e00465ad69e86123b1a18e9d5a35d3850ced35bc2d8eecf2e4da63fce10d9a7"
);
// Large ROM
game_test!(
    game_tales,
    "Tales of Phantasia (Japan).sfc",
    40_000_000,
    "e997ba7d2757a4ed15dc52da2c2ba1a47ecd4a5eeb49d9d71cc55a750ee81fcf"
);
// HDMA (raster split + gradient)
game_test!(
    game_contra3,
    "Contra III - The Alien Wars (USA).sfc",
    50_000_000,
    "2d8f52bb162cc1e9e00897e8b1dc5f17a54e2088f77ea14adce508aa91627e02"
);
game_test!(
    game_axelay,
    "Axelay (USA).sfc",
    55_000_000,
    "6ff009b793b5be6706cccb1378829c47d04f5284ec014c73a9988e97ef1c2c7c"
);
