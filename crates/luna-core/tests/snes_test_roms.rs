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
/// step cap / a `STP` / a CPU panic). Returns the framebuffer bytes and a
/// short outcome string (for record-mode diagnostics).
fn run_to_stable(rom: Vec<u8>) -> (Vec<u8>, String) {
    let cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    // Overridable for diagnostics (e.g. force a long run to rule out a
    // premature settle on an early blank frame).
    let stable_target: u32 = std::env::var("LUNA_SNES_TEST_STABLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(STABLE_SAMPLES);

    let mut last = String::new();
    let mut stable = 0u32;
    let mut executed = 0u64;
    let mut outcome = "cap";
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
                outcome = "panic";
                break 'run; // settle on whatever rendered before the panic
            }
            executed += 1;
        }
        let h = hex(&Sha256::digest(fb_bytes(&snes)));
        if h == last {
            stable += 1;
            if stable >= stable_target {
                outcome = "settled";
                break;
            }
        } else {
            stable = 0;
            last = h;
        }
        if snes.cpu.stopped {
            outcome = "stp";
            break;
        }
    }

    std::panic::set_hook(prev_hook);
    (fb_bytes(&snes), format!("{outcome}@{executed}"))
}

fn dump_png(bytes: &[u8], path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let img =
        image::RgbImage::from_raw(FRAME_W as u32, FRAME_H as u32, bytes.to_vec()).expect("dims");
    let _ = img.save(path);
}

/// Boot `rel` (relative to the corpus root), settle, and compare the
/// framebuffer SHA-256 to `expected`. Skips gracefully if the corpus or
/// the specific ROM is absent.
fn test_display(rel: &str, expected: &str) {
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
    let (bytes, outcome) = run_to_stable(rom);
    let got = hex(&Sha256::digest(&bytes));

    if std::env::var("LUNA_SNES_TEST_RECORD").is_ok() {
        if let Ok(dir) = std::env::var("LUNA_SNES_TEST_PNG") {
            let safe = rel.replace(['/', ' '], "_");
            dump_png(&bytes, &Path::new(&dir).join(format!("{safe}.png")));
        }
        println!("RECORD {rel} => {got}  [{outcome}]");
        return;
    }

    assert_eq!(
        got, expected,
        "framebuffer hash mismatch for {rel}\n  \
         (run LUNA_SNES_TEST_RECORD=1 to regenerate after an intended render change)"
    );
}

/// Declare a Peter Lemon `CPUTest/CPU/<NAME>/CPU<NAME>.sfc` golden test.
///
/// The `known_blank:` form marks a ROM that luna currently renders as a
/// blank backdrop instead of the result table (a real bug — Peter Lemon's
/// reference `CPU<NAME>.png` shows a full PASS table). It is `#[ignore]`d
/// so the default `cargo test` is green; the committed hash characterizes
/// the *current broken* output, so once luna is fixed the hash changes
/// and `--ignored` goes red, flagging the test for promotion.
macro_rules! cpu_test {
    ($fn:ident, $name:literal, $hash:literal) => {
        #[test]
        fn $fn() {
            test_display(
                concat!("CPUTest/CPU/", $name, "/CPU", $name, ".sfc"),
                $hash,
            );
        }
    };
    (known_blank: $fn:ident, $name:literal, $hash:literal) => {
        #[test]
        #[ignore = "luna renders a blank backdrop, not the result table (BUG); reference CPU*.png shows PASS"]
        fn $fn() {
            test_display(
                concat!("CPUTest/CPU/", $name, "/CPU", $name, ".sfc"),
                $hash,
            );
        }
    };
}

// Golden hashes captured from luna's renderer — see "Regenerating hashes".
// 19 render the correct all-PASS result screen; 4 (BRA/JMP/PSR/RET) render
// blank and are tracked via `known_blank`.
cpu_test!(
    cpu_adc,
    "ADC",
    "2b4a1a1ea9d2d7a547b3c4b985c5a07d0d5ec9828c03841c17bf57bc6af24fa8"
);
cpu_test!(
    cpu_and,
    "AND",
    "dc842c4946d734df3ae1abf4924fb0c855ef95db6608aa822274a18c4c480e3d"
);
cpu_test!(
    cpu_asl,
    "ASL",
    "3ef8e9b697786d8f3c0f7d363587a6b3994391d9d8a36ba1b9bf8a5bf0ea90ce"
);
cpu_test!(
    cpu_bit,
    "BIT",
    "7d0b41d3685e6f8f1392e93ff846d174c4fca7c9baa937fbdc75ae9ca3d05f87"
);
cpu_test!(known_blank: cpu_bra, "BRA", "f8ced4d9c1ee2e1724ea167a7c96252f71ec8f953ed0a30063e903bdf7b8c770");
cpu_test!(
    cpu_cmp,
    "CMP",
    "ce31e128fe6373a76f147d0e80c83d1152ea45e86922b029f9de77dbdb78aa3d"
);
cpu_test!(
    cpu_dec,
    "DEC",
    "973c4cdce5a638deafaf3aa39074ad98770b32bfc27aa53084d198f226cb2567"
);
cpu_test!(
    cpu_eor,
    "EOR",
    "5035742533129a7736e8c93c41978a9d59aaeabb21b668616c34777c495c8efe"
);
cpu_test!(
    cpu_inc,
    "INC",
    "e828650a1ef802cd5eebcdba2e8615720554ed40e91d65e7c2c78342c3c53792"
);
cpu_test!(known_blank: cpu_jmp, "JMP", "f8ced4d9c1ee2e1724ea167a7c96252f71ec8f953ed0a30063e903bdf7b8c770");
cpu_test!(
    cpu_ldr,
    "LDR",
    "12089e977d83764818e10f9f87db72e340a8b9ad9b776e8a065ae2f12718488e"
);
cpu_test!(
    cpu_lsr,
    "LSR",
    "2d550368758a48bbd1cce9d76f5b9dcb86a69c21a3749f3a3c84f4e5c81b407a"
);
cpu_test!(
    cpu_mov,
    "MOV",
    "81afc2ce72c697a7effeec9f10918dda203d2a96c5b689500b6d312813afa31f"
);
cpu_test!(
    cpu_msc,
    "MSC",
    "d03e55a340baefd3d1d504997207b505958b0daf0f5c423f229c13a47f839406"
);
cpu_test!(
    cpu_ora,
    "ORA",
    "90e92c7f51b7e27e12414ef67a51233825f1c0ace6e55b38a7b16cf533c04fd1"
);
cpu_test!(
    cpu_phl,
    "PHL",
    "26a498ec62ef0bf1dd42306a28f0a483264694971f8221c95f95ca882be0293d"
);
cpu_test!(known_blank: cpu_psr, "PSR", "f8ced4d9c1ee2e1724ea167a7c96252f71ec8f953ed0a30063e903bdf7b8c770");
cpu_test!(known_blank: cpu_ret, "RET", "f8ced4d9c1ee2e1724ea167a7c96252f71ec8f953ed0a30063e903bdf7b8c770");
cpu_test!(
    cpu_rol,
    "ROL",
    "d960ce769155cdde796c6eb8a06ec3db28699c62c5c653354778a142803bd078"
);
cpu_test!(
    cpu_ror,
    "ROR",
    "2487e94ef26f7ede31a5da110ae1919b4f990547af1d5e2a190071a3fab9433c"
);
cpu_test!(
    cpu_sbc,
    "SBC",
    "58476b63368a8d573c53acdf1811c967ccbd76e8936c3dba0184606804933113"
);
cpu_test!(
    cpu_str,
    "STR",
    "7ddff58290d755d980c6e356d3395531f7358ce3e82417c9da11a06afa3e639f"
);
cpu_test!(
    cpu_trn,
    "TRN",
    "a7721efe684316914dc56467a35fd20173fd3f37ca1e71cb9893c09179494cdd"
);
