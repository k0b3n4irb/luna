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
/// step cap / a `STP` / a CPU panic). Returns the framebuffer bytes.
///
/// The ROM is loaded as **PAL**, matching the `twvd/siena` convention.
/// Peter Lemon's suite is PAL-timed: several tests do a single `WaitNMI`
/// then write the whole result table in one burst that only fits inside
/// PAL's longer V-blank (~72 lines vs NTSC's 37). Run as NTSC, luna
/// correctly drops the writes that overflow into active display and the
/// screen stays blank — so PAL is required to reproduce the reference
/// output.
fn run_to_stable(rom: Vec<u8>) -> Vec<u8> {
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = luna_cartridge::Region::Pal;
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

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
    let bytes = run_to_stable(rom);
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
            test_display(concat!("CPUTest/CPU/", $name, "/CPU", $name, ".sfc"), $hash);
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
