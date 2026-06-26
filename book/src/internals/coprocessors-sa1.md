# SA-1

The SA-1 is the most powerful of the SNES cartridge coprocessors: a
second 65C816 — the same CPU as the console's main processor, but
clocked roughly three times faster — placed on the cartridge alongside
fast on-chip RAM and a set of hardware accelerators. Games such as
Super Mario RPG, Kirby Super Star, and Kirby's Dream Land 3 use it to
offload work the main CPU could not do in time: bulk decompression,
sprite and character-data transforms, arithmetic (multiply / divide /
cumulative-sum), and bitmap/character-conversion helpers.

## What the chip provides

- **A second 65C816 core** running from the same cartridge ROM, able to
  execute its own program in parallel with the main CPU.
- **Internal work RAM (IRAM)** shared between the two processors, plus a
  banked view of the cartridge's battery-backed RAM (BW-RAM), with
  per-side banking so each processor can address a different bank.
- **A DMA unit** for moving and reformatting data, including a
  character-conversion mode that repacks linear bitmap data into the
  SNES planar tile format.
- **Arithmetic accelerators** (signed/unsigned multiply, divide, and a
  running cumulative-sum register) the main CPU reads back through MMIO.
- **Memory-protection registers** that arbitrate which processor owns
  which region, so the two cores do not corrupt each other's working
  set.

## How luna implements it

luna models the SA-1 as a chip-side 65C816 instance driven by a
cooperative scheduler alongside the main CPU: the two cores advance in
lockstep at bus-access granularity, so neither runs ahead of the other.
The shared IRAM, the per-side BW-RAM banking, the B-bus register window
(`$2200-$23FF`), the arithmetic units, and the DMA path — including the
character-conversion mode — are all present. During long main-CPU DMA
bursts the SA-1 is stepped per byte transferred, so it never sees a
frozen timeline while the bus is held.

The implementation follows the hardware reference for register layout,
the memory-protection semantics, and the run cadence. You can observe
the SA-1's MMIO traffic live with `luna state --sa1-log <path>`, which
records every access in the `$2200-$23FF` window.
