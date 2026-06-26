# Acknowledgements

Luna stands on the shoulders of the people and projects that turned accurate
SNES emulation into a shared, documented science. It would not exist without
them.

- **[ares](https://ares-emu.net/)** — the gold standard for SNES hardware
  accuracy, and Luna's primary reference for getting each subsystem right.

- **[Mesen2](https://github.com/SourMesen/Mesen2)** — an independent second
  source and an invaluable debugging companion. Its headless test runner is what
  makes Luna's self-contained differential validation possible.

- **[Tom Harte's processor tests](https://github.com/SingleStepTests)** — the
  exhaustive per-instruction test suites that pin the 65C816 and SPC700 down to
  the cycle.

- **The homebrew hardware-test ROM authors** — whose golden test ROMs reach
  corners of the hardware no commercial game touches.

- **The wider SNES emulation and reverse-engineering community** — decades of
  documentation, disassembly, and patient measurement of real silicon.

Thank you. 🙇

> Where this guide talks about "the hardware reference" or validating against "a
> reference emulator," these are the projects it means. Luna's accuracy is a
> reconstruction of their hard-won work — and of the real console underneath.
