# luna DMA / HDMA subsystem — correctness gaps vs ares

Reference-first audit of luna's DMA/HDMA against ares
(`ares/sfc/cpu/dma.cpp`). Companion to the BG / OBJ / APU gap docs.

Scope: `crates/luna-core/src/dma/` (`channel.rs`, `controller.rs`) and
the scheduler wiring in `snes.rs` (`$420B` MDMAEN, `$420C` HDMAEN, the
frame/scanline HDMA hooks).

Authored 2026-05-30.

**Headline:** the DMA/HDMA *core* (table walk, transfer modes, indirect
addressing, A-bus restrictions) is faithful and well-covered. The
2026-05-31 HDMA-ROM coverage work surfaced and **fixed** one real visible
bug — HDMA writes to CGRAM were dropped during active display, breaking
the HiColor per-tile-row palette technique (#7 below). The rest are
edge-case hardware *restrictions* and timing *approximations*.

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

## ✅ 7. HDMA writes to CGRAM ($2122) were dropped during active display — FIXED

Surfaced 2026-05-31 wiring the `PPU/HDMA/*` test ROMs into the golden
suite. The five smooth-effect demos (WaveHDMA scroll-per-line, RedSpace
fixed-colour gradient ×3, Mode7HDMA matrix) rendered correctly, but the
**HiColor per-tile-row** demos — which rewrite CGRAM via HDMA mid-frame
to exceed 256 colours — rendered the photo with heavy horizontal
**banding** (the mandrill was recognisable but striped).

**First hypothesis was wrong.** It looked like a one-scanline-late
ordering bug in `Snes::sched_one_line` (render-before-HDMA). Empirically
disproved: reordering HDMA *before* the render produced a **byte-identical
frame**, and disabling per-line HDMA entirely *also* produced the
identical frame. So the per-line HDMA transfer was contributing **nothing**
to CGRAM — the writes were being dropped, not mis-timed. (ares confirms
the per-line model already matches: setup at line 0 H≈16 with no transfer,
`hdmaRun` at H=1104 affecting the next line ⇒ line N sees N transfers, same
as luna. `timing.cpp:31-46,62-78`, `dma.cpp:28-41,142-150`.)

**Real root cause.** `PPU::write` gates CGDATA on `active_display`:
`CGDATA => cgram.write_gated(value, !active_display)` (`ppu.rs:724`). That
flag is set **only on the CPU write path** (`snes.rs:1315`) and is `true`
for the whole visible region. HDMA writes go through `DmaBusView::write_b`,
which never refreshed it — so during a HiColor frame the stale-`true` flag
made every HDMA CGDATA write **drop**, leaving the per-line palette
unapplied. (The 5 smooth demos were unaffected because scroll/fixed-colour/
Mode-7 matrix registers aren't gated.)

ares `io.cpp:55-60` shows CGRAM is **never** fully dropped —
`dac.cgram[address] = data` runs unconditionally (only the *address* is
latched during active display). VRAM (`io.cpp:26`, early `return`) and OAM
(`io.cpp:40`) **do** drop during active display.

**Fix** (`DmaBusView::write_b`, `snes.rs`): CGDATA (`$2122`) via DMA/HDMA
bypasses the `active_display` gate (CGRAM is never dropped), with the flag
saved/restored so a later VRAM/OAM channel on the same line stays gated.
VRAM/OAM behaviour is unchanged. The pseudo-hires HiColor demo now renders
the mandrill **pixel-clean** (`ppu_hdma_hicolor64_pseudohires` promoted to
a passing golden); SMRPG + Chrono Trigger smoke screenshots are unchanged
(CT byte-identical, SMRPG correct); full `--lib` + golden suites green.

**Remaining sub-item** (kept `#[ignore]`d): the two *non*-pseudo-hires
HiColor demos display an RGB colour *chart* and still show residual
striping vs the reference PNG that ships with each ROM
(`HiColor*PerTileRow.png`). Dug in 2026-05-31:

- The palette is **not** pushed by HDMA. It's an **H-IRQ-driven general
  DMA**: an H-counter IRQ fires every scanline (`$4207` HTIME ≈ 170-190,
  mid active-display) and its ISR triggers a DMA of N colours into CGDATA.
  (The HDMA channel in these ROMs only drives OAM/sprite size.)
- The CGDATA writes **do land** (confirmed by instrumentation: the gate is
  already open because the ISR's mid-line behaviour differs per demo). The
  cadence is correct (CGADD advances +N colours per DMA).
- Root cause of the residual stripes: the IRQ fires **mid-line**, so on
  hardware the **left and right halves of each scanline use different
  palettes** (an intra-scanline CGRAM change). luna renders each scanline
  atomically from a single CGRAM snapshot and only partial-flushes on the
  **CPU** write path (`snes.rs:1318-1329`), not the DMA path — so it cannot
  reproduce the mid-line split.
- **Quantified vs the reference PNG** (`HiColor64PerTileRow.png`): luna is
  **81.2% pixel-exact** (88.2% within tol 24, MAE 7/255), and the diff is
  confined to the **tile-row boundary scanlines** (rows 0,8,16,24,…; 15 of
  224). Each 8-line tile-row has 7 pixel-exact lines and 1 wrong boundary
  line — exactly the line whose palette the H-IRQ splits mid-dot. Neither
  the pre-swap nor the post-swap palette matches the mix, so the boundary
  line can't be made exact without the mid-line split.
- **Not** a render-order lag: deferring the scanline render by one line was
  tried and neither fixed the stripes nor survived the suite (broke 9 other
  HDMA/Window/Mode-7 goldens). A real fix needs sub-scanline CGRAM tracking
  tied to the CPU's H-position during the DMA — a deep change to the coarse
  per-line model, with ~no commercial-game payoff. Deferred.

The pseudo-hires variant uses the same H-IRQ DMA but a per-8-line ("per
tile row") cadence on photo content, which hides the sub-line error — it
renders the mandrill pixel-clean and is a passing golden.

This **corrects the prior "no clear visible bug" headline** — the bug was
real, just not the ordering issue first suspected.

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
  channel order; per-scanline HDMA hooked at every visible line, line
  timing matches ares (line N sees N transfers — see #7, which was a
  CGRAM gating bug, not a timing one).
- Per-byte cooperative `bus.tick(8)` so coprocessors (SA-1) interleave
  with the DMA instead of freezing.

## Suggested order

1. ~~#1 validA~~ — **done**.
2. ~~#2 WRAM→WRAM block~~ — **done**.
3. ~~#7 HDMA CGRAM drop~~ — **done**: CGDATA via DMA/HDMA no longer gated
   by `active_display`; fixed the HiColor per-tile-row banding (pseudo-hires
   mandrill now pixel-clean). HiColor64/128 charts remain (finer per-scanline
   timing + no reference image).
4. 🟡 #3-#6 — timing approximations; low real-world return (the current
   model is game-compatible). Left as documented approximations.
