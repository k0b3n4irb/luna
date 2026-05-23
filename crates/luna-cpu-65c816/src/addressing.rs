//! 65C816 addressing-mode helpers.
//!
//! Each helper fetches operand bytes from `PB:PC`, applies the relevant
//! transformation (direct page offset, bank wrap, index addition…) and
//! returns the **effective 24-bit address** to read from / write to.
//!
//! Cycle accounting is the bus's responsibility (via `io_cycle`); these
//! helpers only deal with address calculation.

use crate::cpu::Cpu;
use luna_bus::{Addr24, Bus, make_addr};

/// Absolute: `LDA $abs` — operand is a 16-bit address in the data bank.
#[inline]
pub fn absolute<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let offset = cpu.fetch_u16(bus);
    make_addr(cpu.db, offset)
}

/// Absolute Long: `LDA $long` — operand is a 24-bit address.
#[inline]
pub fn absolute_long<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    cpu.fetch_u24(bus)
}

/// Direct Page: `LDA $dp` — operand is a u8 offset added to `DP`.
#[inline]
pub fn direct_page<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let offset = u16::from(cpu.fetch_u8(bus));
    // Direct-page accesses live in bank 0 with the offset added to DP.
    make_addr(0, cpu.dp.wrapping_add(offset))
}

/// Absolute Indexed by X: `LDA $abs,X`.
#[inline]
pub fn absolute_indexed_x<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let base_off = cpu.fetch_u16(bus);
    let effective = base_off.wrapping_add(cpu.x);
    // Carry into the data bank.
    if effective < base_off {
        make_addr(cpu.db.wrapping_add(1), effective)
    } else {
        make_addr(cpu.db, effective)
    }
}

/// Absolute Indexed by Y: `LDA $abs,Y`.
#[inline]
pub fn absolute_indexed_y<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let base_off = cpu.fetch_u16(bus);
    let effective = base_off.wrapping_add(cpu.y);
    if effective < base_off {
        make_addr(cpu.db.wrapping_add(1), effective)
    } else {
        make_addr(cpu.db, effective)
    }
}

/// Read an 8- or 16-bit operand from a given effective address.
///
/// Width is governed by the M flag for accumulator-targeted reads or by
/// X for index-register-targeted reads. Callers pass the relevant flag
/// query as a closure to avoid having two helpers.
#[inline]
pub fn read_byte<B: Bus>(bus: &mut B, addr: Addr24) -> u8 {
    bus.read(addr)
}

/// Read 16 bits little-endian, with the bus advancing PB/DB-correct
/// offsets internally.
#[inline]
pub fn read_word<B: Bus>(bus: &mut B, addr: Addr24) -> u16 {
    let lo = bus.read(addr);
    // High byte at addr+1; the upper byte of `addr` increments within
    // the same bank only — the SNES 65C816 wraps within the bank for
    // 16-bit reads on absolute, but does NOT bank-cross on direct-page
    // accesses. For now we use plain +1 and revisit edge cases later.
    let hi = bus.read(addr.wrapping_add(1));
    u16::from(lo) | (u16::from(hi) << 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flags::bit;
    use luna_bus::testing::RamBus;

    fn setup(pc: u16) -> (Cpu, RamBus) {
        let mut cpu = Cpu::new();
        cpu.pc = pc;
        (cpu, RamBus::new())
    }

    #[test]
    fn absolute_reads_16_bit_in_data_bank() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.db = 0x7E;
        bus.poke_slice(0x00_8000, &[0x34, 0x12]);
        let addr = absolute(&mut cpu, &mut bus);
        assert_eq!(addr, 0x7E_1234);
        assert_eq!(cpu.pc, 0x8002);
    }

    #[test]
    fn absolute_long_reads_24_bit() {
        let (mut cpu, mut bus) = setup(0x8000);
        bus.poke_slice(0x00_8000, &[0x34, 0x12, 0x7E]);
        let addr = absolute_long(&mut cpu, &mut bus);
        assert_eq!(addr, 0x7E_1234);
        assert_eq!(cpu.pc, 0x8003);
    }

    #[test]
    fn direct_page_adds_dp() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.dp = 0x0100;
        bus.poke(0x00_8000, 0x10);
        let addr = direct_page(&mut cpu, &mut bus);
        assert_eq!(addr, 0x00_0110);
    }

    #[test]
    fn absolute_indexed_x_adds_x_no_carry() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.db = 0x7E;
        cpu.x = 0x0010;
        cpu.p.remove(bit::X); // 16-bit index
        bus.poke_slice(0x00_8000, &[0x00, 0x10]);
        let addr = absolute_indexed_x(&mut cpu, &mut bus);
        assert_eq!(addr, 0x7E_1010);
    }

    #[test]
    fn absolute_indexed_x_carries_into_next_bank() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.db = 0x7E;
        cpu.x = 0x0001;
        cpu.p.remove(bit::X);
        bus.poke_slice(0x00_8000, &[0xFF, 0xFF]); // base offset $FFFF
        let addr = absolute_indexed_x(&mut cpu, &mut bus);
        assert_eq!(addr, 0x7F_0000);
    }

    #[test]
    fn read_word_is_little_endian() {
        let mut bus = RamBus::new();
        bus.poke_slice(0x7E_1234, &[0xCD, 0xAB]);
        assert_eq!(read_word(&mut bus, 0x7E_1234), 0xABCD);
    }
}
