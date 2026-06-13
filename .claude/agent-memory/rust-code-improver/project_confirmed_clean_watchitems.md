---
name: project-confirmed-clean-watchitems
description: Subsystem spots that LOOK improvable but were verified correct/deliberate in the 2026-06-13 full review — do not re-flag.
metadata:
  type: project
---

Verified-correct-and-deliberate spots (2026-06-13 full-workspace review). Do NOT re-flag these as bugs:

- PPU `$2104/$2138` OAM-port high table `0x200 | (addr & 0x1F)` (luna-ppu memory.rs ~478,507) is the deliberate ares/Mesen2 indexing. The RENDERER flat-OAM path correctly uses `% 0x220` via `Oam::peek/poke`. Two different paths, both correct. See [[feedback_buffer_mask_vs_modulo]].
- SuperFX is the ONLY mapper using `& (size-1)`; it pads ROM/RAM to a power of two first (`round_up_pow2`), so the mask is valid and matches ares' padded-image model. LoROM/HiROM/ExHiROM use `if off < rom.len()` open-bus and SRAM uses `% len()` — non-power-of-two safe.
- DSP firmware detect `rom.len() & 0x7FFF == 0x2000` (luna-cartridge) is a valid power-of-two mask (0x8000-1), = `% 0x8000`. Not a non-power-of-two hazard.
- WRAM-port `& 0x1FFFF` is valid: WRAM is 0x20000 (power of two), mask == modulo.
- `read_direct_x` (65c816 (dp,X) emulation wrap) deliberately follows SingleStepTests over ares — see [[project_65c816_tomharte_wins_over_ares]].
- `state()` in luna-api is heavy by design but gated to the open Registers debug window, never per-frame — the GUI uses cheap cpu_state/spc700_state accessors. Not a perf bug.

Genuine WATCH-ITEMS (not bugs, but where to aim a differential harness if symptoms appear):
- Hi-res Mode 5/6 tilemap column masking: luna masks the tile index `& (cols-1)` (renderer.rs ~1287); ares masks the pixel offset `& (hsize-1)` BEFORE shifting, with separate hscreen/vscreen for the 0x20 overflow. May diverge for large hi-res tilemaps w/ screen-size bits. Guarded by RPM Racing smoke.
- SA-1 BW-RAM 2bpp/4bpp bitmap-format view at banks $60-$6F appears unimplemented (sa1.rs ~905-919 handles only the $6000-$7FFF window + $40-$4F linear). Rarely exercised.
