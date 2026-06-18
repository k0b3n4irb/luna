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

use luna_cpu_spc700::{IPL_ROM, IPL_ROM_BASE, SPC700_CYCLES, Spc700, SpcBus};

/// Read one ARAM byte as the **SPC700** sees it: `$FFC0-$FFFF` returns
/// the 64-byte IPL ROM while `$F1` bit 7 is set, otherwise the physical
/// RAM underneath. The DSP reads physical ARAM directly and bypasses
/// this overlay (ares: the IPL mapping is SMP-side only).
const fn aram_with_ipl(aram: &[u8; 0x10000], control: u8, addr: u16) -> u8 {
    if addr >= IPL_ROM_BASE && control & 0x80 != 0 {
        IPL_ROM[(addr - IPL_ROM_BASE) as usize]
    } else {
        aram[addr as usize]
    }
}

/// Cycle-accurate ares port of the S-DSP — the live audio path. See
/// `dsp.rs` for the 1-for-1 transliteration of `ares/sfc/dsp/*`.
pub mod dsp;

/// `serde` helper for a heap-boxed fixed byte array (`Box<[u8; N]>`),
/// which `serde_bytes` does not cover directly. Used by the save-state
/// machinery for the 64 KB ARAM.
pub(crate) mod boxed_byte_array {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize `Box<[u8; N]>` as raw bytes (the `&Box` from serde's
    /// `with` call site deref-coerces to this `&[u8; N]`).
    pub(crate) fn serialize<S, const N: usize>(
        data: &[u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&data[..])
    }

    /// Deserialize a byte blob back into `Box<[u8; N]>` (length must match).
    pub(crate) fn deserialize<'de, D, const N: usize>(
        deserializer: D,
    ) -> Result<Box<[u8; N]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <serde_bytes::ByteBuf>::deserialize(deserializer)?;
        let arr: [u8; N] = bytes
            .into_vec()
            .try_into()
            .map_err(|_| serde::de::Error::custom("byte array length mismatch"))?;
        Ok(Box::new(arr))
    }
}

/// Nominal master cycles per SPC instruction (~4 SPC cycles × the
/// 20.97 master/SPC ratio). Since Phase 2 this is **no longer** the
/// scheduler quantum — `Apu::step` charges each instruction its real
/// per-opcode cost — but it is kept as a convenience multiplier for
/// tests that want "≈N instructions of headroom".
pub const MASTER_CYCLES_PER_SPC_STEP: u32 = 84;

/// NTSC SNES master clock (Hz) — the CPU/PPU timebase.
pub const MASTER_CLOCK_HZ: u64 = 21_477_272;

/// SPC700 / S-DSP clock (Hz): the APU crystal ÷ 24. The crystal is
/// nominally 24.576 MHz (→ 1.024 MHz) but real hardware measures
/// ~24.607 MHz; ares (`apuFrequency = 32040·768`) and Mesen2 both use the
/// measured value → `24_606_720` ÷ 24 = **`1_025_280` Hz** (and a `32_040` Hz
/// DSP output). luna's textbook 1.024 MHz ran the SPC ~0.125 % slow, which
/// shifts the CPU↔SPC clock alignment during the boot/upload handshake
/// (differential vs ares: this value moves luna's IPL-upload-loop exit
/// measurably closer to ares — necessary, though not alone sufficient,
/// for the Tales of Phantasia OP).
pub const SPC_CLOCK_HZ: u64 = 1_025_280;

/// SPC cycles per audio sample: 32 SPC cycles → one DSP sample. At
/// [`SPC_CLOCK_HZ`] this yields the `32_040` Hz output rate ares and Mesen2
/// produce (the host audio backend resamples to the device rate).
pub const SPC_CYCLES_PER_SAMPLE: u32 = 32;

/// Maximum number of stereo samples buffered in `audio_queue`. The
/// host audio backend drains it each frame; bursts beyond this cap are
/// dropped to keep emulator-side memory bounded. ~16k samples = 512 ms
/// at 32 kHz, plenty of headroom for normal frame cadence.
pub const AUDIO_QUEUE_CAPACITY: usize = 16384;

/// One pre-opcode SPC700 register snapshot for the instruction trace
/// (`--spc-trace`). Mirrors the SA-1 / Super FX trace events; diff the PC
/// stream against a Mesen2 SPC700 trace to localise audio-driver
/// divergences (e.g. the SMRPG/CT Akao CPU↔SPC handshake).
#[derive(Clone, Copy, Debug)]
pub struct Spc700TraceEvent {
    /// 16-bit SPC700 PC before the opcode runs.
    pub pc: u16,
    /// Accumulator.
    pub a: u8,
    /// X index.
    pub x: u8,
    /// Y index.
    pub y: u8,
    /// Stack pointer.
    pub sp: u8,
    /// Processor status word (`PSW`).
    pub psw: u8,
    /// Running SPC-cycle counter (`timer_subdivider`) at this opcode — for the
    /// SPC-cycle differential vs Mesen `spc.cycle`. Wraps at 2^32 (~70 min).
    pub spc_cycle: u32,
    /// T2 internal counter (`timer_internal[2]`, vs Mesen `spc.timer2.stage2`).
    pub t2_int: u16,
    /// T2 output (`timer_output[2]`, vs Mesen `spc.timer2.stage3`) — the value
    /// `$FF`/`CBNE $FF` reads (and clears).
    pub t2_out: u8,
}

/// All APU state owned by [`Apu`]: SPC700 core + 64 KB ARAM + the two
/// 4-byte mailbox arrays.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Apu {
    /// The SPC700 CPU.
    pub cpu: Spc700,
    /// Cycle-accurate ares-port S-DSP. Owns its own register file
    /// (mirrored into `dsp_regs` for legacy introspection), Voice[8],
    /// Echo, Noise, BRR, Latch, Clock, `MainVol` state. `tick_voices`
    /// drives one `dsp.main()` per 32 SPC cycles → one stereo sample.
    pub dsp: dsp::Dsp,
    /// 64 KB of physical audio RAM. The 64-byte IPL ROM is *not* baked
    /// in — it's a read overlay over `$FFC0..=$FFFF` on the SPC side,
    /// gated on `$F1` bit 7 (see [`aram_with_ipl`] / [`Apu::peek`]). The
    /// DSP reads this array directly, bypassing the overlay.
    #[serde(with = "boxed_byte_array")]
    pub aram: Box<[u8; 0x10000]>,
    /// CPU → SPC mailbox (CPU writes `$2140-$2143`, SPC reads `$F4-$F7`).
    pub to_spc_ports: [u8; 4],
    /// SPC → CPU mailbox (SPC writes `$F4-$F7`, CPU reads `$2140-$2143`).
    pub to_cpu_ports: [u8; 4],
    /// SPIKE (timestamped-mailbox visibility): a CPU write to `$2140+port`
    /// is held here until the SPC's cycle (`timer_subdivider`) catches up
    /// to the CPU's write cycle, then committed to `to_spc_ports`. This
    /// makes CPU→SPC visibility cycle-exact instead of quantized to luna's
    /// whole-instruction APU stepping. `None` = no pending write.
    #[serde(default)]
    to_spc_pending: [Option<u8>; 4],
    /// SPC cycle (`timer_subdivider` value) at which each pending write
    /// becomes visible (= the CPU's write cycle).
    #[serde(default)]
    to_spc_visible_at: [u32; 4],
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
    /// `$F0` TEST register. Bit 0 = `timersDisable`, bit 3 =
    /// `timersEnable` (ares io.cpp:81-94) — together they gate timer
    /// advance. Reset default `0x0A` (timersEnable + ramWritable set)
    /// keeps timers running, matching the ares power-on state. The
    /// RAM-writable/disable and wait-state bits are stored, not modelled.
    pub test: u8,
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
    ///
    /// Transient playback buffer — not part of the save-state. It
    /// `serde(skip)`-defaults to an empty queue on restore.
    #[serde(skip)]
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

    /// Optional full SPC700 instruction trace: `(events, max_events)`. A
    /// pre-opcode register snapshot per SPC700 instruction, capped at
    /// `max_events` (ring buffer: drops the oldest half when full).
    /// Transient — `serde(skip)` so it never enters a save-state.
    #[serde(skip)]
    spc_trace: Option<(Vec<Spc700TraceEvent>, usize)>,
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
        // ARAM is physical RAM only. The 64-byte IPL ROM is a separate
        // read overlay over $FFC0-$FFFF (gated on $F1 bit 7, applied in
        // the bus read path) rather than baked in — so a game that
        // clears bit 7 reclaims the underlying RAM, and the DSP always
        // reads physical ARAM.
        let aram: Box<[u8; 0x10000]> = vec![0u8; 0x10000]
            .into_boxed_slice()
            .try_into()
            .expect("64 KB slice into fixed array");
        let mut apu = Self {
            cpu: Spc700::new(),
            dsp: dsp::Dsp::new(),
            aram,
            to_spc_ports: [0; 4],
            to_cpu_ports: [0; 4],
            to_spc_pending: [None; 4],
            to_spc_visible_at: [0; 4],
            spc_cycle_num: 0,
            spc_cycle_debt: 0,
            control: 0x80, // bit 7: IPL ROM exposed
            test: 0x0A,    // timersEnable + ramWritable (ares power-on)
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
            spc_trace: None,
        };
        // Reset the SPC700 — reads $FFFE/$FFFF for the PC vector,
        // which the IPL ROM populates as $FFC0.
        let mut bus = ApuBusView {
            aram: &mut apu.aram,
            to_spc_ports: &mut apu.to_spc_ports,
            to_cpu_ports: &mut apu.to_cpu_ports,
            control: &mut apu.control,
            test: &mut apu.test,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
            timer_subdivider: &mut apu.timer_subdivider,
            sample_tick_deficit: &mut apu.sample_tick_deficit,
            audio_queue: &mut apu.audio_queue,
            audio_left: &mut apu.audio_left,
            audio_right: &mut apu.audio_right,
            clocked: 0,
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

        // $F0 TEST master gate (ares timing.cpp:45-49): when timersEnable
        // (bit 3) is clear or timersDisable (bit 0) is set, the stage→
        // output propagation is suppressed (timers freeze). The clock
        // divider (`timer_subdivider`) keeps running above, so phase
        // resumes when re-enabled.
        if self.test & 0x08 == 0 || self.test & 0x01 != 0 {
            return;
        }

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

    /// Read an ARAM byte as the SPC700 sees it — i.e. with the IPL ROM
    /// overlaid over `$FFC0-$FFFF` while `$F1` bit 7 is set. (The DSP
    /// reads physical ARAM directly; use [`Self::aram`] for that.)
    #[must_use]
    pub const fn peek(&self, addr: u16) -> u8 {
        aram_with_ipl(&self.aram, self.control, addr)
    }

    /// Main CPU writes `value` to mailbox port (0..=3).
    ///
    /// SPIKE (timestamped-mailbox visibility): rather than make the byte
    /// instantly visible, hold it until the SPC's cycle catches up to the
    /// CPU's write cycle. `spc_cycle_debt` is how far the SPC currently
    /// lags the CPU clock (≥ 0 under no-overshoot), so the CPU's write
    /// cycle in SPC units is `timer_subdivider + spc_cycle_debt`. The SPC
    /// then sees the write exactly when it reaches that cycle (see the
    /// commit at the top of [`run_one_spc`](Self::run_one_spc)) — cycle-
    /// exact CPU→SPC visibility instead of luna's whole-instruction
    /// quantization (the Tales OP timer-phase derail).
    pub fn cpu_write_port(&mut self, port: usize, value: u8) {
        self.to_spc_pending[port] = Some(value);
        let lag = self.spc_cycle_debt.max(0) as u32;
        self.to_spc_visible_at[port] = self.timer_subdivider.wrapping_add(lag);
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
            // No-overshoot catch-up: peek the next opcode's base cost and
            // only run it if it fits the remaining budget, so the SPC stops
            // at-or-before the CPU's master clock rather than overshooting
            // whole instructions past it. luna's one-way CPU-driven model
            // (the SPC catches up to the CPU, never vice versa) desyncs the
            // mailbox handshake under overshoot — Tales of Phantasia's OP
            // hangs with no sound. Stopping short keeps the SPC's view of
            // the CPU mailbox writes correctly ordered. The leftover budget
            // carries forward in `spc_cycle_debt`.
            let next_op = aram_with_ipl(&self.aram, self.control, self.cpu.pc);
            if i64::from(SPC700_CYCLES[next_op as usize]) > budget {
                break;
            }
            budget -= i64::from(self.run_one_spc());
        }
        self.spc_cycle_debt = budget;
    }

    /// Run exactly one SPC700 instruction over the APU bus, clocking the
    /// timers + S-DSP per cycle in position (the ares grammar), and
    /// reconciling any SLEEP/STOP cycles the core charges without driving
    /// the bus. Returns the instruction's cycle cost. Shared by [`step`]
    /// and the trajectory harness [`trace_step_one`].
    ///
    /// [`step`]: Self::step
    /// [`trace_step_one`]: Self::trace_step_one
    /// Enable the SPC700 instruction trace: a pre-opcode register snapshot
    /// per SPC700 instruction, capped at `max_events` (ring buffer). Drain
    /// with [`Self::take_spc_trace`].
    pub fn enable_spc_trace(&mut self, max_events: usize) {
        self.spc_trace = Some((Vec::new(), max_events));
    }

    /// Drain the captured SPC700 instruction trace (empty if disabled).
    pub fn take_spc_trace(&mut self) -> Vec<Spc700TraceEvent> {
        match self.spc_trace.as_mut() {
            Some((events, _)) => std::mem::take(events),
            None => Vec::new(),
        }
    }

    fn run_one_spc(&mut self) -> u32 {
        // SPIKE: commit any CPU mailbox writes whose visibility cycle the
        // SPC has now reached (cycle-exact CPU→SPC visibility). Wrap-safe:
        // `timer_subdivider` is u32 and wraps (~70 min); a write always
        // commits within microseconds, so a wrapping-distance < 2^31 means
        // "at or past the visibility cycle".
        for p in 0..4 {
            if self.to_spc_pending[p].is_some()
                && self
                    .timer_subdivider
                    .wrapping_sub(self.to_spc_visible_at[p])
                    < 0x8000_0000
            {
                self.to_spc_ports[p] = self.to_spc_pending[p].take().unwrap();
            }
        }
        // SPC700 instruction trace (`--spc-trace`): pre-opcode register
        // snapshot. Copy the registers out first so the trace borrow does
        // not alias `self.cpu`.
        if self.spc_trace.is_some() {
            let ev = Spc700TraceEvent {
                pc: self.cpu.pc,
                a: self.cpu.a,
                x: self.cpu.x,
                y: self.cpu.y,
                sp: self.cpu.sp,
                psw: self.cpu.psw.0,
                spc_cycle: self.timer_subdivider,
                t2_int: self.timer_internal[2],
                t2_out: self.timer_output[2],
            };
            if let Some((events, max)) = self.spc_trace.as_mut() {
                if *max > 0 {
                    if events.len() >= *max {
                        events.drain(0..*max / 2);
                    }
                    events.push(ev);
                }
            }
        }
        let (cycles, clocked) = {
            let mut bus = ApuBusView {
                aram: &mut self.aram,
                to_spc_ports: &mut self.to_spc_ports,
                to_cpu_ports: &mut self.to_cpu_ports,
                control: &mut self.control,
                test: &mut self.test,
                dsp_index: &mut self.dsp_index,
                dsp: &mut self.dsp,
                timer_reload: &mut self.timer_reload,
                timer_output: &mut self.timer_output,
                timer_internal: &mut self.timer_internal,
                timer_enabled: &mut self.timer_enabled,
                timer_subdivider: &mut self.timer_subdivider,
                sample_tick_deficit: &mut self.sample_tick_deficit,
                audio_queue: &mut self.audio_queue,
                audio_left: &mut self.audio_left,
                audio_right: &mut self.audio_right,
                clocked: 0,
            };
            let cycles = u32::from(self.cpu.step(&mut bus));
            (cycles, bus.clocked)
        };
        let unclocked = cycles.saturating_sub(clocked);
        if unclocked > 0 {
            self.tick_timers(unclocked);
            self.tick_voices(unclocked);
        }
        if self.cpu.pc < IPL_ROM_BASE {
            self.past_iplrom = true;
        }
        cycles
    }

    /// Trajectory-harness hook (Tales OP derail differential): capture the
    /// pre-instruction SPC register snapshot `(pc, a, x, y, sp, psw)`, then
    /// free-run exactly one SPC700 instruction (full timer/DSP clocking, a
    /// frozen mailbox), and return the snapshot. Not used in normal
    /// stepping; see `crates/luna-core/tests/spc_trajectory.rs`.
    #[doc(hidden)]
    pub fn trace_step_one(&mut self) -> (u16, u8, u8, u8, u8, u8) {
        let snap = (
            self.cpu.pc,
            self.cpu.a,
            self.cpu.x,
            self.cpu.y,
            self.cpu.sp,
            self.cpu.psw.0,
        );
        self.run_one_spc();
        snap
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
    test: &'a mut u8,
    dsp_index: &'a mut u8,
    dsp: &'a mut dsp::Dsp,
    timer_reload: &'a mut [u8; 3],
    timer_output: &'a mut [u8; 3],
    timer_internal: &'a mut [u16; 3],
    timer_enabled: &'a mut [bool; 3],
    /// SPC-cycle accumulator feeding the timer dividers (128 → T0/T1,
    /// 16 → T2). Advanced one per bus cycle by [`Self::clock_cycle`].
    timer_subdivider: &'a mut u32,
    /// SPC cycles since the last DSP sample; one 32 kHz sample is
    /// produced every [`SPC_CYCLES_PER_SAMPLE`].
    sample_tick_deficit: &'a mut u32,
    audio_queue: &'a mut std::collections::VecDeque<(i16, i16)>,
    audio_left: &'a mut i16,
    audio_right: &'a mut i16,
    /// Count of bus cycles clocked during the current instruction, so
    /// the caller can reconcile any cycles with no bus activity (the
    /// SLEEP/STOP halt window emits fewer bus ops than its cycle cost).
    clocked: u32,
}

/// Advance one SPC timer (0/1/2) by one base-clock tick: bump the
/// internal counter, and on reaching the reload target (0 ⇒ 256) wrap-
/// increment the 4-bit output and reset.
fn tick_one_timer(
    reload: [u8; 3],
    output: &mut [u8; 3],
    internal: &mut [u16; 3],
    enabled: [bool; 3],
    idx: usize,
) {
    if !enabled[idx] {
        return;
    }
    internal[idx] = internal[idx].wrapping_add(1);
    let target = if reload[idx] == 0 {
        256
    } else {
        u16::from(reload[idx])
    };
    if internal[idx] >= target {
        internal[idx] = 0;
        output[idx] = (output[idx] + 1) & 0x0F;
    }
}

impl ApuBusView<'_> {
    /// Clock the timers **and** the S-DSP by exactly one SPC cycle, in
    /// position — the faithful ares grammar (`wait()` → `stepTimers` +
    /// `synchronize(dsp)`; Mesen2 `IncCycleCount` → `Timer.Run` +
    /// `dsp->Exec`). Called at the start of every read / write / idle so
    /// a `$FD-$FF` timer read or a `$F3` DSP write lands on the correct
    /// cycle. This is the whole point of the per-cycle SPC700 port: the
    /// number of these calls per opcode equals its true cycle count.
    fn clock_cycle(&mut self) {
        self.clocked = self.clocked.wrapping_add(1);

        // --- timers ---
        *self.timer_subdivider = self.timer_subdivider.wrapping_add(1);
        let after = *self.timer_subdivider;
        // $F0 TEST gate (ares timing.cpp:45-49): output propagation is
        // suppressed when timersEnable (bit 3) is clear or timersDisable
        // (bit 0) is set. The divider keeps running so phase resumes on
        // re-enable.
        if *self.test & 0x08 != 0 && *self.test & 0x01 == 0 {
            if after % 16 == 0 {
                tick_one_timer(
                    *self.timer_reload,
                    self.timer_output,
                    self.timer_internal,
                    *self.timer_enabled,
                    2,
                );
            }
            if after % 128 == 0 {
                tick_one_timer(
                    *self.timer_reload,
                    self.timer_output,
                    self.timer_internal,
                    *self.timer_enabled,
                    0,
                );
                tick_one_timer(
                    *self.timer_reload,
                    self.timer_output,
                    self.timer_internal,
                    *self.timer_enabled,
                    1,
                );
            }
        }

        // --- S-DSP: one 32 kHz sample every 32 SPC cycles ---
        *self.sample_tick_deficit += 1;
        if *self.sample_tick_deficit >= SPC_CYCLES_PER_SAMPLE {
            *self.sample_tick_deficit -= SPC_CYCLES_PER_SAMPLE;
            let (l, r) = self.dsp.main(self.aram);
            if self.audio_queue.len() < AUDIO_QUEUE_CAPACITY {
                self.audio_queue.push_back((l, r));
            }
            *self.audio_left = l;
            *self.audio_right = r;
        }
    }
}

impl SpcBus for ApuBusView<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        self.clock_cycle();
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
            // Everything else — ARAM, with the IPL ROM overlaid over
            // $FFC0-$FFFF while $F1 bit 7 is set.
            _ => aram_with_ipl(self.aram, *self.control, addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.clock_cycle();
        match addr {
            // $F0 — TEST register. Bit 0 = timersDisable, bit 3 =
            // timersEnable gate the timers (ares io.cpp:81-94); the
            // other bits (RAM writable/disable, wait states) are stored
            // but not yet modelled. The P-flag write gate is omitted
            // (writes with PSW.P set are pathological for $F0).
            0x00F0 => *self.test = value,
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

    fn idle(&mut self) {
        self.clock_cycle();
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
        // The SPC sees the IPL ROM at $FFC0 (overlay), while the physical
        // ARAM underneath is still zero (not baked in).
        assert_eq!(apu.peek(IPL_ROM_BASE), IPL_ROM[0]);
        assert_eq!(apu.aram[IPL_ROM_BASE as usize], 0);
    }

    #[test]
    fn ipl_rom_overlay_toggles_with_f1_bit7() {
        let mut apu = Apu::new();
        // Write some RAM under the IPL ROM region.
        apu.aram[IPL_ROM_BASE as usize] = 0x42;
        // Bit 7 set (reset default) → SPC reads the IPL ROM.
        assert_eq!(apu.peek(IPL_ROM_BASE), IPL_ROM[0]);
        // Clear bit 7 → the underlying RAM is exposed.
        apu.control = 0x00;
        assert_eq!(apu.peek(IPL_ROM_BASE), 0x42);
        // Re-enable → IPL ROM again.
        apu.control = 0x80;
        assert_eq!(apu.peek(IPL_ROM_BASE), IPL_ROM[0]);
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
            test: &mut apu.test,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
            timer_subdivider: &mut apu.timer_subdivider,
            sample_tick_deficit: &mut apu.sample_tick_deficit,
            audio_queue: &mut apu.audio_queue,
            audio_left: &mut apu.audio_left,
            audio_right: &mut apu.audio_right,
            clocked: 0,
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
            test: &mut apu.test,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
            timer_subdivider: &mut apu.timer_subdivider,
            sample_tick_deficit: &mut apu.sample_tick_deficit,
            audio_queue: &mut apu.audio_queue,
            audio_left: &mut apu.audio_left,
            audio_right: &mut apu.audio_right,
            clocked: 0,
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
    fn test_register_gates_timer_advance() {
        // $F0 bit 3 (timersEnable) must be set and bit 0 (timersDisable)
        // clear for timers to advance (ares timing.cpp:45-49).
        let mut apu = Apu::new();
        apu.timer_reload[2] = 1;
        apu.timer_enabled[2] = true;
        // Default test = 0x0A → timers run.
        apu.tick_timers(16 * 2);
        assert_eq!(apu.timer_output[2], 2);
        // timersDisable (bit 0) set → frozen.
        apu.test = 0x0B;
        apu.tick_timers(16 * 4);
        assert_eq!(apu.timer_output[2], 2, "timersDisable freezes the timer");
        // timersEnable (bit 3) clear → also frozen.
        apu.test = 0x00;
        apu.tick_timers(16 * 4);
        assert_eq!(apu.timer_output[2], 2, "!timersEnable freezes the timer");
        // Re-enable → advances again (the clock divider kept running, so
        // it picks up from the current phase).
        apu.test = 0x08;
        apu.tick_timers(16 * 3);
        assert_eq!(apu.timer_output[2], 5, "re-enabled timer advances");
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
                test: &mut apu.test,
                dsp_index: &mut apu.dsp_index,
                dsp: &mut apu.dsp,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
                timer_subdivider: &mut apu.timer_subdivider,
                sample_tick_deficit: &mut apu.sample_tick_deficit,
                audio_queue: &mut apu.audio_queue,
                audio_left: &mut apu.audio_left,
                audio_right: &mut apu.audio_right,
                clocked: 0,
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
            test: &mut apu.test,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
            timer_subdivider: &mut apu.timer_subdivider,
            sample_tick_deficit: &mut apu.sample_tick_deficit,
            audio_queue: &mut apu.audio_queue,
            audio_left: &mut apu.audio_left,
            audio_right: &mut apu.audio_right,
            clocked: 0,
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
        // Seed the *committed* input ports directly (this test exercises the
        // $F1 bit-4/5 clear, not the timestamped-visibility deferral of
        // `cpu_write_port` — pending writes commit only once the SPC runs).
        apu.to_spc_ports = [0x11, 0x22, 0x33, 0x44];
        {
            let mut bus = ApuBusView {
                aram: &mut apu.aram,
                to_spc_ports: &mut apu.to_spc_ports,
                to_cpu_ports: &mut apu.to_cpu_ports,
                control: &mut apu.control,
                test: &mut apu.test,
                dsp_index: &mut apu.dsp_index,
                dsp: &mut apu.dsp,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
                timer_subdivider: &mut apu.timer_subdivider,
                sample_tick_deficit: &mut apu.sample_tick_deficit,
                audio_queue: &mut apu.audio_queue,
                audio_left: &mut apu.audio_left,
                audio_right: &mut apu.audio_right,
                clocked: 0,
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
                test: &mut apu.test,
                dsp_index: &mut apu.dsp_index,
                dsp: &mut apu.dsp,
                timer_reload: &mut apu.timer_reload,
                timer_output: &mut apu.timer_output,
                timer_internal: &mut apu.timer_internal,
                timer_enabled: &mut apu.timer_enabled,
                timer_subdivider: &mut apu.timer_subdivider,
                sample_tick_deficit: &mut apu.sample_tick_deficit,
                audio_queue: &mut apu.audio_queue,
                audio_left: &mut apu.audio_left,
                audio_right: &mut apu.audio_right,
                clocked: 0,
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
            test: &mut apu.test,
            dsp_index: &mut apu.dsp_index,
            dsp: &mut apu.dsp,
            timer_reload: &mut apu.timer_reload,
            timer_output: &mut apu.timer_output,
            timer_internal: &mut apu.timer_internal,
            timer_enabled: &mut apu.timer_enabled,
            timer_subdivider: &mut apu.timer_subdivider,
            sample_tick_deficit: &mut apu.sample_tick_deficit,
            audio_queue: &mut apu.audio_queue,
            audio_left: &mut apu.audio_left,
            audio_right: &mut apu.audio_right,
            clocked: 0,
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
