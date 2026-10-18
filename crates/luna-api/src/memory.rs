//! Every memory a debugger addresses, under one name each: Mesen2's memory
//! types (`MemoryType`), the vocabulary its tools, its Lua API and luna's
//! own debugger share.
//!
//! Reads never have a side effect. A bus address goes to the memory behind
//! it (work RAM, or the cartridge memory the mapper's banking picks) and is
//! never read through the bus, so no decompression starts, no port is
//! drained, no latch flips. A register on the bus reads 0, an address
//! nothing answers `$FF`.

use luna_core::CartMemory;
use luna_cpu_spc700::IPL_ROM;
use serde::{Deserialize, Serialize};

use crate::{ApiError, Emulator};

/// One addressable memory, named as Mesen2 names it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryKind {
    /// The 65C816's 24-bit bus.
    SnesMemory,
    /// The SPC700's 16-bit bus: audio RAM, with the IPL ROM over
    /// `$FFC0-$FFFF` while `$F1` bit 7 is set.
    SpcMemory,
    /// The cartridge's program ROM.
    SnesPrgRom,
    /// The 128 KB of work RAM (`$7E:0000-$7F:FFFF`).
    SnesWorkRam,
    /// Cartridge RAM on the CPU bus: SRAM, or the SA-1's BW-RAM.
    SnesSaveRam,
    /// The 64 KB of video RAM, byte-addressed.
    SnesVideoRam,
    /// OAM: the 512-byte low table, then the 32-byte high table.
    SnesSpriteRam,
    /// CGRAM: 256 little-endian BGR555 words.
    SnesCgRam,
    /// The main CPU's I/O registers, `$2000-$2FFF` and `$4000-$4FFF` of
    /// the system banks, by their 16-bit address. Read without side
    /// effects: the DMA registers read back, any other register reads 0.
    /// Never written.
    SnesRegister,
    /// The SPC700's 64 KB of audio RAM, without the IPL ROM.
    SpcRam,
    /// The 64-byte IPL ROM.
    SpcRom,
    /// The S-DSP's 128 registers.
    SpcDspRegisters,
    /// DSP-1 program ROM, 3 bytes per 24-bit word.
    DspProgramRom,
    /// DSP-1 data ROM, 2 bytes per word.
    DspDataRom,
    /// DSP-1 data RAM, 2 bytes per word.
    DspDataRam,
    /// The SA-1's 2 KB internal RAM.
    Sa1InternalRam,
    /// The Super FX Game Pak RAM.
    GsuWorkRam,
    /// The SA-1's 24-bit bus: its I-RAM at `$0000-$07FF` and
    /// `$3000-$37FF`, BW-RAM, ROM, its vectors. Read only (its registers
    /// read 0); write the memories behind it.
    Sa1Memory,
    /// The Super FX GSU's 24-bit bus: ROM at `$00-$5F`, Game Pak RAM at
    /// `$70-$71`. Read only; write the memories behind it.
    GsuMemory,
}

impl MemoryKind {
    /// Every kind, in Mesen2's order.
    pub const ALL: [Self; 19] = [
        Self::SnesMemory,
        Self::SpcMemory,
        Self::SnesPrgRom,
        Self::SnesWorkRam,
        Self::SnesSaveRam,
        Self::SnesVideoRam,
        Self::SnesSpriteRam,
        Self::SnesCgRam,
        Self::SnesRegister,
        Self::SpcRam,
        Self::SpcRom,
        Self::SpcDspRegisters,
        Self::DspProgramRom,
        Self::DspDataRom,
        Self::DspDataRam,
        Self::Sa1InternalRam,
        Self::GsuWorkRam,
        Self::Sa1Memory,
        Self::GsuMemory,
    ];

    /// Mesen2's name, e.g. `SnesWorkRam`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::SnesMemory => "SnesMemory",
            Self::SpcMemory => "SpcMemory",
            Self::SnesPrgRom => "SnesPrgRom",
            Self::SnesWorkRam => "SnesWorkRam",
            Self::SnesSaveRam => "SnesSaveRam",
            Self::SnesVideoRam => "SnesVideoRam",
            Self::SnesSpriteRam => "SnesSpriteRam",
            Self::SnesCgRam => "SnesCgRam",
            Self::SnesRegister => "SnesRegister",
            Self::SpcRam => "SpcRam",
            Self::SpcRom => "SpcRom",
            Self::SpcDspRegisters => "SpcDspRegisters",
            Self::DspProgramRom => "DspProgramRom",
            Self::DspDataRom => "DspDataRom",
            Self::DspDataRam => "DspDataRam",
            Self::Sa1InternalRam => "Sa1InternalRam",
            Self::GsuWorkRam => "GsuWorkRam",
            Self::Sa1Memory => "Sa1Memory",
            Self::GsuMemory => "GsuMemory",
        }
    }

    /// The kind named `name`, ignoring case: Mesen2's `SnesWorkRam` and
    /// its Lua spelling `snesWorkRam` both work.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.name().eq_ignore_ascii_case(name))
    }

    /// The cartridge memory behind this kind, if it is one.
    const fn cart(self) -> Option<CartMemory> {
        match self {
            Self::SnesPrgRom => Some(CartMemory::Rom),
            Self::SnesSaveRam => Some(CartMemory::SaveRam),
            Self::Sa1InternalRam => Some(CartMemory::Sa1Iram),
            Self::GsuWorkRam => Some(CartMemory::GsuRam),
            Self::DspProgramRom => Some(CartMemory::DspProgram),
            Self::DspDataRom => Some(CartMemory::DspDataRom),
            Self::DspDataRam => Some(CartMemory::DspDataRam),
            _ => None,
        }
    }

    const fn from_cart(c: CartMemory) -> Self {
        match c {
            CartMemory::Rom => Self::SnesPrgRom,
            CartMemory::SaveRam => Self::SnesSaveRam,
            CartMemory::Sa1Iram => Self::Sa1InternalRam,
            CartMemory::GsuRam => Self::GsuWorkRam,
            CartMemory::DspProgram => Self::DspProgramRom,
            CartMemory::DspDataRom => Self::DspDataRom,
            CartMemory::DspDataRam => Self::DspDataRam,
        }
    }
}

/// A byte of a memory: which memory, and its offset in it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryLocation {
    /// The memory.
    pub kind: MemoryKind,
    /// The byte's offset in it.
    pub offset: u32,
}

/// Whether `bank` is one of the system banks, where work RAM's first 8 KB
/// and the registers appear.
const fn system_bank(bank: u32) -> bool {
    bank <= 0x3F || (bank >= 0x80 && bank <= 0xBF)
}

/// A read-only view of a machine, for the code that only holds the core
/// (the run loop's stop checks).
#[derive(Clone, Copy)]
pub(crate) struct View<'a>(pub(crate) &'a luna_core::Snes);

impl View<'_> {
    /// Size of `kind` in bytes, 0 if this cartridge has none.
    pub(crate) fn size(self, kind: MemoryKind) -> usize {
        match kind {
            MemoryKind::SnesMemory => 0x100_0000,
            MemoryKind::SpcMemory
            | MemoryKind::SpcRam
            | MemoryKind::SnesVideoRam
            | MemoryKind::SnesRegister => 0x1_0000,
            MemoryKind::SnesWorkRam => 0x2_0000,
            MemoryKind::SnesSpriteRam => 0x220,
            MemoryKind::SnesCgRam => 0x200,
            MemoryKind::SpcRom => IPL_ROM.len(),
            MemoryKind::SpcDspRegisters => 128,
            MemoryKind::Sa1Memory | MemoryKind::GsuMemory => {
                let mut here = false;
                self.0.mapper.with_coproc_view(&mut |v| {
                    here = matches!(
                        (kind, v.regs()),
                        (MemoryKind::Sa1Memory, luna_core::CoprocRegs::Sa1 { .. })
                            | (MemoryKind::GsuMemory, luna_core::CoprocRegs::Gsu { .. })
                    );
                });
                if here { 0x100_0000 } else { 0 }
            }
            _ => kind.cart().map_or(0, |c| self.0.mapper.debug_size(c)),
        }
    }

    /// The memory and offset behind CPU-bus address `addr`.
    pub(crate) fn absolute(self, addr: u32) -> Option<MemoryLocation> {
        let addr = addr & 0x00FF_FFFF;
        let (bank, off) = (addr >> 16, addr & 0xFFFF);
        let wram = |offset| {
            Some(MemoryLocation {
                kind: MemoryKind::SnesWorkRam,
                offset,
            })
        };
        if matches!(bank, 0x7E..=0x7F) {
            return wram(addr - 0x7E_0000);
        }
        if system_bank(bank) && off < 0x2000 {
            return wram(off);
        }
        if system_bank(bank) && matches!(off, 0x2000..=0x2FFF | 0x4000..=0x4FFF) {
            return Some(MemoryLocation {
                kind: MemoryKind::SnesRegister,
                offset: off,
            });
        }
        self.0
            .mapper
            .debug_resolve(addr)
            .map(|(c, offset)| MemoryLocation {
                kind: MemoryKind::from_cart(c),
                offset: offset as u32,
            })
    }

    /// Byte `a` of `kind`, `a` already inside it; `unmapped` for a CPU-bus
    /// address nothing answers.
    pub(crate) fn byte(self, kind: MemoryKind, a: usize, unmapped: u8) -> u8 {
        let snes = self.0;
        match kind {
            MemoryKind::SnesMemory => self.bus_peek(a as u32, unmapped),
            MemoryKind::SpcMemory => snes.apu_real.peek(a as u16),
            MemoryKind::SnesWorkRam => snes.wram[a],
            MemoryKind::SnesVideoRam => snes.ppu.vram.peek(a as u16),
            MemoryKind::SnesSpriteRam => snes.ppu.oam.peek(a as u16),
            MemoryKind::SnesCgRam => snes.ppu.cgram.peek(a as u16),
            MemoryKind::SpcRam => snes.apu_real.aram[a],
            MemoryKind::SpcRom => IPL_ROM[a],
            MemoryKind::SpcDspRegisters => snes.apu_real.dsp.registers[a],
            MemoryKind::SnesRegister if (0x4300..=0x437F).contains(&a) => {
                // The DMA registers read back without a side effect.
                snes.dma.channels[(a >> 4) & 7].read((a & 0xF) as u8)
            }
            MemoryKind::SnesRegister => 0,
            MemoryKind::Sa1Memory | MemoryKind::GsuMemory => {
                let mut byte = unmapped;
                snes.mapper
                    .with_coproc_view(&mut |v| byte = v.peek(a as u32));
                byte
            }
            _ => kind
                .cart()
                .and_then(|c| snes.mapper.debug_peek(c, a))
                .unwrap_or(0xFF),
        }
    }

    /// One CPU-bus byte as a debugger sees it: the memory behind it, a DMA
    /// register, 0 for any other register, `unmapped` for open bus.
    pub(crate) fn bus_peek(self, addr: u32, unmapped: u8) -> u8 {
        if let Some(MemoryLocation { kind, offset }) = self.absolute(addr) {
            return self.byte(kind, offset as usize, unmapped);
        }
        let (bank, off) = (addr >> 16, addr & 0xFFFF);
        if system_bank(bank) && (0x2000..=0x5FFF).contains(&off) {
            return 0;
        }
        unmapped
    }
}

impl Emulator {
    pub(crate) fn view(&self) -> Result<View<'_>, ApiError> {
        self.snes.as_ref().map(View).ok_or(ApiError::NoRom)
    }

    /// The memories this cartridge has, with their size in bytes (the two
    /// buses report their address space).
    pub fn memory_kinds(&self) -> Result<Vec<(MemoryKind, usize)>, ApiError> {
        let view = self.view()?;
        Ok(MemoryKind::ALL
            .into_iter()
            .map(|k| (k, view.size(k)))
            .filter(|&(_, size)| size > 0)
            .collect())
    }

    /// Size of `kind` in bytes, 0 if this cartridge has none.
    pub fn memory_size(&self, kind: MemoryKind) -> Result<usize, ApiError> {
        Ok(self.view()?.size(kind))
    }

    /// The memory and offset behind CPU-bus address `addr` (24-bit): work
    /// RAM, an I/O register (`SnesRegister`), or the cartridge memory the
    /// mapper's banking selects now. `None` for an address nothing answers
    /// (a coprocessor's register window included).
    pub fn absolute_address(&self, addr: u32) -> Result<Option<MemoryLocation>, ApiError> {
        Ok(self.view()?.absolute(addr))
    }

    /// `len` bytes of `kind` from `offset`, without side effects. The two
    /// buses wrap around their address space; any other memory refuses a
    /// range past its end. An address nothing answers reads `$FF`.
    pub fn read_memory(
        &self,
        kind: MemoryKind,
        offset: u32,
        len: usize,
    ) -> Result<Vec<u8>, ApiError> {
        let view = self.view()?;
        let size = view.size(kind);
        Self::check_range(kind, size, offset, len)?;
        Ok((0..len)
            .map(|i| view.byte(kind, (offset as usize + i) % size, 0xFF))
            .collect())
    }

    /// Write `data` into `kind` from `offset`, without side effects (ROM
    /// included; a register on the bus is skipped, as is the IPL ROM).
    /// Returns the bytes written.
    pub fn write_memory(
        &mut self,
        kind: MemoryKind,
        offset: u32,
        data: &[u8],
    ) -> Result<usize, ApiError> {
        let size = self.memory_size(kind)?;
        Self::check_range(kind, size, offset, data.len())?;
        let mut written = 0;
        for (i, &v) in data.iter().enumerate() {
            let a = (offset as usize + i) % size;
            let target = match kind {
                MemoryKind::SnesMemory => self.absolute_address(a as u32)?,
                MemoryKind::SpcMemory => Some(MemoryLocation {
                    kind: MemoryKind::SpcRam,
                    offset: a as u32,
                }),
                _ => Some(MemoryLocation {
                    kind,
                    offset: a as u32,
                }),
            };
            let Some(MemoryLocation { kind, offset }) = target else {
                continue;
            };
            let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
            let o = offset as usize;
            let done = match kind {
                MemoryKind::SnesWorkRam => {
                    snes.wram[o] = v;
                    true
                }
                MemoryKind::SnesVideoRam => {
                    snes.ppu.vram.poke(o as u16, v);
                    true
                }
                MemoryKind::SnesSpriteRam => {
                    snes.ppu.oam.poke(o as u16, v);
                    true
                }
                MemoryKind::SnesCgRam => {
                    snes.ppu.cgram.poke(o as u16, v);
                    true
                }
                MemoryKind::SpcRam => {
                    snes.apu_real.aram[o] = v;
                    true
                }
                MemoryKind::SpcDspRegisters => {
                    snes.apu_real.dsp.registers[o] = v;
                    true
                }
                MemoryKind::SnesMemory
                | MemoryKind::SpcMemory
                | MemoryKind::SnesRegister
                | MemoryKind::SpcRom
                | MemoryKind::Sa1Memory
                | MemoryKind::GsuMemory => false,
                _ => kind.cart().is_some_and(|c| snes.mapper.debug_poke(c, o, v)),
            };
            written += usize::from(done);
        }
        Ok(written)
    }

    /// Refuse a range past the end of a memory that is not a bus.
    fn check_range(kind: MemoryKind, size: usize, offset: u32, len: usize) -> Result<(), ApiError> {
        let bus = matches!(
            kind,
            MemoryKind::SnesMemory
                | MemoryKind::SpcMemory
                | MemoryKind::Sa1Memory
                | MemoryKind::GsuMemory
        );
        if size == 0 {
            return Err(ApiError::BadArg(format!(
                "{}: this cartridge has none",
                kind.name()
            )));
        }
        if !bus && offset as usize + len > size {
            return Err(ApiError::BadArg(format!(
                "{}: ${offset:X} + {len} runs past its {size} bytes",
                kind.name()
            )));
        }
        Ok(())
    }
}
