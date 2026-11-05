//! Hardware strict mode — the journal of accesses a real console refuses or
//! corrupts, as a serialisable report (CLI `luna strict`, MCP
//! `take_hw_faults`). Observation only: switching it on never changes a
//! frame. The rules themselves live in `luna_core::hw_strict`.

use luna_core::HwRule;
use serde::Serialize;

use crate::{ApiError, Emulator};

/// One distinct fault: a rule broken by one instruction at one address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StrictFault {
    /// Rule code, `R1`..`R8`.
    pub rule: &'static str,
    /// One-line statement of the rule.
    pub what: &'static str,
    /// Address of the instruction behind the access (`$bb:aaaa`); for a DMA
    /// it is the instruction that started the transfer.
    pub pc: String,
    /// The address touched (`$2118`, an A-bus address, `$4016`).
    pub addr: String,
    /// `cpu`, `dma<n>` or `hdma<n>`.
    pub origin: String,
    /// Frame of the first occurrence.
    pub frame: u64,
    /// Scanline of the first occurrence.
    pub line: u16,
    /// Master-clock position in the line of the first occurrence.
    pub hclock: u16,
    /// How many times this (rule, pc, address) happened.
    pub count: u64,
}

/// What [`Emulator::take_hw_faults`] returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StrictReport {
    /// The distinct faults, in order of first occurrence.
    pub faults: Vec<StrictFault>,
    /// New distinct faults dropped because the journal was full.
    pub overflow: u64,
}

impl StrictReport {
    /// Total occurrences, folded ones included.
    #[must_use]
    pub fn occurrences(&self) -> u64 {
        self.faults.iter().map(|f| f.count).sum()
    }
}

/// The rules the strict mode knows, `(code, statement)`: what the docs and
/// `luna strict --help` list.
#[must_use]
pub fn strict_rules() -> Vec<(&'static str, &'static str)> {
    [
        HwRule::VramWrite,
        HwRule::OamWrite,
        HwRule::CgramWrite,
        HwRule::PpuRead,
        HwRule::DmaForbiddenA,
        HwRule::DmaWramPort,
        HwRule::AutoJoypadRead,
    ]
    .into_iter()
    .map(|r| (r.code(), r.summary()))
    .collect()
}

fn address(a: u32) -> String {
    if a > 0xFFFF {
        format!("${:02X}:{:04X}", a >> 16, a & 0xFFFF)
    } else {
        format!("${a:04X}")
    }
}

impl Emulator {
    /// Start the hardware strict journal on the loaded ROM. Read-only: the
    /// emulation (and so every frame hash) is unchanged. A later `load_rom`
    /// builds a new machine with the journal off; enable it again.
    pub fn enable_hw_strict(&mut self) -> Result<(), ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        snes.enable_hw_strict();
        Ok(())
    }

    /// Is the strict journal on?
    pub fn hw_strict_enabled(&self) -> Result<bool, ApiError> {
        Ok(self
            .snes
            .as_ref()
            .ok_or(ApiError::NoRom)?
            .hw_strict_enabled())
    }

    /// Drain the strict journal into a report (empty when it is off).
    pub fn take_hw_faults(&mut self) -> Result<StrictReport, ApiError> {
        let snes = self.snes.as_mut().ok_or(ApiError::NoRom)?;
        let (faults, overflow) = snes.take_hw_faults();
        Ok(StrictReport {
            faults: faults
                .into_iter()
                .map(|f| StrictFault {
                    rule: f.rule.code(),
                    what: f.rule.summary(),
                    pc: format!("${:02X}:{:04X}", f.pc >> 16, f.pc & 0xFFFF),
                    addr: address(f.addr),
                    origin: f.origin.label(),
                    frame: f.frame,
                    line: f.line,
                    hclock: f.hclock,
                    count: f.count,
                })
                .collect(),
            overflow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_print_bank_only_when_there_is_one() {
        assert_eq!(address(0x2118), "$2118");
        assert_eq!(address(0x00_8005), "$8005");
        assert_eq!(address(0x7E_1000), "$7E:1000");
    }

    #[test]
    fn every_rule_is_listed_once_in_order() {
        let codes: Vec<_> = strict_rules().into_iter().map(|r| r.0).collect();
        assert_eq!(codes, ["R1", "R2", "R3", "R4", "R5", "R6", "R8"]);
    }

    #[test]
    fn without_a_rom_every_call_says_so() {
        let mut em = Emulator::new();
        assert!(matches!(em.enable_hw_strict(), Err(ApiError::NoRom)));
        assert!(matches!(em.take_hw_faults(), Err(ApiError::NoRom)));
    }
}
