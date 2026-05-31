# luna DMA / HDMA subsystem — correctness gaps vs ares

Reference-first audit of luna's DMA/HDMA against ares
(`ares/sfc/cpu/dma.cpp`). Companion to the BG / OBJ / APU gap docs.

Scope: `crates/luna-core/src/dma/` (`channel.rs`, `controller.rs`) and
the scheduler wiring in `snes.rs` (`$420B` MDMAEN, `$420C` HDMAEN, the
frame/scanline HDMA hooks).

Authored 2026-05-30.

**Headline:** the DMA/HDMA *core* (table walk, transfer modes, indirect
addressing, A-bus restrictions) is faithful and well-covered. But the
2026-05-31 HDMA-ROM coverage work surfaced one **real visible bug** in
the scheduler wiring — HDMA register writes are applied one scanline late
(#7 below). The rest are edge-case hardware *restrictions* and timing
*approximations*.

## Severity legend

- 🔴 real bug, correct ROMs misbehave
- 🟠 feature / restriction missing
- 🟡 precision / timing approximation

---

## ✅ 1. A-bus access restrictions (`validA`) — DONE

ares `dma.cpp:54-83`: the DMA A-bus **cannot** reach the B-bus or CPU
I/O — reads there return open bus (`0x00`/MDR) and writes are dropped.
The blocked ranges (banks `00-3f`/`80-bf`) are:

| range | what |
|---|---|
| `2100-21ff` | B-bus (PPU regs) |
| `4000-41ff` | CPU I/O (joypad serial) |
| `4200-421f` | CPU I/O (NMITIMEN…) |
| `4300-437f` | DMA registers |

**Done**: `valid_a()` + `read_a_valid`/`write_a_valid` wrap every DMA
and HDMA A-bus access (blocked read → 0, blocked write → dropped).
Tests `dma_a_bus_read_blocked_in_io_region_returns_open_bus`,
`dma_a_bus_write_blocked_in_b_bus_region_is_dropped`. (Surfaced a test
that had put its HDMA table at the forbidden `$4000` — fixed to `$8000`.)

## ✅ 2. WRAM→WRAM transfer — DONE (blocked)

ares `dma.cpp:94`: a transfer to B-bus `$2180` (WMDATA) from a WRAM
A-address is **invalid** — the byte is dropped:

```cpp
bool valid = addressB != 0x80
  || ((addressA & 0xfe0000) != 0x7e0000 && (addressA & 0x40e000) != 0x0000);
```

**Done**: a transfer whose computed B-offset is `$80` and whose A-source
is WRAM now suppresses the B-bus side (`is_wram_a()` in `run` + the HDMA
loop). Test `dma_wram_to_wmdata_is_blocked` (and the inverse: non-WRAM →
`$2180` is allowed).

---

## 🔴 7. HDMA register writes applied one scanline late

Surfaced 2026-05-31 wiring the `PPU/HDMA/*` test ROMs into the golden
suite. The five smooth-effect demos (WaveHDMA scroll-per-line, RedSpace
fixed-colour gradient ×3, Mode7HDMA matrix) render correctly, but the
three **HiColor per-tile-row** demos — which rewrite CGRAM via HDMA every
8 scanlines to show a full-colour photo — render the image with
horizontal **banding**: the mandrill is recognisable in the pseudo-hires
variant, but every palette band is shifted down by one line.

**Root cause** (verified in `Snes::sched_one_line`, `snes.rs:1048-1118`):

```text
1. render_current_scanline(ppu_line)   // line N drawn with CURRENT CGRAM
2. ppu_line += 1
3. hdma_run_line()                      // HDMA transfer for the new line
```

The line is rendered **before** its HDMA transfer runs, so the first
transfer only takes effect after line 0, and every write lands one
scanline late. On real hardware HDMA transfers happen in the H-blank
*before* a line is displayed (ares `ppu.cpp` / `dma.cpp` — HDMA setup at
frame start, then per-line transfer ahead of the visible line), so line N
already reflects its HDMA write. luna is off by exactly one line.

The deviation is invisible on smooth effects (a 1-line shift in a wave,
gradient, or Mode-7 matrix is imperceptible — those 5 goldens pass) but
obvious on a per-8-line palette swap.

**Fix** (deferred): reorder `sched_one_line` so the line's HDMA transfer
runs *before* `render_current_scanline`, matching hardware. This is a
core-scheduler change that shifts **every** HDMA golden by a line and
alters visible rendering, so it needs the reference-first treatment
(confirm ares' exact init-vs-first-transfer dot timing), a full
`coproc-testing` sweep + SMRPG smoke test, and GUI validation per
`audible-fixes-test-first` before commit. Tracked by the three
`#[ignore]`d `ppu_hdma_hicolor*` goldens (`snes_test_roms.rs`), which
characterise the current banded output and go red once the fix lands.

This **corrects the prior "no clear visible bug" headline** and the
"per-scanline HDMA hooked at every visible line" verified-correct note
below — the hook exists but fires one line too late.

---

## 🟡 Precision / timing

| # | Issue | ares ref | luna |
|---|---|---|---|
| 3 | MDMA cost charged as flat `8 + bytes·8`; ares adds a per-channel `+8` (and aligns the burst start to a whole CPU cycle) | `dma.cpp:16-22,108-122` | `snes.rs:1444` lumps per-channel into per-byte |
| 4 | Sync DMA is **atomic** (runs all bytes in one `run_mdma` call) so it never yields to HDMA mid-transfer; ares lets HDMA stop an active DMA at a scanline boundary (`dmaEnable = false`) | `dma.cpp:146,175` | OK in practice — sync DMA almost always runs in V-blank with no active HDMA |
| 5 | Enabling an HDMA channel mid-frame via `$420C` doesn't set it up until the next frame's `hdma_init` | `dma.cpp:28-33` | `controller.rs:78` only sets up at frame start |
| 6 | Indirect-HDMA `hdmaCompleted && hdmaFinished()` early-out after reading the first pointer byte not modelled | `dma.cpp:165` | `channel.rs:337-343` reads both pointer bytes regardless |

---

## ✅ Verified correct (do not regress)

- **All 8 transfer modes** + their B-bus offset patterns, incl. the
  aliases (mode 5 `[0,1,0,1]`, 6=`2`, 7=`3`) — matches ares
  `transfer()` `index.bit(...)` logic and the HDMA `lengths[8]` table.
- **Direction** (A→B / B→A), **A-increment** (+1 / −1 / fixed),
  `das == 0` → 64 KB.
- **HDMA**: header decode, repeat (bit 7) vs non-repeat first-line-only,
  7-bit line counter, indirect-mode pointer load + walk, multi-entry
  chaining, terminator (`00`) handling. luna's "preserve header bit 7
  for continuation `do_transfer`" is equivalent to ares' "current
  counter `.bit(7)`" for all valid line counts (1-127).
- **`$43x5/6` shared** between the DMA byte count and the HDMA indirect
  address — correct (hardware shares the register pair).
- Channel register read/write (`$43x0-$43xF`); `$420B` ascending
  channel order; per-scanline HDMA hooked at every visible line (but
  fired one line too late — see 🔴 #7).
- Per-byte cooperative `bus.tick(8)` so coprocessors (SA-1) interleave
  with the DMA instead of freezing.

## Suggested order

1. ~~#1 validA~~ — **done**.
2. ~~#2 WRAM→WRAM block~~ — **done**.
3. 🔴 **#7 HDMA one-line-late** — the only real bug; fixes the HiColor
   per-tile-row banding. Deferred pending reference-first + GUI
   validation (core-scheduler reorder).
4. 🟡 #3-#6 — timing approximations; low real-world return (the current
   model is game-compatible). Left as documented approximations.
