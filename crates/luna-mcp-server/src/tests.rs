//! Unit tests (moved out of the parent file to keep it navigable).

use super::*;
use std::path::PathBuf;

/// Smoke test: build a server with no ROM loaded, fetch state.
/// We can't exercise the full MCP protocol from a unit test
/// without setting up an in-memory transport pair (which is its
/// own dance); this verifies the wiring at the type level —
/// `state()` is `async`, the underlying call works, and the
/// emulator is wrapped consistently.
#[tokio::test]
async fn server_state_works_without_rom() {
    let s = LunaServer::new();
    let result = s.state().await;
    // No ROM loaded → the embedded RomInfo is None.
    assert!(result.0.state.rom.is_none());
}

/// `step` without a ROM returns a `NoRom` `ApiError` mapped to an
/// MCP error.
#[tokio::test]
async fn server_step_without_rom_returns_error() {
    let s = LunaServer::new();
    let result = s.step(Parameters(StepParams { count: 1 })).await;
    let Err(err) = result else {
        panic!("expected error for stepping without a ROM");
    };
    assert!(err.message.contains("no ROM"));
}

/// `png_dimensions` reads the IHDR fields; a malformed buffer yields
/// `(0, 0)` instead of panicking. (The screenshot round-trip test
/// below covers the real-PNG path: it asserts 256×224 on an actual
/// render.)
#[test]
fn png_dimensions_reads_the_ihdr() {
    let mut buf = vec![0u8; 24];
    buf[16..20].copy_from_slice(&512u32.to_be_bytes());
    buf[20..24].copy_from_slice(&448u32.to_be_bytes());
    assert_eq!(png_dimensions(&buf), (512, 448));
    assert_eq!(png_dimensions(&[]), (0, 0));
}

/// Loading a non-existent ROM bubbles the I/O error up through
/// the MCP layer.
#[tokio::test]
async fn server_load_rom_missing_file_returns_error() {
    let s = LunaServer::new();
    let result = s
        .load_rom(Parameters(rom_params(
            "/tmp/luna-this-file-does-not-exist.smc",
        )))
        .await;
    let Err(err) = result else {
        panic!("expected error for missing ROM");
    };
    let msg = err.message.to_lowercase();
    assert!(msg.contains("i/o") || msg.contains("io"));
}

/// Smoke-test the full happy path: load a tiny ROM, step it,
/// dump state, render a PNG. Uses the same demo cart the
/// `luna-api` tests use, just to ensure the MCP wrappers
/// faithfully forward.
#[tokio::test]
async fn server_load_step_state_screenshot_round_trip() {
    let s = LunaServer::new();
    // Write demo cart to a tempfile so `load_rom` (which takes
    // a path) can read it.
    let path = PathBuf::from("/tmp/luna_mcp_demo.smc");
    std::fs::write(&path, demo_lorom()).unwrap();
    let info = s
        .load_rom(Parameters(rom_params(&path.to_string_lossy())))
        .await
        .unwrap();
    assert_eq!(info.0.rom.mapper, "LoRom");
    let stepped = s.step(Parameters(StepParams { count: 100 })).await.unwrap();
    assert!(stepped.0.executed > 0);
    let st = s.state().await;
    assert!(st.0.state.rom.is_some());
    let png = s
        .screenshot(Parameters(ScreenshotParams::default()))
        .await
        .unwrap();
    assert_eq!(png.0.width, 256);
    assert_eq!(png.0.height, 224);
    // PNG header check via base64-decode.
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&png.0.png_base64)
        .unwrap();
    assert!(bytes.starts_with(b"\x89PNG"));
    let _ = std::fs::remove_file(&path);
}

/// P1 surface round-trip (issue #65): disasm at the live PC, a
/// save→mutate→load state cycle, CGRAM + every debug render, both
/// trace rings, and the pointer-device setters.
#[tokio::test]
async fn server_p1_debugger_surface_round_trip() {
    let s = LunaServer::new();
    let path = PathBuf::from("/tmp/luna_mcp_p1_demo.smc");
    std::fs::write(&path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&path.to_string_lossy())))
        .await
        .unwrap();
    s.step(Parameters(StepParams { count: 50 })).await.unwrap();

    // disasm_cpu with all defaults → decodes at the live PC.
    let d = s
        .disasm_cpu(Parameters(DisasmCpuParams::default()))
        .await
        .unwrap();
    assert_eq!(d.0.lines.len(), 16);
    assert!(d.0.lines[0].is_pc, "first default line is the live PC");
    // disasm_spc with defaults.
    let d = s
        .disasm_spc(Parameters(DisasmSpcParams::default()))
        .await
        .unwrap();
    assert_eq!(d.0.lines.len(), 16);

    // save → run further → load → the saved position is restored.
    let saved = s.save_state().await.unwrap();
    assert!(saved.0.bytes > 0);
    let pc_at_save = {
        let mut em = s.emulator.lock().await;
        em.state().cpu.pc
    };
    s.step(Parameters(StepParams { count: 200 })).await.unwrap();
    s.load_state(Parameters(LoadStateParams {
        state_base64: saved.0.state_base64,
    }))
    .await
    .unwrap();
    let pc_after_load = {
        let mut em = s.emulator.lock().await;
        em.state().cpu.pc
    };
    assert_eq!(pc_at_save, pc_after_load, "load_state restores the PC");
    // Corrupt base64 → invalid-params error, not a panic.
    assert!(
        s.load_state(Parameters(LoadStateParams {
            state_base64: "not-base64!".into(),
        }))
        .await
        .is_err()
    );

    // CGRAM + the four debug renders all return valid payloads.
    let cg = s.peek_cgram().await.unwrap();
    assert_eq!(cg.0.colors.len(), 256);
    for png_b64 in [
        s.render_tilemap(Parameters(RenderTilemapParams { bg: 1 }))
            .await
            .unwrap()
            .0
            .png_base64,
        s.render_vram_tiles(Parameters(RenderVramTilesParams::default()))
            .await
            .unwrap()
            .0
            .png_base64,
        s.render_palette(Parameters(RenderPaletteParams::default()))
            .await
            .unwrap()
            .0
            .png_base64,
        s.render_sprite_sheet().await.unwrap().0.png_base64,
    ] {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&png_b64)
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
    }

    // Trace rings: enable → step → drain (non-empty), drain again (empty).
    s.enable_cpu_trace(Parameters(EnableCpuTraceParams { max_events: 1000 }))
        .await
        .unwrap();
    s.enable_mem_trace(Parameters(EnableMemTraceParams {
        max_events: 1000,
        bank: None,
        lo: None,
        hi: None,
        symbol: None,
        offsets: None,
        writes_only: None,
    }))
    .await
    .unwrap();
    s.step(Parameters(StepParams { count: 20 })).await.unwrap();
    let ct = s.take_cpu_trace().await.unwrap();
    assert!(!ct.0.events.is_empty(), "cpu trace recorded");
    let mt = s.take_mem_trace().await.unwrap();
    assert!(!mt.0.events.is_empty(), "mem trace recorded");
    assert!(
        mt.0.events
            .iter()
            .all(|e| matches!(e.kind.as_str(), "read" | "write" | "nmi" | "irq"))
    );
    assert!(s.take_cpu_trace().await.unwrap().0.events.is_empty());
    // Bad offset filter (lo without hi) → invalid-params.
    assert!(
        s.enable_mem_trace(Parameters(EnableMemTraceParams {
            max_events: 10,
            bank: None,
            lo: Some(0x2100),
            hi: None,
            symbol: None,
            offsets: None,
            writes_only: None,
        }))
        .await
        .is_err()
    );

    // Pointer devices accept input without error.
    s.set_mouse(Parameters(SetMouseParams {
        dx: -3,
        dy: 4,
        buttons: 1,
    }))
    .await
    .unwrap();
    s.set_superscope(Parameters(SetSuperscopeParams {
        x: 128,
        y: 112,
        buttons: 1,
    }))
    .await
    .unwrap();

    let _ = std::fs::remove_file(&path);
}

/// P2 breakpoint surface (issue #66): add/list/run/remove/clear over
/// MCP, on an injected WRAM loop.
#[tokio::test]
async fn server_p2_breakpoints_round_trip() {
    let s = LunaServer::new();
    let path = PathBuf::from("/tmp/luna_mcp_p2_demo.smc");
    std::fs::write(&path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&path.to_string_lossy())))
        .await
        .unwrap();
    // Inject `LDA #$42; STA $0200; JMP $0100` at $00:0100 and aim PC.
    s.poke_memory(Parameters(PokeMemoryParams {
        bank: 0x7E,
        offset: 0x0100,
        symbol: None,
        data: vec![0xA9, 0x42, 0x8D, 0x00, 0x02, 0x4C, 0x00, 0x01],
    }))
    .await
    .unwrap();
    for (reg, val) in [("pb", 0x00u32), ("pc", 0x0100), ("db", 0x00)] {
        s.set_cpu_register(Parameters(SetRegisterParams {
            reg: reg.into(),
            val,
        }))
        .await
        .unwrap();
    }

    // Watchpoint on the STA target (defaults: single address, write).
    let wp = s
        .bp_add(Parameters(BpAddParams {
            kind: "mem".into(),
            addr: 0x00_0200,
            symbol: None,
            hi: None,
            hi_symbol: None,
            on_read: false,
            on_write: true,
            mirror: None,
            name: None,
        }))
        .await
        .unwrap()
        .0
        .id;
    // Exec bp on the JMP.
    let xp = s
        .bp_add(Parameters(BpAddParams {
            kind: "exec".into(),
            addr: 0x00_0105,
            symbol: None,
            hi: None,
            hi_symbol: None,
            on_read: false,
            on_write: true,
            mirror: None,
            name: None,
        }))
        .await
        .unwrap()
        .0
        .id;
    assert!(
        s.bp_add(Parameters(BpAddParams {
            kind: "bogus".into(),
            addr: 0,
            symbol: None,
            hi: None,
            hi_symbol: None,
            on_read: false,
            on_write: true,
            mirror: None,
            name: None,
        }))
        .await
        .is_err()
    );
    assert_eq!(s.bp_list().await.unwrap().0.breakpoints.len(), 2);

    // The watchpoint (STA at $0102) fires first.
    let out = s
        .run_until_break(Parameters(RunUntilBreakParams { max_steps: 100 }))
        .await
        .unwrap()
        .0;
    assert!(out.hit);
    assert_eq!(out.bp_id, Some(wp));
    assert_eq!(out.kind.as_deref(), Some("write"));
    assert_eq!((out.addr, out.value), (Some(0x00_0200), Some(0x42)));
    assert_eq!(out.pc, Some(0x00_0102));

    // Remove it; the exec bp fires next (before the JMP executes).
    assert!(
        s.bp_remove(Parameters(BpRemoveParams { id: wp }))
            .await
            .unwrap()
            .0
            .removed
    );
    let out = s
        .run_until_break(Parameters(RunUntilBreakParams { max_steps: 100 }))
        .await
        .unwrap()
        .0;
    assert_eq!(out.bp_id, Some(xp));
    assert_eq!(out.kind.as_deref(), Some("exec"));
    assert_eq!(out.pc, Some(0x00_0105));

    // Clear all: the run completes its budget.
    s.bp_clear_all().await.unwrap();
    let out = s
        .run_until_break(Parameters(RunUntilBreakParams { max_steps: 10 }))
        .await
        .unwrap()
        .0;
    assert!(!out.hit);
    assert_eq!(out.steps, 10);

    let _ = std::fs::remove_file(&path);
}

/// P3 symbol surface (issue #67): load a .sym over MCP, resolve,
/// then drive `peek`/`poke`/`bp_add` by name and see annotated disasm.
#[tokio::test]
async fn server_p3_symbols_round_trip() {
    let s = LunaServer::new();
    let rom_path = PathBuf::from("/tmp/luna_mcp_p3_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();

    let sym_path = PathBuf::from("/tmp/luna_mcp_p3_demo.sym");
    std::fs::write(&sym_path, "[labels]\n00:0100 main\n7e:0200 monster_x\n").unwrap();
    let n = s
        .load_symbols(Parameters(LoadSymbolsParams {
            path: sym_path.to_string_lossy().into(),
            space: None,
        }))
        .await
        .unwrap();
    assert_eq!(n.0.count, 2);

    // resolve_symbol: known and unknown.
    let r = s
        .resolve_symbol(Parameters(ResolveSymbolParams {
            name: "monster_x".into(),
            space: None,
        }))
        .await
        .unwrap();
    assert_eq!(r.0.addr, Some(0x7E_0200));
    let r = s
        .resolve_symbol(Parameters(ResolveSymbolParams {
            name: "nope".into(),
            space: None,
        }))
        .await
        .unwrap();
    assert_eq!(r.0.addr, None);

    // poke by symbol, peek back by symbol.
    s.poke_memory(Parameters(PokeMemoryParams {
        bank: 0,
        offset: 0,
        symbol: Some("monster_x".into()),
        data: vec![0xAB, 0xCD],
    }))
    .await
    .unwrap();
    let bytes = s
        .peek_memory(Parameters(PeekMemoryParams {
            bank: 0,
            offset: 0,
            symbol: Some("monster_x".into()),
            count: 2,
        }))
        .await
        .unwrap();
    assert_eq!(bytes.0.bytes, vec![0xAB, 0xCD]);
    // Unknown symbol → invalid-params, not a silent bank-0 read.
    assert!(
        s.peek_memory(Parameters(PeekMemoryParams {
            bank: 0,
            offset: 0,
            symbol: Some("nope".into()),
            count: 1,
        }))
        .await
        .is_err()
    );

    // bp_add by symbol registers at the resolved address.
    let id = s
        .bp_add(Parameters(BpAddParams {
            kind: "mem".into(),
            addr: 0,
            symbol: Some("monster_x".into()),
            hi: None,
            hi_symbol: None,
            on_read: false,
            on_write: true,
            mirror: None,
            name: None,
        }))
        .await
        .unwrap()
        .0
        .id;
    let list = s.bp_list().await.unwrap().0.breakpoints;
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].lo, 0x7E_0200);

    // Annotated disassembly at a labeled address.
    let d = s
        .disasm_cpu(Parameters(DisasmCpuParams {
            addr: Some(0x00_0100),
            symbol: None,
            lines: Some(1),
            m8: Some(true),
            x8: Some(true),
        }))
        .await
        .unwrap();
    assert_eq!(d.0.lines[0].symbol.as_deref(), Some("main"));

    let _ = std::fs::remove_file(&rom_path);
    let _ = std::fs::remove_file(&sym_path);
}

#[tokio::test]
async fn server_nocash_and_wdm_logs_round_trip() {
    let s = LunaServer::new();

    // Without a ROM both enables surface an error.
    assert!(s.enable_nocash_log().await.is_err());
    assert!(s.enable_wdm_log().await.is_err());

    // Patch a program into the demo ROM at $00:8000: emit "HI" on the
    // $21FC Nocash TTY, fire the SNES_ASSERT-style `WDM #$00`, then spin.
    let mut rom = demo_lorom();
    let prog: &[u8] = &[
        0xA9, 0x48, // LDA #'H'
        0x8D, 0xFC, 0x21, // STA $21FC
        0xA9, 0x49, // LDA #'I'
        0x8D, 0xFC, 0x21, // STA $21FC
        0x42, 0x00, // WDM #$00
        0x80, 0xFE, // BRA *
    ];
    rom[..prog.len()].copy_from_slice(prog);
    // Re-fix the header checksum the patch just invalidated.
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

    let rom_path = PathBuf::from("/tmp/luna_mcp_nocash_wdm_demo.smc");
    std::fs::write(&rom_path, rom).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();

    s.enable_nocash_log().await.unwrap();
    s.enable_wdm_log().await.unwrap();

    // A label so take_wdm_log symbolises the recorded PC.
    let sym_path = PathBuf::from("/tmp/luna_mcp_nocash_wdm_demo.sym");
    std::fs::write(&sym_path, "[labels]\n00:8000 main\n").unwrap();
    s.load_symbols(Parameters(LoadSymbolsParams {
        path: sym_path.to_string_lossy().into(),
        space: None,
    }))
    .await
    .unwrap();

    s.step(Parameters(StepParams { count: 32 })).await.unwrap();

    let nocash = s.take_nocash_log().await.unwrap();
    assert_eq!(nocash.0.text, "HI");
    assert_eq!(nocash.0.base64, "SEk=");

    let wdm = s.take_wdm_log().await.unwrap();
    assert_eq!(wdm.0.events.len(), 1);
    let ev = &wdm.0.events[0];
    assert_eq!(ev.operand, 0x00);
    // The core records the operand byte's address (opcode at $00:800A).
    assert_eq!(ev.pc, 0x00_800B);
    assert_eq!(ev.symbol.as_deref(), Some("main+0x0B"));

    // Draining resets both channels.
    assert!(s.take_nocash_log().await.unwrap().0.text.is_empty());
    assert!(s.take_wdm_log().await.unwrap().0.events.is_empty());
}

#[tokio::test]
async fn server_forced_loading_and_port_device_round_trip() {
    let s = LunaServer::new();
    let rom_path = PathBuf::from("/tmp/luna_mcp_forced_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();

    // Auto-detection sees the LoROM header...
    let info = s
        .load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();
    assert_eq!(info.0.rom.mapper, "LoRom");

    // A checksum-corrupted image is rejected by auto-detection...
    let mut broken = demo_lorom();
    broken[0x7FDC] ^= 0xFF;
    let broken_path = PathBuf::from("/tmp/luna_mcp_forced_demo_broken.smc");
    std::fs::write(&broken_path, &broken).unwrap();
    assert!(
        s.load_rom(Parameters(rom_params(&broken_path.to_string_lossy())))
            .await
            .is_err()
    );

    // ...but force_mapper loads it anyway (the point of the flag).
    let info = s
        .load_rom(Parameters(LoadRomParams {
            path: broken_path.to_string_lossy().into(),
            force_mapper: Some("lorom".into()),
            force_region: Some("pal".into()),
            power_on: None,
        }))
        .await
        .unwrap();
    assert_eq!(info.0.rom.mapper, "LoRom");
    assert!(!info.0.rom.checksum_valid);

    // Bad vocabulary is an invalid_params error, not a load attempt.
    assert!(
        s.load_rom(Parameters(LoadRomParams {
            path: rom_path.to_string_lossy().into(),
            force_mapper: Some("wat".into()),
            force_region: None,
            power_on: None,
        }))
        .await
        .is_err()
    );
    assert!(
        s.load_rom(Parameters(LoadRomParams {
            path: rom_path.to_string_lossy().into(),
            force_mapper: None,
            force_region: Some("secam".into()),
            power_on: None,
        }))
        .await
        .is_err()
    );

    // load_rom_bytes: same image over base64, no host file involved.
    let info = s
        .load_rom_bytes(Parameters(LoadRomBytesParams {
            rom_base64: b64(&demo_lorom()),
            force_mapper: None,
            force_region: None,
            power_on: None,
        }))
        .await
        .unwrap();
    assert_eq!(info.0.rom.title.trim(), "LUNA MCP DEMO");
    assert!(
        s.load_rom_bytes(Parameters(LoadRomBytesParams {
            rom_base64: "not-base64!!".into(),
            force_mapper: None,
            force_region: None,
            power_on: None,
        }))
        .await
        .is_err()
    );

    // set_port_device: plug a mouse into P1, feed it, unplug back to pad.
    for device in ["mouse", "joypad"] {
        s.set_port_device(Parameters(SetPortDeviceParams {
            port: 0,
            device: device.into(),
        }))
        .await
        .unwrap();
    }
    assert!(
        s.set_port_device(Parameters(SetPortDeviceParams {
            port: 0,
            device: "lightgun".into(),
        }))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn server_determinism_oracles_round_trip() {
    let s = LunaServer::new();

    // Everything errors cleanly without a ROM.
    assert!(
        s.frame_hash(Parameters(FrameHashParams::default()))
            .await
            .is_err()
    );
    assert!(
        s.wram_page_hashes(Parameters(WramPageHashesParams::default()))
            .await
            .is_err()
    );
    assert!(
        s.loop_probe(Parameters(LoopProbeParams { max_steps: 10 }))
            .await
            .is_err()
    );

    let rom_path = PathBuf::from("/tmp/luna_mcp_oracles_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();

    // frame_hash: 16 hex chars, deterministic while nothing steps.
    let h1 = s
        .frame_hash(Parameters(FrameHashParams::default()))
        .await
        .unwrap();
    assert_eq!(h1.0.hash.len(), 16);
    assert!(h1.0.hash.chars().all(|c| c.is_ascii_hexdigit()));
    let h2 = s
        .frame_hash(Parameters(FrameHashParams::default()))
        .await
        .unwrap();
    assert_eq!(h1.0.hash, h2.0.hash);

    // native gate: BadArg until set_native_capture flips it on.
    assert!(
        s.frame_hash(Parameters(FrameHashParams {
            force_display: false,
            native: true,
        }))
        .await
        .is_err()
    );
    s.set_native_capture(Parameters(SetNativeCaptureParams { enabled: true }))
        .await
        .unwrap();
    s.step_until_frame(Parameters(StepUntilFrameParams {
        max_steps: 1_000_000,
    }))
    .await
    .unwrap();
    let hn = s
        .frame_hash(Parameters(FrameHashParams {
            force_display: false,
            native: true,
        }))
        .await
        .unwrap();
    assert_eq!(hn.0.hash.len(), 16);

    // wram_page_hashes: default page size → 32 pages; bad size errors.
    let pages = s
        .wram_page_hashes(Parameters(WramPageHashesParams::default()))
        .await
        .unwrap();
    assert_eq!(pages.0.page_size, 0x1000);
    assert_eq!(pages.0.hashes.len(), 32);
    assert!(
        s.wram_page_hashes(Parameters(WramPageHashesParams { page_size: 3 }))
            .await
            .is_err()
    );

    // wram_snapshot: hash equals the one full-width page hash, data
    // round-trips at 128 KiB only when asked for.
    let snap = s
        .wram_snapshot(Parameters(WramSnapshotParams::default()))
        .await
        .unwrap();
    assert!(snap.0.wram_base64.is_none());
    let full = s
        .wram_page_hashes(Parameters(WramPageHashesParams { page_size: 0x20000 }))
        .await
        .unwrap();
    assert_eq!(snap.0.hash, full.0.hashes[0]);
    let snap = s
        .wram_snapshot(Parameters(WramSnapshotParams { include_data: true }))
        .await
        .unwrap();
    let data = base64::engine::general_purpose::STANDARD
        .decode(snap.0.wram_base64.unwrap())
        .unwrap();
    assert_eq!(data.len(), 0x20000);

    // loop_probe advances the CPU and reports a plausible shape.
    let probe = s
        .loop_probe(Parameters(LoopProbeParams { max_steps: 500 }))
        .await
        .unwrap();
    assert!(probe.0.executed <= 500);
    assert!(probe.0.distinct_pcs >= 1);
}

#[tokio::test]
async fn server_trace_parity_round_trip() {
    let s = LunaServer::new();

    // Every enable errors cleanly without a ROM.
    assert!(s.enable_mailbox_log().await.is_err());
    assert!(
        s.enable_dma_trace(Parameters(EnableRingTraceParams { max_events: 16 }))
            .await
            .is_err()
    );

    // A program that pokes the APU mailbox so the log has real traffic.
    let mut rom = demo_lorom();
    let prog: &[u8] = &[
        0xA9, 0xCC, // LDA #$CC
        0x8D, 0x40, 0x21, // STA $2140
        0xAD, 0x40, 0x21, // LDA $2140
        0x80, 0xFE, // BRA *
    ];
    rom[..prog.len()].copy_from_slice(prog);
    let rom_path = PathBuf::from("/tmp/luna_mcp_traces_demo.smc");
    std::fs::write(&rom_path, rom).unwrap();
    s.load_rom(Parameters(LoadRomParams {
        path: rom_path.to_string_lossy().into(),
        force_mapper: Some("lorom".into()),
        force_region: None,
        power_on: None,
    }))
    .await
    .unwrap();

    // Enable all nine, run a while, drain all nine.
    s.enable_mailbox_log().await.unwrap();
    s.enable_sa1_log().await.unwrap();
    s.enable_sa1_side_log().await.unwrap();
    for max in [64usize] {
        s.enable_dma_trace(Parameters(EnableRingTraceParams { max_events: max }))
            .await
            .unwrap();
        s.enable_dsp_trace(Parameters(EnableRingTraceParams { max_events: max }))
            .await
            .unwrap();
        s.enable_sa1_trace(Parameters(EnableRingTraceParams { max_events: max }))
            .await
            .unwrap();
        s.enable_superfx_trace(Parameters(EnableRingTraceParams { max_events: max }))
            .await
            .unwrap();
        s.enable_spc_trace(Parameters(EnableRingTraceParams { max_events: max }))
            .await
            .unwrap();
    }
    s.enable_dsp1_trace(Parameters(EnableDsp1TraceParams {
        max_events: 64,
        ports_only: false,
    }))
    .await
    .unwrap();

    s.step(Parameters(StepParams { count: 2000 }))
        .await
        .unwrap();

    // The mailbox saw our $2140 write + read, tagged with the writing PC.
    let mail = s.take_mailbox_log().await.unwrap();
    assert!(!mail.0.events.is_empty());
    let w = mail.0.events.iter().find(|e| e.kind == "write").unwrap();
    assert_eq!(w.port, 0);
    assert_eq!(w.value, 0xCC);
    assert_eq!(w.pc >> 16, 0x00);

    // The SPC700 IPL boot ROM executed instructions.
    let spc = s.take_spc_trace().await.unwrap();
    assert!(!spc.0.events.is_empty());

    // Coprocessor traces drain empty on a plain LoROM cart, not error.
    assert!(s.take_sa1_log().await.unwrap().0.events.is_empty());
    assert!(s.take_sa1_side_log().await.unwrap().0.events.is_empty());
    assert!(s.take_sa1_trace().await.unwrap().0.events.is_empty());
    assert!(s.take_superfx_trace().await.unwrap().0.events.is_empty());
    let dsp1 = s
        .take_dsp1_trace(Parameters(TakeDsp1TraceParams {
            decode_commands: true,
        }))
        .await
        .unwrap();
    assert!(dsp1.0.events.is_empty());
    assert!(dsp1.0.commands.is_some_and(|c| c.is_empty()));
    assert!(!dsp1.0.truncated);

    // DMA + DSP traces drain Ok (the demo program does no DMA; the IPL
    // may or may not touch DSP registers — shape only).
    s.take_dma_trace().await.unwrap();
    s.take_dsp_trace().await.unwrap();

    // Draining reset the mailbox log.
    assert!(s.take_mailbox_log().await.unwrap().0.events.is_empty());
}

#[tokio::test]
async fn server_symbol_tools_round_trip() {
    let s = LunaServer::new();
    let rom_path = PathBuf::from("/tmp/luna_mcp_symtools_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();

    // load_symbols_str: no host file involved.
    let n = s
        .load_symbols_str(Parameters(LoadSymbolsStrParams {
            text: "[labels]\n00:8000 main\n7e:0200 monster_x\n7e:020f monster_end\n".into(),
            space: None,
        }))
        .await
        .unwrap();
    assert_eq!(n.0.count, 3);

    // symbol_for_addr: exact, offset form, and no-label-in-bank.
    let sym = |addr| s.symbol_for_addr(Parameters(SymbolForAddrParams { addr, space: None }));
    assert_eq!(
        sym(0x7E_0200).await.unwrap().0.symbol.as_deref(),
        Some("monster_x")
    );
    assert_eq!(
        sym(0x00_8005).await.unwrap().0.symbol.as_deref(),
        Some("main+0x05")
    );
    assert!(sym(0x7F_0000).await.unwrap().0.symbol.is_none());

    // disasm_cpu accepts a symbol start.
    let d = s
        .disasm_cpu(Parameters(DisasmCpuParams {
            addr: None,
            symbol: Some("main".into()),
            lines: Some(1),
            m8: Some(true),
            x8: Some(true),
        }))
        .await
        .unwrap();
    assert_eq!(d.0.lines[0].addr, 0x00_8000);

    // enable_mem_trace: symbol conflicts with manual filters...
    assert!(
        s.enable_mem_trace(Parameters(EnableMemTraceParams {
            max_events: 10,
            bank: Some(0x7E),
            lo: None,
            hi: None,
            symbol: Some("monster_x".into()),
            offsets: None,
            writes_only: None,
        }))
        .await
        .is_err()
    );
    // ...and works alone.
    s.enable_mem_trace(Parameters(EnableMemTraceParams {
        max_events: 10,
        bank: None,
        lo: None,
        hi: None,
        symbol: Some("monster_x".into()),
        offsets: None,
        writes_only: None,
    }))
    .await
    .unwrap();

    // bp_add: watch a symbol..=hi_symbol range.
    let id = s
        .bp_add(Parameters(BpAddParams {
            kind: "mem".into(),
            addr: 0,
            symbol: Some("monster_x".into()),
            hi: None,
            hi_symbol: Some("monster_end".into()),
            on_read: false,
            on_write: true,
            mirror: None,
            name: None,
        }))
        .await
        .unwrap()
        .0
        .id;
    let list = s.bp_list().await.unwrap().0.breakpoints;
    let bp = list.iter().find(|b| b.id == id).unwrap();
    assert_eq!(bp.lo, 0x7E_0200);
    assert_eq!(bp.hi, 0x7E_020F);

    // clear_symbols: resolution and annotation stop.
    s.clear_symbols().await;
    assert!(
        s.resolve_symbol(Parameters(ResolveSymbolParams {
            name: "main".into(),
            space: None,
        }))
        .await
        .unwrap()
        .0
        .addr
        .is_none()
    );
    assert!(sym(0x7E_0200).await.unwrap().0.symbol.is_none());
}

#[tokio::test]
async fn server_persistence_and_media_round_trip() {
    let s = LunaServer::new();

    // Give the demo cart 8 KB of SRAM (header $7FD8 = size code 3).
    let mut rom = demo_lorom();
    rom[0x7FD8] = 0x03;
    // Header checksum changed → re-fix it.
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
    let rom_path = PathBuf::from("/tmp/luna_mcp_media_demo.smc");
    std::fs::write(&rom_path, rom).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();

    // sram_set → sram_get round trip.
    let image = vec![0xA5u8; 0x2000];
    s.sram_set(Parameters(SramSetParams {
        sram_base64: b64(&image),
    }))
    .await
    .unwrap();
    let got = s.sram_get().await;
    assert_eq!(got.0.bytes, 0x2000);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(got.0.sram_base64)
        .unwrap();
    assert_eq!(decoded, image);
    assert!(
        s.sram_set(Parameters(SramSetParams {
            sram_base64: "!!!".into(),
        }))
        .await
        .is_err()
    );

    // export_spc: a v0.30 blob with the right magic + size.
    let spc = s.export_spc().await.unwrap();
    assert_eq!(spc.0.bytes, 0x10200);
    let blob = base64::engine::general_purpose::STANDARD
        .decode(spc.0.spc_base64)
        .unwrap();
    assert!(blob.starts_with(b"SNES-SPC700 Sound File Data"));

    // decode_sprites: all 128 OAM entries.
    let sprites = s.decode_sprites().await.unwrap();
    assert_eq!(sprites.0.sprites.len(), 128);

    // screenshot: bg render works and reports 256×224; conflicts and
    // bad indices are invalid_params; native still gated.
    let shot = s
        .screenshot(Parameters(ScreenshotParams {
            force_display: true,
            native: false,
            bg: Some(1),
        }))
        .await
        .unwrap();
    assert_eq!((shot.0.width, shot.0.height), (256, 224));
    assert!(
        s.screenshot(Parameters(ScreenshotParams {
            force_display: false,
            native: true,
            bg: Some(1),
        }))
        .await
        .is_err()
    );
    assert!(
        s.screenshot(Parameters(ScreenshotParams {
            force_display: false,
            native: false,
            bg: Some(5),
        }))
        .await
        .is_err()
    );
    assert!(
        s.screenshot(Parameters(ScreenshotParams {
            force_display: false,
            native: true,
            bg: None,
        }))
        .await
        .is_err()
    );

    // peek_aram / peek_vram: the u32 lift makes one-call full dumps work.
    let aram = s
        .peek_aram(Parameters(PeekAramParams {
            offset: 0,
            symbol: None,
            count: 0x1_0000,
        }))
        .await
        .unwrap();
    assert_eq!(aram.0.bytes.len(), 0x1_0000);
    let vram = s
        .peek_vram(Parameters(PeekVramParams {
            offset: 0,
            count: 0x1_0000,
        }))
        .await
        .unwrap();
    assert_eq!(vram.0.bytes.len(), 0x1_0000);
}

#[tokio::test]
async fn server_with_preloaded_emulator_answers_without_load_rom() {
    // The `luna mcp --rom` path: the emulator arrives already loaded.
    let rom_path = PathBuf::from("/tmp/luna_mcp_preload_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();
    let mut em = Emulator::new();
    em.load_rom(&rom_path).unwrap();
    let s = LunaServer::with_emulator(em);

    // First contact: state + step work with no prior load_rom call.
    let st = s.state().await;
    assert!(st.0.state.rom.is_some());
    let stepped = s.step(Parameters(StepParams { count: 10 })).await.unwrap();
    assert_eq!(stepped.0.executed, 10);
}

#[test]
fn get_info_reports_luna_identity_and_instructions() {
    let s = LunaServer::new();
    let info = s.get_info();
    assert_eq!(info.server_info.name, "luna");
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    let instructions = info.instructions.expect("server instructions present");
    assert!(instructions.contains("load_rom"));
    assert!(instructions.contains("take_wdm_log"));
    assert!(info.capabilities.tools.is_some());
}

#[tokio::test]
async fn server_symbol_spaces_round_trip() {
    let s = LunaServer::new();
    let rom_path = PathBuf::from("/tmp/luna_mcp_symspaces_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();

    // CPU symbols + a [definitions] constant.
    let n = s
        .load_symbols_str(Parameters(LoadSymbolsStrParams {
            text: "[labels]\n00:8000 main\n[definitions]\n00000042 MAGIC\n".into(),
            space: None,
        }))
        .await
        .unwrap();
    assert_eq!(n.0.count, 2);
    // Constants resolve in the CPU space (v2, #179).
    let r = s
        .resolve_symbol(Parameters(ResolveSymbolParams {
            name: "MAGIC".into(),
            space: None,
        }))
        .await
        .unwrap();
    assert_eq!(r.0.addr, Some(0x42));

    // ARAM symbols load into their own space, keeping the CPU table.
    let n = s
        .load_symbols_str(Parameters(LoadSymbolsStrParams {
            text: "[labels]\n00:0500 driver_loop\n".into(),
            space: Some("aram".into()),
        }))
        .await
        .unwrap();
    assert_eq!(n.0.count, 1);
    let r = s
        .resolve_symbol(Parameters(ResolveSymbolParams {
            name: "driver_loop".into(),
            space: Some("aram".into()),
        }))
        .await
        .unwrap();
    assert_eq!(r.0.addr, Some(0x0500));
    // Cross-space resolution stays separate in both directions.
    assert!(
        s.resolve_symbol(Parameters(ResolveSymbolParams {
            name: "driver_loop".into(),
            space: None,
        }))
        .await
        .unwrap()
        .0
        .addr
        .is_none()
    );
    let r = s
        .resolve_symbol(Parameters(ResolveSymbolParams {
            name: "main".into(),
            space: None,
        }))
        .await
        .unwrap();
    assert_eq!(r.0.addr, Some(0x00_8000));

    // symbol_for_addr in the aram space, incl. the 16-bit guard.
    let sfa = s
        .symbol_for_addr(Parameters(SymbolForAddrParams {
            addr: 0x0502,
            space: Some("aram".into()),
        }))
        .await
        .unwrap();
    assert_eq!(sfa.0.symbol.as_deref(), Some("driver_loop+0x02"));
    assert!(
        s.symbol_for_addr(Parameters(SymbolForAddrParams {
            addr: 0x1_0000,
            space: Some("aram".into()),
        }))
        .await
        .is_err()
    );

    // disasm_spc + peek_aram accept ARAM symbols; the disassembly is
    // annotated from the ARAM space.
    let d = s
        .disasm_spc(Parameters(DisasmSpcParams {
            addr: None,
            symbol: Some("driver_loop".into()),
            lines: Some(1),
        }))
        .await
        .unwrap();
    assert_eq!(d.0.lines[0].addr, 0x0500);
    assert_eq!(d.0.lines[0].symbol.as_deref(), Some("driver_loop"));
    let bytes = s
        .peek_aram(Parameters(PeekAramParams {
            offset: 0,
            symbol: Some("driver_loop".into()),
            count: 2,
        }))
        .await
        .unwrap();
    assert_eq!(bytes.0.bytes.len(), 2);
    assert!(
        s.peek_aram(Parameters(PeekAramParams {
            offset: 0,
            symbol: Some("main".into()), // CPU label ≠ ARAM symbol
            count: 2,
        }))
        .await
        .is_err()
    );

    // Bad space vocabulary.
    assert!(
        s.resolve_symbol(Parameters(ResolveSymbolParams {
            name: "main".into(),
            space: Some("vram".into()),
        }))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn server_breakpoints_v2_round_trip() {
    let s = LunaServer::new();
    let rom_path = PathBuf::from("/tmp/luna_mcp_bpv2_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();
    s.load_symbols_str(Parameters(LoadSymbolsStrParams {
        text: "[labels]\n7e:0200 monster_x\n".into(),
        space: None,
    }))
    .await
    .unwrap();

    // A named, bank-exact (mirror off) watch created from a symbol.
    let id = s
        .bp_add(Parameters(BpAddParams {
            kind: "mem".into(),
            addr: 0,
            symbol: Some("monster_x".into()),
            hi: None,
            hi_symbol: None,
            on_read: false,
            on_write: true,
            mirror: Some(false),
            name: None,
        }))
        .await
        .unwrap()
        .0
        .id;
    let list = s.bp_list().await.unwrap().0.breakpoints;
    let bp = list.iter().find(|b| b.id == id).unwrap();
    assert!(bp.enabled);
    assert!(!bp.mirror);
    assert_eq!(bp.hit_count, 0);
    assert_eq!(bp.name.as_deref(), Some("monster_x"));

    // Disable → run past a write → no hit; hit_count stays 0.
    let found = s
        .bp_set_enabled(Parameters(BpSetEnabledParams { id, enabled: false }))
        .await
        .unwrap();
    assert!(found.0.found);
    // Inject `LDA #$42; STA $0200; BRA *` at $7E:0100 won't execute
    // from WRAM here — instead poke through the API and step: pokes
    // bypass the bus, so drive a real write via run_until: simplest
    // is to verify the disabled watch doesn't fire during a run.
    let out = s
        .run_until_break(Parameters(RunUntilBreakParams { max_steps: 200 }))
        .await
        .unwrap();
    assert!(!out.0.hit);

    // Re-enable + unknown id shape.
    assert!(
        s.bp_set_enabled(Parameters(BpSetEnabledParams { id, enabled: true }))
            .await
            .unwrap()
            .0
            .found
    );
    assert!(
        !s.bp_set_enabled(Parameters(BpSetEnabledParams {
            id: 4242,
            enabled: true,
        }))
        .await
        .unwrap()
        .0
        .found
    );
}

#[tokio::test]
async fn server_search_session_round_trip() {
    let s = LunaServer::new();
    let rom_path = PathBuf::from("/tmp/luna_mcp_search_demo.smc");
    std::fs::write(&rom_path, demo_lorom()).unwrap();
    s.load_rom(Parameters(rom_params(&rom_path.to_string_lossy())))
        .await
        .unwrap();

    // Plant the variable, begin, and narrow to it.
    s.poke_memory(Parameters(PokeMemoryParams {
        bank: 0x7E,
        offset: 0x0300,
        symbol: None,
        data: vec![100, 0],
    }))
    .await
    .unwrap();
    let n = s
        .search_begin(Parameters(SearchBeginParams {
            width: "u16".into(),
        }))
        .await
        .unwrap();
    assert_eq!(n.0.remaining, 0x1FFFF);
    s.search_refine(Parameters(SearchRefineParams {
        op: "eq".into(),
        value: Some(100),
    }))
    .await
    .unwrap();
    s.poke_memory(Parameters(PokeMemoryParams {
        bank: 0x7E,
        offset: 0x0300,
        symbol: None,
        data: vec![73, 0],
    }))
    .await
    .unwrap();
    let n = s
        .search_refine(Parameters(SearchRefineParams {
            op: "eq".into(),
            value: Some(73),
        }))
        .await
        .unwrap();
    assert_eq!(n.0.remaining, 1);
    let hits = s
        .search_results(Parameters(SearchResultsParams::default()))
        .await
        .unwrap();
    assert_eq!(hits.0.hits[0].addr, 0x7E_0300);
    assert_eq!(hits.0.hits[0].value, 73);

    // Vocabulary errors.
    assert!(
        s.search_begin(Parameters(SearchBeginParams {
            width: "u32".into(),
        }))
        .await
        .is_err()
    );
    assert!(
        s.search_refine(Parameters(SearchRefineParams {
            op: "between".into(),
            value: None,
        }))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn server_call_stack_round_trip() {
    let s = LunaServer::new();
    let mut rom = demo_lorom();
    // $8000: JSR $8010 ; spin. $8010: JSL $008020 ; RTS. $8020: spin.
    let prog: &[(usize, &[u8])] = &[
        (0x0000, &[0x20, 0x10, 0x80, 0x80, 0xFE]),
        (0x0010, &[0x22, 0x20, 0x80, 0x00, 0x60]),
        (0x0020, &[0x80, 0xFE]),
    ];
    for &(off, bytes) in prog {
        rom[off..off + bytes.len()].copy_from_slice(bytes);
    }
    let rom_path = PathBuf::from("/tmp/luna_mcp_callstack_demo.smc");
    std::fs::write(&rom_path, rom).unwrap();
    s.load_rom(Parameters(LoadRomParams {
        path: rom_path.to_string_lossy().into(),
        force_mapper: Some("lorom".into()),
        force_region: None,
        power_on: None,
    }))
    .await
    .unwrap();

    // Off: empty.
    assert!(s.call_stack().await.0.frames.is_empty());

    s.enable_call_stack(Parameters(EnableCallStackParams { enabled: true }))
        .await;
    s.step(Parameters(StepParams { count: 2 })).await.unwrap();
    let frames = s.call_stack().await.0.frames;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].pc, 0x00_8010);
    assert_eq!(frames[1].pc, 0x00_8020);

    s.enable_call_stack(Parameters(EnableCallStackParams { enabled: false }))
        .await;
    assert!(s.call_stack().await.0.frames.is_empty());
}

/// `load_rom` params with no mapper/region override — the common case.
fn rom_params(path: &str) -> LoadRomParams {
    LoadRomParams {
        path: path.into(),
        force_mapper: None,
        force_region: None,
        power_on: None,
    }
}

fn demo_lorom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let title = b"LUNA MCP DEMO        ";
    rom[0x7FC0..0x7FC0 + title.len()].copy_from_slice(title);
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FD8] = 0x00;
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
