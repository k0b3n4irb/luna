//! Smart APU mailbox stub — minimal state needed to let real games
//! progress past the boot handshake and the IPL upload phase
//! **without** a real SPC700 + DSP emulation.
//!
//! # Hardware model
//!
//! The four `$2140-$2143` mailbox ports are actually **two**
//! registers per port on real hardware: one CPU→SPC (CPU writes, SPC
//! reads) and one SPC→CPU (SPC writes, CPU reads). Our stub models
//! this with two arrays:
//!
//! - `to_cpu` is what the CPU **reads** at each port.
//! - `from_cpu` is what the CPU has **written** at each port
//!   (diagnostic only; in real hardware the SPC would read it).
//!
//! On a fresh reset, `to_cpu = [AA, BB, 00, 00]` — the canonical IPL
//! ROM ready signal. CPU writes do **not** propagate to `to_cpu`
//! until the game performs the `$CC` kick on `$2140`. This protects
//! the handshake from games that clear MMIO by writing `$00`
//! everywhere during init (Super Bomberman is the textbook case).
//!
//! Post-kick, we fall into pure-echo behaviour: every CPU write to a
//! port also lands in `to_cpu` at the same index. That matches both
//! the IPL counter-ACK protocol (CPU writes counter, IPL writes the
//! same value back so the CPU's `CMP $2140 / BNE wait` exits) and
//! the typical music-driver command pattern (game writes command
//! code, reads it back to confirm "command transferred").

/// Phase of the APU stub's state machine. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Post-reset. `$2140`/`$2141` return the canonical IPL ROM ready
    /// values `$AA` / `$BB`. CPU writes are recorded in `from_cpu`
    /// but do **not** propagate to `to_cpu` — this protects the
    /// handshake from init routines that write `$00` everywhere
    /// during MMIO clearing.
    PreKick,
    /// CPU has performed the `$CC` kick on `$2140`. All subsequent
    /// CPU writes echo through to the to-CPU side (= what the CPU
    /// reads back). This covers IPL upload counter ACKs, target-
    /// address staging, and the typical post-upload command/echo
    /// pattern most music drivers use.
    PostKick,
}

/// The smart APU stub. See module-level docs.
pub struct ApuStub {
    /// What the CPU reads at each port.
    to_cpu: [u8; 4],
    /// What the CPU has last written at each port.
    from_cpu: [u8; 4],
    /// Phase — see [`Phase`].
    phase: Phase,
}

impl Default for ApuStub {
    fn default() -> Self {
        Self::new()
    }
}

impl ApuStub {
    /// Build a freshly-reset stub: `$2140` reads return `$AA`,
    /// `$2141` reads return `$BB`, phase = `PreKick`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            to_cpu: [0xAA, 0xBB, 0x00, 0x00],
            from_cpu: [0; 4],
            phase: Phase::PreKick,
        }
    }

    /// Current phase — exposed for the GUI Stubs panel.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The four bytes the CPU currently reads from `$2140-$2143`.
    #[must_use]
    pub fn ports(&self) -> &[u8; 4] {
        &self.to_cpu
    }

    /// The four bytes the CPU last wrote to `$2140-$2143`. On real
    /// hardware these would be on the SPC700's side of the mailbox.
    #[must_use]
    pub fn last_writes(&self) -> &[u8; 4] {
        &self.from_cpu
    }

    /// CPU-side read of mailbox port `port` (0..=3).
    #[must_use]
    pub fn read(&self, port: usize) -> u8 {
        self.to_cpu[port]
    }

    /// CPU-side write of `value` to mailbox port `port` (0..=3).
    pub fn write(&mut self, port: usize, value: u8) {
        self.from_cpu[port] = value;
        if port == 0 && value == 0xCC && self.phase == Phase::PreKick {
            self.phase = Phase::PostKick;
        }
        // Once past the kick, every CPU write echoes through to the
        // to-CPU side (matches IPL counter-ACK behaviour and the
        // typical music-driver "command then echo confirmation"
        // pattern). PreKick writes are absorbed so the $AA/$BB
        // handshake bytes survive games' init MMIO clearing.
        if self.phase == Phase::PostKick {
            self.to_cpu[port] = value;
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
    fn boot_returns_handshake_pattern() {
        let s = ApuStub::new();
        assert_eq!(s.read(0), 0xAA);
        assert_eq!(s.read(1), 0xBB);
        assert_eq!(s.read(2), 0x00);
        assert_eq!(s.read(3), 0x00);
        assert_eq!(s.phase(), Phase::PreKick);
    }

    #[test]
    fn handshake_16bit_read_matches_bbaa() {
        // The Super Bomberman handshake: 16-bit `CMP $002140` reads
        // both $2140 (low) and $2141 (high) — should equal $BBAA.
        let s = ApuStub::new();
        let lo = s.read(0);
        let hi = s.read(1);
        let combined = (u16::from(hi) << 8) | u16::from(lo);
        assert_eq!(combined, 0xBBAA);
    }

    #[test]
    fn pre_kick_writes_dont_clobber_handshake() {
        // Crucial: real hardware has separate registers per direction.
        // A game's init routine writing $00 to every MMIO reg must
        // **not** wipe out the IPL handshake bytes. Super Bomberman
        // is the textbook case for this regression.
        let mut s = ApuStub::new();
        s.write(0, 0x00);
        s.write(1, 0x00);
        s.write(2, 0x00);
        s.write(3, 0x00);
        // Phase still PreKick, handshake bytes intact.
        assert_eq!(s.phase(), Phase::PreKick);
        assert_eq!(s.read(0), 0xAA);
        assert_eq!(s.read(1), 0xBB);
        // last_writes shows what the CPU actually wrote.
        assert_eq!(s.last_writes(), &[0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn cc_kick_enters_post_kick() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC);
        assert_eq!(s.phase(), Phase::PostKick);
        // After the kick, the IPL ROM echoes $CC back.
        assert_eq!(s.read(0), 0xCC);
    }

    #[test]
    fn post_kick_propagates_all_writes() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC);
        s.write(1, 0x42);
        s.write(2, 0x80);
        s.write(3, 0x00);
        s.write(0, 0x00); // counter
        s.write(0, 0x01);
        assert_eq!(s.read(0), 0x01);
        assert_eq!(s.read(1), 0x42);
        assert_eq!(s.read(2), 0x80);
        assert_eq!(s.read(3), 0x00);
    }

    #[test]
    fn ipl_counter_walk() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC);
        for counter in 0u8..=0xFFu8 {
            s.write(1, counter ^ 0x55);
            s.write(0, counter);
            assert_eq!(s.read(0), counter);
        }
    }

    #[test]
    fn last_writes_reflects_pre_kick_writes_even_though_to_cpu_doesnt() {
        let mut s = ApuStub::new();
        s.write(0, 0x55);
        s.write(2, 0x66);
        assert_eq!(s.last_writes(), &[0x55, 0x00, 0x66, 0x00]);
        assert_eq!(s.read(0), 0xAA); // still the handshake byte
        assert_eq!(s.read(2), 0x00); // unchanged from init
    }
}
