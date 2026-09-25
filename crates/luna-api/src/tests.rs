//! Unit tests (moved out of the parent file to keep it navigable).

use super::*;

/// Build a minimal 32 KB `LoROM` cart for tests. Has a valid reset
/// vector + cartridge-checksum so the parser accepts it.
fn demo_lorom() -> Vec<u8> {
    demo_lorom_with(&[], None)
}

/// [`demo_lorom`] with `code` placed at `$8000` and, when given, the
/// NMI vectors (native `$FFEA` + emulation `$FFFA`) pointed at `nmi`.
fn demo_lorom_with(code: &[u8], nmi: Option<u16>) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[..code.len()].copy_from_slice(code);
    if let Some(v) = nmi {
        for off in [0x7FEA, 0x7FFA] {
            rom[off] = v as u8;
            rom[off + 1] = (v >> 8) as u8;
        }
    }
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

/// Every stepping entry point is the same loop (`run_core`): they all
/// count instructions, track the call stack, and honour the interrupt
/// flag. `run_until_pc` used to do none of the three.
#[test]
fn every_run_entry_point_gives_the_same_service_level() {
    // $8000: JSR $8010 ; $8003: BRA $8003 …… $8010: NOP NOP NOP RTS
    let mut code = vec![0x20, 0x10, 0x80, 0x80, 0xFE];
    code.resize(0x10, 0xEA);
    code.extend_from_slice(&[0xEA, 0xEA, 0xEA, 0x60]);
    let boot = |e: &mut Emulator| {
        e.load_rom_bytes(demo_lorom_with(&code, None)).unwrap();
        e.enable_call_stack(true);
    };

    // run_until_pc: reaches $8012 inside the subroutine — JSR + 2 NOPs.
    let mut e = Emulator::new();
    boot(&mut e);
    let before = e.instructions_executed();
    assert!(e.run_until_pc(0x00_8012, 100).unwrap());
    assert_eq!(
        e.instructions_executed(),
        before + 3,
        "instructions are counted"
    );
    assert_eq!(e.call_stack().len(), 1, "the JSR was tracked");

    // …the same point reached by `step` reports the same bookkeeping.
    let mut s = Emulator::new();
    boot(&mut s);
    s.step(3).unwrap();
    assert_eq!(s.instructions_executed(), e.instructions_executed());
    assert_eq!(s.call_stack().len(), e.call_stack().len());
    assert_eq!(s.state().cpu.pc, e.state().cpu.pc);

    // A target never reached: the budget bounds it, and says so.
    assert!(!e.run_until_pc(0x00_9999, 50).unwrap());

    // A raised interrupt flag stops every interruptible variant at once.
    let raised = std::sync::atomic::AtomicBool::new(true);
    let mut i = Emulator::new();
    boot(&mut i);
    let before = i.instructions_executed();
    assert!(
        !i.run_until_pc_interruptible(0x00_8012, 100, &raised)
            .unwrap()
    );
    assert_eq!(
        i.step_until_frame_interruptible(1_000_000, &raised)
            .unwrap(),
        0
    );
    assert_eq!(i.step_interruptible(100, &raised).unwrap(), 0);
    assert!(
        i.run_until_break_interruptible(100, &raised)
            .unwrap()
            .interrupted
    );
    assert_eq!(i.instructions_executed(), before);
}

/// [`demo_lorom`] on a board with 8 KB of battery SRAM — a mapper whose
/// save-state blob carries a RAM the loader must size-check.
fn demo_lorom_sram() -> Vec<u8> {
    let mut rom = demo_lorom();
    rom[0x7FD8] = 0x03;
    let sum: u32 = rom
        .iter()
        .enumerate()
        .filter(|(i, _)| !(0x7FDC..=0x7FDF).contains(i))
        .map(|(_, b)| u32::from(*b))
        .sum();
    let checksum = (sum & 0xFFFF) as u16;
    rom[0x7FDC..0x7FDE].copy_from_slice(&(!checksum).to_le_bytes());
    rom[0x7FDE..0x7FE0].copy_from_slice(&checksum.to_le_bytes());
    rom
}

/// Re-wrap a good state's core with a different mapper blob.
fn state_with_mapper_blob(good: &[u8], mapper: Vec<u8>) -> Vec<u8> {
    let (mut bundle, _): (SaveStateBundle, usize) =
        bincode::serde::decode_from_slice(good, bincode::config::standard()).unwrap();
    bundle.mapper = mapper;
    bincode::serde::encode_to_vec(&bundle, bincode::config::standard()).unwrap()
}

#[test]
fn load_state_refuses_a_bad_mapper_blob_and_leaves_the_machine_untouched() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_sram()).unwrap();
    e.step(2_000).unwrap();
    e.load_sram(&[0xA5; 64]).unwrap();
    let good = e.save_state().unwrap();
    e.step(2_000).unwrap();
    let before = e.save_state().unwrap();

    let cfg = bincode::config::standard();
    let cases = [
        ("undecodable", vec![0xFF; 7]),
        (
            "SRAM too short",
            bincode::serde::encode_to_vec(vec![0u8; 16], cfg).unwrap(),
        ),
        (
            "SRAM empty",
            bincode::serde::encode_to_vec(Vec::<u8>::new(), cfg).unwrap(),
        ),
    ];
    for (what, blob) in cases {
        let forged = state_with_mapper_blob(&good, blob);
        match e.load_state(&forged) {
            Err(ApiError::SaveState(msg)) => assert!(msg.contains("mapper"), "{what}: {msg}"),
            other => panic!("{what}: expected a SaveState error, got {other:?}"),
        }
        // Refused ⇒ nothing moved: not the core, not the mapper.
        assert_eq!(
            e.save_state().unwrap(),
            before,
            "{what}: machine was modified"
        );
        // …and the SRAM keeps its size and contents.
        assert_eq!(e.sram().len(), 8 * 1024, "{what}");
        assert_eq!(e.sram()[..64], [0xA5; 64], "{what}");
    }
    // The genuine state still loads.
    e.load_state(&good).unwrap();
}

/// `run_until_gsu` watches the GO/STOP *transition*, not the level, and
/// refuses a cartridge that has no Super FX rather than reporting a miss.
#[test]
fn run_until_gsu_needs_a_super_fx_and_watches_the_edge() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    // A plain LoROM has no GSU: a silent `false` would read as "the job
    // never finished", sending the caller after the wrong thing.
    match e.run_until_gsu(false, 1000) {
        Err(ApiError::BadArg(m)) => assert!(m.contains("no Super FX"), "{m}"),
        other => panic!("expected a BadArg, got {other:?}"),
    }
}

/// The stack watermark is the deepest `S` a run actually reached, not the
/// value it started at — and it ignores emulation mode, where the hardware
/// pins `S` to page 1 and the figure would say nothing about the program.
#[test]
fn the_stack_watermark_is_the_deepest_native_reach() {
    // CLC, XCE            → native
    // LDX #$1FFF, TXS     → stack at $1FFF
    // PHA, PHA, PHA       → three bytes deep (A is 16-bit? no: M defaults
    //                        to 8-bit after reset, so one byte each)
    // BRA -2              → park
    let code = [
        0x18, 0xFB, // CLC, XCE       → native, S still $01FF from reset
        0xC2, 0x10, // REP #$10       → 16-bit X (A stays 8-bit, so PHA is 1 byte)
        0xA2, 0xFF, 0x1F, // LDX #$1FFF
        0x9A, // TXS            → the real stack
        0x48, 0x48, 0x48, // PHA PHA PHA
        0x80, 0xFE, // BRA -2
    ];
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_with(&code, None)).unwrap();

    // Nothing has run: no native instruction, so nothing to report.
    assert!(e.stack_low().is_none());

    // CLC, XCE, REP, LDX, TXS: native for four of them, and S was $01FF
    // for three — but nothing pushed, so there is still nothing to report.
    // This is the case that makes the figure usable: an inherited $01FF
    // would otherwise sit below every floor worth checking, for ever.
    e.step(5).unwrap();
    assert!(
        e.stack_low().is_none(),
        "S was $01FF in native mode, but no instruction pushed"
    );

    // Each PHA takes it one byte lower.
    e.step(3).unwrap();
    let low = e.stack_low().expect("three pushes happened");
    assert_eq!(low.sp, 0x1FFC, "three pushes from $1FFF");
    assert_eq!(
        low.pc, 0x00_800A,
        "the third PHA is the instruction that got there"
    );

    // The mark is a low-WATER mark: pulling back up does not raise it.
    let before = e.stack_low().unwrap().sp;
    e.step(1).unwrap(); // BRA, no stack traffic
    assert_eq!(e.stack_low().unwrap().sp, before);

    // A reset starts a new measurement.
    e.reset().unwrap();
    assert!(e.stack_low().is_none());
}

/// A firmware dump cannot be re-downloaded, so installing one must never
/// be able to destroy a working install. The source is vetted before the
/// destination is touched at all.
#[test]
fn install_firmware_vets_the_source_before_touching_anything() {
    let dir = std::env::temp_dir().join(format!("luna_fw_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // An existing-but-empty file: exactly the case that replaced a valid
    // 8 KB dump with 0 bytes and reported success.
    let empty = dir.join("empty.rom");
    std::fs::write(&empty, b"").unwrap();
    let err = Emulator::install_firmware(&empty, "dsp1b.rom").unwrap_err();
    assert!(matches!(err, ApiError::Firmware(_)), "{err:?}");
    assert!(err.to_string().contains("8192"), "{err}");

    // Truncated, and absent, are refused the same way.
    let short = dir.join("short.rom");
    std::fs::write(&short, vec![0u8; 4096]).unwrap();
    assert!(matches!(
        Emulator::install_firmware(&short, "dsp1b.rom"),
        Err(ApiError::Firmware(_))
    ));
    assert!(matches!(
        Emulator::install_firmware(&dir.join("absent.rom"), "dsp1b.rom"),
        Err(ApiError::Firmware(_))
    ));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn firmware_size_is_known_only_for_names_luna_recognises() {
    assert_eq!(Emulator::expected_firmware_len("dsp1b.rom"), Some(8192));
    assert_eq!(Emulator::expected_firmware_len("dsp1.rom"), Some(8192));
    // An unknown name carries no expectation, so it is not silently
    // rejected — only names luna knows the shape of are enforced.
    assert_eq!(Emulator::expected_firmware_len("st010.rom"), None);
}

/// The serialized shape of the machine is part of the save-state
/// format. bincode is positional: adding, removing or reordering a
/// serialized field silently mis-decodes every older state, and
/// `#[serde(default)]` does NOT help. If this test fails you changed
/// that shape — bump [`SAVE_STATE_VERSION`] (documenting why), then
/// update the two lengths below. (The lengths are of a freshly built
/// machine; a changed power-on *value* can move them too, through the
/// varint encoding — then only the lengths need updating.)
#[test]
fn save_state_shape_is_pinned_to_the_version() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_sram()).unwrap();
    let (bundle, _): (SaveStateBundle, usize) =
        bincode::serde::decode_from_slice(&e.save_state().unwrap(), bincode::config::standard())
            .unwrap();
    assert_eq!(
        (SAVE_STATE_VERSION, bundle.core.len(), bundle.mapper.len()),
        (7, 447_758, 8_195),
        "serialized machine shape changed — see this test's doc comment"
    );
}

#[test]
fn mclk_buckets_partition_total_and_wai_is_idle() {
    // SEI ; LDA #$80 ; STA $4200 (NMI on) ; loop: WAI ; BRA loop ;
    // nmi: RTI — the canonical "wait for VBlank" idle ROM.
    let code = [0x78, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0xCB, 0x80, 0xFD, 0x40];
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_with(&code, Some(0x8009)))
        .unwrap();
    for _ in 0..3 {
        e.step_until_frame(1_000_000).unwrap();
    }
    let st = e.state();
    let s = &st.stats;
    // Exact partition of the master clock, cumulative and per frame.
    assert_eq!(s.mclk.total, s.total_mclk, "{:?}", s.mclk);
    assert_eq!(
        s.mclk.cpu_active
            + s.mclk.cpu_wai
            + s.mclk.cpu_stp
            + s.mclk.dma
            + s.mclk.hdma
            + s.mclk.refresh,
        s.mclk.total
    );
    // An idle ROM: WAI dominates, the CPU did very little, no DMA/HDMA.
    assert!(s.mclk.cpu_wai > s.mclk.cpu_active * 10, "{:?}", s.mclk);
    assert_eq!(s.mclk.dma, 0);
    assert_eq!(s.mclk.hdma, 0);
    assert_eq!(s.mclk.cpu_stp, 0);
    // DRAM refresh: 40 mclk once per scanline, every line of every frame.
    let lines = st.scheduler.frame_count * 262 + u64::from(st.scheduler.ppu_line);
    assert!(
        (s.mclk.refresh / 40).abs_diff(lines) <= 1,
        "refresh {} lines {lines}",
        s.mclk.refresh
    );
    // The last frame's buckets add up to one NTSC frame, give or take
    // the bus access the boundary fell inside.
    let frame = 262 * 1364;
    assert!(
        s.last_frame.total.abs_diff(frame) <= 48,
        "last_frame {:?}",
        s.last_frame
    );
    assert!(
        s.last_frame.cpu_wai * 10 > s.last_frame.total * 9,
        "{:?}",
        s.last_frame
    );
    // Parked WAI ticks are steps but not instructions.
    assert!(s.instructions_active < s.instructions_executed / 10);
    assert!(s.instructions_active >= 4);
}

#[test]
fn mclk_dma_burst_lands_in_the_dma_bucket() {
    // Fixed-source DMA of $1000 bytes from $00:8000 to $2118 (VRAM):
    // LDA #$01 ; STA $4300 (mode 1, A→B) ; LDA #$18 ; STA $4301 ;
    // LDX #$8000 ; STX $4302 ; LDA #$00 ; STA $4304 ;
    // LDX #$1000 ; STX $4305 ; LDA #$01 ; STA $420B ; STP
    let code = [
        0x18, 0xFB, 0xC2, 0x10, // CLC ; XCE (native) ; REP #$10 (16-bit X)
        0xA9, 0x01, 0x8D, 0x00, 0x43, 0xA9, 0x18, 0x8D, 0x01, 0x43, 0xA2, 0x00, 0x80, 0x8E, 0x02,
        0x43, 0xA9, 0x00, 0x8D, 0x04, 0x43, 0xA2, 0x00, 0x10, 0x8E, 0x05, 0x43, 0xA9, 0x01, 0x8D,
        0x0B, 0x42, 0xDB,
    ];
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_with(&code, None)).unwrap();
    e.step(200).unwrap();
    let s = e.state().stats;
    assert_eq!(s.mclk.total, s.total_mclk);
    // 8 mclk per byte, plus the burst's fixed overhead.
    assert!(s.mclk.dma >= 0x1000 * 8, "{:?}", s.mclk);
    assert!(s.mclk.dma < 0x1000 * 8 + 200, "{:?}", s.mclk);
    // A CPU halted by STP charges no clocks at all (`Cpu::step` returns
    // before any bus access), so its bucket stays empty; the parked
    // steps are still idle steps.
    assert_eq!(s.mclk.cpu_stp, 0);
}

#[test]
fn power_on_state_applies_on_load_and_is_reproducible() {
    let mut e = Emulator::new();
    assert_eq!(e.power_on(), PowerOnState::Zero);
    e.load_rom_bytes(demo_lorom()).unwrap();
    assert!(
        e.peek_memory(0x7E, 0x1000, 64)
            .unwrap()
            .iter()
            .all(|&b| b == 0)
    );

    e.set_power_on(PowerOnState::Random { seed: 0x00C0_FFEE });
    e.load_rom_bytes(demo_lorom()).unwrap();
    let wram_a = e.peek_memory(0x7E, 0x1000, 64).unwrap();
    let aram_a = e.peek_aram(0x2000, 64).unwrap();
    let vram_a = e.peek_vram(0x4000, 64).unwrap();
    assert!(wram_a.iter().any(|&b| b != 0));
    assert!(aram_a.iter().any(|&b| b != 0));
    assert!(vram_a.iter().any(|&b| b != 0));
    // Same seed, same machine.
    e.load_rom_bytes(demo_lorom()).unwrap();
    assert_eq!(e.peek_memory(0x7E, 0x1000, 64).unwrap(), wram_a);
    assert_eq!(e.peek_aram(0x2000, 64).unwrap(), aram_a);
    assert_eq!(e.peek_vram(0x4000, 64).unwrap(), vram_a);
    // Different seed, different machine.
    e.set_power_on(PowerOnState::Random { seed: 0x00C0_FFEF });
    e.load_rom_bytes(demo_lorom()).unwrap();
    assert_ne!(e.peek_memory(0x7E, 0x1000, 64).unwrap(), wram_a);
    // Ones.
    e.set_power_on(PowerOnState::Ones);
    e.load_rom_bytes(demo_lorom()).unwrap();
    assert!(
        e.peek_memory(0x7E, 0x1000, 64)
            .unwrap()
            .iter()
            .all(|&b| b == 0xFF)
    );
    // CGRAM stays 15-bit.
    assert!(e.peek_cgram().unwrap().iter().all(|&w| w & 0x8000 == 0));
}

#[test]
fn a_super_multitap_serves_players_2_to_5() {
    // The multitap protocol as a game drives it: auto-read with iobit
    // high gives pads A/B on port 2's d0/d1 ($421A / $421E), then iobit
    // low and 16 manual $4017 reads give pads C/D on d0/d1.
    let code = [
        0xA9, 0x01, 0x8D, 0x00, 0x42, // LDA #$01 : STA $4200 (auto-read)
        0xAD, 0x12, 0x42, 0x10, 0xFB, // wait: LDA $4212 : BPL wait (vblank)
        0xAD, 0x12, 0x42, 0x29, 0x01, 0xD0, 0xF9, // busy: AND #1 : BNE
        0xAD, 0x1A, 0x42, 0x8D, 0x00, 0x00, // JOY2L -> $00
        0xAD, 0x1B, 0x42, 0x8D, 0x01, 0x00, // JOY2H -> $01
        0xAD, 0x1E, 0x42, 0x8D, 0x02, 0x00, // JOY4L -> $02
        0xAD, 0x1F, 0x42, 0x8D, 0x03, 0x00, // JOY4H -> $03
        0xA9, 0x00, 0x8D, 0x01, 0x42, // LDA #0 : STA $4201 (iobit low)
        0xA2, 0x10, // LDX #16
        0xAD, 0x17, 0x40, 0x9D, 0x0F, 0x00, 0xCA, 0xD0, 0xF7, // LDA $4017 : STA $0F,X
        0x80, 0xFE, // BRA *
    ];
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_with(&code, None)).unwrap();
    e.set_port_device(1, PortDevice::Multitap).unwrap();
    for (port, mask) in [(1, 0x8000), (2, 0x4000), (3, 0x2000), (4, 0x1000)] {
        e.set_joypad(port, mask).unwrap(); // B, Y, Select, Start
    }
    e.step(200_000).unwrap();
    let auto = e.peek_memory(0x7E, 0x0000, 4).unwrap();
    assert_eq!(auto, [0x00, 0x80, 0x00, 0x40], "player 2 = B, player 3 = Y");
    let raw: Vec<u8> = e
        .peek_memory(0x7E, 0x0010, 16)
        .unwrap()
        .into_iter()
        .rev()
        .collect();
    // Only bits 0-1 are the controller; $4017's bits 2-4 are tied high and
    // the rest is open bus (ares `cpu/io.cpp:19-22`, Mesen2 `|= 0x1C`).
    assert!(
        raw.iter().all(|b| b & 0x1C == 0x1C),
        "$4017 bits 2-4 read high on every clock: {raw:02X?}"
    );
    let manual: Vec<u8> = raw.iter().map(|b| b & 0x03).collect();
    assert_eq!(
        manual[..5],
        [0, 0, 1, 2, 0],
        "player 4 Select on d0 at bit 2, player 5 Start on d1 at bit 3"
    );
    assert_eq!(luna_api_parse("multitap"), Ok(PortDevice::Multitap));
}

fn luna_api_parse(s: &str) -> Result<PortDevice, String> {
    super::parse_port_device(s)
}

#[test]
fn a_dma_to_the_apu_ports_reaches_the_spc700() {
    // $2140-$2143 are on the B-bus: a DMA reaches them like the CPU
    // does (ares routes both through `bus.write(0x2100 | addr)`). The
    // DMA path used to drop them. Mode 4 = 4 registers B, B+1, B+2, B+3.
    let code = [
        0xA9, 0x40, 0x8D, 0x01, 0x43, // LDA #$40 : STA $4301 (BBAD $2140)
        0xA9, 0x00, 0x8D, 0x02, 0x43, // LDA #$00 : STA $4302
        0xA9, 0x20, 0x8D, 0x03, 0x43, // LDA #$20 : STA $4303
        0xA9, 0x7E, 0x8D, 0x04, 0x43, // LDA #$7E : STA $4304 ($7E:2000)
        0xA9, 0x04, 0x8D, 0x05, 0x43, // LDA #$04 : STA $4305 (4 bytes)
        0xA9, 0x00, 0x8D, 0x06, 0x43, // LDA #$00 : STA $4306
        0xA9, 0x04, 0x8D, 0x00, 0x43, // LDA #$04 : STA $4300 (mode 4)
        0xA9, 0x01, 0x8D, 0x0B, 0x42, // LDA #$01 : STA $420B
        0x80, 0xFE, // BRA * (keep the SPC clocked)
    ];
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_with(&code, None)).unwrap();
    e.poke_memory(0x7E, 0x2000, &[0x11, 0x22, 0x33, 0x44])
        .unwrap();
    e.step(200).unwrap();
    assert_eq!(e.state().apu.to_spc_ports, [0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn run_until_mem_write_fires_on_a_dma_write_and_the_trace_says_dma() {
    // Channel 0: 4 bytes from $7E:2000 to $2122, triggered by $420B —
    // no CPU instruction ever writes $2122 (issue #226). Both DAS bytes
    // are written, as a real game must: the channel registers power up
    // at $FF (issue #224's second lot), so leaving $4306 alone would
    // ask for $FF04 bytes.
    let code = [
        0xA9, 0x22, 0x8D, 0x01, 0x43, // LDA #$22 : STA $4301
        0xA9, 0x00, 0x8D, 0x02, 0x43, // LDA #$00 : STA $4302
        0xA9, 0x20, 0x8D, 0x03, 0x43, // LDA #$20 : STA $4303
        0xA9, 0x7E, 0x8D, 0x04, 0x43, // LDA #$7E : STA $4304
        0xA9, 0x04, 0x8D, 0x05, 0x43, // LDA #$04 : STA $4305 (DAS low)
        0xA9, 0x00, 0x8D, 0x06, 0x43, // LDA #$00 : STA $4306 (DAS high)
        0xA9, 0x00, 0x8D, 0x00, 0x43, // LDA #$00 : STA $4300
        0xA9, 0x01, 0x8D, 0x0B, 0x42, // LDA #$01 : STA $420B
        0xDB, // STP
    ];
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_with(&code, None)).unwrap();
    e.enable_mem_trace_filtered(
        100,
        MemTraceFilter {
            only_offsets: Some(vec![0x2122]),
            writes_only: true,
            ..MemTraceFilter::default()
        },
    )
    .unwrap();
    let hit = e.run_until_mem_write(0x00_2122, 1000).unwrap();
    assert!(hit.is_some(), "the DMA write must trip the watchpoint");
    e.step(100).ok();
    let ev = e.take_mem_trace_log().unwrap();
    assert_eq!(ev.len(), 4, "{ev:?}");
    assert!(ev.iter().all(|x| x.origin == MemOrigin::Dma(0)), "{ev:?}");
}

#[test]
fn profile_folds_pcs_onto_symbols_and_pages() {
    // main: SEI ; LDA #$80 ; STA $4200 ; loop: WAI ; BRA loop ;
    // nmi (at $8009): RTI — the canonical idle ROM, labelled.
    let code = [0x78, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0xCB, 0x80, 0xFD, 0x40];
    let mut e = Emulator::new();
    assert!(matches!(e.enable_profile(), Err(ApiError::NoRom)));
    e.load_rom_bytes(demo_lorom_with(&code, Some(0x8009)))
        .unwrap();
    e.load_symbols_str("[labels]\n00:8000 main\n00:8006 wait_vblank\n00:8009 nmi_handler\n");
    e.enable_profile().unwrap();
    for _ in 0..3 {
        e.step_until_frame(1_000_000).unwrap();
    }
    let r = e.take_profile().unwrap();
    assert!(r.total_mclk > 0);
    let by_name = |n: &str| r.entries.iter().find(|x| x.symbol == n).cloned();
    let wait = by_name("wait_vblank").expect("wait_vblank row");
    let main = by_name("main").expect("main row");
    let nmi = by_name("nmi_handler").expect("nmi row");
    // The parked WAI dominates and is idle time; main ran three
    // instructions once; the handler ran once per frame.
    assert!(wait.mclk * 10 > r.total_mclk * 9, "{r:?}");
    assert!(wait.idle_mclk * 10 > wait.mclk * 9, "{r:?}");
    assert_eq!(main.instructions, 3);
    assert_eq!(main.idle_mclk, 0);
    assert!(nmi.instructions >= 2, "{r:?}");
    assert_eq!(wait.pcs, 2, "WAI + BRA");
    // Heaviest first, percentages sum to ~100.
    assert_eq!(r.entries[0].symbol, "wait_vblank");
    let pct: f64 = r.entries.iter().map(|x| x.pct).sum();
    assert!((pct - 100.0).abs() < 1e-6, "{pct}");
    // Without symbols, PCs fold onto their page.
    e.clear_symbols();
    e.enable_profile().unwrap();
    e.step_until_frame(1_000_000).unwrap();
    let r = e.take_profile().unwrap();
    assert_eq!(r.entries.len(), 1, "{r:?}");
    assert_eq!(r.entries[0].symbol, "$00:8000 (no symbol)");
    assert_eq!(r.entries[0].addr, 0x00_8000);
}

#[test]
fn profile_reports_the_cost_per_frame() {
    // `OpenSNES` R-B: each row's worst / mean completed frame. On the
    // idle ROM `main` runs once, in the first frame; the handler and
    // the wait loop run every frame.
    let code = [0x78, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0xCB, 0x80, 0xFD, 0x40];
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom_with(&code, Some(0x8009)))
        .unwrap();
    e.load_symbols_str("[labels]\n00:8000 main\n00:8006 wait_vblank\n00:8009 nmi_handler\n");
    e.enable_profile().unwrap();
    for _ in 0..4 {
        e.step_until_frame(1_000_000).unwrap();
    }
    // A few instructions into frame 4: that partial frame must not
    // count anywhere.
    e.step(3).unwrap();
    let r = e.take_profile().unwrap();
    assert_eq!(r.frames, 4, "{r:?}");
    let by_name = |n: &str| r.entries.iter().find(|x| x.symbol == n).cloned().unwrap();
    let main = by_name("main").per_frame.expect("main ran in frame 0");
    assert_eq!(main.frames, 1);
    assert_eq!(main.max_frame, 0);
    assert_eq!(main.max, by_name("main").mclk);
    assert_eq!(
        main.mean,
        by_name("main").mclk / 4,
        "mean over every completed frame"
    );
    let nmi = by_name("nmi_handler")
        .per_frame
        .expect("handler ran every frame");
    assert_eq!(nmi.frames, 4);
    assert!(nmi.max > 0 && nmi.max >= nmi.mean, "{nmi:?}");
    let wait = by_name("wait_vblank")
        .per_frame
        .expect("wait loop ran every frame");
    assert_eq!(wait.frames, 4);
    assert!(wait.max >= wait.mean && wait.mean > 0, "{wait:?}");
    // The rows' per-frame totals add up to the window's whole frames,
    // give or take the boundary steps: sum of means × frames ≈ the
    // total less the partial frame.
    let sum_means: u64 = r
        .entries
        .iter()
        .filter_map(|x| x.per_frame)
        .map(|p| p.mean)
        .sum();
    assert!(sum_means * 4 <= r.total_mclk, "{r:?}");

    // Taking empties the per-frame figures too; a window with no
    // completed frame reports none.
    e.step(10).unwrap();
    let r = e.take_profile().unwrap();
    assert_eq!(r.frames, 0);
    assert!(r.entries.iter().all(|x| x.per_frame.is_none()), "{r:?}");
}

#[test]
fn wram_page_hashes_bad_page_size_is_err_not_panic() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // Valid: default (0 → 4 KiB) and an explicit power of two.
    assert_eq!(e.wram_page_hashes(0).unwrap().len(), 32);
    assert_eq!(e.wram_page_hashes(0x8000).unwrap().len(), 4);

    // Invalid sizes must come back as BadArg — a bad argument from an
    // MCP client must never panic the transport.
    for bad in [3usize, 0x1001, 0x4_0000] {
        match e.wram_page_hashes(bad) {
            Err(ApiError::BadArg(_)) => {}
            other => panic!("expected BadArg for {bad:#x}, got {other:?}"),
        }
    }
}

/// Save-state v5 (#167): the stable FNV-1a rom hash matches the
/// published test vectors (i.e. the algorithm can never drift with the
/// toolchain), a v5 blob round-trips, and stale or foreign blobs are
/// rejected with a clean error.
#[test]
fn save_state_v5_stable_hash_roundtrip_and_rejections() {
    // FNV-1a 64 reference vectors — if these move, every persisted
    // rom_hash breaks, so they are pinned here.
    assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);

    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // Current-version round-trip.
    let blob = e.save_state().unwrap();
    e.load_state(&blob).unwrap();

    // A stale-version bundle is rejected by the version gate.
    let stale = SaveStateBundle {
        version: SAVE_STATE_VERSION - 1,
        rom_hash: 0,
        core: Vec::new(),
        mapper: Vec::new(),
    };
    let bytes = bincode::serde::encode_to_vec(&stale, bincode::config::standard()).unwrap();
    match e.load_state(&bytes) {
        Err(ApiError::SaveState(msg)) => assert!(msg.contains("version")),
        other => panic!("expected SaveState version error, got {other:?}"),
    }

    // Garbage (e.g. an old bincode-1 blob) errors cleanly, never panics.
    assert!(matches!(
        e.load_state(&[0xFF; 16]),
        Err(ApiError::SaveState(_))
    ));
}

#[test]
fn call_stack_tracks_nested_calls_and_returns() {
    let mut rom = demo_lorom();
    // $8000: JSR $8010 ; then spin. $8010: JSL $008020 ; RTS.
    // $8020: RTL.
    let prog: &[(usize, &[u8])] = &[
        (0x0000, &[0x20, 0x10, 0x80, 0x80, 0xFE]),
        (0x0010, &[0x22, 0x20, 0x80, 0x00, 0x60]),
        (0x0020, &[0x6B]),
    ];
    for &(off, bytes) in prog {
        rom[off..off + bytes.len()].copy_from_slice(bytes);
    }
    let mut e = Emulator::new();
    e.load_rom_bytes_forced(rom, luna_core::MapperKind::LoRom)
        .unwrap();
    e.load_symbols_str("[labels]\n00:8010 sub1\n00:8020 sub2\n");

    // Off by default: empty and free.
    e.step(1).unwrap();
    assert!(e.call_stack().is_empty());

    // Reset to the entry point and track from the top.
    e.reset().unwrap();
    e.enable_call_stack(true);
    e.step(2).unwrap(); // JSR, then JSL
    let stack = e.call_stack();
    assert_eq!(stack.len(), 2);
    assert_eq!((stack[0].from, stack[0].pc), (0x00_8000, 0x00_8010));
    assert_eq!(stack[0].kind, CallKind::Jsr);
    assert_eq!(stack[0].symbol.as_deref(), Some("sub1"));
    assert_eq!((stack[1].from, stack[1].pc), (0x00_8010, 0x00_8020));
    assert_eq!(stack[1].kind, CallKind::Jsl);
    assert_eq!(stack[1].symbol.as_deref(), Some("sub2"));

    // RTL then RTS unwind to empty.
    e.step(2).unwrap();
    assert!(e.call_stack().is_empty());

    // state() embeds it only while tracking.
    assert!(e.state().call_stack.is_some());
    e.enable_call_stack(false);
    assert!(e.state().call_stack.is_none());
}

#[test]
fn pokes_reach_all_memory_spaces() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    e.poke_vram(0x1000, &[0xAB, 0xCD]).unwrap();
    assert_eq!(e.peek_vram(0x1000, 2).unwrap(), vec![0xAB, 0xCD]);

    e.poke_cgram(0x0002, &[0x1F, 0x00]).unwrap(); // entry 1 = red
    assert_eq!(e.peek_cgram().unwrap()[1], 0x001F);

    e.poke_oam(0x0004, &[0x77]).unwrap();
    assert_eq!(e.peek_oam().unwrap()[4], 0x77);

    e.poke_aram(0x4000, &[0x99]).unwrap();
    assert_eq!(e.peek_aram(0x4000, 1).unwrap(), vec![0x99]);
}

#[test]
fn freezes_reapply_on_every_run_path() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // Non-WRAM addresses are rejected; low-mirror folds to $7E.
    assert!(e.freeze_add(0x00_8000, 1).is_err());
    e.freeze_add(0x00_0180, 0x11).unwrap();
    assert_eq!(e.freeze_list()[0].addr, 0x7E_0180);

    // freeze_add applies immediately.
    e.freeze_add(0x7E_0300, 0x55).unwrap();
    assert_eq!(e.peek_memory(0x7E, 0x0300, 1).unwrap(), vec![0x55]);

    // Simulate the game clobbering it, then cross a frame boundary
    // through the INTERRUPTIBLE run loop — the GUI's drive path
    // (the API-first coherence requirement of issue #178).
    e.poke_memory(0x7E, 0x0300, &[0x00]).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    e.run_until_break_interruptible(50_000, &stop).unwrap();
    assert_eq!(
        e.peek_memory(0x7E, 0x0300, 1).unwrap(),
        vec![0x55],
        "freeze re-applied at the frame edge inside run_until_break_interruptible"
    );

    // Same through plain step().
    e.poke_memory(0x7E, 0x0300, &[0x00]).unwrap();
    e.step(50_000).unwrap();
    assert_eq!(e.peek_memory(0x7E, 0x0300, 1).unwrap(), vec![0x55]);

    // Remove: the value stops being pinned.
    assert!(e.freeze_remove(0x7E_0300).unwrap());
    assert!(!e.freeze_remove(0x7E_0300).unwrap());
    e.poke_memory(0x7E, 0x0300, &[0x00]).unwrap();
    e.step(50_000).unwrap();
    assert_eq!(e.peek_memory(0x7E, 0x0300, 1).unwrap(), vec![0x00]);
}

#[test]
fn search_memory_reports_7f_hits_in_bank_7f() {
    // Issue #177's opening bug: hits in the WRAM high half surfaced
    // as impossible `$7E:1xxxx` addresses.
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.poke_memory(0x7F, 0x0123, &[0xCA, 0xFE]).unwrap();
    let hits = e.search_memory(&[0xCA, 0xFE]).unwrap();
    assert_eq!(hits, vec![0x7F_0123]);
}

#[test]
fn narrowing_search_session_converges() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // No session yet → refine/results are usage errors.
    assert!(e.search_refine(SearchOp::Changed, None).is_err());
    assert!(e.search_results(10).is_err());

    // Plant a "player HP" u16 variable and begin.
    e.poke_memory(0x7E, 0x0300, &[0x64, 0x00]).unwrap(); // 100
    let n = e.search_begin(SearchWidth::U16).unwrap();
    assert_eq!(n, 0x1FFFF);

    // Round 1: value == 100 — the variable (plus coincidences) survive.
    let n = e.search_refine(SearchOp::Eq, Some(100)).unwrap();
    assert!(n >= 1);

    // "Take damage": 100 → 73, everything else untouched.
    e.poke_memory(0x7E, 0x0300, &[0x49, 0x00]).unwrap();
    let n = e.search_refine(SearchOp::Changed, None).unwrap();
    assert!(n >= 1);
    let n = e.search_refine(SearchOp::Eq, Some(73)).unwrap();
    assert_eq!(n, 1, "converged to exactly the planted variable");
    let hits = e.search_results(10).unwrap();
    assert_eq!(hits[0].addr, 0x7E_0300);
    assert_eq!(hits[0].value, 73);

    // Ops that need a value reject its absence.
    assert!(e.search_refine(SearchOp::Lt, None).is_err());
}

#[test]
fn debug_poke_search_run_until_set_register() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // L8 poke + L9 search round-trip through WRAM.
    assert_eq!(
        e.poke_memory(0x7E, 0x0100, &[0xDE, 0xAD, 0xBE, 0xEF])
            .unwrap(),
        4
    );
    assert_eq!(
        e.peek_memory(0x7E, 0x0100, 4).unwrap(),
        vec![0xDE, 0xAD, 0xBE, 0xEF]
    );
    // low-RAM mirror writes the same bytes ($00:0100 → $7E:0100).
    assert_eq!(e.peek_memory(0x00, 0x0100, 2).unwrap(), vec![0xDE, 0xAD]);
    assert!(
        e.search_memory(&[0xDE, 0xAD, 0xBE, 0xEF])
            .unwrap()
            .contains(&0x7E_0100)
    );
    assert!(e.search_memory(&[]).unwrap().is_empty());

    // L10 set_cpu_register + run_until_pc.
    e.set_cpu_register("pb", 0x00).unwrap();
    e.set_cpu_register("pc", 0x9000).unwrap();
    e.set_cpu_register("a", 0x1234).unwrap();
    assert!(e.set_cpu_register("bogus", 0).is_err());
    // Already at $00:9000 → run_until returns immediately.
    assert!(e.run_until_pc(0x00_9000, 10).unwrap());
    // A PC we won't reach within 1 step from here is not hit.
    assert!(!e.run_until_pc(0x12_3456, 1).unwrap());

    // L7: a memory-write breakpoint on an address the boot code never
    // touches returns None within the step budget (no panic, no hang).
    assert_eq!(e.run_until_mem_write(0x7E_FFFE, 50).unwrap(), None);
    assert_eq!(e.run_until_mem_read(0x7E_FFFE, 50).unwrap(), None);
}

/// The legacy L7 run-until paths ride the breakpoint registry now: the
/// hit is reported exactly as before, a mem trace the caller enabled
/// survives the call (the old implementation silently replaced it), and
/// the temporary watchpoint never leaks into the registry.
#[test]
fn run_until_mem_write_hits_and_preserves_the_mem_trace() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // Same injected loop as the breakpoint test:
    //   0100: A9 42     LDA #$42
    //   0102: 8D 00 02  STA $0200
    //   0105: 4C 00 01  JMP $0100
    e.poke_memory(
        0x7E,
        0x0100,
        &[0xA9, 0x42, 0x8D, 0x00, 0x02, 0x4C, 0x00, 0x01],
    )
    .unwrap();
    e.set_cpu_register("pb", 0x00).unwrap();
    e.set_cpu_register("pc", 0x0100).unwrap();
    e.set_cpu_register("db", 0x00).unwrap();

    // A mem trace the caller enabled before the run...
    e.enable_mem_trace(10_000, None, Some((0x0100, 0x02FF)))
        .unwrap();

    // The write is found and reported with the accessing PC + value.
    let hit = e.run_until_mem_write(0x00_0200, 100).unwrap();
    assert_eq!(hit, Some((0x00_0102, 0x42)));

    // ...is still live and captured the run's accesses.
    let events = e.take_mem_trace_log().unwrap();
    assert!(
        events.iter().any(|ev| ev.addr_full == 0x00_0200),
        "caller's mem trace was clobbered by run_until_mem_write"
    );

    // The temporary watchpoint did not leak into the registry.
    assert!(e.bp_list().unwrap().is_empty());

    // Unhit address: still None within budget.
    assert_eq!(e.run_until_mem_write(0x7E_FFFE, 50).unwrap(), None);
}

/// WLA-DX symbols (issue #67): parse, resolve, annotate the
/// disassembly, and drive the address-taking APIs by name.
#[test]
fn symbol_table_resolves_and_annotates() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // Same injected loop as the breakpoint test, now with names.
    e.poke_memory(
        0x7E,
        0x0100,
        &[0xA9, 0x42, 0x8D, 0x00, 0x02, 0x4C, 0x00, 0x01],
    )
    .unwrap();
    let n = e.load_symbols_str("[labels]\n00:0100 main\n00:0105 main_jump\n00:0200 monster_x\n");
    assert_eq!(n, 3);
    assert_eq!(e.resolve_symbol("main"), Some(0x00_0100));
    assert_eq!(e.resolve_symbol("nope"), None);
    assert_eq!(e.symbol_for_addr(0x00_0102).as_deref(), Some("main+0x02"));

    // Disassembly lines carry the nearest label.
    e.set_cpu_register("pb", 0x00).unwrap();
    e.set_cpu_register("pc", 0x0100).unwrap();
    let lines = e.disassemble_cpu(0x00_0100, 3, true, true).unwrap();
    assert_eq!(lines[0].symbol.as_deref(), Some("main"));
    assert_eq!(lines[1].symbol.as_deref(), Some("main+0x02"));
    assert_eq!(lines[2].symbol.as_deref(), Some("main_jump"));

    // Symbol-driven control flow: resolve + the existing typed APIs.
    let target = e.resolve_symbol("main_jump").unwrap();
    assert!(e.run_until_pc(target, 10).unwrap());

    // A watchpoint at a named WRAM address reports the write.
    let wp_addr = e.resolve_symbol("monster_x").unwrap();
    e.bp_add_mem(wp_addr, wp_addr, false, true, true, None)
        .unwrap();
    let out = e.run_until_break(20).unwrap();
    assert_eq!(out.hit.unwrap().addr, Some(0x00_0200));

    e.clear_symbols();
    assert_eq!(e.resolve_symbol("main"), None);
    let lines = e.disassemble_cpu(0x00_0100, 1, true, true).unwrap();
    assert!(lines[0].symbol.is_none());
}

/// Breakpoint registry (issue #66): exec breakpoints halt-at-speed
/// with resume semantics, watchpoints report the exact accessing
/// instruction, and the registry lifecycle works end-to-end.
#[test]
fn breakpoint_registry_halts_at_speed() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // Inject a tiny loop at $00:0100 (WRAM mirror):
    //   0100: A9 42     LDA #$42
    //   0102: 8D 00 02  STA $0200
    //   0105: 4C 00 01  JMP $0100
    e.poke_memory(
        0x7E,
        0x0100,
        &[0xA9, 0x42, 0x8D, 0x00, 0x02, 0x4C, 0x00, 0x01],
    )
    .unwrap();
    e.set_cpu_register("pb", 0x00).unwrap();
    e.set_cpu_register("pc", 0x0100).unwrap();
    e.set_cpu_register("db", 0x00).unwrap();

    // No registry installed: the run completes its budget, hit = None,
    // and the instructions count into the cumulative stats (same
    // service level as `step` — the GUI loop depends on this).
    let before = e.instructions_executed();
    let out = e.run_until_break(6).unwrap();
    assert_eq!((out.steps, out.hit.is_none()), (6, true));
    assert_eq!(e.instructions_executed(), before + 6);

    // --- exec breakpoint: halts BEFORE the instruction at the target.
    e.set_cpu_register("pc", 0x0100).unwrap();
    let bp_jmp = e.bp_add_exec(0x00_0105, None).unwrap();
    let out = e.run_until_break(100).unwrap();
    let hit = out.hit.expect("exec bp hit");
    assert_eq!(hit.kind, BreakKind::Exec);
    assert_eq!((hit.id, hit.pc), (bp_jmp, 0x00_0105));
    assert_eq!(out.steps, 2, "LDA + STA executed, JMP not yet");
    assert_eq!(e.cpu_state().unwrap().pc, 0x0105);

    // Resume semantics: calling again moves PAST the breakpoint and
    // comes back around the loop to hit it again.
    let out = e.run_until_break(100).unwrap();
    assert_eq!(out.hit.unwrap().pc, 0x00_0105);
    assert_eq!(out.steps, 3, "JMP + LDA + STA this time");

    // --- memory watchpoint: exact accessing instruction reported.
    assert!(e.bp_remove(bp_jmp).unwrap());
    let bp_w = e
        .bp_add_mem(0x00_0200, 0x00_0200, false, true, true, None)
        .unwrap();
    e.set_cpu_register("pc", 0x0100).unwrap();
    let out = e.run_until_break(100).unwrap();
    let hit = out.hit.expect("watchpoint hit");
    assert_eq!(hit.kind, BreakKind::Write);
    assert_eq!(hit.id, bp_w);
    assert_eq!(hit.addr, Some(0x00_0200));
    assert_eq!(hit.value, Some(0x42));
    assert_eq!(hit.pc, 0x00_0102, "the STA instruction did the write");
    assert_eq!(out.steps, 2, "halts after the accessing instruction");

    // --- registry lifecycle + argument validation.
    assert_eq!(e.bp_list().unwrap().len(), 1);
    assert!(
        e.bp_add_mem(0x10, 0x00, true, true, true, None).is_err(),
        "lo > hi"
    );
    assert!(
        e.bp_add_mem(0x00, 0x10, false, false, true, None).is_err(),
        "neither read nor write"
    );
    e.bp_clear().unwrap();
    assert!(e.bp_list().unwrap().is_empty());
    // Cleared registry: the loop runs to its budget again.
    let out = e.run_until_break(10).unwrap();
    assert!(out.hit.is_none());

    // A hit surfaces on the Event Viewer as a MarkedBreakpoint event
    // (issue #68) — visible immediately while paused at the halt.
    e.set_cpu_register("pc", 0x0100).unwrap();
    e.bp_add_exec(0x00_0105, None).unwrap();
    let out = e.run_until_break(100).unwrap();
    assert!(out.hit.is_some());
    assert!(
        e.event_snapshot().iter().any(|ev| ev.category
            == event_viewer::EventCategory::MarkedBreakpoint
            && ev.pc == 0x00_0105),
        "the hit is injected into the Event Viewer buffer"
    );
}

#[test]
fn mem_trace_offset_filter_and_frame_blank_columns() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    // L14: capture only the stack page ($0100-$01FF). The boot BRK loop
    // pushes to the stack, so this catches real accesses while skipping
    // the ROM code fetches.
    e.enable_mem_trace(10_000, None, Some((0x0100, 0x01FF)))
        .unwrap();
    e.step(4_000).unwrap();
    let evs = e.take_mem_trace_log().unwrap();
    assert!(
        !evs.is_empty(),
        "offset filter should still capture stack accesses"
    );
    for ev in &evs {
        // L14: every captured offset is inside the filter window.
        let off = (ev.addr_full & 0xFFFF) as u16;
        assert!(
            (0x0100..=0x01FF).contains(&off),
            "offset {off:#06X} escaped the filter"
        );
        // L13: the per-access line is a valid scanline and `blank` agrees
        // with it (NTSC vblank starts at line 225).
        assert!(ev.line < 262, "scanline {} out of range", ev.line);
        assert_eq!(ev.blank, ev.line >= 225, "blank flag must track vblank");
    }
}

#[test]
fn fresh_emulator_has_no_rom() {
    let e = Emulator::new();
    assert!(!e.has_rom());
}

#[test]
fn save_state_requires_a_rom() {
    let e = Emulator::new();
    assert!(matches!(e.save_state(), Err(ApiError::NoRom)));
    let mut e2 = Emulator::new();
    assert!(matches!(e2.load_state(&[]), Err(ApiError::NoRom)));
}

#[test]
fn load_state_rejects_a_wrong_rom() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.step_until_frame(200_000).unwrap();
    let saved = e.save_state().expect("save");

    // Fresh emulator with the SAME ROM accepts it.
    let mut ok = Emulator::new();
    ok.load_rom_bytes(demo_lorom()).unwrap();
    assert!(ok.load_state(&saved).is_ok());

    // A garbage blob is refused, not panicked on.
    assert!(matches!(
        ok.load_state(&[1, 2, 3, 4]),
        Err(ApiError::SaveState(_))
    ));
}

#[test]
fn save_state_round_trip_rewinds_and_stays_deterministic() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();

    // Run a few frames, then snapshot.
    for _ in 0..3 {
        e.step_until_frame(1_000_000).unwrap();
    }
    let saved = e.save_state().expect("save");
    let hash_at_save = e.framebuffer_hash().expect("hash");
    let cpu_at_save = e.cpu_state().expect("cpu");

    // Run further so the machine has visibly advanced past the save.
    for _ in 0..3 {
        e.step_until_frame(1_000_000).unwrap();
    }
    let hash_after_more = e.framebuffer_hash().expect("hash");
    let cpu_after_more = e.cpu_state().expect("cpu");

    // Loading the state rewinds to exactly the save point.
    e.load_state(&saved).expect("load");
    assert_eq!(
        e.framebuffer_hash().expect("hash"),
        hash_at_save,
        "loading a state must restore the framebuffer hash captured at save time"
    );
    assert_eq!(
        e.cpu_state().expect("cpu").pc,
        cpu_at_save.pc,
        "loading a state must restore the CPU PC captured at save time"
    );

    // Determinism: re-running the same number of frames from the restored
    // point reproduces the post-save run bit-for-bit.
    for _ in 0..3 {
        e.step_until_frame(1_000_000).unwrap();
    }
    assert_eq!(
        e.framebuffer_hash().expect("hash"),
        hash_after_more,
        "replaying from a restored state must reproduce the same frame"
    );
    assert_eq!(e.cpu_state().expect("cpu").pc, cpu_after_more.pc);
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
fn export_spc_has_valid_header_and_embeds_aram_dsp_iplrom() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.step(2000).expect("step");
    let spc = e.export_spc().expect("export_spc");

    // Exact v0.30 file size and signature.
    assert_eq!(spc.len(), 0x1_0200);
    assert_eq!(&spc[0x00..0x21], b"SNES-SPC700 Sound File Data v0.30");
    assert_eq!(&spc[0x21..0x23], &[0x1A, 0x1A]);
    assert_eq!(spc[0x23], 0x1A, "ID666 tag present");

    // SPC700 register block matches the live state.
    let s = e.spc700_state().expect("spc700");
    assert_eq!(u16::from_le_bytes([spc[0x25], spc[0x26]]), s.pc);
    assert_eq!(spc[0x27], s.a);
    assert_eq!(spc[0x2B], s.sp);

    // Game-title ID666 field carries the cartridge title.
    assert!(spc[0x4E..0x6E].starts_with(b"LUNA API TEST DEMO"));

    // Payload regions match the dedicated accessors / the IPL ROM.
    assert_eq!(&spc[0x100..0x1_0100], e.aram_bytes().unwrap().as_slice());
    assert_eq!(&spc[0x1_01C0..0x1_0200], &luna_cpu_spc700::IPL_ROM);
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
fn peek_memory_checked_counts_unmapped_bytes() {
    let mut e = Emulator::new();
    assert!(matches!(
        e.peek_memory_checked(0x00, 0x8000, 1),
        Err(ApiError::NoRom)
    ));
    e.load_rom_bytes(demo_lorom()).unwrap();
    // ROM and WRAM: everything mapped.
    let rom = e.peek_memory_checked(0x00, 0xFFFC, 2).unwrap();
    assert_eq!(rom.bytes, vec![0x00, 0x80]);
    assert_eq!(rom.unmapped, 0);
    assert_eq!(e.peek_memory_checked(0x7E, 0x0000, 4).unwrap().unmapped, 0);
    // The LoROM lower half of a bank ≥ $40 is open bus: `$FF` bytes AND
    // the count that tells them apart from a ROM holding `$FF`.
    let hole = e.peek_memory_checked(0x40, 0x0000, 4).unwrap();
    assert_eq!(hole.bytes, vec![0xFF; 4]);
    assert_eq!(hole.unmapped, 4);
}

#[test]
fn state_json_schema_names_the_top_level_blocks() {
    let schema: serde_json::Value = serde_json::from_str(&state_json_schema()).expect("valid JSON");
    let props = schema["properties"].as_object().expect("object schema");
    for key in ["rom", "cpu", "ppu", "dma", "scheduler", "stats"] {
        assert!(props.contains_key(key), "schema lacks `{key}`");
    }
}

#[test]
fn render_frame_png_round_trips_via_image_crate() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    let png = e.render_frame_png(false).expect("png");
    assert!(png.starts_with(b"\x89PNG"), "header should be PNG magic");
}

#[test]
fn framebuffer_hash_is_deterministic_and_matches_rgba_pixels() {
    let mut e = Emulator::new();
    assert!(matches!(e.framebuffer_hash(), Err(ApiError::NoRom)));
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.step_until_frame(1_000_000).unwrap();
    let h1 = e.framebuffer_hash().expect("hash");
    // Pure function of state: identical when nothing changed.
    assert_eq!(h1, e.framebuffer_hash().unwrap(), "hash must be stable");
    // It hashes the same displayed pixels render_frame_rgba emits: an
    // independent FNV-1a of the RGB channels of that buffer agrees.
    let rgba = e.render_frame_rgba(false).unwrap();
    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|c| [c[0], c[1], c[2]])
        .collect();
    assert_eq!(h1, fnv1a_64(&rgb), "hashes the displayed RGB");
}

#[test]
fn hostile_tool_arguments_do_not_panic() {
    // Both of these used to abort the request handler outright: the
    // palette size overflowed its byte count to zero and then indexed
    // an empty buffer, and a search pattern longer than WRAM indexed
    // past the end of the slice.
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).expect("load");
    let png = e.render_palette_png(4096).expect("huge cell is clamped");
    assert!(!png.is_empty());
    let huge = vec![0u8; 0x2_0001];
    assert!(
        e.search_memory(&huge).expect("no panic").is_empty(),
        "a pattern larger than WRAM matches nothing"
    );
}

#[test]
fn a_raised_interrupt_stops_a_long_step() {
    use std::sync::atomic::AtomicBool;
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).expect("load");
    let flag = AtomicBool::new(true); // already raised, as a queued pause is
    let executed = e.step_interruptible(50_000_000, &flag).expect("step");
    assert!(
        executed < 50_000_000,
        "a pause raised before the call must still stop it (ran {executed})"
    );
}

#[test]
fn frame_hash_is_deterministic_and_matches_render_frame_rgba() {
    let mut e = Emulator::new();
    assert!(matches!(e.frame_hash(false), Err(ApiError::NoRom)));
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.step_until_frame(1_000_000).unwrap();
    let h = e.frame_hash(false).expect("hash");
    // Pure function of state — stable across calls (the property a
    // cross-arch baseline relies on).
    assert_eq!(h, e.frame_hash(false).unwrap(), "frame_hash must be stable");
    // It is exactly the pinned FNV-1a of the displayed RGBA bytes.
    let rgba = e.render_frame_rgba(false).unwrap();
    assert_eq!(h, fnv1a_64(&rgba));
}

/// fbhash v2 is a pinned function: these values must never change, or
/// every committed manifest baseline silently breaks. (The empty input
/// is the FNV offset basis; the others were computed once by hand.)
#[test]
fn fbhash_v2_is_fnv1a_with_pinned_values() {
    assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a_64(b"luna"), fnv1a_64(b"luna"));
    // A black 256x224 RGBA frame, the fbhash of a forced-blank capture.
    let black = vec![0u8; 256 * 224 * 4];
    let mut expect: u64 = 0xcbf2_9ce4_8422_2325;
    for _ in 0..black.len() {
        expect = expect.wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(fnv1a_64(&black), expect);
}

#[test]
fn input_capture_records_only_changes_keyed_by_frame() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.step_until_frame(1_000_000).unwrap();
    let f0 = e.frame_count().unwrap();

    e.start_input_capture();
    assert!(e.is_capturing_input());

    // Idle baseline (mask 0) at f0 adds nothing; press Start (bit 12)…
    e.set_joypad(0, 0x1000).unwrap();
    // …the same mask again is de-duped (a held button = one entry).
    e.set_joypad(0, 0x1000).unwrap();

    e.step_until_frame(1_000_000).unwrap();
    let f1 = e.frame_count().unwrap();
    e.set_joypad(0, 0).unwrap(); // release P1
    e.set_joypad(1, 0x8000).unwrap(); // P2 presses B

    let entries = e.take_input_capture();
    assert!(!e.is_capturing_input(), "take stops the capture");
    assert_eq!(
        entries,
        vec![
            InputCaptureEntry {
                frame: f0,
                port: 0,
                mask: 0x1000
            },
            InputCaptureEntry {
                frame: f1,
                port: 0,
                mask: 0
            },
            InputCaptureEntry {
                frame: f1,
                port: 1,
                mask: 0x8000
            },
        ]
    );
    // Per-port scripts round-trip into the `--input` replay format.
    assert_eq!(
        input_capture_to_script(&entries, 0),
        format!("{f0}:0x1000,{f1}:0x0000")
    );
    assert_eq!(input_capture_to_script(&entries, 1), format!("{f1}:0x8000"));
}

#[test]
fn input_capture_baseline_captures_held_button_at_start() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.step_until_frame(1_000_000).unwrap();
    let f0 = e.frame_count().unwrap();
    // Hold A *before* recording — the baseline must still capture it.
    e.set_joypad(0, 0x0080).unwrap();
    e.start_input_capture();
    assert_eq!(
        e.take_input_capture(),
        vec![InputCaptureEntry {
            frame: f0,
            port: 0,
            mask: 0x0080
        }]
    );
}

#[test]
fn reset_clears_in_progress_input_capture() {
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.start_input_capture();
    e.set_joypad(0, 0x1000).unwrap();
    assert!(e.is_capturing_input());
    e.reset().unwrap();
    assert!(!e.is_capturing_input(), "reset drops the capture");
    assert!(e.take_input_capture().is_empty());
}

#[test]
fn a_sym_next_to_the_rom_loads_for_every_front_end() {
    // Issue #67's sidecar used to be a CLI-only nicety: the GUI and the
    // MCP `load_rom` never saw it. It is the API's now.
    let dir = std::env::temp_dir().join(format!("luna_sidecar_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rom = dir.join("game.sfc");
    std::fs::write(&rom, demo_lorom()).unwrap();
    std::fs::write(dir.join("game.sym"), "[labels]\n00:8000 main\n").unwrap();

    let mut e = Emulator::new();
    let info = e.load_rom(&rom).unwrap();
    assert_eq!(info.symbols_loaded, Some(1));
    assert_eq!(e.resolve_symbol("main"), Some(0x00_8000));
    let info = e.load_rom_forced(&rom, MapperKind::LoRom).unwrap();
    assert_eq!(info.symbols_loaded, Some(1));

    // A broken sidecar never blocks the ROM.
    std::fs::write(dir.join("game.sym"), "[labels]\nnot a label line\n").unwrap();
    let info = e.load_rom(&rom).unwrap();
    assert!(info.symbols_loaded.is_some() || info.symbols_error.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_rom_forced_from_path_bypasses_autodetect() {
    // Write a ROM to disk and load it with a forced mapper (issue #88) —
    // the path the GUI's Force-mapper menu uses. Proves read + forced
    // parse + core build end-to-end.
    let dir = std::env::temp_dir();
    let path = dir.join("luna_test_forced_load.sfc");
    std::fs::write(&path, demo_lorom()).unwrap();
    let mut e = Emulator::new();
    let info = e
        .load_rom_forced(&path, MapperKind::LoRom)
        .expect("forced LoROM load");
    let _ = std::fs::remove_file(&path);
    assert!(e.has_rom());
    assert_eq!(info.mapper, "LoRom");
    // The forced-loaded core actually runs.
    e.step_until_frame(1_000_000).unwrap();
    assert!(e.frame_count().unwrap() >= 1);
}

#[test]
fn run_until_break_interruptible_stops_on_pause_flag() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut e = Emulator::new();
    e.load_rom_bytes(demo_lorom()).unwrap();
    let flag = AtomicBool::new(false);
    // The run holds `e` on this thread; a scoped thread raises the pause
    // flag mid-run (the real front-end model: pause off the emulator lock).
    let out = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            flag.store(true, Ordering::Relaxed);
        });
        e.run_until_break_interruptible(1_000_000_000, &flag)
            .unwrap()
    });
    assert!(out.interrupted, "the pause flag ended the run");
    assert!(out.hit.is_none(), "paused, not a breakpoint hit");
    assert!(
        out.steps > 0 && out.steps < 1_000_000_000,
        "stopped mid-run"
    );
}

#[test]
fn peek_oam_matches_state_oam_full() {
    let mut e = Emulator::new();
    assert!(matches!(e.peek_oam(), Err(ApiError::NoRom)));
    e.load_rom_bytes(demo_lorom()).unwrap();
    e.step_until_frame(1_000_000).unwrap();
    let oam = e.peek_oam().unwrap();
    assert_eq!(oam.len(), 0x220, "544 OAM bytes");
    assert_eq!(oam, e.state().ppu.oam_full, "peek_oam == state oam_full");
}
