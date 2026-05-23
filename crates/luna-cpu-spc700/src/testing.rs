//! Test-only `SpcBus` implementations.

use crate::bus::SpcBus;

/// Flat 64 KB RAM bus, for unit-testing the SPC700 in isolation.
pub struct RamBus {
    mem: [u8; 0x1_0000],
}

impl Default for RamBus {
    fn default() -> Self {
        Self::new()
    }
}

impl RamBus {
    /// Build an empty bus (all zeroes).
    #[must_use]
    pub fn new() -> Self {
        Self { mem: [0; 0x1_0000] }
    }

    /// Direct read (cost-free) for assertions.
    #[must_use]
    pub fn peek(&self, addr: u16) -> u8 {
        self.mem[usize::from(addr)]
    }

    /// Direct write (cost-free) for setup.
    pub fn poke(&mut self, addr: u16, value: u8) {
        self.mem[usize::from(addr)] = value;
    }

    /// Bulk-load bytes at a given address (wrapping the 16-bit space).
    pub fn poke_slice(&mut self, addr: u16, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            let a = addr.wrapping_add(i as u16);
            self.mem[usize::from(a)] = b;
        }
    }
}

impl SpcBus for RamBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[usize::from(addr)]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.mem[usize::from(addr)] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_round_trip() {
        let mut bus = RamBus::new();
        bus.write(0x1234, 0xCC);
        assert_eq!(bus.read(0x1234), 0xCC);
    }
}
