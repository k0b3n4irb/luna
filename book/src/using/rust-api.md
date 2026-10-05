# The `luna-api` Rust surface (`Emulator`)

Add `luna-api` as a dependency and drive the emulator directly. Every
method returns `Result<_, ApiError>` unless noted. Grouped by purpose:

**Lifecycle / loading**
- `load_rom(path)` → `RomInfo`, `load_rom_bytes(bytes)`,
  `load_rom_bytes_forced(bytes, mapper)`
- `reset()`
- `firmware_dir()`, `install_firmware(src, target)` — DSP-1 etc.

**Driving**
- `step(count)` → instructions executed
- `step_until_frame(max_steps)`, `loop_probe(max_steps)` → `LoopProbe`
- `set_joypad(port, mask)`

**Observation**
- `state()` → `EmulatorState` (the whole snapshot)
- `cpu_state()`, `spc700_state()`
- `frame_count()`, `forced_blank()`, `frame_showed_content()`,
  `framebuffer_hash()`

**Rendering**
- `render_frame_png(force_display)`, `render_frame_rgba(force_display)`
- `render_frame_bg_png(bg, force_display)`
- `render_tilemap_rgba(bg_idx)` → `TilemapImage`, `render_tilemap_png(bg_idx)`
- `render_vram_tiles_png(bpp, palette_row)`, `render_palette_png(cell)`, `render_sprite_sheet_png()`
- `bg_bpp(bg_idx)` → 2/4/8 (0 if disabled), `decode_sprites()` → `Vec<SpriteInfo>`

**Save-states & export**
- `save_state()` → bytes, `load_state(bytes)`
- `export_spc()` → a 66 048-byte `.spc` sound file (SPC700 regs + ARAM + DSP regs + IPL ROM)

**Audio**
- `audio_queue_len()`, `drain_audio(max)` → `Vec<(i16, i16)>`

**Memory / register peeking**
- `peek_memory(bank, offset, count)`, `peek_aram(offset, count)`,
  `peek_vram(offset, count)`, `peek_cgram()`, `peek_pc_bytes(count)`
- `vram_bytes()`, `aram_bytes()`, `wram_snapshot()`,
  `wram_page_hashes(page_size)`, `coproc_ram()`

**Disassembly**
- `disassemble_cpu(start, …)` (M/X-aware), `disassemble_spc(start, count)`

**Tracing / diagnostics** (enable, run, then take the buffered log)
- mailbox: `enable_mailbox_log` / `take_mailbox_log`
- SA-1: `enable_sa1_log` / `take_sa1_log`, `…_side_log`, `…_trace`
- Super FX: `enable_superfx_trace` / `take_superfx_trace`
- DMA: `enable_dma_trace` / `take_dma_trace`
- CPU: `enable_cpu_trace` / `take_cpu_trace_log`
- memory: `enable_mem_trace` / `take_mem_trace_log`


## Controls & firmware

- **GUI keyboard bindings + hotkeys:** see the Controls chapter.
- **Coprocessor firmware (DSP-1, …):** install via
  `luna state --dsp1-rom <path>` or `Emulator::install_firmware`.
