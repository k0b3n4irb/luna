# The state JSON (`EmulatorState`)

`luna state` / the MCP `state` tool serialise this top-level shape:

| Field | Contents |
|---|---|
| `rom` | `RomInfo`: `title`, `mapper`, `rom_bytes`, `header_rom_size_kb`, `sram_kb`, `region`, `fast_rom`, `version`, `checksum{,_complement,_valid,_computed}` (`checksum_valid` only says the header pair is consistent; `checksum == checksum_computed` says the header matches the ROM bytes: `luna state game.sfc --until-frame 0 --out - \| jq '.rom \| .checksum == .checksum_computed'`), `missing_firmware`, and `symbols_loaded` / `symbols_error` (how many labels the `.sym` gave, or why it could not be read). |
| `cpu` | 65c816 registers `a/x/y/sp/pc/pb/db/dp/p` + flags. |
| `cpu_regs` | Decoded MMIO/CPU register block. |
| `ppu` | PPU registers + VRAM/CGRAM/OAM occupancy. |
| `scheduler` | Master-clock / line / frame scheduler state: `frame_count`, `ppu_line`, `nmis_serviced`, `last_nmi_frame` (see below), … |
| `apu` | SPC700 + S-DSP state (`spc_stopped`, etc.). |
| `dma` | `mdmaen` / `hdmaen` (the `$420B` / `$420C` enable masks) and the per-channel DMA/HDMA registers (see below). |
| `stats` | Cumulative counters since reset: `instructions_executed`, `instructions_active`, `total_mclk`, and `total_mclk` split by consumer — `mclk` (cumulative) and `last_frame` (the last completed PPU frame), each `{cpu_active, cpu_wai, cpu_stp, dma, hdma, refresh, total}`. See below. |
| `sa1`, `gsu`, `dsp1`, `call_stack` | Coprocessor blocks — SA-1, Super FX, DSP-1 (present when the cart has one; `gsu` is described under *Who owns the cartridge*) — and the `--call-stack` capture. |
| `peeks` | One entry per `--peek`, in order: `{spec, space: "cpu"\|"aram", addr, bytes_hex, unmapped?, error?}`. Always present (empty without `--peek`); a failed peek keeps its slot with an `error` string instead of vanishing; `unmapped` appears only when part of the range is open bus. |
| `until_pc` | Only with `--until-pc`: `{spec, addr, hit, hits_seen, reached, frame?, line?}` — where the run stopped, or `reached: false` when the bound came first. |
| `poke` | Only with `--poke-at`: `{spec, addr, hit, hits_seen, applied}` — whether the bytes were written. |
| `peek_hits` | Only with `--peek-at`: one `{hit, frame, line, peeks: […]}` per arrival on the routine, `peeks` shaped as above. |

```bash
# The harness-friendly peek channel: read bytes from the JSON, not stderr.
luna state -n 1000000 --peek 7E:0200:04 --out - game.sfc \
  | jq -r '.peeks[0].bytes_hex'
# → e.g. 00f04512
```

**Is the NMI still alive (`scheduler.last_nmi_frame`).** `nmis_serviced >
0` passes a ROM whose NMI ran during boot and died later — a handler that
crashed, or code that wrote `$4200 = 0` and never turned it back on.
`last_nmi_frame` is the `frame_count` at which the latest NMI was
delivered (`null` if none since power-on, reset or a state load), so one run
answers it:

```bash
luna state --until-frame 300 --out - game.sfc \
  | jq '.scheduler | .frame_count - .last_nmi_frame <= 1'
# → true while the NMI fires every frame
```

A ROM that leaves `NMITIMEN` off on purpose for a while (a long decompress,
a transition) reads `false` during that window; pick the frame you probe
accordingly.

**Who used the cycles (`stats.mclk` / `stats.last_frame`).** `total_mclk`
says how long the machine ran, not who used the time, and
`instructions_executed` counts every tick of a CPU parked in `WAI`, so on
an idle ROM *less* work per frame reads as *more* instructions. The
buckets partition every master cycle exactly: `cpu_active` (instructions,
interrupt dispatch, reset), `cpu_wai` (parked in `WAI`), `cpu_stp`, `dma`
(general-purpose bursts), `hdma` (per-line transfers + table fetches +
frame-start init) and `refresh` (the 40-clock DRAM refresh per scanline);
`total` is their sum. `last_frame` is the same split for the last
completed PPU frame. A charge that crosses the frame boundary — a bus
access, or a DMA burst — is split at the boundary, so `total` is the
frame's length (357 368 on NTSC, four clocks less on the frames with the
short line). `instructions_active` excludes the parked ticks. For every
frame of a window instead of the last one, see `luna profile
--frames-out`.

```bash
# CPU headroom of the last frame: how much of it the game spent in WAI.
luna state --until-frame 200 --out - game.sfc \
  | jq '.stats.last_frame | {headroom: (.cpu_wai / .total), dma, hdma}'
# → {"headroom": 0.71, "dma": 8536, "hdma": 6080}

# What a boot zero-fill cost: the DMA bucket after the init code ran.
luna state --until-frame 3 --out - game.sfc | jq '.stats.mclk.dma'
```

The full nested field set is the JSON Schema `luna state --schema` prints —
generated from the same types that serialise the JSON, so it never lags
the output. Use it to discover a field instead of exploring by trial:

```bash
# Every top-level block, then every field of the scheduler block.
luna state --schema | jq -r '.properties | keys[]'
luna state --schema | jq -r '.["$defs"].SchedulerState.properties | keys[]'
```

The `dma` block is the decoded view of the `$43xx` DMA/HDMA registers
(`--peek 00:4300:80` gives the same bytes raw). Each of `dma.channels[0..8]`
gives `params` (DMAP), `bbad` (BBAD, target `$2100+bbad`), `a_addr` (A1T,
table start), `a_bank`, `das` and `dasb` (the byte count / HDMA indirect
address, and its bank), `a2a` (HDMA table pointer) and `ntlr` (HDMA line
counter):

```bash
# Watch an HDMA table pointer advance per frame (e.g. a scanline wave effect)
luna state -n 3000000 --force-mapper lorom --out - "WaveHDMA.sfc" \
  | jq '.dma.channels[0] | {bbad, a_addr, ntlr}'
```
