// The `pub _name: T` fields mirror ares' S-DSP `n3 _name;` placeholders
// — internal latches the upstream pipeline writes / reads, kept on the
// Voice / Echo / Latch structs so the 32-step pipeline transliteration
// stays line-for-line with ares. Some are not wired yet on the luna
// side; the `_` prefix marks them as "ares-port scaffold, not part of
// the luna public API". Removing them now would diverge from
// `ares/sfc/dsp/dsp.hpp` and make the next round-trip review (when we
// re-port from ares head) noisier.
#![allow(clippy::pub_underscore_fields)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::used_underscore_items)]

//! Cycle-accurate port of ares' S-DSP (Sony CXD1222Q-1).
//!
//! Mirrors `ares/sfc/dsp/` 1-for-1: the `Dsp` struct holds the same
//! sub-structs (Voice, Echo, Noise, BRR, Latch, MainVol, Clock) and
//! the 32-step macro pipeline in [`Dsp::main`] is a transliteration of
//! `DSP::main()`. Per-method implementations live in this single file
//! grouped by source file (voice / brr / echo / envelope / counter /
//! gaussian / misc / memory) — see the matching `ares/sfc/dsp/*.cpp`
//! for the originals.
//!
//! Integer-size convention: ares uses `n8` (u8), `n16` (u16), `s32`
//! (i32), `i16`, `i17` etc. We use Rust primitives directly; widening
//! casts are explicit. The few places ares' `sclamp<16>` is used we
//! call [`sclamp16`].
//!
//! Field-level docs are suppressed here because the field shapes match
//! the ares Voice / Echo / BRR / Latch struct definitions exactly;
//! re-documenting them in Rust would dilute the value of the 1-for-1
//! mapping. See `ares/sfc/dsp/dsp.hpp` for the contract each field
//! satisfies.

#![allow(missing_docs)]

use std::sync::OnceLock;

// -------------- helpers --------------------------------------------------

/// Saturating clamp to signed 16-bit range. Equivalent to ares'
/// `sclamp<16>`.
#[inline]
fn sclamp16(v: i32) -> i32 {
    v.clamp(-32768, 32767)
}

/// Bit-extract `[lo, hi]` (inclusive) of `v` (matches ares' `.bit(lo, hi)`).
#[inline]
const fn bits(v: u8, lo: u8, hi: u8) -> u8 {
    (v >> lo) & ((1u8 << (hi - lo + 1)) - 1)
}

#[inline]
const fn bit(v: u8, n: u8) -> bool {
    (v >> n) & 1 != 0
}

// -------------- enums + structs ----------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnvelopeMode {
    Release = 0,
    Attack = 1,
    Decay = 2,
    Sustain = 3,
}

impl EnvelopeMode {
    /// `EnvelopeMode >= Decay` per ares' enum ordering (Release=0,
    /// Attack=1, Decay=2, Sustain=3).
    #[inline]
    const fn at_least_decay(self) -> bool {
        self as u32 >= Self::Decay as u32
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Voice {
    pub index: u8, // voice channel register base: 0x00, 0x10, ..., 0x70

    pub volume: [i8; 2],
    pub pitch: u16, // 14-bit
    pub source: u8,
    pub adsr0: u8,
    pub adsr1: u8,
    pub gain: u8,
    pub envx: u8,
    pub keyon: bool,
    pub keyoff: bool,
    pub modulate: bool,
    pub noise: bool,
    pub echo: bool,
    pub end: bool,

    pub buffer: [i16; 12],
    pub buffer_offset: u8,    // 0..11 (n4 in ares)
    pub gaussian_offset: u16, // n16
    pub brr_address: u16,
    pub brr_offset: u8,  // 1..8 (n4)
    pub keyon_delay: u8, // n3
    pub envelope_mode: EnvelopeMode,
    pub envelope: u16, // 11-bit (n11)

    // internal latches
    pub _envelope: i32, // used by GAIN mode 7
    pub _keylatch: bool,
    pub _keyon: bool,
    pub _keyoff: bool,
    pub _modulate: bool,
    pub _noise: bool,
    pub _echo: bool,
    pub _end: bool,
    pub _looped: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            index: 0,
            volume: [0; 2],
            pitch: 0,
            source: 0,
            adsr0: 0,
            adsr1: 0,
            gain: 0,
            envx: 0,
            keyon: false,
            keyoff: false,
            modulate: false,
            noise: false,
            echo: false,
            end: false,
            buffer: [0; 12],
            buffer_offset: 0,
            gaussian_offset: 0,
            brr_address: 0,
            brr_offset: 1,
            keyon_delay: 0,
            envelope_mode: EnvelopeMode::Release,
            envelope: 0,
            _envelope: 0,
            _keylatch: false,
            _keyon: false,
            _keyoff: false,
            _modulate: false,
            _noise: false,
            _echo: false,
            _end: false,
            _looped: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Clock {
    pub counter: u16, // n15: max 0x7FFF
    pub sample: bool,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            counter: 0,
            sample: true,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MainVol {
    pub reset: bool,
    pub mute: bool,
    pub volume: [i8; 2],
    pub output: [i32; 2], // i17 in ares
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Echo {
    pub feedback: i8,
    pub volume: [i8; 2],
    pub fir: [i8; 8],
    pub history: [[i16; 8]; 2], // [channel][history slot]
    pub page: u8,
    pub delay: u8, // n4
    pub readonly: bool,
    pub input: [i32; 2],
    pub output: [i32; 2],
    pub _page: u8,
    pub _readonly: bool,
    pub _address: u16,
    pub _offset: u16,
    pub _length: u16,
    pub _history_offset: u8, // n3 (0..7)
}

impl Default for Echo {
    fn default() -> Self {
        Self {
            feedback: 0,
            volume: [0; 2],
            fir: [0; 8],
            history: [[0; 8]; 2],
            page: 0,
            delay: 0,
            readonly: true,
            input: [0; 2],
            output: [0; 2],
            _page: 0,
            _readonly: true,
            _address: 0,
            _offset: 0,
            _length: 0,
            _history_offset: 0,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Noise {
    pub frequency: u8, // n5
    pub lfsr: u16,     // n15
}

impl Default for Noise {
    fn default() -> Self {
        Self {
            frequency: 0,
            lfsr: 0x4000,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Brr {
    pub bank: u8,
    pub _bank: u8,
    pub _source: u8,
    pub _address: u16,
    pub _next_address: u16,
    pub _header: u8,
    pub _byte: u8,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Latch {
    pub adsr0: u8,
    pub envx: u8,
    pub outx: u8,
    pub pitch: u16, // n15
    pub output: i16,
}

// -------------- counter tables (counter.cpp) -----------------------------

pub const COUNTER_RATE: [u16; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80, 64, 48, 40, 32,
    24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1,
];

pub const COUNTER_OFFSET: [u16; 32] = [
    0, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040,
    536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 0, 0,
];

pub const COUNTER_RELOAD: u16 = 30_720;

// -------------- gaussian table -------------------------------------------

fn build_gaussian_table() -> [i16; 512] {
    let mut table = [0f64; 512];
    for n in 0..512 {
        let k = 0.5 + n as f64;
        let s = (std::f64::consts::PI * k * 1.280 / 1024.0).sin();
        let t = ((std::f64::consts::PI * k * 2.000 / 1023.0).cos() - 1.0) * 0.50;
        let u = ((std::f64::consts::PI * k * 4.000 / 1023.0).cos() - 1.0) * 0.08;
        let r = s * (t + u + 1.0) / k;
        table[511 - n] = r;
    }
    let mut out = [0i16; 512];
    for phase in 0..128 {
        let sum = table[phase] + table[phase + 256] + table[511 - phase] + table[255 - phase];
        let scale = 2048.0 / sum;
        out[phase] = table[phase].mul_add(scale, 0.5) as i16;
        out[phase + 256] = table[phase + 256].mul_add(scale, 0.5) as i16;
        out[511 - phase] = table[511 - phase].mul_add(scale, 0.5) as i16;
        out[255 - phase] = table[255 - phase].mul_add(scale, 0.5) as i16;
    }
    out
}

#[must_use]
pub fn gaussian_table() -> &'static [i16; 512] {
    static T: OnceLock<[i16; 512]> = OnceLock::new();
    T.get_or_init(build_gaussian_table)
}

// -------------- the DSP itself -------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Dsp {
    #[serde(with = "serde_bytes")]
    pub registers: [u8; 128],
    pub voices: [Voice; 8],
    pub clock: Clock,
    pub mainvol: MainVol,
    pub echo: Echo,
    pub noise: Noise,
    pub brr: Brr,
    pub latch: Latch,
    /// Per-`main()` accumulator: the last produced stereo sample
    /// (echo27 calls `sample()` once per macro-pipeline run).
    pub last_sample: (i16, i16),
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Dsp {
    pub fn new() -> Self {
        let mut voices: [Voice; 8] = Default::default();
        for (i, v) in voices.iter_mut().enumerate() {
            v.index = (i as u8) << 4;
        }
        let mut registers = [0u8; 128];
        // Reset DSP register file with FLG=$E0 (soft reset + mute amp +
        // echo write disable). Matches what ares' power-on does after
        // randomising; we choose deterministic for now.
        registers[0x6C] = 0xE0;
        let mut dsp = Self {
            registers,
            voices,
            clock: Clock::default(),
            mainvol: MainVol {
                mute: true,
                reset: true,
                ..Default::default()
            },
            echo: Echo::default(),
            noise: Noise::default(),
            brr: Brr::default(),
            latch: Latch::default(),
            last_sample: (0, 0),
        };
        // Trigger gaussian table init so the first sample isn't slow.
        let _ = gaussian_table();
        // Apply the FLG write to mirror what a real $6C=0xE0 store does.
        dsp.write(0x6C, 0xE0);
        dsp
    }

    // ---------------- memory.cpp -----------------

    pub const fn read(&self, address: u8) -> u8 {
        self.registers[(address & 0x7F) as usize]
    }

    pub fn write(&mut self, address: u8, data: u8) {
        let addr = address & 0x7F;
        self.registers[addr as usize] = data;

        match addr {
            0x0C => self.mainvol.volume[0] = data as i8,
            0x1C => self.mainvol.volume[1] = data as i8,
            0x2C => self.echo.volume[0] = data as i8,
            0x3C => self.echo.volume[1] = data as i8,
            0x4C => {
                for n in 0..8 {
                    self.voices[n].keyon = bit(data, n as u8);
                    self.voices[n]._keylatch = bit(data, n as u8);
                }
            }
            0x5C => {
                for n in 0..8 {
                    self.voices[n].keyoff = bit(data, n as u8);
                }
            }
            0x6C => {
                self.noise.frequency = bits(data, 0, 4);
                self.echo.readonly = bit(data, 5);
                self.mainvol.mute = bit(data, 6);
                self.mainvol.reset = bit(data, 7);
            }
            0x7C => {
                for n in 0..8 {
                    self.voices[n]._end = false;
                }
                self.registers[0x7C] = 0; // always cleared regardless of data
            }
            0x0D => self.echo.feedback = data as i8,
            0x2D => {
                for n in 0..8 {
                    self.voices[n].modulate = bit(data, n as u8);
                }
                self.voices[0].modulate = false; // voice 0 cannot modulate
            }
            0x3D => {
                for n in 0..8 {
                    self.voices[n].noise = bit(data, n as u8);
                }
            }
            0x4D => {
                for n in 0..8 {
                    self.voices[n].echo = bit(data, n as u8);
                }
            }
            0x5D => self.brr.bank = data,
            0x6D => self.echo.page = data,
            0x7D => self.echo.delay = bits(data, 0, 3),
            _ => {}
        }

        let n = (bits(addr, 4, 6)) as usize;
        match addr & 0x0F {
            0x00 => self.voices[n].volume[0] = data as i8,
            0x01 => self.voices[n].volume[1] = data as i8,
            0x02 => self.voices[n].pitch = (self.voices[n].pitch & 0xFF00) | u16::from(data),
            0x03 => {
                self.voices[n].pitch =
                    (self.voices[n].pitch & 0x00FF) | (u16::from(data & 0x3F) << 8);
            }
            0x04 => self.voices[n].source = data,
            0x05 => self.voices[n].adsr0 = data,
            0x06 => self.voices[n].adsr1 = data,
            0x07 => self.voices[n].gain = data,
            0x08 => self.latch.envx = data,
            0x09 => self.latch.outx = data,
            0x0F => self.echo.fir[n] = data as i8,
            _ => {}
        }
    }

    // ---------------- counter.cpp ----------------

    #[inline]
    const fn counter_tick(&mut self) {
        if self.clock.counter == 0 {
            self.clock.counter = COUNTER_RELOAD;
        }
        self.clock.counter -= 1;
    }

    #[inline]
    const fn counter_poll(&self, rate: u32) -> bool {
        if rate == 0 {
            return false;
        }
        let r = COUNTER_RATE[rate as usize] as u32;
        let off = COUNTER_OFFSET[rate as usize] as u32;
        (self.clock.counter as u32 + off) % r == 0
    }

    // ---------------- gaussian.cpp ---------------

    fn gaussian_interpolate(v: &Voice) -> i32 {
        let table = gaussian_table();
        let offset8 = (v.gaussian_offset >> 4) as usize & 0xFF;
        let forward = |i: usize| i32::from(table[255 - offset8 + i]);
        let reverse = |i: usize| i32::from(table[offset8 + i]);

        let mut off = ((v.buffer_offset as u32 + (v.gaussian_offset as u32 >> 12)) % 12) as usize;
        let s0 = i32::from(v.buffer[off]);
        let mut output = (forward(0) * s0) >> 11;
        off = (off + 1) % 12;
        output += (forward(256) * i32::from(v.buffer[off])) >> 11;
        off = (off + 1) % 12;
        output += (reverse(256) * i32::from(v.buffer[off])) >> 11;
        off = (off + 1) % 12;
        output = i32::from(output as i16); // truncate to i16 (wrap)
        output = output.wrapping_add((reverse(0) * i32::from(v.buffer[off])) >> 11);
        sclamp16(output) & !1
    }

    // ---------------- envelope.cpp ---------------

    fn envelope_run(&mut self, vi: usize) {
        let mut envelope: i32 = i32::from(self.voices[vi].envelope);

        if self.voices[vi].envelope_mode == EnvelopeMode::Release {
            envelope -= 8;
            if envelope < 0 {
                envelope = 0;
            }
            self.voices[vi].envelope = envelope as u16;
            return;
        }

        let rate: u32;
        let mut envelope_data = self.voices[vi].adsr1;
        let latched_adsr0 = self.latch.adsr0;
        if bit(latched_adsr0, 7) {
            if self.voices[vi].envelope_mode.at_least_decay() {
                envelope -= 1;
                envelope -= envelope >> 8;
                rate = (envelope_data & 0x1F) as u32;
                let rate_actual = if self.voices[vi].envelope_mode == EnvelopeMode::Decay {
                    (bits(latched_adsr0, 4, 6) as u32) * 2 + 16
                } else {
                    rate
                };
                self.envelope_finish(vi, envelope, envelope_data, rate_actual);
            } else {
                // Attack
                let rate_a = (bits(latched_adsr0, 0, 3) as u32) * 2 + 1;
                envelope += if rate_a < 31 { 0x20 } else { 0x400 };
                self.envelope_finish(vi, envelope, envelope_data, rate_a);
            }
        } else {
            envelope_data = self.voices[vi].gain;
            let mode = envelope_data >> 5;
            if mode < 4 {
                envelope = (envelope_data as i32) << 4;
                let rate_d = 31u32;
                self.envelope_finish(vi, envelope, envelope_data, rate_d);
            } else {
                let rate_g = (envelope_data & 0x1F) as u32;
                if mode == 4 {
                    envelope -= 0x20;
                } else if mode < 6 {
                    envelope -= 1;
                    envelope -= envelope >> 8;
                } else {
                    envelope += 0x20;
                    if mode > 6 && (self.voices[vi]._envelope as u32) >= 0x600 {
                        envelope += 0x8 - 0x20;
                    }
                }
                self.envelope_finish(vi, envelope, envelope_data, rate_g);
            }
        }
    }

    fn envelope_finish(&mut self, vi: usize, mut envelope: i32, envelope_data: u8, rate: u32) {
        // sustain-level transition: fires on the unclamped candidate
        if (envelope >> 8) == ((envelope_data >> 5) as i32)
            && self.voices[vi].envelope_mode == EnvelopeMode::Decay
        {
            self.voices[vi].envelope_mode = EnvelopeMode::Sustain;
        }
        self.voices[vi]._envelope = envelope;

        // u32-cast trick in ares: catches both >0x7FF and <0 (linear decrease underflow)
        if (envelope as u32) > 0x7FF {
            envelope = if envelope < 0 { 0 } else { 0x7FF };
            if self.voices[vi].envelope_mode == EnvelopeMode::Attack {
                self.voices[vi].envelope_mode = EnvelopeMode::Decay;
            }
        }

        if self.counter_poll(rate) {
            self.voices[vi].envelope = envelope as u16;
        }
    }

    // ---------------- brr.cpp --------------------

    fn brr_decode(&mut self, vi: usize, apuram: &[u8; 0x10000]) {
        let v = &mut self.voices[vi];
        let mut nybbles: u32 = (u32::from(self.brr._byte) << 8)
            | u32::from(
                apuram[(v
                    .brr_address
                    .wrapping_add(u16::from(v.brr_offset))
                    .wrapping_add(1)) as usize],
            );
        let filter = bits(self.brr._header, 2, 3) as i32;
        let scale = bits(self.brr._header, 4, 7) as i32;

        for _ in 0..4 {
            // top 4 bits sign-extended
            let mut s = (nybbles as i16) >> 12;
            nybbles = (nybbles << 4) & 0xFFFF;
            let mut s32_s = i32::from(s);
            // suppress unused-mut warning on `s`
            let _ = &mut s;

            if scale <= 12 {
                s32_s <<= scale;
                s32_s >>= 1;
            } else {
                s32_s &= !0x7FF;
            }

            let mut off = v.buffer_offset as i32 - 1;
            if off < 0 {
                off = 11;
            }
            let p1 = i32::from(v.buffer[off as usize]);
            off -= 1;
            if off < 0 {
                off = 11;
            }
            let p2 = i32::from(v.buffer[off as usize]) >> 1;

            match filter {
                0 => {}
                1 => {
                    s32_s += p1 >> 1;
                    s32_s += (-p1) >> 5;
                }
                2 => {
                    s32_s += p1;
                    s32_s -= p2;
                    s32_s += p2 >> 4;
                    s32_s += (p1 * -3) >> 6;
                }
                3 => {
                    s32_s += p1;
                    s32_s -= p2;
                    s32_s += (p1 * -13) >> 7;
                    s32_s += (p2 * 3) >> 4;
                }
                _ => {}
            }

            s32_s = sclamp16(s32_s);
            let stored = (s32_s << 1) as i16; // (i16)(s << 1) — wrap
            v.buffer[v.buffer_offset as usize] = stored;
            v.buffer_offset = (v.buffer_offset + 1) % 12;
        }
    }

    // ---------------- voice.cpp ------------------

    fn voice_output(&mut self, vi: usize, channel: usize) {
        let amp = (i32::from(self.latch.output) * (self.voices[vi].volume[channel] as i32)) >> 7;
        self.mainvol.output[channel] += amp;
        self.mainvol.output[channel] = sclamp16(self.mainvol.output[channel]);
        if self.voices[vi]._echo {
            self.echo.output[channel] += amp;
            self.echo.output[channel] = sclamp16(self.echo.output[channel]);
        }
    }

    fn voice1(&mut self, vi: usize) {
        self.brr._address =
            (u16::from(self.brr._bank) << 8).wrapping_add(u16::from(self.brr._source) << 2);
        self.brr._source = self.voices[vi].source;
    }

    fn voice2(&mut self, vi: usize, apuram: &[u8; 0x10000]) {
        let mut address = self.brr._address;
        if self.voices[vi].keyon_delay == 0 {
            address = address.wrapping_add(2);
        }
        let lo = apuram[address as usize];
        let hi = apuram[address.wrapping_add(1) as usize];
        self.brr._next_address = u16::from(lo) | (u16::from(hi) << 8);
        self.latch.adsr0 = self.voices[vi].adsr0;
        self.latch.pitch = self.voices[vi].pitch & 0xFF;
    }

    fn voice3(&mut self, vi: usize, apuram: &[u8; 0x10000]) {
        self.voice3a(vi);
        self.voice3b(vi, apuram);
        self.voice3c(vi);
    }

    const fn voice3a(&mut self, vi: usize) {
        self.latch.pitch |= self.voices[vi].pitch & !0xFFu16;
    }

    fn voice3b(&mut self, vi: usize, apuram: &[u8; 0x10000]) {
        let v = &self.voices[vi];
        self.brr._byte = apuram[(v.brr_address.wrapping_add(u16::from(v.brr_offset))) as usize];
        self.brr._header = apuram[v.brr_address as usize];
    }

    fn voice3c(&mut self, vi: usize) {
        // pitch modulation using previous voice's output
        if self.voices[vi]._modulate {
            let p = i32::from(self.latch.pitch);
            let new_pitch = (i32::from(self.latch.pitch)
                + (((i32::from(self.latch.output) >> 5) * p) >> 10))
                as u16;
            self.latch.pitch = new_pitch & 0x7FFF;
        }

        if self.voices[vi].keyon_delay != 0 {
            if self.voices[vi].keyon_delay == 5 {
                let next = self.brr._next_address;
                let v = &mut self.voices[vi];
                v.brr_address = next;
                v.brr_offset = 1;
                v.buffer_offset = 0;
                self.brr._header = 0; // header ignored on this sample
            }
            self.voices[vi].envelope = 0;
            self.voices[vi]._envelope = 0;
            self.voices[vi].gaussian_offset = 0;
            self.voices[vi].keyon_delay -= 1;
            if self.voices[vi].keyon_delay & 3 != 0 {
                self.voices[vi].gaussian_offset = 0x4000;
            }
            self.latch.pitch = 0;
        }

        // gaussian interpolation (immutable borrow → take a snapshot)
        let output = if self.voices[vi]._noise {
            i32::from((self.noise.lfsr << 1) as i16)
        } else {
            Self::gaussian_interpolate(&self.voices[vi])
        };

        // apply envelope
        self.latch.output = (((output * i32::from(self.voices[vi].envelope)) >> 11) & !1) as i16;
        self.voices[vi].envx = (self.voices[vi].envelope >> 4) as u8;

        // immediate silence due to end of sample or soft reset
        if self.mainvol.reset || bits(self.brr._header, 0, 1) == 1 {
            self.voices[vi].envelope_mode = EnvelopeMode::Release;
            self.voices[vi].envelope = 0;
        }

        if self.clock.sample {
            if self.voices[vi]._keyoff {
                self.voices[vi].envelope_mode = EnvelopeMode::Release;
            }
            if self.voices[vi]._keyon {
                self.voices[vi].keyon_delay = 5;
                self.voices[vi].envelope_mode = EnvelopeMode::Attack;
            }
        }

        if self.voices[vi].keyon_delay == 0 {
            self.envelope_run(vi);
        }
    }

    fn voice4(&mut self, vi: usize, apuram: &[u8; 0x10000]) {
        self.voices[vi]._looped = false;
        if self.voices[vi].gaussian_offset >= 0x4000 {
            self.brr_decode(vi, apuram);
            self.voices[vi].brr_offset += 2;
            if self.voices[vi].brr_offset >= 9 {
                self.voices[vi].brr_address = self.voices[vi].brr_address.wrapping_add(9);
                if bit(self.brr._header, 0) {
                    self.voices[vi].brr_address = self.brr._next_address;
                    self.voices[vi]._looped = true;
                }
                self.voices[vi].brr_offset = 1;
            }
        }
        // apply pitch
        let pa = (self.voices[vi].gaussian_offset & 0x3FFF) as u32 + self.latch.pitch as u32;
        let mut new_off = pa as u16;
        if new_off > 0x7FFF {
            new_off = 0x7FFF;
        }
        self.voices[vi].gaussian_offset = new_off;

        self.voice_output(vi, 0);
    }

    fn voice5(&mut self, vi: usize) {
        self.voice_output(vi, 1);
        // ENDX, OUTX, ENVX won't update if you wrote to them 1-2 clocks earlier
        self.voices[vi]._end = self.voices[vi]._end || self.voices[vi]._looped;
        if self.voices[vi].keyon_delay == 5 {
            self.voices[vi]._end = false;
        }
    }

    const fn voice6(&mut self, _vi: usize) {
        self.latch.outx = ((self.latch.output as i32) >> 8) as u8;
    }

    fn voice7(&mut self, vi: usize) {
        // ENDX register reflects per-voice _end bits
        let mut endx = 0u8;
        for n in 0..8 {
            if self.voices[n]._end {
                endx |= 1 << n;
            }
        }
        self.registers[0x7C] = endx;
        self.latch.envx = self.voices[vi].envx;
    }

    const fn voice8(&mut self, vi: usize) {
        let idx = self.voices[vi].index | 0x09;
        self.registers[idx as usize] = self.latch.outx;
    }

    const fn voice9(&mut self, vi: usize) {
        let idx = self.voices[vi].index | 0x08;
        self.registers[idx as usize] = self.latch.envx;
    }

    // ---------------- echo.cpp -------------------

    const fn calculate_fir(&self, channel: usize, index: i32) -> i32 {
        let hist_idx = (self.echo._history_offset as i32 + index + 1) & 7;
        let sample = self.echo.history[channel][hist_idx as usize] as i32;
        (sample * self.echo.fir[index as usize] as i32) >> 6
    }

    fn echo_output(&self, channel: usize) -> i16 {
        let mainvol_out =
            ((self.mainvol.output[channel] * self.mainvol.volume[channel] as i32) >> 7) as i16;
        let echo_out = ((self.echo.input[channel] * self.echo.volume[channel] as i32) >> 7) as i16;
        sclamp16(i32::from(mainvol_out) + i32::from(echo_out)) as i16
    }

    fn echo_read(&mut self, channel: usize, apuram: &[u8; 0x10000]) {
        let address = self.echo._address.wrapping_add((channel as u16) * 2);
        let lo = apuram[address as usize];
        let hi = apuram[address.wrapping_add(1) as usize];
        let s = ((u16::from(hi) << 8) | u16::from(lo)) as i16;
        let half = (s as i32) >> 1;
        self.echo.history[channel][self.echo._history_offset as usize] = half as i16;
    }

    const fn echo_write(&mut self, channel: usize, apuram: &mut [u8; 0x10000]) {
        if !self.echo._readonly {
            let address = self.echo._address.wrapping_add((channel as u16) * 2);
            let sample = self.echo.output[channel];
            apuram[address as usize] = sample as u8;
            apuram[address.wrapping_add(1) as usize] = (sample >> 8) as u8;
        }
        self.echo.output[channel] = 0;
    }

    fn echo22(&mut self, apuram: &[u8; 0x10000]) {
        self.echo._history_offset = (self.echo._history_offset + 1) & 7;
        self.echo._address = (u16::from(self.echo._page) << 8).wrapping_add(self.echo._offset);
        self.echo_read(0, apuram);
        let l = self.calculate_fir(0, 0);
        let r = self.calculate_fir(1, 0);
        self.echo.input[0] = l;
        self.echo.input[1] = r;
    }

    fn echo23(&mut self, apuram: &[u8; 0x10000]) {
        let l = self.calculate_fir(0, 1) + self.calculate_fir(0, 2);
        let r = self.calculate_fir(1, 1) + self.calculate_fir(1, 2);
        self.echo.input[0] += l;
        self.echo.input[1] += r;
        self.echo_read(1, apuram);
    }

    const fn echo24(&mut self) {
        let l = self.calculate_fir(0, 3) + self.calculate_fir(0, 4) + self.calculate_fir(0, 5);
        let r = self.calculate_fir(1, 3) + self.calculate_fir(1, 4) + self.calculate_fir(1, 5);
        self.echo.input[0] += l;
        self.echo.input[1] += r;
    }

    fn echo25(&mut self) {
        let mut l = self.echo.input[0] + self.calculate_fir(0, 6);
        let mut r = self.echo.input[1] + self.calculate_fir(1, 6);
        l = i32::from(l as i16);
        r = i32::from(r as i16);
        l += i32::from(self.calculate_fir(0, 7) as i16);
        r += i32::from(self.calculate_fir(1, 7) as i16);
        self.echo.input[0] = sclamp16(l) & !1;
        self.echo.input[1] = sclamp16(r) & !1;
    }

    fn echo26(&mut self) {
        self.mainvol.output[0] = self.echo_output(0) as i32;
        let l = self.echo.output[0]
            + ((self.echo.input[0] * self.echo.feedback as i32) >> 7) as i16 as i32;
        let r = self.echo.output[1]
            + ((self.echo.input[1] * self.echo.feedback as i32) >> 7) as i16 as i32;
        self.echo.output[0] = sclamp16(l) & !1;
        self.echo.output[1] = sclamp16(r) & !1;
    }

    fn echo27(&mut self) {
        let outl = self.mainvol.output[0] as i16;
        let outr = self.echo_output(1);
        self.mainvol.output[0] = 0;
        self.mainvol.output[1] = 0;
        let (outl, outr) = if self.mainvol.mute {
            (0i16, 0i16)
        } else {
            (outl, outr)
        };
        self.last_sample = (outl, outr);
    }

    const fn echo28(&mut self) {
        self.echo._readonly = self.echo.readonly;
    }

    const fn echo29(&mut self, apuram: &mut [u8; 0x10000]) {
        self.echo._page = self.echo.page;
        if self.echo._offset == 0 {
            self.echo._length = (self.echo.delay as u16) << 11;
        }
        self.echo._offset = self.echo._offset.wrapping_add(4);
        if self.echo._offset >= self.echo._length {
            self.echo._offset = 0;
        }
        self.echo_write(0, apuram);
        self.echo._readonly = self.echo.readonly;
    }

    const fn echo30(&mut self, apuram: &mut [u8; 0x10000]) {
        self.echo_write(1, apuram);
    }

    // ---------------- misc.cpp -------------------

    fn misc27(&mut self) {
        for v in &mut self.voices {
            v._modulate = v.modulate;
        }
    }

    fn misc28(&mut self) {
        for v in &mut self.voices {
            v._noise = v.noise;
            v._echo = v.echo;
        }
        self.brr._bank = self.brr.bank;
    }

    fn misc29(&mut self) {
        self.clock.sample = !self.clock.sample;
        if self.clock.sample {
            for v in &mut self.voices {
                v._keylatch &= !v._keyon;
            }
        }
    }

    fn misc30(&mut self) {
        if self.clock.sample {
            for v in &mut self.voices {
                v._keyon = v._keylatch;
                v._keyoff = v.keyoff;
            }
        }
        self.counter_tick();
        if self.counter_poll(self.noise.frequency as u32) {
            let feedback = (self.noise.lfsr << 13) ^ (self.noise.lfsr << 14);
            self.noise.lfsr = (feedback & 0x4000) | (self.noise.lfsr >> 1);
        }
    }

    // ---------------- main() pipeline ------------

    /// Runs one full 32-step macro pipeline → one stereo output sample.
    /// Mirrors `DSP::main()` from `ares/sfc/dsp/dsp.cpp:34-185`.
    pub fn main(&mut self, apuram: &mut [u8; 0x10000]) -> (i16, i16) {
        self.voice5(0);
        self.voice2(1, apuram);
        self.voice6(0);
        self.voice3(1, apuram);
        self.voice7(0);
        self.voice4(1, apuram);
        self.voice1(3);
        self.voice8(0);
        self.voice5(1);
        self.voice2(2, apuram);
        self.voice9(0);
        self.voice6(1);
        self.voice3(2, apuram);
        self.voice7(1);
        self.voice4(2, apuram);
        self.voice1(4);
        self.voice8(1);
        self.voice5(2);
        self.voice2(3, apuram);
        self.voice9(1);
        self.voice6(2);
        self.voice3(3, apuram);
        self.voice7(2);
        self.voice4(3, apuram);
        self.voice1(5);
        self.voice8(2);
        self.voice5(3);
        self.voice2(4, apuram);
        self.voice9(2);
        self.voice6(3);
        self.voice3(4, apuram);
        self.voice7(3);
        self.voice4(4, apuram);
        self.voice1(6);
        self.voice8(3);
        self.voice5(4);
        self.voice2(5, apuram);
        self.voice9(3);
        self.voice6(4);
        self.voice3(5, apuram);
        self.voice7(4);
        self.voice4(5, apuram);
        self.voice1(7);
        self.voice8(4);
        self.voice5(5);
        self.voice2(6, apuram);
        self.voice9(4);
        self.voice6(5);
        self.voice3(6, apuram);
        self.voice1(0);
        self.voice7(5);
        self.voice4(6, apuram);
        self.voice8(5);
        self.voice5(6);
        self.voice2(7, apuram);
        self.voice9(5);
        self.voice6(6);
        self.voice3(7, apuram);
        self.voice1(1);
        self.voice7(6);
        self.voice4(7, apuram);
        self.voice8(6);
        self.voice5(7);
        self.voice2(0, apuram);
        self.voice3a(0);
        self.voice9(6);
        self.voice6(7);
        self.echo22(apuram);
        self.voice7(7);
        self.echo23(apuram);
        self.voice8(7);
        self.echo24();
        self.voice3b(0, apuram);
        self.voice9(7);
        self.echo25();
        self.echo26();
        self.misc27();
        self.echo27();
        self.misc28();
        self.echo28();
        self.misc29();
        self.echo29(apuram);
        self.misc30();
        self.voice3c(0);
        self.echo30(apuram);
        self.voice4(0, apuram);
        self.voice1(2);

        self.last_sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_dsp_outputs_silence_on_first_sample() {
        let mut dsp = Dsp::new();
        let mut aram: Box<[u8; 0x10000]> = vec![0u8; 0x10000]
            .into_boxed_slice()
            .try_into()
            .expect("64 KB slice into fixed array");
        // With FLG.7 (reset) set on new, mainvol.mute is true → silence.
        let (l, r) = dsp.main(&mut aram);
        assert_eq!(l, 0);
        assert_eq!(r, 0);
    }

    #[test]
    fn write_to_kon_latches_keyon_bit() {
        let mut dsp = Dsp::new();
        dsp.write(0x4C, 0x01);
        assert!(dsp.voices[0].keyon);
        assert!(dsp.voices[0]._keylatch);
        assert!(!dsp.voices[1].keyon);
    }

    #[test]
    fn counter_tick_wraps_at_zero() {
        let mut dsp = Dsp::new();
        dsp.clock.counter = 0;
        dsp.counter_tick();
        assert_eq!(dsp.clock.counter, COUNTER_RELOAD - 1);
    }

    #[test]
    fn counter_poll_rate_zero_never_fires() {
        let dsp = Dsp::new();
        assert!(!dsp.counter_poll(0));
    }

    #[test]
    fn counter_poll_rate_31_always_fires() {
        // Rate 31 = period 1, so any counter+offset is divisible.
        let mut dsp = Dsp::new();
        for c in 0..50 {
            dsp.clock.counter = c;
            assert!(dsp.counter_poll(31), "counter={c}");
        }
    }

    #[test]
    fn dsp_register_write_round_trip() {
        let mut dsp = Dsp::new();
        dsp.write(0x00, 0x40);
        assert_eq!(dsp.read(0x00), 0x40);
        assert_eq!(dsp.voices[0].volume[0], 0x40);
    }
}
