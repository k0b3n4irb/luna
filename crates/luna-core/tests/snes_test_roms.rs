//! SNES test-ROM golden suite (Peter Lemon hardware tests + homebrew).
//!
//! The ROM corpus is **not vendored** (it's large): it is checked out into
//! `.luna_tests` at the root of the repository (git-ignored). Each test
//! boots a ROM, runs it until the 256×224 framebuffer settles, and
//! asserts a SHA-256 of that framebuffer against a committed golden hash.
//!
//! ## Setup
//!
//! ```bash
//! tools/fetch-snes-test-roms.sh        # sparse-clone into .luna_tests
//! cargo test -p luna-core --test snes_test_roms
//! ```
//!
//! Or point `LUNA_SNES_TEST_DIR` at a corpus root. If the corpus is
//! absent, every test prints a skip notice and passes — so `cargo test`
//! works with or without the checkout. A skip is therefore NOT a pass:
//! set `LUNA_SNES_TEST_REQUIRE=1` (CI does) to turn a missing corpus ROM
//! into a failure, and `LUNA_GAME_TEST_REQUIRE=1` to do the same for the
//! commercial ROMs under `tests/roms/` (local pre-release check).
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
//!
//! Only the value `1` records, and recording with a `*_REQUIRE` variable
//! set is refused (see `record_mode`).

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
/// Pictures to run after the reset that follows an `STP` (see `settle`).
const STP_RESET_FRAMES: u64 = 120;
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

/// Set to make a missing corpus ROM a FAILURE instead of a skip. CI's
/// `snes-test-roms` job sets it, so a broken fetch can never read as green.
const CORPUS_REQUIRE: &str = "LUNA_SNES_TEST_REQUIRE";
/// Same, for the gitignored commercial ROMs under `tests/roms/` — set it
/// locally before tagging a release (CI never has these ROMs).
const GAMES_REQUIRE: &str = "LUNA_GAME_TEST_REQUIRE";

/// Report a test that cannot run: a skip notice, or a panic when the
/// matching `*_REQUIRE` variable is set.
fn skip(require_var: &str, why: &str) {
    assert!(
        std::env::var_os(require_var).is_none(),
        "{why} — and {require_var} is set, so a skip is a failure"
    );
    eprintln!("[skip] {why}");
}

/// `LUNA_SNES_TEST_RECORD=1`: print the freshly computed hash and return
/// without asserting. Only the value `1` enables it — a stray
/// `LUNA_SNES_TEST_RECORD=0` (or an empty one) must not switch every golden
/// off. Recording while a `*_REQUIRE` gate is set is a contradiction (the
/// gate would pass having asserted nothing), so that combination panics.
fn record_mode() -> bool {
    if std::env::var("LUNA_SNES_TEST_RECORD").as_deref() != Ok("1") {
        return false;
    }
    for gate in [CORPUS_REQUIRE, GAMES_REQUIRE] {
        assert!(
            std::env::var_os(gate).is_none(),
            "LUNA_SNES_TEST_RECORD=1 skips every assertion, and {gate} is set: \
             a gate run cannot record. Unset one of them."
        );
    }
    true
}

/// Corpus root: `$LUNA_SNES_TEST_DIR`, else `.luna_tests` in the repository.
fn corpus_root() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("LUNA_SNES_TEST_DIR") {
        let p = PathBuf::from(s);
        return p.is_dir().then_some(p);
    }
    let mut repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // crates/luna-core
    repo.pop(); // crates
    repo.pop(); // <repo root>
    let p = repo.join(".luna_tests");
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
    fb_bytes(&settle(rom, hold, region, false))
}

/// The settle-runner behind [`run_to_stable`]; hands back the machine so a
/// caller can read the native 512x448 buffer (`native` switches that capture on
/// before the run; it does not change the 256-wide picture the settle test
/// hashes).
fn settle(rom: Vec<u8>, hold: u16, region: luna_cartridge::Region, native: bool) -> Snes {
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = region;
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.ppu.set_native_capture(native);

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
    }

    // A ROM that ends in `STP` is waiting for the RESET button: CPUMSC's own
    // banner reads "Please Reset To PASS STP", and it prints the STP row's PASS
    // only after it finds, at boot, that the last run ended on a stop. The
    // reference capture shipped with the ROM is taken after that reset, so the
    // harness presses it too (the reset is still needed: the machine runs on
    // after an `STP`, but only a boot finds the stop and prints the PASS).
    if snes.cpu.stopped {
        snes.reset();
        if hold != 0 {
            snes.set_joypad(0, hold);
        }
        let target = snes.frame_count + STP_RESET_FRAMES;
        let mut n = 0u64;
        while snes.frame_count < target && n < STEP_CAP {
            if catch_unwind(AssertUnwindSafe(|| snes.step())).is_err() {
                break;
            }
            n += 1;
        }
    }

    std::panic::set_hook(prev_hook);
    eprintln!(
        "[settle] frame={} executed={} stable={}",
        snes.frame_count, executed, stable
    );
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
    snes
}

fn dump_png(bytes: &[u8], path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 224 rows, or 239 when the ROM ran in overscan.
    let height = (bytes.len() / (FRAME_W * 3)) as u32;
    let img = image::RgbImage::from_raw(FRAME_W as u32, height, bytes.to_vec()).expect("dims");
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
        skip(
            CORPUS_REQUIRE,
            "SNES test corpus not found — run tools/fetch-snes-test-roms.sh \
             (into .luna_tests) or set LUNA_SNES_TEST_DIR",
        );
        return;
    };
    let path = root.join(rel);
    if !path.is_file() {
        skip(
            CORPUS_REQUIRE,
            &format!("{rel}: not present under {}", root.display()),
        );
        return;
    }

    let rom = std::fs::read(&path).expect("read rom");
    let bytes = run_to_stable(rom, hold, region);
    let got = hex(&Sha256::digest(&bytes));

    if record_mode() {
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
        skip(CORPUS_REQUIRE, "SNES test corpus not found");
        return;
    };
    let path = root.join("PPU/Interlace/InterlaceFont/InterlaceFont.sfc");
    let Ok(rom) = std::fs::read(&path) else {
        skip(CORPUS_REQUIRE, &format!("{} absent", path.display()));
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
        snes.ppu.native_framebuffer().len(),
        512 * 448,
        "native buffer must be 512x448 (the ROM runs without overscan)"
    );
    let mut bytes = Vec::with_capacity(512 * 448 * 3);
    for px in snes.ppu.native_framebuffer() {
        bytes.extend_from_slice(px);
    }
    let got = hex(&Sha256::digest(&bytes));
    if record_mode() {
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
// CPUMSC ends in STP and is captured after the reset it asks for (see
// `settle`): the hash is of the picture with every PASS row drawn.
cpu_test!(
    cpu_msc,
    "MSC",
    "8e54d6650fc4b4ac61765184e49ac964e4093a701de7e77dd70341c9aae330a1"
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

/// Outcome of one Peter Lemon `CPUTest/SPC700/<NAME>` run.
struct Spc700Run {
    /// Last value read from CPUIO0 (`$2140`).
    port0: u8,
    /// The emulator panicked before the ROM reached a verdict.
    panicked: bool,
    /// S-CPU instructions executed.
    executed: u64,
}

/// Peter Lemon `CPUTest/SPC700/<NAME>` ALU hardware test — checked by its
/// **memory-result protocol**, not a framebuffer hash (the result display
/// cycles per addressing mode, so a hash settles on a non-deterministic
/// transient). Per the ROM's `SPC700<NAME>_spc.asm`, the SPC700 program runs
/// its sub-tests in order and reports each on CPUIO0 (`$2140`):
///
/// - sub-test `k` passed → it writes `k` (`$01`, `$02`, …) and goes on;
/// - sub-test `k` failed → it writes `$80 | k` and halts in a `bra` loop.
///
/// So the run is over when CPUIO0 shows either a fail code or `tests` (the
/// last sub-test's pass value), and the only passing outcome is the latter.
/// The mailbox is ignored until the SPC700 has left the IPL ROM and reported
/// sub-test 1: before that it carries the boot protocol (`$AA`, then the
/// upload's echoes), whose values mean nothing here.
fn run_spc700_mailbox(rom: Vec<u8>, tests: u8) -> Spc700Run {
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = luna_cartridge::Region::Pal;
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut executed = 0u64;
    let mut panicked = false;
    let mut started = false;
    'run: while executed < SPC700_STEP_CAP {
        for _ in 0..SPC_POLL_EVERY {
            if catch_unwind(AssertUnwindSafe(|| snes.step())).is_err() {
                panicked = true;
                break 'run;
            }
            executed += 1;
        }
        if !snes.apu_real.past_iplrom {
            continue;
        }
        let port0 = snes.apu_real.cpu_read_port(0);
        started |= port0 & 0x7F == 1;
        let failed = port0 & 0x80 != 0 && (1..=tests).contains(&(port0 & 0x7F));
        if started && (failed || port0 == tests) {
            break;
        }
    }
    std::panic::set_hook(prev_hook);
    Spc700Run {
        port0: snes.apu_real.cpu_read_port(0),
        panicked,
        executed,
    }
}

/// `$tests` is the number of sub-tests in the ROM, i.e. the pass value of the
/// last one (the highest `str REG_CPUIO0=#$xx` below `$80` in its
/// `SPC700<NAME>_spc.asm`).
macro_rules! spc700_test {
    ($fn:ident, $name:literal, $tests:literal) => {
        #[test]
        fn $fn() {
            let rel = concat!("CPUTest/SPC700/", $name, "/SPC700", $name, ".sfc");
            let Some(root) = corpus_root() else {
                skip(
                    CORPUS_REQUIRE,
                    "SNES test corpus not found (tools/fetch-snes-test-roms.sh)",
                );
                return;
            };
            let path = root.join(rel);
            if !path.is_file() {
                skip(
                    CORPUS_REQUIRE,
                    &format!("{rel}: not present under {}", root.display()),
                );
                return;
            }
            let rom = std::fs::read(&path).expect("read rom");
            let run = run_spc700_mailbox(rom, $tests);
            assert!(
                !run.panicked,
                "SPC700 {}: the emulator panicked after {} instructions (CPUIO0 = ${:02X})",
                $name, run.executed, run.port0
            );
            assert!(
                run.port0 & 0x80 == 0,
                "SPC700 {}: sub-test {} of {} FAILED on the hardware-result protocol \
                 (CPUIO0/$2140 = ${:02X}, the SPC700 halted in its fail loop)",
                $name,
                run.port0 & 0x7F,
                $tests,
                run.port0
            );
            assert_eq!(
                run.port0, $tests,
                "SPC700 {}: did not run to completion — CPUIO0/$2140 = ${:02X} after {} \
                 instructions (cap {}), the last sub-test reports ${:02X}",
                $name, run.port0, run.executed, SPC700_STEP_CAP, $tests
            );
        }
    };
}

spc700_test!(spc700_adc, "ADC", 26);
spc700_test!(spc700_and, "AND", 28);
spc700_test!(spc700_dec, "DEC", 14);
spc700_test!(spc700_eor, "EOR", 26);
spc700_test!(spc700_inc, "INC", 14);
spc700_test!(spc700_ora, "ORA", 28);
spc700_test!(spc700_sbc, "SBC", 26);

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
    // A golden that is recorded but not blessed yet: it runs only with
    // `--ignored`, and fails there if the render moves.
    ($fn:ident, $path:literal, $hash:literal, hold = $mask:expr, ignore = $reason:literal) => {
        #[test]
        #[ignore = $reason]
        fn $fn() {
            test_display(
                concat!("PPU/", $path),
                $hash,
                $mask,
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
    "5b1687c40daeaa87d7a260a8a44351f7f4201d3a55eae48ff1e9b73af32488c4"
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
    "406990b17cca553ce576f5f96758f3d2757a6e285dcf456d74da4ae86b1b8716"
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
    "664bfdd293b8e60a83a93ba2e912adec4bbd56a48ce276391ab9bd0fdc2483df",
    hold = PAD_R
);
// Re-baselined 2026-10-05 (both mosaic goldens): the vertical mosaic counter
// is now ares' / Mesen2's (blocks start on PPU line 1, reload when `$2106`
// switches mosaic on), where luna rounded the line down (blocks from line 0).
// At the demo's largest size (16) luna's frame is pixel-identical to the
// corpus' `MosaicMode3.png`, which the old grid was one line off from.
//
// MosaicMode5: Mode 5 hi-res + interlace, the Moogle figure, with a mosaic
// whose size only grows while R is held (`MosaicMode5.asm`: `$2106` starts at
// `%00000001`, size 0, and gains `$10` every 8 frames of R). Until 2026-10-04
// this test held nothing, so it rendered the un-mosaicked picture — the very
// frame of `ppu_interlace_moogle`, same hash — and Mode 5 mosaic had no
// coverage at all. It holds R like `ppu_mosaic_mode3`; the ramp never
// settles, so the capture is the frame-cap one (`$2106` = `$31`, 4-pixel
// blocks).
//
// Blessed 2026-10-05: in hi-res the mosaic latch is taken on the below
// (left) half-pixel and the above half reads it (ares `background.cpp`,
// Mesen2 `RenderTilemap`), so a block is one colour across both halves —
// luna sampled the two halves separately (vertical stripes). With that and
// the vertical counter above, luna's native 512x448 frame at size 14 is
// pixel-identical to a Mesen2 capture of the same scene. The corpus'
// `MosaicMode5.png` (16x32 blocks, horizontal field stripes) matches neither
// reference and is not used.
ppu_test!(
    ppu_mosaic_mode5,
    "Mosaic/Mode5/MosaicMode5.sfc",
    "b89a07587a39f552671c38f59878433008d7c55614cbe59ea494ec0dbd4d2254",
    hold = PAD_R
);

// -----------------------------------------------------------------------
// Interlace scenes (512x448 = Mode 5/6 hi-res + SETINI bit 0). luna
// collapses to 256x224 by averaging both fields (Phase C). Validated
// against each ROM's 512x448 reference (downsampled). OBJ interlace
// (SETINI bit 1) is rendered too — see `ppu_interlace_rpg` below.
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
    "5d4275902b391be90de78a3d6b990b791c8f2e6e8461fee2de4518ea829b7474"
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
// following CGDATA DMA was dropped by the active-display write gate.
// Fixed: `Ppu::write` never drops a CGDATA write, from the CPU or from
// DMA/HDMA (CGRAM is never dropped on hardware, ares `io.cpp:55-60`);
// VRAM/OAM stay gated (`io.cpp:26,40`).
// The pseudo-hires variant — whose per-8-line
// ("per tile row") cadence + photo content hides the residual sub-line
// timing — now renders the full-colour mandrill cleanly → passing golden.
// See docs/archive/luna_dma_gaps.md #7.
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
// HiColor128 (gap #7b): pixel-exact vs the hardware reference since the
// 65C816 hardware-interrupt entry gained ares' two cycles (2026-09-19). The
// chart fires its palette DMA from an H-IRQ; entering every IRQ 14 master
// clocks early moved alternate 8-line bands' bursts across a line (the old
// "band-group parity" signature, 91-ish % by the tolerant metric, 11 % exact).
ppu_test!(
    ppu_hdma_hicolor128,
    "HDMA/HiColor128PerTileRow/HiColor128PerTileRow.sfc",
    "238610ed67b080242bc4a5fad903486516b39c48e2f3c07ef26fc1869b1da9b1"
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
    let rate: u32 = luna_apu::SAMPLE_RATE_HZ;
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
        skip(
            CORPUS_REQUIRE,
            "SNES test corpus not found (run tools/fetch-snes-test-roms.sh)",
        );
        return;
    };
    let path = root.join(rel);
    if !path.is_file() {
        skip(
            CORPUS_REQUIRE,
            &format!("{rel}: not present under {}", root.display()),
        );
        return;
    }

    let rom = std::fs::read(&path).expect("read rom");
    let samples = run_audio(rom, hold);
    let got = hex(&Sha256::digest(audio_bytes(&samples)));
    let nonsilent = samples.iter().filter(|(l, r)| *l != 0 || *r != 0).count();

    if record_mode() {
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
}

// Golden hashes of luna's 32 kHz PCM output (first 3 s, loaded as PAL).
// All 8 auditioned (recognisable, clean) and re-baselined 2026-06-23 after the
// Phase-2/3 SPC700 cycle-accuracy waveform shift; the multi-block-upload music
// ROMs play thanks to the IPL-ROM byte fix. PitchMod stays ignored (a real,
// ares-matching STOP halt — see its reason).
spc_test!(
    spc_italo,
    "ItaloTest/ItaloTest.sfc",
    "cffc62b5d0800b88f445eefebeeedfbd573dea63a2e5c65fcfe4805e7b4ec791"
);
spc_test!(
    spc_pitchmod,
    "PitchMod/PitchMod.sfc",
    "2d0b4cf14f382dff76f4e77a016e98827c70e36c3fcc6b9016ac92ec75bc529e",
    ignore = "PitchMod is a knife-edge timing ROM — Mesen2 ALSO halts its SPC700 on STOP ~1.8s in (frame 108), so luna is correct; golden was captured with pre-8ffb323 wrong cycles (project_pitchmod_spc700_crash)"
);
spc_test!(
    spc_play_brr,
    "PlayBRRSample/PlayBRRSample.sfc",
    "f63e508778df81c83237481e1dabec43d30fd3d56666a0697ceebfccea50ada6"
);
spc_test!(
    spc_play_noise,
    "PlayNoise/PlayNoise.sfc",
    "1c0d22d2d4c3007562b0145ef5c11d11e63de66c9cd067b74f97c93ab0e7f9bd"
);
spc_test!(
    spc_twinkle,
    "Twinkle/Twinkle.sfc",
    "443c7d44acdc065359d204afd8f62b227a47a406f8d9c6a431bb1a26d457b2a8"
);
// Multi-block uploads — silent until the IPL-ROM `$FFEE` byte fix.
spc_test!(
    spc_axel_f,
    "Axel-F/Axel-F.sfc",
    "4f67124261fb0054f371786389e4a0fb5bdb78e2947f4977e3ed83eac44358df"
);
spc_test!(
    spc_ffvii_prelude,
    "FFVIIPrelude/FFVIIPrelude.sfc",
    "07b55afcd3b98f530f3bca104832cb8652d0aebec3ffef6af54c0c9d5ec6569b"
);
spc_test!(
    spc_speech,
    "SpeechSynth/SpeechSynth.sfc",
    "67a2b75791014584f0e8fcce25cf5c64bab1ac477402cefcc8c82c1999d12b11"
);
// Plays only on a button press — hold A (song 1) until the driver boots.
spc_test!(
    spc_play_two_song,
    "PlayTwoSong/PlayTwoSong.sfc",
    "30bc23273d3e36325d74e11a43d26761b2c149635537900b0aa57f62800d22a0",
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
// Re-baselined 2026-09-27: a stopped GSU now drains its RAM write buffer
// (ares superfx.cpp:29), so the last byte of a job lands before the CPU copies
// the framebuffer. Star Fox shifts phase, Stunt Race FX's demo takes another
// line; both render cleanly (GUI-validated).
// Re-baselined 2026-09-27 (SA-1 idleJump/idleBranch, ares sa1/memory.cpp):
// a SA-1 jump into ROM costs one more step, so SMRPG's intro and Kirby's
// play run a few frames' worth of SA-1 work later; both render cleanly.
// Again 2026-09-27: the SA-1 now keeps its HDMA time (no 120-mclk budget
// clamp) and sees the DMA's address in its bus-conflict check.
// Re-baselined 2026-10-10 (Star Fox only): a DMA burst is realigned on its
// whole length (hdma_ares_audit row 15). Over frames 1700-2100 the intro
// shows the same 61 pictures in the same order; each one arrives up to
// three frames apart, and frame 1939 holds the previous one.
macro_rules! game_test {
    ($fn:ident, $file:literal, $frames:literal, $hash:literal) => {
        #[test]
        fn $fn() {
            let Some(root) = games_root() else {
                skip(GAMES_REQUIRE, "commercial ROMs (tests/roms/) absent — gitignored, dump your own");
                return;
            };
            let path = root.join($file);
            if !path.is_file() {
                skip(GAMES_REQUIRE, &format!("{}: not present under {}", $file, root.display()));
                return;
            }
            let rom = std::fs::read(&path).expect("read rom");
            let bytes = run_game_to_frame(rom, $frames);
            let got = hex(&Sha256::digest(&bytes));
            if record_mode() {
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
    "a0e8108d00fd2fbef29c64f08e0bab42142df0c633aee768b5bd5603e6a5cba2"
);
game_test!(
    game_mariokart,
    "Super Mario Kart (USA).sfc",
    3587,
    "b7bc468dec89ba2f02f36190f0fc4c6945ec2cee1f6e68000452596e8e21456e"
);
// SA-1
game_test!(
    game_smrpg,
    "Super Mario RPG - Legend of the Seven Stars (USA).sfc",
    905,
    "47eb9b35d655c2360482fe7821bdf6cd7e5bc913599e454ff57c3cbb9c04e1f8"
);
game_test!(
    game_kirby_ss,
    "Kirby Super Star (USA).sfc",
    3054,
    "c95226c41ecf5860d59ab4faaabfbedf6fcce4fba8a448fbbbe8a26779c9cfb0"
);
// Super FX (GSU)
game_test!(
    game_starfox,
    "Star Fox (USA) (Rev 2).sfc",
    1939,
    "014d210ea9c8d3714af81c7c89db07408de684d3c98bb60afb82c1c99f7e3755"
);
game_test!(
    game_stuntfx,
    "Stunt Race FX (USA) (Rev 1).sfc",
    2299,
    "5fbff90c93959457c44a756089d3457d585038d44061004dffa308b3f486faa2"
);
// S-DD1
game_test!(
    game_starocean,
    "Star Ocean (tr).sfc",
    1875,
    "e17c1c5c7d04e8f4a33a805dc26638de41870f67d09788d3857038aaf84c1dde"
);
// DSP-1
game_test!(
    game_pilotwings,
    "Pilotwings (USA).sfc",
    1395,
    "f8f76478156ea150f3e8be3fc73f13adb11396be8fcb52d817d920ad475b5a43"
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
    "3b3d83cd082b3676051a8d4b601fc0f0151bdf33c870095a56f32e1c9f81efa2"
);
game_test!(
    game_chrono,
    "Chrono Trigger (USA).sfc",
    1870,
    "5d7bdb953bc3f122d15dc1c22ae1e00d1cfc38248c6fa3148967e0e2e7065f1a"
);
// Large ROM
game_test!(
    game_tales,
    "Tales of Phantasia (Japan).sfc",
    2574,
    "1b32965bbd7e33bfb59ad8e80f83df6cc7c3e645bbefe395410471a40dd10f8e"
);
// HDMA (raster split + gradient)
game_test!(
    game_contra3,
    "Contra III - The Alien Wars (USA).sfc",
    4573,
    "0e9ef26dd6a3c8986d9e9ae4ed7c9167aeadb36e1daa1b97c8ac373af42632df"
);
game_test!(
    game_axelay,
    "Axelay (USA).sfc",
    4801,
    "d7434531cc2f5d7f4fa766991e20fa746c77235ae9bdefe29bc0e7b0dc95db5d"
);

// =============================================================================
// Reference PNGs shipped with the corpus
// =============================================================================

/// Decode a reference picture to `(width, height, RGB888)`. The corpus has one
/// reference (`Mode7/Perspective/Perspective.png`) that is really a 24-bit BMP
/// under a `.png` name, and the `image` crate here is built with PNG only, so
/// that format is read by hand (bottom-up BGR rows padded to 4 bytes); the
/// format is told by the file's magic, never by its extension.
fn load_reference(path: &Path) -> Result<(usize, usize, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.starts_with(b"BM") {
        let u32at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let (off, w, h) = (u32at(10) as usize, u32at(18) as usize, u32at(22) as i32);
        let bpp = u16::from_le_bytes([bytes[28], bytes[29]]);
        if bpp != 24 || h <= 0 || bytes.len() < off + ((w * 3).div_ceil(4) * 4) * h as usize {
            return Err(format!(
                "{}: unsupported BMP (bpp {bpp}, height {h})",
                path.display()
            ));
        }
        let (h, stride) = (h as usize, (w * 3).div_ceil(4) * 4);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in (0..h).rev() {
            for x in 0..w {
                let p = off + y * stride + x * 3;
                rgb.extend_from_slice(&[bytes[p + 2], bytes[p + 1], bytes[p]]);
            }
        }
        return Ok((w, h, rgb));
    }
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_rgb8();
    Ok((img.width() as usize, img.height() as usize, img.into_raw()))
}

/// Every `.sfc` under `root`, sorted.
fn all_roms(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read corpus dir")
            .map(|e| e.expect("dir entry").path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "sfc") {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, &mut v);
    v
}

/// The reference picture of a ROM: the `.png` of the same directory with the
/// same name, compared case-insensitively (`RedspaceHDMA.png` belongs to
/// `RedSpaceHDMA.sfc`).
fn find_reference(rom: &Path) -> Option<PathBuf> {
    let stem = rom.file_stem()?.to_string_lossy().to_lowercase();
    std::fs::read_dir(rom.parent()?)
        .ok()?
        .map(|e| e.expect("dir entry").path())
        .find(|p| {
            p.extension().is_some_and(|e| e == "png")
                && p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().to_lowercase() == stem)
        })
}

/// How far a captured picture is from its reference.
#[derive(Debug, PartialEq, Eq)]
struct PicDiff {
    /// Pixels where at least one channel differs.
    pixels: usize,
    /// Largest per-channel difference.
    max: u8,
}

/// Compare two RGB888 buffers of the same size, pixel by pixel.
fn diff_rgb(got: &[u8], want: &[u8]) -> PicDiff {
    assert_eq!(got.len(), want.len(), "diff_rgb needs equal sizes");
    let mut d = PicDiff { pixels: 0, max: 0 };
    for (g, w) in got.chunks_exact(3).zip(want.chunks_exact(3)) {
        let m = (0..3).map(|c| g[c].abs_diff(w[c])).max().unwrap_or(0);
        if m > 0 {
            d.pixels += 1;
            d.max = d.max.max(m);
        }
    }
    d
}

/// What a ROM's capture is expected to be, against its reference picture.
#[derive(Debug)]
enum RefClass {
    /// Not one pixel differs (tolerance 0).
    Exact,
    /// Named class "colour conversion off by one LSB": every pixel within one
    /// LSB per channel, and the picture otherwise identical. The reference
    /// PNGs of these ROMs have a background of 248 where luna draws 247; they
    /// come from a capture chain with another RGB555 to RGB888 conversion
    /// (provenance unverified). luna expands with `(c << 3) | (c >> 2)`.
    OneLsb,
    /// A known difference. It may only shrink: more than `max_pixels` pixels
    /// differing fails (a regression), and so does an exact match or one within
    /// the 1-LSB bound (the entry has rotted: promote it). `why` is the
    /// hypothesis, unverified unless it says otherwise.
    Exception {
        max_pixels: usize,
        why: &'static str,
    },
}

/// Judge a measured difference against the class the ROM is listed under.
fn judge(rel: &str, class: &RefClass, d: &PicDiff) -> Result<(), String> {
    match class {
        RefClass::Exact if d.pixels == 0 => Ok(()),
        RefClass::Exact => Err(format!(
            "{rel}: {} pixels differ from the reference (max channel diff {}), tolerance is 0",
            d.pixels, d.max
        )),
        RefClass::OneLsb if d.pixels > 0 && d.max <= 1 => Ok(()),
        RefClass::OneLsb if d.pixels == 0 => Err(format!(
            "{rel}: now matches the reference exactly: list it as Exact"
        )),
        RefClass::OneLsb => Err(format!(
            "{rel}: max channel diff {} on {} pixels, the 1-LSB class allows 1",
            d.max, d.pixels
        )),
        RefClass::Exception { .. } if d.pixels == 0 => Err(format!(
            "{rel}: now matches the reference exactly: remove it from the exceptions"
        )),
        RefClass::Exception { .. } if d.max <= 1 => Err(format!(
            "{rel}: now within 1 LSB of the reference: move it to the OneLsb class"
        )),
        RefClass::Exception { max_pixels, why } if d.pixels > *max_pixels => Err(format!(
            "{rel}: {} pixels differ, more than the {max_pixels} recorded ({why})",
            d.pixels
        )),
        RefClass::Exception { .. } => Ok(()),
    }
}

/// Region and held button a ROM is run with: the same as its golden test
/// (the `CPUTest` family NTSC, the PPU demos PAL, A held for the latency ROM,
/// R for the Mosaic demos).
fn run_config(rel: &str) -> (u16, luna_cartridge::Region) {
    let region = if rel.starts_with("CPUTest/CPU/") {
        luna_cartridge::Region::Ntsc
    } else {
        luna_cartridge::Region::Pal
    };
    let hold = if rel.contains("ControllerLatency") {
        PAD_A
    } else if rel.contains("/Mosaic/") {
        PAD_R
    } else {
        0
    };
    (hold, region)
}

/// Run `rel` to its stable picture and return it as `(width, height, RGB888)`:
/// the 256-wide picture, or the native 512-wide one (`native`).
fn capture(root: &Path, rel: &str, native: bool) -> (usize, usize, Vec<u8>) {
    let (hold, region) = run_config(rel);
    let rom = std::fs::read(root.join(rel)).expect("read rom");
    let snes = settle(rom, hold, region, native);
    if native {
        let bytes: Vec<u8> = snes
            .ppu
            .native_framebuffer()
            .iter()
            .flat_map(|px| px.iter().copied())
            .collect();
        (512, bytes.len() / 3 / 512, bytes)
    } else {
        let bytes = fb_bytes(&snes);
        (FRAME_W, bytes.len() / 3 / FRAME_W, bytes)
    }
}

/// Compare the capture of `rel` with its reference picture under `class`.
/// `native` compares at the native 512x448 resolution (the references of the
/// hi-res and interlace ROMs are that size).
fn check_reference(rel: &str, class: &RefClass, native: bool) {
    let Some(root) = corpus_root() else {
        skip(CORPUS_REQUIRE, "SNES test corpus not found");
        return;
    };
    let rom = root.join(rel);
    if !rom.is_file() {
        skip(
            CORPUS_REQUIRE,
            &format!("{rel}: not present under {}", root.display()),
        );
        return;
    }
    let Some(png) = find_reference(&rom) else {
        skip(
            CORPUS_REQUIRE,
            &format!("{rel}: no reference picture next to it"),
        );
        return;
    };
    let (rw, rh, want) = load_reference(&png).expect("decode reference");
    let (gw, gh, got) = capture(&root, rel, native);
    assert_eq!(
        (gw, gh),
        (rw, rh),
        "{rel}: capture is {gw}x{gh}, the reference {rw}x{rh}: not comparable"
    );
    let d = diff_rgb(&got, &want);
    println!(
        "REFERENCE {rel}: {} pixels differ, max channel diff {}",
        d.pixels, d.max
    );
    if let Err(e) = judge(rel, class, &d) {
        panic!("{e}");
    }
}

/// One test per ROM compared with its reference picture, and the list the
/// coverage test checks the corpus against.
macro_rules! reference_tests {
    ($($name:ident: $rel:literal, $native:literal, $class:expr;)*) => {
        /// Every ROM compared with its reference picture.
        const REFERENCE_ROMS: &[&str] = &[$($rel),*];
        $(
            #[test]
            fn $name() {
                check_reference($rel, &$class, $native);
            }
        )*
    };
}

// The reference pictures shipped with the corpus, at tolerance 0 (see
// `RefClass`). Measured 2026-11-08; the numbers are pixel counts of the
// 256x224 (or native 512x448) picture.
reference_tests! {
    // Exact.
    png_cpu_dec: "CPUTest/CPU/DEC/CPUDEC.sfc", false, RefClass::Exact;
    png_cpu_inc: "CPUTest/CPU/INC/CPUINC.sfc", false, RefClass::Exact;
    png_cpu_mov: "CPUTest/CPU/MOV/CPUMOV.sfc", false, RefClass::Exact;
    png_controller_latency: "INPUT/ControllerLatency/ControllerLatency.sfc", false, RefClass::Exact;
    png_bg1_2bpp: "PPU/BGMAP/8x8/2BPP/8x8BG1Map2BPP32x328PAL/8x8BG1Map2BPP32x328PAL.sfc", false, RefClass::Exact;
    png_bg2_2bpp: "PPU/BGMAP/8x8/2BPP/8x8BG2Map2BPP32x328PAL/8x8BG2Map2BPP32x328PAL.sfc", false, RefClass::Exact;
    png_bg3_2bpp: "PPU/BGMAP/8x8/2BPP/8x8BG3Map2BPP32x328PAL/8x8BG3Map2BPP32x328PAL.sfc", false, RefClass::Exact;
    png_bg4_2bpp: "PPU/BGMAP/8x8/2BPP/8x8BG4Map2BPP32x328PAL/8x8BG4Map2BPP32x328PAL.sfc", false, RefClass::Exact;
    png_bg_4bpp: "PPU/BGMAP/8x8/4BPP/8x8BGMap4BPP32x328PAL/8x8BGMap4BPP32x328PAL.sfc", false, RefClass::Exact;
    png_tile_flip: "PPU/BGMAP/8x8/8BPP/TileFlip/8x8BGMapTileFlip.sfc", false, RefClass::Exact;
    png_green_space: "PPU/GreenSpace/GreenSpace.sfc", false, RefClass::Exact;
    png_rings: "PPU/Rings/Rings.sfc", false, RefClass::Exact;
    png_hicolor_3840: "PPU/Blend/HiColor/HiColor3840/HiColor3840.sfc", false, RefClass::Exact;
    png_hicolor_575myst: "PPU/Blend/HiColor/HiColor575Myst/HiColor575Myst.sfc", false, RefClass::Exact;
    png_hicolor_128: "PPU/HDMA/HiColor128PerTileRow/HiColor128PerTileRow.sfc", false, RefClass::Exact;
    png_hicolor_64: "PPU/HDMA/HiColor64PerTileRow/HiColor64PerTileRow.sfc", false, RefClass::Exact;
    png_mode7_hdma: "PPU/HDMA/Mode7HDMA/Mode7HDMA.sfc", false, RefClass::Exact;
    png_redspace_hdma: "PPU/HDMA/RedSpaceHDMA/RedSpaceHDMA.sfc", false, RefClass::Exact;
    png_redspace_indirect_hdma: "PPU/HDMA/RedSpaceIndirectHDMA/RedSpaceIndirectHDMA.sfc", false, RefClass::Exact;
    png_window_hdma: "PPU/Window/WindowHDMA/WindowHDMA.sfc", false, RefClass::Exact;
    // Exact, found by the first measurement: a reference that is a BMP under a
    // `.png` name, and the one interlace ROM whose native picture matches.
    png_mode7_perspective: "PPU/Mode7/Perspective/Perspective.sfc", false, RefClass::Exact;
    png_interlace_rpg: "PPU/Interlace/InterlaceRPG/InterlaceRPG.sfc", true, RefClass::Exact;

    // Named class: colour conversion off by one LSB.
    png_cpu_adc: "CPUTest/CPU/ADC/CPUADC.sfc", false, RefClass::OneLsb;
    png_cpu_and: "CPUTest/CPU/AND/CPUAND.sfc", false, RefClass::OneLsb;
    png_cpu_asl: "CPUTest/CPU/ASL/CPUASL.sfc", false, RefClass::OneLsb;
    png_cpu_bit: "CPUTest/CPU/BIT/CPUBIT.sfc", false, RefClass::OneLsb;
    png_cpu_bra: "CPUTest/CPU/BRA/CPUBRA.sfc", false, RefClass::OneLsb;
    png_cpu_cmp: "CPUTest/CPU/CMP/CPUCMP.sfc", false, RefClass::OneLsb;
    png_cpu_eor: "CPUTest/CPU/EOR/CPUEOR.sfc", false, RefClass::OneLsb;
    png_cpu_jmp: "CPUTest/CPU/JMP/CPUJMP.sfc", false, RefClass::OneLsb;
    png_cpu_ldr: "CPUTest/CPU/LDR/CPULDR.sfc", false, RefClass::OneLsb;
    png_cpu_lsr: "CPUTest/CPU/LSR/CPULSR.sfc", false, RefClass::OneLsb;
    png_cpu_msc: "CPUTest/CPU/MSC/CPUMSC.sfc", false, RefClass::OneLsb;
    png_cpu_ora: "CPUTest/CPU/ORA/CPUORA.sfc", false, RefClass::OneLsb;
    png_cpu_phl: "CPUTest/CPU/PHL/CPUPHL.sfc", false, RefClass::OneLsb;
    png_cpu_psr: "CPUTest/CPU/PSR/CPUPSR.sfc", false, RefClass::OneLsb;
    png_cpu_ret: "CPUTest/CPU/RET/CPURET.sfc", false, RefClass::OneLsb;
    png_cpu_rol: "CPUTest/CPU/ROL/CPUROL.sfc", false, RefClass::OneLsb;
    png_cpu_ror: "CPUTest/CPU/ROR/CPUROR.sfc", false, RefClass::OneLsb;
    png_cpu_sbc: "CPUTest/CPU/SBC/CPUSBC.sfc", false, RefClass::OneLsb;
    png_cpu_str: "CPUTest/CPU/STR/CPUSTR.sfc", false, RefClass::OneLsb;
    png_cpu_trn: "CPUTest/CPU/TRN/CPUTRN.sfc", false, RefClass::OneLsb;
    png_redspace_9bit_hdma: "PPU/HDMA/RedSpace9BitHDMA/RedSpace9BitHDMA.sfc", false, RefClass::OneLsb;

    // Exceptions: real differences, named, never silently tolerated.
    png_rot_zoom: "PPU/Mode7/RotZoom/RotZoom.sfc", false, RefClass::Exception {
        max_pixels: 47_565,
        why: "hypothesis: the animation phase of the capture differs from the reference's",
    };
    png_wave_hdma: "PPU/HDMA/WaveHDMA/WaveHDMA.sfc", false, RefClass::Exception {
        max_pixels: 53_805,
        why: "hypothesis: the animation phase of the capture differs from the reference's",
    };
    png_window_multi_hdma: "PPU/Window/WindowMultiHDMA/WindowMultiHDMA.sfc", false, RefClass::Exception {
        max_pixels: 30_798,
        why: "hypothesis: the animation phase of the capture differs from the reference's",
    };
    png_star_wars: "PPU/Mode7/StarWars/StarWars.sfc", false, RefClass::Exception {
        max_pixels: 5669,
        why: "hypothesis: the animation phase of the capture differs from the reference's",
    };
    png_mosaic_mode3: "PPU/Mosaic/Mode3/MosaicMode3.sfc", false, RefClass::Exception {
        max_pixels: 48_676,
        why: "hypothesis: the mosaic ramp is at another step than the reference's",
    };
    png_mosaic_mode5: "PPU/Mosaic/Mode5/MosaicMode5.sfc", true, RefClass::Exception {
        max_pixels: 17_664,
        why: "hypothesis: the mosaic ramp is at another step than the reference's",
    };
    png_interlace_scroll: "PPU/Interlace/InterlaceScroll/InterlaceScroll.sfc", true, RefClass::Exception {
        max_pixels: 222_208,
        why: "hypothesis: the scroll position of the capture differs from the reference's",
    };
    png_interlace_font: "PPU/Interlace/InterlaceFont/InterlaceFont.sfc", true, RefClass::Exception {
        max_pixels: 9850,
        why: "unexplained: 4.3 % of the pixels differ, up to 255 on a channel (not a few-LSB offset)",
    };
    png_interlace_moogle: "PPU/Interlace/InterlaceMoogle/InterlaceMoogle.sfc", true, RefClass::Exception {
        max_pixels: 209_667,
        why: "unexplained: 91 % of the pixels differ, up to 70 on a channel",
    };
    png_interlace_myst_hdma: "PPU/Interlace/InterlaceMystHDMA/InterlaceMystHDMA.sfc", true, RefClass::Exception {
        max_pixels: 229_376,
        why: "hypothesis: another colour conversion in the capture chain (every pixel differs, by at most 6)",
    };
    png_interlace_simpsons_hdma: "PPU/Interlace/InterlaceSimpsonsHDMA/InterlaceSimpsonsHDMA.sfc", true, RefClass::Exception {
        max_pixels: 229_376,
        why: "hypothesis: another colour conversion in the capture chain (every pixel differs, by at most 7)",
    };
    png_hicolor_64_pseudo_hires: "PPU/HDMA/HiColor64PerTileRowPseudoHiRes/HiColor64PerTileRowPseudoHiRes.sfc", true, RefClass::Exception {
        max_pixels: 64_924,
        why: "unexplained: 28 % of the pixels differ, up to 91 on a channel",
    };
}

/// ROMs that have a picture next to them yet cannot be compared, and why.
const NOT_COMPARABLE: &[(&str, &str)] = &[
    (
        "PPU/Blend/HiColor/HiColor1241DLair/HiColor1241DLair.sfc",
        "its reference is 256x225, luna's picture 256x224",
    ),
    (
        "INPUT/XBandKeyboard/XBandKeyboard.sfc",
        "needs the XBand keyboard, which luna does not model; no display golden either",
    ),
];

/// ROMs the corpus ships without any reference picture.
const NO_REFERENCE: &[&str] = &[
    "PPU/BGMAP/8x8/8BPP/32x32/8x8BGMap8BPP32x32.sfc",
    "PPU/BGMAP/8x8/8BPP/32x64/8x8BGMap8BPP32x64.sfc",
    "PPU/BGMAP/8x8/8BPP/64x32/8x8BGMap8BPP64x32.sfc",
    "PPU/BGMAP/8x8/8BPP/64x64/8x8BGMap8BPP64x64.sfc",
];

/// The list above is the whole corpus: every ROM is compared, or named as not
/// comparable, or named as having no reference, and each of those claims is
/// still true. A new ROM or PNG in the corpus fails here until it is
/// classified. The SPC700 ROMs (audio: mailbox protocol or PCM hash) and the
/// `TEST/` input images are not display tests.
#[test]
fn png_reference_coverage() {
    let Some(root) = corpus_root() else {
        skip(CORPUS_REQUIRE, "SNES test corpus not found");
        return;
    };
    let mut problems = Vec::new();
    for rom in all_roms(&root) {
        let rel = rom
            .strip_prefix(&root)
            .expect("under root")
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("/TEST/")
            || rel.starts_with("CPUTest/SPC700/")
            || rel.starts_with("SPC700/")
        {
            continue;
        }
        let compared = REFERENCE_ROMS.contains(&rel.as_str());
        let not_comparable = NOT_COMPARABLE.iter().any(|(r, _)| *r == rel);
        let no_ref = NO_REFERENCE.contains(&rel.as_str());
        if [compared, not_comparable, no_ref]
            .iter()
            .filter(|b| **b)
            .count()
            != 1
        {
            problems.push(format!("{rel}: not in exactly one of the three lists"));
        }
        let has_png = find_reference(&rom).is_some();
        if no_ref && has_png {
            problems.push(format!("{rel}: listed without a reference, yet one exists"));
        }
        if (compared || not_comparable) && !has_png {
            problems.push(format!("{rel}: listed with a reference, yet none exists"));
        }
    }
    for rel in REFERENCE_ROMS
        .iter()
        .chain(NO_REFERENCE)
        .chain(NOT_COMPARABLE.iter().map(|(r, _)| r))
    {
        if !root.join(rel).is_file() {
            problems.push(format!("{rel}: listed, but absent from the corpus"));
        }
    }
    // A "not comparable" reason must still hold where it is a size.
    let lair = root.join(NOT_COMPARABLE[0].0);
    if let Some(png) = find_reference(&lair) {
        let (w, h, _) = load_reference(&png).expect("decode");
        if (w, h) == (FRAME_W, FRAME_H) {
            problems.push(format!(
                "{}: its reference is now {w}x{h}: compare it",
                NOT_COMPARABLE[0].0
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "reference pictures out of step:\n{}",
        problems.join("\n")
    );
}

/// The comparison itself can go red: a single flipped pixel fails an exact
/// match, a 2-LSB pixel fails the 1-LSB class, and the exception list
/// notices a ROM that starts matching.
#[test]
fn png_comparison_detects_faults() {
    let want: Vec<u8> = (0..FRAME_W * FRAME_H * 3)
        .map(|i| (i % 251) as u8)
        .collect();
    let mut got = want.clone();
    assert_eq!(diff_rgb(&got, &want), PicDiff { pixels: 0, max: 0 });
    assert!(judge("t", &RefClass::Exact, &diff_rgb(&got, &want)).is_ok());

    // One channel of one pixel, one LSB off.
    got[12_345] = got[12_345].wrapping_add(1);
    let d = diff_rgb(&got, &want);
    assert_eq!(d, PicDiff { pixels: 1, max: 1 });
    assert!(
        judge("t", &RefClass::Exact, &d).is_err(),
        "one pixel must fail Exact"
    );
    assert!(judge("t", &RefClass::OneLsb, &d).is_ok());
    let exception = RefClass::Exception {
        max_pixels: 10,
        why: "t",
    };
    assert!(
        judge("t", &exception, &d).is_err(),
        "within 1 LSB: the entry has rotted"
    );

    // Two LSB off: out of the 1-LSB class.
    got[12_345] = want[12_345].wrapping_add(2);
    let d = diff_rgb(&got, &want);
    assert_eq!(d.max, 2);
    assert!(judge("t", &RefClass::OneLsb, &d).is_err());
    assert!(judge("t", &exception, &d).is_ok());
    // More pixels than the exception recorded.
    for i in 0..11 {
        got[3 * i] = want[3 * i].wrapping_add(9);
    }
    assert!(judge("t", &exception, &diff_rgb(&got, &want)).is_err());
    // An exception that matches exactly has rotted.
    assert!(judge("t", &exception, &diff_rgb(&want, &want)).is_err());
}

/// The same, end to end on a real ROM: with the PPU faulted after the ROM has
/// settled (main-screen layers switched off), the next picture no longer
/// matches the reference, and the check says so.
#[test]
fn png_comparison_detects_a_faulted_render() {
    let Some(root) = corpus_root() else {
        skip(CORPUS_REQUIRE, "SNES test corpus not found");
        return;
    };
    let rel = "PPU/BGMAP/8x8/2BPP/8x8BG1Map2BPP32x328PAL/8x8BG1Map2BPP32x328PAL.sfc";
    let path = root.join(rel);
    let Some(png) = path.is_file().then(|| find_reference(&path)).flatten() else {
        skip(CORPUS_REQUIRE, &format!("{rel}: absent"));
        return;
    };
    let (_, _, want) = load_reference(&png).expect("decode reference");
    let (hold, region) = run_config(rel);
    let mut snes = settle(std::fs::read(&path).expect("read rom"), hold, region, false);
    assert_eq!(
        diff_rgb(&fb_bytes(&snes), &want).pixels,
        0,
        "unfaulted run must match"
    );
    snes.ppu.tm = 0;
    let target = snes.frame_count + 2;
    while snes.frame_count < target {
        snes.step();
    }
    let d = diff_rgb(&fb_bytes(&snes), &want);
    assert!(
        d.pixels > 0,
        "a faulted render must differ from the reference"
    );
    assert!(judge(rel, &RefClass::Exact, &d).is_err());
}
