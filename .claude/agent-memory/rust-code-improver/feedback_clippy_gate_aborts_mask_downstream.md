---
name: clippy-gate-aborts-mask-downstream
description: Under `-D warnings` clippy aborts at the FIRST crate's error, masking findings in downstream crates — re-run per-crate or without -D to get the full picture.
metadata:
  type: feedback
---

When reporting clippy results for this workspace, never trust a single `cargo clippy --workspace ... -D warnings` run's count.

**Why:** `-D warnings` turns the first finding into a hard compile error, which aborts the build before downstream crates are checked. In the 2026-06-13 review, the workspace run reported "1 error" (use_self in luna-bus/sa1.rs) but two more `borrowed_box` errors (luna-apu/lib.rs, luna-core/snes.rs) plus a latent one (luna-api) were hiding behind that abort because luna-api/gui/core all depend on luna-bus.

**How to apply:** To get the COMPLETE clippy picture, either (a) re-run per-crate `cargo clippy -p <crate> ...`, or (b) run once WITHOUT `-D warnings` so everything compiles and all warnings surface, then treat each as gate-blocking. The recurring offenders here are `clippy::use_self` and `clippy::borrowed_box` (`&Box<[u8; N]>`) in the save-state serde helpers — the `Box<[u8; N]>` serde-skip pattern is copy-pasted across luna-apu/luna-core/luna-api, so a borrowed_box in one usually means it in all three.
