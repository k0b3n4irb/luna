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

/// Lazily-built 512-entry Gaussian interpolation table used by the
/// SPC700 DSP for 4-tap pitch interpolation.
///
/// Generated from the canonical [ares] formula. After raw computation
/// the table is normalised so each fraction-aligned 4-tap group
/// (indices `[phase, 255-phase, 256+phase, 511-phase]`) sums to
/// exactly `2048`. That keeps the interpolation lossless when all
/// four history samples are equal: the right-shift by 11 in the
/// 4-tap formula then recovers the input sample byte-for-byte.
///
/// [ares]: https://github.com/ares-emulator/ares/blob/master/ares/sfc/dsp/gaussian.cpp
#[must_use]
pub fn gaussian_table() -> &'static [i16; 512] {
    static TABLE: std::sync::OnceLock<[i16; 512]> = std::sync::OnceLock::new();
    TABLE.get_or_init(build_gaussian_table)
}

/// One-shot builder for the Gaussian table — only the first call to
/// [`gaussian_table`] reaches this.
fn build_gaussian_table() -> [i16; 512] {
    let pi = std::f64::consts::PI;
    let mut raw = [0.0_f64; 512];
    for n in 0..512 {
        let k = 0.5 + n as f64;
        let s = (pi * k * 1.280 / 1024.0).sin();
        let t = ((pi * k * 2.000 / 1023.0).cos() - 1.0) * 0.50;
        let u = ((pi * k * 4.000 / 1023.0).cos() - 1.0) * 0.08;
        // Match ares: store the raw value at the MIRRORED index, so
        // the table is monotone-increasing 0..511 (peak at 511, zero
        // at 0). The 4-tap formula then reads from both ends.
        raw[511 - n] = s * (t + u + 1.0) / k;
    }
    let mut table = [0i16; 512];
    // Normalise each of the 128 phase-groups so the 4-tap sum is 2048.
    for phase in 0..128 {
        let idxs = [phase, 255 - phase, 256 + phase, 511 - phase];
        let sum = idxs.iter().map(|&i| raw[i]).sum::<f64>();
        let scale = 2048.0 / sum;
        for &i in &idxs {
            // +0.5 round-half-up; values are positive and small (<2K).
            table[i] = (raw[i] * scale + 0.5) as i16;
        }
    }
    table
}

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
    /// Per-voice current BRR block start address in ARAM. Set on
    /// KON from the sample directory at `DIR + SRCN*4`; advances by
    /// 9 bytes each time a block of 16 samples is consumed; jumps
    /// to the loop address from the directory when an end-of-sample
    /// block has the loop bit set.
    pub voice_block_addr: [u16; 8],
    /// Position within the current BRR block (0..15). When it wraps,
    /// the block header is re-read to handle end / loop bits.
    pub voice_block_sample: [u8; 8],
    /// Last four decoded BRR samples per voice, newest-first:
    /// `[s(n), s(n-1), s(n-2), s(n-3)]`.
    ///
    /// The BRR filters (F1/F2/F3) only need the most recent two
    /// samples to reconstruct the next, but the 4-tap Gaussian
    /// interpolator needs four. We keep the wider slot universally
    /// so both consumers index a single array.
    pub voice_brr_history: [[i16; 4]; 8],
    /// Pitch accumulator per voice. Each output sample (32 kHz) the
    /// 14-bit pitch (from `$x2`/`$x3`) is added; whenever the
    /// accumulator crosses 0x1000 the BRR sample pointer advances
    /// once. So pitch 0x1000 = 1:1 (32 kHz reproduction); pitch
    /// 0x0800 plays at half speed; pitch 0x2000 plays at double.
    pub voice_pitch_acc: [u16; 8],
    /// Echo ring buffer position, in stereo samples. The ring lives
    /// in ARAM starting at `(ESA << 8)` and is `max(1, EDL) * 512`
    /// stereo samples long (each sample = 4 bytes: lo_L, hi_L, lo_R,
    /// hi_R). On each sample tick the FIR taps read the 8 most-recent
    /// samples relative to this index, then the echo write-back (if
    /// `FLG.5` is clear) overwrites the buffer at this index and
    /// the index advances modulo the buffer size.
    pub echo_pos_samples: u16,
    /// 15-bit noise LFSR, initialised to `$4000`. Stepped at the
    /// rate selected by `FLG[4:0]`. Output (sign-extended) replaces
    /// the BRR sample for any voice whose `NON ($3D)` bit is set.
    pub noise_lfsr: u16,
    /// Sub-tick deficit for the noise LFSR clock. Increments by 1
    /// every audio sample; when it crosses the per-rate period
    /// from [`ADSR_RATE_PERIODS`] (indexed by `FLG[4:0]`) we step
    /// the LFSR once and subtract the period.
    pub noise_deficit: u32,
    /// Each voice's Gaussian-interpolated output sample for the
    /// previous tick (post-interp, *before* envelope and volume).
    /// Voice `v+1` reads `voice_pmod_output[v]` when its `PMON`
    /// (`$2D`) bit is set to modulate its pitch. Voice 0 never
    /// reads from here (no preceding voice).
    pub voice_pmod_output: [i16; 8],
    /// Mixed L/R audio output for the current sample, post voice and
    /// master volume. Updated each `tick_one_sample`; consumers
    /// (future audio backend) can read it via [`Self::audio_sample`].
    pub audio_left: i16,
    /// Mixed R audio (see [`Self::audio_left`]).
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
            // Real hardware resets the DSP register file with FLG=$E0
            // (soft reset + mute amp + echo write disable). Without
            // this, the echo subsystem starts writing into ARAM[0..3]
            // before the SPC driver has had a chance to configure
            // ESA/EDL, corrupting the SPC's zero page. Every commercial
            // driver clears these bits explicitly once it's safe.
            dsp_regs: {
                let mut r = [0u8; 128];
                r[0x6C] = 0xE0;
                r
            },
            voice_active: [false; 8],
            voice_age: [0; 8],
            voice_phase: [AdsrPhase::Off; 8],
            voice_envelope: [0; 8],
            voice_position: [0; 8],
            voice_block_addr: [0; 8],
            voice_block_sample: [0; 8],
            voice_brr_history: [[0, 0, 0, 0]; 8],
            voice_pitch_acc: [0; 8],
            echo_pos_samples: 0,
            // LFSR reset to $4000 matches real hardware (a single
            // bit set in the upper half) — guarantees full 32767-
            // cycle period without falling into the all-zero
            // absorbing state.
            noise_lfsr: 0x4000,
            noise_deficit: 0,
            voice_pmod_output: [0; 8],
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
            voice_block_addr: &mut apu.voice_block_addr,
            voice_block_sample: &mut apu.voice_block_sample,
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
            // OUTX = signed-8-bit (latest decoded sample × envelope).
            // Real DSP: out = sample × env / 0x800 (envelope is
            // 11-bit). We then keep only the upper byte for the
            // signed-8 OUTX. Voices that aren't playing or have
            // 0 envelope output 0.
            self.dsp_regs[outx_idx] = if self.voice_active[v] {
                let s = i32::from(self.voice_brr_history[v][0]);
                let e = i32::from(self.voice_envelope[v]);
                let mixed = (s * e) >> 11; // ≈ sample × env / 2048
                let clipped = mixed.clamp(-32768, 32767);
                // Top byte of the 16-bit mix → signed-8 OUTX.
                ((clipped >> 8) & 0xFF) as u8
            } else {
                0
            };
        }
    }

    /// Advance every voice's ADSR state by one 32 kHz audio sample.
    fn tick_one_sample(&mut self) {
        // Reset accumulated stereo mix; voices add to it below.
        let mut mix_l: i32 = 0;
        let mut mix_r: i32 = 0;
        // Echo input — voices whose `EON` bit is set also dump their
        // post-volume output here. After the voice loop we run the
        // FIR filter and feedback path.
        let mut echo_in_l: i32 = 0;
        let mut echo_in_r: i32 = 0;
        let eon = self.dsp_regs[0x4D];
        let non = self.dsp_regs[0x3D];
        let pmon = self.dsp_regs[0x2D];

        // Tick the noise LFSR. FLG[4:0] selects the clock rate from
        // the same 32-entry period table as ADSR. At rate 0 the
        // period is "never" (table entry 0xFFFF); we never advance.
        let noise_rate = self.dsp_regs[0x6C] & 0x1F;
        let noise_period = ADSR_RATE_PERIODS[noise_rate as usize];
        if noise_period != 0xFFFF {
            self.noise_deficit = self.noise_deficit.saturating_add(1);
            while self.noise_deficit >= u32::from(noise_period) {
                self.noise_deficit -= u32::from(noise_period);
                // Galois LFSR step: new bit = bit0 XOR bit1, shift
                // right, place new bit at position 14. The all-zero
                // state is the only absorbing one and we initialise
                // away from it; once running the LFSR cycles
                // through 2^15-1 = 32767 distinct values.
                let new_bit = (self.noise_lfsr & 1) ^ ((self.noise_lfsr >> 1) & 1);
                self.noise_lfsr = (self.noise_lfsr >> 1) | (new_bit << 14);
            }
        }
        // Sign-extend the 15-bit LFSR into a signed-16-bit noise
        // sample (-32768..+32766 range). Bit 14 acts as the sign.
        let noise_sample: i16 = {
            let signed_15 = (self.noise_lfsr as i32) - if self.noise_lfsr & 0x4000 != 0 {
                0x8000
            } else {
                0
            };
            (signed_15 << 1) as i16
        };

        for v in 0..8 {
            if !self.voice_active[v] && self.voice_phase[v] == AdsrPhase::Off {
                continue;
            }
            self.voice_age[v] = self.voice_age[v].saturating_add(SPC_CYCLES_PER_SAMPLE);
            if self.voice_active[v] {
                self.voice_position[v] = self.voice_position[v].wrapping_add(1);
                // Pitch counter: 14-bit pitch register added each
                // output tick. Every time the accumulator crosses
                // `$1000` we consume one BRR sample (`$1000` = 1:1
                // rate). For high pitches (`> $1000`) more than one
                // sample may be consumed per tick — we loop instead
                // of capping at one, otherwise notes above the
                // pitch-table's neutral entry play at half-speed.
                let raw_pitch = u16::from(self.dsp_regs[0x02 + v * 0x10])
                    | (u16::from(self.dsp_regs[0x03 + v * 0x10]) << 8);
                let raw_pitch = raw_pitch & 0x3FFF;
                // Pitch modulation: when PMON.v is set (only valid for
                // v >= 1; voice 0 has no preceding voice), the previous
                // voice's pre-volume output scales this voice's pitch
                // step. Factor is centered at $400 (= unity); each unit
                // of `prev_output >> 5` adds/subtracts 1/0x400 of the
                // base pitch per step. Matches bsnes/ares behaviour.
                let pitch = if v > 0 && pmon & (1 << v) != 0 {
                    let prev = i32::from(self.voice_pmod_output[v - 1]);
                    let factor = (prev >> 5) + 0x400; // 0..0x800-ish
                    ((i32::from(raw_pitch) * factor) >> 10).clamp(0, 0x3FFF) as u16
                } else {
                    raw_pitch
                };
                let mut acc = u32::from(self.voice_pitch_acc[v]) + u32::from(pitch);
                while acc >= 0x1000 {
                    acc -= 0x1000;
                    let sample = self.decode_current_brr_sample(v);
                    // Shift history newest→oldest by one slot, then
                    // place the new sample at index 0 (newest).
                    self.voice_brr_history[v][3] = self.voice_brr_history[v][2];
                    self.voice_brr_history[v][2] = self.voice_brr_history[v][1];
                    self.voice_brr_history[v][1] = self.voice_brr_history[v][0];
                    self.voice_brr_history[v][0] = sample;
                    self.advance_brr_block(v);
                    if !self.voice_active[v] {
                        break;
                    }
                }
                self.voice_pitch_acc[v] = acc as u16;
            }
            self.advance_voice_envelope(v);

            // Fold this voice's contribution into the global stereo
            // mix. `outx = sample × env / 0x800` (signed 16-bit);
            // we then scale by signed-7-bit VOL_L / VOL_R per voice.
            //
            // The "sample" we feed in is the output of a 4-tap
            // Gaussian filter — matching real SPC700 hardware. The
            // upper 8 bits of the 12-bit pitch accumulator index a
            // 512-entry table; the four taps line up at indices
            // `[255-frac, 511-frac, 256+frac, frac]` against the
            // four most-recent decoded BRR samples (oldest → newest).
            // The table is normalised so the 4-tap weights sum to
            // exactly 2048 — the `>> 11` then recovers the input
            // amplitude when all four taps are equal.
            if self.voice_active[v] {
                let frac = (self.voice_pitch_acc[v] >> 4) as usize; // 0..=255
                let table = gaussian_table();
                let s3 = i32::from(self.voice_brr_history[v][0]); // newest
                let s2 = i32::from(self.voice_brr_history[v][1]);
                let s1 = i32::from(self.voice_brr_history[v][2]);
                let s0 = i32::from(self.voice_brr_history[v][3]); // oldest
                let sum = i32::from(table[255 - frac]) * s0
                    + i32::from(table[511 - frac]) * s1
                    + i32::from(table[256 + frac]) * s2
                    + i32::from(table[frac]) * s3;
                let gaussian_s = (sum >> 11).clamp(-32768, 32767);
                // If NON.v is set, the BRR/Gaussian path is bypassed
                // and the LFSR noise sample becomes this voice's
                // source. The envelope and per-voice volume still
                // apply, so noise can be shaped like a regular voice
                // (typical use: percussion hi-hats with a fast ADSR).
                let s = if non & (1 << v) != 0 {
                    i32::from(noise_sample)
                } else {
                    gaussian_s
                };
                // Store the pre-envelope sample so the next voice's
                // pitch-modulation step (above) can read it. Saved
                // even for noise voices so PMON works consistently.
                self.voice_pmod_output[v] = s.clamp(-32768, 32767) as i16;
                let env = i32::from(self.voice_envelope[v]);
                let outx = (s * env) >> 11; // signed 16-bit
                let vol_l = self.dsp_regs[v * 0x10] as i8 as i32;
                let vol_r = self.dsp_regs[v * 0x10 + 1] as i8 as i32;
                let contrib_l = (outx * vol_l) >> 7;
                let contrib_r = (outx * vol_r) >> 7;
                mix_l += contrib_l;
                mix_r += contrib_r;
                // Per-voice echo routing: if EON.v is set, this voice's
                // post-volume output is also added to the echo input.
                // Real hardware mixes BEFORE clamping; we accumulate
                // i32 here and clamp at the FIR write-back below.
                if eon & (1 << v) != 0 {
                    echo_in_l += contrib_l;
                    echo_in_r += contrib_r;
                }
            }
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

        // ---------------- Echo subsystem ----------------
        //
        // The echo buffer lives in ARAM as a circular ring of stereo
        // samples (4 bytes each — lo_L, hi_L, lo_R, hi_R), starting
        // at `(ESA << 8)` and `max(1, EDL) * 512` samples long.
        // Each output sample we:
        //   1. read 8 historical samples to feed the FIR filter,
        //   2. compute the FIR-filtered echo output,
        //   3. (if `FLG.5` is clear) write the new sample —
        //      `echo_in + echo_out * EFB / 128` — back to the ring,
        //   4. mix `echo_out * EVOL_L/R / 128` into the final output.
        let (echo_out_l, echo_out_r) = self.process_echo(echo_in_l, echo_in_r);

        // Apply master volume — MVOL_L (\$0C) and MVOL_R (\$1C) are
        // signed 7-bit master gain for the left / right outputs.
        let mvol_l = self.dsp_regs[0x0C] as i8 as i32;
        let mvol_r = self.dsp_regs[0x1C] as i8 as i32;
        let main_l = (mix_l * mvol_l) >> 7;
        let main_r = (mix_r * mvol_r) >> 7;
        // Echo volume: EVOL_L ($2C) / EVOL_R ($3C) are signed 7-bit
        // gain on the FIR output, added to the main mix.
        let evol_l = self.dsp_regs[0x2C] as i8 as i32;
        let evol_r = self.dsp_regs[0x3C] as i8 as i32;
        let mut final_l = main_l + ((echo_out_l * evol_l) >> 7);
        let mut final_r = main_r + ((echo_out_r * evol_r) >> 7);
        // FLG register at $6C:
        //   bit 7 — soft reset (mute output + keep voices off)
        //   bit 6 — mute amp (silence all output)
        //   bit 5 — echo write disable (handled inside `process_echo`)
        //   bits 4..0 — noise frequency (noise not yet implemented)
        let flg = self.dsp_regs[0x6C];
        if flg & 0xC0 != 0 {
            final_l = 0;
            final_r = 0;
        }
        self.audio_left = final_l.clamp(-32768, 32767) as i16;
        self.audio_right = final_r.clamp(-32768, 32767) as i16;
        // Enqueue the sample for the host audio backend. Bound the
        // queue so a paused or slow consumer can't grow it without
        // limit; we drop oldest samples on overflow (audible click,
        // far better than allocating forever).
        const MAX_QUEUED: usize = 16_384;
        if self.audio_queue.len() >= MAX_QUEUED {
            self.audio_queue.pop_front();
        }
        self.audio_queue
            .push_back((self.audio_left, self.audio_right));
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
    pub fn audio_sample(&self) -> (i16, i16) {
        (self.audio_left, self.audio_right)
    }

    /// Run one sample tick's worth of echo processing.
    ///
    /// Reads 8 historical samples from the ARAM ring buffer at
    /// `(ESA << 8)` (with size `max(1, EDL) * 512` stereo samples),
    /// applies the 8-tap FIR filter from the coefficient registers
    /// at `$0F, $1F, $2F, ..., $7F`, optionally writes the
    /// `echo_in + echo_out * EFB / 128` blend back to the buffer
    /// (gated by `FLG.5`), and advances the ring position.
    ///
    /// Returns the clipped stereo FIR output, which the caller
    /// scales by `EVOL_L / EVOL_R` before adding to the main mix.
    fn process_echo(&mut self, echo_in_l: i32, echo_in_r: i32) -> (i32, i32) {
        let esa = self.dsp_regs[0x6D];
        let edl = self.dsp_regs[0x7D] & 0x0F; // 4 bits — spec says 0..=15
        // EDL=0 still gives a 1-sample (4-byte) buffer. Otherwise
        // EDL * 0x800 bytes = EDL * 512 stereo samples.
        let size_samples: u16 = if edl == 0 { 1 } else { u16::from(edl) * 512 };
        let echo_base: u16 = u16::from(esa) << 8;

        // Position is recomputed modulo the (possibly changed) size
        // each tick; real hardware corrupts ARAM if the size shrinks
        // mid-song, but most games never touch EDL after init.
        let pos = self.echo_pos_samples % size_samples;

        // Read 8 FIR taps. Convention (matches fullsnes + ares):
        //   tap[0] = oldest sample (pos - 8)
        //   tap[7] = newest sample (pos - 1) — the slot most recently
        //                                       written in the *previous*
        //                                       tick. The current `pos`
        //                                       is the slot we're about
        //                                       to overwrite, so it never
        //                                       enters the FIR.
        let mut tap_l = [0i32; 8];
        let mut tap_r = [0i32; 8];
        for i in 0..8 {
            // (pos + size - 8 + i) % size; using i32 to avoid u16 underflow.
            let off_back = 8 - i as i32;
            let idx = ((pos as i32) - off_back).rem_euclid(size_samples as i32) as u16;
            let addr = echo_base.wrapping_add(idx.wrapping_mul(4));
            let lo_l = self.aram[addr as usize];
            let hi_l = self.aram[addr.wrapping_add(1) as usize];
            let lo_r = self.aram[addr.wrapping_add(2) as usize];
            let hi_r = self.aram[addr.wrapping_add(3) as usize];
            tap_l[i] = i32::from(i16::from_le_bytes([lo_l, hi_l]));
            tap_r[i] = i32::from(i16::from_le_bytes([lo_r, hi_r]));
        }

        // FIR convolution. Coefficients are at $0F, $1F, ..., $7F
        // (one per voice block, low nibble = $F). Each is a signed
        // 8-bit gain; the sum is shifted right by 7 to normalize
        // (so a single $7F coefficient ≈ unity gain).
        let mut out_l: i32 = 0;
        let mut out_r: i32 = 0;
        for i in 0..8 {
            let coef = self.dsp_regs[0x0F + i * 0x10] as i8 as i32;
            out_l += tap_l[i] * coef;
            out_r += tap_r[i] * coef;
        }
        out_l >>= 7;
        out_r >>= 7;
        let out_l = out_l.clamp(-32768, 32767);
        let out_r = out_r.clamp(-32768, 32767);

        // Write-back: `new = echo_in + echo_out * EFB / 128`, gated
        // by `FLG.5` (ECEN = echo write enable when 0, disable when 1).
        let flg = self.dsp_regs[0x6C];
        if flg & 0x20 == 0 {
            let efb = self.dsp_regs[0x0D] as i8 as i32;
            let wl = (echo_in_l + ((out_l * efb) >> 7)).clamp(-32768, 32767) as i16;
            let wr = (echo_in_r + ((out_r * efb) >> 7)).clamp(-32768, 32767) as i16;
            let addr = echo_base.wrapping_add(pos.wrapping_mul(4));
            let [llo, lhi] = wl.to_le_bytes();
            let [rlo, rhi] = wr.to_le_bytes();
            self.aram[addr as usize] = llo;
            self.aram[addr.wrapping_add(1) as usize] = lhi;
            self.aram[addr.wrapping_add(2) as usize] = rlo;
            self.aram[addr.wrapping_add(3) as usize] = rhi;
        }

        // Advance ring position.
        self.echo_pos_samples = (pos + 1) % size_samples;

        (out_l, out_r)
    }

    /// Decode the current BRR sample for voice `v` — the one at
    /// `voice_block_sample[v]` within `voice_block_addr[v]`. Applies
    /// the block's range shift and one of the four standard SPC700
    /// filters (F0..F3) using the per-voice history.
    fn decode_current_brr_sample(&self, v: usize) -> i16 {
        let block_addr = self.voice_block_addr[v];
        let header = self.aram[block_addr as usize];
        let range = (header >> 4) & 0x0F;
        let filter = (header >> 2) & 0x03;
        let sample_idx = self.voice_block_sample[v];
        // Nibble layout: byte 1 holds samples 0,1 (high, low); byte
        // 2 holds 2,3; ...; byte 8 holds 14,15.
        let byte_off = 1 + u16::from(sample_idx / 2);
        let byte = self.aram[block_addr.wrapping_add(byte_off) as usize];
        let nibble = if sample_idx & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        };
        // Sign-extend a 4-bit signed value to i32.
        let signed_nibble = ((nibble as i8) << 4) >> 4;
        // Range shift: 0..12 are normal; 13..15 clamp to 0 or 0xF800
        // per real hardware.
        let raw = if range <= 12 {
            i32::from(signed_nibble) << range
        } else if signed_nibble < 0 {
            -2048
        } else {
            0
        };
        let p1 = i32::from(self.voice_brr_history[v][0]);
        let p2 = i32::from(self.voice_brr_history[v][1]);
        let mixed = match filter {
            0 => raw,
            1 => raw + p1 + ((-p1) >> 4),
            2 => raw + p1 * 2 + ((-p1 * 3) >> 5) - p2 + (p2 >> 4),
            3 => raw + p1 * 2 + ((-p1 * 13) >> 6) - p2 + ((p2 * 3) >> 4),
            _ => raw, // unreachable but defensive
        };
        mixed.clamp(-32768, 32767) as i16
    }

    /// Advance voice `v` by one sample within its current BRR block.
    /// When we cross a 16-sample boundary, we read the *current*
    /// block's header byte (in ARAM at `voice_block_addr[v]`),
    /// react to its end/loop bits, and either jump to the next
    /// block (`addr + 9`), loop back via the directory, or end the
    /// sample (latch ENDX, deactivate or move to Release).
    fn advance_brr_block(&mut self, v: usize) {
        self.voice_block_sample[v] = self.voice_block_sample[v].wrapping_add(1);
        if self.voice_block_sample[v] < 16 {
            return;
        }
        self.voice_block_sample[v] = 0;
        let header = self.aram[self.voice_block_addr[v] as usize];
        let end = header & 0x01 != 0;
        let loop_bit = header & 0x02 != 0;
        if end {
            // Latch ENDX every time we cross an end block.
            self.dsp_regs[0x7C] |= 1 << v;
            if loop_bit {
                // Jump to the loop address from the sample directory.
                let dir_base = u16::from(self.dsp_regs[0x5D]) << 8;
                let srcn = self.dsp_regs[0x04 + v * 0x10];
                let entry = dir_base.wrapping_add(u16::from(srcn).wrapping_mul(4));
                let loop_lo = self.aram[entry.wrapping_add(2) as usize];
                let loop_hi = self.aram[entry.wrapping_add(3) as usize];
                self.voice_block_addr[v] = u16::from(loop_lo) | (u16::from(loop_hi) << 8);
            } else {
                // No loop → voice ends. ADSR path moves to Release
                // so the envelope fades; gain mode deactivates.
                if self.voice_phase[v] == AdsrPhase::Off {
                    self.voice_active[v] = false;
                } else {
                    self.voice_phase[v] = AdsrPhase::Release;
                }
            }
        } else {
            // Plain block: move to the next 9-byte BRR block in ARAM.
            self.voice_block_addr[v] = self.voice_block_addr[v].wrapping_add(9);
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
            // Gain mode. The `$x7` GAIN register splits as:
            //   bit 7   : 0 = direct, 1 = custom
            //   bits 6:5: custom-mode selector
            //              00 = Linear  Decrease — env -= 0x20 per step
            //              01 = Exp.    Decrease — env -= (env >> 8) + 1
            //              10 = Linear  Increase — env += 0x20
            //              11 = Bent    Increase — +0x20 below 0x600,
            //                                       then +0x08 above
            //   bits 4:0: rate index into ADSR_RATE_PERIODS
            let gain = self.dsp_regs[0x07 + v * 0x10];
            if gain & 0x80 == 0 {
                // Direct gain: envelope is just the low 7 bits scaled up.
                self.voice_envelope[v] = (u16::from(gain) << 4).min(0x7FF);
                return;
            }
            let rate = gain & 0x1F;
            let period = ADSR_RATE_PERIODS[rate as usize];
            if period == 0xFFFF {
                return;
            }
            // Mirror the ADSR-phase logic: only step on the period
            // boundary, with the same age-modulo gating.
            if self.voice_age[v] % u32::from(period.max(1)) >= SPC_CYCLES_PER_SAMPLE {
                return;
            }
            let env = self.voice_envelope[v];
            self.voice_envelope[v] = match (gain >> 5) & 0x03 {
                0b00 => env.saturating_sub(0x20),
                0b01 => env.saturating_sub((env >> 8) + 1),
                0b10 => (env + 0x20).min(0x7FF),
                _ /* 0b11 */ => {
                    let step: u16 = if env < 0x600 { 0x20 } else { 0x08 };
                    (env + step).min(0x7FF)
                }
            };
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
                voice_block_addr: &mut self.voice_block_addr,
                voice_block_sample: &mut self.voice_block_sample,
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
    voice_block_addr: &'a mut [u16; 8],
    voice_block_sample: &'a mut [u8; 8],
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
                        // KON: each `1` bit keys the matching voice
                        // ON. We also walk the BRR sample directory
                        // to find that voice's starting block, so
                        // tick_one_sample can advance through real
                        // BRR data and latch ENDX correctly at the
                        // sample's end / loop boundaries.
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
                                // Sample directory at `DIR << 8`.
                                // Each entry: 4 bytes — start_lo,
                                // start_hi, loop_lo, loop_hi. SRCN
                                // selects the entry index.
                                let dir_base = u16::from(self.dsp_regs[0x5D]) << 8;
                                let srcn = self.dsp_regs[0x04 + v * 0x10];
                                let entry = dir_base.wrapping_add(u16::from(srcn).wrapping_mul(4));
                                let start_lo = self.aram[entry as usize];
                                let start_hi = self.aram[entry.wrapping_add(1) as usize];
                                self.voice_block_addr[v] =
                                    u16::from(start_lo) | (u16::from(start_hi) << 8);
                                self.voice_block_sample[v] = 0;
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
                voice_block_addr: &mut apu.voice_block_addr,
                voice_block_sample: &mut apu.voice_block_sample,
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
                voice_block_addr: &mut apu.voice_block_addr,
                voice_block_sample: &mut apu.voice_block_sample,
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
    fn brr_decode_f0_with_no_shift_is_signed_nibble_extension() {
        // Filter 0, range 0 — the decoded sample should be just the
        // signed-extended nibble (no history mix, no shift). We craft
        // a single block of 8 alternating high/low nibbles ($08 = -8
        // and $07 = +7) and verify two consecutive samples decode to
        // those values.
        let mut apu = Apu::new();
        // Header: range 0, filter 0, no end, no loop.
        apu.aram[0x0500] = 0x00;
        // Sample 0 = $8 (= -8), sample 1 = $7 (= +7).
        apu.aram[0x0501] = 0x87;
        apu.voice_block_addr[0] = 0x0500;
        apu.voice_block_sample[0] = 0;
        let s0 = apu.decode_current_brr_sample(0);
        assert_eq!(s0, -8);
        apu.voice_brr_history[0] = [s0, 0, 0, 0];
        apu.voice_block_sample[0] = 1;
        let s1 = apu.decode_current_brr_sample(0);
        assert_eq!(s1, 7);
    }

    #[test]
    fn brr_decode_f0_range_4_shifts_nibble_left() {
        // Range 4 means each nibble shifts left by 4 bits before
        // becoming the sample.
        let mut apu = Apu::new();
        apu.aram[0x0500] = 0x40; // range 4, filter 0
        apu.aram[0x0501] = 0x10; // sample 0 nibble = 1 → 1 << 4 = 16
        apu.voice_block_addr[0] = 0x0500;
        apu.voice_block_sample[0] = 0;
        assert_eq!(apu.decode_current_brr_sample(0), 16);
    }

    #[test]
    fn pitch_below_1000_slows_sample_advance() {
        // Set up a sample at $0500. Pitch = $0800 means we should
        // consume one BRR sample every TWO output ticks. Use gain
        // mode with a non-zero direct gain so the envelope stays
        // up and the voice doesn't deactivate.
        let mut apu = Apu::new();
        apu.aram[0x0500] = 0x00; // header: no end, F0, no shift
        apu.aram[0x0501] = 0x12; // samples 0,1 = $1, $2
        apu.voice_block_addr[0] = 0x0500;
        apu.voice_block_sample[0] = 0;
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off; // gain mode
        apu.voice_envelope[0] = 0x7F0;
        apu.dsp_regs[0x02] = 0x00;
        apu.dsp_regs[0x03] = 0x08; // pitch = $0800
        apu.dsp_regs[0x05] = 0x00; // ADSR disabled
        apu.dsp_regs[0x07] = 0x7F; // gain = direct $7F

        // After one sample tick, accumulator = $0800, no advance yet.
        apu.tick_voices(SPC_CYCLES_PER_SAMPLE);
        assert_eq!(apu.voice_block_sample[0], 0);
        // Second tick: accumulator overflows $1000, advances by 1.
        apu.tick_voices(SPC_CYCLES_PER_SAMPLE);
        assert_eq!(apu.voice_block_sample[0], 1);
    }

    #[test]
    fn voice_volume_scales_stereo_mix() {
        // Pre-load voice 0 with a known sample and full envelope
        // (gain mode, so the voice doesn't decay during this tick).
        //
        // The BRR block is filled with $77 in every data byte so
        // every decoded sample is `+7`. After a few ticks the 4-tap
        // Gaussian window holds four `+7` samples and the
        // interpolation recovers the input amplitude exactly.
        let mut apu = Apu::new();
        apu.aram[0x0600] = 0x00; // header — no range, no filter, no end
        for off in 1..=8 {
            apu.aram[0x0600 + off] = 0x77;
        }
        apu.voice_block_addr[0] = 0x0600;
        apu.voice_block_sample[0] = 0;
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off; // gain mode
        apu.voice_envelope[0] = 0x7F0;
        apu.dsp_regs[0x02] = 0x00;
        apu.dsp_regs[0x03] = 0x10; // pitch $1000 = 1:1
        apu.dsp_regs[0x05] = 0x00; // ADSR disabled
        apu.dsp_regs[0x07] = 0x7F; // gain direct = $7F
        // VOL_L = $7F (max positive), VOL_R = 0.
        apu.dsp_regs[0x00] = 0x7F;
        apu.dsp_regs[0x01] = 0x00;
        // Master = $7F / $7F.
        apu.dsp_regs[0x0C] = 0x7F;
        apu.dsp_regs[0x1C] = 0x7F;
        // Clear FLG (default is $E0 = soft-reset + mute + ECEN); the
        // mute bit would otherwise zero the output.
        apu.dsp_regs[0x6C] = 0x00;

        // Run 4 ticks so the Gaussian window fills with four `+7`
        // samples (the first 3 ticks ramp through partially-zero
        // history, the 4th lands on a saturated window).
        for _ in 0..4 {
            apu.tick_voices(SPC_CYCLES_PER_SAMPLE);
        }
        let (l, r) = apu.audio_sample();
        // Right channel should be 0 (vol_r = 0).
        assert_eq!(r, 0);
        // Left channel should be non-zero.
        assert!(l.abs() > 0, "left should carry the voice's output");
    }

    #[test]
    fn brr_block_advance_lands_on_end_bit_and_loops() {
        // Build a tiny "sample": directory at $0100, sample at $0200
        // with the first block having end + loop set, loop address
        // pointing back at itself. KON voice 0, run enough samples
        // to cross the 16-sample boundary, then verify ENDX was
        // latched and the voice still plays (looping).
        let mut apu = Apu::new();
        // ARAM directory entry 0 at DIR << 8 = $0100:
        //   $0100/$0101 = sample start = $0200
        //   $0102/$0103 = loop start   = $0200
        apu.aram[0x0100] = 0x00;
        apu.aram[0x0101] = 0x02;
        apu.aram[0x0102] = 0x00;
        apu.aram[0x0103] = 0x02;
        // Sample at $0200: header byte $03 = end + loop bits, no
        // range, no filter. Body (8 bytes of zero) doesn't matter
        // since we don't decode samples.
        apu.aram[0x0200] = 0x03;

        // Wire DSP registers: DIR ($5D) = 1 (page $0100); SRCN of
        // voice 0 ($04) = 0 → uses directory entry 0; ADSR enabled
        // so the voice stays alive through Release rather than
        // dying via the age-based timeout.
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
                voice_block_addr: &mut apu.voice_block_addr,
                voice_block_sample: &mut apu.voice_block_sample,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
            };
            // DIR = 1 (sample dir at $0100).
            bus.write(0x00F2, 0x5D);
            bus.write(0x00F3, 0x01);
            // Voice 0 ADSR1 = $8F (ADSR enabled, fast attack so the
            // envelope stays well above 0 while we run the test).
            bus.write(0x00F2, 0x05);
            bus.write(0x00F3, 0x8F);
            // Voice 0 SRCN = 0.
            bus.write(0x00F2, 0x04);
            bus.write(0x00F3, 0x00);
            // Voice 0 pitch = $1000 (1:1 rate so each tick advances
            // one BRR sample).
            bus.write(0x00F2, 0x02);
            bus.write(0x00F3, 0x00);
            bus.write(0x00F2, 0x03);
            bus.write(0x00F3, 0x10);
            // KON voice 0.
            bus.write(0x00F2, 0x4C);
            bus.write(0x00F3, 0x01);
        }
        // Voice now points at sample $0200.
        assert_eq!(apu.voice_block_addr[0], 0x0200);
        assert_eq!(apu.voice_block_sample[0], 0);

        // Run 16 samples — exactly one block boundary. Header has
        // end + loop, so ENDX bit 0 should fire and block_addr
        // should jump to loop_addr = $0200.
        apu.tick_voices(SPC_CYCLES_PER_SAMPLE * 16);
        assert!(apu.dsp_regs[0x7C] & 0x01 != 0, "ENDX bit 0 should be set");
        assert_eq!(apu.voice_block_addr[0], 0x0200, "loop back to sample start");
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
                voice_block_addr: &mut apu.voice_block_addr,
                voice_block_sample: &mut apu.voice_block_sample,
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
            voice_block_addr: &mut apu.voice_block_addr,
            voice_block_sample: &mut apu.voice_block_sample,
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
                voice_block_addr: &mut apu.voice_block_addr,
                voice_block_sample: &mut apu.voice_block_sample,
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
            voice_block_addr: &mut apu.voice_block_addr,
            voice_block_sample: &mut apu.voice_block_sample,
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

    // ====================================================================
    // Echo subsystem
    // ====================================================================

    /// Build a hand-loaded APU with voice 0 wired up to produce a
    /// known impulse value `imp` on its mix output every sample tick
    /// (no ADSR, gain mode, fully open VOL_L/R/MVOL_L/R). Returns
    /// the assembled APU.
    fn apu_with_impulse_voice(imp: i16) -> Apu {
        let mut apu = Apu::new();
        // Seed voice 0's history so the linear interp consistently
        // returns `imp` regardless of pitch_acc fraction.
        apu.voice_brr_history[0] = [imp, imp, imp, imp];
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off; // gain mode
        apu.voice_envelope[0] = 0x7FF; // full
        apu.dsp_regs[0x00] = 0x7F; // VOL_L
        apu.dsp_regs[0x01] = 0x7F; // VOL_R
        apu.dsp_regs[0x05] = 0x00; // ADSR disabled
        apu.dsp_regs[0x07] = 0x7F; // gain = direct $7F
        apu.dsp_regs[0x02] = 0x00; // pitch low
        apu.dsp_regs[0x03] = 0x10; // pitch high → $1000 = unity rate
        // Master volume open.
        apu.dsp_regs[0x0C] = 0x7F;
        apu.dsp_regs[0x1C] = 0x7F;
        // Clear FLG (default $E0 = soft-reset + mute + ECEN). Tests
        // that want to assert echo writes must clear it themselves.
        apu.dsp_regs[0x6C] = 0x00;
        // ARAM zero everywhere; echo buffer starts clean.
        apu.echo_pos_samples = 0;
        apu
    }

    /// Read one stereo sample from the echo buffer at the given
    /// position (in samples).
    fn read_echo_sample(apu: &Apu, esa: u8, pos: u16) -> (i16, i16) {
        let base = u16::from(esa) << 8;
        let addr = base.wrapping_add(pos.wrapping_mul(4));
        let l = i16::from_le_bytes([
            apu.aram[addr as usize],
            apu.aram[addr.wrapping_add(1) as usize],
        ]);
        let r = i16::from_le_bytes([
            apu.aram[addr.wrapping_add(2) as usize],
            apu.aram[addr.wrapping_add(3) as usize],
        ]);
        (l, r)
    }

    #[test]
    fn echo_with_eon_off_leaves_buffer_untouched() {
        // EON=0 means no voice contributes to echo_in; with a clean
        // (zeroed) buffer and no FIR taps loaded, the echo path
        // writes back `0 + 0*EFB = 0`. The buffer must remain a
        // sea of zeros after many ticks.
        let mut apu = apu_with_impulse_voice(0x2000);
        apu.dsp_regs[0x6D] = 0x10; // ESA = $1000
        apu.dsp_regs[0x7D] = 0x01; // EDL = 1 → 512-sample buffer
        apu.dsp_regs[0x4D] = 0x00; // EON = 0
        for _ in 0..600 {
            apu.tick_one_sample();
        }
        // Spot-check 4 buffer slots — all still zero.
        for pos in [0u16, 1, 100, 500] {
            assert_eq!(read_echo_sample(&apu, 0x10, pos), (0, 0));
        }
    }

    #[test]
    fn echo_eon_writes_voice_output_into_buffer() {
        // Wire the voice to feed echo (EON.0 = 1), kill the FIR so
        // we don't get any feedback, and verify a freshly-written
        // buffer slot carries the voice's post-volume output.
        let mut apu = apu_with_impulse_voice(0x1000);
        apu.dsp_regs[0x6D] = 0x10; // ESA = $1000
        apu.dsp_regs[0x7D] = 0x01;
        apu.dsp_regs[0x4D] = 0x01; // EON voice 0
        apu.dsp_regs[0x0D] = 0x00; // EFB = 0
        // All FIR taps = 0 → echo_out = 0 → write-back = echo_in.
        for i in 0..8 {
            apu.dsp_regs[0x0F + i * 0x10] = 0x00;
        }
        // One tick — pos 0 gets written, then advances.
        apu.tick_one_sample();
        // The buffer at position 0 should now hold ~ the voice's
        // L/R contribution. With history=[0x1000, 0x1000] and
        // pitch_acc=0 the interp lands on the previous sample
        // (0x1000), env=0x7FF → outx ≈ 0x1000 * 0x7FF / 0x800 ≈ 0x0FFF,
        // then * VOL_L($7F) / 128 ≈ 0x0FFF. Allow some slop.
        let (l, r) = read_echo_sample(&apu, 0x10, 0);
        assert!(l > 0x0F00 && l < 0x1100, "expected ~$1000, got ${l:04X}");
        assert!(r > 0x0F00 && r < 0x1100, "expected ~$1000, got ${r:04X}");
        // Next buffer slot still zero (we only ticked once).
        assert_eq!(read_echo_sample(&apu, 0x10, 1), (0, 0));
        // Ring position has advanced.
        assert_eq!(apu.echo_pos_samples, 1);
    }

    #[test]
    fn echo_fir_identity_replays_buffer_to_output() {
        // FIR with only the newest tap = $7F (≈ unity) reads back
        // whatever was previously written. We pre-seed the buffer
        // and confirm `process_echo` returns that seed (modulo the
        // signed-7-bit shift).
        let mut apu = Apu::new();
        apu.dsp_regs[0x6D] = 0x20; // ESA = $2000
        apu.dsp_regs[0x7D] = 0x01;
        apu.dsp_regs[0x4D] = 0x00;
        apu.dsp_regs[0x0D] = 0x00;
        // FIR: only tap 7 (newest) = $7F.
        for i in 0..7 {
            apu.dsp_regs[0x0F + i * 0x10] = 0x00;
        }
        apu.dsp_regs[0x0F + 7 * 0x10] = 0x7F;
        // FLG = $20 (ECEN = 1, echo write disabled) so the buffer
        // doesn't get clobbered by this tick's `echo_in=0` write.
        apu.dsp_regs[0x6C] = 0x20;
        // Pre-seed the "newest" tap slot. With echo_pos_samples = 1
        // the FIR's tap[7] reads pos - 1 = 0, where we put the seed.
        apu.echo_pos_samples = 1;
        let base: u16 = 0x2000;
        apu.aram[base as usize] = 0x00;
        apu.aram[(base + 1) as usize] = 0x10; // L = $1000
        apu.aram[(base + 2) as usize] = 0x00;
        apu.aram[(base + 3) as usize] = 0xF0; // R = $F000 (= -4096)
        let (l, r) = apu.process_echo(0, 0);
        // out = (sample * $7F) >> 7 ≈ sample.
        assert!(l > 0x0F00 && l < 0x1100, "expected ~+$1000, got {l}");
        assert!(r < -0x0F00 && r > -0x1100, "expected ~-$1000, got {r}");
    }

    #[test]
    fn echo_efb_creates_exponential_decay() {
        // Single sample seeded in a 4-sample buffer (so it cycles
        // fast). FIR identity on newest, EFB = $40 (half feedback),
        // EON = 0 (no new input). The buffer should decay by ~50%
        // per cycle through the ring.
        let mut apu = Apu::new();
        apu.dsp_regs[0x6D] = 0x30; // ESA = $3000
        // EDL=1 = 512 samples; for the test we just need it > 8 so
        // the FIR doesn't wrap onto itself within the first cycle.
        apu.dsp_regs[0x7D] = 0x01;
        apu.dsp_regs[0x4D] = 0x00;
        apu.dsp_regs[0x0D] = 0x40; // EFB ≈ 1/2
        apu.dsp_regs[0x6C] = 0x00; // ECEN = 0 (writes enabled)
        for i in 0..7 {
            apu.dsp_regs[0x0F + i * 0x10] = 0x00;
        }
        apu.dsp_regs[0x0F + 7 * 0x10] = 0x7F;
        // Seed sample at position 0 with a large positive value.
        apu.echo_pos_samples = 1; // so the seed (pos 0) is the
                                  // "newest" tap on the first call
        let base: u16 = 0x3000;
        apu.aram[base as usize] = 0x00;
        apu.aram[(base + 1) as usize] = 0x40; // L = $4000
        apu.aram[(base + 2) as usize] = 0x00;
        apu.aram[(base + 3) as usize] = 0x40;
        // Run one tick. The FIR reads $4000 at the newest tap, so
        // echo_out ≈ $4000, write-back = echo_in(0) + ($4000 * $40)/128
        //   ≈ $2000. The new sample lands at pos 1.
        let (out_l, _) = apu.process_echo(0, 0);
        assert!(out_l > 0x3F00 && out_l < 0x4100, "FIR pass-through: got ${out_l:04X}");
        let (l1, _) = read_echo_sample(&apu, 0x30, 1);
        assert!(l1 > 0x1F00 && l1 < 0x2100, "1st feedback: got ${l1:04X}");
        // Second tick — the FIR's newest tap is now the $2000 we
        // just wrote. New write ≈ ($2000 * $40)/128 = $1000.
        let (_, _) = apu.process_echo(0, 0);
        let (l2, _) = read_echo_sample(&apu, 0x30, 2);
        assert!(l2 > 0x0F00 && l2 < 0x1100, "2nd feedback: got ${l2:04X}");
    }

    #[test]
    fn echo_write_disable_freezes_buffer_but_reads_still_fir() {
        // Pre-seed a buffer slot, set FLG.5 (ECEN=1 → echo writes
        // disabled), and verify that:
        //   (a) the FIR output reflects what's in the buffer, and
        //   (b) the buffer is NOT overwritten by the would-be
        //       echo_in + EFB*out write-back.
        let mut apu = apu_with_impulse_voice(0x2000);
        apu.dsp_regs[0x6D] = 0x40; // ESA = $4000
        apu.dsp_regs[0x7D] = 0x01;
        apu.dsp_regs[0x4D] = 0x01; // EON voice 0 — would write echo_in
        apu.dsp_regs[0x0D] = 0x7F; // EFB high — would amplify feedback
        apu.dsp_regs[0x6C] = 0x20; // ECEN = 1 → writes disabled
        for i in 0..7 {
            apu.dsp_regs[0x0F + i * 0x10] = 0x00;
        }
        apu.dsp_regs[0x0F + 7 * 0x10] = 0x7F;
        // Seed: position 0 holds $1234 / $5678.
        let base: u16 = 0x4000;
        apu.aram[base as usize] = 0x34;
        apu.aram[(base + 1) as usize] = 0x12;
        apu.aram[(base + 2) as usize] = 0x78;
        apu.aram[(base + 3) as usize] = 0x56;
        apu.echo_pos_samples = 1; // so pos 0 is the "newest" tap
        let (out_l, out_r) = apu.process_echo(0x7FFF, 0x7FFF); // huge echo_in
        // FIR pass-through delivers the seed values.
        assert!(out_l > 0x1100 && out_l < 0x1300, "FIR L: got ${out_l:04X}");
        assert!(out_r > 0x5500 && out_r < 0x5800, "FIR R: got ${out_r:04X}");
        // Buffer at position 1 (the write slot) must still be all
        // zero — the write was suppressed by ECEN.
        assert_eq!(read_echo_sample(&apu, 0x40, 1), (0, 0));
        // Original seed at position 0 is untouched.
        assert_eq!(read_echo_sample(&apu, 0x40, 0), (0x1234, 0x5678));
    }

    #[test]
    fn echo_volume_scales_fir_output_into_main_mix() {
        // EVOL_L / EVOL_R scale the FIR output before it's mixed
        // with the main signal. We drive an impulse voice (no EON),
        // pre-seed the echo buffer, and verify the FIR output ends
        // up in the audio_sample via EVOL.
        let mut apu = apu_with_impulse_voice(0); // silent main
        apu.dsp_regs[0x6D] = 0x50;
        apu.dsp_regs[0x7D] = 0x01;
        apu.dsp_regs[0x4D] = 0x00; // EON = 0
        apu.dsp_regs[0x0D] = 0x00; // EFB = 0
        apu.dsp_regs[0x6C] = 0x20; // ECEN = 1 (don't clobber seed)
        apu.dsp_regs[0x2C] = 0x40; // EVOL_L = $40 (≈ half)
        apu.dsp_regs[0x3C] = 0x00; // EVOL_R = 0
        for i in 0..7 {
            apu.dsp_regs[0x0F + i * 0x10] = 0x00;
        }
        apu.dsp_regs[0x0F + 7 * 0x10] = 0x7F;
        // Seed: buffer slot 0 has L=$4000, R=$4000.
        let base: u16 = 0x5000;
        apu.aram[(base) as usize] = 0x00;
        apu.aram[(base + 1) as usize] = 0x40;
        apu.aram[(base + 2) as usize] = 0x00;
        apu.aram[(base + 3) as usize] = 0x40;
        apu.echo_pos_samples = 1;
        apu.tick_one_sample();
        let (l, r) = apu.audio_sample();
        // FIR gives ≈ $4000, EVOL_L half → final_l ≈ $2000.
        assert!(l > 0x1E00 && l < 0x2200, "L expected ~$2000, got ${l:04X}");
        // EVOL_R = 0 → R stays silent.
        assert_eq!(r, 0);
    }

    // ====================================================================
    // Gaussian interpolation
    // ====================================================================

    #[test]
    fn gaussian_table_is_monotone_increasing() {
        // The canonical 512-entry Gaussian table grows from 0 at
        // index 0 to its peak at index 511. Verify by walking the
        // table and asserting non-decreasing — small wobbles from
        // floating-point rounding are OK as long as no entry
        // *decreases*.
        let t = gaussian_table();
        assert_eq!(t[0], 0, "table[0] should be 0");
        for i in 1..512 {
            assert!(
                t[i] >= t[i - 1],
                "table monotonicity broken at i={i}: t[{}]={} > t[{i}]={}",
                i - 1,
                t[i - 1],
                t[i],
            );
        }
        // Peak around $519 (= 1305) per the canonical table.
        assert!(
            t[511] > 0x500 && t[511] < 0x520,
            "table[511] expected ~$519, got ${:X}",
            t[511]
        );
    }

    #[test]
    fn gaussian_4tap_sum_is_near_2048() {
        // The normalisation targets a 4-tap sum of exactly 2048 for
        // each phase — the `>> 11` in the interpolation formula
        // then near-preserves input amplitude on a flat signal. The
        // individual table entries are independently rounded
        // (`+0.5` per ares), so the sum can come out as 2047, 2048,
        // or 2049 depending on phase. Test the tolerance, not the
        // exact equality.
        let t = gaussian_table();
        for phase in 0..128 {
            let sum = i32::from(t[phase])
                + i32::from(t[255 - phase])
                + i32::from(t[256 + phase])
                + i32::from(t[511 - phase]);
            assert!(
                (2047..=2049).contains(&sum),
                "phase={phase}: sum was {sum}, expected 2047..=2049",
            );
        }
    }

    #[test]
    fn gaussian_flat_signal_preserves_amplitude() {
        // History full of $1000. At any frac, the interpolation must
        // recover $1000 exactly (4-tap weights normalised to 2048,
        // shift by 11 → unity gain on a flat input).
        let mut apu = Apu::new();
        apu.voice_brr_history[0] = [0x1000, 0x1000, 0x1000, 0x1000];
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off;
        apu.voice_envelope[0] = 0x7FF; // full
        apu.dsp_regs[0x00] = 0x7F; // VOL_L
        apu.dsp_regs[0x01] = 0x7F; // VOL_R
        apu.dsp_regs[0x0C] = 0x7F; // MVOL_L
        apu.dsp_regs[0x1C] = 0x7F; // MVOL_R
        apu.dsp_regs[0x6C] = 0x00; // clear FLG mute
        apu.dsp_regs[0x07] = 0x7F; // gain direct
        // pitch = 0 so we never advance BRR (no shifts to history)
        apu.dsp_regs[0x02] = 0x00;
        apu.dsp_regs[0x03] = 0x00;
        // Run a few ticks at different fractional offsets.
        for offset in [0u16, 0x400, 0x800, 0xC00] {
            apu.voice_pitch_acc[0] = offset;
            // re-seed history (in case any prior tick advanced it)
            apu.voice_brr_history[0] = [0x1000, 0x1000, 0x1000, 0x1000];
            apu.tick_one_sample();
            let (l, _) = apu.audio_sample();
            // Expected ≈ $1000 × env($7FF)/0x800 × vol_l($7F)/$80
            //           × mvol_l($7F)/$80 ≈ $0FE2 (a few LSB of slop
            // from each shift). Allow ±10%.
            assert!(
                l > 0x0E00 && l < 0x1100,
                "offset=${offset:04X}: expected ~$1000, got ${l:04X}",
            );
        }
    }

    // ====================================================================
    // Noise generator
    // ====================================================================

    #[test]
    fn noise_lfsr_advances_and_has_full_period() {
        // 15-bit Galois LFSR with taps at bits 0,1 visits exactly
        // 32767 distinct states (every non-zero u15) before cycling.
        // Verify both that one tick advances the state, and that
        // 32767 ticks return it to the start.
        let mut apu = Apu::new();
        apu.dsp_regs[0x6C] = 0x1F; // FLG[4:0] = 0x1F → period = 1 sample
        let start = apu.noise_lfsr;
        apu.tick_one_sample();
        assert_ne!(apu.noise_lfsr, start, "LFSR should step on first tick");
        // 32766 more ticks → total of 32767 = full cycle → back to start.
        for _ in 0..32766 {
            apu.tick_one_sample();
        }
        assert_eq!(
            apu.noise_lfsr, start,
            "LFSR should cycle back to start after 32767 steps"
        );
    }

    #[test]
    fn noise_replaces_sample_when_non_bit_set() {
        // When NON.0 is set, voice 0's source is the LFSR — not the
        // (zeroed) BRR history. Run a few ticks and confirm voice 0
        // contributes a non-zero (and varying) signal to the mix.
        let mut apu = Apu::new();
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off;
        apu.voice_envelope[0] = 0x7FF;
        apu.dsp_regs[0x00] = 0x7F; // VOL_L
        apu.dsp_regs[0x01] = 0x7F; // VOL_R
        apu.dsp_regs[0x0C] = 0x7F; // MVOL_L
        apu.dsp_regs[0x1C] = 0x7F; // MVOL_R
        apu.dsp_regs[0x6C] = 0x1F; // FLG: clear mute/reset/ECEN, noise rate 0x1F
        apu.dsp_regs[0x07] = 0x7F; // gain direct full
        apu.dsp_regs[0x02] = 0x00;
        apu.dsp_regs[0x03] = 0x00; // pitch=0
        apu.dsp_regs[0x3D] = 0x01; // NON voice 0
        // BRR history all zero — without NON the output would be 0.
        let mut samples = Vec::new();
        for _ in 0..16 {
            apu.tick_one_sample();
            samples.push(apu.audio_left);
        }
        let nonzero = samples.iter().filter(|s| **s != 0).count();
        assert!(
            nonzero > 8,
            "expected noise to drive >8/16 ticks non-zero, got {nonzero}: {samples:?}"
        );
        let unique: std::collections::HashSet<_> = samples.iter().collect();
        assert!(
            unique.len() > 4,
            "noise output should be variable, only {} distinct values",
            unique.len()
        );
    }

    // ====================================================================
    // Gain modes
    // ====================================================================

    /// Helper: build a voice in gain mode with the given GAIN byte
    /// and step it `ticks` times, returning the envelope after.
    fn step_gain_voice(gain: u8, initial_env: u16, ticks: u32) -> u16 {
        let mut apu = Apu::new();
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off;
        apu.voice_envelope[0] = initial_env;
        apu.dsp_regs[0x05] = 0x00; // ADSR disabled
        apu.dsp_regs[0x07] = gain;
        apu.dsp_regs[0x6C] = 0x00; // clear FLG mute / reset
        apu.dsp_regs[0x0C] = 0x7F; // MVOL — irrelevant but defensive
        apu.dsp_regs[0x1C] = 0x7F;
        for _ in 0..ticks {
            apu.tick_one_sample();
        }
        apu.voice_envelope[0]
    }

    #[test]
    fn gain_direct_mode_uses_register_value() {
        // GAIN < 0x80 → direct: envelope = (gain << 4), clamped to 0x7FF.
        assert_eq!(step_gain_voice(0x40, 0, 1), 0x400);
        assert_eq!(step_gain_voice(0x7F, 0, 1), 0x7F0);
    }

    #[test]
    fn gain_custom_linear_increase_ramps_up_by_0x20() {
        // GAIN bit 7 = 1, bits 6:5 = 10 (linear increase),
        // bits 4:0 = 0x1F (fastest rate, period = 1 sample).
        let gain = 0x80 | (0b10 << 5) | 0x1F; // = 0xDF
        // After 5 ticks at +0x20 per tick (with the first tick
        // ramping from age=0): envelope should be ~5 * 0x20 = 0xA0.
        let env = step_gain_voice(gain, 0, 5);
        assert!(env >= 0x80 && env <= 0xC0, "got {env:#X}");
    }

    #[test]
    fn gain_custom_linear_decrease_ramps_down_by_0x20() {
        let gain = 0x80 | (0b00 << 5) | 0x1F; // = 0x9F
        let env = step_gain_voice(gain, 0x7FF, 5);
        // 0x7FF - 5*0x20 = 0x73F; allow some slop.
        assert!(env >= 0x6E0 && env <= 0x7E0, "got {env:#X}");
    }

    #[test]
    fn gain_custom_bent_increase_slows_above_0x600() {
        let gain = 0x80 | (0b11 << 5) | 0x1F; // = 0xFF
        // Start at 0x600 — first tick should add 0x08, not 0x20.
        // After 1 tick at age=0 we get +0x20 (still triggers the
        // boundary check correctly), so let's just do a few and
        // observe the slope changes.
        let env_at_5 = step_gain_voice(gain, 0x600, 5);
        let env_at_15 = step_gain_voice(gain, 0x600, 15);
        // Both should be > 0x600 and < 0x7FF. Slope must be < 0x20/tick.
        let slope_per_10 = env_at_15 - env_at_5;
        assert!(env_at_5 > 0x600 && env_at_15 > 0x600, "env didn't rise");
        assert!(
            slope_per_10 < 10 * 0x20,
            "bent-mode slope too steep: got {slope_per_10:#X} for 10 ticks"
        );
    }

    // ====================================================================
    // Pitch modulation (PMON)
    // ====================================================================

    #[test]
    fn pmon_voice0_cannot_be_modulated() {
        // Voice 0 has no predecessor, so PMON.0 is a no-op even
        // when set. Sanity-check the special case.
        let mut apu = Apu::new();
        apu.dsp_regs[0x2D] = 0x01; // PMON.0 (which is ignored)
        // Just make sure the code path doesn't panic on the v=0
        // case (we never read voice_pmod_output[-1]).
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off;
        apu.voice_envelope[0] = 0x7FF;
        apu.dsp_regs[0x07] = 0x7F;
        apu.dsp_regs[0x6C] = 0x00;
        apu.dsp_regs[0x03] = 0x10; // pitch $1000
        apu.tick_one_sample();
        // Sanity: voice 0's pitch_acc must have advanced by ~$1000,
        // not been mangled by reading uninitialised voice_pmod_output.
        // It may have wrapped if a BRR sample was consumed.
        // Just assert no panic.
    }

    #[test]
    fn pmon_voice0_drives_voice1_pitch_swing() {
        // Voice 1's pitch should advance further with PMON.1 set
        // when voice 0's pre-volume output is strongly positive.
        // We run two fresh APUs side-by-side: identical except for
        // the PMON register, and pre-seed voice_pmod_output[0] so
        // the modulation factor is sampled on the very first tick.
        fn setup() -> Apu {
            let mut apu = Apu::new();
            apu.voice_active[0] = true;
            apu.voice_active[1] = true;
            apu.voice_phase[0] = AdsrPhase::Off;
            apu.voice_phase[1] = AdsrPhase::Off;
            apu.voice_envelope[0] = 0x7FF;
            apu.voice_envelope[1] = 0x7FF;
            apu.voice_brr_history[0] = [0x7FFF, 0x7FFF, 0x7FFF, 0x7FFF];
            apu.dsp_regs[0x07] = 0x7F; // V0 gain direct
            apu.dsp_regs[0x17] = 0x7F; // V1 gain direct
            apu.dsp_regs[0x6C] = 0x00;
            apu.dsp_regs[0x13] = 0x08; // V1 pitch high $800
            // Pre-load: voice 1 reads this on its pitch step before
            // voice 0 has run its own first sample.
            apu.voice_pmod_output[0] = 0x7FFF;
            apu
        }
        let mut apu_no_mod = setup();
        let mut apu_mod = setup();
        apu_mod.dsp_regs[0x2D] = 0x02; // PMON.1
        apu_mod.tick_one_sample();
        apu_no_mod.tick_one_sample();
        assert!(
            apu_mod.voice_pitch_acc[1] > apu_no_mod.voice_pitch_acc[1],
            "PMON should accelerate v1's pitch: mod={:#X} vs nomod={:#X}",
            apu_mod.voice_pitch_acc[1],
            apu_no_mod.voice_pitch_acc[1]
        );
    }

    #[test]
    fn gaussian_at_frac_zero_picks_prev_old_sample() {
        // At frac=0, TABLE[511] is the peak coefficient. In the
        // ares-matching 4-tap order, that coefficient pairs with the
        // *third-newest* sample (`history[2]`). Verify by seeding
        // only that slot and confirming the interpolation reflects
        // primarily that value.
        let mut apu = Apu::new();
        apu.voice_brr_history[0] = [0, 0, 0x4000, 0];
        apu.voice_active[0] = true;
        apu.voice_phase[0] = AdsrPhase::Off;
        apu.voice_envelope[0] = 0x7FF;
        apu.dsp_regs[0x00] = 0x7F; // VOL_L
        apu.dsp_regs[0x0C] = 0x7F; // MVOL_L
        apu.dsp_regs[0x6C] = 0x00;
        apu.dsp_regs[0x07] = 0x7F;
        apu.dsp_regs[0x02] = 0x00;
        apu.dsp_regs[0x03] = 0x00; // pitch=0, no BRR advance
        apu.voice_pitch_acc[0] = 0;
        apu.tick_one_sample();
        let (l, _) = apu.audio_sample();
        // Expected ≈ $4000 × TABLE[511] / 2048 × env × vol × mvol
        //          ≈ $4000 × 1305/2048 × ~unity gain ≈ $2800
        // Allow ±20%.
        assert!(
            l > 0x2000 && l < 0x3000,
            "expected ~$2800, got ${l:04X}",
        );
    }
}
