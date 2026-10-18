//! Per-byte access counters (Mesen2's access counters, behind its Memory
//! Viewer's highlighting and its Memory Search columns): how many times
//! each byte was read, written and executed, and the master clock of the
//! last time.
//!
//! Counted on the byte an access lands on, never on its address: a bus
//! view ([`MemoryKind::SnesMemory`], [`MemoryKind::SpcMemory`]) shows, at
//! each address, the counters of the byte the bus reaches there now, so
//! every mirror shows the same counts. What counts: the 65C816's code
//! reads (opcode and operands, as exec) and data accesses; both buses of a
//! DMA or HDMA; VRAM, OAM and CGRAM at the byte their ports touch (never
//! the PPU's own fetches while it draws); the SPC700's accesses (ARAM, the
//! IPL ROM while it is mapped), not the S-DSP's. The coprocessors' own
//! accesses are not counted yet.
//!
//! The counters start at zero when turned on, and go back to zero on a
//! reset and when a state is loaded, as in Mesen2.

use luna_core::CountedMemory;
use schemars::JsonSchema;
use serde::Serialize;

use crate::memory::MemoryKind;
use crate::{ApiError, Emulator};

/// One byte's counters. A stamp of 0 means never.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AccessCounter {
    /// Data reads.
    pub read: u32,
    /// Writes.
    pub write: u32,
    /// Code reads (opcode or operand).
    pub exec: u32,
    /// Master clock of the last data read.
    pub last_read: u64,
    /// Master clock of the last write.
    pub last_write: u64,
    /// Master clock of the last code read.
    pub last_exec: u64,
}

impl From<luna_core::ByteCounters> for AccessCounter {
    fn from(b: luna_core::ByteCounters) -> Self {
        Self {
            read: b.read,
            write: b.write,
            exec: b.exec,
            last_read: b.last_read,
            last_write: b.last_write,
            last_exec: b.last_exec,
        }
    }
}

/// The memory kind a counted memory is.
pub(crate) const fn kind_of(m: CountedMemory) -> MemoryKind {
    match m {
        CountedMemory::WorkRam => MemoryKind::SnesWorkRam,
        CountedMemory::Register => MemoryKind::SnesRegister,
        CountedMemory::VideoRam => MemoryKind::SnesVideoRam,
        CountedMemory::SpriteRam => MemoryKind::SnesSpriteRam,
        CountedMemory::CgRam => MemoryKind::SnesCgRam,
        CountedMemory::Cart(c) => MemoryKind::from_cart(c),
    }
}

/// The counted memory a kind is, for the S-CPU side's memories.
fn counted(kind: MemoryKind) -> Option<CountedMemory> {
    Some(match kind {
        MemoryKind::SnesWorkRam => CountedMemory::WorkRam,
        MemoryKind::SnesRegister => CountedMemory::Register,
        MemoryKind::SnesVideoRam => CountedMemory::VideoRam,
        MemoryKind::SnesSpriteRam => CountedMemory::SpriteRam,
        MemoryKind::SnesCgRam => CountedMemory::CgRam,
        _ => CountedMemory::Cart(kind.cart()?),
    })
}

impl Emulator {
    /// Count every access to each byte (`true`, from zero), or stop and
    /// drop the counters (`false`). Off by default: counting costs run
    /// time and about 40 bytes per byte of every memory (ROM included).
    pub fn enable_access_counters(&mut self, on: bool) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_access_counters(on);
        Ok(())
    }

    /// Whether the access counters are on.
    #[must_use]
    pub fn access_counters_enabled(&self) -> bool {
        self.snes
            .as_ref()
            .is_some_and(|s| s.access_counters.is_some())
    }

    /// The master clock now (master cycles since power-on), the time base
    /// of the counters' `last_*` stamps.
    pub fn master_clock(&self) -> Result<u64, ApiError> {
        Ok(self.snes.as_ref().ok_or(ApiError::NoRom)?.total_mclk)
    }

    /// Every access counter back to zero (Mesen2's *Reset access stats*).
    pub fn reset_access_counters(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.clear_access_counters();
        Ok(())
    }

    /// The counters of `len` bytes of `kind` from `offset`. A bus view
    /// gives, at each address, the counters of the byte behind it (zero
    /// where nothing answers). The S-DSP registers and the coprocessors'
    /// buses have none.
    pub fn access_counters(
        &self,
        kind: MemoryKind,
        offset: u32,
        len: usize,
    ) -> Result<Vec<AccessCounter>, ApiError> {
        let size = self.memory_size(kind)?;
        Self::check_range(kind, size, offset, len)?;
        let snes = self.snes.as_ref().ok_or(ApiError::NoRom)?;
        let (Some(cpu), Some(spc)) = (
            snes.access_counters.as_deref(),
            snes.apu_real.access_counters.as_deref(),
        ) else {
            return Err(ApiError::BadArg(
                "the access counters are off (enable_access_counters)".into(),
            ));
        };
        let cpu_byte = |m: CountedMemory, o: usize| AccessCounter::from(cpu.memory(m).get(o));
        let ipl = snes.apu_real.control & 0x80 != 0;
        let offsets = (0..len).map(|i| (offset as usize + i) % size);
        Ok(match kind {
            MemoryKind::SnesMemory => offsets
                .map(|a| {
                    luna_core::locate(snes.mapper.as_ref(), a as u32)
                        .map(|(m, o)| cpu_byte(m, o))
                        .unwrap_or_default()
                })
                .collect(),
            MemoryKind::SpcMemory => offsets
                .map(|a| {
                    if ipl && a >= 0xFFC0 {
                        spc.rom.get(a - 0xFFC0).into()
                    } else {
                        spc.ram.get(a).into()
                    }
                })
                .collect(),
            MemoryKind::SpcRam => offsets.map(|a| spc.ram.get(a).into()).collect(),
            MemoryKind::SpcRom => offsets.map(|a| spc.rom.get(a).into()).collect(),
            _ => {
                let m = counted(kind).ok_or_else(|| {
                    ApiError::BadArg(format!("{}: no access counters", kind.name()))
                })?;
                offsets.map(|a| cpu_byte(m, a)).collect()
            }
        })
    }
}
