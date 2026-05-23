//! Cycle-accurate 65C816 CPU (Western Design Center) for the SNES main
//! processor.
//!
//! Dispatch via a `[fn(&mut Cpu, &mut B); 256]` jump-table; mid-instruction
//! accuracy via `Bus::io_cycle()`.
//!
//! See `ARCHITECTURE.md` §6.1 and §6.6.
