//! The machines the `load_state` target restores into: one synthetic
//! cartridge per mapper kind whose save-state blob has its own decoder.
//!
//! Each coprocessor cart boots a few instructions of 65C816 that start the
//! chip and then keep touching it, so the pristine state is a chip caught
//! mid-work and the steps after a load run straight into whatever was
//! restored. [`build`] checks that liveness once per process: a fixture
//! that quietly stopped exercising its chip would fuzz nothing.

use luna_api::Emulator;
use luna_core::MapperKind;

/// Machine kinds, in selector order. [`LOROM`] is the pre-existing one.
pub const LOROM: usize = 0;
const SUPERFX: usize = 1;
const SDD1: usize = 2;
const SA1: usize = 3;
/// How many machines [`build`] returns; the last one is the DSP-1.
const KINDS: usize = 5;

/// `LoROM`-style header offset (also SA-1 and Super FX when forced).
const HEADER_LO: usize = 0x7FC0;
/// `HiROM`-style header offset (S-DD1 when forced).
const HEADER_HI: usize = 0xFFC0;

/// Write the checksum pair for the header at `header`.
fn stamp_checksum(rom: &mut [u8], header: usize) {
    let skip = header + 0x1C..header + 0x20;
    let sum: u32 = rom
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip.contains(i))
        .map(|(_, b)| u32::from(*b))
        .sum();
    let checksum = (sum & 0xFFFF) as u16;
    rom[header + 0x1C..header + 0x1E].copy_from_slice(&(!checksum).to_le_bytes());
    rom[header + 0x1E..header + 0x20].copy_from_slice(&checksum.to_le_bytes());
}

/// Deterministic filler (an LCG): dense, non-repeating bytes for a
/// decompressor, a GSU or a DSP to chew on.
fn noise(out: &mut [u8], mut seed: u32) {
    for b in out {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (seed >> 24) as u8;
    }
}

/// `size` bytes of zeroes with `code` at the reset vector's target
/// (`$00:8000`, ROM offset 0 on every board here).
fn cart(size: usize, code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; size];
    rom[..code.len()].copy_from_slice(code);
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    rom
}

/// A 32 KB `LoROM` with 8 KB of SRAM: `SEI ; loop: BRA loop`.
///
/// Byte-for-byte the cart this target has always used: the committed seeds
/// that are whole containers carry its ROM hash.
fn lorom() -> Vec<u8> {
    let mut rom = cart(0x8000, &[0x78, 0x80, 0xFE]);
    rom[HEADER_LO..HEADER_LO + 21].copy_from_slice(b"LUNA FUZZ LOAD STATE ");
    rom[HEADER_LO + 0x15] = 0x20; // LoROM
    rom[HEADER_LO + 0x17] = 0x05; // 32 KB
    rom[HEADER_LO + 0x18] = 0x03; // 8 KB SRAM
    stamp_checksum(&mut rom, HEADER_LO);
    rom
}

/// Super FX with 2 KB of Game Pak RAM. The CPU copies a GSU program into
/// that RAM, hands the RAM over (SCMR `RAN`) and starts the GSU in bank
/// `$70`, then polls SFR — it keeps the ROM, so its own code stays visible.
fn superfx() -> Vec<u8> {
    #[rustfmt::skip]
    let code = [
        0x78,                   //       SEI
        0xA2, 0x00,             //       LDX #$00
        0xBD, 0x40, 0x80,       // copy: LDA $8040,X
        0x9F, 0x00, 0x00, 0x70, //       STA $700000,X
        0xE8,                   //       INX
        0xD0, 0xF6,             //       BNE copy          (256 bytes)
        0xA9, 0x08,             //       LDA #$08
        0x8D, 0x3A, 0x30,       //       STA $303A         SCMR = RAN
        0xA9, 0x70,             //       LDA #$70
        0x8D, 0x34, 0x30,       //       STA $3034         PBR = $70
        0xA9, 0x00,             //       LDA #$00
        0x8D, 0x1E, 0x30,       //       STA $301E         R15 low
        0x8D, 0x1F, 0x30,       //       STA $301F         R15 high: GO
        0xAD, 0x30, 0x30,       // poll: LDA $3030
        0x80, 0xFB,             //       BRA poll
    ];
    #[rustfmt::skip]
    let gsu = [
        0xF3, 0x00, 0x04, //       IWT R3,#$0400     scratch word, clear of the code
        0xA2, 0x10,       //       IBT R2,#$10
        0xA0, 0x05,       //       IBT R0,#$05
        0xA1, 0x10,       // loop: IBT R1,#$10       x (PLOT advances it)
        0x4E,             //       COLOR
        0x4C,             //       PLOT              tiles at $0220-$03FF
        0x50,             //       ADD R0
        0xD2,             //       INC R2            y
        0x3D, 0x4C,       //       RPIX
        0x33,             //       STW (R3)
        0x43,             //       LDW (R3)
        0x05, 0xF4,       //       BRA loop
        0x01,             //       NOP               (delay slot)
    ];
    let mut rom = cart(0x8000, &code);
    // The 256 bytes the CPU copies: the program, then a NOP sled that
    // branches back, so the GSU never runs off into a STOP.
    rom[0x40..0x140].fill(0x01);
    rom[0x40..0x40 + gsu.len()].copy_from_slice(&gsu);
    rom[0x13C..0x140].copy_from_slice(&[0x05, 0x80, 0x01, 0x01]); // BRA back
    rom[HEADER_LO - 3] = 0x01; // expansion RAM: 2 KB
    rom
}

/// S-DD1 with 2 KB of SRAM. The CPU arms channel 0 for a 64 KiB
/// decompressing transfer from `$C0:0000` and then reads that address for
/// ever, so every read is one byte out of the streaming decompressor. The
/// compressed input is the ROM itself (code, then noise).
fn sdd1() -> Vec<u8> {
    #[rustfmt::skip]
    let code = [
        0x78,                   //       SEI
        0xA9, 0x01,             //       LDA #$01
        0x8D, 0x00, 0x48,       //       STA $4800         channel 0 eligible
        0x8D, 0x01, 0x48,       //       STA $4801         channel 0 armed
        0xA9, 0x00,             //       LDA #$00
        0x8D, 0x02, 0x43,       //       STA $4302
        0x8D, 0x03, 0x43,       //       STA $4303
        0x8D, 0x05, 0x43,       //       STA $4305
        0x8D, 0x06, 0x43,       //       STA $4306         length 0 = 64 KiB
        0xA9, 0xC0,             //       LDA #$C0
        0x8D, 0x04, 0x43,       //       STA $4304         source $C0:0000
        0xA2, 0x00,             //       LDX #$00
        0xAF, 0x00, 0x00, 0xC0, // read: LDA $C00000       one stream byte
        0x95, 0x10,             //       STA $10,X
        0xE8,                   //       INX
        0x80, 0xF7,             //       BRA read
    ];
    let mut rom = cart(0x1_0000, &code);
    noise(&mut rom[0x100..0x7F00], 0x5DD1);
    noise(&mut rom[0x8000..0xFF00], 0x1DD5);
    rom[HEADER_HI + 0x18] = 0x01; // 2 KB SRAM
    rom
}

/// SA-1 with 2 KB of BW-RAM. The CPU points the SA-1 reset vector at
/// `$8100` and releases the chip, then keeps reading I-RAM and SFR; the
/// SA-1 loops over an I-RAM write, a variable-length-bit read and the
/// arithmetic unit, with its timer running underneath.
fn sa1() -> Vec<u8> {
    #[rustfmt::skip]
    let code = [
        0x78,             //       SEI
        0xA9, 0x00,       //       LDA #$00
        0x8D, 0x03, 0x22, //       STA $2203         CRV low
        0xA9, 0x81,       //       LDA #$81
        0x8D, 0x04, 0x22, //       STA $2204         CRV = $8100
        0xA9, 0x00,       //       LDA #$00
        0x8D, 0x00, 0x22, //       STA $2200         CCNT: release the SA-1
        0xAD, 0x00, 0x30, // spin: LDA $3000
        0xAD, 0x00, 0x23, //       LDA $2300
        0x80, 0xF8,       //       BRA spin
    ];
    #[rustfmt::skip]
    let sa1_code = [
        0xEE, 0x00, 0x30, // loop: INC $3000
        0xAD, 0x0C, 0x23, //       LDA $230C         VLBP window
        0x8D, 0x51, 0x22, //       STA $2251         MA
        0x8D, 0x53, 0x22, //       STA $2253         MB
        0x8D, 0x54, 0x22, //       STA $2254         run the arithmetic
        0x80, 0xEF,       //       BRA loop
    ];
    let mut rom = cart(0x8000, &code);
    rom[0x100..0x100 + sa1_code.len()].copy_from_slice(&sa1_code);
    rom[HEADER_LO + 0x18] = 0x01; // 2 KB BW-RAM
    rom
}

/// DSP-1 on the `LoROM` board, firmware appended to the dump the way some
/// real dumps carry it. The firmware is noise — arbitrary microcode, which
/// is all a restored `pc` / `rp` / `dp` / `sp` needs to be consumed — and
/// the CPU keeps polling SR and moving bytes through DR.
///
/// This one is detected from its header (a forced DSP-1 gets no firmware,
/// and without firmware the chip is never clocked).
fn dsp1() -> Vec<u8> {
    #[rustfmt::skip]
    let code = [
        0x78,                   //       SEI
        0xAF, 0x00, 0xC0, 0x20, // loop: LDA $20C000       SR
        0xAF, 0x00, 0x80, 0x20, //       LDA $208000       DR
        0x8F, 0x00, 0x80, 0x20, //       STA $208000       DR
        0x80, 0xF2,             //       BRA loop
    ];
    let mut rom = cart(0x8000, &code);
    rom[HEADER_LO..HEADER_LO + 21].copy_from_slice(b"SUPER MARIO KART     ");
    rom[HEADER_LO + 0x15] = 0x20; // LoROM
    rom[HEADER_LO + 0x16] = 0x05; // ROM + RAM + battery + DSP
    rom[HEADER_LO + 0x17] = 0x05; // 32 KB
    stamp_checksum(&mut rom, HEADER_LO);
    let mut firmware = [0u8; 0x2000];
    noise(&mut firmware, 0xD5B1);
    rom.extend_from_slice(&firmware);
    rom
}

/// Instructions each coprocessor cart runs before its pristine state is
/// taken: past its boot code, well into the chip's loop.
const WARM_UP: u64 = 20_000;

/// Build every machine with its pristine state, indexed by kind.
///
/// Panics if a coprocessor cart does not load as its kind or its chip is
/// not running after the warm-up — the fixture is then not testing what
/// this target claims.
pub fn build() -> Vec<(Emulator, Vec<u8>)> {
    let mut machines = Vec::with_capacity(KINDS);
    for kind in 0..KINDS {
        let mut emu = Emulator::new();
        let info = match kind {
            LOROM => emu.load_rom_bytes(lorom()),
            SUPERFX => emu.load_rom_bytes_forced(superfx(), MapperKind::SuperFx),
            SDD1 => emu.load_rom_bytes_forced(sdd1(), MapperKind::Sdd1),
            SA1 => emu.load_rom_bytes_forced(sa1(), MapperKind::Sa1),
            _ => emu.load_rom_bytes(dsp1()),
        };
        info.expect("the fixed ROM loads");
        if kind != LOROM {
            emu.step(WARM_UP).expect("the fixed ROM runs");
            assert_live(kind, &mut emu);
        }
        let good = emu.save_state().expect("the machine saves");
        machines.push((emu, good));
    }
    machines
}

/// The chip of machine `kind` has done real work since power-on.
fn assert_live(kind: usize, emu: &mut Emulator) {
    match kind {
        SUPERFX => {
            let gsu = emu.state().gsu.expect("a Super FX cartridge");
            assert!(gsu.running, "the GSU stopped");
            assert!(gsu.instructions_executed > 1_000, "the GSU barely ran");
        }
        SDD1 => {
            // A read that is not decompressed returns ROM byte 0 for ever.
            let seen = emu.peek_memory(0x7E, 0x0010, 64).expect("WRAM");
            assert!(
                seen.iter().any(|&b| b != seen[0]),
                "the S-DD1 stream is not decompressing"
            );
        }
        SA1 => {
            let stats = emu.sa1_stats().expect("a ROM").expect("an SA-1");
            assert!(stats.instructions > 1_000, "the SA-1 barely ran");
        }
        _ => {
            let ran = emu.dsp1_instructions().expect("a ROM").expect("a DSP-1");
            assert!(ran > 1_000, "the DSP-1 barely ran");
        }
    }
}
