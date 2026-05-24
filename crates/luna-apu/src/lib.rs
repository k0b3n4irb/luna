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

/// After how many SPC cycles a "playing" voice transitions to
/// "ended" and lights up its bit in `ENDX` (`$7C`) when ADSR is
/// disabled (i.e. gain mode). With ADSR enabled the phase machine
/// drives the end time instead.
pub const VOICE_END_SPC_CYCLES: u32 = 16_000;

/// SPC cycles per audio sample (32 kHz output at 1.024 MHz).
pub const SPC_CYCLES_PER_SAMPLE: u32 = 32;

/// Phase of one voice's ADSR envelope generator. Real hardware
/// uses 11-bit envelope values; we keep that resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdsrPhase {
    /// Voice is silent — KOF or post-release.
    Off,
    /// Attack: envelope rises linearly from 0 to 0x7FF.
    Attack,
    /// Decay: envelope falls from 0x7FF toward the sustain level.
    Decay,
    /// Sustain: envelope drifts down at the sustain rate.
    Sustain,
    /// Release: KOF gated the voice; envelope drops fast to 0.
    Release,
}

/// ADSR rate table — sample periods (at 32 kHz) for each 5-bit
/// rate index. Lifted from fullsnes + ares; rate 0 = "never advance"
/// (we approximate that with a huge value).
pub const ADSR_RATE_PERIODS: [u16; 32] = [
    0xFFFF, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80, 64, 48,
    40, 32, 24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1,
];

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

    // ------------- DSP (audio synth) — memory-backed stub -------------
    /// Last value written to `$F2` — the DSP register-index port.
    /// `$F3` reads / writes the byte at `dsp_regs[dsp_index & 0x7F]`.
    pub dsp_index: u8,
    /// The 128 DSP registers (voice params, KON / KOF, echo, etc.).
    /// Memory-backed: every write stores, every read returns. Some
    /// addresses get special treatment (KON, KOF, ENDX) so the
    /// envelope tracker can update voice state.
    pub dsp_regs: [u8; 128],
    /// Per-voice "is this voice currently playing" flag (0..7).
    /// Set by a `1` bit in KON ($4C); cleared when the envelope
    /// hits 0 or KOF ($5C) gates the voice into Release.
    pub voice_active: [bool; 8],
    /// Per-voice age in SPC cycles since KON. Used by the gain-mode
    /// fallback when ADSR is disabled.
    pub voice_age: [u32; 8],
    /// Current ADSR phase for each voice.
    pub voice_phase: [AdsrPhase; 8],
    /// 11-bit envelope value per voice (0..0x7FF). `dsp_regs[$x8]`
    /// (ENVX) is the upper 7 bits of this value.
    pub voice_envelope: [u16; 8],
    /// Per-voice "sample-position counter" — increments once per
    /// audio sample (every 32 SPC cycles) while the voice plays.
    /// Real hardware doesn't expose this on a register, but some
    /// driver-visualisation paths read auxiliary slots; we expose
    /// it through OUTX (`$x9`) so the music engine sees a bouncing
    /// signal rather than a static 0.
    pub voice_position: [u32; 8],
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
            dsp_index: 0,
            dsp_regs: [0; 128],
            voice_active: [false; 8],
            voice_age: [0; 8],
            voice_phase: [AdsrPhase::Off; 8],
            voice_envelope: [0; 8],
            voice_position: [0; 8],
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
            to_spc_ports: &apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            dsp_index: &mut apu.dsp_index,
            dsp_regs: &mut apu.dsp_regs,
            voice_active: &mut apu.voice_active,
            voice_age: &mut apu.voice_age,
            voice_phase: &mut apu.voice_phase,
            voice_envelope: &mut apu.voice_envelope,
            voice_position: &mut apu.voice_position,
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
            self.tick_one_sample();
        }
        // Refresh per-voice DSP register slots so polling drivers
        // see live values.
        for v in 0..8 {
            let envx_idx = 0x08 + v * 0x10;
            let outx_idx = 0x09 + v * 0x10;
            // ENVX = upper 7 bits of the 11-bit envelope.
            self.dsp_regs[envx_idx] = (self.voice_envelope[v] >> 4) as u8;
            // OUTX: a coarse proxy for "what's coming out of this
            // voice right now". We mix the per-voice position
            // counter's low bits with the envelope so polling
            // drivers see a moving non-zero signal during playback.
            self.dsp_regs[outx_idx] = if self.voice_active[v] {
                ((self.voice_position[v] as u8).wrapping_mul(13))
                    .wrapping_add((self.voice_envelope[v] >> 4) as u8)
            } else {
                0
            };
        }
    }

    /// Advance every voice's ADSR state by one 32 kHz audio sample.
    fn tick_one_sample(&mut self) {
        for v in 0..8 {
            if !self.voice_active[v] && self.voice_phase[v] == AdsrPhase::Off {
                continue;
            }
            self.voice_age[v] = self.voice_age[v].saturating_add(SPC_CYCLES_PER_SAMPLE);
            if self.voice_active[v] {
                self.voice_position[v] = self.voice_position[v].wrapping_add(1);
            }
            self.advance_voice_envelope(v);
            // When the envelope hits zero, the voice is done. Latch
            // its ENDX bit and deactivate.
            if self.voice_envelope[v] == 0
                && matches!(
                    self.voice_phase[v],
                    AdsrPhase::Release | AdsrPhase::Decay | AdsrPhase::Sustain
                )
            {
                self.voice_active[v] = false;
                self.voice_phase[v] = AdsrPhase::Off;
                self.dsp_regs[0x7C] |= 1 << v;
            }
            // Fallback: if a voice runs without ADSR enabled (gain
            // mode), we use the legacy age-based timeout so it
            // still finishes eventually.
            if self.voice_active[v]
                && self.voice_phase[v] == AdsrPhase::Off
                && self.voice_age[v] >= VOICE_END_SPC_CYCLES
            {
                self.voice_active[v] = false;
                self.dsp_regs[0x7C] |= 1 << v;
            }
        }
    }

    /// Step one voice's envelope by one audio sample.
    ///
    /// Each phase has its own progression:
    ///
    ///   - **Attack**: env += `0x20` per sample-period of the attack
    ///     rate, clamped to `0x7FF`. When the cap is hit we transition
    ///     to Decay.
    ///   - **Decay**: env steps down by `(env >> 8) + 1` per sample-
    ///     period of the decay rate (an exponential-ish curve in
    ///     8 stages). Reaches sustain level → moves to Sustain.
    ///   - **Sustain**: env steps down by `(env >> 8) + 1` per sample-
    ///     period of the sustain rate (usually slow).
    ///   - **Release**: env -= `8` per sample (KOF-gated).
    fn advance_voice_envelope(&mut self, v: usize) {
        let adsr1 = self.dsp_regs[0x05 + v * 0x10];
        let adsr2 = self.dsp_regs[0x06 + v * 0x10];
        let use_adsr = adsr1 & 0x80 != 0;

        if !use_adsr {
            // Gain mode: keep the linear-decay envelope so we still
            // have *something*. Don't transition phases (they stay
            // Off and the age-based timeout above handles end-of-life).
            let env = u16::from(self.dsp_regs[0x07 + v * 0x10]) << 4;
            self.voice_envelope[v] = env.min(0x7FF);
            return;
        }

        let attack_rate = (adsr1 & 0x0F) | 0x10; // attack uses indices 16..31
        let decay_rate = ((adsr1 >> 4) & 0x07) | 0x10; // decay uses indices 16..23
        let sustain_rate = adsr2 & 0x1F;
        let sustain_level = u16::from(adsr2 >> 5);
        let sustain_target = (sustain_level + 1) * 0x100; // 1/8..8/8 of 0x800

        match self.voice_phase[v] {
            AdsrPhase::Attack => {
                let period = ADSR_RATE_PERIODS[attack_rate as usize];
                if self.voice_age[v] % u32::from(period.max(1)) < SPC_CYCLES_PER_SAMPLE {
                    let env = self.voice_envelope[v];
                    self.voice_envelope[v] = (env + 0x20).min(0x7FF);
                    if self.voice_envelope[v] == 0x7FF {
                        self.voice_phase[v] = AdsrPhase::Decay;
                    }
                }
            }
            AdsrPhase::Decay => {
                let period = ADSR_RATE_PERIODS[decay_rate as usize];
                if self.voice_age[v] % u32::from(period.max(1)) < SPC_CYCLES_PER_SAMPLE {
                    let env = self.voice_envelope[v];
                    let step = (env >> 8) + 1;
                    self.voice_envelope[v] = env.saturating_sub(step);
                    if self.voice_envelope[v] <= sustain_target {
                        self.voice_envelope[v] = sustain_target;
                        self.voice_phase[v] = AdsrPhase::Sustain;
                    }
                }
            }
            AdsrPhase::Sustain => {
                let period = ADSR_RATE_PERIODS[sustain_rate as usize];
                if period < 0xFFFF
                    && self.voice_age[v] % u32::from(period.max(1)) < SPC_CYCLES_PER_SAMPLE
                {
                    let env = self.voice_envelope[v];
                    let step = (env >> 8) + 1;
                    self.voice_envelope[v] = env.saturating_sub(step);
                }
            }
            AdsrPhase::Release => {
                self.voice_envelope[v] = self.voice_envelope[v].saturating_sub(8);
            }
            AdsrPhase::Off => {}
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
            self.tick_voices(4);
            let mut bus = ApuBusView {
                aram: &mut self.aram,
                to_spc_ports: &self.to_spc_ports,
                to_cpu_ports: &mut self.to_cpu_ports,
                control: &mut self.control,
                dsp_index: &mut self.dsp_index,
                dsp_regs: &mut self.dsp_regs,
                voice_active: &mut self.voice_active,
                voice_age: &mut self.voice_age,
                voice_phase: &mut self.voice_phase,
                voice_envelope: &mut self.voice_envelope,
                voice_position: &mut self.voice_position,
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
    dsp_index: &'a mut u8,
    dsp_regs: &'a mut [u8; 128],
    voice_active: &'a mut [bool; 8],
    voice_age: &'a mut [u32; 8],
    voice_phase: &'a mut [AdsrPhase; 8],
    voice_envelope: &'a mut [u16; 8],
    voice_position: &'a mut [u32; 8],
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
            // $F3 — DSP register-data port.
            0x00F3 => {
                let idx = (*self.dsp_index & 0x7F) as usize;
                let v = self.dsp_regs[idx];
                if idx == 0x7C {
                    // ENDX — read **clears** the register on real
                    // hardware. Music drivers spam-read this in
                    // their main loop, processing each `1` bit by
                    // freeing the corresponding voice.
                    self.dsp_regs[0x7C] = 0;
                }
                v
            }
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
            // $F2 — DSP register-index port.
            0x00F2 => *self.dsp_index = value,
            // $F3 — DSP register-data port. Most writes just store,
            // but KON ($4C) and KOF ($5C) drive our voice state
            // tracker so ENDX gets populated correctly. Writing
            // ENDX ($7C) clears the bits that the value has set
            // (this is the "ack the voice end" pattern).
            0x00F3 => {
                let idx = (*self.dsp_index & 0x7F) as usize;
                self.dsp_regs[idx] = value;
                match idx {
                    0x4C => {
                        // KON: each `1` bit in `value` keys the
                        // corresponding voice ON. The envelope
                        // starts at 0 and enters Attack phase if
                        // ADSR is enabled; in gain mode the legacy
                        // linear path runs (AdsrPhase::Off).
                        for v in 0..8 {
                            if value & (1 << v) != 0 {
                                self.voice_active[v] = true;
                                self.voice_age[v] = 0;
                                self.voice_position[v] = 0;
                                self.voice_envelope[v] = 0;
                                let adsr1 = self.dsp_regs[0x05 + v * 0x10];
                                self.voice_phase[v] = if adsr1 & 0x80 != 0 {
                                    AdsrPhase::Attack
                                } else {
                                    AdsrPhase::Off
                                };
                                self.dsp_regs[0x7C] &= !(1 << v); // clear stale ENDX
                            }
                        }
                    }
                    0x5C => {
                        // KOF: each `1` bit gates the voice into
                        // Release. The envelope falls to 0 from
                        // there; ENDX fires when it does.
                        for v in 0..8 {
                            if value & (1 << v) != 0 {
                                self.voice_phase[v] = AdsrPhase::Release;
                            }
                        }
                    }
                    0x7C => {
                        // Writes to ENDX clear the bits set in
                        // `value` (driver-style acknowledgement).
                        self.dsp_regs[0x7C] &= !value;
                    }
                    _ => {}
                }
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
    fn kon_then_age_sets_endx_then_read_clears_it() {
        let mut apu = Apu::new();
        // Construct a bus view long enough to KON voice 0 + voice 3.
        {
            let mut bus = ApuBusView {
                aram: &mut apu.aram,
                to_spc_ports: &apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                dsp_index: &mut apu.dsp_index,
                dsp_regs: &mut apu.dsp_regs,
                voice_active: &mut apu.voice_active,
                voice_age: &mut apu.voice_age,
                voice_phase: &mut apu.voice_phase,
                voice_envelope: &mut apu.voice_envelope,
                voice_position: &mut apu.voice_position,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
            };
            bus.write(0x00F2, 0x4C); // index = KON
            bus.write(0x00F3, 0b0000_1001); // KON voices 0 and 3
        }
        assert!(apu.voice_active[0]);
        assert!(apu.voice_active[3]);
        assert!(!apu.voice_active[1]);

        // Age past the threshold for both — ENDX should light up
        // bits 0 and 3.
        apu.tick_voices(VOICE_END_SPC_CYCLES + 100);
        assert_eq!(apu.dsp_regs[0x7C], 0b0000_1001);
        assert!(!apu.voice_active[0]);
        assert!(!apu.voice_active[3]);

        // Driver reads ENDX → clears.
        {
            let mut bus = ApuBusView {
                aram: &mut apu.aram,
                to_spc_ports: &apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                dsp_index: &mut apu.dsp_index,
                dsp_regs: &mut apu.dsp_regs,
                voice_active: &mut apu.voice_active,
                voice_age: &mut apu.voice_age,
                voice_phase: &mut apu.voice_phase,
                voice_envelope: &mut apu.voice_envelope,
                voice_position: &mut apu.voice_position,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
            };
            bus.write(0x00F2, 0x7C);
            let v = bus.read(0x00F3);
            assert_eq!(v, 0b0000_1001, "first read returns ENDX bits");
            assert_eq!(
                bus.read(0x00F3),
                0,
                "second read sees 0 (real HW: read clears ENDX)"
            );
        }
    }

    #[test]
    fn envx_rises_during_attack_phase() {
        // With ADSR enabled and a fast attack rate, KON should put
        // the voice in Attack phase where the envelope rises from 0
        // toward $7FF. Verify by reading ENVX before and after some
        // sample ticks.
        let mut apu = Apu::new();
        {
            let mut bus = ApuBusView {
                aram: &mut apu.aram,
                to_spc_ports: &apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                dsp_index: &mut apu.dsp_index,
                dsp_regs: &mut apu.dsp_regs,
                voice_active: &mut apu.voice_active,
                voice_age: &mut apu.voice_age,
                voice_phase: &mut apu.voice_phase,
                voice_envelope: &mut apu.voice_envelope,
                voice_position: &mut apu.voice_position,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
            };
            // Voice 2 ADSR1 = $8F (bit 7 = ADSR on, attack rate $F).
            bus.write(0x00F2, 0x25);
            bus.write(0x00F3, 0x8F);
            // Voice 2 ADSR2 = $1F (sustain rate fast, sustain level 0).
            bus.write(0x00F2, 0x26);
            bus.write(0x00F3, 0x1F);
            // KON voice 2.
            bus.write(0x00F2, 0x4C);
            bus.write(0x00F3, 1 << 2);
        }
        assert_eq!(apu.voice_phase[2], AdsrPhase::Attack);
        apu.tick_voices(0);
        let envx_at_0 = apu.dsp_regs[0x08 + 2 * 0x10];
        // Tick a bunch of samples — envelope should rise.
        apu.tick_voices(SPC_CYCLES_PER_SAMPLE * 200);
        let envx_after = apu.dsp_regs[0x08 + 2 * 0x10];
        assert!(envx_after > envx_at_0, "ENVX should rise during attack");
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
            to_spc_ports: &apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            dsp_index: &mut apu.dsp_index,
            dsp_regs: &mut apu.dsp_regs,
            voice_active: &mut apu.voice_active,
            voice_age: &mut apu.voice_age,
            voice_phase: &mut apu.voice_phase,
            voice_envelope: &mut apu.voice_envelope,
            voice_position: &mut apu.voice_position,
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
                dsp_index: &mut apu.dsp_index,
                dsp_regs: &mut apu.dsp_regs,
                voice_active: &mut apu.voice_active,
                voice_age: &mut apu.voice_age,
                voice_phase: &mut apu.voice_phase,
                voice_envelope: &mut apu.voice_envelope,
                voice_position: &mut apu.voice_position,
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
            dsp_index: &mut apu.dsp_index,
            dsp_regs: &mut apu.dsp_regs,
            voice_active: &mut apu.voice_active,
            voice_age: &mut apu.voice_age,
            voice_phase: &mut apu.voice_phase,
            voice_envelope: &mut apu.voice_envelope,
            voice_position: &mut apu.voice_position,
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
