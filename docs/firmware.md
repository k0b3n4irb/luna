# Coprocessor firmware (DSP-1)

Some SNES cartridges carry an extra chip whose program ("microcode") is **not**
captured by a normal ROM dump — it lives inside the chip and is copyrighted, so
luna (like ares / Mesen2) cannot bundle it. **You supply it once.**

Today this applies to **DSP-1** titles — Super Mario Kart, Pilotwings, Suzuka 8
Hours, Ballz 3D, … — which need **`dsp1b.rom`** (~8 KB).

## Where luna looks (first hit wins)

1. **Embedded** in the ROM dump — some `.sfc` files append the 8 KB firmware.
2. **Beside the game file** — a `dsp1b.rom` in the same folder as the `.sfc`.
3. **luna's firmware folder** — `~/.config/luna/firmware/dsp1b.rom`
   (`<config>/luna/firmware`, per platform).

## Supplying it

- **GUI** — if none is found, luna pops a dialog to locate `dsp1b.rom`; once
  picked it's installed to the firmware folder and the ROM reloads.
- **CLI** — `luna state --dsp1-rom <path> <game>` installs it (persists for
  future runs).

Without firmware the game still runs, but the DSP stays inert (Mode 7 graphics
are wrong) and the CLI prints a clear message. Games with no coprocessor — or
SA-1 / Super FX titles — need no firmware.
