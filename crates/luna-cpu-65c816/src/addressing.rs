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

/// Access kind for indexed addressing modes. Only `abs,X`/`abs,Y` and
/// `(dp),Y` charge a different internal cycle for reads vs. stores/RMW:
/// a **read** pays the page-cross cycle only when the page actually
/// changed (or the index is 16-bit) — ares `idle4()` — whereas a
/// **write** always pays one fixed internal cycle. (The extra RMW dead
/// cycle is charged separately in `modify_memory`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Loads / comparisons: pay the page-cross cycle conditionally.
    Read,
    /// Stores and read-modify-write: pay one fixed index cycle.
    Write,
}

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
///
/// In emulation mode with `DP.low == 0`, the effective address wraps
/// within the current 256-byte page of the direct page (6502 behavior).
/// In all other cases the effective address wraps within the 16-bit
/// page-zero of bank 0.
#[inline]
pub fn direct_page<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let offset = u16::from(cpu.fetch_u8(bus));
    cpu.idle2(bus);
    cpu.bank0_wrap = true;
    make_addr(0, cpu.dp.wrapping_add(offset))
}

/// Direct Page Indexed X: `LDA $dp,X`.
///
/// Wraps with the same emulation-mode caveat as [`direct_page`].
#[inline]
pub fn direct_page_indexed_x<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let base = u16::from(cpu.fetch_u8(bus));
    cpu.idle2(bus);
    cpu.io(bus);
    cpu.bank0_wrap = true;
    let off = if cpu.e && (cpu.dp & 0xFF) == 0 {
        // Emulation: wrap within the 256-byte direct page.
        let dp_high = cpu.dp & 0xFF00;
        let wrapped = (base as u8).wrapping_add(cpu.x8());
        dp_high | u16::from(wrapped)
    } else {
        cpu.dp.wrapping_add(base).wrapping_add(cpu.x)
    };
    make_addr(0, off)
}

/// Direct Page Indexed Y: `STX $dp,Y` (the index-Y direct-page family).
#[inline]
pub fn direct_page_indexed_y<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let base = u16::from(cpu.fetch_u8(bus));
    cpu.idle2(bus);
    cpu.io(bus);
    cpu.bank0_wrap = true;
    let off = if cpu.e && (cpu.dp & 0xFF) == 0 {
        let dp_high = cpu.dp & 0xFF00;
        let wrapped = (base as u8).wrapping_add(cpu.y8());
        dp_high | u16::from(wrapped)
    } else {
        cpu.dp.wrapping_add(base).wrapping_add(cpu.y)
    };
    make_addr(0, off)
}

/// Read a 16-bit pointer at the given bank-0 offset.
///
/// Inlined helper used by the various direct-page-indirect modes.
#[inline]
fn read_ptr16<B: Bus>(bus: &mut B, ptr_off: u16) -> u16 {
    let lo = bus.read(make_addr(0, ptr_off));
    let hi = bus.read(make_addr(0, ptr_off.wrapping_add(1)));
    u16::from(lo) | (u16::from(hi) << 8)
}

/// Direct Page Indirect: `LDA ($dp)` — read a 16-bit pointer at DP+dp,
/// resolved against the data bank.
#[inline]
pub fn direct_page_indirect<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let dp_off = u16::from(cpu.fetch_u8(bus));
    cpu.idle2(bus);
    let ptr_off = cpu.dp.wrapping_add(dp_off);
    let offset = read_ptr16(bus, ptr_off);
    make_addr(cpu.db, offset)
}

/// Direct Page Indirect Long: `LDA [$dp]` — read a 24-bit pointer.
#[inline]
pub fn direct_page_indirect_long<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let dp_off = u16::from(cpu.fetch_u8(bus));
    cpu.idle2(bus);
    let ptr_off = cpu.dp.wrapping_add(dp_off);
    let lo = bus.read(make_addr(0, ptr_off));
    let mid = bus.read(make_addr(0, ptr_off.wrapping_add(1)));
    let hi = bus.read(make_addr(0, ptr_off.wrapping_add(2)));
    Addr24::from(lo) | (Addr24::from(mid) << 8) | (Addr24::from(hi) << 16)
}

/// Direct Page Indirect Y: `LDA ($dp),Y` — read pointer, add Y with
/// bank carry into the data bank.
#[inline]
pub fn direct_page_indirect_y<B: Bus>(cpu: &mut Cpu, bus: &mut B, acc: Access) -> Addr24 {
    let dp_off = u16::from(cpu.fetch_u8(bus));
    cpu.idle2(bus);
    let ptr_off = cpu.dp.wrapping_add(dp_off);
    let base_off = read_ptr16(bus, ptr_off);
    let new_off = base_off.wrapping_add(cpu.y);
    match acc {
        Access::Read => cpu.idle4(bus, base_off, new_off),
        Access::Write => cpu.io(bus),
    }
    let bank = if new_off < base_off {
        cpu.db.wrapping_add(1)
    } else {
        cpu.db
    };
    make_addr(bank, new_off)
}

/// Direct Page Indirect Long Y: `LDA [$dp],Y` — read 24-bit pointer,
/// add Y with full bank carry.
#[inline]
pub fn direct_page_indirect_long_y<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let base = direct_page_indirect_long(cpu, bus);
    base.wrapping_add(Addr24::from(cpu.y)) & 0x00FF_FFFF
}

/// Direct Page Indexed X Indirect: `LDA ($dp,X)`.
///
/// The X register is added BEFORE the indirect read (pointer fetch).
#[inline]
pub fn direct_page_indexed_indirect<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let dp_off = u32::from(cpu.fetch_u8(bus));
    cpu.idle2(bus);
    cpu.io(bus);
    // ares: V.l = readDirectX(U.l + X.w, 0); V.h = readDirectX(U.l + X.w, 1).
    let address = dp_off.wrapping_add(u32::from(cpu.x));
    let lo = read_direct_x(cpu, bus, address, 0);
    let hi = read_direct_x(cpu, bus, address, 1);
    let offset = u16::from(lo) | (u16::from(hi) << 8);
    make_addr(cpu.db, offset)
}

/// Fetch one byte of a `(dp,X)` pointer. `address` is `dp_operand + X`;
/// `offset` is 0 (low byte) or 1 (high byte).
///
/// Resolve one byte of a `(direct,X)` pointer.
///
/// In **emulation mode with `D.l == 0`** the index addition wraps within
/// the 256-byte direct page to form the pointer *base* (`D.w | n8(dp+X)`);
/// the two pointer bytes are then read **linearly** from `base` and
/// `base+1` (bank-0 wrap). Every other case (E with `D.l != 0`, or native)
/// is a plain 16-bit add within bank 0.
///
/// This diverges subtly from ares `readDirectX` (`memory.cpp:62-67`), which
/// re-applies the page-wrap to the *high* byte (`D.w | n8(dp+X+1)`). The
/// `SingleStepTests/65816` suite — hardware-derived, so the empirical gate —
/// disagrees only when `n8(dp+X) == 0xFF`:
/// - case `e1.e 125` (`D=0x7700`, `dp+X=0x197`) needs the base wrap: bytes
///   at `0x7797`/`0x7798`;
/// - case `e1.e 8669` (`D=0xF400`, `dp+X=0xFF`) needs the linear high byte:
///   `0xF4FF`/`0xF500`, not the page-wrapped `0xF400`.
///
/// Wrapping the base once and adding `offset` linearly satisfies both.
fn read_direct_x<B: Bus>(cpu: &Cpu, bus: &mut B, address: u32, offset: u32) -> u8 {
    let dp = u32::from(cpu.dp);
    let addr = if cpu.e && (cpu.dp & 0x00FF) == 0 {
        (dp | (address & 0x00FF)).wrapping_add(offset) & 0xFFFF
    } else {
        dp.wrapping_add(address).wrapping_add(offset) & 0xFFFF
    };
    bus.read(addr)
}

/// Absolute Indexed by X: `LDA $abs,X`.
#[inline]
pub fn absolute_indexed_x<B: Bus>(cpu: &mut Cpu, bus: &mut B, acc: Access) -> Addr24 {
    let base_off = cpu.fetch_u16(bus);
    let effective = base_off.wrapping_add(cpu.x);
    match acc {
        Access::Read => cpu.idle4(bus, base_off, effective),
        Access::Write => cpu.io(bus),
    }
    // Carry into the data bank.
    if effective < base_off {
        make_addr(cpu.db.wrapping_add(1), effective)
    } else {
        make_addr(cpu.db, effective)
    }
}

/// Absolute Indexed by Y: `LDA $abs,Y`.
#[inline]
pub fn absolute_indexed_y<B: Bus>(cpu: &mut Cpu, bus: &mut B, acc: Access) -> Addr24 {
    let base_off = cpu.fetch_u16(bus);
    let effective = base_off.wrapping_add(cpu.y);
    match acc {
        Access::Read => cpu.idle4(bus, base_off, effective),
        Access::Write => cpu.io(bus),
    }
    if effective < base_off {
        make_addr(cpu.db.wrapping_add(1), effective)
    } else {
        make_addr(cpu.db, effective)
    }
}

/// Absolute Long Indexed X: `LDA $long,X` — adds X to a 24-bit base
/// with full bank carry.
#[inline]
pub fn absolute_long_indexed_x<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let base = cpu.fetch_u24(bus);
    base.wrapping_add(Addr24::from(cpu.x)) & 0x00FF_FFFF
}

/// Stack Relative: `LDA $sr,S` — operand is a u8 offset added to SP.
#[inline]
pub fn stack_relative<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let off = u16::from(cpu.fetch_u8(bus));
    cpu.io(bus);
    cpu.bank0_wrap = true;
    make_addr(0, cpu.sp.wrapping_add(off))
}

/// Stack Relative Indirect Y: `LDA ($sr,S),Y`.
#[inline]
pub fn stack_relative_indirect_y<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> Addr24 {
    let off = u16::from(cpu.fetch_u8(bus));
    cpu.io(bus);
    let ptr_off = cpu.sp.wrapping_add(off);
    let base_off = read_ptr16(bus, ptr_off);
    cpu.io(bus);
    let new_off = base_off.wrapping_add(cpu.y);
    let bank = if new_off < base_off {
        cpu.db.wrapping_add(1)
    } else {
        cpu.db
    };
    make_addr(bank, new_off)
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

/// Read 16 bits little-endian with the high byte carrying into the next
/// bank (ares `readBank`: `addr + 1`). This is the bank/long-mode rule.
///
/// Direct-page and stack-relative accesses instead wrap the high byte
/// within bank 0; the opcode handlers use [`crate::cpu::Cpu::read_word16`],
/// which honors the per-instruction `bank0_wrap` latch. Kept here as the
/// pure bank-carrying primitive for tests.
#[inline]
pub fn read_word<B: Bus>(bus: &mut B, addr: Addr24) -> u16 {
    let lo = bus.read(addr);
    let hi = bus.read(addr.wrapping_add(1) & 0x00FF_FFFF);
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
        let addr = absolute_indexed_x(&mut cpu, &mut bus, Access::Read);
        assert_eq!(addr, 0x7E_1010);
    }

    #[test]
    fn absolute_indexed_x_carries_into_next_bank() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.db = 0x7E;
        cpu.x = 0x0001;
        cpu.p.remove(bit::X);
        bus.poke_slice(0x00_8000, &[0xFF, 0xFF]); // base offset $FFFF
        let addr = absolute_indexed_x(&mut cpu, &mut bus, Access::Read);
        assert_eq!(addr, 0x7F_0000);
    }

    #[test]
    fn read_word_is_little_endian() {
        let mut bus = RamBus::new();
        bus.poke_slice(0x7E_1234, &[0xCD, 0xAB]);
        assert_eq!(read_word(&mut bus, 0x7E_1234), 0xABCD);
    }

    // -----------------------------------------------------------------
    // Direct-page indexed
    // -----------------------------------------------------------------

    #[test]
    fn direct_page_indexed_x_adds_x_in_native_mode() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.e = false;
        cpu.p.remove(bit::X);
        cpu.dp = 0x1000;
        cpu.x = 0x0020;
        bus.poke(0x00_8000, 0x10); // operand
        // dp + 0x10 + 0x0020 = 0x1030
        assert_eq!(direct_page_indexed_x(&mut cpu, &mut bus), 0x00_1030);
    }

    #[test]
    fn direct_page_indexed_x_wraps_in_emulation_with_dp_aligned() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.e = true;
        cpu.p.insert(bit::X);
        cpu.dp = 0x0000; // low byte == 0 triggers DP-page wrap
        cpu.x = 0x00FF;
        bus.poke(0x00_8000, 0x02);
        // 0x02 + 0xFF wraps within DP page → 0x0001.
        assert_eq!(direct_page_indexed_x(&mut cpu, &mut bus), 0x00_0001);
    }

    #[test]
    fn direct_page_indexed_y_does_the_same_with_y() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.e = false;
        cpu.p.remove(bit::X);
        cpu.dp = 0x1000;
        cpu.y = 0x0007;
        bus.poke(0x00_8000, 0x10);
        assert_eq!(direct_page_indexed_y(&mut cpu, &mut bus), 0x00_1017);
    }

    // -----------------------------------------------------------------
    // Direct-page indirect
    // -----------------------------------------------------------------

    #[test]
    fn direct_page_indirect_resolves_via_dp_pointer() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        bus.poke(0x00_8000, 0x10); // dp offset
        // Pointer at DP+0x10 = 0x110 stores $1234 LE
        bus.poke_slice(0x00_0110, &[0x34, 0x12]);
        assert_eq!(direct_page_indirect(&mut cpu, &mut bus), 0x7E_1234);
    }

    #[test]
    fn direct_page_indirect_long_returns_24bit_pointer() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.dp = 0x0100;
        bus.poke(0x00_8000, 0x10);
        // 24-bit pointer at 0x110: $123456
        bus.poke_slice(0x00_0110, &[0x56, 0x34, 0x12]);
        assert_eq!(direct_page_indirect_long(&mut cpu, &mut bus), 0x12_3456);
    }

    #[test]
    fn direct_page_indirect_y_adds_y_to_offset() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        cpu.p.remove(bit::X);
        cpu.y = 0x0010;
        bus.poke(0x00_8000, 0x10);
        bus.poke_slice(0x00_0110, &[0x00, 0x12]); // pointer = $1200
        // $1200 + $0010 = $1210 in bank $7E.
        assert_eq!(
            direct_page_indirect_y(&mut cpu, &mut bus, Access::Read),
            0x7E_1210
        );
    }

    #[test]
    fn direct_page_indirect_y_carries_into_next_bank() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        cpu.p.remove(bit::X);
        cpu.y = 0x0001;
        bus.poke(0x00_8000, 0x10);
        bus.poke_slice(0x00_0110, &[0xFF, 0xFF]); // pointer = $FFFF
        assert_eq!(
            direct_page_indirect_y(&mut cpu, &mut bus, Access::Read),
            0x7F_0000
        );
    }

    #[test]
    fn direct_page_indirect_long_y_handles_full_24bit_addition() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.dp = 0x0100;
        cpu.p.remove(bit::X);
        cpu.y = 0x0010;
        bus.poke(0x00_8000, 0x10);
        bus.poke_slice(0x00_0110, &[0x00, 0x12, 0x7E]); // pointer = $7E:1200
        assert_eq!(direct_page_indirect_long_y(&mut cpu, &mut bus), 0x7E_1210);
    }

    #[test]
    fn direct_page_indexed_indirect_applies_x_before_pointer_fetch() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.dp = 0x0100;
        cpu.db = 0x7E;
        cpu.p.remove(bit::X);
        cpu.x = 0x0004;
        bus.poke(0x00_8000, 0x10);
        // DP + dp + X = 0x100 + 0x10 + 0x04 = 0x114.
        bus.poke_slice(0x00_0114, &[0x78, 0x56]); // pointer = $5678
        assert_eq!(direct_page_indexed_indirect(&mut cpu, &mut bus), 0x7E_5678);
    }

    // -----------------------------------------------------------------
    // Absolute long indexed
    // -----------------------------------------------------------------

    #[test]
    fn absolute_long_indexed_x_carries_across_banks() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.p.remove(bit::X);
        cpu.x = 0x0001;
        bus.poke_slice(0x00_8000, &[0xFF, 0xFF, 0x7E]); // base = $7E:FFFF
        assert_eq!(absolute_long_indexed_x(&mut cpu, &mut bus), 0x7F_0000);
    }

    // -----------------------------------------------------------------
    // Stack relative
    // -----------------------------------------------------------------

    #[test]
    fn stack_relative_uses_sp_plus_offset() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.sp = 0x01F0;
        bus.poke(0x00_8000, 0x04);
        assert_eq!(stack_relative(&mut cpu, &mut bus), 0x00_01F4);
    }

    #[test]
    fn stack_relative_indirect_y_resolves_then_adds_y() {
        let (mut cpu, mut bus) = setup(0x8000);
        cpu.sp = 0x01F0;
        cpu.db = 0x7E;
        cpu.p.remove(bit::X);
        cpu.y = 0x0010;
        bus.poke(0x00_8000, 0x04);
        bus.poke_slice(0x00_01F4, &[0x00, 0x12]); // pointer = $1200
        assert_eq!(stack_relative_indirect_y(&mut cpu, &mut bus), 0x7E_1210);
    }
}
