//! Super FX (GSU — Graphics Support Unit) cartridge mapping + coprocessor.
//!
//! Unlike the SA-1 (which embeds a second 65C816 and therefore needs a
//! `luna-core`-side chip wrapper), the GSU is a self-contained bespoke
//! RISC core with no external CPU dependency. The entire chip — register
//! file, instruction-cache RAM, the SNES↔GSU memory map and the MMIO
//! handshake — lives here in one [`SuperFxMapper`], driven by the existing
//! [`Mapper::step_coproc`] hook.
//!
//! This is the **scaffolding phase**: memory map, the register/MMIO
//! surface, the GO / STOP / IRQ handshake, and a diagnostic snapshot. The
//! GSU *instruction engine* (the opcode interpreter), the ROM/RAM buffer
//! timing and the pixel-plot pipeline land in later phases — `step_coproc`
//! is a no-op for now, so a game can arm the GSU and the SNES side behaves
//! correctly, but the GSU executes no opcodes yet.
//!
//! Reference: `docs/superfx_reference.md` (synthesised ares + Mesen2 spec).
//! Citations like `(spec §1.9)` index that document; ultimate sources are
//! ares `ares/sfc/coprocessor/superfx/*` + `ares/component/processor/gsu/*`
//! and Mesen2 `Core/SNES/Coprocessors/GSU`.

use crate::mapper::{Mapper, MapperKind};
use crate::types::{Addr24, bank_of, offset_of};

// --- SFR (Status Flag Register) bit masks (spec §1.2) ---------------------
// Only the bits the scaffolding handshake touches are defined here; the ALU
// flag bits (z/cy/s/ov/alt1/alt2/il/ih/b) arrive with the engine phase.
const SFR_G: u16 = 1 << 5; // go (GSU running)
const SFR_R: u16 = 1 << 6; // ROM-read pending (R14 fetch in flight)
const SFR_IRQ: u16 = 1 << 15; // interrupt asserted to SNES

/// The GSU register file (spec §1). Owned by [`SuperFxMapper`]; MMIO at
/// `$3000-$303F` reads/writes it, and the engine phase will consume it.
#[derive(Debug)]
struct Registers {
    /// R0–R15. R14 = ROM-load trigger, R15 = PC (spec §1.1).
    r: [u16; 16],
    /// Status flags — raw 16-bit; see the `SFR_*` masks.
    sfr: u16,
    /// Program Bank Register — bank for PC fetches (spec §1.3).
    pbr: u8,
    /// ROM data Bank Register — bank for the R14 ROM-buffer fetch.
    rombr: u8,
    /// RAM data Bank Register — 1-bit RAM bank for LD/ST buffer ops.
    rambr: bool,
    /// Cache Base Register — base PC of the 512-byte cache window.
    cbr: u16,
    /// Screen Base Register — Game Pak RAM tile-map base (`scbr << 10`).
    scbr: u8,
    /// SCMR height select (2 bits, scrambled wire layout — spec §1.4).
    scmr_ht: u8,
    /// SCMR ROM-access grant (1 = GSU owns ROM, SNES locked out).
    scmr_ron: bool,
    /// SCMR RAM-access grant (1 = GSU owns RAM, SNES locked out).
    scmr_ran: bool,
    /// SCMR colour-depth mode (0..3 → bpp 2/4/4/8).
    scmr_md: u8,
    /// Colour register (PLOT colour).
    colr: u8,
    /// Plot Option Register — raw 5 bits (decoded by the plot phase).
    por: u8,
    /// Backup-RAM write enable ($3033).
    bramr: bool,
    /// Version Code Register (read-only $303B, reset 0x04).
    vcr: u8,
    /// Config register — raw byte; bit7 = IRQ mask, bit5 = high-speed mult.
    cfgr: u8,
    /// Clock Select — false = 10.7 MHz, true = 21.4 MHz.
    clsr: bool,
}

impl Registers {
    /// Power / reset state (spec §1.8).
    const fn reset() -> Self {
        Self {
            r: [0; 16],
            sfr: 0,
            pbr: 0,
            rombr: 0,
            rambr: false,
            cbr: 0,
            scbr: 0,
            scmr_ht: 0,
            scmr_ron: false,
            scmr_ran: false,
            scmr_md: 0,
            colr: 0,
            por: 0,
            bramr: false,
            vcr: 0x04,
            cfgr: 0,
            clsr: false,
        }
    }

    /// SCMR wire byte ← fields (spec §1.4): `bit5=ht_hi, bit4=ron,
    /// bit3=ran, bit2=ht_lo, bits1:0=md`.
    const fn scmr_byte(&self) -> u8 {
        ((self.scmr_ht >> 1) << 5)
            | ((self.scmr_ron as u8) << 4)
            | ((self.scmr_ran as u8) << 3)
            | ((self.scmr_ht & 1) << 2)
            | self.scmr_md
    }

    /// SCMR fields ← wire byte (spec §1.4, the scrambled `ht`).
    const fn set_scmr(&mut self, data: u8) {
        self.scmr_ht = (((data & 0x20 != 0) as u8) << 1) | (data & 0x04 != 0) as u8;
        self.scmr_ron = data & 0x10 != 0;
        self.scmr_ran = data & 0x08 != 0;
        self.scmr_md = data & 0x03;
    }
}

/// Diagnostic snapshot of the GSU's architectural registers — the Super FX
/// analogue of [`crate::Sa1Snapshot`]. Lets the CLI / GUI debugger observe
/// GSU state through the same kind of surface SA-1 already exposes.
#[derive(Debug, Clone, Copy)]
pub struct SuperFxSnapshot {
    /// R0–R15 (R15 = PC).
    pub r: [u16; 16],
    /// Status flags (raw 16-bit).
    pub sfr: u16,
    /// Program bank register.
    pub pbr: u8,
    /// ROM data bank register.
    pub rombr: u8,
    /// RAM data bank register (1-bit).
    pub rambr: bool,
    /// Cache base register.
    pub cbr: u16,
    /// Screen base register.
    pub scbr: u8,
    /// Screen-mode register, packed wire byte.
    pub scmr: u8,
    /// Colour register.
    pub colr: u8,
    /// Plot-option register (raw 5 bits).
    pub por: u8,
    /// Backup-RAM write enable.
    pub bramr: bool,
    /// Version code register.
    pub vcr: u8,
    /// Config register (raw byte).
    pub cfgr: u8,
    /// Clock select (false = 10.7 MHz, true = 21.4 MHz).
    pub clsr: bool,
    /// `true` while the GSU is running (SFR go flag set).
    pub running: bool,
}

/// Super FX (GSU) mapper + coprocessor state.
pub struct SuperFxMapper {
    /// ROM image, zero-padded up to `rom_mask + 1` (a power of two) so
    /// `offset & rom_mask` always lands in bounds and mirrors cleanly
    /// (spec §3.1: ares rounds the ROM size up to a power of two).
    rom: Vec<u8>,
    /// Original (unpadded) ROM length, reported by [`Mapper::rom_size`].
    rom_len: usize,
    /// Power-of-two mask over the padded ROM image.
    rom_mask: usize,
    /// Game Pak work RAM (the GSU's plot target, shared with the SNES);
    /// allocated to `ram_mask + 1` (a power of two).
    ram: Vec<u8>,
    /// Original (unpadded) RAM length, reported by [`Mapper::sram_size`].
    ram_len: usize,
    ram_mask: usize,
    /// Register file.
    regs: Registers,
    /// 512-byte instruction cache + 32 line-valid flags (spec §5).
    cache: Box<[u8; 512]>,
    cache_valid: [bool; 32],
}

impl SuperFxMapper {
    /// Build a Super FX mapper around `rom`, with `ram_size` bytes of
    /// Game Pak work RAM.
    #[must_use]
    pub fn new(rom: Vec<u8>, ram_size: usize) -> Self {
        let rom_len = rom.len();
        let rom_alloc = round_up_pow2(rom_len);
        let mut rom = rom;
        rom.resize(rom_alloc, 0);
        let ram_len = ram_size.max(1);
        let ram_alloc = round_up_pow2(ram_len);
        Self {
            rom,
            rom_len,
            rom_mask: rom_alloc - 1,
            ram: vec![0; ram_alloc],
            ram_len,
            ram_mask: ram_alloc - 1,
            regs: Registers::reset(),
            cache: Box::new([0; 512]),
            cache_valid: [false; 32],
        }
    }

    /// Diagnostic snapshot of the GSU's architectural registers.
    #[must_use]
    pub const fn snapshot(&self) -> SuperFxSnapshot {
        SuperFxSnapshot {
            r: self.regs.r,
            sfr: self.regs.sfr,
            pbr: self.regs.pbr,
            rombr: self.regs.rombr,
            rambr: self.regs.rambr,
            cbr: self.regs.cbr,
            scbr: self.regs.scbr,
            scmr: self.regs.scmr_byte(),
            colr: self.regs.colr,
            por: self.regs.por,
            bramr: self.regs.bramr,
            vcr: self.regs.vcr,
            cfgr: self.regs.cfgr,
            clsr: self.regs.clsr,
            running: self.regs.sfr & SFR_G != 0,
        }
    }

    // --- SFR helpers ------------------------------------------------------
    const fn sfr_get(&self, mask: u16) -> bool {
        self.regs.sfr & mask != 0
    }
    const fn sfr_set(&mut self, mask: u16, on: bool) {
        if on {
            self.regs.sfr |= mask;
        } else {
            self.regs.sfr &= !mask;
        }
    }

    /// Invalidate every instruction-cache line (spec §5, `flushCache`).
    const fn flush_cache(&mut self) {
        self.cache_valid = [false; 32];
    }

    // --- SNES-side ROM / RAM address translation (spec §3.2) -------------

    /// ROM offset for a SNES-CPU access, or `None` if the address is not
    /// in a Super FX ROM window. Two views of the same image:
    /// `$00-$3F/$80-$BF:$8000-$FFFF` `LoROM`, `$40-$5F/$C0-$DF:$0000-$FFFF`
    /// linear (spec §3.2).
    const fn rom_offset(bank: u8, offset: u16) -> Option<usize> {
        let b = bank & 0x7F; // fold $80-$FF mirror onto $00-$7F
        if b <= 0x3F && offset >= 0x8000 {
            Some((b as usize * 0x8000) + (offset as usize - 0x8000))
        } else if 0x40 <= b && b <= 0x5F {
            Some((b as usize - 0x40) * 0x10000 + offset as usize)
        } else {
            None
        }
    }

    /// RAM offset for a SNES-CPU access (spec §3.2): the linear window at
    /// `$70-$71/$F0-$F1:$0000-$FFFF` and the `$00-$3F/$80-$BF:$6000-$7FFF`
    /// 8 KB window.
    const fn ram_offset(bank: u8, offset: u16) -> Option<usize> {
        let b = bank & 0x7F;
        if 0x70 <= b && b <= 0x71 {
            Some((b as usize - 0x70) * 0x10000 + offset as usize)
        } else if b <= 0x3F && 0x6000 <= offset && offset <= 0x7FFF {
            Some(offset as usize - 0x6000)
        } else {
            None
        }
    }

    /// Is `addr` inside the GSU MMIO window `$00-$3F/$80-$BF:$3000-$34FF`?
    /// Returns the normalised internal address `$3000 | (off & 0x3FF)`
    /// (spec §3.2: ares mirrors every 0x400).
    const fn mmio_addr(bank: u8, offset: u16) -> Option<u16> {
        let b = bank & 0x7F;
        if b <= 0x3F && 0x3000 <= offset && offset <= 0x34FF {
            Some(0x3000 | (offset & 0x03FF))
        } else {
            None
        }
    }

    /// The fixed 16-byte "GSU busy" vector the SNES sees in place of ROM
    /// while the GSU is running and owns ROM access (spec §3.3,
    /// ares bus.cpp:11-20).
    const fn busy_rom_vector(addr_low: u8) -> u8 {
        const VECTOR: [u8; 16] = [
            0x00, 0x01, 0x00, 0x01, 0x04, 0x01, 0x00, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0x01,
            0x0C, 0x01,
        ];
        VECTOR[(addr_low & 0x0F) as usize]
    }

    // --- MMIO register file (spec §1.9) ----------------------------------

    /// Read a GSU MMIO register at the normalised address `a`.
    fn mmio_read(&mut self, a: u16) -> u8 {
        // Instruction-cache RAM window $3100-$32FF (spec §1.9, §5).
        if (0x3100..=0x32FF).contains(&a) {
            let idx = (usize::from(a - 0x3100) + usize::from(self.regs.cbr)) & 511;
            return self.cache[idx];
        }
        // R0-R15 little-endian byte pairs $3000-$301F.
        if (0x3000..=0x301F).contains(&a) {
            let reg = (a >> 1) & 15;
            let shift = (a & 1) * 8;
            return (self.regs.r[reg as usize] >> shift) as u8;
        }
        match a {
            0x3030 => self.regs.sfr as u8,
            0x3031 => {
                let hi = (self.regs.sfr >> 8) as u8;
                // Reading SFR high byte acknowledges the IRQ (spec §7.3).
                self.sfr_set(SFR_IRQ, false);
                hi
            }
            0x3034 => self.regs.pbr,
            0x3036 => self.regs.rombr,
            0x303B => self.regs.vcr,
            0x303C => u8::from(self.regs.rambr),
            0x303E => self.regs.cbr as u8,
            0x303F => (self.regs.cbr >> 8) as u8,
            _ => 0x00,
        }
    }

    /// Write a GSU MMIO register at the normalised address `a`.
    fn mmio_write(&mut self, a: u16, data: u8) {
        // Instruction-cache RAM window $3100-$32FF: writing the 16th byte
        // of a line validates it (spec §5, lets the SNES preload code).
        if (0x3100..=0x32FF).contains(&a) {
            let idx = (usize::from(a - 0x3100) + usize::from(self.regs.cbr)) & 511;
            self.cache[idx] = data;
            if (a & 0x0F) == 0x0F {
                self.cache_valid[(idx >> 4) & 31] = true;
            }
            return;
        }
        // R0-R15 byte writes $3000-$301F.
        if (0x3000..=0x301F).contains(&a) {
            let reg = ((a >> 1) & 15) as usize;
            if a & 1 == 0 {
                self.regs.r[reg] = (self.regs.r[reg] & 0xFF00) | u16::from(data);
            } else {
                self.regs.r[reg] = (u16::from(data) << 8) | (self.regs.r[reg] & 0x00FF);
            }
            if reg == 14 {
                // Arm the R14 ROM-buffer fetch (spec §6.2). The countdown +
                // latch land with the engine phase; here we just raise the
                // pending flag so the SNES sees `sfr.r`.
                self.sfr_set(SFR_R, true);
            }
            // Writing R15 high byte ($301F) launches the GSU (spec §7.1).
            if a == 0x301F {
                self.sfr_set(SFR_G, true);
            }
            return;
        }
        match a {
            0x3030 => {
                let was_go = self.sfr_get(SFR_G);
                self.regs.sfr = (self.regs.sfr & 0xFF00) | u16::from(data);
                // g 1→0 transition: clear CBR + flush cache (spec §7.2).
                if was_go && !self.sfr_get(SFR_G) {
                    self.regs.cbr = 0x0000;
                    self.flush_cache();
                }
            }
            0x3031 => self.regs.sfr = (u16::from(data) << 8) | (self.regs.sfr & 0x00FF),
            0x3033 => self.regs.bramr = data & 0x01 != 0,
            0x3034 => {
                self.regs.pbr = data & 0x7F;
                self.flush_cache();
            }
            0x3037 => self.regs.cfgr = data,
            0x3038 => self.regs.scbr = data,
            0x3039 => self.regs.clsr = data & 0x01 != 0,
            0x303A => self.regs.set_scmr(data),
            _ => {}
        }
    }
}

/// Round `n` up to the next power of two (≥ 1).
const fn round_up_pow2(n: usize) -> usize {
    if n <= 1 {
        return 1;
    }
    let bits = usize::BITS - (n - 1).leading_zeros();
    1usize << bits
}

impl Mapper for SuperFxMapper {
    fn kind(&self) -> MapperKind {
        MapperKind::SuperFx
    }

    fn read(&mut self, addr: Addr24) -> Option<u8> {
        let bank = bank_of(addr);
        let offset = offset_of(addr);

        // GSU MMIO window.
        if let Some(a) = Self::mmio_addr(bank, offset) {
            return Some(self.mmio_read(a));
        }
        // ROM. While the GSU runs and owns ROM, the SNES sees the busy
        // vector instead of ROM data (spec §3.3).
        if let Some(o) = Self::rom_offset(bank, offset) {
            if self.sfr_get(SFR_G) && self.regs.scmr_ron {
                return Some(Self::busy_rom_vector(offset as u8));
            }
            return Some(self.rom[o & self.rom_mask]);
        }
        // Game Pak RAM. While the GSU owns RAM, SNES reads are open bus
        // (spec §3.3); we surface `None` so the bus returns its open-bus.
        if let Some(o) = Self::ram_offset(bank, offset) {
            if self.sfr_get(SFR_G) && self.regs.scmr_ran {
                return None;
            }
            return Some(self.ram[o & self.ram_mask]);
        }
        None
    }

    fn write(&mut self, addr: Addr24, value: u8) -> bool {
        let bank = bank_of(addr);
        let offset = offset_of(addr);

        if let Some(a) = Self::mmio_addr(bank, offset) {
            self.mmio_write(a, value);
            return true;
        }
        if let Some(o) = Self::ram_offset(bank, offset) {
            // ares lets the SNES RAM write land even while the GSU owns RAM
            // (spec §3.3 / divergence #4 — follow ares).
            self.ram[o & self.ram_mask] = value;
            return true;
        }
        // ROM writes are dropped but claimed so the bus doesn't fall through.
        Self::rom_offset(bank, offset).is_some()
    }

    fn rom_size(&self) -> usize {
        self.rom_len
    }

    fn sram_size(&self) -> usize {
        self.ram_len
    }

    /// Advance the GSU by `main_mclk` master cycles of main-CPU progress.
    ///
    /// The instruction engine lands in the next phase; for now this is a
    /// no-op. The GO flag, MMIO surface and IRQ handshake are all live, so
    /// a game can arm the GSU and the SNES side behaves correctly — the
    /// GSU simply executes no opcodes yet.
    fn step_coproc(&mut self, _main_mclk: u32) {}

    /// The GSU asserts the main-CPU IRQ line while `sfr.irq` is latched
    /// (spec §7.3). Set by the STOP opcode (next phase); acknowledged by a
    /// SNES read of SFR high byte ($3031).
    fn coproc_main_irq_pending(&self) -> bool {
        self.sfr_get(SFR_IRQ)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::make_addr;

    fn ramp_rom(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    fn fx() -> SuperFxMapper {
        SuperFxMapper::new(ramp_rom(1024 * 1024), 0x8000)
    }

    #[test]
    fn kind_is_superfx() {
        assert_eq!(fx().kind(), MapperKind::SuperFx);
    }

    #[test]
    fn round_up_pow2_works() {
        assert_eq!(round_up_pow2(0), 1);
        assert_eq!(round_up_pow2(1), 1);
        assert_eq!(round_up_pow2(3), 4);
        assert_eq!(round_up_pow2(1024 * 1024), 1024 * 1024);
        assert_eq!(round_up_pow2(1024 * 1024 + 1), 2 * 1024 * 1024);
    }

    #[test]
    fn reset_state_matches_spec() {
        let m = fx();
        let s = m.snapshot();
        assert_eq!(s.vcr, 0x04, "VCR resets to 0x04 (spec §1.8)");
        assert!(!s.running, "GSU starts halted");
        assert!(m.cache_valid.iter().all(|&v| !v), "cache starts invalid");
        assert_eq!(m.rom_size(), 1024 * 1024);
        assert_eq!(m.sram_size(), 0x8000);
    }

    #[test]
    fn lorom_window_reads_rom() {
        let mut m = fx();
        assert_eq!(m.read(make_addr(0x00, 0x8000)), Some(0));
        assert_eq!(m.read(make_addr(0x00, 0x8001)), Some(1));
        assert_eq!(m.read(make_addr(0x00, 0xFFFF)), Some(0xFF));
        // $80 mirrors $00.
        assert_eq!(m.read(make_addr(0x80, 0x8000)), Some(0));
    }

    #[test]
    fn linear_window_aliases_lorom() {
        let mut m = fx();
        assert_eq!(m.read(make_addr(0x40, 0x0000)), Some(0));
        assert_eq!(
            m.read(make_addr(0x40, 0x8000)),
            m.read(make_addr(0x01, 0x8000)),
            "linear and LoROM views alias the same ROM byte"
        );
    }

    #[test]
    fn ram_round_trips_via_both_windows() {
        let mut m = fx();
        assert!(m.write(make_addr(0x70, 0x0000), 0xAB));
        assert_eq!(m.read(make_addr(0x70, 0x0000)), Some(0xAB));
        assert_eq!(m.read(make_addr(0x00, 0x6000)), Some(0xAB));
        assert!(m.write(make_addr(0x00, 0x6001), 0xCD));
        assert_eq!(m.read(make_addr(0x70, 0x0001)), Some(0xCD));
    }

    #[test]
    fn register_file_byte_access() {
        let mut m = fx();
        assert!(m.write(make_addr(0x00, 0x3006), 0x34)); // R3 low
        assert!(m.write(make_addr(0x00, 0x3007), 0x12)); // R3 high
        assert_eq!(m.snapshot().r[3], 0x1234);
        assert_eq!(m.read(make_addr(0x00, 0x3006)), Some(0x34));
        assert_eq!(m.read(make_addr(0x00, 0x3007)), Some(0x12));
    }

    #[test]
    fn writing_r15_high_byte_starts_the_gsu() {
        let mut m = fx();
        assert!(!m.snapshot().running);
        m.write(make_addr(0x00, 0x301E), 0x00); // R15 low
        assert!(!m.snapshot().running, "R15 low alone does not GO");
        m.write(make_addr(0x00, 0x301F), 0x80); // R15 high → GO
        assert!(
            m.snapshot().running,
            "writing R15 high byte sets the GO flag"
        );
    }

    #[test]
    fn clearing_go_via_sfr_zeroes_cbr_and_flushes_cache() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x301F), 0x80); // GO
        m.regs.cbr = 0x1230;
        m.cache_valid[0] = true;
        m.write(make_addr(0x00, 0x3030), 0x00); // SFR low, g clear → 1→0
        assert!(!m.snapshot().running);
        assert_eq!(m.snapshot().cbr, 0, "g 1→0 zeroes CBR");
        assert!(m.cache_valid.iter().all(|&v| !v), "g 1→0 flushes cache");
    }

    #[test]
    fn irq_latch_and_acknowledge() {
        let mut m = fx();
        m.sfr_set(SFR_IRQ, true);
        assert!(m.coproc_main_irq_pending(), "IRQ asserted to main CPU");
        let _ = m.read(make_addr(0x00, 0x3031)); // SFR high read acknowledges
        assert!(!m.coproc_main_irq_pending(), "$3031 read clears the IRQ");
    }

    #[test]
    fn scmr_scrambled_bit_layout_round_trips() {
        let mut m = fx();
        // byte bit5=ht_hi, bit4=ron, bit3=ran, bit2=ht_lo, bits1:0=md.
        // ht=0b10, ron=1, ran=0, md=3 → 0b0011_0011 = 0x33
        m.write(make_addr(0x00, 0x303A), 0x33);
        assert_eq!(m.regs.scmr_ht, 0b10);
        assert!(m.regs.scmr_ron);
        assert!(!m.regs.scmr_ran);
        assert_eq!(m.regs.scmr_md, 3);
        assert_eq!(m.snapshot().scmr, 0x33, "round-trips back to the wire byte");
    }

    #[test]
    fn busy_vector_shown_while_gsu_owns_rom() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x303A), 0x10); // ron = 1
        m.write(make_addr(0x00, 0x301F), 0x80); // GO
        // Vector indexed by addr & 0x0F: $FFE4 → idx 4 → 0x04;
        // $FFEE → idx 14 → 0x0C; odd addresses → 0x01.
        assert_eq!(m.read(make_addr(0x00, 0xFFE4)), Some(0x04));
        assert_eq!(m.read(make_addr(0x00, 0xFFEE)), Some(0x0C));
        assert_eq!(m.read(make_addr(0x00, 0xFFEF)), Some(0x01));
    }

    #[test]
    fn cache_ram_write_validates_line_on_byte_15() {
        let mut m = fx();
        assert!(!m.cache_valid[0]);
        for i in 0..16u16 {
            m.write(make_addr(0x00, 0x3100 + i), i as u8);
        }
        assert!(m.cache_valid[0], "writing byte 15 of line 0 validates it");
        assert_eq!(m.read(make_addr(0x00, 0x3100)), Some(0));
        assert_eq!(m.read(make_addr(0x00, 0x310F)), Some(15));
    }

    #[test]
    fn config_registers_decode() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x3037), 0xA0); // CFGR
        assert_eq!(m.snapshot().cfgr, 0xA0);
        m.write(make_addr(0x00, 0x3039), 0x01); // CLSR fast
        assert!(m.snapshot().clsr);
        m.write(make_addr(0x00, 0x3038), 0x42); // SCBR
        assert_eq!(m.snapshot().scbr, 0x42);
        m.write(make_addr(0x00, 0x3033), 0x01); // BRAMR
        assert!(m.snapshot().bramr);
    }

    #[test]
    fn r14_write_raises_rom_pending() {
        let mut m = fx();
        m.write(make_addr(0x00, 0x301C), 0x00); // R14 low
        assert!(
            m.snapshot().sfr & SFR_R != 0,
            "R14 write arms ROM-read pending"
        );
    }
}
