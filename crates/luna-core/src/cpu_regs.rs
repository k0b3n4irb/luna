//! CPU-system registers at `$4200-$420F` and `$4210-$421F`.
//!
//! These live "between" the 65C816 and the rest of the chip: the
//! multiplier / divider are functional units, NMITIMEN and the
//! interrupt-status registers gate the CPU's interrupt service, and
//! HVBJOY exposes the PPU's H/V-blank state to game code.
//!
//! Reference: <https://problemkaputt.de/fullsnes.htm> §"S-CPU Registers".

use crate::controller::{Mouse, Multitap, PortDevice, SuperScope, clean_dpad};

/// CPU-side registers.
///
/// Reads from write-only registers return open-bus (we return the last
/// value seen on the CPU bus, which the calling [`crate::Snes`]
/// `SnesBus` tracks). Writes to read-only registers are silently dropped.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CpuRegs {
    /// `$4200` — NMITIMEN: NMI enable (bit 7), V-IRQ (bit 5),
    /// H-IRQ (bit 4), joypad auto-read (bit 0).
    pub nmitimen: u8,
    /// `$4201` — WRIO: programmable I/O pins (write side). Bit 7
    /// connects to the PPU latch (writing 0 latches H/V counters).
    pub wrio: u8,
    /// `$4202` — WRMPYA: multiplicand.
    pub wrmpya: u8,
    /// `$4203` — WRMPYB: multiplier. Writing this triggers the
    /// 8x8 = 16-bit multiplication; the result lands in `rdmpy`.
    pub wrmpyb: u8,
    /// `$4204/$4205` — WRDIVL/H: 16-bit dividend.
    pub wrdiv: u16,
    /// `$4206` — WRDVDD: 8-bit divisor. Writing this triggers the
    /// 16/8 = 16-bit division; quotient in `rddiv`, remainder in `rdmpy`.
    pub wrdvdd: u8,
    /// `$4207/$4208` — HTIMEL/H: 9-bit H-counter trigger (low + bit 0
    /// of high).
    pub htime: u16,
    /// `$4209/$420A` — VTIMEL/H: 9-bit V-counter trigger.
    pub vtime: u16,

    /// `$4210` read side — bit 7 = "NMI flag" (set on vblank when
    /// NMI is enabled, OR on any read in some hardware). Cleared on
    /// read. Bits 0-3 = CPU version (we return 2, the most common rev).
    pub nmi_flag: bool,
    /// `$4211` read side — bit 7 = "IRQ flag" (set when the IRQ
    /// condition fires). Cleared on read — unless the read lands inside
    /// the 4-clock hold window after the raise (see
    /// [`Self::read_timeup`]).
    pub irq_flag: bool,
    /// Absolute master clock of the last H/V-IRQ raise. ares holds
    /// `/IRQ` for four clocks (`irq.cpp:30` `irqHold`, cleared by the
    /// next `irqPoll` 4 clocks later): a `$4211` read sampling inside
    /// `[raise, raise+4)` sees the flag SET but does **not** acknowledge
    /// it — the mirror of the RDNMI hold.
    pub irq_raise_mclk: u64,
    /// `$4212` read side — HVBJOY: bit 7 = V-blank. Bit 0 (joypad auto-read
    /// busy) is not stored: it is derived from the live `$4200` bit 0 and the
    /// auto-read sequence ([`Self::auto_joypad_busy`], [`Self::hvbjoy_now`]);
    /// bit 6 (H-blank) is the bus's, from the beam.
    pub hvbjoy: u8,

    /// Result of the multiplication ($4216/$4217) or the division
    /// remainder. Hardware shares the same 16-bit "RDMPY" register.
    pub rdmpy: u16,
    /// Result of the division ($4214/$4215).
    pub rddiv: u16,
    /// Cycles left of a running multiplication (ares `alu.mpyctr`): the
    /// product is built one bit per CPU cycle over eight.
    pub alu_mpyctr: u8,
    /// Cycles left of a running division (ares `alu.divctr`), sixteen.
    pub alu_divctr: u8,
    /// The unit's shift register (ares `alu.shift`).
    pub alu_shift: u32,

    // ---------- Joypad auto-read ----------
    /// Current bitmask for player 1 (16-bit). Bit layout per SNES
    /// hardware: B Y SEL START Up Down Left Right A X L R 0 0 0 0
    /// (MSB → LSB). The front-end pushes this via [`Self::set_joypad`].
    pub joypad1: u16,
    /// Same for player 2.
    pub joypad2: u16,
    /// `$4218/$4219` — ares `io.joy1`: zeroed when an auto-read starts, then
    /// shifted left once per 256 master clocks as the 16 bits come in (see
    /// [`Self::joypad_edge`]). It stays put between auto-reads, but a read
    /// *during* one sees the partial word — as on the console.
    pub joypad1_latched: u16,
    /// `$421A/$421B` — player-2 shift register (ares `io.joy2`).
    pub joypad2_latched: u16,
    /// Pads B, C, D of a Super Multitap (players 3-5 when it sits on port 2,
    /// 2-4 on port 1). Pad A is the port's own `joypad1` / `joypad2`. One
    /// tap is modelled (the 8-player double-tap setup is not).
    pub joypad_tap: [u16; 3],
    /// `$421C/$421D` — the auto-read's port-1 **d1** line (joypad 3): 0 for
    /// a pad, a multitap's second selected pad.
    pub joypad3_latched: u16,
    /// `$421E/$421F` — the auto-read's port-2 d1 line (joypad 4).
    pub joypad4_latched: u16,
    /// Auto-read sequence position: `0` = idle, otherwise ares'
    /// `autoJoypadCounter + 1` (so a default-built register file is idle).
    /// Serialised.
    pub joy_step: u8,
    /// What the four shift registers receive, MSB first, over one sequence
    /// (`joy1`, `joy2`, `joy3`, `joy4`): sampled when the sequence starts.
    pub joy_pending: [u16; 4],

    /// Device on controller port 1 (the auto-read + `$4016` serial path).
    pub port1: PortDevice,
    /// Device on controller port 2 (the auto-read + `$4017` serial path).
    pub port2: PortDevice,
    /// SNES Mouse state — shared by whichever port is set to
    /// [`PortDevice::Mouse`] (a host has at most one mouse).
    pub mouse: Mouse,
    /// Super Scope state — used when a port is [`PortDevice::SuperScope`].
    pub super_scope: SuperScope,
    /// Super Multitap state — used when a port is [`PortDevice::Multitap`].
    pub multitap: Multitap,
}

impl CpuRegs {
    /// Build a powered-on register file. All zero except WRIO ($4201),
    /// whose out-pins power up HIGH (ares `io.pio = 0xff`) — required
    /// for the falling-edge counter latch to be armed from reset.
    #[must_use]
    pub fn new() -> Self {
        Self {
            wrio: 0xFF,
            ..Self::default()
        }
    }

    /// One serial bit for `port`'s peripheral (Mouse / Super Scope) on a
    /// manual `$4016` (port 0) / `$4017` (port 1) read; `None` for a pad.
    ///
    /// The value carries both data lines (bit 0 = d0, bit 1 = d1, ares
    /// `data.bit(0,1) = controllerPort.data()`): only a Multitap drives d1.
    pub fn port_serial_bit(&mut self, port: usize) -> Option<u8> {
        match if port == 0 { self.port1 } else { self.port2 } {
            PortDevice::Mouse => Some(self.mouse.data() & 1),
            PortDevice::SuperScope => Some(self.super_scope.data() & 1),
            PortDevice::Multitap => {
                let (iobit, live) = self.tap_lines(port);
                Some(self.multitap.data(iobit, live) & 3)
            }
            // An empty port drives both data lines low, on every clock —
            // including past bit 15, where a connected pad idles high. That
            // asymmetry is the only way software can tell them apart.
            PortDevice::None => Some(0),
            PortDevice::Pad => None,
        }
    }

    /// A multitap on `port`: its iobit (WRIO bit 6 for port 1, bit 7 for
    /// port 2 — ares `Controller::iobit`) and its four pads' live masks.
    const fn tap_lines(&self, port: usize) -> (bool, [u16; 4]) {
        let (bit, pad_a) = if port == 0 {
            (0x40, self.joypad1)
        } else {
            (0x80, self.joypad2)
        };
        let [b, c, d] = self.joypad_tap;
        (self.wrio & bit != 0, [pad_a, b, c, d])
    }

    /// If a Super Scope is connected and aimed onscreen, and the beam has
    /// reached its aimed scanline `line`, the `(h, v)` counter values its
    /// light-gun latches into OPHCT/OPVCT (which the game reads for the beam
    /// position). `None` otherwise — no scope, offscreen, or wrong scanline.
    pub fn scope_latch_target(&self, line: u16) -> Option<(u16, u16)> {
        let active = self.port1 == PortDevice::SuperScope || self.port2 == PortDevice::SuperScope;
        if active && !self.super_scope.offscreen() && u32::from(line) == self.super_scope.cy as u32
        {
            Some(self.super_scope.latch_hv())
        } else {
            None
        }
    }

    /// Drive the shared `$4016` strobe line into every connected peripheral's
    /// serial latch.
    pub fn latch_devices(&mut self, strobe: bool) {
        if self.port1 == PortDevice::Mouse || self.port2 == PortDevice::Mouse {
            self.mouse.latch(strobe);
        }
        if self.port1 == PortDevice::SuperScope || self.port2 == PortDevice::SuperScope {
            self.super_scope.latch(strobe);
        }
        for port in 0..2 {
            if [self.port1, self.port2][port] == PortDevice::Multitap {
                let (_, live) = self.tap_lines(port);
                self.multitap.latch(strobe, live);
            }
        }
    }

    /// Read a register at the 16-bit CPU bank-0 offset (`$4200-$421F`).
    /// Returns `None` for an offset outside that range and for every
    /// register without an arm below (`$4200-$420F`, `$4211`, `$4213`):
    /// the caller decides what such a read returns.
    pub const fn read(&mut self, offset: u16) -> Option<u8> {
        let v = match offset {
            // The write-only registers ($4200-$420D) are open bus on
            // real hardware: they are not answered here (the `None` arms
            // at the bottom).
            0x4210 => {
                // RDNMI: bit 7 = nmi flag (cleared by this read).
                // For the cleared-on-read semantics with the 4-cycle
                // hold window at the start of the NMI scanline, use
                // [`Self::read_rdnmi`] from a bus-level caller that
                // knows the current hclock. Plain `read()` always
                // clears — fine for direct test callers, not for games
                // that race a `BPL $4210` mainline against an NMI
                // handler that ACKs the same flag (CT, Terranigma).
                let v = if self.nmi_flag { 0x80 } else { 0x00 } | 0x02; // CPU rev 2
                self.nmi_flag = false;
                v
            }
            // 0x4211 TIMEUP is handled by the bus (`snes.rs read_inner`):
            // the hold window needs the master clock and the low bits are
            // CPU open bus — see `read_timeup`.
            _ => return self.peek(offset),
        };
        Some(v)
    }

    /// The registers of [`Self::read`] whose read has no side effect, as
    /// they read now; `None` for the others.
    #[must_use]
    pub const fn peek(&self, offset: u16) -> Option<u8> {
        Some(match offset {
            0x4212 => self.hvbjoy_now(),
            0x4214 => self.rddiv as u8,
            0x4215 => (self.rddiv >> 8) as u8,
            0x4216 => self.rdmpy as u8,
            0x4217 => (self.rdmpy >> 8) as u8,
            // Joypad auto-read latches at $4218-$421F: the d0 lines of
            // ports 1/2, then their d1 lines ($421C = port 1, $421E =
            // port 2) — non-zero only behind a Super Multitap.
            0x4218 => self.joypad1_latched as u8,
            0x4219 => (self.joypad1_latched >> 8) as u8,
            0x421A => self.joypad2_latched as u8,
            0x421B => (self.joypad2_latched >> 8) as u8,
            0x421C => self.joypad3_latched as u8,
            0x421D => (self.joypad3_latched >> 8) as u8,
            0x421E => self.joypad4_latched as u8,
            0x421F => (self.joypad4_latched >> 8) as u8,
            _ => return None,
        })
    }

    /// `$4210` RDNMI read.
    ///
    /// `visible` is false only for a read the S-CPU has not yet presented the
    /// raised NMI line to — the first few H-clocks of the NMI scanline. Such
    /// a read reads the flag **clear** and, crucially, does not clear it: that
    /// is what keeps an NMI handler that acknowledges `$4210` from starving a
    /// mainline `BPL $4210` poll of it. The bus derives it from the H-clock
    /// — see `RDNMI_VISIBLE_HCLOCK` in `snes.rs` for the window and for where
    /// ares and Mesen2 disagree on it.
    pub const fn read_rdnmi(&mut self, raised: bool, in_hold: bool) -> u8 {
        let v = if self.nmi_flag && raised { 0x80 } else { 0x00 } | 0x02; // CPU rev 2
        if !in_hold {
            self.nmi_flag = false;
        }
        v
    }

    /// `$4211` TIMEUP read — the exact mirror of [`Self::read_rdnmi`]
    /// (ares `irq.cpp:60-66` `timeup()`): returns the held IRQ line in
    /// bit 7 and acknowledges it, **unless** the read samples inside the
    /// 4-clock hold window after the raise (`in_hold`, computed by the
    /// bus from [`Self::irq_raise_mclk`]) — then the flag survives so
    /// the pending IRQ still fires. Bits 0-6 are CPU open bus, `OR`ed in
    /// by the bus.
    pub const fn read_timeup(&mut self, in_hold: bool) -> u8 {
        let v = if self.irq_flag { 0x80 } else { 0x00 };
        if !in_hold {
            self.irq_flag = false;
        }
        v
    }

    /// `$4212` bit 0: the auto-read sequence is running (ares `io.cpp:34`,
    /// `autoJoypadPoll && autoJoypadCounter < 33`). Live in `$4200` bit 0:
    /// clearing it mid-sequence drops the flag at once.
    #[must_use]
    pub const fn auto_joypad_busy(&self) -> bool {
        self.nmitimen & 0x01 != 0 && self.joy_step != 0
    }

    /// `$4212` without the H-blank bit (the bus adds it): V-blank as latched
    /// by the scheduler, busy as [`Self::auto_joypad_busy`].
    #[must_use]
    pub const fn hvbjoy_now(&self) -> u8 {
        (self.hvbjoy & 0x80) | self.auto_joypad_busy() as u8
    }

    /// One edge of the auto-read sequence, ares `CPU::joypadEdge`
    /// (`cpu/timing.cpp:144-196`): the scheduler calls it at every multiple
    /// of 128 master clocks, `start` being true at the edge that begins a
    /// sequence (first multiple of 256 at or after H130 of the first V-blank
    /// line). 33 edges of 128 clocks (4224) follow:
    ///
    /// - counter 0: the pads are latched; with `$4200` bit 0 set `joy1..4`
    ///   are zeroed;
    /// - counter 1: the latch is released (the only counter that survives
    ///   `$4200` bit 0 being off — otherwise the sequence ends at once);
    /// - odd counters 3..=33: one bit shifts in, MSB first (ares reads the
    ///   port on the even counter and shifts on the odd one; luna samples
    ///   every pad at counter 0, a deviation noted in
    ///   `docs/luna_65c816_gaps.md` #6).
    ///
    /// D-pad opposing-direction quirk (ares `gamepad.cpp` `latch()`):
    /// `clean_dpad` clears both of an opposing pair. A Mouse feeds its first
    /// 16 protocol bits instead of a pad.
    pub fn joypad_edge(&mut self, start: bool) {
        let counter = if start {
            0
        } else if self.joy_step == 0 {
            return;
        } else {
            self.joy_step
        };
        if counter == 0 && self.nmitimen & 0x01 != 0 {
            self.sample_auto_read();
            self.joypad1_latched = 0;
            self.joypad2_latched = 0;
            self.joypad3_latched = 0;
            self.joypad4_latched = 0;
        }
        if counter != 1 && self.nmitimen & 0x01 == 0 {
            // `$4200` bit 0 is off at this point: the sequence is skipped.
            self.joy_step = 0;
            return;
        }
        if counter >= 3 && counter & 1 == 1 {
            let bit = 15 - u32::from((counter - 3) / 2);
            let pick = |w: u16| (w >> bit) & 1;
            self.joypad1_latched = self.joypad1_latched << 1 | pick(self.joy_pending[0]);
            self.joypad2_latched = self.joypad2_latched << 1 | pick(self.joy_pending[1]);
            self.joypad3_latched = self.joypad3_latched << 1 | pick(self.joy_pending[2]);
            self.joypad4_latched = self.joypad4_latched << 1 | pick(self.joy_pending[3]);
        }
        self.joy_step = if counter >= 33 { 0 } else { counter + 1 };
    }

    /// What the four data lines will deliver this sequence.
    fn sample_auto_read(&mut self) {
        // A Mouse on a port feeds the auto-read its first 16 protocol bits
        // (signature + buttons + speed) instead of the pad's button mask —
        // that is how the SDK's `mouseInit` detects it.
        let (mut d1_port1, mut d1_port2) = (0, 0);
        let p1 = match self.port1 {
            PortDevice::Mouse => self.mouse.auto_read_16(),
            PortDevice::SuperScope => self.super_scope.auto_read_16(),
            PortDevice::Multitap => {
                let (iobit, live) = self.tap_lines(0);
                let (d0, d1) = self.multitap.auto_read(iobit, live);
                d1_port1 = d1;
                d0
            }
            PortDevice::None => 0,
            PortDevice::Pad => clean_dpad(self.joypad1),
        };
        let p2 = match self.port2 {
            PortDevice::Mouse => self.mouse.auto_read_16(),
            PortDevice::SuperScope => self.super_scope.auto_read_16(),
            PortDevice::Multitap => {
                let (iobit, live) = self.tap_lines(1);
                let (d0, d1) = self.multitap.auto_read(iobit, live);
                d1_port2 = d1;
                d0
            }
            PortDevice::None => 0,
            PortDevice::Pad => clean_dpad(self.joypad2),
        };
        self.joy_pending = [p1, p2, d1_port1, d1_port2];
    }

    /// Set the current button bitmask for controller `idx` (`0` or
    /// `1`). The bitmask isn't visible to game code until the next
    /// auto-read latch (typically the next `VBlank`).
    ///
    /// Bit layout (high → low): B Y SEL START Up Down Left Right
    /// A X L R 0 0 0 0.
    ///
    /// `idx` 2-4 are a Super Multitap's pads B-D (players 3-5 with the tap
    /// on port 2); ignored unless a port holds one.
    pub const fn set_joypad(&mut self, idx: usize, mask: u16) {
        match idx {
            0 => self.joypad1 = mask,
            1 => self.joypad2 = mask,
            2..=4 => self.joypad_tap[idx - 2] = mask,
            _ => {}
        }
    }

    /// One CPU cycle of the multiplier / divider (ares `CPU::aluEdge`,
    /// `cpu/timing.cpp:81-98`): shift-and-add for a product, restoring
    /// division for a quotient. A read before the last cycle sees the
    /// intermediate value.
    pub const fn alu_edge(&mut self) {
        if self.alu_mpyctr > 0 {
            self.alu_mpyctr -= 1;
            if self.rddiv & 1 != 0 {
                self.rdmpy = self.rdmpy.wrapping_add(self.alu_shift as u16);
            }
            self.rddiv >>= 1;
            self.alu_shift <<= 1;
        }
        if self.alu_divctr > 0 {
            self.alu_divctr -= 1;
            self.rddiv <<= 1;
            self.alu_shift >>= 1;
            if self.rdmpy as u32 >= self.alu_shift {
                self.rdmpy -= self.alu_shift as u16;
                self.rddiv |= 1;
            }
        }
    }

    /// Write a register at the 16-bit CPU bank-0 offset.
    pub fn write(&mut self, offset: u16, value: u8) -> bool {
        match offset {
            0x4200 => {
                self.nmitimen = value;
                // ares `nmitimenUpdate` (irq.cpp:40-43): disabling BOTH
                // H/V IRQ sources drops the held line at once — a stale
                // TIMEUP flag must not linger past the disable. (The
                // NMI-side late-enable retrigger of the same ares
                // function is ported in the bus write path, `snes.rs`
                // `write_inner`, which sees the previous NMITIMEN.7.)
                if value & 0x30 == 0 {
                    self.irq_flag = false;
                }
                // ares `io.cpp:135`: switching auto-read off from the third
                // counter on ends the sequence for good (counter = 33);
                // `joy_step` is the NEXT counter, so "counter >= 2" is `>= 3`.
                if value & 0x01 == 0 && self.joy_step >= 3 {
                    self.joy_step = 0;
                }
            }
            0x4201 => self.wrio = value,
            0x4202 => self.wrmpya = value,
            0x4203 => {
                // ares `cpu/io.cpp:152-160`. The product is not there yet:
                // `alu_edge` builds it over the next eight CPU cycles. A
                // write while the unit is busy clears RDMPY and starts
                // nothing.
                self.rdmpy = 0;
                if self.alu_mpyctr == 0 && self.alu_divctr == 0 {
                    self.wrmpyb = value;
                    self.rddiv = (u16::from(value) << 8) | u16::from(self.wrmpya);
                    self.alu_mpyctr = 8;
                    self.alu_shift = u32::from(value);
                }
            }
            0x4204 => self.wrdiv = (self.wrdiv & 0xFF00) | u16::from(value),
            0x4205 => self.wrdiv = (self.wrdiv & 0x00FF) | (u16::from(value) << 8),
            0x4206 => {
                // ares `cpu/io.cpp:171-178`: RDMPY takes the dividend, and
                // `alu_edge` turns it into the remainder over sixteen CPU
                // cycles while the quotient shifts into RDDIV. A zero
                // divisor leaves `$FFFF` and the dividend by itself.
                self.rdmpy = self.wrdiv;
                if self.alu_mpyctr == 0 && self.alu_divctr == 0 {
                    self.wrdvdd = value;
                    self.alu_divctr = 16;
                    self.alu_shift = u32::from(value) << 16;
                }
            }
            0x4207 => self.htime = (self.htime & 0xFF00) | u16::from(value),
            0x4208 => self.htime = (self.htime & 0x00FF) | (u16::from(value & 0x01) << 8),
            0x4209 => self.vtime = (self.vtime & 0xFF00) | u16::from(value),
            0x420A => self.vtime = (self.vtime & 0x00FF) | (u16::from(value & 0x01) << 8),
            // $420B (MDMAEN) and $420C (HDMAEN) are routed to the DMA
            // controller, not this struct.
            0x420B | 0x420C => return false,
            0x420D => {
                // MEMSEL — bit 0 = FastROM enable. Stored externally
                // (luna-core::Snes.fast_rom), so we return "not us".
                return false;
            }
            // Read-only registers — writes ignored.
            0x4210..=0x421F => {}
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // Multiplication
    // ---------------------------------------------------------------

    #[test]
    fn multiplication_7_times_8_yields_56() {
        let mut r = CpuRegs::new();
        r.write(0x4202, 7);
        r.write(0x4203, 8); // triggers multiply
        settle(&mut r);
        assert_eq!(r.read(0x4216).unwrap(), 56);
        assert_eq!(r.read(0x4217).unwrap(), 0);
    }

    #[test]
    fn multiplication_full_16bit_range() {
        // 255 × 255 = 65 025 = $FE01.
        let mut r = CpuRegs::new();
        r.write(0x4202, 0xFF);
        r.write(0x4203, 0xFF);
        settle(&mut r);
        assert_eq!(r.rdmpy, 0xFE01);
    }

    /// Run the unit to the end of whatever it is computing.
    fn settle(r: &mut CpuRegs) {
        for _ in 0..16 {
            r.alu_edge();
        }
    }

    #[test]
    fn a_product_is_built_one_bit_per_cycle() {
        // ares `aluEdge`: 0xFF x 0xFF read after each of the eight cycles.
        let mut r = CpuRegs::new();
        r.write(0x4202, 0xFF);
        r.write(0x4203, 0xFF);
        let mut seen = vec![r.rdmpy];
        for _ in 0..8 {
            r.alu_edge();
            seen.push(r.rdmpy);
        }
        assert_eq!(
            seen,
            [
                0x0000, 0x00FF, 0x02FD, 0x06F9, 0x0EF1, 0x1EE1, 0x3EC1, 0x7E81, 0xFE01
            ]
        );
        // Done: further cycles change nothing, and RDDIV holds factor B.
        r.alu_edge();
        assert_eq!((r.rdmpy, r.rddiv), (0xFE01, 0x00FF));
    }

    #[test]
    fn a_quotient_is_built_one_bit_per_cycle() {
        let mut r = CpuRegs::new();
        r.write(0x4204, 100);
        r.write(0x4205, 0);
        r.write(0x4206, 3);
        // The remainder register starts as the dividend.
        assert_eq!((r.rddiv, r.rdmpy), (0, 100));
        for _ in 0..15 {
            r.alu_edge();
        }
        assert_eq!((r.rddiv, r.rdmpy), (16, 4));
        r.alu_edge();
        assert_eq!((r.rddiv, r.rdmpy), (33, 1));
    }

    #[test]
    fn a_write_while_the_unit_is_busy_starts_nothing() {
        // ares `io.cpp:153,172`: the write clears (or reloads) RDMPY and
        // returns; the operation in flight carries on from there.
        let mut r = CpuRegs::new();
        r.write(0x4202, 3);
        r.write(0x4203, 5);
        r.alu_edge();
        r.write(0x4203, 7);
        assert_eq!((r.rdmpy, r.wrmpyb, r.alu_mpyctr), (0, 5, 7));
    }

    #[test]
    fn multiplication_triggers_only_on_4203_write() {
        // Writing $4202 alone should NOT trigger the multiplication.
        let mut r = CpuRegs::new();
        r.write(0x4202, 0xFF);
        assert_eq!(r.rdmpy, 0);
    }

    // ---------------------------------------------------------------
    // Division
    // ---------------------------------------------------------------

    #[test]
    fn division_100_by_3_gives_33_remainder_1() {
        let mut r = CpuRegs::new();
        r.write(0x4204, 100);
        r.write(0x4205, 0);
        r.write(0x4206, 3); // triggers division
        settle(&mut r);
        assert_eq!(r.rddiv, 33);
        assert_eq!(r.rdmpy, 1);
    }

    #[test]
    fn division_by_zero_returns_ffff_and_dividend() {
        let mut r = CpuRegs::new();
        r.write(0x4204, 0x34);
        r.write(0x4205, 0x12); // dividend = $1234
        r.write(0x4206, 0); // divisor = 0
        settle(&mut r);
        assert_eq!(r.rddiv, 0xFFFF);
        assert_eq!(r.rdmpy, 0x1234);
    }

    #[test]
    fn division_triggers_only_on_4206_write() {
        let mut r = CpuRegs::new();
        r.write(0x4204, 100);
        r.write(0x4205, 0);
        // No $4206 write — division should not run yet.
        assert_eq!(r.rddiv, 0);
    }

    // ---------------------------------------------------------------
    // NMITIMEN, RDNMI, TIMEUP
    // ---------------------------------------------------------------

    #[test]
    fn an_unplugged_port_reads_zero_where_a_pad_idles_high() {
        // Both references model an empty port as data lines low, for ever:
        // ares returns 0 from `ControllerPort::data()` with no device
        // allocated, Mesen2 masks the data bits out of open bus with no
        // device to OR them back. A connected pad, by contrast, idles HIGH
        // past bit 15 (ares `gamepad.cpp:45-46`) — and that asymmetry is
        // the only thing software can detect, because auto-read gives
        // $0000 for both an empty port and an idle pad.
        let mut r = CpuRegs {
            nmitimen: 0x01,
            port2: PortDevice::None,
            ..Default::default()
        };
        r.set_joypad(0, 0x0000); // an idle pad on port 1
        r.set_joypad(1, 0xFFFF); // ...on the empty port, must be ignored

        run_auto_read(&mut r);
        assert_eq!(r.joypad1_latched, 0x0000, "idle pad");
        assert_eq!(
            r.joypad2_latched, 0x0000,
            "empty port reads the same as an idle pad — auto-read cannot tell them apart"
        );

        // The serial path is where they differ: the empty port answers
        // every clock with 0, including the ones past the 16 button bits.
        for clock in 0..24 {
            assert_eq!(
                r.port_serial_bit(1),
                Some(0),
                "empty port must read 0 on clock {clock}"
            );
        }
        // A pad defers to the shift register (`None` here means "not a
        // serial peripheral"), which is what supplies its idle-high 1s.
        assert_eq!(r.port_serial_bit(0), None);
    }

    #[test]
    fn nmitimen_stores_byte_verbatim() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x81);
        assert_eq!(r.nmitimen, 0x81);
    }

    #[test]
    fn rdnmi_returns_flag_and_clears_it() {
        let mut r = CpuRegs::new();
        r.nmi_flag = true;
        let v = r.read(0x4210).unwrap();
        assert!(v & 0x80 != 0, "bit 7 reflects the latched NMI flag");
        // Subsequent read should NOT see the flag set.
        let v = r.read(0x4210).unwrap();
        assert_eq!(v & 0x80, 0);
    }

    #[test]
    fn rdnmi_reports_cpu_revision_in_low_nibble() {
        let mut r = CpuRegs::new();
        let v = r.read(0x4210).unwrap();
        // We model CPU revision 2 — most common in the wild.
        assert_eq!(v & 0x0F, 0x02);
    }

    #[test]
    fn timeup_returns_irq_flag_and_clears_it() {
        let mut r = CpuRegs::new();
        r.irq_flag = true;
        let v = r.read_timeup(false);
        assert!(v & 0x80 != 0);
        let v = r.read_timeup(false);
        assert_eq!(v, 0);
    }

    #[test]
    fn timeup_read_inside_the_hold_window_does_not_acknowledge() {
        // ares irq.cpp:60-66: `timeup()` clears the line only when
        // `!irqHold` — a read within 4 clocks of the raise sees the flag
        // but leaves it set, so the pending IRQ still fires.
        let mut r = CpuRegs::new();
        r.irq_flag = true;
        assert_eq!(r.read_timeup(true) & 0x80, 0x80);
        assert!(r.irq_flag, "hold read must not acknowledge");
        assert_eq!(r.read_timeup(false) & 0x80, 0x80);
        assert!(!r.irq_flag, "post-hold read acknowledges");
    }

    #[test]
    fn disabling_both_irq_sources_drops_a_held_timeup_flag() {
        // ares nmitimenUpdate: `if(!io.irqEnable) status.irqLine = 0`.
        let mut r = CpuRegs::new();
        r.irq_flag = true;
        r.write(0x4200, 0x10); // H-IRQ still enabled — flag survives
        assert!(r.irq_flag);
        r.write(0x4200, 0x00); // both sources off — flag drops
        assert!(!r.irq_flag);
    }

    // ---------------------------------------------------------------
    // HTIME / VTIME 9-bit packing
    // ---------------------------------------------------------------

    #[test]
    fn htime_high_byte_uses_only_bit_0() {
        let mut r = CpuRegs::new();
        r.write(0x4207, 0xFF);
        r.write(0x4208, 0xFF); // only bit 0 should land in htime bit 8
        assert_eq!(r.htime, 0x01FF);
    }

    // ---------------------------------------------------------------
    // Range checks
    // ---------------------------------------------------------------

    #[test]
    fn reads_outside_range_return_none() {
        let mut r = CpuRegs::new();
        assert!(r.read(0x41FF).is_none());
        assert!(r.read(0x4220).is_none());
    }

    #[test]
    fn write_to_dma_enable_returns_false_so_caller_routes_elsewhere() {
        let mut r = CpuRegs::new();
        assert!(!r.write(0x420B, 0xFF), "$420B is DMA, not CpuRegs");
        assert!(!r.write(0x420C, 0xFF), "$420C is HDMA, not CpuRegs");
    }

    // ---------------------------------------------------------------
    // Joypad auto-read
    // ---------------------------------------------------------------

    #[test]
    fn joypad_latch_copies_live_state_when_nmitimen_0_set() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01); // NMITIMEN.0 = auto-read enable
        r.set_joypad(0, 0xAA55);
        r.set_joypad(1, 0x1234);
        run_auto_read(&mut r);
        // Latched mirrors live; reads expose them at $4218-$421B.
        assert_eq!(r.read(0x4218).unwrap(), 0x55);
        assert_eq!(r.read(0x4219).unwrap(), 0xAA);
        assert_eq!(r.read(0x421A).unwrap(), 0x34);
        assert_eq!(r.read(0x421B).unwrap(), 0x12);
        assert!(!r.auto_joypad_busy(), "the sequence is over");
    }

    #[test]
    fn joypad_latch_is_a_noop_when_nmitimen_0_clear() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x00); // bit 0 cleared
        r.set_joypad(0, 0xFFFF);
        run_auto_read(&mut r);
        // Latches stay at their default 0; reads see no buttons.
        assert_eq!(r.read(0x4218).unwrap(), 0x00);
        assert_eq!(r.read(0x4219).unwrap(), 0x00);
        assert!(!r.auto_joypad_busy());
    }

    #[test]
    fn joypad_3_and_4_return_zero() {
        let mut r = CpuRegs::new();
        // No multitap modelled — $421C-$421F always read 0.
        for off in 0x421C..=0x421F {
            assert_eq!(r.read(off).unwrap(), 0x00);
        }
    }

    /// Play one whole auto-read sequence: the start edge, then 33 more.
    fn run_auto_read(r: &mut CpuRegs) {
        r.joypad_edge(true);
        for _ in 0..33 {
            r.joypad_edge(false);
        }
    }

    #[test]
    fn auto_read_fills_the_registers_progressively_like_ares() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01);
        r.set_joypad(0, 0x8000);
        r.joypad1_latched = 0xFFFF; // the previous frame's value
        assert!(!r.auto_joypad_busy(), "idle before the sequence");
        // Counter 0: pads latched, registers zeroed, busy.
        r.joypad_edge(true);
        assert_eq!(r.joypad1_latched, 0);
        assert!(r.auto_joypad_busy());
        // Counters 1 and 2: nothing shifts yet.
        for counter in 1..=2 {
            r.joypad_edge(false);
            assert_eq!(r.joypad1_latched, 0, "counter {counter}");
            assert!(r.auto_joypad_busy());
        }
        // Odd counters 3..=33 shift one bit each, MSB first: after n shifts
        // the pad's bit 15 sits at bit n-1. Even counters change nothing.
        let mut expect = 0u16;
        for counter in 3..=33u32 {
            r.joypad_edge(false);
            if counter % 2 == 1 {
                expect = 1 << ((counter - 3) / 2);
            }
            assert_eq!(r.joypad1_latched, expect, "counter {counter}");
            assert_eq!(
                r.auto_joypad_busy(),
                counter < 33,
                "busy at counter {counter}"
            );
        }
        assert_eq!(r.joypad1_latched, 0x8000, "the whole word after 16 shifts");
        // Idle: further edges change nothing.
        r.joypad_edge(false);
        assert_eq!(r.joypad1_latched, 0x8000);
        assert!(!r.auto_joypad_busy());
    }

    #[test]
    fn auto_read_first_shift_is_after_counter_3_with_the_msb_first() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01);
        r.set_joypad(0, 0x8000);
        r.joypad_edge(true);
        for _ in 0..3 {
            r.joypad_edge(false); // counters 1, 2, 3
        }
        assert_eq!(r.joypad1_latched, 0x0001);
        r.joypad_edge(false); // counter 4: no shift
        assert_eq!(r.joypad1_latched, 0x0001);
        r.joypad_edge(false); // counter 5
        assert_eq!(r.joypad1_latched, 0x0002);
    }

    #[test]
    fn auto_read_with_nmitimen_0_off_at_the_start_leaves_the_registers() {
        let mut r = CpuRegs::new();
        r.set_joypad(0, 0xFFFF);
        r.joypad1_latched = 0x1234;
        run_auto_read(&mut r);
        assert_eq!(r.joypad1_latched, 0x1234);
        assert!(!r.auto_joypad_busy());
    }

    #[test]
    fn auto_read_ends_when_nmitimen_0_goes_off_after_counter_1() {
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01);
        r.set_joypad(0, 0xFFFF);
        r.joypad_edge(true);
        r.joypad_edge(false); // counter 1
        r.joypad_edge(false); // counter 2
        r.write(0x4200, 0x00);
        assert!(!r.auto_joypad_busy(), "busy is live in $4200 bit 0");
        r.joypad_edge(false); // counter 3: skipped, sequence over
        assert_eq!(r.joypad1_latched, 0);
        r.write(0x4200, 0x01);
        r.joypad_edge(false);
        assert_eq!(r.joypad1_latched, 0, "the sequence did not resume");
    }

    #[test]
    fn auto_read_switched_off_and_on_between_two_edges_does_not_resume() {
        // ares `io.cpp:135`: off from counter 2 on ends the sequence at once,
        // so a quick off/on pair cannot revive it.
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01);
        r.set_joypad(0, 0xFFFF);
        r.joypad_edge(true);
        for _ in 0..3 {
            r.joypad_edge(false); // counters 1, 2, 3
        }
        r.write(0x4200, 0x00);
        r.write(0x4200, 0x01);
        assert!(!r.auto_joypad_busy());
        let word = r.joypad1_latched;
        r.joypad_edge(false);
        assert_eq!(r.joypad1_latched, word);
    }

    #[test]
    fn hvbjoy_keeps_the_vblank_bit_and_derives_busy() {
        let mut r = CpuRegs::new();
        r.hvbjoy = 0x80;
        r.write(0x4200, 0x01);
        assert_eq!(r.peek(0x4212), Some(0x80));
        r.joypad_edge(true);
        assert_eq!(r.peek(0x4212), Some(0x81));
    }

    #[test]
    fn dpad_lockout_clears_opposing_directions() {
        // Bit layout per SNES JOY1L/JOY1H pair (high → low):
        //   B Y SEL START Up Down Left Right A X L R 0 0 0 0
        let mut r = CpuRegs::new();
        r.write(0x4200, 0x01); // auto-read enable
        // Up + Down + B held → keep B, drop both Up + Down.
        r.set_joypad(0, 0x8000 | 0x0800 | 0x0400);
        run_auto_read(&mut r);
        assert_eq!(r.joypad1_latched & 0x0C00, 0, "up + down lockout");
        assert_eq!(r.joypad1_latched & 0x8000, 0x8000, "B preserved");

        // Left + Right + Start held → keep Start, drop L + R.
        r.set_joypad(0, 0x1000 | 0x0200 | 0x0100);
        run_auto_read(&mut r);
        assert_eq!(r.joypad1_latched & 0x0300, 0, "left + right lockout");
        assert_eq!(r.joypad1_latched & 0x1000, 0x1000, "Start preserved");

        // Only Up held (no opposing) → passes through.
        r.set_joypad(0, 0x0800);
        run_auto_read(&mut r);
        assert_eq!(r.joypad1_latched, 0x0800);
    }

    #[test]
    fn joypad_bit_layout_byss_udlr_axlr() {
        // Compile-time sanity check that we agree with ares'
        // gamepad.cpp shift order. Test by name so a future
        // bit-flip will fire here loudly.
        let b = 0x8000u16;
        let y = 0x4000;
        let sel = 0x2000;
        let start = 0x1000;
        let up = 0x0800;
        let down = 0x0400;
        let left = 0x0200;
        let right = 0x0100;
        let a = 0x0080;
        let x = 0x0040;
        let l = 0x0020;
        let r_ = 0x0010;
        let all = b | y | sel | start | up | down | left | right | a | x | l | r_;
        // Bits 3..0 are the device signature — always zero for a
        // standard gamepad. So a "press everything" mask is
        // 0xFFF0.
        assert_eq!(all, 0xFFF0);
    }
}
