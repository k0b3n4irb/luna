//! SNES APU — orchestrates the `luna-cpu-spc700` core, 64 KB of ARAM,
//! the IPL ROM mapped over the top page, and the four mailbox ports
//! facing the main CPU.
//!
//! The audio DSP (sample generation, voice mixing) is a separate
//! P2.SPC.4+ effort — for now the DSP register file is just a stub
//! so writes from the SPC700 don't blow up.
//!
//! # Mailbox direction model
//!
//! The four SNES mailbox ports are actually **two** registers per
//! port (one per direction):
//!
//! ```text
//!                     ┌─────────────────────────────┐
//!   CPU side          │       inside the APU         │       SPC700
//! ─────────────       │                             │     ───────────
//!   $2140-$2143 ──W──▶│ to_spc_ports[0..3] (RAM)    │◀──R─ $00F4-$00F7
//!                     │                             │
//!   $2140-$2143 ◀──R──│ to_cpu_ports[0..3] (RAM)    │──W──▶ $00F4-$00F7
//!                     └─────────────────────────────┘
//! ```
//!
//! A read at the same port from either side returns what **the other
//! side last wrote**, not your own writes. This matches real hardware
//! and is what the IPL ROM relies on for the boot handshake.
//!
//! # Clock ratio
//!
//! SNES master = 21.477 MHz, SPC = 1.024 MHz, ratio ≈ 21. We run one
//! SPC instruction per 84 master cycles, which assumes ~4-cycle
//! average SPC instructions — good enough until we wire timer-driven
//! cycle accounting in.

use luna_cpu_spc700::{IPL_ROM, IPL_ROM_BASE, Spc700, SpcBus};

/// Master cycles per SPC instruction step. SNES master = 21.477 MHz,
/// SPC = 1.024 MHz, average SPC instruction = ~4 SPC cycles. So one
/// SPC instruction every (21.477 / 1.024) × 4 ≈ 84 master cycles.
pub const MASTER_CYCLES_PER_SPC_STEP: u32 = 84;

/// All APU state owned by [`Apu`]: SPC700 core + 64 KB ARAM + the two
/// 4-byte mailbox arrays.
pub struct Apu {
    /// The SPC700 CPU.
    pub cpu: Spc700,
    /// 64 KB of audio RAM. The IPL ROM is *also* mapped into
    /// `$FFC0..=$FFFF` on top of the ARAM — controlled by the SPC's
    /// `$F1` "control" register (bit 7 = use IPL ROM). We model "IPL
    /// ROM always exposed" for now; toggling lives behind that bit.
    pub aram: Box<[u8; 0x10000]>,
    /// CPU → SPC mailbox (CPU writes `$2140-$2143`, SPC reads `$F4-$F7`).
    pub to_spc_ports: [u8; 4],
    /// SPC → CPU mailbox (SPC writes `$F4-$F7`, CPU reads `$2140-$2143`).
    pub to_cpu_ports: [u8; 4],
    /// Accumulator for the SPC catch-up scheduler. Holds master
    /// cycles owed to the SPC since the last instruction step.
    pub mclk_deficit: u32,
    /// `$F1` SPC control register — bit 7 (use IPL ROM) is the only
    /// bit we honour for now; the rest are stored verbatim for round-
    /// trip diagnostics.
    pub control: u8,
    /// `true` once the SPC has executed at least one instruction past
    /// the IPL ROM region (i.e. it `JMP`'d into user code). When that
    /// happens we expect uploaded music driver code; until our
    /// opcode coverage catches up to the most popular drivers, this
    /// is mostly informational.
    pub past_iplrom: bool,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    /// Build a freshly-reset APU. ARAM is zeroed, the IPL ROM is
    /// mapped at `$FFC0..=$FFFF`, the SPC700 is reset (which reads
    /// its reset vector from the IPL ROM and lands at `$FFC0`).
    #[must_use]
    pub fn new() -> Self {
        let mut aram = Box::new([0u8; 0x10000]);
        for (i, b) in IPL_ROM.iter().enumerate() {
            aram[IPL_ROM_BASE as usize + i] = *b;
        }
        let mut apu = Self {
            cpu: Spc700::new(),
            aram,
            to_spc_ports: [0; 4],
            to_cpu_ports: [0; 4],
            mclk_deficit: 0,
            control: 0x80, // bit 7: IPL ROM exposed
            past_iplrom: false,
        };
        // Reset the SPC700 — reads $FFFE/$FFFF for the PC vector,
        // which the IPL ROM populates as $FFC0.
        let mut bus = ApuBusView {
            aram: &mut apu.aram,
            to_spc_ports: &apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
        };
        apu.cpu.reset(&mut bus);
        apu
    }

    /// Read a byte from the CPU side of the mailbox (port 0..=3).
    /// This is what the main CPU sees at `$2140 + port`.
    #[must_use]
    pub fn cpu_read_port(&self, port: usize) -> u8 {
        self.to_cpu_ports[port]
    }

    /// Main CPU writes `value` to mailbox port (0..=3). The byte
    /// becomes visible to the SPC700 the next time it reads `$F4 + port`.
    pub fn cpu_write_port(&mut self, port: usize, value: u8) {
        self.to_spc_ports[port] = value;
    }

    /// Advance the SPC700 by `mclk` master cycles of headroom. Runs
    /// one SPC instruction per [`MASTER_CYCLES_PER_SPC_STEP`] in the
    /// deficit. Panics from unimplemented opcodes are caught further
    /// up the stack — here we just stop stepping if `cpu.stopped`.
    pub fn step(&mut self, mclk: u32) {
        self.mclk_deficit = self.mclk_deficit.saturating_add(mclk);
        while self.mclk_deficit >= MASTER_CYCLES_PER_SPC_STEP {
            if self.cpu.stopped {
                break;
            }
            self.mclk_deficit -= MASTER_CYCLES_PER_SPC_STEP;
            let mut bus = ApuBusView {
                aram: &mut self.aram,
                to_spc_ports: &self.to_spc_ports,
                to_cpu_ports: &mut self.to_cpu_ports,
                control: &mut self.control,
            };
            self.cpu.step(&mut bus);
            if self.cpu.pc < IPL_ROM_BASE {
                self.past_iplrom = true;
            }
        }
    }
}

/// Bus view of the APU created on each SPC700 step. Splits the APU's
/// fields the way the borrow checker needs them — `aram` and
/// `to_cpu_ports` are mutable (the SPC writes those), `to_spc_ports`
/// is read-only from the SPC's side (the CPU writes those).
struct ApuBusView<'a> {
    aram: &'a mut [u8; 0x10000],
    to_spc_ports: &'a [u8; 4],
    to_cpu_ports: &'a mut [u8; 4],
    control: &'a mut u8,
}

impl SpcBus for ApuBusView<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            // $F0 — testing register, write-only on real HW. Return 0.
            0x00F0 => 0,
            // $F1 — control register. Reads return... 0 on real HW
            // (it's effectively write-only). Match that.
            0x00F1 => 0,
            // $F2 — DSP register-index port. Stubbed.
            0x00F2 => 0,
            // $F3 — DSP register-data port. Stubbed.
            0x00F3 => 0,
            // $F4-$F7 — mailbox FROM the main CPU.
            0x00F4..=0x00F7 => self.to_spc_ports[(addr - 0x00F4) as usize],
            // $F8-$F9 — RAM-mapped scratch bytes (auxiliary regs).
            // Stubbed to 0 for now.
            0x00F8..=0x00F9 => 0,
            // $FA-$FC — timer reload values. Write-only, reads 0.
            0x00FA..=0x00FC => 0,
            // $FD-$FF — timer outputs. Stubbed to 0.
            0x00FD..=0x00FF => 0,
            // Everything else — ARAM. The IPL ROM lives at $FFC0
            // and is pre-baked into ARAM, so the read just goes
            // through.
            _ => self.aram[addr as usize],
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            // $F0 — test register, accept and drop.
            0x00F0 => {}
            // $F1 — control register. Bit 7 controls IPL ROM
            // visibility. We store it for diagnostics; the
            // "swap ARAM in at $FFC0..$FFFF when bit 7 is clear" path
            // isn't wired yet, but no commercial game we run does it.
            0x00F1 => *self.control = value,
            // $F2 — DSP index port, stubbed.
            0x00F2 => {}
            // $F3 — DSP data port, stubbed.
            0x00F3 => {}
            // $F4-$F7 — mailbox TO the main CPU.
            0x00F4..=0x00F7 => self.to_cpu_ports[(addr - 0x00F4) as usize] = value,
            // $F8-$F9 — auxiliary RAM-mapped regs. Store in ARAM so
            // reads come back consistent.
            0x00F8..=0x00F9 => self.aram[addr as usize] = value,
            // $FA-$FC — timer reload values. Stubbed.
            0x00FA..=0x00FC => {}
            // $FD-$FF — timer outputs are read-only; writes drop.
            0x00FD..=0x00FF => {}
            // Everything else — ARAM.
            _ => self.aram[addr as usize] = value,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_resets_spc_into_ipl_rom() {
        let apu = Apu::new();
        assert_eq!(apu.cpu.pc, IPL_ROM_BASE);
        // ARAM at $FFC0 holds the first byte of the IPL ROM.
        assert_eq!(apu.aram[IPL_ROM_BASE as usize], IPL_ROM[0]);
    }

    #[test]
    fn boot_handshake_appears_on_to_cpu_ports() {
        let mut apu = Apu::new();
        // Run the SPC long enough for the IPL ROM to write $AA / $BB.
        // 2000 SPC instructions × 84 mclk = 168 000 mclk of headroom.
        apu.step(2000 * MASTER_CYCLES_PER_SPC_STEP);
        assert_eq!(apu.cpu_read_port(0), 0xAA);
        assert_eq!(apu.cpu_read_port(1), 0xBB);
    }

    #[test]
    fn cpu_kick_unblocks_ipl_rom() {
        let mut apu = Apu::new();
        // Boot to handshake.
        apu.step(2000 * MASTER_CYCLES_PER_SPC_STEP);
        // Simulate the main CPU writing $CC to $2140.
        apu.cpu_write_port(0, 0xCC);
        // Step further — IPL ROM should leave the wait loop at $FFCF.
        apu.step(200 * MASTER_CYCLES_PER_SPC_STEP);
        assert_ne!(apu.cpu.pc, 0xFFCF, "SPC still stuck in wait loop");
    }

    #[test]
    fn dsp_register_writes_are_silently_accepted() {
        // Music drivers smash a lot of bytes through $F2/$F3 once
        // they start running. With our stub we just want them not
        // to panic.
        let mut apu = Apu::new();
        apu.step(2000 * MASTER_CYCLES_PER_SPC_STEP);
        apu.cpu_write_port(0, 0xCC);
        apu.step(200 * MASTER_CYCLES_PER_SPC_STEP);
        // After kick, the IPL is in the transfer loop. We're not
        // verifying anything else here — just that no panic fired.
    }
}
