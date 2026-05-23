//! Smart APU mailbox stub — a state machine that mimics enough of the
//! IPL ROM upload protocol and the post-upload command/ack dance for
//! many SNES games to progress past their boot phase **without** a
//! real SPC700 + DSP emulation.
//!
//! # Why this exists
//!
//! Real games talk to the APU through the four mailbox ports
//! `$2140-$2143`. A naïve "echo" stub (return whatever the CPU last
//! wrote) is enough to fake the initial `$AA / $BB` handshake and even
//! the IPL ROM byte-by-byte upload loop (which spins on the counter
//! matching what the CPU just wrote). **But** it fails for the
//! *post-upload* phase: most music drivers ack a command by writing a
//! *different* byte back to `$2140` (typically `$00` for "idle/done").
//! Pure echo keeps `$2140 == command code`, so the game's
//! "wait-for-ack" loop spins forever.
//!
//! # State machine
//!
//! ```text
//!     ┌────────────┐  write $CC to $2140   ┌────────────┐
//!     │ PreKick    │ ────────────────────▶ │ Uploading  │
//!     │ ($AA/$BB)  │                       │ (echo)     │
//!     └────────────┘                       └────────────┘
//!           │                                     │
//!           │ any non-$CC write to $2140          │ non-incremental
//!           │ (game skips IPL upload entirely)    │ write to $2140
//!           ▼                                     ▼
//!     ┌─────────────────────────────────────────────┐
//!     │              PostUpload                     │
//!     │  $2140 reads ← $00 (fake "driver idle" ack) │
//!     │  $2141-$2143 reads ← echo of last write     │
//!     └─────────────────────────────────────────────┘
//!                          │
//!                          │ write $CC to $2140 (new upload)
//!                          ▼
//!                     Uploading
//! ```
//!
//! Heuristic: the IPL upload writes monotonically-incrementing counters
//! to `$2140` (`$CC, $00, $01, $02, ...`). The moment we see a write
//! that is **not** `prev + 1` (and isn't a `$CC` kick), we conclude
//! the upload has ended and switch to `PostUpload`.

/// Phase of the APU stub's state machine. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Post-reset. `$2140`/`$2141` return the canonical IPL ROM ready
    /// values `$AA` / `$BB`. We're waiting for the game to write `$CC`
    /// to `$2140` to kick off an upload.
    PreKick,
    /// The game is uploading bytes through the IPL ROM protocol.
    /// `$2140` echoes the last value written (which is the counter
    /// from the CPU's perspective).
    Uploading,
    /// Upload has terminated (or the game never used IPL upload at
    /// all). `$2140` reads return `$00` to fake "driver idle / ack".
    PostUpload,
}

/// The smart APU stub.
pub struct ApuStub {
    /// Last value the CPU wrote to each mailbox port. `$2141-$2143` are
    /// always echoed back as-is; `$2140`'s read behaviour depends on
    /// the current [`Phase`].
    ports: [u8; 4],
    /// Current state-machine phase.
    phase: Phase,
}

impl Default for ApuStub {
    fn default() -> Self {
        Self::new()
    }
}

impl ApuStub {
    /// Build a freshly-reset stub. `$2140` reads return `$AA`,
    /// `$2141` reads return `$BB`. Phase = `PreKick`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ports: [0xAA, 0xBB, 0x00, 0x00],
            phase: Phase::PreKick,
        }
    }

    /// Current phase — exposed for the GUI Stubs panel.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Direct view of the four mailbox bytes — exposed for the GUI
    /// Stubs panel. Note that `$2140` *reads* don't always return
    /// `ports[0]` — see [`Self::read`].
    #[must_use]
    pub fn ports(&self) -> &[u8; 4] {
        &self.ports
    }

    /// CPU-side read of mailbox port `port` (0..=3).
    ///
    /// Behaviour:
    /// - Port 0 (`$2140`): depends on phase (see module docs).
    /// - Ports 1-3: always echo last value written.
    #[must_use]
    pub fn read(&self, port: usize) -> u8 {
        if port != 0 {
            return self.ports[port];
        }
        match self.phase {
            Phase::PreKick | Phase::Uploading => self.ports[0],
            Phase::PostUpload => 0x00,
        }
    }

    /// CPU-side write of `value` to mailbox port `port` (0..=3).
    pub fn write(&mut self, port: usize, value: u8) {
        if port == 0 {
            self.transition_on_p0_write(value);
        }
        self.ports[port] = value;
    }

    /// Advance the phase state machine for a write to `$2140`.
    fn transition_on_p0_write(&mut self, value: u8) {
        self.phase = match self.phase {
            Phase::PreKick => {
                if value == 0xCC {
                    // Standard IPL kick — start of upload.
                    Phase::Uploading
                } else {
                    // Game writes a command directly to $2140 without
                    // ever doing an IPL upload (rare but possible —
                    // some demos / homebrew). Jump straight to ack.
                    Phase::PostUpload
                }
            }
            Phase::Uploading => {
                let expected_next = self.ports[0].wrapping_add(1);
                if value == expected_next {
                    // Normal counter increment — still uploading.
                    Phase::Uploading
                } else if value == self.ports[0] {
                    // Same counter rewritten (CPU retry in some
                    // drivers — keep uploading).
                    Phase::Uploading
                } else {
                    // Non-sequential write → upload terminated, game
                    // has entered its post-upload command phase.
                    Phase::PostUpload
                }
            }
            Phase::PostUpload => {
                if value == 0xCC {
                    // Some games kick a NEW upload (e.g. switching
                    // music banks). Restart upload state.
                    Phase::Uploading
                } else {
                    Phase::PostUpload
                }
            }
        };
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
    fn kick_enters_uploading_and_acks_with_cc() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC);
        assert_eq!(s.phase(), Phase::Uploading);
        // The IPL ROM ack to the kick is just $CC echoed back.
        assert_eq!(s.read(0), 0xCC);
    }

    #[test]
    fn upload_counter_echoes() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC); // kick
        // First byte: write data to $2141, counter to $2140.
        s.write(1, 0x42);
        s.write(0, 0xCD); // wraps from $CC + 1 = $CD
        assert_eq!(s.phase(), Phase::Uploading);
        assert_eq!(s.read(0), 0xCD);
        // Second byte.
        s.write(1, 0x43);
        s.write(0, 0xCE);
        assert_eq!(s.phase(), Phase::Uploading);
        assert_eq!(s.read(0), 0xCE);
    }

    #[test]
    fn upload_counter_wraps_through_zero() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC);
        // Walk counter from $CD all the way around to confirm wrap-add.
        let mut prev = 0xCCu8;
        for _ in 0..512 {
            let next = prev.wrapping_add(1);
            s.write(0, next);
            assert_eq!(s.phase(), Phase::Uploading);
            prev = next;
        }
    }

    #[test]
    fn non_sequential_write_terminates_upload() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC); // kick
        s.write(0, 0xCD); // counter+1
        s.write(0, 0xCE); // counter+1 again
        // Now CPU writes a value that's not $CF — termination.
        s.write(0, 0x42);
        assert_eq!(s.phase(), Phase::PostUpload);
    }

    #[test]
    fn post_upload_reads_return_zero_on_p0() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC);
        s.write(0, 0x05); // termination
        assert_eq!(s.phase(), Phase::PostUpload);
        // Game sends a music command.
        s.write(0, 0xFF);
        // Game waits for ack — we fake $00.
        assert_eq!(s.read(0), 0x00);
    }

    #[test]
    fn post_upload_ports_1_2_3_still_echo() {
        let mut s = ApuStub::new();
        s.write(0, 0x42); // game skips IPL, goes straight to commands
        assert_eq!(s.phase(), Phase::PostUpload);
        s.write(1, 0xAA);
        s.write(2, 0xBB);
        s.write(3, 0xCC);
        assert_eq!(s.read(1), 0xAA);
        assert_eq!(s.read(2), 0xBB);
        assert_eq!(s.read(3), 0xCC);
    }

    #[test]
    fn skipping_ipl_kicks_straight_to_post_upload() {
        // If the very first write to $2140 isn't $CC, we conclude the
        // game isn't going to use IPL upload and switch to ack mode.
        let mut s = ApuStub::new();
        s.write(0, 0x42);
        assert_eq!(s.phase(), Phase::PostUpload);
        assert_eq!(s.read(0), 0x00);
    }

    #[test]
    fn new_kick_after_post_upload_restarts_upload() {
        let mut s = ApuStub::new();
        s.write(0, 0xCC);
        s.write(0, 0x42); // terminate
        assert_eq!(s.phase(), Phase::PostUpload);
        // New upload (e.g. switching music banks).
        s.write(0, 0xCC);
        assert_eq!(s.phase(), Phase::Uploading);
        assert_eq!(s.read(0), 0xCC);
    }

    #[test]
    fn ct_style_command_loop_unsticks() {
        // Reproduce the CT scenario: game writes $F0 to $2141, $FF to
        // $2140, then loops reading $2140 expecting a non-$FF ack.
        // With echo we returned $FF forever. With our stub, reading
        // $2140 once it's in PostUpload returns $00 → game's
        // `CMP #$FF / BEQ wait` exits.
        let mut s = ApuStub::new();
        // (Game might have done an IPL upload first — short version
        // here.)
        s.write(0, 0xCC);
        s.write(0, 0x00); // wraps from $CC
        s.write(1, 0x42);
        // Termination by writing a non-sequential code.
        s.write(0, 0x80);
        // CT's music command sequence.
        s.write(1, 0xF0);
        s.write(0, 0xFF);
        // The wait loop:
        assert_eq!(s.read(0), 0x00); // ack — exits the BEQ wait
    }
}
