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
/// Frame budget for the settle-runner. Like the commercial-game runner (see
/// [`run_game_to_frame`]) the capture point is anchored to a FRAME, never to an
/// instruction count: a demo animates per frame, so frame N is the same picture
/// however cycle-exact the CPU's timing is, while a fixed instruction count
/// slides backwards through the ROM the moment that timing gets *more*
/// accurate. Most of these ROMs settle to a static screen long before this and
/// never reach it — for them it is only a ceiling.
const FRAME_CAP: u64 = 2300;

/// Hang guard for the settle-runner — never the thing that stops a healthy run.
/// It has to be generous: a ROM that busy-waits on `VBlank` (all five `CPUTest`
/// ROMs do) spends *more* instructions per frame as the emulation gets more
/// cycle-accurate, and the old 30M cap silently truncated them mid-run, leaving
/// goldens of half-drawn "BCC PASS / BCS PASS / BNE…" screens — throwing away
/// the very assertion those tests exist to make.
const STEP_CAP: u64 = 200_000_000;

/// Safety net for the frame-anchored commercial-game runs — a ROM that hangs
/// must not spin forever. It is deliberately generous: the *budget* is the
/// frame target (see [`run_game_to_frame`]), and this must never be what stops
/// a healthy run, or we are back to an instruction-indexed capture that slides
/// whenever the CPU's cycle timing improves.
const GAME_STEP_CAP: u64 = 200_000_000;
/// SPC700 ALU tests run every addressing mode before the pass/fail verdict
/// lands in the mailbox (ADC/SBC ~35M instructions), so they get a higher
/// ceiling than the framebuffer-settle tests (some of which intentionally
/// cap-out mid-animation and must keep their 30M frame).
const SPC700_STEP_CAP: u64 = 45_000_000;
/// Sample the framebuffer hash every this many instructions.
const SAMPLE_FRAMES: u64 = 30;

/// Instruction batch between mailbox polls in the SPC700 runner. That one reads
/// a `$2140` mailbox, not the framebuffer, and its ROMs signal completion
/// explicitly — so it has no settle heuristic to get wrong and stays in
/// instructions.
const SPC_POLL_EVERY: u64 = 100_000;
/// Consecutive identical samples that count as "settled".
const STABLE_SAMPLES: u32 = 8;

// The settle window is therefore SAMPLE_FRAMES * STABLE_SAMPLES = 240 frames,
// i.e. four seconds of unchanged picture. It has to be wide, because "the
// screen has not changed" is a weak signal: a ROM that is merely PAUSED between
// two printed lines looks exactly like one that has finished. The `CPUTest`
// ROMs wait for VBlank, do a VRAM DMA, then compute — and once the DMA is
// charged its real cost (the DRAM refresh halts it, as on hardware) that work
// no longer fits in one VBlank, so they lose a frame per line and their pauses
// stretch well past a second. A narrow window mistakes that for completion and
// freezes a half-drawn "BCC PASS / BCS PASS / BNE…" screen as the golden.
//
// NOTE on both of the above: the settle criterion is measured in FRAMES, not
// instructions, and that is not cosmetic. It used to sample every 100k
// instructions and call the ROM settled after 8 unchanged samples. But a ROM
// that busy-waits on VBlank executes MORE instructions per frame as the
// emulation gets more cycle-accurate — so 100k instructions buys fewer and
// fewer frames, the stability window shrinks in real time, and a ROM that is
// merely PAUSED between two printed lines looks finished. That is exactly how
// the five `CPUTest` goldens ended up as half-drawn "BCC PASS / BCS PASS /
// BNE…" screens. In frames the window is what it says it is, whatever the CPU
// timing does.

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
/// `region` picks the video standard, and it is a per-family decision:
///
/// - The PPU demos run as **PAL**, matching the `twvd/siena` convention and
///   krom's reference captures.
/// - The `CPUTest` family runs as **NTSC**. It used to be PAL too, on the
///   theory that its result table only fits inside PAL's longer V-blank —
///   but that was calibrated against luna's old, too-fast boot (the DRAM
///   refresh was not charged during DMA). With cycle-exact timing the truth
///   is the opposite, and **Mesen2 agrees on both counts**: in PAL the write
///   burst overruns V-blank and the table is truncated mid-row (Mesen2's PAL
///   screen is pixel-identical to luna's, last VRAM write within 2 master
///   clocks), while in NTSC both emulators render the full all-PASS table.
///   A truncated table has no assertion value; NTSC keeps it, and the final
///   screen being static makes the golden timing-invariant.
fn run_to_stable(rom: Vec<u8>, hold: u16, region: luna_cartridge::Region) -> Vec<u8> {
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = region;
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
    'run: while executed < STEP_CAP && snes.frame_count < FRAME_CAP {
        let sample_at = snes.frame_count + SAMPLE_FRAMES;
        while snes.frame_count < sample_at && executed < STEP_CAP {
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

/// Boot a commercial ROM (auto-detected mapper + native region) with no input,
/// run to **frame `frames`**, and return the framebuffer. These scenes animate
/// and never settle, so the capture point has to be pinned to something.
///
/// It is pinned to a FRAME, not to an instruction count, and that distinction
/// is the whole point. A SNES game advances its logic once per frame (its NMI
/// handler), so its state at frame N is the same no matter how cycle-exact the
/// emulator's timing is. An instruction count is not: make the CPU spend more
/// master clocks per instruction — i.e. make it *more* accurate — and a
/// fixed-instruction capture slides backwards through the game. That made the
/// golden suite penalise every accuracy improvement, and it silently truncated
/// the `CPUTest` ROMs mid-run (they wait on `VBlank`, so a longer frame costs
/// them busy-wait instructions rather than progress). Frame-anchored, the
/// captures survive timing work; only a real behaviour change moves them.
///
/// `GAME_STEP_CAP` is a safety net for a ROM that hangs, nothing more.
fn run_game_to_frame(rom: Vec<u8>, frames: u64) -> Vec<u8> {
    let cart = Cartridge::from_bytes(rom).expect("auto-detect cartridge");
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut executed = 0u64;
    while snes.frame_count < frames && executed < GAME_STEP_CAP {
        if snes.cpu.stopped {
            break;
        }
        if catch_unwind(AssertUnwindSafe(|| snes.step())).is_err() {
            break;
        }
        executed += 1;
    }
    std::panic::set_hook(prev_hook);
    // Stopping the instant the counter ticks over leaves the framebuffer
    // holding the frame that just completed — a whole, coherent picture,
    // where a mid-frame stop caught a mix of two.
    fb_bytes(&snes)
}

/// Boot `rel` (relative to the corpus root), settle, and compare the
/// framebuffer SHA-256 to `expected`. Skips gracefully if the corpus or
/// the specific ROM is absent.
fn test_display(rel: &str, expected: &str, hold: u16, region: luna_cartridge::Region) {
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
    let bytes = run_to_stable(rom, hold, region);
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
                luna_cartridge::Region::Ntsc,
            );
        }
    };
}

/// Issue #115: the native 512×448 capture, end to end. Boots `InterlaceFont`
/// (mode 5 hi-res + SETINI interlace — the exact class the feature exists
/// for), runs it to its settled font grid with capture on, and pins the
/// SHA-256 of the native buffer. krom ships a real-hardware 512×448 PNG of
/// this screen; luna's native frame measures 95.9 % pixel-exact against it
/// (the rest is a uniform few-LSB colorimetric offset from the capture
/// chain), so this hash is a true exact-resolution baseline.
#[test]
fn ppu_interlace_font_native_512x448() {
    let Some(root) = corpus_root() else {
        eprintln!("[skip] SNES test corpus not found");
        return;
    };
    let path = root.join("PPU/Interlace/InterlaceFont/InterlaceFont.sfc");
    let Ok(rom) = std::fs::read(&path) else {
        eprintln!("[skip] {} absent", path.display());
        return;
    };
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = luna_cartridge::Region::Pal;
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.ppu.set_native_capture(true);
    while snes.frame_count < 120 {
        snes.step();
    }
    assert_eq!(
        snes.ppu.native_framebuffer.len(),
        512 * 448,
        "native buffer must be 512x448"
    );
    let mut bytes = Vec::with_capacity(512 * 448 * 3);
    for px in &snes.ppu.native_framebuffer {
        bytes.extend_from_slice(px);
    }
    let got = hex(&Sha256::digest(&bytes));
    if std::env::var("LUNA_SNES_TEST_RECORD").is_ok() {
        println!("RECORD native InterlaceFont => {got}");
        return;
    }
    assert_eq!(
        got, "e0d00e0a5af0003a8dc6323d1a04880fdbd96f86ce8314e4118f50e9093db681",
        "native 512x448 InterlaceFont hash moved — re-record after an intended render change"
    );
}

// Golden hashes captured from luna's renderer (loaded as NTSC — see
// `run_to_stable` for why the CPUTest family is NTSC where the PPU demos are
// PAL). All 23 render the correct all-PASS result screen.
cpu_test!(
    cpu_adc,
    "ADC",
    "b3adb5fba9c957ae762713dff0eeee8a83cb43d02e1c3649d63737a6915debda"
);
cpu_test!(
    cpu_and,
    "AND",
    "2243f54469fb08c7238b5b0a6ceeb22f8cf79d220fdc16e505c540ebbb147460"
);
cpu_test!(
    cpu_asl,
    "ASL",
    "475a18346e09be4c30bdfa28c3b30a877c85037c69e66bdebc1856ceae4c1ed9"
);
cpu_test!(
    cpu_bit,
    "BIT",
    "f2e5cb5b2fe13083aa1c6c1ec8f8905bdf9ac1c4f26be470ed5753fe55c65174"
);
cpu_test!(
    cpu_bra,
    "BRA",
    "9c2a6d06fb317ec256f5f5f59b437e3717eb66282d7ba6d3c47d78b6c9d85211"
);
cpu_test!(
    cpu_cmp,
    "CMP",
    "9375f7a8b4e38c5629feb87b2055bca84f67cf514da1526b5797fcb90f50a9a7"
);
cpu_test!(
    cpu_dec,
    "DEC",
    "3249f51754297d45a47da17b1b29c2f5fafe6ecde62e0e194b78e1ac20439914"
);
cpu_test!(
    cpu_eor,
    "EOR",
    "f8c17f1ff716af939869167e249a4c27002d4d36c1774ff0241fccbcd6361a9d"
);
cpu_test!(
    cpu_inc,
    "INC",
    "6e5bbea05a36f019c93687bc830ea2c6ee5e88e03e63e9ac059fc95c5ee08c4d"
);
cpu_test!(
    cpu_jmp,
    "JMP",
    "776340215a96509489bc17b394fa0f3971dc73790f1c126961a5704e4b80e68d"
);
cpu_test!(
    cpu_ldr,
    "LDR",
    "3c8bbf8285672ca6d5f95fded62377d0da4b0fabe418b6464e0ce25049a39955"
);
cpu_test!(
    cpu_lsr,
    "LSR",
    "9db3f1b7a8b20a505e164db67ce08ce357ff79420cd7b1c3ca63bf6040cc7529"
);
cpu_test!(
    cpu_mov,
    "MOV",
    "e4ac2da27999979f098956f8ffbf72ff65691d24ce78370cf88c4b3c3352f4ab"
);
cpu_test!(
    cpu_msc,
    "MSC",
    "5a64e3067d54d0351ff8c84194e5530a306fd7e2b3cbcf3c75aec329b5343485"
);
cpu_test!(
    cpu_ora,
    "ORA",
    "5fac6b67e578658a961b6318f3d15e2420c736b3a58f99250302062d03b3fa0c"
);
cpu_test!(
    cpu_phl,
    "PHL",
    "fb03b6271b4a927e075d2b6aa2a7084cebfe84449500f5ac8eaae2df1d64946d"
);
cpu_test!(
    cpu_psr,
    "PSR",
    "faf91c2c0c1d620eb8468324f87b809273f176fd0f4dc98e22b4f110dce0e8c6"
);
cpu_test!(
    cpu_ret,
    "RET",
    "75f9ef7fbd98dc1843d6748186cad3d0f3259685eb5e520bc6965bf7108ff8f6"
);
cpu_test!(
    cpu_rol,
    "ROL",
    "ceb99cb521187cbb4b9c634d728fb0fd6d85bf853a2e166e65702fa827164129"
);
cpu_test!(
    cpu_ror,
    "ROR",
    "3ea40b0d84bec054c167412bbde562de78088c5ff574e3de061c4b1b16a84000"
);
cpu_test!(
    cpu_sbc,
    "SBC",
    "329baf59349fc52b4ce148281884b5bfdf630ff15b75f1caf0311deb635f2d4a"
);
cpu_test!(
    cpu_str,
    "STR",
    "4968af4f04d840ed40aa4c7aee44808f4782e050ec7fe5255e9974fcacbe00da"
);
cpu_test!(
    cpu_trn,
    "TRN",
    "1aa58b7e0202d88c4fb40298b160ffd080f5acb323c058f1cd687658b2f09716"
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
        for _ in 0..SPC_POLL_EVERY {
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
            test_display(
                concat!("PPU/", $path),
                $hash,
                0,
                luna_cartridge::Region::Pal,
            );
        }
    };
    // `hold = <mask>` holds a controller-1 button for the whole run — for
    // demos driven by input (the Mosaic demos ramp the mosaic size while R
    // is held).
    ($fn:ident, $path:literal, $hash:literal, hold = $mask:expr) => {
        #[test]
        fn $fn() {
            test_display(
                concat!("PPU/", $path),
                $hash,
                $mask,
                luna_cartridge::Region::Pal,
            );
        }
    };
    // A scene luna renders wrong (tracked PPU gap). `#[ignore]`d, with the
    // committed hash characterising the current (wrong) output — once the
    // gap is fixed the render changes, the `--ignored` run goes red.
    ($fn:ident, $path:literal, $hash:literal, ignore = $reason:literal) => {
        #[test]
        #[ignore = $reason]
        fn $fn() {
            test_display(
                concat!("PPU/", $path),
                $hash,
                0,
                luna_cartridge::Region::Pal,
            );
        }
    };
}

// Curated PPU scenes (the twvd/siena selection): BG maps, hi-colour
// blending, windows, and Mode 7. Golden hashes are luna's own PAL render.
ppu_test!(
    ppu_bg1_2bpp,
    "BGMAP/8x8/2BPP/8x8BG1Map2BPP32x328PAL/8x8BG1Map2BPP32x328PAL.sfc",
    "2f5a6b5b2430be80963b02345f9d7939b9dba12b7367fe2fa04231843cb792fc"
);
ppu_test!(
    ppu_bg2_2bpp,
    "BGMAP/8x8/2BPP/8x8BG2Map2BPP32x328PAL/8x8BG2Map2BPP32x328PAL.sfc",
    "b1c8ed442d709dc26ce49d4ee0aab8146459b90765804cd3c332ffc85286f6e4"
);
ppu_test!(
    ppu_bg3_2bpp,
    "BGMAP/8x8/2BPP/8x8BG3Map2BPP32x328PAL/8x8BG3Map2BPP32x328PAL.sfc",
    "b1c8ed442d709dc26ce49d4ee0aab8146459b90765804cd3c332ffc85286f6e4"
);
ppu_test!(
    ppu_bg4_2bpp,
    "BGMAP/8x8/2BPP/8x8BG4Map2BPP32x328PAL/8x8BG4Map2BPP32x328PAL.sfc",
    "b1c8ed442d709dc26ce49d4ee0aab8146459b90765804cd3c332ffc85286f6e4"
);
ppu_test!(
    ppu_bg_4bpp,
    "BGMAP/8x8/4BPP/8x8BGMap4BPP32x328PAL/8x8BGMap4BPP32x328PAL.sfc",
    "e5deb17973db08bd6fca4262925dfec1e3016629500e9f1e1ea2f3895699bdc4"
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
// Re-baselined 2026-07-13 — see the WaitNMI note on `ppu_hdma_wave` (#107).
ppu_test!(
    ppu_bg_8bpp_32x32,
    "BGMAP/8x8/8BPP/32x32/8x8BGMap8BPP32x32.sfc",
    "b5467e295995c8ad193b81cefb17fa191ab14b6a66b471f3c09ab53ac9ca21fc"
);
ppu_test!(
    ppu_bg_8bpp_32x64,
    "BGMAP/8x8/8BPP/32x64/8x8BGMap8BPP32x64.sfc",
    "4bbb0bdb9f88d5e2d08d76f2dd73419102436d6b07a5d7d46097aa28c0486d48"
);
ppu_test!(
    ppu_bg_8bpp_64x32,
    "BGMAP/8x8/8BPP/64x32/8x8BGMap8BPP64x32.sfc",
    "4bbb0bdb9f88d5e2d08d76f2dd73419102436d6b07a5d7d46097aa28c0486d48"
);
ppu_test!(
    ppu_bg_8bpp_64x64,
    "BGMAP/8x8/8BPP/64x64/8x8BGMap8BPP64x64.sfc",
    "4bbb0bdb9f88d5e2d08d76f2dd73419102436d6b07a5d7d46097aa28c0486d48"
);
ppu_test!(
    ppu_bg_8bpp_tileflip,
    "BGMAP/8x8/8BPP/TileFlip/8x8BGMapTileFlip.sfc",
    "b4bcc1f52aaef003a8080deafc89a6db90eafa92748eb048b83a5bbf908cc8cd"
);
ppu_test!(
    ppu_rings,
    "Rings/Rings.sfc",
    "ad85874fc9cca1779ea17619bdf34cdaa141e158f8237f49848147715915b82f"
);
ppu_test!(
    ppu_hicolor_dlair,
    "Blend/HiColor/HiColor1241DLair/HiColor1241DLair.sfc",
    "d8652d4c5692d49e533d25602d071f337c0f8361910c6a2806e3811c26106999"
);
ppu_test!(
    ppu_hicolor_3840,
    "Blend/HiColor/HiColor3840/HiColor3840.sfc",
    "7e955d6dd9fe2a5c87a71b3a27c3f9733f828c317d8f7c72b662a054af0342a7"
);
ppu_test!(
    ppu_hicolor_myst,
    "Blend/HiColor/HiColor575Myst/HiColor575Myst.sfc",
    "992bdd7a70664c196df6b50bd9d1bf224369d97ba07138c6a70855b8f350b228"
);
ppu_test!(
    ppu_window_hdma,
    "Window/WindowHDMA/WindowHDMA.sfc",
    "43fa9c46d4d27cfd63c94fd668be369514fb1aa87e4062494ffcaa588986ad2e"
);
ppu_test!(
    ppu_window_multi,
    "Window/WindowMultiHDMA/WindowMultiHDMA.sfc",
    "d960958706735e07da9cdc504c9a9f6b868770e893d6e3a7b504838f88238876"
);
ppu_test!(
    ppu_mode7_rotzoom,
    "Mode7/RotZoom/RotZoom.sfc",
    "76b76eeb6a096a180e2acaabb6c8b16d228cd983b396af2dc2e20f8f93ceb04e"
);
ppu_test!(
    ppu_mode7_persp,
    "Mode7/Perspective/Perspective.sfc",
    "d87d90b83e244ac20d248ee6376936e5d15ed0573e909ddcaf2744932347c1e3"
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
    "be60a30fcda54db1f495b121b558cf0987621f1648fd377bb5237b1cbf5631c0"
);
ppu_test!(
    ppu_greenspace,
    "GreenSpace/GreenSpace.sfc",
    "26b8e01e014df9777a8a7afed5c7f713f12048af50c3cd8b3168ee1639928734"
);
// MosaicMode3 ramps the BG mosaic size while R is held — hold R so the
// captured frame exercises the $2106 mosaic (verified pixelated).
// Re-baselined 2026-06-23 (P1 faithful nmiLine): clearing $4210 at VBlank end
// shifts this timing-sensitive ramp demo by ~1 frame, so the fixed -n lands a
// little further along. Old (c3048a2e) and new (df5e17e0) frames both eyeball-
// confirmed clean 8×8 mosaics of the same lake/island scene (the new one is
// slightly more detailed) — a benign frame-shift, not a render regression.
// Re-baselined 2026-07-13 (#107) — see the WaitNMI note on `ppu_hdma_wave`.
// The correct 1x loop rate puts this back on c3048a2e, the very frame this
// test baselined against before the 2026-06-23 nmiLine shift above.
ppu_test!(
    ppu_mosaic_mode3,
    "Mosaic/Mode3/MosaicMode3.sfc",
    "9424df0b5273fd06c961a6c57ff64b81949f0f39fff5d947917761f6bab93b8b",
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
    "fdec5062bbc5532825b7a34011970cbed12ddc70ca6106096f668bd54e0cc14d"
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
    "a22e716dda80673a147256ab326e57148f2a2de98081adf570cd1bbf68137da4"
);
ppu_test!(
    ppu_interlace_scroll,
    "Interlace/InterlaceScroll/InterlaceScroll.sfc",
    "42715721ffa92227169211e8cd0fabdbab3a994d555dfb8b7976f1b421012d8f"
);
// The only wired Interlace ROM with a sprite (the hero). Phase D made its
// sprite render half-height (interlace), matching the reference — pre-Phase-D
// it was drawn 2x too tall at screen-y.
ppu_test!(
    ppu_interlace_rpg,
    "Interlace/InterlaceRPG/InterlaceRPG.sfc",
    "e56b22a580bec9f8e3a665b712d0092ffaa25418ee117beaad77e982b88917ce"
);
ppu_test!(
    ppu_interlace_moogle,
    "Interlace/InterlaceMoogle/InterlaceMoogle.sfc",
    "fdec5062bbc5532825b7a34011970cbed12ddc70ca6106096f668bd54e0cc14d"
);
ppu_test!(
    ppu_interlace_myst_hdma,
    "Interlace/InterlaceMystHDMA/InterlaceMystHDMA.sfc",
    "2dc7bea4e849e7959243246d6ebe5cd73e990efe6d2ff9de393d8870d2f7139a"
);
ppu_test!(
    ppu_interlace_simpsons_hdma,
    "Interlace/InterlaceSimpsonsHDMA/InterlaceSimpsonsHDMA.sfc",
    "ada7fa4f77366ca6118d6a8ab3e21326375ea7105aeffb3166679fe352ae1cbd"
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
// Re-baselined 2026-07-14 with the frame-anchored settle runner (`FRAME_CAP`):
// these three never settle, so they used to be captured wherever a fixed
// instruction count happened to land. They are now captured at frame 2300 —
// same demos, correct animation phase, eyeball-confirmed (the water wave, the
// lake mosaic, the 8BPP cathedral).
//
// Re-baselined 2026-07-13 (issue #107, RDNMI visibility window): these three
// demos idle on the corpus' `WaitNMI` macro (`BIT $4210 / BPL`), which used to
// pass TWICE per VBlank whenever its read landed in the first clocks of the
// VBlank scanline — luna handed the flag back set there but could not clear it.
// They therefore animated ~4/3x too fast. At the same fixed instruction budget
// the correct 1x rate lands on a different animation phase; all three were
// eyeball-confirmed clean (wave: same water, other phase; mosaic: same lake
// scene, a coarser step of its ramp — and back to the c3048a2e frame this test
// baselined against before the 2026-06-23 nmiLine shift; 8BPP: same tilemap, a
// different scroll offset). Verified against a Mesen2 headless trace: one pass
// per frame, HDMA table pointer +3/frame.
ppu_test!(
    ppu_hdma_wave,
    "HDMA/WaveHDMA/WaveHDMA.sfc",
    "a69f3e647f821b823a16a30b23069ee987cba858a99f9dff61c0fb6d87c532f5"
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
    "0178471f15cdac7ad2c10ef5b532c2a6d3ef2d6c6d2646e95f76381c86f9d383"
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
    "91ab0f56a02343f9936b9e37222b6802bbab475b229d8ca70e6dbd379d4c6dd1"
);
// The two non-pseudo-hires variants display an RGB colour *chart* (sharp
// gradient bands; reference image ships as `HiColor*PerTileRow.png`).
//
// GAP #7 — CRACKED 2026-07-26. The 19-percent diff was never a CGRAM-timing
// bug: the full luna-vs-Mesen2 write timeline (values, lines, order) was
// byte-identical (1 benign vblank CGADD skew in 3598 events). The real cause
// was the FRAMEBUFFER LINE ORIGIN: hardware displays PPU lines 1..=224 (fb
// row r is scanned during line r+1; line 0 is the pre-render line), while
// luna mapped line L to row L — invisible on static screens, one full row
// off the moment the palette changes every line. Fixed by the hardware line
// origin (`flush_partial_scanline_inner` writes line V to row V-1), the
// DMA-path partial flush (a B-bus CGRAM write mid-line commits the
// in-progress row with the pre-write palette, like the CPU path), and the
// HDMA end-of-line application point. HiColor64 is now PIXEL-EXACT against
// the hardware reference PNG (0/57344), along with 15 other corpus refs
// (WindowHDMA, Mode7HDMA, Perspective, Rings, HiColor3840, HiColor575Myst,
// the BGMap family, ...).
ppu_test!(
    ppu_hdma_hicolor64,
    "HDMA/HiColor64PerTileRow/HiColor64PerTileRow.sfc",
    "5b3439273c97532f00b1c233d3423b05d4ef04b6a6fa79153f7254512ed086dd"
);
ppu_test!(
    ppu_hdma_hicolor128,
    "HDMA/HiColor128PerTileRow/HiColor128PerTileRow.sfc",
    "24f07ecd1ef6839b042d15fce801340720b9dc1d8b7678b315678fa3da2214f5",
    ignore = "HiColor128 residual (gap #7b): 91.0% vs the hardware reference after the line-origin + HDMA-phase fixes (was 83.7%). MEASURED 2026-07-26, second pass with the NMI-vector clock calibration (the FIRST pass's ~200-clock IRQ-entry-skew lead was an ARTIFACT of comparing uncalibrated master-clock line phases across emulators — deltas-not-absolutes; after calibrating on the $FFEA fetch position, luna's IRQ raise, vector fetch, handler length AND $420B/CGRAM-burst positions are all cycle-aligned with Mesen2, and the burst is post-visible on hardware too). The REAL residual signature: in the bottom half, every SECOND 8-line tile-row band (y 168-175, 184-191, 200-207, 216-223) renders exactly ONE LINE EARLY (band content matches the reference at local shift -1 within 256 px), plus rows 95/111 — an alternating per-tile-row palette-group parity, likely in how the every-16-lines CGADD reset interacts with which batch each band's first row samples. CGRAM write timeline itself is byte-identical to Mesen2."
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
        luna_cartridge::Region::Pal,
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
    "9f3cc4abf78e16acd6d69a3147e303887ec653ababa827e49a92d233932673b0"
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
    "a47bc23f14447de6111a7c128b349833099964d2224f09000200a3cfd4ee02ee"
);
spc_test!(
    spc_play_noise,
    "PlayNoise/PlayNoise.sfc",
    "124decc81ceb450910e076396f3e77ea10d9f101e1a08d351ee73b9ff7ad51b2"
);
spc_test!(
    spc_twinkle,
    "Twinkle/Twinkle.sfc",
    "77183a84670e3e32f77b7b33b6816104a8be8a8e2ccd35d537e9f3930312283b"
);
// Multi-block uploads — silent until the IPL-ROM `$FFEE` byte fix.
spc_test!(
    spc_axel_f,
    "Axel-F/Axel-F.sfc",
    "26c62a40fafa3dbe24664f9defc0eef0572122c35d0779918b9b87d67acddb28"
);
spc_test!(
    spc_ffvii_prelude,
    "FFVIIPrelude/FFVIIPrelude.sfc",
    "2d16c154dc5a24e9a135a725810685370b918b39a6a8a3665cc18d5f40d095c7"
);
spc_test!(
    spc_speech,
    "SpeechSynth/SpeechSynth.sfc",
    "e455e6e5d6423a76899f4fe68b0d3c90e1d770c566e2c684a79afd42e2adfe2d"
);
// Plays only on a button press — hold A (song 1) until the driver boots.
spc_test!(
    spc_play_two_song,
    "PlayTwoSong/PlayTwoSong.sfc",
    "619879848ba540f89c2c103510b9f8e956a96988791186bdd1b762c37926eb91",
    hold = PAD_A
);

/// Declare a representative commercial-title golden — one eyeball-validated
/// scene per hardware feature (mapper / coprocessor / PPU effect). The ROM
/// boots with NO input to a fixed instruction count and its framebuffer hash
/// is asserted. These are an **integration** regression net (the mapper +
/// coproc boot + the full-game render path), complementing the Peter Lemon
/// **primitive** goldens. Copyrighted ROMs live in `tests/roms/` (gitignored),
/// so these SKIP unless the developer has dumped them.
// Re-baselined 2026-07-14 with the frame-anchored runner (see
// `run_game_to_frame`): the capture now stops on a frame boundary instead of
// wherever a fixed instruction count happened to land mid-frame, so five of
// these hold a whole picture where they used to hold a mix of two. Same scenes,
// eyeball-confirmed (F-Zero mid-race, SMRPG's Peach-in-the-garden intro, Kirby
// in play, Star Fox's 3D intro, Tales' forest). `game_starfox` had been red on
// develop for a while — its golden predated a render change and is refreshed
// here too.
macro_rules! game_test {
    ($fn:ident, $file:literal, $frames:literal, $hash:literal) => {
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
            let bytes = run_game_to_frame(rom, $frames);
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
    2226,
    "e75992a5e9d55fb30cb8349a1796ab1f860196f896265f255fafaaf09f94b669"
);
game_test!(
    game_mariokart,
    "Super Mario Kart (USA).sfc",
    3587,
    "aa62f60e42226041fef35dd20bb9642e9deec009b936129811f4eaba1d76f038"
);
// SA-1
game_test!(
    game_smrpg,
    "Super Mario RPG - Legend of the Seven Stars (USA).sfc",
    905,
    "65d4908b5d63fe0fc8afb8f69b5dae6643f52377e73ec5d2d726b44dd942e8ac"
);
game_test!(
    game_kirby_ss,
    "Kirby Super Star (USA).sfc",
    3054,
    "a4184f024deaa411b30f65621bff893505e6b1b6782dff3c843019b3e8f3ee1a"
);
// Super FX (GSU)
game_test!(
    game_starfox,
    "Star Fox (USA) (Rev 2).sfc",
    1939,
    "f2898571d973c0c265b27f21ac3d186f8b78cbcf05b688127bfaa9bd4413e1e3"
);
game_test!(
    game_stuntfx,
    "Stunt Race FX (USA) (Rev 1).sfc",
    2299,
    "4ccf73b8c336054676bb48c8dc668c81fddc4123fb2630bf15538e35d79d2008"
);
// S-DD1
game_test!(
    game_starocean,
    "Star Ocean (tr).sfc",
    1875,
    "fa9e3f45b35f8331d9f3aa5a0064a2eaf1ab3d27a6b2b91ccd32784eca976f17"
);
// DSP-1
game_test!(
    game_pilotwings,
    "Pilotwings (USA).sfc",
    1395,
    "6a05fbbe26b4692619b7f66838be6bdefbd7f28acd6e899b14b4fd3c7f56d80f"
);
// Color math / transparency
game_test!(
    game_som,
    "Secret of Mana (USA).sfc",
    2307,
    "ba2f10628fb5751a835495e33f7243075958410275c112cc993ab41db278b338"
);
game_test!(
    game_zelda,
    "Legend of Zelda, The - A Link to the Past (USA).sfc",
    2631,
    "59233337f7cb7152ce1dc6e78b4e40aa53701439b5485198fd45a50edcc4d154"
);
// HiROM (+ Mode 7 pendulum)
game_test!(
    game_metroid,
    "Super Metroid (Japan, USA) (En,Ja).sfc",
    2303,
    "c0a2ecaadc4020e95d74b41c40275214ac0cda55fea07955bb2e407ae1ea2e0a"
);
game_test!(
    game_chrono,
    "Chrono Trigger (USA).sfc",
    1870,
    "18aabf052b1ed53c3a8d6a73614f073059b48797ad3a551fabd75a21ec0d72d9"
);
// Large ROM
game_test!(
    game_tales,
    "Tales of Phantasia (Japan).sfc",
    2574,
    "959e2bff422f3c6fb6b89521290009adf5779c34c9783f62d70931e908132573"
);
// HDMA (raster split + gradient)
game_test!(
    game_contra3,
    "Contra III - The Alien Wars (USA).sfc",
    4573,
    "a428991c2a453b533db6f00c732f958cc56ae5ba9932d3a7dc8112fee18db22b"
);
game_test!(
    game_axelay,
    "Axelay (USA).sfc",
    4801,
    "2425cfc89488d6ca003b3a9af5a79edb316c0b0828d0af66e92c6b6f270cc734"
);
