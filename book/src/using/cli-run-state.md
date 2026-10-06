# `luna run` and `luna state`

The two subcommands that run a ROM and look at the machine: `luna run`
for a picture, a sound or a hash, `luna state` for everything else (the
JSON snapshot, memory, asserts, traces). The exit codes are on
[the CLI overview](cli-api-mcp.md#exit-codes-the-ci-contract).

## `luna run` — quick render / audio dump

```
luna run [OPTIONS] <ROM>
```

| Option | Default | Purpose |
|---|---|---|
| `<ROM>` | — | Path to the `.sfc` / `.smc` ROM. |
| `-n, --steps <N>` | `64` | CPU instructions to execute before dumping. |
| `--until-frame <F>` | — | Run until PPU frame `F` instead of the `-n` count (which is then ignored). Pins a baseline to a **frame**, not an instruction count — see below. |
| `--screenshot <PATH>` | — | Render a PNG of the framebuffer to `PATH` — 256×224, or 256×239 while the game runs in overscan (SETINI bit 2). |
| `--force-display` | off | Bypass INIDISP forced-blank so you see whatever is in VRAM/CGRAM. |
| `--bg <1..=4>` | composited | Render ONLY that BG layer instead of the composited frame. |
| `--audio-out <PATH>` | — | Capture the APU's 32 kHz stereo output to a WAV. |
| `--force-mapper <M>` | auto | Force a mapper for a headerless / checksum-invalid ROM: `lorom`, `hirom`, `exhirom`, `sa1`, `superfx`, `dsp1`, `sdd1`. (`spc7110` is recognised but not emulated: forcing it fails at load with an unsupported-mapper error.) |
| `--force-region <R>` | header | Force the video standard (`ntsc`/`pal`) — changes the scanline count (262/312) and frame rate. |
| `--power-on <S>` | `zero` | What RAM holds before the ROM boots: `zero`, `ones`, `random` (seed derived and printed) or `random=<seed>`. See *Power-on memory state* below. |
| `--native-res` | off | Emit the native **512×448** frame for `--screenshot` *and* `--print-fbhash` (both, since v1.24.0 — `run`'s screenshot used to stay 256×224 while its hash went native): hi-res modes 5/6 & pseudo-512 keep both horizontal subpixels, interlace keeps both fields as lines. `--bg N` has no native form and stays 256 wide. |
| `--wdm-out <PATH>` | — | Write captured `WDM $xx` executions (the `SNES_ASSERT` channel) — a non-empty file means an assertion fired. |
| `--nocash-out <PATH>` | — | Write everything the ROM printed to the `$21FC` Nocash TTY (the SDK's `SNES_NOCASH` text channel), as raw bytes — the ROM's own log, readable with no debugger attached. |
| `--print-fbhash` | off | Print `fbhash=<16-hex>`, a cross-arch-stable key for the displayed frame. |

```bash
luna run -n 12000000 --screenshot /tmp/title.png "game.sfc"

# Visual baseline for a reference ROM with a bad header (e.g. a PeterLemon
# test ROM): force the mapper so it renders, and print the hash key.
luna run -n 3000000 --force-mapper lorom --print-fbhash "WaveHDMA.sfc"
# → fbhash=7429bf441a1c7d6c   (record this as the test's expected value)

# Read the ROM's own printf channel and its assertion channel after 60 frames.
luna run --until-frame 60 --nocash-out /tmp/tty.txt --wdm-out /tmp/wdm.txt "game.sfc"
# → Nocash ($21FC) log written to /tmp/tty.txt  (0 bytes)     <- the ROM printed nothing
#   WDM log written to /tmp/wdm.txt  (0 hit(s))               <- no assertion fired
```

**Index baselines by frame, not by instruction count.** A `-n` baseline
lands on whatever the ROM is doing after N instructions, so any change to
the code between boot and the capture — a codegen improvement, a longer
zero-fill, five bytes more of crt0 — moves the capture onto another
animation phase and the hash changes without any rendering regression.
`--until-frame` stops at the start of PPU frame `F` instead, so the same
game frame is captured before and after the change:

```bash
# Visual baseline at PPU frame 200 — stable across codegen changes.
luna run --until-frame 200 --print-fbhash --screenshot f200.png "game.sfc"
# → fbhash=303497668ba19add
```

Scripted input is frame-indexed too, so the two combine: each `--input`
checkpoint lands on its exact PPU frame and any checkpoint past the target
frame simply never fires.

```bash
# Hold Right from frame 10, snapshot at frame 130.
luna state --until-frame 130 --input "10:0x0100" --peek player_x:2 game.sfc
```

The same option exists on `luna state` (with `--input`, asserts and
traces), and `luna test` manifests take the run bound as `frames = N` /
`[[checkpoint]] at_frame = N` for the same reason.

**Power-on memory state (`--power-on`).** Real RAM does not come up
zeroed, and luna's default all-zero machine hides every boot bug that
depends on it: a ROM that forgets to force-blank the screen renders a
black frame from zero VRAM/CGRAM, and passes. `--power-on random` fills
WRAM, VRAM, CGRAM (kept to 15 bits), OAM and APU RAM with seeded
pseudo-random bytes before the ROM boots — what ares does on power
(`cpu.cpp`, `ppu.cpp`, `dsp.cpp`) and Mesen2's `Random` RAM state — so
the bug shows on the emulator too. `ones` fills with `$FF` (Mesen2's
`AllOnes`), the other classic tripwire. A soft reset keeps memory, as the
hardware and both references do. `random` also draws the PPU registers
and latches that come up undefined (ares does; `zero` and `ones` leave
them at their defaults).

The cartridge's own RAM follows the same rule when **no battery keeps
it**: a Super FX board's Game Pak RAM (`$70:0000`), an SA-1 board's
BW-RAM (`$40:0000`) and I-RAM (`$00:3000`), the save RAM of a `ROM+RAM`
header. A read of a Super FX framebuffer nobody cleared, or of an I-RAM
byte nobody wrote, then shows as it would on a console. RAM the header
declares battery-backed (`$FFD6` low nibble 2, 5, 6, 9 or A) is the save:
it stays zero, or what `--srm-in` loaded. The I-RAM is filled either way
(no save file carries it). It is drawn after everything else, so a seed
still gives the WRAM, VRAM and APU RAM it gave when the cartridge was
left out.

```bash
# Seed 1: the Game Pak RAM no longer reads zero before the GSU has drawn.
luna state --power-on random=1 --until-frame 0 --peek 70:0000:10 game.sfc
# →   $700000  8E 84 DB 22 1D 73 AC 2D A6 11 DA B0 B5 B9 2A AB
```

```bash
# Boot under garbage RAM; the derived seed is printed so a failure replays.
luna run --power-on random --until-frame 2 --print-fbhash game.sfc
# → power-on: random (seed=0x18c2a5e34f9b1d07)
#   fbhash=…
# Replay that exact machine.
luna run --power-on random=0x18c2a5e34f9b1d07 --until-frame 2 --print-fbhash game.sfc
```

Run a corpus in both modes: `luna test` manifests take `power_on =
"random"` (+ `seed`, default 1) — see the homebrew CI chapter.

## `luna state` — JSON snapshot + diagnostics (the workhorse)

```
luna state [OPTIONS] <ROM>
```

Emits the same `EmulatorState` JSON the MCP `state` tool returns ([the state JSON](state-json.md)),
and is the hub for every headless diagnostic.

| Option | Default | Purpose |
|---|---|---|
| `<ROM>` | — | Path to the ROM (not needed with `--schema`). |
| `-n, --steps <N>` | `1000` | CPU instructions before snapshotting. |
| `--until-frame <F>` | — | Run until PPU frame `F` (then snapshot) instead of the `-n` count, which is then ignored. Frame-indexed baselines and asserts (see `run`). |
| `--schema` | off | Print the JSON Schema of the `--out` payload ([the state JSON](state-json.md)) and exit — no ROM needed. |
| `--out <PATH>` | `-` | Where to write the JSON (`-` = stdout). |
| `--force-mapper <M>` | auto | Force a mapper for headerless ROMs: `lorom`, `hirom`, `exhirom`, `sa1`, `superfx`, `dsp1`, `sdd1` (as in `run`; `spc7110` is recognised but not emulated). |
| `--force-region <R>` | header | Force the video standard: `ntsc` or `pal`. |
| `--power-on <S>` | `zero` | What RAM holds before the ROM boots: `zero`, `ones`, `random` (seed derived and printed) or `random=<seed>`. See *Power-on memory state* above. |
| `--native-res` | off | As in `run` — native 512×448 output for `--screenshot` and `--print-fbhash`. |
| `--sym <PATH>` | auto-detect `<rom>.sym` | Load a WLA-DX symbol file (annotated disasm, named addresses). |
| `--dsp1-rom <PATH>` | — | Install `dsp1b.rom` firmware then load (Mario Kart, Pilotwings). Persists. The file must be exactly 8192 bytes or it is refused with the reason, leaving any working install untouched — a firmware dump cannot be re-downloaded, and an empty or truncated one that installed "successfully" would leave every DSP-1 game running with an inert chip and no error. A dump that is present but unusable is reported as missing. |
| `--load-state <PATH>` | — | Load a `.luna` save-state right after ROM load, before warm-up (resume a GUI-captured scene). |
| `--input <SCRIPT>` | — | Scripted joypad-1 input ([input scripts](input-scripts.md)). |
| `--input2 <SCRIPT>` | — | Scripted joypad-2 input, same grammar ([input scripts](input-scripts.md)) — a two-player probe, or replaying an MCP `script_p2` capture. |
| `--input3`, `--input4`, `--input5 <SCRIPT>` | — | Players 3-5: a Super Multitap's pads B-D with `--port2 multitap` (pad A is player 2). Same grammar. |
| `--port1 <DEV>`, `--port2 <DEV>` | `pad` | What is plugged into each controller port: `pad`, `mouse`, `superscope`, `multitap` or `none` ([input scripts](input-scripts.md)). |
| `--mouse <SCRIPT>`, `--superscope <SCRIPT>` | — | Scripted SNES Mouse motion / Super Scope aim for whichever port holds that device ([input scripts](input-scripts.md)). |
| `--srm-in <PATH>` | — | Load battery SRAM from a `.srm` file before running — the second half of a power-cycle test. See *Asserting on memory* below. |
| `--srm-out <PATH>` | — | Write battery SRAM to a `.srm` file after the run (an empty file on a cartridge with no battery). |
| `--screenshot <PATH>` | — | Also write a PNG. |
| `--audio-out <PATH>` | — | Also write a 32 kHz stereo WAV. |
| `--peek <B:O:C>` | — | Hex-dump `COUNT` bytes at `BANK:OFFSET` to stderr (repeatable; **all three fields are hex** — `7E:0200:20` is 32 bytes). The whole 24-bit space is readable: WRAM, ROM (including `$C0-$FF` HiROM banks), SRAM, coprocessor RAM; the `$2000-$5FFF` register band reads `0` (no side effects), except the DMA channel registers `$4300-$437F`, which read their real values (`$FF` at power-on). An unmapped range reads `$FF` like the open bus, with a stderr note and an `unmapped` count in the JSON entry. Each result is mirrored into the `--out` JSON `peeks` array (see [the state JSON](state-json.md)) — the machine-readable channel a harness should parse. |
| `--assert <SPEC>` | — | After the run, check that memory holds the expected bytes: `BANK:OFFSET=HEX` (all hex) or `SYMBOL=HEX` through the loaded `.sym`. Prints `PASS` / `FAIL` per spec; any `FAIL` makes the exit code `1`. Repeatable. See *Asserting on memory* below. |
| `--assert-aram <SPEC>`, `--assert-vram <SPEC>`, `--assert-cgram <SPEC>` | — | The same check over APU RAM, VRAM and CGRAM: `OFFSET=HEX`, a hex byte offset into that memory (CGRAM is 512 bytes, low byte of each colour first). Repeatable. |
| `--call-stack` | off | Track the 65C816 call stack during the run (JSR / JSL / RTS / RTL and interrupts); the `--out` JSON gains a `call_stack` array. See *Where is the CPU, and how did it get there* below. |
| `--dump-vram <PATH>` | — | Dump all 64 KB PPU VRAM (raw). |
| `--dump-aram <PATH>` | — | Dump all 64 KB APU ARAM (raw). |
| `--dump-coproc-ram <PATH>` | — | Dump coprocessor work RAM (Super FX Game Pak RAM), ungated. |
| `--apu-log <PATH>` | — | CSV of every `$2140-$2143` CPU↔APU mailbox access. |
| `--dsp1-trace <PATH>` | — | DSP-1 (µPD77C25) trace: microcode execution **and** CPU-side DR/SR traffic in one stream — `seq,kind,pc,opcode,value,a,b,dr,sr,rqm` (`kind` = E/W/R/S). |
| `--dsp1-trace-ports` | off | Restrict the above to the DR/SR transactions (the stock firmware idles in an RQM loop, so a full trace is mostly idle spin). |
| `--dsp1-trace-commands <PATH>` | — | The DSP-1 port traffic grouped into one CSV row per command (implies `--dsp1-trace-ports`). See *Command transactions* below. |
| `--dsp1-trace-max <N>` | `200000` | Cap on captured DSP-1 events. |
| `--dsp-trace <PATH>` | — | CSV of every DSP register write: `spc_cycles,reg,name,value`, with `name` decoded (`V0_ADSR1`, `KON`, `FLG`, …). `spc_cycles` counts SPC700 cycles from the start of the run (1 024 000 per second; 32 per output sample). |
| `--dsp-trace-max <N>` | `100000` | Cap on captured DSP writes. |
| `--sa1-log <PATH>` | — | CSV of every `$2200-$23FF` SA-1 MMIO access. |
| `--sa1-side-log <PATH>` | — | The same registers seen from the **SA-1's** side: its own reads and writes of `$2200-$23FF`, plus its writes to I-RAM (`$3000-$37FF`), each with the SA-1 PC — `seq,sa1_pc,kind,reg,value`. Shows the handshake flags the two CPUs exchange, which the S-CPU-side `--sa1-log` cannot. |
| `--cpu-trace <PATH>` | — | Per-instruction 65C816 register trace: `mclk_total,frame_ntsc,pc,a,x,y,sp,p,db,dp,e` (pre-opcode snapshot). The stream to diff against a Mesen2 trace when bisecting a divergence — see the example below. |
| `--cpu-trace-from <N>`, `--cpu-trace-max <N>` | `0`, `100000` | Start capturing at instruction count `N`; hard cap on captured events (≈ 40 bytes each). Aim the window at the scene under test instead of tracing from reset. |
| `--sa1-trace <PATH>`, `--sa1-trace-max <N>` | —, `200000` | Per-instruction SA-1 trace (`seq,pc,a,x,y,sp,p,db,dp,e`) and its event cap. |
| `--superfx-trace <PATH>`, `--superfx-trace-max <N>` | —, `200000` | Per-opcode GSU trace (`seq,mclk,go,stop,pc,opcode,sfr,r0..r15`; `go` / `stop` are `1` on the row where a job starts / ends) and its event cap. |
| `--superfx-trace-from <N>` | `0` | Start the GSU trace at instruction `N` — an instruction count like `--dma-trace-from`, not a frame. The trace is a ring that keeps the most recent events once full, so the run's end (`-n` / `--until-frame`) chooses the window and this trims its head: you get exactly `[N, end]` as long as it fits under the cap. For frame `F`, read `stats.instructions_executed` from `luna state --until-frame F --out -`. |
| `--gsu-bus-trace <PATH>`, `--gsu-bus-trace-max <N>` | —, `200000` | Every CPU read of Game Pak ROM or RAM made **while the Super FX owned it** (`seq,frame,line,mclk,pc,addr,kind`). See *Who owns the cartridge* below. |
| `--spc-trace <PATH>`, `--spc-trace-max <N>` | —, `200000` | Per-instruction SPC700 trace (`seq,pc,a,x,y,sp,psw,spc_cycle,t2_int,t2_out`) and its event cap. |
| `--dma-trace <PATH>` | — | DMA→VRAM bytes as read during the transfer, with `line`, `hclock`, blank flags, the A-bus `src` and the `vram_word` each byte lands at. |
| `--dma-trace-from <N>`, `--dma-trace-max <N>` | `0`, `500000` | Instruction count at which the DMA trace starts; its event cap. |
| `--mem-trace <PATH>` | — | CSV of bus accesses: `mclk_total,frame_ntsc,pc,addr,kind,value,line,hclock,blank,force_blank,origin`. `origin` = `cpu`, `dma<n>` or `hdma<n>` — DMA / HDMA writes (B-bus `$21xx` and A-bus) are in the same stream as CPU accesses, stamped with the burst / line start and the PC whose access ran them. |
| `--mem-trace-from <N>`, `--mem-trace-max <N>` | `0`, `100000` | Instruction count at which the memory trace starts; its event cap. |
| `--mem-trace-bank <B>`, `--mem-trace-addr <LO:HI>` | all | Bank / offset-range filters for `--mem-trace` (both must match). |
| `--trace-writes <O,…>` | — | With `--mem-trace`: keep only **writes** to these hex offsets, any bank (`2121,2122,420C`). The "who wrote this register" hunt — see below. |
| `--print-fbhash` | off | Print `fbhash=<16-hex>` for the displayed frame — the same key as `run`, so an `--input`-driven test can carry a visual baseline. |
| `--wdm-out <PATH>` | — | Write captured `WDM $xx` (`SNES_ASSERT`) executions — keeps the assertion oracle on an `--input` test. |

```bash
# JSON snapshot to stdout, plus a peek at SMW shadow-OAM
luna state -n 1000000 --peek 7E:0200:220 "game.sfc"

# Reach the name-entry screen by pulsing Start, then screenshot
luna state -n 55000000 \
  --input "1600:0x1000,1610:0,2000:0x1000,2010:0" \
  --screenshot /tmp/name.png "game.sfc"

# A self-contained gameplay regression test: drive input, then emit BOTH a
# visual baseline (fbhash) and the assertion oracle (WDM) in one run.
luna state -n 55000000 --input @repro.input \
  --print-fbhash --wdm-out /tmp/asserts.txt "game.sfc"

# Two players: pad 1 presses Start, pad 2 holds A from frame 200; the
# latched words land in `cpu_regs.joy1` / `cpu_regs.joy2`.
luna state --until-frame 300 --input "100:0x1000,110:0" \
  --input2 "200:0x0080" --out - "game.sfc"

# Did the crt0 clear the DMA channels? `$43x0-$43xB` come up $FF on a real
# console; a build that clears them reads zeros here.
luna state -n 200 --power-on random --peek 00:4300:10 "game.sfc"
```

Reproduce the golden harness's configuration exactly — it runs the homebrew
corpus as **PAL** to match krom's reference captures, which a blank test-ROM
header can't say:

```bash
luna state -n 5000000 --force-mapper lorom --force-region pal \
  --screenshot /tmp/bra.png "CPUTest/CPU/BRA/CPUBRA.sfc"
```

Exact-resolution regression for the hi-res / interlace demos: the
PPU really computes 512 horizontal subpixels and two interlace fields, then
averages them into the displayed 256×224 — `--native-res` keeps them:

```bash
luna state -n 8000000 --force-mapper lorom --force-region pal --native-res \
  --screenshot /tmp/font.png --print-fbhash \
  "PPU/Interlace/InterlaceFont/InterlaceFont.sfc"   # → a 512×448 PNG
```

### Asserting on memory (`--assert*`, `--srm-in` / `--srm-out`)

A one-off check does not need a `luna test` manifest. `--assert` compares
bytes on the CPU bus after the run, and its three siblings do the same in
APU RAM, VRAM and CGRAM; each spec prints a `PASS` or `FAIL` line on stdout
and one `FAIL` is enough for exit code `1`. The expected value is a run of
hex bytes **in memory order**, so a 16-bit variable holding `$FDA5` is
written `A5FD`.

```bash
luna state --until-frame 120 \
  --assert 7E:0000=A5FD --assert-aram 0000=0000 \
  --assert-vram 0000=00000000 --assert-cgram 0000=0000 \
  --out /dev/null "game.sfc"
# PASS $7E:0000=a5fd
# PASS aram:0000=0000
# PASS vram:0000=00000000
# PASS cgram:0000=0000                                   → exit 0
# FAIL $7E:0000 expected a5fe got a5fd                   → exit 1 (when it differs)
```

With a symbol file loaded (`--sym`, or a `<rom>.sym` beside the ROM) the
CPU-bus form takes a label instead of an address: `--assert r_done=EFBE`.

**A C `static` by the name you wrote.** A compiler that lets two source
files each own a `static` of the same name writes the file into the
label: OpenSNES emits `player_x.main` for `static u16 player_x` in
`main.c`. Where a label is accepted (`--peek`, `--assert`, the keys of a
`luna test` manifest, the `symbol` argument of the MCP tools), the bare
name stands for that label when it is the **only** `name.<suffix>` in the
table. The exact name always wins, and two candidates are refused by name
rather than picked:

```bash
# OpenSNES's dsp1_ground: `static … tab_ab` in main.c, `7e:2000 tab_ab.main` in the .sym.
luna state --until-frame 60 --peek tab_ab:8 dsp1_ground.sfc
# peek $7E:2000 +0008:
#   $7E2000  62 00 01 00 00 FE FE 55
# Their static_dup fixture: main.c and other.c each own a `static u16 k`.
luna state --until-frame 5 --peek k:2 static_dup.sfc
# error: --peek `k:2`: ambiguous symbol `k`: `k.main` ($00:00C3), `k.other` ($00:00C9) (write the full name) (and not BANK:OFFSET:COUNT: …)
```

`--srm-out` and `--srm-in` are the two halves of a **power-cycle test**:
run A plays and writes the battery RAM to a file, run B boots a fresh
machine from that file and checks that the save survived.

```bash
# Run A: play (add --input to reach a save), then keep what the battery would keep.
luna state --until-frame 600 --srm-out /tmp/save.srm --out /dev/null "game.sfc"
# wrote 8192 bytes of SRAM to /tmp/save.srm

# Run B: a new machine with that battery RAM; the first save byte is still there.
luna state --until-frame 2 --srm-in /tmp/save.srm --assert 70:0000=5A \
  --out /dev/null "game.sfc"
# loaded 8192 bytes of SRAM from /tmp/save.srm
# PASS $70:0000=5a
```

(`$70:0000` is where a LoROM cartridge maps its SRAM; a HiROM one has it
from `$20:6000`.) In a `luna test` suite the same pair is the manifest keys
`srm_out` / `srm_in`.

### Where is the CPU, and how did it get there (`--call-stack`)

A snapshot gives a PC; `--call-stack` gives the chain of calls that led to
it. Tracking is off by default (it costs one opcode peek per instruction)
and starts with the run, so the stack holds the calls made since reset.
The array is ordered oldest first, each frame `{pc, from, kind, symbol}`:
the address entered, the address of the call, `jsr` / `jsl` / `interrupt`,
and the nearest label when a symbol file is loaded.

```bash
luna state --until-frame 120 --call-stack --out - "game.sfc" \
  | jq -c '.call_stack[-2:][]'
# {"pc":34316,"from":34228,"kind":"jsl","symbol":null}
# {"pc":34310,"from":34344,"kind":"jsr","symbol":null}   <- the innermost call
```

### What the SA-1 itself did (`--sa1-side-log`)

`--sa1-log` records the S-CPU touching the SA-1's registers.
`--sa1-side-log` records the other half of the conversation: what the SA-1
read and wrote, with the SA-1's own PC. Use both when one CPU is waiting
for a flag the other never sets.

```bash
luna state -n 2000000 --sa1-side-log /tmp/sa1_side.csv --out /dev/null "Kirby Super Star (USA).sfc"
# SA-1-side log written to /tmp/sa1_side.csv (24071 events)
head -2 /tmp/sa1_side.csv
# seq,sa1_pc,kind,reg,value
# 0,$00:8BF9,W,$2230,$00
```

Reads of I-RAM are not logged: an SA-1 spinning on a flag would fill the
file with them.

### Who wrote this register? (`--trace-writes`)

A palette entry comes out wrong and nothing in the game's own code
writes it. Rather than diffing `ppu.cgram` between two builds, record
every write to the CGRAM ports with its author — CPU instruction, DMA
burst or HDMA channel — and where in the frame it landed:

```bash
luna state --until-frame 120 --mem-trace writes.csv --trace-writes 2121,2122 game.sfc
# mclk_total,frame_ntsc,pc,addr,kind,value,line,hclock,blank,force_blank,origin
# 41822160,117,$00:8A31,$00:2121,W,$02,226,410,1,0,cpu      <- CGADD = 2, in VBlank
# 41822168,117,$00:8A34,$00:2122,W,$1F,226,418,1,0,cpu
# 42127540,117,$00:8C02,$00:2122,W,$00,12,1180,0,0,hdma1    <- HDMA channel 1, line 12, mid-frame
```

The `hdma1` row at line 12 is the overwrite: an HDMA channel enabled
mid-frame whose table still points at stale data. The CSV also keeps the
interrupt markers, to place the writes in time — `kind` `N` (NMI, at
`$4210`) and `I` (IRQ, at `$4211`) — so count writes on `kind` `W`. The same stream is
available over MCP (`enable_mem_trace { offsets, writes_only }`), and a
`run_until_mem_write` / `bp_add mem` watchpoint fires on the DMA / HDMA
write too.

### Tracing a window of CPU execution

The CPU, memory and DMA traces **stop** once their `-max` cap is reached, so
aim them with `-from` rather than tracing from reset. (The coprocessor
traces — `--spc-trace`, `--sa1-trace`, `--superfx-trace` — are rings
instead: when full they drop their oldest half, so the file ends at the
last instruction executed.) Capture 50 000 CPU instructions starting 12 M
instructions in:

```bash
luna state -n 12050000 --cpu-trace /tmp/cpu.csv \
  --cpu-trace-from 12000000 --cpu-trace-max 50000 "Super Mario World.sfc"
head -3 /tmp/cpu.csv
# mclk_total,frame_ntsc,pc,a,x,y,sp,p,db,dp,e
# 317204990,887,$00:806B,$0100,$0000,$00FE,$01FF,$32,$00,$0000,0
# 317205014,887,$00:806D,$0100,$0000,$00FE,$01FF,$32,$00,$0000,0
```

The same capture is `enable_cpu_trace` / `take_cpu_trace` over MCP.

The window can also end on a frame: with `--until-frame`, every `-from`
starts where it is asked and the run still stops on that frame (a start
past it records nothing). To aim at frame `F`, read
`stats.instructions_executed` from `luna state --until-frame F --out -`
and pass it as the `-from`:

```bash
from=$(luna state --until-frame 600 --out - game.sfc | jq '.stats.instructions_executed')
luna state --until-frame 610 --cpu-trace /tmp/cpu.csv --cpu-trace-from "$from" game.sfc
# the CPU trace covers frames 600-610
```

### Coprocessor liveness and the DSP-1 handshake

`--superfx-trace` and `--sa1-trace` let a harness prove those chips ran;
`--dsp1-trace` closes the gap for the DSP-1 (µPD77C25). Note the two
distinct flags: `--dsp-trace` is the **audio** S-DSP, `--dsp1-trace` the
**cart coprocessor**.

```bash
# Liveness without any trace: state JSON carries the instruction count.
luna state "Super Mario Kart (USA).sfc" -n 5000000 --out - \
  | jq '.dsp1.instructions_executed'    # assert >= 1
# -> 41780039

# The command handshake. --dsp1-trace-ports is what makes it readable:
# the stock firmware idles in a two-instruction RQM wait loop, so a full
# trace spends its whole budget on idle spin before your command lands.
luna state "Super Mario Kart (USA).sfc" -n 5000000 \
  --dsp1-trace dsp1.csv --dsp1-trace-ports
# seq,kind,pc,opcode,value,a,b,dr,sr,rqm
# 0,W,$0004,$000000,$80,$0000,$00C0,$0080,$0400,0   <- command byte in
# 143,S,$0185,$000000,$00,$7FFF,$0000,$3400,$0000,0 <- poll: RQM clear, busy
# 195,R,$034D,$000000,$00,$003E,$0000,$0000,$9000,1 <- result byte out

# Drop --dsp1-trace-ports to see the microcode between the transactions.
```

On a port row `pc` is *not* a CPU address: it is where the DSP-1 microcode
was sitting when the CPU touched the port. That is the column that turns a
handshake into something readable — group by it and the firmware's structure
falls out. On the run above:

```console
$ awk -F, 'NR>1 {print $2, $3}' dsp1.csv | sort | uniq -c | sort -rn | head -5
  21462 R $038F
  21462 R $038D
  21462 R $038A
  21462 R $0387
  19224 S $0387
```

Four read sites hit an identical number of times is the firmware handing back
a four-word result, one word per site — and the `S` rows at `$0387` are the
CPU polling the same site until `RQM` comes up. An off-by-one in a command's
result length shows up here as a fifth site, or as one count that does not
match the others.

#### Command transactions

`--dsp1-trace-commands` groups that byte stream into one row per command —
the command byte, the input words it consumed, the output words it produced:

```bash
luna state "Super Mario Kart (USA).sfc" -n 5000000 \
  --dsp1-trace-commands dsp1_cmds.csv
# seq,cmd,name,pc,in_words,out_words,expected_in,expected_out,confidence,status,in,out
# 128,$02,Parameter,$0004,7,4,7,4,provisional,ok,$0880|$27A0|…,$0000|$FFB2|…
# 203,$0A,Raster,$0004,5,384,-,-,unbounded,unbounded,$FFB6|$8000|…,$05FF|…
```

Boundaries come from the **protocol** — an 8-bit (`DRC`) write opens a
command, and every word until the next one belongs to it — never from the
word-count table. The table only supplies the `expected_*` columns, so a
stale entry surfaces as `status=mismatch` on that single row, with both
counts side by side, while every other transaction stays correctly grouped.
That is deliberate: a word count is documentation, and documentation must
never be able to make an emulator look broken.

Read the two verdict columns together:

| Column | Meaning |
|---|---|
| `confidence` | `verified` (checked on hardware-grade traces) → `documented` → `provisional`. How much the `expected_*` figures are worth. |
| `status` | `ok`, `mismatch`, `unbounded` (open-ended output — observed length reported, nothing asserted), `truncated` (capture hit its cap mid-transaction), `unknown` (command not in the table). |

A `mismatch` on a `provisional` row is far more likely a stale table entry
than an emulator defect. A `mismatch` on a `verified` row is worth chasing.

### Audio-side visibility

Three views for driver debugging, when a WAV capture alone cannot say
what the SPC actually did:

```bash
# 1. Structured DSP state: per-voice registers + live decode state.
luna state game.sfc -n 3000000 --out - \
  | jq '.apu.dsp | {mvol_l, kon, dir, voices: [.voices[] | select(.keyed_on)
        | {index, srcn, pitch, envx, outx, envelope_phase}]}'

# 2. Peek ARAM directly — verify an uploaded driver image or the
#    $F0-$FF register page (hex offset:count, like a CPU-bus peek).
luna state game.sfc -n 3000000 --peek APU:0200:40 --peek APU:00F0:10

# 3. DSP register-write trace — the sequencing oracle: did the
#    KON/KOFF pulses reach the chip in the intended order?
luna state game.sfc -n 2000000 --dsp-trace dsp.csv
# spc_cycles,reg,name,value
# 159382,$6C,FLG,$20
# 159499,$5D,DIR,$0A      <- sample directory at $0A00
# 162748,$5C,KOFF,$FF     <- driver mutes every voice before setup
#
# spc_cycles is the SPC700 cycle of the write, counted from the start of
# the run: divide by 32 for the sample index in an --audio-out capture.
```
