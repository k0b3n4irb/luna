# luna-cpu-upd96050

A **NEC uPD7725 / uPD96050** DSP core, extracted from the [luna]
emulator and usable on its own. These are the coprocessors on SNES
carts like Super Mario Kart and Pilotwings (DSP-1), Top Gear 3000
(DSP-4) and F1 ROC II (ST010).

[luna]: https://github.com/k0b3n4irb/luna

## What you get

- Both revisions behind one type: `Revision::Upd7725` (11-bit PC /
  10-bit RP / 8-bit DP) and `Revision::Upd96050` (14/11/11).
- The **host handshake as the hardware exposes it**: `read_dr` /
  `write_dr` implement the 8/16-bit DR protocol with the `DRS`/`DRC`
  latching, and `read_sr` reports `RQM`/`DRS` — so a consumer wires the
  chip to its bus without reimplementing the protocol.
- **No SNES glue**: no memory map, no cart-board logic. You load the
  firmware (`load_program` / `load_data`) and drive `exec()`.
- Save-state ready (`save_state` / `load_state`, `serde` + `bincode`).
- `#![forbid(unsafe_code)]`.

## Evidence

The core is validated by a **port-level differential against Mesen2**:
the complete DR command/result byte stream — the chip's entire
observable behaviour — is **byte-identical over 380 783 events** across
Super Mario Kart's title and demo race. (Mesen2's Lua API does not
expose the NecDsp registers, so the DR protocol stream *is* the oracle.)

## Usage

```rust
use luna_cpu_upd96050::{Revision, Upd96050};

let mut dsp = Upd96050::new(Revision::Upd7725);
dsp.load_program(&program_words); // from the cart's firmware image
dsp.load_data(&data_words);
dsp.power();

// Host side: feed a command, run, read the result back.
dsp.write_dr(0x00);
dsp.exec();
let status = dsp.read_sr();
let result = dsp.read_dr();
```

Firmware images are **not** included — they are copyrighted; dump your
own from a cart.

## License

MPL-2.0.
