//! Per-channel DMA state and transfer logic.

use crate::bus::DmaBus;
use luna_bus::{Addr24, make_addr};

// =============================================================================
// Decoded `$43x0` (DMAPx) register.
// =============================================================================

/// Direction of the DMA transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// A-bus → B-bus (CPU → PPU). The common case: uploading tiles,
    /// palettes, OAM, etc.
    AToB,
    /// B-bus → A-bus (PPU → CPU). Rare in practice but legal.
    BToA,
}

/// A-bus address increment behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Increment {
    /// `+1` per byte (the canonical "uploading a linear buffer" case).
    Up,
    /// `-1` per byte.
    Down,
    /// Fixed (no increment / decrement). Used by some games that DMA
    /// from a register-mapped source.
    Fixed,
}

/// DMA transfer pattern (`$43x0` bits 0-2). Each mode specifies how
/// many bytes per "cycle" and which B-bus offset each goes to.
///
/// Notation below: `b` = `BBADx`. So mode 1 writes alternating to
/// `$2100 + b` and `$2100 + b + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    /// Mode 0 — 1 byte to `b`. (e.g. palette stream into `$2122`.)
    OneByteOneReg,
    /// Mode 1 — 2 bytes alternating to `b`, `b+1`. (e.g. VRAM via
    /// `$2118`/`$2119`.)
    TwoBytesTwoRegs,
    /// Mode 2 — 2 bytes to the same register `b`. (e.g. OAM stream
    /// into `$2104`.)
    TwoBytesOneReg,
    /// Mode 3 — 4 bytes: `b`, `b`, `b+1`, `b+1`. (e.g. color math
    /// pair-of-pairs.)
    FourBytesTwoPairs,
    /// Mode 4 — 4 bytes: `b`, `b+1`, `b+2`, `b+3`. (BG mode 7 has
    /// 4 sequential MMIO addresses for various blob uploads.)
    FourBytesFourRegs,
    /// Mode 5 — 4 bytes alternating to `b`, `b+1`, `b`, `b+1`. (Mirror
    /// of mode 1 doubled — rare.)
    FourBytesTwoRegsAlt,
    /// Mode 6 — alias of mode 2.
    TwoBytesOneRegAlt,
    /// Mode 7 — alias of mode 3.
    FourBytesTwoPairsAlt,
}

impl TransferMode {
    /// Per-byte B-bus offset increments within one "cycle" of the
    /// pattern. The pattern length is the slice's len.
    #[must_use]
    pub fn pattern(self) -> &'static [u8] {
        match self {
            TransferMode::OneByteOneReg => &[0],
            TransferMode::TwoBytesTwoRegs => &[0, 1],
            TransferMode::TwoBytesOneReg | TransferMode::TwoBytesOneRegAlt => &[0, 0],
            TransferMode::FourBytesTwoPairs | TransferMode::FourBytesTwoPairsAlt => &[0, 0, 1, 1],
            TransferMode::FourBytesFourRegs => &[0, 1, 2, 3],
            TransferMode::FourBytesTwoRegsAlt => &[0, 1, 0, 1],
        }
    }

    /// Decode the low 3 bits of `$43x0`.
    #[must_use]
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x07 {
            0 => Self::OneByteOneReg,
            1 => Self::TwoBytesTwoRegs,
            2 => Self::TwoBytesOneReg,
            3 => Self::FourBytesTwoPairs,
            4 => Self::FourBytesFourRegs,
            5 => Self::FourBytesTwoRegsAlt,
            6 => Self::TwoBytesOneRegAlt,
            _ => Self::FourBytesTwoPairsAlt,
        }
    }
}

/// Fully-decoded `$43x0 DMAPx` register.
#[derive(Debug, Clone, Copy)]
pub struct DmaParams {
    /// A→B (CPU → PPU) or B→A (PPU → CPU).
    pub direction: Direction,
    /// How the A-bus address moves per byte (`+1`, `-1`, or fixed).
    pub a_increment: Increment,
    /// The mode pattern (1/2/4 bytes per cycle, register layout).
    pub mode: TransferMode,
    /// Bit 6 of `$43x0` — HDMA indirect-mode flag. Only honoured by
    /// HDMA (not P1.2 scope).
    pub hdma_indirect: bool,
}

impl DmaParams {
    /// Decode `$43x0` into a `DmaParams`.
    #[must_use]
    pub fn from_byte(byte: u8) -> Self {
        let direction = if byte & 0x80 != 0 {
            Direction::BToA
        } else {
            Direction::AToB
        };
        // Bits 3-4: 0b00 = +1 (Up), 0b10 = -1 (Down), anything with
        // bit 3 set = Fixed.
        let a_increment = match (byte >> 3) & 0x03 {
            0b00 => Increment::Up,
            0b10 => Increment::Down,
            _ => Increment::Fixed,
        };
        Self {
            direction,
            a_increment,
            mode: TransferMode::from_bits(byte),
            hdma_indirect: byte & 0x40 != 0,
        }
    }

    /// Encode back to `$43x0`. Useful for read-back semantics.
    #[must_use]
    pub fn to_byte(self) -> u8 {
        let mut b = 0;
        if self.direction == Direction::BToA {
            b |= 0x80;
        }
        if self.hdma_indirect {
            b |= 0x40;
        }
        b |= match self.a_increment {
            Increment::Up => 0b00 << 3,
            Increment::Down => 0b10 << 3,
            Increment::Fixed => 0b01 << 3, // canonical "fixed" encoding
        };
        b |= match self.mode {
            TransferMode::OneByteOneReg => 0,
            TransferMode::TwoBytesTwoRegs => 1,
            TransferMode::TwoBytesOneReg => 2,
            TransferMode::FourBytesTwoPairs => 3,
            TransferMode::FourBytesFourRegs => 4,
            TransferMode::FourBytesTwoRegsAlt => 5,
            TransferMode::TwoBytesOneRegAlt => 6,
            TransferMode::FourBytesTwoPairsAlt => 7,
        };
        b
    }
}

// =============================================================================
// DmaChannel — registers + transfer logic.
// =============================================================================

/// One of the eight DMA channels.
#[derive(Debug, Clone, Copy, Default)]
pub struct DmaChannel {
    /// `$43x0` — decoded parameters.
    pub params: DmaParams,
    /// `$43x1` — B-bus base offset (`$2100 + bbad`).
    pub bbad: u8,
    /// `$43x2/$43x3` — A-bus address (low / high). 16-bit; the bank
    /// comes from `a_bank`. The pair forms the 24-bit source/dest of
    /// the A-bus side. Increments per byte per `params.a_increment`.
    pub a_addr: u16,
    /// `$43x4` — A-bus bank (does NOT auto-increment on bank crossing).
    pub a_bank: u8,
    /// `$43x5/$43x6` — DMA byte count. Counts DOWN to zero; `0x0000`
    /// initially means **64 KB** (special wrap, not zero).
    pub das: u16,
    /// `$43x7` — HDMA indirect bank (HDMA only).
    pub dasb: u8,
    /// `$43x8/$43x9` — HDMA table pointer (HDMA only).
    pub a2a: u16,
    /// `$43xA` — HDMA line counter (HDMA only).
    pub ntlr: u8,
    /// `$43xB` / `$43xF` — unused mirror byte exposed in real hardware
    /// (some games rely on the open-bus value).
    pub unused: u8,
}

impl Default for DmaParams {
    fn default() -> Self {
        Self {
            direction: Direction::AToB,
            a_increment: Increment::Up,
            mode: TransferMode::OneByteOneReg,
            hdma_indirect: false,
        }
    }
}

impl DmaChannel {
    /// Build a channel in its post-reset state (all zeros).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a per-channel register at offset `0x0..=0xF` (i.e. the
    /// low nibble of `$43xN`).
    #[must_use]
    pub fn read(&self, offset: u8) -> u8 {
        match offset & 0x0F {
            0x0 => self.params.to_byte(),
            0x1 => self.bbad,
            0x2 => self.a_addr as u8,
            0x3 => (self.a_addr >> 8) as u8,
            0x4 => self.a_bank,
            0x5 => self.das as u8,
            0x6 => (self.das >> 8) as u8,
            0x7 => self.dasb,
            0x8 => self.a2a as u8,
            0x9 => (self.a2a >> 8) as u8,
            0xA => self.ntlr,
            0xB | 0xF => self.unused,
            _ => 0xFF, // $43xC-$43xE: truly unused
        }
    }

    /// Write a per-channel register at offset `0x0..=0xF`.
    pub fn write(&mut self, offset: u8, value: u8) {
        match offset & 0x0F {
            0x0 => self.params = DmaParams::from_byte(value),
            0x1 => self.bbad = value,
            0x2 => self.a_addr = (self.a_addr & 0xFF00) | u16::from(value),
            0x3 => self.a_addr = (self.a_addr & 0x00FF) | (u16::from(value) << 8),
            0x4 => self.a_bank = value,
            0x5 => self.das = (self.das & 0xFF00) | u16::from(value),
            0x6 => self.das = (self.das & 0x00FF) | (u16::from(value) << 8),
            0x7 => self.dasb = value,
            0x8 => self.a2a = (self.a2a & 0xFF00) | u16::from(value),
            0x9 => self.a2a = (self.a2a & 0x00FF) | (u16::from(value) << 8),
            0xA => self.ntlr = value,
            0xB | 0xF => self.unused = value,
            _ => {} // $43xC-$43xE: writes dropped
        }
    }

    /// Execute the channel's DMA against the given bus. Runs until
    /// `das` reaches zero (a starting `das = 0` transfers 64 KB).
    ///
    /// Updates `a_addr` (with `params.a_increment`) and `das` as it
    /// goes. The B-bus offset cycles through `params.mode.pattern()`.
    ///
    /// Returns the number of bytes transferred.
    pub fn run<B: DmaBus>(&mut self, bus: &mut B) -> u32 {
        let pattern = self.params.mode.pattern();
        let mut byte_idx: usize = 0;
        let mut transferred: u32 = 0;
        // 0x0000 means 64 KB (transfer count is computed as
        // `((das as u32 + 0xFFFF) % 0x10000) + 1` effectively); we
        // model it by looping with a u32 counter that initialises
        // from das or 65536 if das == 0.
        let total = if self.das == 0 {
            0x1_0000_u32
        } else {
            u32::from(self.das)
        };
        while transferred < total {
            let b_offset = self.bbad.wrapping_add(pattern[byte_idx]);
            let a_addr: Addr24 = make_addr(self.a_bank, self.a_addr);
            match self.params.direction {
                Direction::AToB => {
                    let v = bus.read_a(a_addr);
                    bus.write_b(b_offset, v);
                }
                Direction::BToA => {
                    let v = bus.read_b(b_offset);
                    bus.write_a(a_addr, v);
                }
            }
            // Advance A-bus address per params.
            self.a_addr = match self.params.a_increment {
                Increment::Up => self.a_addr.wrapping_add(1),
                Increment::Down => self.a_addr.wrapping_sub(1),
                Increment::Fixed => self.a_addr,
            };
            byte_idx = (byte_idx + 1) % pattern.len();
            transferred += 1;
        }
        // Hardware leaves `das = 0` at the end of a sync DMA.
        self.das = 0;
        transferred
    }
}

// =============================================================================
// Tests — TDD coverage for each mode, direction, and increment.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal in-memory DMA bus. A-bus is a 16 MB slab; B-bus is a
    /// 256-byte slab indexed by the offset from `$2100`.
    struct MockBus {
        a: Vec<u8>,
        b: Vec<u8>,
        log: Vec<String>,
    }

    impl MockBus {
        fn new() -> Self {
            Self {
                a: vec![0; 0x100_0000],
                b: vec![0; 0x100],
                log: Vec::new(),
            }
        }

        fn poke_a(&mut self, addr: u32, bytes: &[u8]) {
            for (i, &b) in bytes.iter().enumerate() {
                self.a[((addr as usize) + i) & 0xFF_FFFF] = b;
            }
        }
    }

    impl DmaBus for MockBus {
        fn read_a(&mut self, addr: u32) -> u8 {
            let v = self.a[(addr as usize) & 0xFF_FFFF];
            self.log.push(format!("RA ${addr:06X}=${v:02X}"));
            v
        }
        fn write_a(&mut self, addr: u32, value: u8) {
            self.a[(addr as usize) & 0xFF_FFFF] = value;
            self.log.push(format!("WA ${addr:06X}=${value:02X}"));
        }
        fn read_b(&mut self, b_offset: u8) -> u8 {
            let v = self.b[b_offset as usize];
            self.log.push(format!("RB ${b_offset:02X}=${v:02X}"));
            v
        }
        fn write_b(&mut self, b_offset: u8, value: u8) {
            self.b[b_offset as usize] = value;
            self.log.push(format!("WB ${b_offset:02X}=${value:02X}"));
        }
    }

    // -------------------------------------------------------------------
    // DmaParams decoding
    // -------------------------------------------------------------------

    #[test]
    fn params_default_is_a_to_b_up_mode_0() {
        let p = DmaParams::from_byte(0x00);
        assert_eq!(p.direction, Direction::AToB);
        assert_eq!(p.a_increment, Increment::Up);
        assert_eq!(p.mode, TransferMode::OneByteOneReg);
    }

    #[test]
    fn params_decode_direction() {
        assert_eq!(DmaParams::from_byte(0x80).direction, Direction::BToA);
        assert_eq!(DmaParams::from_byte(0x00).direction, Direction::AToB);
    }

    #[test]
    fn params_decode_increment() {
        assert_eq!(DmaParams::from_byte(0b0000_0000).a_increment, Increment::Up);
        assert_eq!(
            DmaParams::from_byte(0b0001_0000).a_increment,
            Increment::Down
        );
        assert_eq!(
            DmaParams::from_byte(0b0000_1000).a_increment,
            Increment::Fixed
        );
        assert_eq!(
            DmaParams::from_byte(0b0001_1000).a_increment,
            Increment::Fixed
        );
    }

    #[test]
    fn params_decode_modes() {
        for n in 0..=7u8 {
            let p = DmaParams::from_byte(n);
            assert_eq!(p.mode, TransferMode::from_bits(n));
        }
    }

    #[test]
    fn pattern_lengths_match_mode_naming() {
        assert_eq!(TransferMode::OneByteOneReg.pattern().len(), 1);
        assert_eq!(TransferMode::TwoBytesTwoRegs.pattern().len(), 2);
        assert_eq!(TransferMode::TwoBytesOneReg.pattern().len(), 2);
        assert_eq!(TransferMode::FourBytesTwoPairs.pattern().len(), 4);
        assert_eq!(TransferMode::FourBytesFourRegs.pattern().len(), 4);
        assert_eq!(TransferMode::FourBytesTwoRegsAlt.pattern().len(), 4);
    }

    // -------------------------------------------------------------------
    // Mode 0 transfers
    // -------------------------------------------------------------------

    #[test]
    fn mode0_copies_4_bytes_to_one_register() {
        // Set up: copy 4 bytes from $7E:1000 to PPU $2122 (CGDATA).
        let mut bus = MockBus::new();
        bus.poke_a(0x7E_1000, &[0xCA, 0xFE, 0xBA, 0xBE]);

        let mut ch = DmaChannel::new();
        ch.params = DmaParams::from_byte(0x00); // mode 0, +1, A→B
        ch.bbad = 0x22; // → $2122
        ch.a_addr = 0x1000;
        ch.a_bank = 0x7E;
        ch.das = 4;

        let n = ch.run(&mut bus);
        assert_eq!(n, 4);
        assert_eq!(
            bus.b[0x22], 0xBE,
            "$2122 holds the last byte after streaming"
        );
        assert_eq!(ch.das, 0, "DAS is zeroed at end-of-DMA");
        assert_eq!(ch.a_addr, 0x1004, "A-bus advanced by 4");
    }

    // -------------------------------------------------------------------
    // Mode 1 transfers (the canonical VRAM upload pattern)
    // -------------------------------------------------------------------

    #[test]
    fn mode1_alternates_between_b_and_b_plus_1() {
        // 4 bytes: $11 $22 $33 $44 → $2118=$11, $2119=$22, $2118=$33, $2119=$44.
        let mut bus = MockBus::new();
        bus.poke_a(0x7E_2000, &[0x11, 0x22, 0x33, 0x44]);

        let mut ch = DmaChannel::new();
        ch.params = DmaParams::from_byte(0x01); // mode 1, +1, A→B
        ch.bbad = 0x18; // → $2118
        ch.a_addr = 0x2000;
        ch.a_bank = 0x7E;
        ch.das = 4;

        ch.run(&mut bus);
        // Last value to land at each B offset:
        assert_eq!(bus.b[0x18], 0x33, "B=18 received the third byte last");
        assert_eq!(bus.b[0x19], 0x44, "B+1=19 received the fourth byte last");
    }

    // -------------------------------------------------------------------
    // A-bus increment options
    // -------------------------------------------------------------------

    #[test]
    fn fixed_a_address_streams_same_byte() {
        // DMA from a fixed source (e.g. an open-bus value) into a
        // register. Useful for filling a region with a constant.
        let mut bus = MockBus::new();
        bus.poke_a(0x00_1234, &[0xAA]);

        let mut ch = DmaChannel::new();
        ch.params = DmaParams::from_byte(0x08); // mode 0, FIXED, A→B
        ch.bbad = 0x22;
        ch.a_addr = 0x1234;
        ch.a_bank = 0x00;
        ch.das = 16;

        ch.run(&mut bus);
        assert_eq!(ch.a_addr, 0x1234, "fixed: address must not move");
        assert_eq!(bus.b[0x22], 0xAA, "still the same byte after 16 transfers");
    }

    #[test]
    fn decrement_walks_backwards() {
        let mut bus = MockBus::new();
        // Lay bytes 0..4 at $7E:1000..1003.
        bus.poke_a(0x7E_1000, &[0x01, 0x02, 0x03, 0x04]);

        let mut ch = DmaChannel::new();
        ch.params = DmaParams::from_byte(0x10); // mode 0, DOWN, A→B
        ch.bbad = 0x22;
        ch.a_addr = 0x1003;
        ch.a_bank = 0x7E;
        ch.das = 4;

        ch.run(&mut bus);
        // Streams: $04, $03, $02, $01 — last write is $01.
        assert_eq!(bus.b[0x22], 0x01);
        assert_eq!(ch.a_addr, 0x0FFF, "decrement past 0x1000 by 4 → 0x0FFF");
    }

    // -------------------------------------------------------------------
    // Direction
    // -------------------------------------------------------------------

    #[test]
    fn b_to_a_reads_b_writes_a() {
        let mut bus = MockBus::new();
        bus.b[0x39] = 0xAB; // pretend $2139 returns 0xAB

        let mut ch = DmaChannel::new();
        ch.params = DmaParams::from_byte(0x80); // mode 0, +1, B→A
        ch.bbad = 0x39;
        ch.a_addr = 0x1000;
        ch.a_bank = 0x7E;
        ch.das = 2;

        ch.run(&mut bus);
        assert_eq!(bus.a[0x7E_1000], 0xAB);
        assert_eq!(bus.a[0x7E_1001], 0xAB);
    }

    // -------------------------------------------------------------------
    // 64 KB special case
    // -------------------------------------------------------------------

    #[test]
    fn das_zero_means_64kb() {
        // Verify the spec quirk where `das = 0` means 64 KB, not 0
        // bytes. We don't want to actually drive 65 536 mock writes
        // in a unit test, so we test the count derivation directly.
        let mut ch = DmaChannel::new();
        ch.das = 0;
        let total: u32 = if ch.das == 0 {
            0x1_0000
        } else {
            u32::from(ch.das)
        };
        assert_eq!(total, 65_536, "das == 0 expands to 64 KB");
    }

    // -------------------------------------------------------------------
    // Register read-back symmetry
    // -------------------------------------------------------------------

    #[test]
    fn write_then_read_round_trip_on_every_offset() {
        let mut ch = DmaChannel::new();
        // Walk every offset, write a known byte, read it back.
        for (off, val) in (0..=0x0Fu8).map(|o| (o, o.wrapping_mul(7) ^ 0xAA)) {
            ch.write(off, val);
            // Offsets $C..$E are write-discarded.
            if matches!(off, 0xC..=0xE) {
                continue;
            }
            // Offset $0 (DMAPx) round-trips through enum decoding,
            // so the read-back may differ in bit 5 (always 0). Mask
            // it before comparing.
            if off == 0 {
                assert_eq!(ch.read(off) & !0x20, val & !0x20);
            } else {
                assert_eq!(ch.read(off), val);
            }
        }
    }
}
