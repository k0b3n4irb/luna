# SNES Emulators — Comparative Survey

An overview of the principal Super Nintendo (SNES / Super Famicom) emulators,
ranked by level of maturity, hardware fidelity, and feature richness.
The goal: to help choose the emulator suited to a given use case (casual
gaming, preservation, homebrew development, multi-system integration…).

---

## Contents

- [Evaluation Criteria](#evaluation-criteria)
- [Summary Ranking](#summary-ranking)
- [Detailed Profiles](#detailed-profiles)
  - [ares](#ares)
  - [bsnes](#bsnes)
  - [Mesen-S / Mesen 2](#mesen-s--mesen-2)
  - [no$sns](#nosns)
  - [Snes9x](#snes9x)
  - [bsnes-mercury (RetroArch core)](#bsnes-mercury-retroarch-core)
  - [higan](#higan)
  - [ZSNES](#zsnes)
- [Technical Stacks at a Glance](#technical-stacks-at-a-glance)
- [Summary Table](#summary-table)
- [Recommendations by Use Case](#recommendations-by-use-case)
- [Glossary](#glossary)
- [Sources](#sources)

---

## Evaluation Criteria

Each emulator is rated along three axes:

- **Maturity**: project age, stability, development activity,
  size of the community.
- **Hardware fidelity**: accuracy of the hardware simulation
  (cycle-accurate vs HLE), handling of expansion chips (Super FX, SA-1,
  DSP, SPC7110…), audio/video behaviors matching the real console.
- **Features**: save states, rewind, netplay, shaders, debugging
  tools, multi-platform support, customization…

---

## Summary Ranking

| Rank | Emulator         | Maturity | HW Fidelity | Features        | Profile       |
|------|------------------|----------|-------------|-----------------|--------------|
| 🥇   | **ares**         | ★★★★★    | ★★★★★       | ★★★★☆           | Modern accuracy |
| 🥈   | **bsnes**        | ★★★★☆    | ★★★★★       | ★★★☆☆           | Accuracy reference |
| 🥉   | **Mesen-S**      | ★★★★☆    | ★★★★★       | ★★★★★ (debug)   | Dev / homebrew |
| 4    | **Snes9x**       | ★★★★★    | ★★★★☆       | ★★★★☆           | Versatile mainstream |
| 5    | **bsnes-mercury**| ★★★★☆    | ★★★★★       | ★★★★☆ (RA)      | RetroArch integration |
| 6    | **no$sns**       | ★★★☆☆    | ★★★★☆       | ★★★★☆ (debug)   | Reverse engineering / docs |
| 7    | **higan**        | ★★☆☆☆    | ★★★★★       | ★★★☆☆           | Obsolete (→ ares) |
| 8    | **ZSNES**        | ★☆☆☆☆    | ★★☆☆☆       | ★★☆☆☆           | Historical only |

---

## Detailed Profiles

### ares

An open-source multi-system emulator, a fork of higan, regarded as its
spiritual successor. It retains the highly accurate emulation core of
bsnes/higan while remaining actively developed and more accessible.

**Strengths**
- Cycle-accurate emulation of the SNES (incorporates the bsnes core).
- Near-total compatibility, including exotic chips
  (Super FX, SA-1, DSP-1/2/3/4, SPC7110, S-DD1, Cx4…).
- Modern interface, simpler to configure than higan.
- Modern features: run-ahead (reduces input latency),
  rewind, save states, CRT shaders, color correction.
- Covers many systems beyond the SNES (NES, GB/GBC, GBA, N64, experimental
  PSX, Master System, Mega Drive, PC Engine, etc.).
- Active development.

**Weaknesses**
- More CPU-hungry than Snes9x.
- No official RetroArch core (licensing restrictions).
- Availability limited to Windows / Linux / macOS (no Android, iOS, or
  handheld consoles).
- Fewer community plugins / extensions than RetroArch.

**What sets it apart**: it is today *the* optimal combination of
accuracy and ergonomics. If you want "the bsnes of 2026," that's ares.

**Technical stack**

- Primary language: **C++** (~94.6%), with ~4% C, plus CMake,
  GLSL, and a bit of Objective-C for the macOS glue code.
- Stated coding philosophy: **clarity over performance** (which
  partly explains the CPU demands vs Snes9x).
- Does **not use the standard STL** — it relies on an ecosystem of
  in-house libraries inherited from higan/bsnes (written by Near):

  | Library | Role |
  |---|---|
  | **nall** | STL alternative (containers, strings, utilities) |
  | **hiro** | Cross-platform GUI toolkit using native APIs (Win32, GTK, Cocoa) |
  | **ruby** | Video/audio/input abstraction layer (Direct3D, OpenGL, ALSA…) |
  | **libco** | Cooperative multi-threading (coroutines) |
  | **mia** | ROM database and internal loader |

- The choice of **libco** (cooperative coroutines) is the key
  architectural trick: each emulated component (CPU, PPU, APU,
  coprocessors) is written as a "thread" that yields control to the
  scheduler after X cycles, which makes the cycle-accurate code readable
  rather than a nested state machine.
- Build system: GNU make (with `debug` / `stable` / `release` /
  `minified` / `optimized` profiles).
- Repository: [github.com/ares-emulator/ares](https://github.com/ares-emulator/ares).

---

### bsnes

A landmark SNES emulator created by byuu (Near). Designed from the outset to
be the most accurate emulator possible, at the cost of high CPU demands.

**Strengths**
- Pioneer of cycle-accurate SNES emulation.
- Three historical profiles: *Performance*, *Balanced*, *Accuracy*.
- Excellent compatibility (close to 100%).
- Experimental graphics options (HD Mode 7 upscaling, widescreen
  hacks on certain games).
- Reference source code for SNES hardware documentation.

**Weaknesses**
- Development slowed since the passing of Near (2021).
- Recent forks now converge toward ares.
- Fewer built-in tools than Mesen-S on the debugging side.
- Less modern interface than ares.

**What sets it apart**: the founding project of high-fidelity SNES
emulation. Still relevant, but ares is generally recommended in its
place to benefit from active development.

**Technical stack**

- Primary language: **C++**.
- Shares the same technical base as ares/higan: uses **nall**
  (STL alternative), **hiro** (native cross-platform GUI), **ruby**
  (video/audio/input layer), and **libco** (cooperative coroutines) —
  all developed by Near.
- Cycle-accurate architecture based on libco, exactly like ares.
- License: **GPLv3**.
- Build: GNU make.
- Current repository: [github.com/bsnes-emu/bsnes](https://github.com/bsnes-emu/bsnes).

---

### Mesen-S / Mesen 2

Mesen-S is the SNES extension of the well-known NES emulator "Mesen." Since
Mesen 2, the two have merged into a single multi-system emulator
(NES, SNES, GB/GBC, PC Engine).

**Strengths**
- Cycle-accurate emulation of the SNES.
- **Exceptional debugging tools**, among the best of all emulators
  combined:
  - Debugger with breakpoints, watch, labels.
  - Built-in assembler.
  - Event Viewer (raster, DMA, IRQ…).
  - Tile / Sprite / Palette / Tilemap viewers.
  - Trace Logger, Performance Profiler.
  - Script window (Lua).
- HD packs, video filters, netplay, rewind, overclocking, custom
  palettes.
- Clear interface, per-game configuration saved automatically.

**Weaknesses**
- Limited platforms (mainly Windows, Linux via build).
- No mobile version.
- Smaller community than Snes9x.
- Less geared toward the "average gamer" — the richness of the UI can be intimidating.

**What sets it apart**: it is the reference emulator for
**homebrew development** and **ROM hacking**. No competitor offers
such a level of integrated debugging tooling.

**Technical stack**

- **Dual-language architecture** typical of Mesen:
  - **Emulation core in C++** (CPU, PPU, APU, coprocessors) for
    performance.
  - **Graphical interface and debugging tools in C#** (.NET) — first
    WinForms, then Avalonia for Mesen 2 to gain
    Linux/macOS portability.
- This separation explains the richness of the debug UI: C# allows
  rapid development of the many tool windows without
  compromising the core's performance.
- License: **GPLv3**.
- Repositories:
  - [github.com/SourMesen/Mesen-S](https://github.com/SourMesen/Mesen-S) (historical, no longer maintained)
  - [github.com/SourMesen/Mesen2](https://github.com/SourMesen/Mesen2) (active, recommended)

---

### no$sns

A Windows emulator/debugger developed by **Martin Korth** (alias "Martin
Korth de Problemkaputt"), author of the no$ lineage (no$gba, no$gmb, no$nes,
no$psx…), long renowned for its technical accuracy and the unmatched
quality of the associated hardware documentation.

**Strengths**
- **Exceptional "fullsnes" hardware documentation**: the
  [fullsnes.htm reference](https://problemkaputt.de/fullsnes.htm) is
  regarded as **the** unofficial SNES spec by the
  homebrew/reverse community — used by the other emulators themselves as a
  primary source (registers, timings, coprocessor behaviors).
- A very polished debugger: built-in assembler, disassembler.
- **The only emulator** offering in-depth debugging of coprocessors beyond
  the SPC700 (SA-1, Super FX, DSP, CX4, ST018, SPC7110…).
- Very broad emulation of exotic add-ons and accessories:
  Satellaview, Super Disc CDROM, Turbofile, lightguns, Exertainment Bike,
  Barcode Battler, X-Band Keyboard, NTT Data Pad…
- **Xboo-Upload**: allows sending code directly to real
  SNES hardware for testing (rare).
- Compact, starts instantly, dense but efficient "old-school" interface.

**Weaknesses**
- **Closed source** (unlike all the other serious emulators
  on this list).
- Very slow development: latest version 1.9 in 2017, few updates
  since (the project is in near-hibernation).
- No **watchpoints** (data breakpoints) — a strong limitation for debugging,
  forcing one to supplement it with bsnes/Mesen-S.
- Accuracy reputed to be good on common games but inferior to
  bsnes/ares/Mesen-S on edge cases.
- Windows only (works via Wine on Linux/macOS).
- Very dated interface, confusing ergonomics for newcomers.
- The "free" version is restricted, the paid "no$sns debug" version requires payment
  (donation via the author's site).

**What sets it apart**: its **major contribution to the SNES ecosystem is
not the emulator itself but the fullsnes documentation**, which has enabled
a whole generation of developers and competing emulators
(bsnes, Mesen-S, ares) to advance. It is also the only one to push
coprocessor debugging to this level.

To be used as a **complement** to another emulator (typically Mesen-S
for watchpoints, no$sns for the docs and coprocessor debugging).

**Technical stack**

- **100% x86 assembly** — this is the signature of the entire no$ lineage by
  Martin Korth (no$gba, no$gmb, no$nes, no$psx, no$sns).
- Direct consequence: a **tiny memory footprint** and extreme
  performance. Martin Korth states that "on 1 GHz PCs, most
  games run 5 to 10× faster than on real hardware."
- **Closed source** — source code not published.
- **32-bit x86 only**, which makes any native port to
  ARM, pure x86-64, or other architectures impossible. On Linux/macOS, you must
  go through Wine.
- No standard build system, no public repository — distribution
  only via binaries on [problemkaputt.de](https://problemkaputt.de/sns.htm).
- Of note: the author has released some of his emulators (no$gba 2.7c+)
  as freeware, but not the source code.

---

### Snes9x

A landmark SNES emulator, the most popular for the general public. It has
existed since 1997 and continues to evolve.

**Strengths**
- High compatibility (~99.5% of the SNES catalog).
- Very light on CPU, runs on modest hardware.
- Available on nearly every platform:
  Windows, Linux, macOS, Android, iOS, 3DS, PSP, Wii, Xbox, Switch
  (homebrew), browsers (WASM), etc.
- Save states, netplay, cheats, fast-forward, slow motion, controller
  support, multiplayer, very extensive customization.
- Active development, large community.
- Extremely numerous derivative forks.

**Weaknesses**
- Not cycle-accurate: uses a few HLE approximations for
  performance.
- A few games with intricate effects or technical demos may exhibit
  subtle flaws invisible to the average gamer.
- Debugging tools markedly more rudimentary than Mesen-S.

**What sets it apart**: the best
**compatibility / performance / portability** ratio. It is *the* default
choice recommended for 95% of users.

**Technical stack**

- Primary language: **C++**, with some historical portions in C and
  old CPU cores partially in assembly (largely removed over
  the versions in favor of portability).
- **No dependence on a single GUI toolkit**: the core is a pure
  emulation engine, and several official or third-party front-ends coexist
  (GTK, Qt, native Windows, Cocoa, SDL, Android…).
- This decoupled architecture is what explains the **exceptional
  portability**: it is trivial for a third-party developer to graft
  the Snes9x core onto any platform.
- License: **non-commercial** (custom, derived from a BSD spirit but
  with a clause prohibiting commercial use without agreement — hence the absence
  of Snes9x from certain commercial Linux distributions).
- Build: Autotools / Make depending on the platform.
- Repository: [github.com/snes9xgit/snes9x](https://github.com/snes9xgit/snes9x).

---

### bsnes-mercury (RetroArch core)

A fork of bsnes maintained by the libretro community, designed to integrate
into RetroArch while remaining as accurate as the official bsnes.

**Strengths**
- Accuracy identical to bsnes by default (optional HLE is
  disabled).
- Three cores available: Performance / Balanced / Accuracy.
- Benefits from the entire RetroArch ecosystem:
  shaders, netplay, achievements (RetroAchievements), run-ahead,
  rewind, unified controller management, cloud saves.
- Available on all RetroArch platforms
  (including Android, consoles, Raspberry Pi).
- FPS and sample rates conforming to the SNES NTSC/PAL standard.

**Weaknesses**
- Requires RetroArch (UI confusing for beginners).
- The "Accuracy" core remains CPU-demanding.
- No advanced debugging tools.

**What sets it apart**: it is the only way to have bsnes-type
accuracy **within RetroArch**, hence in a unified multi-system
environment.

**Technical stack**

- Primary language: **C++**, inherited directly from bsnes (and therefore from the
  nall/libco libs).
- **Wrapped as a libretro core**: the code exposes the standard libretro
  API, which allows RetroArch (and any libretro front-end) to load it
  as a dynamic library (`.so` / `.dll` / `.dylib`).
- The modifications relative to the official bsnes mainly concern:
  restored features, targeted optimizations, integration of
  libretro hooks (input, audio, video callbacks).
- License: **GPLv3** (inherited from bsnes).
- Build: GNU make with libretro adaptations.
- Repository: [github.com/libretro/bsnes-mercury](https://github.com/libretro/bsnes-mercury).

---

### higan

The historical evolution of bsnes by Near, which expanded the project to several
Nintendo systems (NES, SNES, GB/GBC/GBA, Famicom Disk System, Super Game
Boy, Satellaview…).

**Strengths (historical)**
- First emulator to reach 100% SNES compatibility.
- First to correctly emulate SPC7110, cycle-accurate SPC700,
  Super FX, Super Game Boy.
- Dot-based renderer for the GBA (instead of scanline).

**Weaknesses**
- No longer maintained: replaced by ares.
- Interface reputed to be austere and confusing.
- ROM (Game Pak) configuration complicated for newcomers.

**What sets it apart**: historical interest only. All the
technical advantages of higan are today present in ares, with
the addition of active development and a more accessible UI.

**Technical stack**

- Primary language: **C++**, written by Near.
- **Origin of the nall / hiro / ruby / libco ecosystem**: these
  libraries were designed for higan, and are today
  reused by bsnes and ares.
- Cycle-accurate architecture based on libco (cooperative coroutines).
- License: **GPLv3**.
- Build: GNU make.
- Status: **archived** — development has shifted to ares.

---

### ZSNES

One of the very first mainstream SNES emulators (1997). Very
popular in the 2000s thanks to its performance on the machines
of the era.

**Strengths (historical)**
- Very performant on period hardware.
- A "console"-style visual interface appreciated at the time.
- Enormous library of compatible ROM hacks.

**Weaknesses**
- Development abandoned since 2007.
- Many hardware inaccuracies (used hacks lacking high fidelity).
- Known security vulnerabilities in the hand-written x86 code.
- Compatibility inferior to modern emulators.

**What sets it apart**: purely historical / nostalgic interest.
**To be avoided** for any serious use today.

**Technical stack**

- **Massively written in x86 assembly** (the emblematic signature of
  ZSNES), with a bit of C and C++ for the glue code and the GUI.
- In version 1.50 (2006), only about **15%** of the asm code had
  been ported to C — the rest was (and remained) in 32-bit x86
  assembly.
- Direct consequences:
  - Spectacular performance on 1990s–2000s hardware.
  - **Porting nearly impossible** to other architectures (ARM,
    pure x86-64, PowerPC…), which sealed its obsolescence.
  - **Security vulnerabilities**: the hand-written asm code contains
    several buffer overflow flaws exploitable via malicious
    ROMs (documented CVEs).
- License: **GPLv2**.
- Status: **abandoned since 2007**. A few forks attempt to
  maintain a modern build (e.g.
  [github.com/xyproto/zsnes](https://github.com/xyproto/zsnes)).

---

## Technical Stacks at a Glance

| Emulator       | Primary language(s)              | License        | Open source | GUI toolkit            | Notable source                              |
|----------------|----------------------------------|----------------|-------------|------------------------|---------------------------------------------|
| ares           | C++ (~95%)                       | GPLv3 / ISC    | ✅          | hiro (native)          | nall / hiro / ruby / libco / mia (Near)     |
| bsnes          | C++                              | GPLv3          | ✅          | hiro (native)          | nall / hiro / ruby / libco (Near)           |
| Mesen-S / 2    | **C++ (core) + C# (.NET / Avalonia for Mesen 2)** | GPLv3 | ✅ | WinForms → Avalonia    | Dual-language core/UI architecture          |
| no$sns         | **100% x86 assembly (32-bit)**   | Proprietary    | ❌          | native Win32 (asm)     | Martin Korth's no$ lineage                   |
| Snes9x         | C++ (little C, ex-asm removed)   | Custom non-commercial | ✅   | Multiple (GTK, Qt, Win32…) | Portable core, decoupled front-ends     |
| bsnes-mercury  | C++ (bsnes fork)                 | GPLv3          | ✅          | None (libretro core)   | libretro wrap                                |
| higan          | C++                              | GPLv3          | ✅          | hiro (native)          | Origin of nall / hiro / ruby / libco        |
| ZSNES          | **x86 assembly (~85%) + C/C++**  | GPLv2          | ✅          | Custom (console mode)  | Nearly impossible to port beyond x86         |

> **Key takeaways**:
> - Four technical families emerge:
>   1. **The Near school** (ares, bsnes, higan, bsnes-mercury): C++ + nall + hiro + libco, cycle-accurate via cooperative coroutines.
>   2. **The Mesen school** (Mesen-S, Mesen 2): high-performance C++ core + tool-rich C# UI.
>   3. **The portable school** (Snes9x): pure C++, core decoupled from the GUI, optimized for maximum portability.
>   4. **The assembly school** (no$sns, ZSNES): raw performance at the cost of portability and maintainability.

---

## Summary Table

| Emulator       | Type        | Cycle-accurate | Multi-platform             | Debug | Netplay | Shaders | Run-ahead | RetroArch core |
|----------------|-------------|----------------|----------------------------|-------|---------|---------|-----------|----------------|
| ares           | Multi-sys   | ✅             | Win / Linux / macOS         | Basic | ❌      | ✅      | ✅        | ❌             |
| bsnes          | SNES-only   | ✅             | Win / Linux / macOS         | Basic | ❌      | ⚠️ Limited | ❌      | ✅ (official)  |
| Mesen-S        | Multi-sys   | ✅             | Win / Linux                 | ✅✅✅ | ✅      | ✅      | ❌        | ❌             |
| no$sns         | SNES-only   | ≈ (good)       | Windows (Wine elsewhere)    | ✅✅ (coproc.) | ❌ | ❌      | ❌        | ❌             |
| Snes9x         | SNES-only   | ❌ (partial HLE) | Everywhere                | Basic | ✅      | ✅      | ✅ (via RA) | ✅           |
| bsnes-mercury  | SNES-only   | ✅             | Everywhere (via RetroArch)  | ❌    | ✅ (RA) | ✅ (RA) | ✅ (RA)   | ✅             |
| higan          | Multi-sys   | ✅             | Win / Linux / macOS         | Basic | ❌      | ❌      | ❌        | ❌             |
| ZSNES          | SNES-only   | ❌             | Win / DOS (legacy)          | ❌    | ✅ (LAN) | ❌    | ❌        | ❌             |

---

## Recommendations by Use Case

| Use case                                               | Primary recommendation               | Alternative              |
|--------------------------------------------------------|--------------------------------------|--------------------------|
| Playing on a modern PC, faithful experience            | **ares**                             | bsnes / bsnes-mercury    |
| Playing on modest hardware / Raspberry Pi              | **Snes9x**                           | bsnes-mercury Performance|
| Playing on mobile (Android / iOS)                      | **Snes9x EX+** (Android)             | RetroArch + Snes9x core  |
| Homebrew development / ROM hacking                     | **Mesen-S / Mesen 2**                | bsnes (debug build)      |
| Hardware reverse engineering / coprocessor docs        | **no$sns** + fullsnes.htm            | Mesen-S                  |
| Multi-system + shaders + achievements                  | **RetroArch** (bsnes-mercury core)   | RetroArch + Snes9x       |
| Preservation / archiving (academic reference)          | **ares** or **bsnes**                | Mesen-S                  |
| Speedrun (minimal input lag)                           | **ares** or **Snes9x** + run-ahead   | bsnes-mercury            |
| Online netplay / multiplayer                           | **RetroArch** (Snes9x or bsnes-mercury) | native Snes9x         |

---

## Glossary

- **Cycle-accurate**: cycle-by-cycle simulation of the processor, faithfully
  reproducing the temporal behavior of the hardware. Opposed to
  HLE approaches that simulate the results without reproducing the real
  timing.
- **HLE (High-Level Emulation)**: "high-level" emulation where certain
  parts of the hardware (audio chips, coprocessors) are replaced by
  faster but less accurate software equivalents.
- **Run-ahead**: a technique consisting of executing several frames
  ahead and then rolling back, to hide the input latency
  inherent to emulation.
- **Save state**: a save of the complete state of the emulated machine at a
  given instant, instantly restorable.
- **SNES expansion chips**: Super FX (Star Fox), SA-1 (Super Mario RPG),
  DSP-1/2/3/4 (Super Mario Kart, Pilotwings…), SPC7110 (Far East of Eden
  Zero), S-DD1 (Star Ocean), Cx4 (Mega Man X2/X3).
- **Mode 7**: a SNES graphics mode allowing rotation and scaling
  of a tile plane (the ground in F-Zero, Mario Kart…).
- **Libretro / RetroArch**: an open-source multi-system emulation framework
  using "cores" (the emulators themselves) loaded into a
  unified UI.

---

## Sources

- [Higan — Wikipedia](https://en.wikipedia.org/wiki/Higan_(emulator))
- [SNES emulators — Emulation General Wiki](https://emulation.gametechwiki.com/index.php/Super_Nintendo_Entertainment_System_emulators)
- [Mesen-S Documentation](https://www.mesen.ca/snes/docs/)
- [bsnes-mercury Accuracy — Libretro Docs](https://docs.libretro.com/library/bsnes_mercury_accuracy/)
- [bsnes-mercury Performance — Libretro Docs](https://docs.libretro.com/library/bsnes_mercury_performance/)
- [Best SNES Emulators 2026 — RetroDodo](https://retrododo.com/best-snes-emulators/)
- [ares vs Snes9x EX — Comparison](https://sugggest.com/compare/ares-formerly-higan-bsnes--vs-snes9x-ex)
- [Snes9x Alternatives — AlternativeTo](https://alternativeto.net/software/snes9x/)
- [no$sns — Problemkaputt (Martin Korth)](https://problemkaputt.de/sns.htm)
- [fullsnes.htm — Reference SNES Specification](https://problemkaputt.de/fullsnes.htm)
- [No$ — Emulation General Wiki](https://emulation.gametechwiki.com/index.php/No$)
