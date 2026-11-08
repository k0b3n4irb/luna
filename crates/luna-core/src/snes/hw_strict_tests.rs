//! Hardware strict mode: one injected fault per rule, healthy cases at zero,
//! and the edge of `VBlank` measured through the real CPU path. Every ROM is
//! synthetic, built here.

use super::*;
use crate::hw_strict::{HwFault, HwRule};
use std::collections::BTreeSet;

/// A 32 KB `LoROM` whose code at `$8000` is `code`, followed by a branch back
/// to `loop_at` (a `$8000`-relative offset into `code`).
fn rom_looping(code: &[u8], loop_at: usize) -> Cartridge {
    let mut rom = vec![0xEA; 32 * 1024];
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let off = 0x7FC0;
    for (i, b) in b"LUNA STRICT TEST     ".iter().enumerate() {
        rom[off + i] = *b;
    }
    rom[off + 0x15] = 0x20;
    rom[off + 0x17] = 0x05;
    rom[off + 0x19] = 0x01;
    rom[off + 0x1C] = 0x34;
    rom[off + 0x1D] = 0x12;
    rom[off + 0x1E] = 0xCB;
    rom[off + 0x1F] = 0xED;
    rom[..code.len()].copy_from_slice(code);
    // BRA loop_at
    let from = code.len() + 2;
    rom[code.len()] = 0x80;
    rom[code.len() + 1] = (loop_at as isize - from as isize) as i8 as u8;
    Cartridge::from_bytes(rom).unwrap()
}

/// `LDA #inidisp ; STA $2100 ; [LDA #setini ; STA $2133 ;] <access> ; loop`.
/// The access is looped; its address is `$8000 + the returned offset`.
fn rom_access(inidisp: u8, setini: Option<u8>, access: &[u8]) -> (Cartridge, u32) {
    let mut code = vec![0xA9, inidisp, 0x8D, 0x00, 0x21];
    if let Some(s) = setini {
        code.extend([0xA9, s, 0x8D, 0x33, 0x21]);
    }
    let at = code.len();
    code.extend(access);
    (rom_looping(&code, at), 0x8000 + at as u32)
}

const STA_VMDATAL: [u8; 3] = [0x8D, 0x18, 0x21];
const STA_VMDATAH: [u8; 3] = [0x8D, 0x19, 0x21];
const STA_OAMDATA: [u8; 3] = [0x8D, 0x04, 0x21];
const STA_CGDATA: [u8; 3] = [0x8D, 0x22, 0x21];
const LDA_OAMREAD: [u8; 3] = [0xAD, 0x38, 0x21];
const LDA_VMREADL: [u8; 3] = [0xAD, 0x39, 0x21];
const LDA_VMREADH: [u8; 3] = [0xAD, 0x3A, 0x21];
const LDA_CGREAD: [u8; 3] = [0xAD, 0x3B, 0x21];
const LDA_STAT78: [u8; 3] = [0xAD, 0x3F, 0x21];

/// Step `frames` frames with the journal on, draining it after every
/// instruction: each drained fault is a fresh occurrence, stamped with the
/// beam position where it happened.
fn run_journal(cart: Cartridge, frames: u64) -> Vec<HwFault> {
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.enable_hw_strict();
    let stop = snes.frame_count + frames;
    let mut all = Vec::new();
    while snes.frame_count < stop {
        snes.step();
        all.extend(snes.take_hw_faults().0);
    }
    all
}

fn lines(faults: &[HwFault]) -> BTreeSet<u16> {
    faults.iter().map(|f| f.line).collect()
}

#[test]
fn r1_vram_write_is_flagged_on_every_picture_line_and_only_there() {
    for access in [STA_VMDATAL, STA_VMDATAH] {
        let (cart, pc) = rom_access(0x0F, None, &access);
        let faults = run_journal(cart, 2);
        assert!(faults.iter().all(|f| f.rule == HwRule::VramWrite));
        assert!(
            faults
                .iter()
                .all(|f| f.pc == pc && f.origin == MemOrigin::Cpu)
        );
        assert!(
            faults
                .iter()
                .all(|f| f.addr == 0x2100 | u32::from(access[1]))
        );
        let l = lines(&faults);
        // Line 0 included (ares `vcounter() < vdisp()`), VBlank (225..) not.
        assert_eq!(l.first(), Some(&0));
        assert_eq!(l.last(), Some(&224), "VBlank edge, NTSC");
        assert_eq!(l.len(), 225, "every picture line saw the write");
    }
}

#[test]
fn r1_vblank_edge_moves_to_line_240_under_overscan() {
    let (cart, _) = rom_access(0x0F, Some(0x04), &STA_VMDATAL);
    let l = lines(&run_journal(cart, 2));
    assert_eq!(l.last(), Some(&239), "VBlank edge, overscan");
}

#[test]
fn r2_oam_write_is_flagged_outside_vblank() {
    let (cart, pc) = rom_access(0x0F, None, &STA_OAMDATA);
    let faults = run_journal(cart, 2);
    assert!(!faults.is_empty());
    assert!(
        faults
            .iter()
            .all(|f| f.rule == HwRule::OamWrite && f.pc == pc && f.addr == 0x2104)
    );
    assert_eq!(lines(&faults).last(), Some(&224));
    assert_eq!(lines(&faults).first(), Some(&0));
}

#[test]
fn r3_cgram_write_is_flagged_only_in_the_colour_fetch_window() {
    let (cart, pc) = rom_access(0x0F, None, &STA_CGDATA);
    let faults = run_journal(cart, 2);
    assert!(!faults.is_empty());
    assert!(
        faults
            .iter()
            .all(|f| f.rule == HwRule::CgramWrite && f.pc == pc && f.addr == 0x2122)
    );
    let l = lines(&faults);
    assert_eq!(l.first(), Some(&1), "line 0 is not fetched");
    assert_eq!(l.last(), Some(&224));
    // The access is stamped at the start of its last cycle sequence, which
    // lies inside `[88, 1096)` modulo the instruction's own length.
    assert!(faults.iter().all(|f| (88..1096).contains(&f.hclock)));
}

#[test]
fn r4_ppu_memory_reads_are_flagged_while_the_picture_is_drawn() {
    for access in [LDA_OAMREAD, LDA_VMREADL, LDA_VMREADH, LDA_CGREAD] {
        let (cart, pc) = rom_access(0x0F, None, &access);
        let faults = run_journal(cart, 2);
        assert!(!faults.is_empty(), "read of {:#x}", access[1]);
        assert!(
            faults
                .iter()
                .all(|f| f.rule == HwRule::PpuRead && f.pc == pc)
        );
        assert_eq!(lines(&faults).last(), Some(&224));
    }
}

#[test]
fn forced_blank_and_status_reads_raise_nothing() {
    for access in [
        STA_VMDATAL,
        STA_VMDATAH,
        STA_OAMDATA,
        STA_CGDATA,
        LDA_OAMREAD,
        LDA_VMREADL,
        LDA_VMREADH,
        LDA_CGREAD,
    ] {
        let (cart, _) = rom_access(0x80, None, &access);
        assert!(run_journal(cart, 2).is_empty(), "{access:02x?}");
    }
    // A register that no rule names, with the screen on.
    let (cart, _) = rom_access(0x0F, None, &LDA_STAT78);
    assert!(run_journal(cart, 2).is_empty());
}

#[test]
fn the_journal_is_off_by_default_and_never_changes_the_emulation() {
    let (cart, _) = rom_access(0x0F, None, &STA_VMDATAL);
    let run = |strict: bool| {
        let mut snes = Snes::from_cartridge(cart.clone());
        snes.reset();
        if strict {
            snes.enable_hw_strict();
        }
        while snes.frame_count < 3 {
            snes.step();
        }
        snes
    };
    let (plain, strict) = (run(false), run(true));
    assert!(!plain.hw_strict_enabled());
    assert_eq!(plain.total_mclk, strict.total_mclk);
    assert_eq!(plain.ppu.framebuffer(), strict.ppu.framebuffer());
    assert_eq!(plain.ppu.vram.address, strict.ppu.vram.address);
    assert_eq!(plain.wram[..], strict.wram[..]);
}

/// A one-byte DMA on channel 0 between A-bus `src` and B-bus register
/// `$21<b>` (`dmap` = `$00` A->B, `$80` B->A), started in a loop, with the
/// screen on or off (`inidisp`).
fn rom_dma(inidisp: u8, dmap: u8, b: u8, src: u32) -> Cartridge {
    let code = [
        0xA9,
        inidisp,
        0x8D,
        0x00,
        0x21, // LDA #inidisp ; STA $2100
        0xA9,
        dmap,
        0x8D,
        0x00,
        0x43, // DMAP0
        0xA9,
        b,
        0x8D,
        0x01,
        0x43, // BBAD0
        0xA9,
        src as u8,
        0x8D,
        0x02,
        0x43, // A1T0L
        0xA9,
        (src >> 8) as u8,
        0x8D,
        0x03,
        0x43, // A1T0H
        0xA9,
        (src >> 16) as u8,
        0x8D,
        0x04,
        0x43, // A1B0
        0xA9,
        0x01,
        0x8D,
        0x05,
        0x43,
        0xA9,
        0x00,
        0x8D,
        0x06,
        0x43, // DAS0 = 1
        0xA9,
        0x01,
        0x8D,
        0x0B,
        0x42, // STA $420B
    ];
    rom_looping(&code, 5)
}

/// A one-byte DMA from `$00:8000` (ROM) to B-bus register `$21<b>`.
fn rom_dma_to(inidisp: u8, b: u8) -> Cartridge {
    rom_dma(inidisp, 0x00, b, 0x00_8000)
}

#[test]
fn a_dma_into_a_ppu_port_is_judged_like_a_cpu_write() {
    for (b, rule) in [
        (0x18, HwRule::VramWrite),
        (0x04, HwRule::OamWrite),
        (0x22, HwRule::CgramWrite),
    ] {
        let faults = run_journal(rom_dma_to(0x0F, b), 2);
        assert!(!faults.is_empty(), "port {b:#x}");
        assert!(
            faults
                .iter()
                .all(|f| f.rule == rule && f.origin == MemOrigin::Dma(0)),
            "port {b:#x}"
        );
        assert_eq!(lines(&faults).last(), Some(&224));
    }
    // The same DMA in forced blank is clean.
    assert!(run_journal(rom_dma_to(0x80, 0x18), 2).is_empty());
}

#[test]
fn r5_a_dma_through_the_forbidden_a_bus_zone_is_flagged() {
    // Reads from each forbidden window, writes into one (B->A).
    for src in [0x00_2100, 0x00_4000, 0x00_4210, 0x80_4300, 0x3F_437F] {
        let faults = run_journal(rom_dma(0x80, 0x00, 0x18, src), 2);
        assert!(!faults.is_empty(), "src {src:#08x}");
        assert!(faults.iter().all(|f| f.rule == HwRule::DmaForbiddenA
            && f.addr == src
            && f.origin == MemOrigin::Dma(0)));
    }
    let faults = run_journal(rom_dma(0x80, 0x80, 0x39, 0x00_4300), 2);
    assert!(faults.iter().any(|f| f.rule == HwRule::DmaForbiddenA));
    // Healthy: ROM, WRAM, the open $4400 edge, a bank the rule does not
    // cover (bank $40 reaches no I/O).
    for src in [0x00_8000, 0x7E_0000, 0x00_4400, 0x40_2100] {
        let faults = run_journal(rom_dma(0x80, 0x00, 0x18, src), 2);
        assert!(
            faults.iter().all(|f| f.rule != HwRule::DmaForbiddenA),
            "src {src:#08x}"
        );
    }
}

#[test]
fn r6_wram_to_the_wmdata_port_is_flagged_rom_to_it_is_not() {
    for src in [0x7E_1000, 0x7F_0000, 0x00_0100, 0x80_1FFF] {
        let faults = run_journal(rom_dma(0x80, 0x00, 0x80, src), 2);
        assert!(!faults.is_empty(), "src {src:#08x}");
        assert!(
            faults
                .iter()
                .all(|f| f.rule == HwRule::DmaWramPort && f.addr == src)
        );
    }
    // ROM -> WMDATA and WRAM -> VRAM data port (forced blank) are fine.
    assert!(run_journal(rom_dma(0x80, 0x00, 0x80, 0x00_8000), 2).is_empty());
    assert!(run_journal(rom_dma(0x80, 0x00, 0x18, 0x7E_0000), 2).is_empty());
}

/// `LDA #nmitimen ; STA $4200 ; loop: LDA $40<port>`.
fn rom_joy_read(nmitimen: u8, port: u8) -> (Cartridge, u32) {
    let code = [0xA9, nmitimen, 0x8D, 0x00, 0x42, 0xAD, port, 0x40];
    (rom_looping(&code, 5), 0x8005)
}

#[test]
fn r8_a_manual_joypad_read_during_the_auto_read_is_flagged() {
    for port in [0x16, 0x17] {
        let (cart, pc) = rom_joy_read(0x01, port);
        let faults = run_journal(cart, 3);
        assert!(!faults.is_empty(), "port {port:#x}");
        assert!(faults.iter().all(|f| f.rule == HwRule::AutoJoypadRead
            && f.pc == pc
            && f.addr == 0x4000 | u32::from(port)));
        // Only in the few lines after VBlank starts, never on a picture line.
        assert!(faults.iter().all(|f| f.line >= 225));
    }
}

#[test]
fn r8_stays_quiet_without_auto_joypad_and_outside_its_window() {
    // Auto-read off: every read is a manual read.
    let (cart, _) = rom_joy_read(0x00, 0x16);
    assert!(run_journal(cart, 3).is_empty());
    // Auto-read on but the reads are of an unrelated port.
    let code = [0xA9, 0x01, 0x8D, 0x00, 0x42, 0xAD, 0x12, 0x42];
    assert!(run_journal(rom_looping(&code, 5), 3).is_empty());
}

/// `LDA #nmitimen ; STA $4200 ; loop: LDA $42<reg>`.
fn rom_joy_reg_read(nmitimen: u8, reg: u8) -> (Cartridge, u32) {
    let code = [0xA9, nmitimen, 0x8D, 0x00, 0x42, 0xAD, reg, 0x42];
    (rom_looping(&code, 5), 0x8005)
}

#[test]
fn r9_a_joypad_register_read_during_the_auto_read_is_flagged() {
    for reg in 0x18..=0x1F {
        let (cart, pc) = rom_joy_reg_read(0x01, reg);
        let faults = run_journal(cart, 3);
        assert!(!faults.is_empty(), "reg {reg:#x}");
        assert!(
            faults.iter().all(|f| f.rule == HwRule::AutoJoypadRegRead
                && f.pc == pc
                && f.addr == 0x4200 | u32::from(reg)),
            "reg {reg:#x}: {faults:?}"
        );
        // Only in the window after VBlank starts, never on a picture line.
        assert!(faults.iter().all(|f| f.line >= 225));
    }
}

#[test]
fn r9_stays_quiet_without_auto_joypad_and_for_other_registers() {
    // Auto-read off: the registers are never being filled.
    let (cart, _) = rom_joy_reg_read(0x00, 0x18);
    assert!(run_journal(cart, 3).is_empty());
    // $4212 alone is how a program waits, not a fault.
    let (cart, _) = rom_joy_reg_read(0x01, 0x12);
    assert!(run_journal(cart, 3).is_empty());
}

#[test]
fn r9_stays_quiet_when_the_program_waits_for_busy_to_fall() {
    // Wait for busy to rise, then to fall, then read $4218 (once $4212
    // bit 0 is clear), and start again.
    let code = [
        0xA9, 0x01, 0x8D, 0x00, 0x42, // enable
        0xAD, 0x12, 0x42, 0x4A, 0x90, 0xFA, // rise: LDA $4212 ; LSR ; BCC rise
        0xAD, 0x12, 0x42, 0x4A, 0xB0, 0xFA, // fall: LDA $4212 ; LSR ; BCS fall
        0xAD, 0x18, 0x42, // LDA $4218
    ];
    assert!(run_journal(rom_looping(&code, 5), 3).is_empty());
}

/// `CLC ; XCE ; REP #$10 ; LDX #x ; STX abs` : a 16-bit store whose low byte
/// lands on `abs` and its high byte on `abs + 1`, so the bus holds the low
/// byte when the second register is written. Returns the access bytes and the
/// offset of the `STX` in them.
fn stx16(x: u16, abs: u16) -> (Vec<u8>, u32) {
    let code = vec![
        0x18,
        0xFB,
        0xC2,
        0x10,
        0xA2,
        x as u8,
        (x >> 8) as u8,
        0x8E,
        abs as u8,
        (abs >> 8) as u8,
    ];
    (code, 7)
}

fn early_latch_faults(inidisp: u8, access: &[u8]) -> Vec<HwFault> {
    let (cart, _) = rom_access(inidisp, None, access);
    run_journal(cart, 2)
}

#[test]
fn r10_inidisp_written_with_bit_7_on_the_bus_is_flagged_on_every_picture_line() {
    // Bus byte 0x8F (written to $20FF), then INIDISP <- 0x0F.
    let (access, stx) = stx16(0x0F8F, 0x20FF);
    let (cart, pc) = rom_access(0x0F, None, &access);
    let faults = run_journal(cart, 2);
    assert!(!faults.is_empty());
    assert!(
        faults.iter().all(|f| f.rule == HwRule::InidispEarlyLatch
            && f.pc == pc + stx
            && f.addr == 0x2100
            && f.origin == MemOrigin::Cpu),
        "{faults:?}"
    );
    let l = lines(&faults);
    assert_eq!(l.first(), Some(&0));
    assert_eq!(l.last(), Some(&224), "silent from the first VBlank line");
}

#[test]
fn r10_stays_quiet_when_the_bus_or_the_value_makes_it_safe() {
    // Bus bit 7 clear (0x0F then 0x0F).
    let (access, _) = stx16(0x0F0F, 0x20FF);
    assert!(early_latch_faults(0x0F, &access).is_empty());
    // Entering force blank: the value has bit 7 set (0x8F, 0x8F).
    let (access, _) = stx16(0x8F8F, 0x20FF);
    assert!(early_latch_faults(0x0F, &access).is_empty());
    // A plain `sta $2100`: the last byte on the bus is the address high byte.
    let access = [0xA9, 0x0F, 0x8D, 0x00, 0x21];
    assert!(early_latch_faults(0x0F, &access).is_empty());
    // The mitigation of the hardware test: `sta.l $0f2100`, bank byte 0x0F.
    let access = [0xA9, 0x0F, 0x8F, 0x00, 0x21, 0x0F];
    assert!(early_latch_faults(0x0F, &access).is_empty());
}

#[test]
fn r10_flags_a_long_store_through_a_bank_with_bit_7_set() {
    // `sta.l $802100`: the bank byte 0x80 is the last byte on the bus.
    let access = [0xA9, 0x0F, 0x8F, 0x00, 0x21, 0x80];
    let faults = early_latch_faults(0x0F, &access);
    assert!(!faults.is_empty());
    assert!(faults.iter().all(|f| f.rule == HwRule::InidispEarlyLatch));
}

#[test]
fn r11_setini_written_over_a_different_bus_byte_is_flagged_when_drawing() {
    // $2132 <- 0x00 then SETINI <- 0x40: the bus holds 0x00.
    let (access, stx) = stx16(0x4000, 0x2132);
    let (cart, pc) = rom_access(0x0F, None, &access);
    let faults = run_journal(cart, 2);
    assert!(!faults.is_empty());
    assert!(
        faults.iter().all(|f| f.rule == HwRule::SetiniEarlyLatch
            && f.pc == pc + stx
            && f.addr == 0x2133
            && f.origin == MemOrigin::Cpu),
        "{faults:?}"
    );
    let l = lines(&faults);
    assert_eq!(l.first(), Some(&0));
    assert_eq!(l.last(), Some(&224));
}

#[test]
fn r11_stays_quiet_in_force_blank_or_when_the_bits_agree() {
    let (access, _) = stx16(0x4000, 0x2132);
    assert!(early_latch_faults(0x8F, &access).is_empty(), "force blank");
    // Bus 0x40, value 0x40.
    let (access, _) = stx16(0x4040, 0x2132);
    assert!(early_latch_faults(0x0F, &access).is_empty());
    // A plain `sta $2133`: the bus holds the address high byte 0x21.
    let access = [0xA9, 0x00, 0x8D, 0x33, 0x21];
    assert!(early_latch_faults(0x0F, &access).is_empty());
}

#[test]
fn r10_and_r11_ignore_a_dma_to_the_registers() {
    // A DMA writes through the B bus, with no data-bus byte of its own.
    let cart = rom_dma_to(0x0F, 0x00);
    assert!(
        run_journal(cart, 2)
            .iter()
            .all(|f| f.rule != HwRule::InidispEarlyLatch)
    );
}

/// Force blank, CGRAM entry 0 = `colour`, then `$2100 <- brightness` (the
/// instruction reported by R12 is the `STA $2100` at the returned address).
fn rom_brightness(colour: u16, brightness: u8) -> (Cartridge, u32) {
    let code = vec![
        0xA9,
        0x80,
        0x8D,
        0x00,
        0x21, // force blank
        0x9C,
        0x21,
        0x21, // CGADD = 0
        0xA9,
        colour as u8,
        0x8D,
        0x22,
        0x21, // CGDATA low
        0xA9,
        (colour >> 8) as u8,
        0x8D,
        0x22,
        0x21, // CGDATA high
        0xA9,
        brightness,
        0x8D,
        0x00,
        0x21, // INIDISP
    ];
    let at = code.len();
    (rom_looping(&code, at), 0x8000 + (at as u32 - 3))
}

fn r12_faults(cart: Cartridge) -> Vec<HwFault> {
    run_journal(cart, 4)
        .into_iter()
        .filter(|f| f.rule == HwRule::BrightnessZero)
        .collect()
}

#[test]
fn r12_brightness_zero_over_a_lit_picture_is_flagged_once_per_frame() {
    let (cart, pc) = rom_brightness(0x7FFF, 0x00);
    let faults = r12_faults(cart);
    assert!(!faults.is_empty());
    assert!(
        faults.iter().all(|f| f.pc == pc
            && f.addr == 0x2100
            && f.origin == MemOrigin::Cpu
            && f.line == 224
            && f.count == 1),
        "{faults:?} (expected pc {pc:#x})"
    );
    // One fresh occurrence per whole frame, never two on the same frame.
    let frames: BTreeSet<u64> = faults.iter().map(|f| f.frame).collect();
    assert_eq!(frames.len(), faults.len());
}

#[test]
fn r12_stays_quiet_when_the_brightness_is_not_zero() {
    for b in [0x01, 0x08, 0x0F] {
        let (cart, _) = rom_brightness(0x7FFF, b);
        assert!(r12_faults(cart).is_empty(), "brightness {b}");
    }
}

#[test]
fn r12_stays_quiet_in_force_blank() {
    // Bit 7 set, brightness 0: the screen is blanked, not dim.
    let (cart, _) = rom_brightness(0x7FFF, 0x80);
    assert!(r12_faults(cart).is_empty());
}

#[test]
fn r12_stays_quiet_over_a_black_picture() {
    // Brightness 0 over black is black on a console too: the usual
    // "blank by brightness" of a loading screen.
    let (cart, _) = rom_brightness(0x0000, 0x00);
    assert!(r12_faults(cart).is_empty());
}

#[test]
fn r12_stays_quiet_when_the_brightness_toggles_every_few_cycles() {
    // `INIDISP` 0 then 15, forever: no whole frame is dark.
    let code = [
        0xA9, 0x80, 0x8D, 0x00, 0x21, 0x9C, 0x21, 0x21, 0xA9, 0xFF, 0x8D, 0x22, 0x21, 0xA9, 0x7F,
        0x8D, 0x22, 0x21, 0xA9, 0x00, 0x8D, 0x00, 0x21, 0xA9, 0x0F, 0x8D, 0x00, 0x21,
    ];
    assert!(r12_faults(rom_looping(&code, 18)).is_empty());
}
