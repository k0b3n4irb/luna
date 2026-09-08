//! Master-cycle accounting by consumer (issue #223).
//!
//! `total_mclk` says how long the machine ran; it does not say who used
//! the time. A ROM parked in `WAI` for most of the frame and a ROM that
//! is CPU-bound spend the same master cycles, and the instruction count
//! is worse than useless for telling them apart (a parked `WAI` step is
//! counted as an instruction, so *less* work per frame reads as *more*
//! instructions). These buckets split every master cycle the scheduler
//! charges into who paid for it, cumulatively and for the last completed
//! PPU frame, so "CPU headroom per frame" is a direct read.
//!
//! The buckets are exact partitions of `total_mclk`: every increment of
//! the master clock (`SnesBus::advance_time`) credits exactly one bucket.

use serde::{Deserialize, Serialize};

/// Who a master-cycle charge is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MclkKind {
    /// The CPU executing instructions (bus accesses + internal cycles),
    /// including the reset sequence.
    #[default]
    CpuActive,
    /// The CPU parked in `WAI`, ticking the clock until an interrupt.
    CpuWai,
    /// The CPU halted by `STP` (nothing but a reset restarts it).
    CpuStp,
    /// A general-purpose DMA burst (`$420B`): the CPU is halted while
    /// the controller moves bytes.
    Dma,
    /// HDMA: per-scanline table fetches + transfers and the frame-start
    /// `hdma_init`, charged as a CPU stall at each line crossing.
    Hdma,
    /// The once-per-scanline DRAM refresh halt (40 master clocks).
    Refresh,
}

/// Master cycles per consumer. Every field is a count of master clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MclkBuckets {
    /// CPU executing instructions.
    pub cpu_active: u64,
    /// CPU parked in `WAI`.
    pub cpu_wai: u64,
    /// CPU halted by `STP`.
    pub cpu_stp: u64,
    /// General-purpose DMA bursts.
    pub dma: u64,
    /// HDMA transfers + table fetches + frame-start init.
    pub hdma: u64,
    /// DRAM refresh halts.
    pub refresh: u64,
}

impl MclkBuckets {
    /// Credit `mclk` to the bucket `kind` names.
    #[inline]
    pub const fn credit(&mut self, kind: MclkKind, mclk: u64) {
        let slot = match kind {
            MclkKind::CpuActive => &mut self.cpu_active,
            MclkKind::CpuWai => &mut self.cpu_wai,
            MclkKind::CpuStp => &mut self.cpu_stp,
            MclkKind::Dma => &mut self.dma,
            MclkKind::Hdma => &mut self.hdma,
            MclkKind::Refresh => &mut self.refresh,
        };
        *slot = slot.saturating_add(mclk);
    }

    /// Sum of every bucket — equals `total_mclk` for the cumulative set.
    pub const fn total(&self) -> u64 {
        self.cpu_active + self.cpu_wai + self.cpu_stp + self.dma + self.hdma + self.refresh
    }

    /// `self - other`, field by field (saturating).
    #[must_use]
    pub const fn delta(&self, other: &Self) -> Self {
        Self {
            cpu_active: self.cpu_active.saturating_sub(other.cpu_active),
            cpu_wai: self.cpu_wai.saturating_sub(other.cpu_wai),
            cpu_stp: self.cpu_stp.saturating_sub(other.cpu_stp),
            dma: self.dma.saturating_sub(other.dma),
            hdma: self.hdma.saturating_sub(other.hdma),
            refresh: self.refresh.saturating_sub(other.refresh),
        }
    }
}

/// The live accounting state: cumulative buckets, the snapshot taken at
/// the last frame wrap, and the consumer the current charge belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MclkAccounting {
    /// Since reset.
    pub cumulative: MclkBuckets,
    /// The last completed PPU frame (`cumulative` between the two most
    /// recent frame wraps). All zero until the first frame completes.
    pub last_frame: MclkBuckets,
    /// `cumulative` at the most recent frame wrap.
    pub frame_start: MclkBuckets,
    /// Who the caller's own time is charged to right now. The CPU step
    /// sets it per instruction (active / `WAI` / `STP`); the DMA edge
    /// sets it to [`MclkKind::Dma`] for the burst. Stalls (HDMA, refresh)
    /// are credited to their own buckets regardless.
    pub current: MclkKind,
    /// CPU steps that were a parked `WAI` / `STP` tick rather than an
    /// instruction — `instructions_executed - steps_idle` is the count
    /// of instructions that did work.
    pub steps_idle: u64,
}

impl MclkAccounting {
    /// Credit `mclk` to the current consumer.
    #[inline]
    pub const fn credit_current(&mut self, mclk: u64) {
        self.cumulative.credit(self.current, mclk);
    }

    /// Credit `mclk` to an explicit consumer (a stall).
    #[inline]
    pub const fn credit(&mut self, kind: MclkKind, mclk: u64) {
        self.cumulative.credit(kind, mclk);
    }

    /// Called at the PPU frame wrap: close the frame's buckets.
    pub const fn on_frame_wrap(&mut self) {
        self.last_frame = self.cumulative.delta(&self.frame_start);
        self.frame_start = self.cumulative;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_partition_and_frame_wrap_snapshots() {
        let mut acc = MclkAccounting {
            current: MclkKind::CpuWai,
            ..Default::default()
        };
        acc.credit_current(100);
        acc.credit(MclkKind::Refresh, 40);
        acc.credit(MclkKind::Hdma, 18);
        assert_eq!(acc.cumulative.total(), 158);
        assert_eq!(acc.last_frame, MclkBuckets::default());

        acc.on_frame_wrap();
        assert_eq!(acc.last_frame.cpu_wai, 100);
        assert_eq!(acc.last_frame.refresh, 40);
        assert_eq!(acc.last_frame.hdma, 18);

        acc.current = MclkKind::Dma;
        acc.credit_current(8);
        acc.on_frame_wrap();
        assert_eq!(acc.last_frame.total(), 8);
        assert_eq!(acc.last_frame.dma, 8);
        assert_eq!(acc.cumulative.total(), 166);
    }
}
