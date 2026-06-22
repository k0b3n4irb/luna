//! Controller-port peripherals beyond the standard pad — faithful ports of
//! ares `sfc/controller/{mouse,super-scope}`.
//!
//! The standard gamepad stays in [`crate::cpu_regs`] (the 16-bit auto-read +
//! `$4016/$4017` shift). These devices need their own serial protocols: the
//! Mouse clocks out a 32-bit stream carrying a 4-bit device **signature**
//! (`0001`) that a game's DETECT loop checks, and the Super Scope reports its
//! buttons plus a beam position latched against the PPU H/V counters.

/// Which device occupies a controller port. Port 1 is currently always a
/// [`Pad`](Self::Pad); port 2 can be reassigned to a peripheral.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PortDevice {
    /// Standard SNES gamepad (the `cpu_regs` 16-bit path handles it).
    #[default]
    Pad,
    /// SNES Mouse (serial, [`Mouse`]).
    Mouse,
    /// Super Scope (serial + PPU-latched beam position) — coming next.
    SuperScope,
}

/// SNES Mouse — faithful port of ares `controller/mouse/mouse.cpp`.
///
/// Read serially through `$4017`: each read returns one bit, advancing an
/// internal counter that the `$4016` strobe resets. The 32-bit stream is
/// `0×8, right, left, speed[1:0], 0,0,0,1 (signature), dy, cy[6:0], dx,
/// cx[6:0]`. Strobing while latched cycles the sensitivity (slow/normal/fast),
/// exactly as the hardware's sensitivity-change sequence.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Mouse {
    /// This-frame signed X motion (set by the front-end; +right / −left).
    pub dx: i32,
    /// This-frame signed Y motion (+down / −up).
    pub dy: i32,
    /// Buttons: bit 0 = left, bit 1 = right.
    pub buttons: u8,

    speed: u8, // 0 = slow, 1 = normal, 2 = fast
    counter: u32,
    latched: bool,

    // Latched at strobe time (ares does abs()+multiplier in `latch`).
    cx: u32,
    cy: u32,
    dx_dir: bool, // 0 = right, 1 = left
    dy_dir: bool, // 0 = down,  1 = up
}

impl Mouse {
    /// Drive the shared `$4016` latch line with the current strobe level.
    /// On any change it resets the serial counter and snapshots the motion
    /// (abs magnitude + direction) through the speed multiplier. Mirrors ares
    /// `Mouse::latch`.
    pub fn latch(&mut self, strobe: bool) {
        if self.latched == strobe {
            return;
        }
        self.latched = strobe;
        self.counter = 0;

        let (mut cx, mut cy) = (self.dx, self.dy);
        self.dx_dir = cx < 0;
        self.dy_dir = cy < 0;
        cx = cx.abs();
        cy = cy.abs();
        // ares multiplies by 1.0 / 1.5 / 2.0; that is exactly ×2 / ×3 / ×4
        // then ÷2 on these small integers — keep it integer for determinism.
        let mul = match self.speed {
            1 => 3,
            2 => 4,
            _ => 2,
        };
        self.cx = ((cx as u32 * mul) / 2).min(127);
        self.cy = ((cy as u32 * mul) / 2).min(127);
    }

    /// The 16-bit word an **auto-joypad-read** (`$4218`) latches for a Mouse:
    /// a fresh strobe then 16 clocked bits, MSB-first (bit 15 = first clocked
    /// bit, like the pad's `B`), so the `0001` device signature lands in the
    /// low nibble where the SDK's `mouseInit` detects it. The auto-read only
    /// sees the first 16 of the 32 protocol bits (buttons + speed + signature).
    pub fn auto_read_16(&mut self) -> u16 {
        self.latch(true);
        self.latch(false);
        let mut v = 0u16;
        for i in 0..16 {
            v |= u16::from(self.data() & 1) << (15 - i);
        }
        v
    }

    /// Clock out one serial bit (LSB of the return value). Mirrors ares
    /// `Mouse::data`: while strobed it cycles the sensitivity and returns 0.
    pub fn data(&mut self) -> u8 {
        if self.latched {
            self.speed = (self.speed + 1) % 3;
            return 0;
        }
        let c = self.counter;
        self.counter += 1;
        match c {
            0..=7 => 0,
            8 => (self.buttons >> 1) & 1, // right
            9 => self.buttons & 1,        // left
            10 => (self.speed >> 1) & 1,
            11 => self.speed & 1,
            12..=14 => 0,
            15 => 1, // 4-bit device signature `0001`
            16 => u8::from(self.dy_dir),
            17..=23 => ((self.cy >> (23 - c)) & 1) as u8, // cy[6:0]
            24 => u8::from(self.dx_dir),
            25..=31 => ((self.cx >> (31 - c)) & 1) as u8, // cx[6:0]
            _ => 1,
        }
    }
}

/// Super Scope — faithful-enough port of ares `controller/super-scope`.
///
/// A light gun on port 2. Its **buttons** are read as an 8-bit serial stream
/// (trigger, cursor, turbo, pause, 0, 0, offscreen, noise) — clocked like the
/// pad, also captured by the auto-read so the SDK's `scopeIsConnected` detects
/// it. Its **position** is not in that stream: when the CRT beam crosses the
/// aimed `(cx, cy)`, the gun strobes the port-2 `IOBit` (`$4201.d7`) which
/// latches the PPU H/V counters (OPHCT/OPVCT) the game reads for `scopeGetX/Y`.
/// The bus drives that latch from a per-scanline hook (see `snes.rs`).
///
/// The scripted model sets `cx/cy` + the four buttons directly; the
/// edge/turbo-toggle/trigger-lock state machine ares runs for a *held* real
/// button is intentionally simplified (a script provides discrete per-frame
/// states), which is enough for the detection + position acceptance.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SuperScope {
    /// Aimed beam X in screen pixels (0..255); off-range = offscreen.
    pub cx: i32,
    /// Aimed beam Y in screen lines (0..239); off-range = offscreen.
    pub cy: i32,
    /// Trigger (fire) button — set per frame.
    pub trigger: bool,
    /// Cursor button — set per frame.
    pub cursor: bool,
    /// Turbo switch — set per frame.
    pub turbo: bool,
    /// Pause button — set per frame.
    pub pause: bool,

    counter: u32,
    latched: bool,
}

impl SuperScope {
    /// True when the aim is outside the 256×240 active screen.
    pub const fn offscreen(&self) -> bool {
        self.cx < 0 || self.cy < 0 || self.cx >= 256 || self.cy >= 240
    }

    /// The H/V counter values to latch when the beam crosses the aim — ares
    /// `target = cy*1364 + (cx+24)*4`, i.e. H ≈ `cx + 24`, V = `cy`.
    pub fn latch_hv(&self) -> (u16, u16) {
        (
            (self.cx + 24).clamp(0, 339) as u16,
            self.cy.clamp(0, 261) as u16,
        )
    }

    /// Drive the shared `$4016` latch line; resets the serial counter.
    pub const fn latch(&mut self, strobe: bool) {
        if self.latched != strobe {
            self.latched = strobe;
            self.counter = 0;
        }
    }

    /// Clock one serial button bit (ares `SuperScope::data`).
    pub fn data(&mut self) -> u8 {
        let off = self.offscreen();
        let c = self.counter;
        self.counter += 1;
        match c {
            0 => u8::from(self.trigger && !off),
            1 => u8::from(self.cursor),
            2 => u8::from(self.turbo),
            3 => u8::from(self.pause),
            4 | 5 => 0,
            6 => u8::from(off),
            7 => 0, // noise
            _ => 1,
        }
    }

    /// The 16-bit auto-joypad-read word (8 button bits MSB-first, then 1s) so
    /// `scopeIsConnected` detects the gun on the port-2 auto-read.
    pub fn auto_read_16(&mut self) -> u16 {
        self.latch(true);
        self.latch(false);
        let mut v = 0u16;
        for i in 0..16 {
            v |= u16::from(self.data() & 1) << (15 - i);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clock the full 32-bit stream out of a freshly-latched mouse.
    fn stream(m: &mut Mouse) -> [u8; 32] {
        // Strobe high then low to latch (ares latches on change).
        m.latch(true);
        m.latch(false);
        let mut bits = [0u8; 32];
        for b in &mut bits {
            *b = m.data() & 1;
        }
        bits
    }

    #[test]
    fn signature_bit_is_set_so_detect_succeeds() {
        // The DETECT loop keys on bit 15 = 1 (the `0001` device signature);
        // a standard pad never sets it. dx/dy 0, no buttons.
        let mut m = Mouse::default();
        let bits = stream(&mut m);
        assert_eq!(bits[15], 1, "mouse device signature bit");
        assert_eq!(&bits[12..15], &[0, 0, 0], "signature high bits are 0");
        assert_eq!(&bits[0..8], &[0; 8], "leading padding is 0");
    }

    #[test]
    fn buttons_and_signed_motion_encode_per_ares() {
        let mut m = Mouse {
            dx: 5,         // +right
            dy: -3,        // up
            buttons: 0b01, // left
            ..Default::default()
        };
        let bits = stream(&mut m);
        assert_eq!(bits[8], 0, "right not pressed");
        assert_eq!(bits[9], 1, "left pressed");
        // dy negative → up → dir bit 1; |dy| = 3 at normal-speed×1 (speed 0 = ×1)
        assert_eq!(bits[16], 1, "dy direction = up");
        let cy: u32 = bits[17..24].iter().fold(0, |a, &b| (a << 1) | u32::from(b));
        assert_eq!(cy, 3, "|dy| magnitude");
        // dx positive → right → dir bit 0; |dx| = 5
        assert_eq!(bits[24], 0, "dx direction = right");
        let cx: u32 = bits[25..32].iter().fold(0, |a, &b| (a << 1) | u32::from(b));
        assert_eq!(cx, 5, "|dx| magnitude");
    }

    #[test]
    fn strobing_cycles_sensitivity() {
        let mut m = Mouse::default();
        // Each strobe-high `data()` read advances speed 0→1→2→0.
        m.latch(true);
        assert_eq!(m.data(), 0);
        m.latch(false);
        m.latch(true);
        assert_eq!(m.data(), 0);
        // After two cycles the reported speed bits reflect the new value.
        m.latch(false);
        let bits = {
            let mut b = [0u8; 32];
            for x in &mut b {
                *x = m.data() & 1;
            }
            b
        };
        let speed = (u8::from(bits[10] == 1) << 1) | u8::from(bits[11] == 1);
        assert_eq!(speed, 2, "two strobes → fast");
    }

    #[test]
    fn super_scope_aim_latches_to_hv_and_gates_trigger_offscreen() {
        // Onscreen aim: latch target is (cx + 24, cy); a serial read leads with
        // the trigger bit. (Drove the example ROM from CALIBRATE to READY.)
        let mut s = SuperScope {
            cx: 128,
            cy: 112,
            trigger: true,
            ..Default::default()
        };
        assert!(!s.offscreen());
        assert_eq!(s.latch_hv(), (152, 112), "OPHCT = cx+24, OPVCT = cy");
        s.latch(true);
        s.latch(false);
        assert_eq!(s.data() & 1, 1, "trigger reads 1 when onscreen");

        // Offscreen aim suppresses the trigger bit (ares `trigger & !offscreen`).
        let mut off = SuperScope {
            cx: 300,
            cy: 112,
            trigger: true,
            ..Default::default()
        };
        assert!(off.offscreen());
        off.latch(true);
        off.latch(false);
        assert_eq!(off.data() & 1, 0, "trigger suppressed when offscreen");
    }
}
