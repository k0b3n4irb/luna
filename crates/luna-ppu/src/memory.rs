//! PPU-side memory regions: VRAM, CGRAM, OAM.
//!
//! Each region exposes the auto-incrementing read/write semantics that
//! games use to upload data via the PPU registers.

// =============================================================================
// VRAM — 64 KB tile/tilemap memory.
// =============================================================================

/// SNES VRAM: 64 KB = 32 768 16-bit words.
///
/// The PPU is accessed as 16-bit words. Software writes the address via
/// `$2116/$2117` (VMADDL/H) and then writes data via `$2118/$2119`
/// (VMDATAL/H). The `VMAIN` register at `$2115` controls increment
/// behaviour: after low or high byte access, by how much, and in what
/// word-mapping pattern.
pub struct Vram {
    data: Box<[u8; 0x1_0000]>,
    /// 16-bit word address (the SNES exposes 32 768 words, so the high
    /// bit of this is ignored when computing the byte offset).
    pub address: u16,
    /// Decoded VMAIN settings.
    pub vmain: VmainSettings,
    /// Read prefetch buffer (2 bytes, low + high). Reads from `$2139`
    /// and `$213A` return the buffered byte first, **then** trigger the
    /// next read into the buffer. This is the 65816 "delayed read"
    /// quirk that games rely on.
    prefetch_lo: u8,
    prefetch_hi: u8,
}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

impl Vram {
    /// Build an empty VRAM (all zeroes).
    #[must_use]
    pub fn new() -> Self {
        let v = vec![0u8; 0x1_0000].into_boxed_slice();
        let data: Box<[u8; 0x1_0000]> = v.try_into().expect("64 KB slice into fixed array");
        Self {
            data,
            address: 0,
            // Post-reset VMAIN value (= byte 0x00): step 1, increment on
            // low-byte access, no remap. Note `VmainSettings::default()`
            // gives step=0 which is invalid — we want the spec default.
            vmain: VmainSettings::from_byte(0),
            prefetch_lo: 0,
            prefetch_hi: 0,
        }
    }

    /// Direct read for tests and the future renderer.
    #[must_use]
    pub fn peek(&self, addr: u16) -> u8 {
        self.data[usize::from(addr)]
    }

    /// Direct write for tests and DMA fast paths.
    pub fn poke(&mut self, addr: u16, value: u8) {
        self.data[usize::from(addr)] = value;
    }

    /// Set the word address (`$2116/$2117` writes). The hardware also
    /// triggers a prefetch of the byte at the new address.
    pub fn set_address(&mut self, lo: u8, hi: u8) {
        self.address = u16::from(lo) | (u16::from(hi) << 8);
        let byte_addr = self.byte_addr();
        self.prefetch_lo = self.data[byte_addr];
        self.prefetch_hi = self.data[byte_addr.wrapping_add(1) & 0xFFFF];
    }

    /// Compute the byte address for the current word address, applying
    /// VMAIN's word remapping.
    #[must_use]
    pub fn byte_addr(&self) -> usize {
        // 32 768 words → bottom 15 bits of address index the byte pair.
        let word = self.address & 0x7FFF;
        let word = self.vmain.remap(word);
        (usize::from(word) << 1) & 0xFFFF
    }

    /// Write the low byte (`$2118`) at the current address. Increments
    /// the address if VMAIN says "increment on low".
    pub fn write_lo(&mut self, value: u8) {
        let byte_addr = self.byte_addr();
        self.data[byte_addr] = value;
        if !self.vmain.increment_on_high {
            self.advance();
        }
    }

    /// Write the high byte (`$2119`) at the current address + 1.
    pub fn write_hi(&mut self, value: u8) {
        let byte_addr = self.byte_addr().wrapping_add(1) & 0xFFFF;
        self.data[byte_addr] = value;
        if self.vmain.increment_on_high {
            self.advance();
        }
    }

    /// Read the low byte (`$2139`). Returns the buffered byte first,
    /// then refills the buffer.
    pub fn read_lo(&mut self) -> u8 {
        let v = self.prefetch_lo;
        if !self.vmain.increment_on_high {
            self.refill_prefetch();
            self.advance();
        }
        v
    }

    /// Read the high byte (`$213A`).
    pub fn read_hi(&mut self) -> u8 {
        let v = self.prefetch_hi;
        if self.vmain.increment_on_high {
            self.refill_prefetch();
            self.advance();
        }
        v
    }

    fn refill_prefetch(&mut self) {
        let byte_addr = self.byte_addr();
        self.prefetch_lo = self.data[byte_addr];
        self.prefetch_hi = self.data[byte_addr.wrapping_add(1) & 0xFFFF];
    }

    fn advance(&mut self) {
        self.address = self.address.wrapping_add(self.vmain.step);
    }
}

/// Decoded `$2115 VMAIN` register.
#[derive(Debug, Clone, Copy, Default)]
pub struct VmainSettings {
    /// `true` ⇒ address increments on high-byte access (`$2119`/`$213A`).
    /// `false` ⇒ increments on low-byte access (`$2118`/`$2139`).
    pub increment_on_high: bool,
    /// Address increment amount: 1, 32, 128, or 128 (the duplicate is
    /// intentional — hardware exposes only the 4 patterns the games
    /// use).
    pub step: u16,
    /// Word-mapping mode (0 = none, 1/2/3 = various tile-row remaps for
    /// 2bpp / 4bpp / 8bpp tile layouts).
    pub remap_mode: u8,
}

impl VmainSettings {
    /// Decode a `$2115` byte.
    #[must_use]
    pub fn from_byte(byte: u8) -> Self {
        let step = match byte & 0x03 {
            0 => 1,
            1 => 32,
            2 => 128,
            _ => 128,
        };
        Self {
            increment_on_high: byte & 0x80 != 0,
            step,
            remap_mode: (byte >> 2) & 0x03,
        }
    }

    /// Apply the word-address remap. Mode 0 is identity; the other
    /// modes shuffle bits to make tile uploads contiguous.
    #[must_use]
    pub fn remap(self, word_addr: u16) -> u16 {
        match self.remap_mode {
            0 => word_addr,
            1 => (word_addr & 0xFF00) | ((word_addr & 0x001F) << 3) | ((word_addr & 0x00E0) >> 5),
            2 => (word_addr & 0xFE00) | ((word_addr & 0x003F) << 3) | ((word_addr & 0x01C0) >> 6),
            3 => (word_addr & 0xFC00) | ((word_addr & 0x007F) << 3) | ((word_addr & 0x0380) >> 7),
            _ => unreachable!(),
        }
    }
}

// =============================================================================
// CGRAM — 512 bytes palette memory.
// =============================================================================

/// 256 × 15-bit BGR colors stored as 256 little-endian u16 words.
///
/// Software writes a u8 address via `$2121` (CGADD), then writes pairs
/// of bytes via `$2122` (CGDATA) — the first sets the low byte of the
/// word, the second sets the high byte (the latter triggers the word
/// advance).
pub struct Cgram {
    data: [u8; 0x200],
    /// Word address (0..=255).
    pub address: u8,
    /// Latch for the low byte while waiting for the high.
    latch: u8,
    /// `true` ⇒ next `$2122` write is the high byte.
    high_pending: bool,
}

impl Default for Cgram {
    fn default() -> Self {
        Self::new()
    }
}

impl Cgram {
    /// Build an empty palette.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: [0; 0x200],
            address: 0,
            latch: 0,
            high_pending: false,
        }
    }

    /// Direct read for tests and the renderer.
    #[must_use]
    pub fn peek(&self, addr: u16) -> u8 {
        self.data[usize::from(addr) & 0x1FF]
    }

    /// Direct write for tests and DMA fast paths.
    pub fn poke(&mut self, addr: u16, value: u8) {
        self.data[usize::from(addr) & 0x1FF] = value;
    }

    /// `$2121` write — set the word address. Also resets the
    /// low/high latch so the next `$2122` write is the low byte.
    pub fn set_address(&mut self, value: u8) {
        self.address = value;
        self.high_pending = false;
    }

    /// `$2122` write — first call latches the low byte, second call
    /// stores both bytes and advances the address.
    pub fn write(&mut self, value: u8) {
        if self.high_pending {
            let off = usize::from(self.address) << 1;
            self.data[off] = self.latch;
            self.data[off + 1] = value;
            self.address = self.address.wrapping_add(1);
            self.high_pending = false;
        } else {
            self.latch = value;
            self.high_pending = true;
        }
    }

    /// `$213B` read — returns the byte at the current word address,
    /// alternating low/high and advancing on the high read.
    pub fn read(&mut self) -> u8 {
        let off = usize::from(self.address) << 1;
        if self.high_pending {
            // After high-byte read the address advances.
            let v = self.data[off + 1];
            self.address = self.address.wrapping_add(1);
            self.high_pending = false;
            v
        } else {
            self.high_pending = true;
            self.data[off]
        }
    }

    /// Decode a CGRAM word as a 16-bit BGR555 color (low byte first).
    /// Useful for tests and the future renderer.
    #[must_use]
    pub fn color(&self, index: u8) -> u16 {
        let off = usize::from(index) << 1;
        u16::from(self.data[off]) | (u16::from(self.data[off + 1]) << 8)
    }
}

// =============================================================================
// OAM — 544 bytes (low table 512 + high table 32).
// =============================================================================

/// Sprite attribute memory.
///
/// The low table (`$0000-$01FF`) holds 128 × 4-byte entries (x, y,
/// tile, attributes). The high table (`$0200-$021F`) holds 32 × 1-byte
/// packed entries — 2 bits per sprite (x.high + size flag).
///
/// Software addresses OAM as a **9-bit word address** (set via `$2102`
/// / `$2103`). The hardware uses an **internal 10-bit byte address**
/// derived as `(word_address & 0x1FF) << 1`. Bit 9 of the byte address
/// selects the high table. Each `$2104` write advances the byte
/// address by one. In the low table, even-byte writes are latched and
/// committed only when the odd-byte write arrives, so software
/// updating a sprite never observes a torn state mid-frame.
pub struct Oam {
    data: [u8; 0x220],
    /// 9-bit word address set via OAMADDL/H. Kept around so we can
    /// reset the byte counter to its canonical position on each
    /// register write.
    pub word_address: u16,
    /// 10-bit byte address used internally for `$2104` reads/writes.
    /// Advances by 1 per access; resets to `word_address << 1` on each
    /// OAMADDL/H write.
    pub address: u16,
    latch: u8,
    /// Per-sprite "last non-hidden Y" shadow.
    ///
    /// Many games keep their sprites at `Y = $F0` (off-screen)
    /// throughout most of the visible frame and only briefly place
    /// them at a real position during their NMI handler before
    /// hiding them again. Without cycle-accurate scheduling, a free-
    /// running renderer that samples the live OAM almost always
    /// catches the "all hidden" phase — every sprite ends up
    /// invisible despite the game clearly meaning to draw it.
    ///
    /// The shadow captures, for each of 128 sprites, the **last Y
    /// value that wasn't `$F0`**. The renderer falls back to it when
    /// the live Y is `$F0`. This gives a stable "last seen" view of
    /// each sprite — close enough to display Mario, Yoshi, the menu
    /// cursor, etc. on real title screens until proper timing lands.
    pub shadow_y: [u8; 128],
}

impl Default for Oam {
    fn default() -> Self {
        Self::new()
    }
}

impl Oam {
    /// Build an empty OAM.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: [0; 0x220],
            word_address: 0,
            address: 0,
            latch: 0,
            // Start with everything hidden — nothing renders until
            // the game does its first frame of OAM uploads.
            shadow_y: [0xF0; 128],
        }
    }

    /// Helper: if this byte address is the Y byte of a low-table
    /// sprite (`offset & 3 == 1`, `offset < 0x200`) **and** the
    /// value isn't the hide signal `$F0`, update [`Self::shadow_y`]
    /// for the corresponding sprite slot.
    fn update_shadow(&mut self, byte_addr: usize, value: u8) {
        if byte_addr >= 0x200 {
            return;
        }
        if byte_addr & 3 != 1 {
            return;
        }
        if value == 0xF0 {
            return;
        }
        self.shadow_y[byte_addr / 4] = value;
    }

    /// Direct read for tests and the renderer.
    #[must_use]
    pub fn peek(&self, addr: u16) -> u8 {
        self.data[usize::from(addr) & 0x21F]
    }

    /// Direct write for tests and DMA fast paths.
    pub fn poke(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr) & 0x21F;
        self.data[a] = value;
        self.update_shadow(a, value);
    }

    fn reset_byte_address(&mut self) {
        self.address = (self.word_address & 0x01FF) << 1;
    }

    /// `$2102` write — low 8 bits of the word address.
    pub fn set_address_low(&mut self, value: u8) {
        self.word_address = (self.word_address & 0xFF00) | u16::from(value);
        self.reset_byte_address();
    }

    /// `$2103` write — bit 8 of the word address (lives in bit 0 of the
    /// written byte). Bit 7 of the value is the priority-rotation flag
    /// (not yet modeled).
    pub fn set_address_high(&mut self, value: u8) {
        self.word_address = (self.word_address & 0x00FF) | (u16::from(value & 0x01) << 8);
        self.reset_byte_address();
    }

    /// `$2104` write — OAM data write with the even/odd dance for the
    /// low table and direct byte write for the high table.
    pub fn write(&mut self, value: u8) {
        let addr = self.address;
        if addr & 0x200 != 0 {
            // High table: direct byte write at the wrapped offset.
            let off = usize::from(addr & 0x21F);
            self.data[off] = value;
        } else if addr & 1 == 0 {
            // Low table even byte — latch until the odd-byte commit.
            self.latch = value;
        } else {
            // Low table odd byte — commit the latched pair atomically.
            let even_off = usize::from(addr.wrapping_sub(1) & 0x1FF);
            self.data[even_off] = self.latch;
            self.data[even_off + 1] = value;
            // The pair we just committed lives at `even_off` (even
            // byte) and `even_off + 1` (the byte we just wrote). If
            // that's a sprite's Y, refresh the shadow.
            self.update_shadow(even_off + 1, value);
        }
        self.advance();
    }

    /// `$2138` read — OAM data read at the current byte address.
    /// Advances by one byte per read; no latching on reads.
    pub fn read(&mut self) -> u8 {
        let value = self.data[usize::from(self.address) & 0x21F];
        self.advance();
        value
    }

    fn advance(&mut self) {
        // Byte address wraps modulo 0x220 (low table 0x000-0x1FF + high
        // table 0x200-0x21F = 544 bytes). Past that, the hardware
        // re-enters the low table at byte 0.
        let next = self.address.wrapping_add(1);
        self.address = if next >= 0x220 { 0 } else { next };
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmain_decodes_step_and_remap_modes() {
        let v = VmainSettings::from_byte(0x00);
        assert_eq!(v.step, 1);
        assert!(!v.increment_on_high);
        assert_eq!(v.remap_mode, 0);

        let v = VmainSettings::from_byte(0x81);
        assert_eq!(v.step, 32);
        assert!(v.increment_on_high);

        let v = VmainSettings::from_byte(0x0F);
        assert_eq!(v.step, 128);
        assert_eq!(v.remap_mode, 3);
    }

    #[test]
    fn vram_write_lo_advances_when_vmain_says_so() {
        // VMAIN default (0x00): increment on $2118 (low). Means each
        // $2118 write writes byte (addr*2) and advances; the matching
        // $2119 then writes the HIGH byte of the *next* word, which is
        // a real game-side quirk to be aware of. Games that want both
        // bytes of the same word use VMAIN=0x80 (see next test).
        let mut v = Vram::new();
        v.set_address(0x00, 0x00);
        v.write_lo(0xAA); // byte 0, advance to word 1
        assert_eq!(v.address, 1);
        assert_eq!(v.peek(0), 0xAA);
        v.write_lo(0xBB); // byte 2, advance to word 2
        assert_eq!(v.address, 2);
        assert_eq!(v.peek(2), 0xBB);
    }

    #[test]
    fn vram_write_advances_on_high_with_vmain_0x80() {
        let mut v = Vram::new();
        v.vmain = VmainSettings::from_byte(0x80);
        v.set_address(0x00, 0x00);
        v.write_lo(0x11);
        v.write_hi(0x22);
        assert_eq!(v.address, 1, "address advances on the high-byte write");
        assert_eq!(v.peek(0), 0x11);
        assert_eq!(v.peek(1), 0x22);
    }

    #[test]
    fn vram_read_returns_prefetched_byte_after_set_address() {
        // After set_address, the first read returns the byte at the
        // address we just set. (Subsequent reads work but the prefetch
        // quirk — refill-then-advance — makes the second value stale.
        // That's verified in the next test.)
        let mut v = Vram::new();
        v.poke(0, 0xAA);
        v.poke(1, 0xBB);
        v.poke(2, 0xCC);
        v.poke(3, 0xDD);
        v.set_address(1, 0); // word 1 → byte 2/3 prefetched

        assert_eq!(v.read_lo(), 0xCC);
        assert_eq!(v.read_hi(), 0xDD);
    }

    #[test]
    fn vram_read_prefetch_quirk_returns_stale_on_consecutive_low_reads() {
        // The canonical 65816 prefetch quirk: read_lo returns the
        // BUFFERED byte (prefetched on the previous access), THEN
        // refills the buffer and advances. So two reads in a row
        // return the same byte — games handle this with a throwaway
        // first read.
        let mut v = Vram::new();
        v.poke(0, 0xAA);
        v.poke(2, 0xCC);
        v.set_address(0, 0);

        assert_eq!(v.read_lo(), 0xAA, "first read returns prefetch from word 0");
        assert_eq!(
            v.read_lo(),
            0xAA,
            "second read returns same stale prefetch (refill happened *before* advance)"
        );
        assert_eq!(
            v.read_lo(),
            0xCC,
            "third read returns the now-fresh prefetch from word 1"
        );
    }

    #[test]
    fn cgram_word_write_dance() {
        let mut c = Cgram::new();
        c.set_address(0);
        c.write(0x34); // low byte latched
        c.write(0x12); // high byte → commit, advance
        assert_eq!(c.color(0), 0x1234);
        assert_eq!(c.address, 1);
    }

    #[test]
    fn cgram_set_address_resets_high_pending() {
        let mut c = Cgram::new();
        c.write(0x55); // latched as low, never paired
        c.set_address(2); // reset
        c.write(0xAA);
        c.write(0xBB);
        // Color at index 2 = 0xBBAA, color at index 0 unaffected.
        assert_eq!(c.color(2), 0xBBAA);
        assert_eq!(c.color(0), 0x0000);
    }

    #[test]
    fn oam_low_table_even_odd_commit() {
        let mut o = Oam::new();
        o.set_address_low(0);
        o.set_address_high(0);
        o.write(0x10); // latched as low
        o.write(0x20); // commit pair at $0000-$0001
        assert_eq!(o.peek(0), 0x10);
        assert_eq!(o.peek(1), 0x20);
        assert_eq!(o.address, 2);
    }

    #[test]
    fn oam_high_table_direct_byte_writes() {
        let mut o = Oam::new();
        o.set_address_low(0x00);
        o.set_address_high(0x01); // bit 9 set → high table
        o.write(0xCC);
        assert_eq!(o.peek(0x200), 0xCC);
    }

    #[test]
    fn oam_wraps_at_0x220() {
        let mut o = Oam::new();
        o.address = 0x21F;
        o.write(0x77);
        // Advance from $21F wraps to 0 (since $220 is one past the end).
        assert_eq!(o.address, 0);
    }
}
