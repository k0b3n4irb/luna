# luna-cpu-spc700

A cycle-accurate **SPC700** CPU core — the Sony processor inside the
SNES audio subsystem — extracted from the [luna] emulator and usable on
its own.

[luna]: https://github.com/k0b3n4irb/luna

## What you get

- **All 256 opcodes**, cycle-stepped: the core can be driven one *cycle*
  at a time (`step_cycle`) or one instruction at a time (`step`), so a
  consumer can interleave it with other components at bus-access
  granularity rather than instruction granularity.
- **Verified against [Tom Harte's SingleStepTests]** — 100% pass,
  including the per-cycle bus activity, not just the register results.
- **No SNES glue.** The core talks to a `SpcBus` trait you implement;
  it has no opinion about DSP registers, timers, or the CPU mailbox.
  Those live in the consumer.
- **Save-state ready**: the CPU state is `serde`-serialisable.
- One dependency (`serde`), no `unsafe` (`unsafe_code = "deny"`).

[Tom Harte's SingleStepTests]: https://github.com/SingleStepTests

## Usage

```rust
use luna_cpu_spc700::{Spc700, SpcBus};

struct Ram([u8; 0x10000]);

impl SpcBus for Ram {
    fn read(&mut self, addr: u16) -> u8 { self.0[addr as usize] }
    fn write(&mut self, addr: u16, value: u8) { self.0[addr as usize] = value; }
}

let mut cpu = Spc700::new();
let mut bus = Ram([0; 0x10000]);
cpu.pc = 0x0200;
let cycles = cpu.step(&mut bus); // one instruction, returns its cycle count
```

For cycle-granular interleaving use `step_cycle`, which returns after a
single cycle and resumes mid-instruction on the next call.

## Status

Used in production by luna, whose audio path is validated by PCM
goldens and a BRR decoder differential. The SPC700 row of luna's
[accuracy scorecard] is **A−**.

[accuracy scorecard]: https://github.com/k0b3n4irb/luna/blob/main/docs/accuracy_scorecard.md

## License

MPL-2.0.
