# Coprocessors

Many SNES cartridges carried an extra chip on the board to do work the base
console could not. Luna implements the major ones, each as a faithful port of
its ares/Mesen2 reference:

| Chip | What it does | Example games |
|---|---|---|
| **[SA-1](coprocessors-sa1.md)** | A second, faster 65C816 + accelerators | Super Mario RPG, Kirby Super Star |
| **[Super FX / GSU](coprocessors-superfx.md)** | A RISC-like 3D/graphics processor | Star Fox, Doom, Yoshi's Island |
| **DSP-1** | A NEC uPD7725 math DSP (Mode 7 maths) | Super Mario Kart, Pilotwings |
| **[S-DD1](coprocessors-sdd1.md)** | A real-time graphics decompressor | Star Ocean, Street Fighter Alpha 2 |

The DSP-1 core is shared with `luna-cpu-upd96050`, a standalone NEC
uPD7725 / uPD96050 DSP — usable on its own like the other CPU cores.

Each coprocessor page covers its register model, how it is detected and wired
onto the bus, and where its implementation stands against the reference.

> A recurring lesson from porting these chips: translate the reference's
> **scheduling model**, not just its per-operation logic. See
> [Why "faithful port"](../method/faithful-port.md) for the cautionary tale.
