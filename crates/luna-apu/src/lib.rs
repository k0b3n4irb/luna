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

    // ------------- Timers -------------
    /// Reload values at `$FA` (T0), `$FB` (T1), `$FC` (T2). A value
    /// of `0` is treated as `256` per the SPC700 spec.
    pub timer_reload: [u8; 3],
    /// 4-bit output counters at `$FD` (T0), `$FE` (T1), `$FF` (T2).
    /// Increment when the timer crosses its reload; reset to 0 when
    /// the CPU reads the register.
    pub timer_output: [u8; 3],
    /// Internal divider counter for each timer.
    pub timer_internal: [u16; 3],
    /// `true` if `$F1.bits[0..2]` enabled the corresponding timer.
    /// A 0→1 transition resets the internal counter on real HW; we
    /// honour that.
    pub timer_enabled: [bool; 3],
    /// Sub-tick counter for the timer base clocks. T0/T1 tick once
    /// every 128 SPC cycles; T2 ticks once every 16 SPC cycles. We
    /// derive both from the same counter rather than running them
    /// separately.
    pub timer_subdivider: u32,
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
            timer_reload: [0; 3],
            timer_output: [0; 3],
            timer_internal: [0; 3],
            timer_enabled: [false; 3],
            timer_subdivider: 0,
        };
        // Reset the SPC700 — reads $FFFE/$FFFF for the PC vector,
        // which the IPL ROM populates as $FFC0.
        let mut bus = ApuBusView {
            aram: &mut apu.aram,
            to_spc_ports: &apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
        };
        apu.cpu.reset(&mut bus);
        apu
    }

    /// Tick the three SPC timers by `spc_cycles` of headroom.
    ///
    /// T0 / T1 base clock: 8 kHz = one tick every 128 SPC cycles.
    /// T2 base clock: 64 kHz = one tick every 16 SPC cycles.
    ///
    /// On a tick, if the timer is enabled, the internal counter
    /// increments; when it reaches the reload value (0 = 256), the
    /// 4-bit output counter wraps-increments and the internal counter
    /// resets.
    fn tick_timers(&mut self, spc_cycles: u32) {
        let before = self.timer_subdivider;
        self.timer_subdivider = self.timer_subdivider.wrapping_add(spc_cycles);
        let after = self.timer_subdivider;

        // T2 ticks at the 16-cycle boundary.
        let t2_ticks = (after / 16) - (before / 16);
        for _ in 0..t2_ticks {
            self.tick_one_timer(2);
        }
        // T0 and T1 tick at the 128-cycle boundary.
        let slow_ticks = (after / 128) - (before / 128);
        for _ in 0..slow_ticks {
            self.tick_one_timer(0);
            self.tick_one_timer(1);
        }
    }

    /// Advance one timer (0, 1, or 2) by one base-clock tick.
    fn tick_one_timer(&mut self, idx: usize) {
        if !self.timer_enabled[idx] {
            return;
        }
        self.timer_internal[idx] = self.timer_internal[idx].wrapping_add(1);
        let target = if self.timer_reload[idx] == 0 {
            256
        } else {
            u16::from(self.timer_reload[idx])
        };
        if self.timer_internal[idx] >= target {
            self.timer_internal[idx] = 0;
            self.timer_output[idx] = (self.timer_output[idx] + 1) & 0x0F;
        }
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
    /// deficit, ticking the three SPC timers between steps.
    pub fn step(&mut self, mclk: u32) {
        self.mclk_deficit = self.mclk_deficit.saturating_add(mclk);
        while self.mclk_deficit >= MASTER_CYCLES_PER_SPC_STEP {
            if self.cpu.stopped {
                break;
            }
            self.mclk_deficit -= MASTER_CYCLES_PER_SPC_STEP;
            // Approximate: an SPC instruction averages ~4 SPC cycles.
            // That gives T0/T1 a tick every 128 / 4 = 32 instructions
            // and T2 a tick every 16 / 4 = 4 instructions.
            self.tick_timers(4);
            let mut bus = ApuBusView {
                aram: &mut self.aram,
                to_spc_ports: &self.to_spc_ports,
                to_cpu_ports: &mut self.to_cpu_ports,
                control: &mut self.control,
                timer_reload: &mut self.timer_reload,
                timer_output: &mut self.timer_output,
                timer_internal: &mut self.timer_internal,
                timer_enabled: &mut self.timer_enabled,
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
    timer_reload: &'a mut [u8; 3],
    timer_output: &'a mut [u8; 3],
    timer_internal: &'a mut [u16; 3],
    timer_enabled: &'a mut [bool; 3],
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
            // $FA-$FC — timer reload values. Write-only on real HW,
            // reads return 0.
            0x00FA..=0x00FC => 0,
            // $FD-$FF — timer outputs. 4-bit counters that **clear on
            // read** — the SPC driver typically does
            // `MOV A,$FD / BNE somewhere` once per its main loop.
            0x00FD..=0x00FF => {
                let idx = (addr - 0x00FD) as usize;
                let v = self.timer_output[idx];
                self.timer_output[idx] = 0;
                v
            }
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
            // visibility (we don't yet model un-mapping). Bits 0-2
            // enable timers T0/T1/T2; a 0→1 transition resets the
            // corresponding internal counter on real HW.
            0x00F1 => {
                for i in 0..3 {
                    let bit = 1u8 << i;
                    let was_enabled = self.timer_enabled[i];
                    let now_enabled = value & bit != 0;
                    if now_enabled && !was_enabled {
                        self.timer_internal[i] = 0;
                        self.timer_output[i] = 0;
                    }
                    self.timer_enabled[i] = now_enabled;
                }
                *self.control = value;
            }
            // $F2 — DSP index port, stubbed.
            0x00F2 => {}
            // $F3 — DSP data port, stubbed.
            0x00F3 => {}
            // $F4-$F7 — mailbox TO the main CPU.
            0x00F4..=0x00F7 => self.to_cpu_ports[(addr - 0x00F4) as usize] = value,
            // $F8-$F9 — auxiliary RAM-mapped regs. Store in ARAM so
            // reads come back consistent.
            0x00F8..=0x00F9 => self.aram[addr as usize] = value,
            // $FA-$FC — timer reload (target) values.
            0x00FA..=0x00FC => {
                let idx = (addr - 0x00FA) as usize;
                self.timer_reload[idx] = value;
            }
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
    fn timer_t2_increments_output_when_enabled() {
        let mut apu = Apu::new();
        // Manually configure: enable T2 with reload = 1 (tick every
        // 16 SPC cycles), bypassing the SPC700 — we want to test the
        // timer math, not the SPC's writes.
        apu.timer_reload[2] = 1;
        apu.timer_enabled[2] = true;
        // Tick 16 SPC cycles ×  4 = 64 cycles of headroom should give
        // 64/16 = 4 T2 ticks → output counter reaches 4.
        for _ in 0..4 {
            apu.tick_timers(16);
        }
        assert_eq!(apu.timer_output[2], 4);
    }

    #[test]
    fn timer_t0_t1_tick_at_128_cycle_boundary() {
        let mut apu = Apu::new();
        apu.timer_reload[0] = 1;
        apu.timer_reload[1] = 1;
        apu.timer_enabled[0] = true;
        apu.timer_enabled[1] = true;
        // 128 cycles → 1 T0/T1 tick.
        apu.tick_timers(128);
        assert_eq!(apu.timer_output[0], 1);
        assert_eq!(apu.timer_output[1], 1);
    }

    #[test]
    fn timer_reload_zero_means_256() {
        let mut apu = Apu::new();
        apu.timer_reload[2] = 0; // = 256
        apu.timer_enabled[2] = true;
        // T2 ticks every 16 SPC cycles, so 256 ticks = 4096 SPC cycles.
        // After 4095 cycles, the output should still be 0.
        apu.tick_timers(16 * 255);
        assert_eq!(apu.timer_output[2], 0);
        // One more tick should cross the threshold.
        apu.tick_timers(16);
        assert_eq!(apu.timer_output[2], 1);
    }

    #[test]
    fn timer_output_clears_on_read_via_bus() {
        let mut apu = Apu::new();
        apu.timer_reload[2] = 1;
        apu.timer_enabled[2] = true;
        apu.tick_timers(16 * 3);
        assert_eq!(apu.timer_output[2], 3);
        // Construct a temporary bus view and read $FF.
        {
            let mut bus = ApuBusView {
                aram: &mut apu.aram,
                to_spc_ports: &apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
            };
            assert_eq!(bus.read(0x00FF), 3);
            assert_eq!(bus.read(0x00FF), 0, "second read should be cleared");
        }
    }

    #[test]
    fn writing_control_register_toggles_timer_enables() {
        let mut apu = Apu::new();
        let mut bus = ApuBusView {
            aram: &mut apu.aram,
            to_spc_ports: &apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
        };
        // Enable all 3 timers via $F1.
        bus.write(0x00F1, 0x07);
        assert!(apu.timer_enabled[0]);
        assert!(apu.timer_enabled[1]);
        assert!(apu.timer_enabled[2]);
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
