---
description: Fetch ares + Mesen2 reference sources for a SNES subsystem into /tmp/
argument-hint: "<subsystem> (e.g. ppu, dma, cpu, sa1, dsp)"
allowed-tools: Bash(curl *), Bash(gh *), Bash(mkdir *), Bash(ls *), Read
---

Implements the first step of the reference-first workflow in
`.claude/rules/reference-first.md`: fetch the canonical files from
both reference emulators into `/tmp/` so they can be diffed and read.

## Subsystem mapping

| `$ARGUMENTS` | ares directory | Mesen2 file |
|---|---|---|
| `ppu`  | `ares/sfc/ppu/`  | `Core/SNES/SnesPpu.cpp` + `SnesPpuTypes.h` |
| `dma`  | `ares/sfc/cpu/dma.cpp` + `timing.cpp` | `Core/SNES/DmaController.cpp` |
| `cpu`  | `ares/sfc/cpu/`  | `Core/SNES/Cpu/SnesCpu.cpp` |
| `sa1`  | `ares/sfc/coprocessor/sa1/` | `Core/SNES/Coprocessors/SA1/` |
| `dsp`  | `ares/sfc/dsp/` + `ares/sfc/smp/` | `Core/SNES/SpcRam.cpp` + `Spc.cpp` + `Dsp.cpp` |

## Workflow

1. Create `/tmp/ares/<subsys>/` and `/tmp/mesen2/<subsys>/`.
2. Discover the ares file list via the GitHub API:
   `gh api repos/ares-emulator/ares/contents/ares/sfc/<subsys> --jq '.[].name'`.
3. `curl -s` each file into the local directory.
4. Repeat for Mesen2.
5. Print a one-line summary per file: name + line count + 1-line purpose
   guess from the first non-comment line.

When the fetch is complete, the next step (per the reference-first
rule) is to read the files thoroughly and produce a spec at
`/tmp/<subsys>_reference.md`. Use the `general-purpose` agent for
that if the surface is large.

If `$ARGUMENTS` is empty, ask the user which subsystem to fetch.
