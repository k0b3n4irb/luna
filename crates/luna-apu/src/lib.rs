//! SNES APU — orchestrates the `luna-cpu-spc700` core, 64 KB of ARAM,
//! the IPL ROM mapped over the top page, and the four mailbox ports
//! facing the main CPU.
//!
//! The audio DSP (sample generation, voice mixing) is the
//! cycle-accurate ares port in [`dsp`]; `$F2`/`$F3` route through it.
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

/// Cycle-accurate ares port of the S-DSP — the live audio path. See
/// `dsp.rs` for the 1-for-1 transliteration of `ares/sfc/dsp/*`.
pub mod dsp;

/// Nominal master cycles per SPC instruction (~4 SPC cycles × the
/// 20.97 master/SPC ratio). Since Phase 2 this is **no longer** the
/// scheduler quantum — `Apu::step` charges each instruction its real
/// per-opcode cost — but it is kept as a convenience multiplier for
/// tests that want "≈N instructions of headroom".
pub const MASTER_CYCLES_PER_SPC_STEP: u32 = 84;

/// NTSC SNES master clock (Hz) — the CPU/PPU timebase.
pub const MASTER_CLOCK_HZ: u64 = 21_477_272;

/// SPC700 / S-DSP clock (Hz): the 24.576 MHz APU crystal ÷ 24.
pub const SPC_CLOCK_HZ: u64 = 1_024_000;

/// SPC cycles per audio sample (32 kHz output at 1.024 MHz).
pub const SPC_CYCLES_PER_SAMPLE: u32 = 32;

/// Maximum number of stereo samples buffered in `audio_queue`. The
/// host audio backend drains it each frame; bursts beyond this cap are
/// dropped to keep emulator-side memory bounded. ~16k samples = 512 ms
/// at 32 kHz, plenty of headroom for normal frame cadence.
pub const AUDIO_QUEUE_CAPACITY: usize = 16384;

/// All APU state owned by [`Apu`]: SPC700 core + 64 KB ARAM + the two
/// 4-byte mailbox arrays.
pub struct Apu {
    /// The SPC700 CPU.
    pub cpu: Spc700,
    /// Cycle-accurate ares-port S-DSP. Owns its own register file
    /// (mirrored into `dsp_regs` for legacy introspection), Voice[8],
    /// Echo, Noise, BRR, Latch, Clock, `MainVol` state. `tick_voices`
    /// drives one `dsp.main()` per 32 SPC cycles → one stereo sample.
    pub dsp: dsp::Dsp,
    /// 64 KB of audio RAM. The IPL ROM is *also* mapped into
    /// `$FFC0..=$FFFF` on top of the ARAM — controlled by the SPC's
    /// `$F1` "control" register (bit 7 = use IPL ROM). We model "IPL
    /// ROM always exposed" for now; toggling lives behind that bit.
    pub aram: Box<[u8; 0x10000]>,
    /// CPU → SPC mailbox (CPU writes `$2140-$2143`, SPC reads `$F4-$F7`).
    pub to_spc_ports: [u8; 4],
    /// SPC → CPU mailbox (SPC writes `$F4-$F7`, CPU reads `$2140-$2143`).
    pub to_cpu_ports: [u8; 4],
    /// Fractional master→SPC cycle accumulator: holds
    /// `Σ(master cycles) × SPC_CLOCK_HZ` modulo `MASTER_CLOCK_HZ`, so
    /// the long-run SPC rate is exactly `SPC_CLOCK_HZ / MASTER_CLOCK_HZ`
    /// with zero drift.
    spc_cycle_num: u64,
    /// Signed SPC-cycle budget carried between `step` calls. Negative
    /// means the last instruction overran; the debt is repaid from the
    /// next batch of converted cycles.
    spc_cycle_debt: i64,
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

    // ------------- DSP (audio synth) — owned by `dsp` -------------
    /// Last value written to `$F2` — the DSP register-index port.
    /// `$F3` reads/writes `dsp.registers[dsp_index & 0x7F]` via
    /// [`dsp::Dsp::read`] / [`dsp::Dsp::write`], which fans out the
    /// data to the relevant Voice/Echo/Noise/etc field.
    pub dsp_index: u8,
    /// Mixed L audio output for the most recent sample produced by
    /// [`dsp::Dsp::main`]. Consumers (audio backend) prefer
    /// [`Self::audio_queue`] for proper sample-rate output, but a few
    /// debug paths read this last-sample snapshot.
    pub audio_left: i16,
    /// Mixed R audio output — companion to [`Self::audio_left`].
    pub audio_right: i16,
    /// FIFO of stereo PCM samples ready for the host audio backend
    /// to consume. Sized to a few frames at 32 kHz so brief audio
    /// underruns don't cause sustained drift; the host drains it on
    /// every UI frame.
    pub audio_queue: std::collections::VecDeque<(i16, i16)>,
    /// SPC cycles owed to the audio-sample tick (32-cycle base
    /// clock). Drives both ADSR rate progression and the per-voice
    /// position counter.
    sample_tick_deficit: u32,

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
        let mut aram: Box<[u8; 0x10000]> = vec![0u8; 0x10000]
            .into_boxed_slice()
            .try_into()
            .expect("64 KB slice into fixed array");
        for (i, b) in IPL_ROM.iter().enumerate() {
            aram[IPL_ROM_BASE as usize + i] = *b;
        }
        let mut apu = Self {
            cpu: Spc700::new(),
            dsp: dsp::Dsp::new(),
            aram,
            to_spc_ports: [0; 4],
            to_cpu_ports: [0; 4],
            spc_cycle_num: 0,
            spc_cycle_debt: 0,
            control: 0x80, // bit 7: IPL ROM exposed
            past_iplrom: false,
            dsp_index: 0,
            audio_left: 0,
            audio_right: 0,
            audio_queue: std::collections::VecDeque::with_capacity(8192),
            sample_tick_deficit: 0,
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
            to_spc_ports: &mut apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
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

    /// Tick the per-voice envelope state machines. We accumulate SPC
    /// cycles in `sample_tick_deficit`; each time it crosses a
    /// 32-cycle boundary the DSP advances by one 32 kHz audio sample
    /// — which is when the ADSR phase machine actually moves and
    /// the per-voice position counter increments.
    fn tick_voices(&mut self, spc_cycles: u32) {
        self.sample_tick_deficit = self.sample_tick_deficit.saturating_add(spc_cycles);
        while self.sample_tick_deficit >= SPC_CYCLES_PER_SAMPLE {
            self.sample_tick_deficit -= SPC_CYCLES_PER_SAMPLE;
            // One full 32-step DSP macro pipeline → one stereo sample.
            // `dsp.main` drains internal latches (KON, KOFF, ENDX, BRR
            // step, envelope, gaussian, echo FIR) and writes a single
            // (i16,i16) into `dsp.last_sample`.
            let (l, r) = self.dsp.main(&mut self.aram);
            if self.audio_queue.len() < AUDIO_QUEUE_CAPACITY {
                self.audio_queue.push_back((l, r));
            }
            self.audio_left = l;
            self.audio_right = r;
        }
    }

    /// Advance every voice's ADSR state by one 32 kHz audio sample.
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
    pub const fn cpu_read_port(&self, port: usize) -> u8 {
        self.to_cpu_ports[port]
    }

    /// Main CPU writes `value` to mailbox port (0..=3). The byte
    /// becomes visible to the SPC700 the next time it reads `$F4 + port`.
    pub const fn cpu_write_port(&mut self, port: usize, value: u8) {
        self.to_spc_ports[port] = value;
    }

    /// Drain up to `max` queued stereo samples into `out`, in oldest-
    /// first order. The caller (audio backend) typically calls this
    /// every UI frame and pushes results into a SPSC ring read by
    /// the cpal callback.
    pub fn drain_audio(&mut self, out: &mut Vec<(i16, i16)>, max: usize) {
        let n = self.audio_queue.len().min(max);
        for _ in 0..n {
            if let Some(s) = self.audio_queue.pop_front() {
                out.push(s);
            }
        }
    }

    /// Snapshot of the most recent stereo audio sample produced by
    /// the DSP. Returns `(left, right)` 16-bit signed PCM at 32 kHz.
    /// Future audio backends can consume this in a tight loop;
    /// today it's mostly a sanity-check probe.
    #[must_use]
    pub const fn audio_sample(&self) -> (i16, i16) {
        (self.audio_left, self.audio_right)
    }

    /// Advance the SPC700 by `mclk` master cycles. Converts master
    /// cycles into an SPC-cycle budget at the exact 21.477 MHz : 1.024
    /// MHz ratio (with fractional carry), then runs SPC instructions —
    /// each charged its **actual** per-opcode cost (incl. the taken-
    /// branch penalty) — until the budget is spent. Timers and the DSP
    /// voice clock advance by the same real per-instruction cycles.
    pub fn step(&mut self, mclk: u32) {
        // Convert this batch of master cycles into whole SPC cycles,
        // carrying the fractional remainder so the rate has no drift.
        self.spc_cycle_num += u64::from(mclk) * SPC_CLOCK_HZ;
        let gained = (self.spc_cycle_num / MASTER_CLOCK_HZ) as i64;
        self.spc_cycle_num %= MASTER_CLOCK_HZ;
        let mut budget = self.spc_cycle_debt + gained;
        while budget > 0 {
            if self.cpu.stopped {
                // SPC halted: drop the budget rather than let it pile up
                // into a burst if it ever un-stops.
                budget = 0;
                break;
            }
            let mut bus = ApuBusView {
                aram: &mut self.aram,
                to_spc_ports: &mut self.to_spc_ports,
                to_cpu_ports: &mut self.to_cpu_ports,
                control: &mut self.control,
                dsp_index: &mut self.dsp_index,
                dsp: &mut self.dsp,
                timer_reload: &mut self.timer_reload,
                timer_output: &mut self.timer_output,
                timer_internal: &mut self.timer_internal,
                timer_enabled: &mut self.timer_enabled,
            };
            let cycles = u32::from(self.cpu.step(&mut bus));
            budget -= i64::from(cycles);
            self.tick_timers(cycles);
            self.tick_voices(cycles);
            if self.cpu.pc < IPL_ROM_BASE {
                self.past_iplrom = true;
            }
        }
        self.spc_cycle_debt = budget;
    }
}

/// Bus view of the APU created on each SPC700 step. Splits the APU's
/// fields the way the borrow checker needs them — `aram` and
/// `to_cpu_ports` are mutable (the SPC writes those), `to_spc_ports`
/// is read-only from the SPC's side (the CPU writes those).
struct ApuBusView<'a> {
    aram: &'a mut [u8; 0x10000],
    to_spc_ports: &'a mut [u8; 4],
    to_cpu_ports: &'a mut [u8; 4],
    control: &'a mut u8,
    dsp_index: &'a mut u8,
    dsp: &'a mut dsp::Dsp,
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
            // $F2 — DSP register-index port. Real HW returns the
            // last value written; we model that.
            0x00F2 => *self.dsp_index,
            // $F3 — DSP register-data port. Now routed through the
            // cycle-accurate ares-port S-DSP (`crates/luna-apu/dsp.rs`).
            0x00F3 => {
                // DSP reads have NO side effects (ares dsp/memory.cpp:1-3).
                // ENDX ($7C) is cleared only by a write to $7C or by KON —
                // never on read. Clearing it here drops end-of-sample bits
                // a driver may read more than once.
                self.dsp.read(*self.dsp_index & 0x7F)
            }
            // $F4-$F7 — mailbox FROM the main CPU.
            0x00F4..=0x00F7 => self.to_spc_ports[(addr - 0x00F4) as usize],
            // $F8-$F9 — AUXIO4/5 scratch registers. ares io.cpp:49-53
            // returns the last value written; we keep it in ARAM (the
            // write path stores it there) and read it straight back.
            0x00F8..=0x00F9 => self.aram[addr as usize],
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
                // Bits 4/5 clear the CPU→SMP input mailbox ports the SPC
                // reads at $F4-$F7 (ares io.cpp:113-123): bit 4 → ports
                // 0/1, bit 5 → ports 2/3.
                if value & 0x10 != 0 {
                    self.to_spc_ports[0] = 0;
                    self.to_spc_ports[1] = 0;
                }
                if value & 0x20 != 0 {
                    self.to_spc_ports[2] = 0;
                    self.to_spc_ports[3] = 0;
                }
                *self.control = value;
            }
            // $F2 — DSP register-index port.
            0x00F2 => *self.dsp_index = value,
            // $F3 — DSP register-data port. Routed through the
            // cycle-accurate ares-port S-DSP. `dsp.write` handles all
            // register side effects (KON/KOFF latching, FLG bit-flip,
            // ESA/DIR/EON/PMON fanout, per-voice volume/pitch/SRCN/
            // ADSR/GAIN demuxing, FIR taps) matching ares' memory.cpp.
            0x00F3 => {
                let idx = *self.dsp_index & 0x7F;
                self.dsp.write(idx, value);
            }
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
    fn dsp_register_round_trip_via_index_and_data_ports() {
        // Drivers use $F2 / $F3 a *lot*: write index, write data, or
        // write index, read data. We verify both sides — write
        // through one register slot, read back, plus a sanity check
        // that the index advances on neither port (the driver
        // increments it manually when stepping voices).
        let mut apu = Apu::new();
        let mut bus = ApuBusView {
            aram: &mut apu.aram,
            to_spc_ports: &mut apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
        };
        // Pick voice 0 envelope-X output register ($08).
        bus.write(0x00F2, 0x08);
        bus.write(0x00F3, 0x42);
        // Reading $F2 returns the index; $F3 returns the stored data.
        assert_eq!(bus.read(0x00F2), 0x08);
        assert_eq!(bus.read(0x00F3), 0x42);
        // Index bit 7 is masked when indexing the register array.
        bus.write(0x00F2, 0x88); // bit 7 set + same index 8
        assert_eq!(bus.read(0x00F3), 0x42, "bit 7 of index should be masked");
    }

    #[test]
    fn endx_is_not_cleared_on_f3_read() {
        // ares dsp/memory.cpp:1-3 — DSP reads have no side effects. ENDX
        // ($7C) must persist across repeated reads (it's cleared only by
        // a write to $7C or by KON).
        let mut apu = Apu::new();
        apu.dsp.registers[0x7C] = 0b0000_0101; // voices 0 and 2 ended
        let mut bus = ApuBusView {
            aram: &mut apu.aram,
            to_spc_ports: &mut apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
        };
        bus.write(0x00F2, 0x7C); // point the index at ENDX
        assert_eq!(bus.read(0x00F3), 0b0000_0101);
        assert_eq!(
            bus.read(0x00F3),
            0b0000_0101,
            "ENDX must survive repeated reads"
        );
        // A write to $7C clears it (the real reset path).
        bus.write(0x00F3, 0xFF);
        assert_eq!(bus.read(0x00F3), 0, "write to $7C clears ENDX");
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
                to_spc_ports: &mut apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                dsp_index: &mut apu.dsp_index,
                dsp: &mut apu.dsp,
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
            to_spc_ports: &mut apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
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
    fn control_bits_4_5_clear_input_ports() {
        // ares io.cpp:113-123 — $F1 bit 4 clears CPU→SMP ports 0/1, bit
        // 5 clears 2/3 (the ports the SPC reads at $F4-$F7).
        let mut apu = Apu::new();
        apu.cpu_write_port(0, 0x11);
        apu.cpu_write_port(1, 0x22);
        apu.cpu_write_port(2, 0x33);
        apu.cpu_write_port(3, 0x44);
        {
            let mut bus = ApuBusView {
                aram: &mut apu.aram,
                to_spc_ports: &mut apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                dsp_index: &mut apu.dsp_index,
                dsp: &mut apu.dsp,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
            };
            bus.write(0x00F1, 0x10); // bit 4 → clear ports 0/1
        }
        assert_eq!(apu.to_spc_ports, [0, 0, 0x33, 0x44]);
        {
            let mut bus = ApuBusView {
                aram: &mut apu.aram,
                to_spc_ports: &mut apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                dsp_index: &mut apu.dsp_index,
                dsp: &mut apu.dsp,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
            };
            bus.write(0x00F1, 0x20); // bit 5 → clear ports 2/3
        }
        assert_eq!(apu.to_spc_ports, [0, 0, 0, 0]);
    }

    #[test]
    fn auxio_f8_f9_read_back_written_value() {
        // ares io.cpp:49-53 — $F8/$F9 (AUXIO4/5) read back the last
        // value written, not 0.
        let mut apu = Apu::new();
        let mut bus = ApuBusView {
            aram: &mut apu.aram,
            to_spc_ports: &mut apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
        };
        bus.write(0x00F8, 0x42);
        bus.write(0x00F9, 0x99);
        assert_eq!(bus.read(0x00F8), 0x42);
        assert_eq!(bus.read(0x00F9), 0x99);
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
