//! Per-byte access counters for a debugger (Mesen2's "access counters"):
//! how many times each byte of a memory was read, written and executed,
//! and the master clock of the last time.
//!
//! The counters belong to a physical memory (work RAM, a ROM offset,
//! VRAM…), never to a bus address, so every mirror of a byte shows the
//! same counts. Whoever drives a bus maps the address to its byte and
//! calls [`AccessCounters::note`].

/// What an access did to its byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A data read.
    Read,
    /// A write.
    Write,
    /// A code read: the opcode or an operand of an instruction.
    Exec,
}

/// One byte's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ByteCounters {
    /// Data reads.
    pub read: u32,
    /// Writes.
    pub write: u32,
    /// Code reads.
    pub exec: u32,
    /// Master clock of the last data read, 0 for never.
    pub last_read: u64,
    /// Master clock of the last write, 0 for never.
    pub last_write: u64,
    /// Master clock of the last code read, 0 for never.
    pub last_exec: u64,
}

/// The counters of every byte of one memory.
#[derive(Debug, Clone, Default)]
pub struct AccessCounters(Vec<ByteCounters>);

impl AccessCounters {
    /// Counters for a memory of `size` bytes, all at zero.
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self(vec![ByteCounters::default(); size])
    }

    /// Count one access to byte `offset` at master clock `clock`. An
    /// offset past the end is ignored.
    #[inline]
    pub fn note(&mut self, offset: usize, access: Access, clock: u64) {
        let Some(b) = self.0.get_mut(offset) else {
            return;
        };
        match access {
            Access::Read => {
                b.read = b.read.saturating_add(1);
                b.last_read = clock;
            }
            Access::Write => {
                b.write = b.write.saturating_add(1);
                b.last_write = clock;
            }
            Access::Exec => {
                b.exec = b.exec.saturating_add(1);
                b.last_exec = clock;
            }
        }
    }

    /// Byte `offset`'s counters (zero past the end).
    #[must_use]
    pub fn get(&self, offset: usize) -> ByteCounters {
        self.0.get(offset).copied().unwrap_or_default()
    }

    /// The memory's size in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// No byte to count.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every counter back to zero.
    pub fn clear(&mut self) {
        self.0.fill(ByteCounters::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_access_counts_on_its_own_counter() {
        let mut c = AccessCounters::new(4);
        c.note(1, Access::Read, 10);
        c.note(1, Access::Read, 12);
        c.note(1, Access::Exec, 13);
        c.note(2, Access::Write, 14);
        c.note(9, Access::Write, 15);
        let b = c.get(1);
        assert_eq!((b.read, b.write, b.exec), (2, 0, 1));
        assert_eq!((b.last_read, b.last_write, b.last_exec), (12, 0, 13));
        assert_eq!(c.get(2).write, 1);
        assert_eq!(c.get(9), ByteCounters::default());
        c.clear();
        assert_eq!(c.get(1), ByteCounters::default());
    }
}
