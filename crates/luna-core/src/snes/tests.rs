//! Unit tests (moved out of the parent file to keep it navigable).

use super::*;
use luna_bus::make_addr;

/// Build a 32 KB `LoROM` that starts with `LDA #$42 ; STA $7E0000 ; STP`
/// and has its reset vector pointing at `$8000`.
fn demo_lorom() -> Cartridge {
    let mut rom = vec![0xEA; 32 * 1024]; // NOP-padded
    // Reset vector at $7FFC = $8000 (already in bank 0 LoROM space).
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    // Header at $7FC0.
    let off = 0x7FC0;
    for (i, b) in b"LUNA P0.6 DEMO       ".iter().enumerate() {
        rom[off + i] = *b;
    }
    rom[off + 0x15] = 0x20; // LoROM
    rom[off + 0x17] = 0x05; // 32 KB
    rom[off + 0x18] = 0x00; // no SRAM
    rom[off + 0x19] = 0x01; // NTSC
    // Checksum complement: 0x1234, checksum: !0x1234 = 0xEDCB.
    rom[off + 0x1C] = 0x34;
    rom[off + 0x1D] = 0x12;
    rom[off + 0x1E] = 0xCB;
    rom[off + 0x1F] = 0xED;
    // Program at $8000 (file offset 0):
    //   LDA #$42        A9 42
    //   STA $7E:0000    8F 00 00 7E
    //   STP             DB
    rom[0x0000] = 0xA9;
    rom[0x0001] = 0x42;
    rom[0x0002] = 0x8F;
    rom[0x0003] = 0x00;
    rom[0x0004] = 0x00;
    rom[0x0005] = 0x7E;
    rom[0x0006] = 0xDB;
    Cartridge::from_bytes(rom).unwrap()
}

/// [`demo_lorom`] with the idle program instead — `SEI ; LDA #$80 ;
/// STA $4200 ; loop: WAI ; BRA loop ; nmi: RTI` — and the NMI vectors
/// pointed at the `RTI`, so frames keep advancing forever.
fn idle_lorom() -> Cartridge {
    let mut rom = vec![0xEA; 32 * 1024];
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let off = 0x7FC0;
    for (i, b) in b"LUNA IDLE DEMO       ".iter().enumerate() {
        rom[off + i] = *b;
    }
    rom[off + 0x15] = 0x20;
    rom[off + 0x17] = 0x05;
    rom[off + 0x18] = 0x00;
    rom[off + 0x19] = 0x01;
    rom[off + 0x1C] = 0x34;
    rom[off + 0x1D] = 0x12;
    rom[off + 0x1E] = 0xCB;
    rom[off + 0x1F] = 0xED;
    let prog = [0x78, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0xCB, 0x80, 0xFD, 0x40];
    rom[..prog.len()].copy_from_slice(&prog);
    for v in [0x7FEA, 0x7FFA] {
        rom[v] = 0x09;
        rom[v + 1] = 0x80;
    }
    Cartridge::from_bytes(rom).unwrap()
}

#[test]
fn from_cartridge_sets_initial_state() {
    let cart = demo_lorom();
    let snes = Snes::from_cartridge(cart);
    assert_eq!(snes.total_mclk, 0);
    assert!(!snes.nmi_pending);
    assert!(!snes.irq_pending);
}

#[test]
fn scanline_helpers_pick_per_region_constants() {
    assert_eq!(scanlines_per_frame(luna_cartridge::Region::Ntsc), 262);
    assert_eq!(scanlines_per_frame(luna_cartridge::Region::Pal), 312);
    // VBlank entry follows OVERSCAN, not the region (ares io.cpp:641,
    // Mesen2 SnesPpu.cpp:559): PAL differs only in total scanlines.
    assert_eq!(vblank_start_line(false), 225);
    assert_eq!(vblank_start_line(true), 240);
}

/// Drive the scheduler until `VBlank` is entered (the `$4210` NMI flag
/// rises) and report the PPU line it happened on. The test ROM never
/// reads `$4210`, so the first `true` is the entry line.
fn vblank_entry_line(snes: &mut Snes, max_frames: u64) -> Option<u16> {
    let stop = snes.frame_count + max_frames;
    while snes.frame_count < stop {
        snes.step();
        if snes.cpu_regs.nmi_flag {
            return Some(snes.ppu_line);
        }
    }
    None
}

/// ROM that enables NMI (`$4200 = $80`) and then spins.
fn nmi_enable_rom(country: u8, setini: u8) -> Cartridge {
    let mut rom = demo_lorom().rom;
    rom[0x7FD9] = country;
    let mut pc = 0x0000;
    let emit = |bytes: &[u8], rom: &mut Vec<u8>, pc: &mut usize| {
        for &b in bytes {
            rom[*pc] = b;
            *pc += 1;
        }
    };
    if setini != 0 {
        // LDA #setini ; STA $2133
        emit(&[0xA9, setini, 0x8D, 0x33, 0x21], &mut rom, &mut pc);
    }
    // LDA #$80 ; STA $4200 ; BRA -2
    emit(
        &[0xA9, 0x80, 0x8D, 0x00, 0x42, 0x80, 0xFE],
        &mut rom,
        &mut pc,
    );
    Cartridge::from_bytes(rom).unwrap()
}

#[test]
fn vblank_entry_is_line_225_in_both_regions_and_240_under_overscan() {
    // PAL used to be hardcoded to 240: a PAL game without overscan got
    // its NMI 15 lines late (and 15 extra HDMA lines).
    for country in [0x01u8, 0x02] {
        let mut snes = Snes::from_cartridge(nmi_enable_rom(country, 0x00));
        snes.reset();
        assert_eq!(
            vblank_entry_line(&mut snes, 3),
            Some(225),
            "country {country:#04x}: VBlank starts at line 225 without overscan"
        );
    }
    // SETINI bit 2 moves it to 240 — ares recomputes `vdisp` on the write.
    let mut snes = Snes::from_cartridge(nmi_enable_rom(0x01, 0x04));
    snes.reset();
    assert_eq!(vblank_entry_line(&mut snes, 3), Some(240), "overscan armed");
}

#[test]
fn pal_cart_flips_stat78_region_bit_and_propagates_scanlines() {
    // Patch the country byte in our demo ROM to PAL (0x02 = EU).
    let mut rom = demo_lorom().rom;
    rom[0x7FD9] = 0x02;
    let cart = Cartridge::from_bytes(rom).unwrap();
    assert_eq!(cart.header.region, luna_cartridge::Region::Pal);
    let snes = Snes::from_cartridge(cart);
    assert_eq!(snes.region, luna_cartridge::Region::Pal);
    assert_eq!(snes.region_scanlines(), 312);
    // STAT78 bit 4 should reflect the region.
    assert_eq!(snes.ppu.stat78 & 0x10, 0x10);
}

#[test]
fn power_on_random_is_seeded_masks_cgram_and_survives_reset() {
    use crate::power::PowerOnState;
    let mk = |st| Snes::try_from_cartridge_with(demo_lorom(), st).expect("snes");
    let zero = mk(PowerOnState::Zero);
    assert!(zero.wram.iter().all(|&b| b == 0));
    let ones = mk(PowerOnState::Ones);
    assert!(ones.wram.iter().all(|&b| b == 0xFF));
    assert!(ones.apu_real.aram.iter().all(|&b| b == 0xFF));
    // CGRAM stays 15-bit even under `ones`.
    assert!((0..256).all(|i| ones.ppu.cgram.peek(i * 2 + 1) & 0x80 == 0));

    let a = mk(PowerOnState::Random { seed: 99 });
    let b = mk(PowerOnState::Random { seed: 99 });
    let c = mk(PowerOnState::Random { seed: 100 });
    assert_eq!(a.wram[..], b.wram[..]);
    assert_eq!(a.apu_real.aram[..], b.apu_real.aram[..]);
    assert_eq!(a.ppu.vram.peek(0x1234), b.ppu.vram.peek(0x1234));
    assert_ne!(a.wram[..], c.wram[..]);
    assert!(a.wram.iter().any(|&x| x != 0));
    assert!((0..256).all(|i| a.ppu.cgram.peek(i * 2 + 1) & 0x80 == 0));

    // A soft reset keeps every RAM array (ares dsp.cpp:199, cpu.cpp:92).
    let mut r = mk(PowerOnState::Random { seed: 99 });
    let aram_before = r.apu_real.aram.clone();
    let wram_before = r.wram.clone();
    r.reset();
    assert_eq!(r.apu_real.aram[..], aram_before[..]);
    assert_eq!(r.wram[..], wram_before[..]);
}

#[test]
fn reset_loads_pc_from_vector_via_lorom_mapper() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    assert_eq!(snes.cpu.pc, 0x8000);
    assert_eq!(snes.cpu.pb, 0x00);
}

#[test]
fn pal_console_clocks_the_spc_against_the_pal_master_clock() {
    let mut rom = demo_lorom().rom;
    rom[0x7FD9] = 0x02; // PAL
    let mut pal = Snes::from_cartridge(Cartridge::from_bytes(rom).unwrap());
    assert_eq!(
        pal.apu_real.master_clock_hz(),
        luna_apu::PAL_MASTER_CLOCK_HZ
    );
    pal.reset();
    assert_eq!(
        pal.apu_real.master_clock_hz(),
        luna_apu::PAL_MASTER_CLOCK_HZ
    );
    let ntsc = Snes::from_cartridge(demo_lorom());
    assert_eq!(ntsc.apu_real.master_clock_hz(), luna_apu::MASTER_CLOCK_HZ);
}

#[test]
fn debug_peek_and_poke_walk_into_the_next_bank() {
    // A debugger range that runs off the end of a bank continues into
    // the next one: $7E:FFFF + 1 is $7F:0000, the contiguous half of
    // WRAM — not $7E:0000, which used to get clobbered.
    let mut snes = Snes::from_cartridge(demo_lorom());
    assert_eq!(snes.dbg_poke_bytes(0x7E, 0xFFFF, &[0xAA, 0xBB]), 2);
    assert_eq!(snes.wram[0xFFFF], 0xAA, "last byte of $7E");
    assert_eq!(snes.wram[0x1_0000], 0xBB, "first byte of $7F");
    assert_eq!(snes.wram[0], 0, "$7E:0000 untouched");
    assert_eq!(snes.dbg_peek_bytes(0x7E, 0xFFFF, 2), vec![0xAA, 0xBB]);
}

#[test]
fn debug_peek_returns_the_dma_channel_registers() {
    // `$4300-$437F` is the one slice of the register band a debugger
    // may read without side effects, so the peek returns the channel
    // file instead of the band's `0`: `$FF` at power-on (issue #224),
    // and whatever the ROM wrote afterwards.
    let mut snes = Snes::from_cartridge(demo_lorom());
    assert_eq!(snes.dbg_peek_bytes(0x00, 0x4300, 12), vec![0xFF; 12]);
    snes.dma.channels[1].write(0x0, 0x01);
    snes.dma.channels[1].write(0x5, 0x40);
    snes.dma.channels[1].write(0x6, 0x02);
    assert_eq!(
        snes.dbg_peek_bytes(0x80, 0x4310, 7),
        vec![0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x40, 0x02]
    );
    // `$4200-$42FF` and `$4380+` stay in the zero band.
    assert_eq!(snes.dbg_peek_bytes(0x00, 0x42FF, 2), vec![0x00, 0xFF]);
    assert_eq!(snes.dbg_peek_bytes(0x00, 0x437F, 2), vec![0xFF, 0x00]);
}

#[test]
fn diagnostic_logs_stop_growing_at_their_cap() {
    let mut snes = Snes::from_cartridge(demo_lorom());
    snes.enable_mailbox_log();
    if let Some(log) = snes.mailbox_log.as_mut() {
        log.resize(
            DEBUG_LOG_MAX_EVENTS,
            MailboxEvent {
                mclk_total: 0,
                pc_full: 0,
                kind: MailboxEventKind::Read,
                port: 0,
                value: 0,
            },
        );
    }
    // At the cap, further traffic is dropped rather than queued.
    for _ in 0..64 {
        snes.step();
    }
    assert_eq!(
        snes.mailbox_log.as_ref().map(Vec::len),
        Some(DEBUG_LOG_MAX_EVENTS),
        "a full log must stop growing"
    );
    // Draining it re-opens capture.
    assert_eq!(snes.take_mailbox_log().len(), DEBUG_LOG_MAX_EVENTS);
    assert_eq!(snes.mailbox_log.as_ref().map(Vec::len), Some(0));
}

#[test]
fn apu_ports_mirror_every_four_bytes_up_to_217f() {
    assert_eq!(SnesBus::apu_port(make_addr(0x00, 0x2140)), Some(0));
    assert_eq!(SnesBus::apu_port(make_addr(0x00, 0x2145)), Some(1));
    assert_eq!(SnesBus::apu_port(make_addr(0x80, 0x217F)), Some(3));
    assert_eq!(SnesBus::apu_port(make_addr(0x00, 0x2180)), None, "WMDATA");
    assert_eq!(SnesBus::apu_port(make_addr(0x40, 0x2140)), None, "bank $40");
}

#[test]
fn memsel_write_persists_and_powers_on_slow() {
    // Header advertises FastROM ($30), but MEMSEL powers up SLOW; the
    // game's own `$420D` write must then persist past its instruction.
    let mut rom = demo_lorom().rom;
    rom[0x7FD5] = 0x30;
    rom[0x0000] = 0xA9; // LDA #$01
    rom[0x0001] = 0x01;
    rom[0x0002] = 0x8D; // STA $420D
    rom[0x0003] = 0x0D;
    rom[0x0004] = 0x42;
    rom[0x0005] = 0x9C; // STZ $420D
    rom[0x0006] = 0x0D;
    rom[0x0007] = 0x42;
    rom[0x0008] = 0xDB; // STP
    let mut snes = Snes::from_cartridge(Cartridge::from_bytes(rom).unwrap());
    snes.reset();
    assert!(!snes.fast_rom, "MEMSEL powers up slow regardless of header");
    snes.step(); // LDA
    snes.step(); // STA $420D
    assert!(snes.fast_rom, "$420D=1 must persist");
    snes.step(); // STZ $420D
    assert!(!snes.fast_rom, "$420D=0 must persist");
}

#[test]
fn random_power_on_also_randomises_ppu_registers_and_latches() {
    use crate::power::PowerOnState;
    // Issue #224, second lot: ares randomises the PPU's registers,
    // latches and both chip MDRs on power (`ppu.cpp`), not just RAM.
    // A seed still reproduces the exact machine.
    let mk = |st| Snes::try_from_cartridge_with(demo_lorom(), st).expect("snes");
    let zero = mk(PowerOnState::Zero);
    assert_eq!(zero.ppu.ppu1_mdr, 0, "zero leaves the registers alone");
    assert_eq!(zero.ppu.m7a, 0);
    assert_eq!(zero.ppu.setini, 0);

    let a = mk(PowerOnState::Random { seed: 7 });
    let b = mk(PowerOnState::Random { seed: 7 });
    let c = mk(PowerOnState::Random { seed: 8 });
    assert_eq!(a.ppu.ppu1_mdr, b.ppu.ppu1_mdr, "same seed, same machine");
    assert_eq!(a.ppu.m7a, b.ppu.m7a);
    assert_eq!(a.ppu.setini, b.ppu.setini);
    let differs = [
        a.ppu.ppu1_mdr != c.ppu.ppu1_mdr,
        a.ppu.m7a != c.ppu.m7a,
        a.ppu.vram.address != c.ppu.vram.address,
        a.ppu.cgram.address != c.ppu.cgram.address,
    ];
    assert!(
        differs.iter().any(|d| *d),
        "another seed must yield another machine"
    );
    // ares sets these explicitly rather than randomising them.
    assert_eq!(
        a.ppu.setini & 0x05,
        0,
        "overscan and interlace come up clear"
    );
    assert_eq!(a.ppu.bgmode, 0, "BGMODE comes up 0");
}

#[test]
fn dma_channel_registers_power_on_at_ff() {
    // ares `cpu.hpp:217-251` and Mesen2's constructor both power the
    // channel registers up at $FF; `$420B` / `$420C` come up clear
    // (anomie-regs). luna powered the channels up at zero.
    let mut snes = Snes::from_cartridge(demo_lorom());
    for off in 0x0u8..=0x7 {
        assert_eq!(
            snes.dma.channels[0].read(off),
            0xFF,
            "$430{off:X} at power-on"
        );
    }
    assert_eq!(snes.dma.hdmaen, 0);
    // A reset rebuilds them, as ares does.
    snes.dma.channels[0].write(0x0, 0x00);
    snes.reset();
    assert_eq!(snes.dma.channels[0].read(0x0), 0xFF, "reset restores $FF");
}

#[test]
fn reset_keeps_port_devices_and_clears_memsel_and_hdmaen() {
    use crate::controller::PortDevice;
    let mut snes = Snes::from_cartridge(demo_lorom());
    snes.cpu_regs.port1 = PortDevice::Mouse;
    snes.cpu_regs.port2 = PortDevice::SuperScope;
    snes.cpu_regs.nmitimen = 0x81;
    snes.fast_rom = true;
    snes.dma.hdmaen = 0xFF;
    snes.reset();
    assert_eq!(snes.cpu_regs.port1, PortDevice::Mouse);
    assert_eq!(snes.cpu_regs.port2, PortDevice::SuperScope);
    assert_eq!(snes.cpu_regs.nmitimen, 0, "register file back to power-on");
    assert!(!snes.fast_rom);
    assert_eq!(snes.dma.hdmaen, 0);

    // A multitap and its host pads 3-5 are host configuration too.
    snes.cpu_regs.port2 = PortDevice::Multitap;
    snes.cpu_regs.set_joypad(4, 0x1000);
    snes.reset();
    assert_eq!(snes.cpu_regs.port2, PortDevice::Multitap);
    assert_eq!(snes.cpu_regs.joypad_tap[2], 0x1000);
}

#[test]
fn step_lda_imm_then_sta_long() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // LDA #$42
    snes.step();
    assert_eq!(snes.cpu.a8(), 0x42);
    // STA $7E:0000 — write goes through WRAM
    snes.step();
    assert_eq!(snes.wram[0], 0x42);
    // STP — CPU halts
    snes.step();
    assert!(snes.cpu.stopped);
}

/// ROM that does `LDA #$80; STA $4200; STP` — raise NMITIMEN.7 then halt.
fn nmitimen_enable_rom() -> Cartridge {
    let mut rom = demo_lorom().rom;
    rom[0x0000] = 0xA9; // LDA #$80
    rom[0x0001] = 0x80;
    rom[0x0002] = 0x8D; // STA $4200 (absolute, DB=0 → $00:4200)
    rom[0x0003] = 0x00;
    rom[0x0004] = 0x42;
    rom[0x0005] = 0xDB; // STP
    Cartridge::from_bytes(rom).unwrap()
}

#[test]
fn late_nmi_enable_fires_when_line_asserted() {
    // P2: raising NMITIMEN.7 (0→1) while the NMI line is asserted (the
    // $4210 flag is set, i.e. mid-VBlank, un-read) fires the NMI now.
    let mut snes = Snes::from_cartridge(nmitimen_enable_rom());
    snes.reset();
    snes.cpu_regs.nmi_flag = true; // NMI line asserted (in VBlank)
    snes.cpu_regs.nmitimen = 0x00; // NMI not yet enabled
    snes.nmi_pending = false;
    snes.step(); // LDA #$80
    snes.step(); // STA $4200 — the 0→1 raise
    assert!(snes.cpu.pending_nmi, "late NMITIMEN.7 enable fires the NMI");
}

#[test]
fn late_nmi_enable_does_not_fire_when_line_clear() {
    // The P1 guarantee: outside VBlank the line is clear (not stale-true),
    // so the same write must NOT fire a spurious NMI (the SMRPG tripwire).
    let mut snes = Snes::from_cartridge(nmitimen_enable_rom());
    snes.reset();
    snes.cpu_regs.nmi_flag = false; // line NOT asserted (outside VBlank)
    snes.cpu_regs.nmitimen = 0x00;
    snes.nmi_pending = false;
    snes.step(); // LDA #$80
    snes.step(); // STA $4200
    assert!(
        !snes.cpu.pending_nmi,
        "no spurious NMI when the line is clear"
    );
}

#[test]
fn wram_low_mirror_aliases_bank_7e() {
    // Direct write via the bus to bank 0, offset 0x100 should land
    // in WRAM[0x100] and be visible from bank 0x7E offset 0x100.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    let ppu_line_snapshot = snes.ppu_line;
    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: ppu_line_snapshot,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: false,
        cpu_pc_full: cpu_pc_snapshot,
    });
    bus.write(make_addr(0x00, 0x0100), 0xAA);
    // Read back from the mirror in $00:
    assert_eq!(bus.read(make_addr(0x00, 0x0100)), 0xAA);
    // And from $7E (full WRAM):
    assert_eq!(bus.read(make_addr(0x7E, 0x0100)), 0xAA);
}

#[test]
fn wrio_latch_fires_on_falling_edge_and_slhv_gates_on_pio_high() {
    // ares cpu/io.cpp:143: the H/V counter latch fires when WRIO
    // bit 7 FALLS (1→0); the pins power up HIGH ($FF). A rising
    // edge must NOT latch, and SLHV ($2137) only latches while the
    // line is high.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    let ppu_line_snapshot = snes.ppu_line;
    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    assert_eq!(snes.cpu_regs.wrio, 0xFF, "WRIO powers up high");
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: ppu_line_snapshot,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: false,
        cpu_pc_full: cpu_pc_snapshot,
    });
    // Falling edge (FF → 00): latch fires, PIO mirror goes low.
    bus.write(make_addr(0x00, 0x4201), 0x00);
    assert!(bus.ppu.external_latch_hit, "1→0 must latch");
    assert!(!bus.ppu.pio_bit7);
    // While low, STAT78 bit 6 reads 1 and does not clear the flag.
    assert_eq!(bus.read(make_addr(0x00, 0x213F)) & 0x40, 0x40);
    assert!(bus.ppu.external_latch_hit);
    // While low, SLHV must NOT latch.
    bus.ppu.external_latch_hit = false;
    let _ = bus.read(make_addr(0x00, 0x2137));
    assert!(!bus.ppu.external_latch_hit, "SLHV gated while PIO low");
    // Rising edge (00 → 80): NO latch, mirror goes high again.
    bus.write(make_addr(0x00, 0x4201), 0x80);
    assert!(!bus.ppu.external_latch_hit, "0→1 must not latch");
    assert!(bus.ppu.pio_bit7);
    // SLHV latches again with the line high.
    let _ = bus.read(make_addr(0x00, 0x2137));
    assert!(bus.ppu.external_latch_hit, "SLHV latches while PIO high");
}

#[test]
fn open_bus_read_returns_last_data_bus_value_not_ff() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    let ppu_line_snapshot = snes.ppu_line;
    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: ppu_line_snapshot,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: false,
        cpu_pc_full: cpu_pc_snapshot,
    });
    // A write drives 0x5A onto the data bus → latches the MDR.
    bus.write(make_addr(0x00, 0x0100), 0x5A);
    // $420B (MDMAEN) is write-only → an open-bus read returns the MDR
    // (last bus byte), NOT a fixed 0xFF.
    assert_eq!(bus.read(make_addr(0x00, 0x420B)), 0x5A);
    // A mapped read updates the MDR too: stage 0x33 in WRAM, read it,
    // then confirm the open-bus read now follows.
    bus.write(make_addr(0x00, 0x0200), 0x33);
    assert_eq!(bus.read(make_addr(0x00, 0x0200)), 0x33);
    assert_eq!(bus.read(make_addr(0x00, 0x420B)), 0x33);
    // An entirely unmapped address open-buses to the same MDR.
    assert_eq!(bus.read(make_addr(0x00, 0x420C)), 0x33);
}

#[test]
fn wram_port_round_trips_through_2180_and_address_registers() {
    // Set WMADD to $1F00 (a low-RAM mirror), write 0xAB via $2180,
    // re-set the address, read back via $2180.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    let ppu_line_snapshot = snes.ppu_line;
    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: ppu_line_snapshot,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: false,
        cpu_pc_full: cpu_pc_snapshot,
    });
    // WMADD = $00:1F00.
    bus.write(make_addr(0x00, 0x2181), 0x00);
    bus.write(make_addr(0x00, 0x2182), 0x1F);
    bus.write(make_addr(0x00, 0x2183), 0x00);
    // Write 0xAB via WMDATA (auto-increments).
    bus.write(make_addr(0x00, 0x2180), 0xAB);
    // Reset address back to $1F00.
    bus.write(make_addr(0x00, 0x2181), 0x00);
    bus.write(make_addr(0x00, 0x2182), 0x1F);
    // Read it back.
    assert_eq!(bus.read(make_addr(0x00, 0x2180)), 0xAB);
}

#[test]
fn nocash_21fc_writes_are_captured_and_filtered() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.enable_nocash_log();
    let ppu_line_snapshot = snes.ppu_line;
    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    {
        let (_, mut bus) = snes.cpu_and_bus(BusCursor {
            ppu_line: ppu_line_snapshot,
            mcycles_in_line: 0,
            frame_count: 0,
            nmis_serviced: 0,
            sched_enabled: false,
            cpu_pc_full: cpu_pc_snapshot,
        });
        bus.write(make_addr(0x00, 0x21FC), b'H');
        bus.write(make_addr(0x00, 0x21FC), b'i');
        // A non-$21FC write is NOT captured.
        bus.write(make_addr(0x00, 0x2100), 0x0F);
        // The `$80-BF` mirror of $21FC IS captured.
        bus.write(make_addr(0x80, 0x21FC), b'!');
    }
    assert_eq!(snes.take_nocash_log(), b"Hi!", "only $21FC bytes, in order");
    // Drained: a second take is empty.
    assert!(snes.take_nocash_log().is_empty());
}

#[test]
fn manual_joypad_serial_shifts_msb_first() {
    // Set joypad1 = $8001 (just bit 15 + bit 0 lit), then drive
    // the $4016 strobe (1 → 0) and shift MSB-first via reads.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.cpu_regs.set_joypad(0, 0x8001);
    let ppu_line_snapshot = snes.ppu_line;
    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: ppu_line_snapshot,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: false,
        cpu_pc_full: cpu_pc_snapshot,
    });
    // Latch then de-strobe.
    bus.write(make_addr(0x00, 0x4016), 0x01);
    bus.write(make_addr(0x00, 0x4016), 0x00);
    // First read = bit 15 of joypad1 = 1.
    assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
    // Next 14 reads = bits 14..1 = 0.
    for _ in 0..14 {
        assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 0);
    }
    // 16th read = bit 0 = 1.
    assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
    // Shift exhausted — subsequent reads return 1 (pulled-high).
    assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
}

#[test]
fn dma_uploads_palette_via_mdmaen_trigger() {
    // End-to-end integration: CPU writes the DMA channel 0 setup
    // bytes, then writes $01 to $420B → the DMA controller pulls
    // 4 bytes from WRAM and pumps them through PPU $2122 (CGDATA).
    //
    // Program at $8000 (long-hand because we don't yet have
    // store-immediate; we LDA / STA each byte):
    //   LDA #$22 ; STA $4301  ; channel 0 BBAD = $22 → $2122
    //   LDA #$00 ; STA $4302  ; A1TL = $00
    //   LDA #$20 ; STA $4303  ; A1TH = $20 → A-bus addr $002000
    //   LDA #$7E ; STA $4304  ; A1B  = $7E
    //   LDA #$04 ; STA $4305  ; DAS low  = $04
    //   LDA #$00 ; STA $4306  ; DAS high = $00 → $0004 bytes
    //   LDA #$00 ; STA $4300  ; DMAP = mode 0, +1, A→B
    //   LDA #$01 ; STA $420B  ; MDMAEN bit 0
    //   STP
    //
    // Both DAS bytes are written, as a real game must: the channel
    // registers power up at $FF (issue #224), so leaving $4306 alone
    // would ask for $FF04 bytes.
    let cart = demo_lorom();
    let mut rom = cart.rom;
    let prog = [
        0xA9, 0x22, 0x8D, 0x01, 0x43, // LDA #$22 ; STA $4301
        0xA9, 0x00, 0x8D, 0x02, 0x43, // LDA #$00 ; STA $4302
        0xA9, 0x20, 0x8D, 0x03, 0x43, // LDA #$20 ; STA $4303
        0xA9, 0x7E, 0x8D, 0x04, 0x43, // LDA #$7E ; STA $4304
        0xA9, 0x04, 0x8D, 0x05, 0x43, // LDA #$04 ; STA $4305 (DAS low)
        0xA9, 0x00, 0x8D, 0x06, 0x43, // LDA #$00 ; STA $4306 (DAS high)
        0xA9, 0x00, 0x8D, 0x00, 0x43, // LDA #$00 ; STA $4300
        0xA9, 0x01, 0x8D, 0x0B, 0x42, // LDA #$01 ; STA $420B (trigger)
        0xDB, // STP
    ];
    rom[..prog.len()].copy_from_slice(&prog);
    let cart = Cartridge::from_bytes(rom).unwrap();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.cpu.db = 0;
    // Seed the palette bytes in WRAM at $7E:2000.
    // (CGRAM expects a low/high pair per color → 2 colors here.)
    snes.wram[0x2000] = 0x1F; // red.low
    snes.wram[0x2001] = 0x00; // red.high  → BGR555 = 0x001F (pure red)
    snes.wram[0x2002] = 0xE0; // green.low
    snes.wram[0x2003] = 0x03; // green.high → 0x03E0 (pure green)
    // Make sure the CGRAM word address starts at 0.
    snes.ppu.cgram.set_address(0);

    // Run until the STP halts the CPU. The longest path is 8
    // groups of LDA+STA = 16 instructions, plus the STP.
    for _ in 0..32 {
        if snes.cpu.stopped {
            break;
        }
        snes.step();
    }
    assert!(snes.cpu.stopped, "program should reach STP");

    // After the DMA, CGRAM colors 0 and 1 should be red then green.
    assert_eq!(snes.ppu.cgram.color(0), 0x001F, "color 0 = red");
    assert_eq!(snes.ppu.cgram.color(1), 0x03E0, "color 1 = green");
    // DAS is zeroed by hardware on completion.
    assert_eq!(snes.dma.channels[0].das, 0);
}

#[test]
fn mem_trace_tags_dma_writes_with_their_origin_and_fires_watchpoints() {
    // The palette-upload program above, plus one CPU write to $2122
    // first, with a writes-only trace on $2122 — the "who wrote CGRAM
    // entry N" hunt (issue #226): the CPU row and the four DMA rows
    // share one stream, each tagged by origin.
    let cart = demo_lorom();
    let mut rom = cart.rom;
    let prog = [
        0xA9, 0x55, 0x8D, 0x22, 0x21, // LDA #$55 ; STA $2122 (CPU write)
        0xA9, 0x22, 0x8D, 0x01, 0x43, // LDA #$22 ; STA $4301
        0xA9, 0x00, 0x8D, 0x02, 0x43, // LDA #$00 ; STA $4302
        0xA9, 0x20, 0x8D, 0x03, 0x43, // LDA #$20 ; STA $4303
        0xA9, 0x7E, 0x8D, 0x04, 0x43, // LDA #$7E ; STA $4304
        0xA9, 0x04, 0x8D, 0x05, 0x43, // LDA #$04 ; STA $4305 (DAS low)
        0xA9, 0x00, 0x8D, 0x06, 0x43, // LDA #$00 ; STA $4306 (DAS high)
        0xA9, 0x00, 0x8D, 0x00, 0x43, // LDA #$00 ; STA $4300
        0xA9, 0x01, 0x8D, 0x0B, 0x42, // LDA #$01 ; STA $420B (trigger)
        0xDB, // STP
    ];
    rom[..prog.len()].copy_from_slice(&prog);
    let cart = Cartridge::from_bytes(rom).unwrap();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.cpu.db = 0;
    snes.wram[0x2000..0x2004].copy_from_slice(&[0x1F, 0x00, 0xE0, 0x03]);
    snes.enable_mem_trace_filtered(
        1000,
        MemTraceFilter {
            only_offsets: Some(vec![0x2122]),
            writes_only: true,
            ..MemTraceFilter::default()
        },
    );
    // A watchpoint on $00:2122 must fire on the DMA's write too.
    let mut bps = crate::breakpoints::BreakpointSet::new();
    bps.add_mem(0x00_2122, 0x00_2122, false, true, false, None);
    snes.breakpoints = Some(Box::new(bps));
    let mut hits = 0;
    for _ in 0..40 {
        if snes.cpu.stopped {
            break;
        }
        snes.step();
        if let Some(bp) = snes.breakpoints.as_mut()
            && bp.take_pending().is_some()
        {
            hits += 1;
        }
    }
    assert!(snes.cpu.stopped);
    assert_eq!(
        hits, 2,
        "one CPU hit, one DMA hit (first byte of the burst)"
    );

    let ev = snes.take_mem_trace_log();
    assert_eq!(ev.len(), 5, "{ev:?}");
    assert!(
        ev.iter()
            .all(|e| e.addr_full == 0x00_2122 && e.kind == MemEventKind::Write)
    );
    assert_eq!(ev[0].origin, MemOrigin::Cpu);
    assert_eq!(ev[0].value, 0x55);
    let dma: Vec<u8> = ev[1..].iter().map(|e| e.value).collect();
    assert_eq!(dma, vec![0x1F, 0x00, 0xE0, 0x03]);
    assert!(
        ev[1..].iter().all(|e| e.origin == MemOrigin::Dma(0)),
        "{ev:?}"
    );
    // The DMA rows carry the PC of the instruction whose access ran the
    // edge (the STP after `STA $420B`), and the burst-start clock.
    assert_eq!(ev[1].pc_full, 0x00_8000 + prog.len() as u32 - 1);
    assert!(ev[1].mclk_total >= ev[0].mclk_total);
}

#[test]
fn profile_credits_each_pc_with_its_real_cost() {
    // LDA #$42 ; STA $7E:0000 ; STP — three PCs, then parked STP ticks
    // (which cost nothing, are not instructions, and leave no sample).
    let mut snes = Snes::from_cartridge(demo_lorom());
    snes.reset();
    snes.enable_profile();
    for _ in 0..8 {
        snes.step();
    }
    let p = snes.take_profile();
    assert_eq!(p.samples.len(), 3, "{p:?}");
    let lda = p.samples[&0x00_8000];
    let sta = p.samples[&0x00_8002];
    let stp = p.samples[&0x00_8006];
    assert_eq!(lda.instructions, 1);
    assert_eq!(sta.instructions, 1);
    assert_eq!(stp.instructions, 1, "the STP itself executes once");
    // LDA #imm = 2 slow accesses (16 mclk); STA long = 4 accesses + the
    // WRAM write (40 mclk); every cost is real master clocks, so the
    // long store costs more than the immediate load.
    assert!(sta.mclk > lda.mclk, "{p:?}");
    assert_eq!(p.total_mclk(), lda.mclk + sta.mclk + stp.mclk);
    assert_eq!(lda.idle_mclk, 0);
    // Taking empties; the profiler stays on.
    assert!(snes.take_profile().samples.is_empty());
    assert!(snes.profile.is_some());
    snes.disable_profile();
    assert!(snes.profile.is_none());
}

#[test]
fn profile_splits_the_cost_by_ppu_frame() {
    // The per-frame buckets (`OpenSNES` R-B): a frame edge closes the
    // bucket in progress and opens the next; a step is credited to
    // the frame it ends in; taking the profile restarts the bucket at
    // the live frame rather than at 0.
    let mut p = Profile::starting_at(7);
    p.record(0x8000, 10, false, 7);
    p.record(0x8002, 20, false, 7);
    p.record(0x8000, 5, false, 8);
    p.record(0x8004, 0, true, 8); // a free parked tick: no cost
    p.record(0x8006, 1, false, 10); // a step spanning frame 9 entirely
    assert_eq!(p.completed.len(), 3, "{p:?}");
    assert_eq!(p.completed[0].0, 7);
    assert_eq!(p.completed[0].1[&0x8000], 10);
    assert_eq!(p.completed[0].1[&0x8002], 20);
    assert_eq!(p.completed[1].0, 8);
    assert_eq!(p.completed[1].1.len(), 1);
    assert_eq!(p.completed[2].0, 9, "the skipped frame still has a bucket");
    assert!(p.completed[2].1.is_empty());
    assert_eq!(p.frame, 10);
    assert_eq!(p.current[&0x8006], 1);
    assert_eq!(
        p.samples[&0x8000].mclk, 15,
        "the totals still see every frame"
    );

    // Wired: the live machine tags each step with its frame, and a
    // take restarts the bucket at that frame. (The idle ROM: `SEI ;
    // LDA #$80 ; STA $4200 ; loop: WAI ; BRA loop ; nmi: RTI` — the
    // demo ROM's `STP` would stop time.)
    let mut snes = Snes::from_cartridge(idle_lorom());
    snes.reset();
    snes.enable_profile();
    let mut guard = 0u32;
    while snes.frame_count < 2 {
        snes.step();
        guard += 1;
        assert!(guard < 2_000_000, "frames never advanced");
    }
    let frames = snes.take_profile_frames();
    assert_eq!(
        frames.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
        vec![0, 1],
        "two completed frames, the third in progress"
    );
    assert!(snes.take_profile_frames().is_empty(), "drained");
    let taken = snes.take_profile();
    assert_eq!(taken.frame, 2);
    assert_eq!(snes.profile.as_ref().unwrap().frame, 2);
}

#[test]
fn mem_origin_from_channel_tag_and_labels() {
    assert_eq!(MemOrigin::from_channel_tag(3), MemOrigin::Dma(3));
    assert_eq!(
        MemOrigin::from_channel_tag(HDMA_CHANNEL_FLAG | 5),
        MemOrigin::Hdma(5)
    );
    assert_eq!(MemOrigin::Cpu.label(), "cpu");
    assert_eq!(MemOrigin::Dma(0).label(), "dma0");
    assert_eq!(MemOrigin::Hdma(7).label(), "hdma7");
}

#[test]
fn hdma_preempts_a_long_mid_frame_dma_at_scanline_boundaries() {
    // Phase 5 increment 1: a long sync DMA triggered mid-visible-frame
    // with HDMA armed must *yield* to HDMA at scanline boundaries, not
    // land all at once. Discriminator: the DMA streams 0xCD to
    // successive VRAM words (shared VMADD); the armed HDMA writes 0xAB
    // to the same $2118 at the VMADD reached mid-burst — so an
    // interleaved HDMA byte lands in the *middle* of the DMA's output
    // range, which is impossible in any non-interleaved (lump) model.
    use crate::dma::DmaParams;

    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();

    // Forced blank so $2118 VRAM writes land (and stay un-gated across
    // line crossings), VMADD = 0, mid-visible scanline.
    snes.ppu.inidisp = 0x80;
    snes.ppu.active_display = false;
    snes.ppu.vram.set_address(0, 0);
    snes.ppu_line = 50;
    // DMA ch0: mode 0 (→ $2118), FIXED source so every byte reads the
    // same 0xCD; 600 bytes (~3.5 scanlines of 8-mclk transfers).
    snes.wram[0x4000] = 0xCD;
    snes.dma.channels[0].params = DmaParams::from_byte(0x08); // AToB, Fixed, mode 0
    snes.dma.channels[0].bbad = 0x18;
    snes.dma.channels[0].a_addr = 0x4000;
    snes.dma.channels[0].a_bank = 0x7E;
    snes.dma.channels[0].das = 600;
    // HDMA ch1: direct mode 0 (→ $2118), pre-armed in its active state,
    // repeat header (0x86 = repeat + 6 lines) so it fires every crossed
    // line, reading 0xAB from its table.
    for i in 0..8 {
        snes.wram[0x5000 + i] = 0xAB;
    }
    snes.dma.channels[1].params = DmaParams::from_byte(0x00); // AToB, +1, mode 0
    snes.dma.channels[1].bbad = 0x18;
    snes.dma.channels[1].a_bank = 0x7E;
    snes.dma.channels[1].a2a = 0x5000;
    snes.dma.channels[1].ntlr = 0x86;
    snes.dma.channels[1].hdma_active = true;
    snes.dma.channels[1].hdma_do_transfer = true;
    snes.dma.hdmaen = 0x02;

    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: 50,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: true,
        cpu_pc_full: cpu_pc_snapshot,
    });
    // Trigger channel-0 sync DMA via the bus (→ the segmented path,
    // since HDMAEN != 0).
    bus.write(make_addr(0x00, 0x420B), 0x01);
    // `$420B` only arms the burst (ares `dmaPending`); it executes at the
    // next bus access's `dmaEdge()`. Fire that edge.
    let _ = bus.read(make_addr(0x00, 0x8000));
    // (the `&mut snes` borrow held by `bus` ends at its last use above)

    // VRAM low byte of word w == $2118 write #w.
    let lows: Vec<u8> = (0..600u16).map(|w| snes.ppu.vram.peek(w * 2)).collect();
    let first_ab = lows.iter().position(|&b| b == 0xAB);
    assert!(first_ab.is_some(), "HDMA must have written 0xAB into VRAM");
    let idx = first_ab.unwrap();
    assert!(
        idx < 500,
        "HDMA byte landed mid-DMA-range (interleaved), not at the tail: idx={idx}"
    );
    assert!(
        lows[idx + 1..].contains(&0xCD),
        "DMA must resume (0xCD) after the HDMA preemption"
    );
    let ab_count = lows.iter().fold(0usize, |n, &b| n + usize::from(b == 0xAB));
    assert!(
        ab_count >= 2,
        "HDMA should fire on each crossed scanline (got {ab_count})"
    );
}

#[test]
fn cpu_writes_to_ppu_register_reach_the_ppu() {
    // Build a program that writes $42 to PPU $2100 (INIDISP).
    // Reuse demo_lorom() so the SRAM exponent / checksum etc. are
    // all set correctly — then patch in the program bytes.
    let cart = demo_lorom();
    let mut rom = cart.rom;
    // Program at $8000 (file offset 0): LDA #$42, STA $2100
    rom[0] = 0xA9;
    rom[1] = 0x42;
    rom[2] = 0x8D;
    rom[3] = 0x00;
    rom[4] = 0x21;
    // Re-checksum so the header parser still accepts the ROM (we
    // overwrote the demo_lorom's STA-target program at offset 0).
    // For now, demo_lorom's checksum bytes at $7FDC-$7FDF are
    // already valid for the original ROM. Since we're patching
    // only 5 bytes, just keep the same checksum (parser only checks
    // complement vs checksum XOR, not against ROM contents).
    let cart = Cartridge::from_bytes(rom).unwrap();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.cpu.db = 0; // ensure data bank is 0 for abs addressing
    snes.step(); // LDA #$42
    snes.step(); // STA $2100
    assert_eq!(snes.ppu.inidisp, 0x42, "PPU INIDISP must reflect the write");
}

#[test]
fn step_accumulates_master_cycles() {
    // NOP costs 6 master cycles (FastROM not set on a default LoROM,
    // so it's actually SLOW = 8). LDA #$42 reads opcode + operand =
    // 2 × 8 = 16. After running 1 NOP we should have ≥ 8 mclk total.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset(); // already pays 2 reads for the reset vector.
    let before = snes.total_mclk;
    snes.step(); // LDA #$42
    let after = snes.total_mclk;
    assert!(after > before, "step should advance master clock");
}

#[test]
fn scheduler_advances_to_next_scanline_after_one_line_of_mcycles() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // Reset leaves the CPU `RESET_SEQUENCE_MCLK` into scanline 0 (ares'
    // `//H=186`), so a full line of cycles lands at the same offset in the
    // next line — not at H=0.
    let line_before = snes.ppu_line;
    assert_eq!(snes.mcycles_in_line, RESET_SEQUENCE_MCLK);
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(snes.ppu_line, line_before + 1);
    assert_eq!(snes.mcycles_in_line, RESET_SEQUENCE_MCLK);
}

#[test]
fn scheduler_fires_nmi_at_vblank_start_when_nmitimen_set() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.cpu_regs.nmitimen = 0x80; // NMI on VBlank enabled
    snes.ppu_line = VBLANK_START_LINE - 1;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(snes.ppu_line, VBLANK_START_LINE);
    assert!(snes.cpu_regs.nmi_flag);
    assert_eq!(snes.cpu_regs.hvbjoy & 0x80, 0x80);
    assert_eq!(snes.nmis_serviced, 1);
}

#[test]
fn scheduler_does_not_trigger_nmi_when_masked_but_still_sets_flag() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.cpu_regs.nmitimen = 0x00; // NMI masked
    snes.ppu_line = VBLANK_START_LINE - 1;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(snes.ppu_line, VBLANK_START_LINE);
    assert!(snes.cpu_regs.nmi_flag);
    assert_eq!(snes.nmis_serviced, 0);
}

/// Read `$4210` through the CPU bus with the line cursor parked at `hclock`
/// of the `VBlank` scanline and the NMI flag already raised. The scheduler
/// is off, so the cursor does not move and the read lands exactly there.
/// Returns the byte the CPU sees and whether the flag survived the read.
fn rdnmi_read_at(hclock: u32) -> (u8, bool) {
    let mut snes = Snes::from_cartridge(demo_lorom());
    snes.reset();
    snes.cpu_regs.nmi_flag = true;
    let vblank = vblank_start_line(snes.ppu.setini & 0x04 != 0);
    snes.ppu_line = vblank;

    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: vblank,
        mcycles_in_line: hclock,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: false,
        cpu_pc_full: cpu_pc_snapshot,
    });
    let v = bus.read(make_addr(0x00, 0x4210));
    let still_set = bus.cpu_regs.nmi_flag;
    (v, still_set)
}

/// Before the raise (H < 2) a `$4210` read sees the flag clear and — being
/// inside the hold — cannot clear it.
#[test]
fn rdnmi_reads_clear_before_the_raise() {
    let (v, still_set) = rdnmi_read_at(0);
    assert_eq!(v & 0x80, 0x00, "H=0: line not raised yet");
    assert!(still_set, "H=0: read must not clear the flag");
}

/// Inside the hold (`H` in `[2, 6)`) the read returns the flag **set**
/// but leaves it set — ares `nmiHold`, the Terranigma / Chrono Trigger
/// protection. This is the faithful rule, live since the phase locked
/// (#109); #107's masking (clear below H=6) is retired.
#[test]
fn rdnmi_reads_set_but_does_not_clear_inside_the_hold() {
    for h in [2, 4] {
        let (v, still_set) = rdnmi_read_at(h);
        assert_eq!(v & 0x80, 0x80, "H={h}: flag must read set");
        assert!(still_set, "H={h}: hold must survive the read");
    }
}

/// From H=6 the line is presented: the read sees it and clears it, so the
/// next poll blocks until the next frame — one pass per `VBlank`.
#[test]
fn rdnmi_is_visible_and_clears_from_hclock_6() {
    for h in [6, 8, 40, 600] {
        let (v, still_set) = rdnmi_read_at(h);
        assert_eq!(v & 0x80, 0x80, "H={h}: flag must read set");
        assert!(!still_set, "H={h}: read must clear the flag");
    }
}

// ---- Phase 4: dot-precise H/V-counter IRQ (poll_hv_irq) ----
// Reference: ares cpu/irq.cpp:18-31, Mesen2 InternalRegisters::UpdateIrqLevel.
// NMITIMEN bit 4 = H-IRQ enable, bit 5 = V-IRQ enable. A dot = 4 mclk.

fn irq_snes() -> Snes {
    let mut snes = Snes::from_cartridge(demo_lorom());
    snes.reset();
    snes.ppu_line = 0;
    snes.mcycles_in_line = 0;
    snes.cpu_regs.irq_flag = false;
    snes.irq_pending = false;
    snes
}

#[test]
fn hv_irq_mode00_never_fires() {
    let mut snes = irq_snes();
    snes.cpu_regs.nmitimen = 0x00; // neither H nor V IRQ
    snes.cpu_regs.htime = 100;
    snes.cpu_regs.vtime = 50;
    snes.ppu_line = 50;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert!(!snes.cpu_regs.irq_flag, "mode 00 must never raise IRQ");
}

#[test]
fn hv_irq_h_only_fires_at_htime_dot_every_line() {
    // Mode 01: fire once per scanline at h == htime, regardless of line.
    // (Old code fired every scanline boundary — wrong dot.)
    let mut snes = irq_snes();
    snes.cpu_regs.nmitimen = 0x10; // H-IRQ only
    snes.cpu_regs.htime = 100; // dot 100 → mclk 400, assert at 410
    snes.ppu_line = 10;
    // ares samples the counters 10 clocks in the past (vcounter(10)/
    // hcounter(10), irq.cpp:26-28), so the assert point is htime*4+10.
    snes.advance_scheduler(409); // up to mclk 409 — not asserted yet
    assert!(!snes.cpu_regs.irq_flag, "not yet at htime dot");
    snes.advance_scheduler(2); // crosses mclk 410
    assert!(snes.cpu_regs.irq_flag, "fires at htime dot 100");
    // Next scanline fires again (H-IRQ is per-line).
    snes.cpu_regs.irq_flag = false;
    snes.ppu_line = 11;
    snes.mcycles_in_line = 0;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert!(snes.cpu_regs.irq_flag, "fires on the next line too");
}

#[test]
fn hv_irq_v_only_fires_at_matching_line_start() {
    // Mode 10: fire once, at the start of line == vtime; H irrelevant.
    let mut snes = irq_snes();
    snes.cpu_regs.nmitimen = 0x20; // V-IRQ only
    snes.cpu_regs.vtime = 50;
    // On the wrong line: no fire across the whole line.
    snes.ppu_line = 49;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert!(!snes.cpu_regs.irq_flag, "no fire on line 49");
    // Crossing into line 50 fires at its start.
    assert_eq!(snes.ppu_line, 50);
    // The 10-clock counter-sampling delay puts the assert point 10
    // clocks into the matching line.
    snes.advance_scheduler(12); // first dots of line 50, past +10
    assert!(snes.cpu_regs.irq_flag, "V-IRQ fires at line 50 start");
}

#[test]
fn hv_irq_hv_mode_fires_at_h_and_v_with_nonzero_htime() {
    // Mode 11 with htime != 0 — the exact case the old code got wrong
    // (it only fired when htime == 0). Must fire at (h=htime, v=vtime).
    let mut snes = irq_snes();
    snes.cpu_regs.nmitimen = 0x30; // H+V IRQ
    snes.cpu_regs.htime = 80; // dot 80 → mclk 320, assert at 330
    snes.cpu_regs.vtime = 60;
    // Wrong line: V gate blocks it entirely.
    snes.ppu_line = 59;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert!(!snes.cpu_regs.irq_flag, "no fire on line 59");
    // Right line, before the htime dot: still no fire.
    assert_eq!(snes.ppu_line, 60);
    snes.advance_scheduler(329);
    assert!(!snes.cpu_regs.irq_flag, "not yet at htime on line 60");
    // Cross the htime assert point (320 + the 10-clock delay) → fire.
    snes.advance_scheduler(2);
    assert!(
        snes.cpu_regs.irq_flag,
        "H+V IRQ fires at htime!=0 on the vtime line"
    );
}

#[test]
fn hv_irq_hv_mode_does_not_fire_off_the_htime_dot() {
    // Mode 11: advancing a full vtime line but with htime beyond the
    // line's dot range must NOT fire (htime never crossed).
    let mut snes = irq_snes();
    snes.cpu_regs.nmitimen = 0x30;
    snes.cpu_regs.htime = 350; // dot 350 → mclk 1400 > line length (1364)
    snes.cpu_regs.vtime = 70;
    snes.ppu_line = 70;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert!(
        !snes.cpu_regs.irq_flag,
        "htime past the line never matches → no IRQ"
    );
    // …and the next line must not fire either (this is NOT the
    // 10-clock-delay wrap — the dot itself is unreachable).
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert!(!snes.cpu_regs.irq_flag, "no phantom wrap fire");
}

#[test]
fn hv_irq_assert_delay_wraps_into_the_next_line() {
    // htime near the line end: the dot matches on this line, but the
    // 10-clock detect→assert delay pushes the assert point past the
    // line boundary — ares' vcounter(10)/hcounter(10) still read the
    // matching line there, so the IRQ fires in the FIRST clocks of
    // the next line.
    let mut snes = irq_snes();
    snes.cpu_regs.nmitimen = 0x30; // H+V
    snes.cpu_regs.htime = 339; // dot 339 → mclk 1356; assert at 1366 ≥ 1364
    snes.cpu_regs.vtime = 60;
    snes.ppu_line = 60;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert!(
        !snes.cpu_regs.irq_flag,
        "assert point is past this line's end"
    );
    assert_eq!(snes.ppu_line, 61);
    snes.advance_scheduler(4); // crosses the wrapped point (1366-1364=2)
    assert!(
        snes.cpu_regs.irq_flag,
        "wrapped assert fires on the next line's first clocks"
    );
}

#[test]
fn hv_irq_no_trigger_across_the_field_boundary() {
    // ares irq.cpp:29: `(vcounter(6) || hcounter(6))` — IRQs cannot
    // trigger on the last dot of a field. A wrap from the field's
    // LAST line into line 0 is exactly that case and must be
    // suppressed.
    let mut snes = irq_snes();
    let last_line = snes.region_scanlines() - 1;
    snes.cpu_regs.nmitimen = 0x30;
    snes.cpu_regs.htime = 339;
    snes.cpu_regs.vtime = last_line;
    snes.ppu_line = last_line;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    snes.advance_scheduler(16);
    assert!(
        !snes.cpu_regs.irq_flag,
        "no IRQ may trigger across the field boundary"
    );
}

#[test]
fn timeup_hold_window_survives_a_simultaneous_read() {
    // The raise clock is stamped by poll_hv_irq; a $4211 read whose
    // bus sample lands within 4 master clocks of it must see the
    // flag WITHOUT acknowledging it (ares timeup() under irqHold).
    let mut snes = irq_snes();
    snes.cpu_regs.nmitimen = 0x10;
    snes.cpu_regs.htime = 100; // assert at line-relative 410
    snes.ppu_line = 10;
    snes.advance_scheduler(412);
    assert!(snes.cpu_regs.irq_flag);
    // The raise is stamped at the line-relative assert point (410 =
    // htime*4 + the 10-clock delay), whatever the absolute base.
    assert_eq!(
        snes.cpu_regs.irq_raise_mclk % u64::from(MCYCLES_PER_SCANLINE),
        410
    );
    // Reads go through the bus (`read_inner`: in_hold = mclk_total <
    // raise + 4); exercise the window arithmetic on both sides.
    let raise = snes.cpu_regs.irq_raise_mclk;
    let in_hold = (raise + 2) < raise + 4;
    assert!(in_hold, "sample 2 clocks after the raise is held");
    assert_eq!(snes.cpu_regs.read_timeup(in_hold) & 0x80, 0x80);
    assert!(snes.cpu_regs.irq_flag, "held read must not acknowledge");
    let in_hold = (raise + 6) < raise + 4;
    assert!(!in_hold);
    assert_eq!(snes.cpu_regs.read_timeup(in_hold) & 0x80, 0x80);
    assert!(!snes.cpu_regs.irq_flag, "post-hold read acknowledges");
}

#[test]
fn interlace_field_toggles_each_frame_wrap() {
    // Interlace Phase A: STAT78 bit-7 field parity flips every frame at
    // the V-counter wrap (ares counter/inline.hpp:32), unconditionally.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    let f0 = snes.ppu.field;
    snes.ppu_line = NTSC_SCANLINES_PER_FRAME - 1;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(snes.ppu_line, 0, "frame wrapped");
    assert_eq!(snes.ppu.field, !f0, "field flipped at frame wrap");
    snes.ppu_line = NTSC_SCANLINES_PER_FRAME - 1;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(snes.ppu.field, f0, "field flipped back next frame");
}

#[test]
fn vblank_entry_reloads_oam_address_from_latch_when_not_force_blanked() {
    // Mirrors ares `object.cpp:31-32` (`addressReset()` at vcounter==vdisp
    // when force-blank is off) and Mesen2 `SnesPpu.cpp:464-472`. Games
    // (SMW etc.) rely on this so every NMI's OAM stream lands at index 0.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // Force-blank OFF, brightness max.
    snes.ppu.write(luna_ppu::register::INIDISP, 0x0F);
    // Latch word address = $0010 → byte addr should be $0020.
    snes.ppu.oam.set_address_low(0x10);
    assert_eq!(snes.ppu.oam.address, 0x0020);
    // Streaming 4 bytes advances the byte address.
    snes.ppu.oam.write(0x11);
    snes.ppu.oam.write(0x22);
    snes.ppu.oam.write(0x33);
    snes.ppu.oam.write(0x44);
    assert_eq!(snes.ppu.oam.address, 0x0024);
    // Cross the vblank-entry scanline.
    snes.ppu_line = VBLANK_START_LINE - 1;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(snes.ppu_line, VBLANK_START_LINE);
    // Address has been reloaded from the latched word_address.
    assert_eq!(
        snes.ppu.oam.address, 0x0020,
        "vblank entry must reload OAM byte address from word_address << 1"
    );
}

#[test]
fn vblank_entry_does_not_reload_oam_address_when_force_blanked() {
    // Same scenario but force-blank ON — both refs skip the reload.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    snes.ppu.write(luna_ppu::register::INIDISP, 0x80); // force-blank on
    snes.ppu.oam.set_address_low(0x10);
    snes.ppu.oam.write(0x11);
    snes.ppu.oam.write(0x22);
    snes.ppu.oam.write(0x33);
    snes.ppu.oam.write(0x44);
    assert_eq!(snes.ppu.oam.address, 0x0024);
    snes.ppu_line = VBLANK_START_LINE - 1;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(
        snes.ppu.oam.address, 0x0024,
        "force-blank suppresses the OAM address auto-reset"
    );
}

#[test]
fn inidisp_write_exiting_force_blank_at_vblank_line_reloads_oam_address() {
    // ares `ppu_io.cpp:194` and Mesen2 `SnesPpu.cpp:1889-1896`:
    // a $2100 write that turns off forced-blank while sitting on
    // the vblank-entry scanline triggers the same auto-reset.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // Start with force-blank ON and the byte address advanced.
    snes.ppu.write(luna_ppu::register::INIDISP, 0x80);
    snes.ppu.oam.set_address_low(0x10);
    snes.ppu.oam.write(0x11);
    snes.ppu.oam.write(0x22);
    assert_eq!(snes.ppu.oam.address, 0x0022);
    // Park on the vblank-entry scanline.
    snes.ppu_line = VBLANK_START_LINE;
    // Drive the bus write for $2100 = $0F (force-blank OFF).
    let ppu_line_snapshot = snes.ppu_line;
    let cpu_pc_snapshot = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
    let (_, mut bus) = snes.cpu_and_bus(BusCursor {
        ppu_line: ppu_line_snapshot,
        mcycles_in_line: 0,
        frame_count: 0,
        nmis_serviced: 0,
        sched_enabled: false,
        cpu_pc_full: cpu_pc_snapshot,
    });
    bus.write(make_addr(0x00, 0x2100), 0x0F);
    assert_eq!(
        snes.ppu.oam.address, 0x0020,
        "exiting force-blank at vdisp must reload OAM address"
    );
}

#[test]
fn scheduler_wraps_to_line_zero_after_full_frame_and_clears_vblank() {
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // Pretend we're at the last scanline of the frame, with VBlank
    // currently set (as it would be).
    snes.ppu_line = NTSC_SCANLINES_PER_FRAME - 1;
    snes.cpu_regs.hvbjoy = 0x80;
    let frame_before = snes.frame_count;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE);
    assert_eq!(snes.ppu_line, 0);
    assert_eq!(snes.cpu_regs.hvbjoy & 0x80, 0);
    assert_eq!(snes.frame_count, frame_before + 1);
}

#[test]
fn scheduler_handles_multi_line_advance_in_a_single_call() {
    // A single instruction can in theory consume more than one
    // scanline's worth of mcycles (e.g. inside a DMA burst). The
    // scheduler must run all line ticks instead of dropping them.
    let cart = demo_lorom();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    let line_before = snes.ppu_line;
    snes.advance_scheduler(MCYCLES_PER_SCANLINE * 5 + 100);
    assert_eq!(snes.ppu_line, line_before + 5);
    // Offset from the post-reset cursor (see `RESET_SEQUENCE_MCLK`).
    assert_eq!(snes.mcycles_in_line, RESET_SEQUENCE_MCLK + 100);
}

/// Build a minimal SA-1 cartridge whose main-CPU boot code seeds
/// the SA-1 I-RAM with NOPs, sets the SA-1 reset vector, then
/// releases the SA-1 by toggling `$2200 CCNT` bit 7 from 1 → 0.
/// Used by [`sa1_main_cpu_releases_coproc_and_step_runs_it`].
fn demo_sa1_cart() -> Cartridge {
    let mut rom = vec![0xEA; 32 * 1024];
    // Reset vector $7FFC = $8000.
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    // Header.
    let off = 0x7FC0;
    for (i, b) in b"LUNA SA1 DEMO        ".iter().enumerate() {
        rom[off + i] = *b;
    }
    rom[off + 0x15] = 0x23; // SA-1 mapping (low nibble = 3)
    rom[off + 0x17] = 0x05;
    rom[off + 0x18] = 0x00;
    rom[off + 0x19] = 0x01;
    rom[off + 0x1C] = 0x34;
    rom[off + 0x1D] = 0x12;
    rom[off + 0x1E] = 0xCB;
    rom[off + 0x1F] = 0xED;
    // Boot code at $00:8000. SA-1 ROM at $00:8000 maps to ROM[0]
    // (CXB = 0 after reset → bank 0 region of the SA-1 mapping).
    //
    //   SEI                        78
    //   CLC, XCE                   18 FB    (native mode)
    //   REP #$30                   C2 30    (16-bit A + X/Y)
    //   LDA #$EAEA                 A9 EA EA
    //   STA $003000                8F 00 30 00
    //   STA $003002                8F 02 30 00
    //   SEP #$20                   E2 20
    //   LDA #$30                   A9 30          ; PC hi byte
    //   STA $002204                8F 04 22 00    ; CRV hi
    //   STZ $002203                9C 03 22       ; CRV lo = 0
    //   LDA #$80                   A9 80
    //   STA $002200                8F 00 22 00    ; CCNT bit 7 set (already default)
    //   STZ $002200                9C 00 22       ; release SA-1
    //   STP                        DB
    let mut p = 0;
    let mut emit = |bytes: &[u8]| {
        for b in bytes {
            rom[p] = *b;
            p += 1;
        }
    };
    emit(&[0x78]);
    emit(&[0x18, 0xFB]);
    emit(&[0xC2, 0x30]);
    emit(&[0xA9, 0xEA, 0xEA]);
    emit(&[0x8F, 0x00, 0x30, 0x00]);
    emit(&[0x8F, 0x02, 0x30, 0x00]);
    emit(&[0xE2, 0x20]);
    emit(&[0xA9, 0x30]);
    emit(&[0x8F, 0x04, 0x22, 0x00]);
    emit(&[0x9C, 0x03, 0x22]);
    emit(&[0xA9, 0x80]);
    emit(&[0x8F, 0x00, 0x22, 0x00]);
    emit(&[0x9C, 0x00, 0x22]);
    emit(&[0xDB]);
    let _ = p;
    Cartridge::from_bytes(rom).unwrap()
}

#[test]
fn sa1_main_cpu_releases_coproc_and_step_runs_it() {
    // End-to-end: the main CPU's boot path runs through `Snes::step`,
    // which (a) routes its $2200/$2203/$2204/$003000 writes through
    // the `Sa1Chip` mapper and (b) calls `step_coproc(consumed)`
    // each instruction. After the main CPU STPs, the SA-1's PC
    // should have advanced past 0x3000 — proving the chip wiring.
    let cart = demo_sa1_cart();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // The main-CPU program is < 40 instructions long; 200 steps is
    // safe headroom even at one instruction per call.
    for _ in 0..200 {
        snes.step();
        if snes.cpu.stopped {
            break;
        }
    }
    assert!(snes.cpu.stopped, "main CPU should have hit STP by now");
    // Reach into the SA-1 chip via its mapper trait. We can't
    // downcast safely, so we verify the side-effect: read the I-RAM
    // NOPs that the main CPU wrote (proves the SA-1 mapper claimed
    // the $3000/$3002 writes), and check the chip's snapshot reports
    // it released (CCNT itself is write-only from the S-CPU).
    let iram_3000 = snes.mapper.read(luna_bus::make_addr(0x00, 0x3000));
    let iram_3002 = snes.mapper.read(luna_bus::make_addr(0x00, 0x3002));
    assert_eq!(iram_3000, Some(0xEA), "NOP should be in SA-1 I-RAM");
    assert_eq!(iram_3002, Some(0xEA), "NOP should be in SA-1 I-RAM");
    let snap = snes.mapper.sa1_snapshot().expect("an SA-1 cart");
    assert!(snap.running, "the SA-1 should have been released");
}

/// Build an SA-1 cart where the main CPU:
///   1. Seeds I-RAM with a small SA-1 program: enable its own CIE.7
///      (S-CPU → SA-1 IRQ — CIE is the SA-1's register, the S-CPU
///      cannot write it), `CLI`, then a NOP loop at $3000, and an IRQ
///      handler at $3010 that writes sentinel `$AA` to I-RAM `$3500`
///      then `STP`s.
///   2. Sets CRV = $3000 and CIV = $3010.
///   3. Releases the SA-1 via CCNT 1→0 edge.
///   4. Burns through a NOP run-up so the SA-1 has time to start.
///   5. Triggers the SA-1 IRQ via CCNT.7 0→1 edge.
///   6. NOP-pauses then `STP`s.
fn demo_sa1_irq_cart() -> Cartridge {
    let mut rom = vec![0xEA; 32 * 1024];
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let off = 0x7FC0;
    for (i, b) in b"LUNA SA1 IRQ DEMO    ".iter().enumerate() {
        rom[off + i] = *b;
    }
    rom[off + 0x15] = 0x23;
    rom[off + 0x17] = 0x05;
    rom[off + 0x18] = 0x00;
    rom[off + 0x19] = 0x01;
    rom[off + 0x1C] = 0x34;
    rom[off + 0x1D] = 0x12;
    rom[off + 0x1E] = 0xCB;
    rom[off + 0x1F] = 0xED;

    let mut p = 0usize;
    let emit = |bytes: &[u8], rom: &mut [u8], p: &mut usize| {
        for b in bytes {
            rom[*p] = *b;
            *p += 1;
        }
    };
    // SEI, native mode, 16-bit A/X/Y.
    emit(&[0x78], &mut rom, &mut p);
    emit(&[0x18, 0xFB], &mut rom, &mut p);
    emit(&[0xC2, 0x30], &mut rom, &mut p);

    // CRV = $3000:  LDA #$3000 ; STA $002203
    emit(&[0xA9, 0x00, 0x30], &mut rom, &mut p);
    emit(&[0x8F, 0x03, 0x22, 0x00], &mut rom, &mut p);
    // CIV = $3010:  LDA #$3010 ; STA $002207
    emit(&[0xA9, 0x10, 0x30], &mut rom, &mut p);
    emit(&[0x8F, 0x07, 0x22, 0x00], &mut rom, &mut p);

    // Back to 8-bit accumulator for byte writes.
    emit(&[0xE2, 0x20], &mut rom, &mut p);

    // Seed SA-1 program at I-RAM $3000:
    //   $3000: LDA #$80 ; STA $220A         A9 80 8D 0A 22   (CIE.7)
    //   $3005: CLI                          58
    //   $3006..$300F: NOP loop              EA…
    //   $3010 (IRQ handler):
    //         LDA #$AA                       A9 AA
    //         STA $3500                      8D 00 35
    //         STP                            DB
    // We store byte-by-byte with STA absolute long ($8F).
    let writes: &[(u32, u8)] = &[
        (0x00_3000, 0xA9), // LDA #$80
        (0x00_3001, 0x80),
        (0x00_3002, 0x8D), // STA $220A
        (0x00_3003, 0x0A),
        (0x00_3004, 0x22),
        (0x00_3005, 0x58), // CLI
        (0x00_3006, 0xEA),
        (0x00_3007, 0xEA),
        (0x00_3008, 0xEA),
        (0x00_3009, 0xEA),
        (0x00_300A, 0xEA),
        (0x00_300B, 0xEA),
        (0x00_300C, 0xEA),
        (0x00_300D, 0xEA),
        (0x00_300E, 0xEA),
        (0x00_300F, 0xEA),
        (0x00_3010, 0xA9), // LDA #
        (0x00_3011, 0xAA),
        (0x00_3012, 0x8D), // STA abs
        (0x00_3013, 0x00),
        (0x00_3014, 0x35),
        (0x00_3015, 0xDB), // STP
    ];
    for (addr, byte) in writes {
        // LDA #imm
        emit(&[0xA9, *byte], &mut rom, &mut p);
        // STA $aabbcc (absolute long: 8F lo mid hi)
        emit(
            &[
                0x8F,
                (*addr & 0xFF) as u8,
                ((*addr >> 8) & 0xFF) as u8,
                ((*addr >> 16) & 0xFF) as u8,
            ],
            &mut rom,
            &mut p,
        );
    }

    // Release SA-1: default CCNT is $20 (bit 5 = reset). A write
    // of $00 clears bit 5, producing the 1→0 release edge.
    emit(&[0xA9, 0x00], &mut rom, &mut p);
    emit(&[0x8F, 0x00, 0x22, 0x00], &mut rom, &mut p);

    // Run-up: 40 NOPs so the SA-1 can reach its NOP loop.
    for _ in 0..40 {
        emit(&[0xEA], &mut rom, &mut p);
    }

    // Trigger SA-1 IRQ: CCNT bit 7 0→1 edge.
    emit(&[0xA9, 0x80], &mut rom, &mut p);
    emit(&[0x8F, 0x00, 0x22, 0x00], &mut rom, &mut p);

    // More NOPs to give the SA-1 time to service the IRQ.
    for _ in 0..60 {
        emit(&[0xEA], &mut rom, &mut p);
    }

    // STP — main CPU done.
    emit(&[0xDB], &mut rom, &mut p);
    let _ = p;
    Cartridge::from_bytes(rom).unwrap()
}

#[test]
fn sa1_main_triggers_irq_and_sa1_handler_runs() {
    // End-to-end IRQ message: main CPU writes CCNT.4 → SA-1 takes
    // an IRQ → SA-1 IRQ handler writes a sentinel into I-RAM. We
    // verify the sentinel landed and the SA-1 has STP'd.
    let cart = demo_sa1_irq_cart();
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    for _ in 0..1500 {
        snes.step();
        if snes.cpu.stopped {
            break;
        }
    }
    assert!(snes.cpu.stopped, "main CPU should have STP'd");
    // The IRQ handler wrote $AA at I-RAM $3500.
    let sentinel = snes.mapper.read(luna_bus::make_addr(0x00, 0x3500));
    assert_eq!(
        sentinel,
        Some(0xAA),
        "SA-1 IRQ handler should have written sentinel"
    );
}
