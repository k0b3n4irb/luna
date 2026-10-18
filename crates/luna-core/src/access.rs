//! The S-CPU side's per-byte access counters (Mesen2's access counters):
//! the memories the 65C816 and the DMA reach, counted at the byte an
//! access lands on, so every mirror of a byte shares its counts.
//!
//! What counts (Mesen2, measured on its build): the CPU's code reads
//! (opcode and operands, as exec) and data reads / writes; both buses of a
//! DMA or HDMA (the A-bus byte, and the `$21xx` register on the B-bus);
//! VRAM, OAM and CGRAM at the byte their ports touch, never the PPU's own
//! fetches while it draws. The SPC700 keeps its own (`luna_apu`).

use luna_bus::{Access, AccessCounters, Addr24, CartMemory, Mapper};
use luna_ppu::{PpuAccess, PpuMemory};

/// A memory the S-CPU side counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CountedMemory {
    /// The 128 KB of work RAM.
    WorkRam,
    /// The I/O registers, by the low 16 bits of their address (`$2100`,
    /// `$4300`…).
    Register,
    /// Video RAM, byte-addressed.
    VideoRam,
    /// Sprite attribute memory (544 bytes).
    SpriteRam,
    /// Palette RAM (512 bytes).
    CgRam,
    /// A memory of the cartridge.
    Cart(CartMemory),
}

/// Whether `bank` holds the system area (`$0000-$5FFF`: low work RAM and
/// the I/O registers).
const fn system_bank(bank: u32) -> bool {
    bank <= 0x3F || (bank >= 0x80 && bank <= 0xBF)
}

/// The byte a CPU-bus address reaches: work RAM, a register, or what the
/// cartridge maps there. `None` for an address nothing answers.
#[must_use]
pub fn locate(mapper: &dyn Mapper, addr: Addr24) -> Option<(CountedMemory, usize)> {
    let addr = addr & 0x00FF_FFFF;
    let (bank, off) = (addr >> 16, addr & 0xFFFF);
    if matches!(bank, 0x7E..=0x7F) {
        return Some((CountedMemory::WorkRam, (addr - 0x7E_0000) as usize));
    }
    if system_bank(bank) && off < 0x2000 {
        return Some((CountedMemory::WorkRam, off as usize));
    }
    if system_bank(bank) && matches!(off, 0x2000..=0x2FFF | 0x4000..=0x4FFF) {
        return Some((CountedMemory::Register, off as usize));
    }
    mapper
        .debug_resolve(addr)
        .map(|(c, o)| (CountedMemory::Cart(c), o))
}

/// Every cartridge memory, in [`CpuAccessCounters`]' order.
const CART: [CartMemory; 7] = [
    CartMemory::Rom,
    CartMemory::SaveRam,
    CartMemory::Sa1Iram,
    CartMemory::GsuRam,
    CartMemory::DspProgram,
    CartMemory::DspDataRom,
    CartMemory::DspDataRam,
];

/// The counters of every memory the S-CPU side reaches.
#[derive(Debug, Clone)]
pub struct CpuAccessCounters {
    work_ram: AccessCounters,
    register: AccessCounters,
    vram: AccessCounters,
    oam: AccessCounters,
    cgram: AccessCounters,
    cart: [AccessCounters; 7],
}

impl CpuAccessCounters {
    /// Counters at zero for the memories this cartridge has.
    #[must_use]
    pub fn new(mapper: &dyn Mapper) -> Self {
        Self {
            work_ram: AccessCounters::new(0x2_0000),
            register: AccessCounters::new(0x1_0000),
            vram: AccessCounters::new(0x1_0000),
            oam: AccessCounters::new(0x220),
            cgram: AccessCounters::new(0x200),
            cart: CART.map(|c| AccessCounters::new(mapper.debug_size(c))),
        }
    }

    /// One memory's counters.
    #[must_use]
    pub const fn memory(&self, m: CountedMemory) -> &AccessCounters {
        match m {
            CountedMemory::WorkRam => &self.work_ram,
            CountedMemory::Register => &self.register,
            CountedMemory::VideoRam => &self.vram,
            CountedMemory::SpriteRam => &self.oam,
            CountedMemory::CgRam => &self.cgram,
            CountedMemory::Cart(c) => &self.cart[cart_index(c)],
        }
    }

    const fn memory_mut(&mut self, m: CountedMemory) -> &mut AccessCounters {
        match m {
            CountedMemory::WorkRam => &mut self.work_ram,
            CountedMemory::Register => &mut self.register,
            CountedMemory::VideoRam => &mut self.vram,
            CountedMemory::SpriteRam => &mut self.oam,
            CountedMemory::CgRam => &mut self.cgram,
            CountedMemory::Cart(c) => &mut self.cart[cart_index(c)],
        }
    }

    /// Count an access to the CPU-bus address `addr` at master clock
    /// `clock`, on the byte it reaches.
    #[inline]
    pub fn note_bus(&mut self, mapper: &dyn Mapper, addr: Addr24, access: Access, clock: u64) {
        if let Some((m, o)) = locate(mapper, addr) {
            self.memory_mut(m).note(o, access, clock);
        }
    }

    /// Count the PPU-memory bytes a port read or wrote.
    pub fn note_ppu(&mut self, accesses: &[PpuAccess], clock: u64) {
        for a in accesses.iter().filter(|a| !a.fill) {
            let m = match a.memory {
                PpuMemory::Vram => CountedMemory::VideoRam,
                PpuMemory::Oam => CountedMemory::SpriteRam,
                PpuMemory::Cgram => CountedMemory::CgRam,
            };
            let access = if a.write { Access::Write } else { Access::Read };
            self.memory_mut(m).note(usize::from(a.addr), access, clock);
        }
    }

    /// Every counter back to zero.
    pub fn clear(&mut self) {
        for c in [
            &mut self.work_ram,
            &mut self.register,
            &mut self.vram,
            &mut self.oam,
            &mut self.cgram,
        ] {
            c.clear();
        }
        for c in &mut self.cart {
            c.clear();
        }
    }
}

const fn cart_index(c: CartMemory) -> usize {
    match c {
        CartMemory::Rom => 0,
        CartMemory::SaveRam => 1,
        CartMemory::Sa1Iram => 2,
        CartMemory::GsuRam => 3,
        CartMemory::DspProgram => 4,
        CartMemory::DspDataRom => 5,
        CartMemory::DspDataRam => 6,
    }
}
