//! Breakpoints on a cartridge coprocessor (SA-1, Super FX, DSP-1).
//!
//! A coprocessor runs whole instructions in a catch-up loop behind the
//! S-CPU. While a debugger arms [`CoprocDebug`], the loop checks its exec
//! breakpoints at each instruction start and its watchpoints after each
//! instruction; a hit freezes the chip (it stops catching up) until
//! [`CoprocDebug::resume`]. Conditions are closures the debugger builds,
//! handed a [`CoprocView`] of the chip.

use std::sync::Arc;

/// A coprocessor's registers, as its debugger names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoprocRegs {
    /// The SA-1's 65C816.
    Sa1 {
        /// The 16-bit accumulator.
        a: u16,
        /// Index X.
        x: u16,
        /// Index Y.
        y: u16,
        /// Stack pointer.
        sp: u16,
        /// Direct page.
        d: u16,
        /// Data bank.
        db: u8,
        /// `K:PC`.
        pc: u32,
        /// The status byte P.
        p: u8,
        /// An NMI is pending.
        nmi: bool,
        /// An IRQ is pending.
        irq: bool,
        /// The scanline its H/V timer follows.
        scanline: u16,
        /// The master clock in that line.
        hclock: u16,
    },
    /// The Super FX's GSU.
    Gsu {
        /// R0-R15 (R15 is the program counter).
        r: [u16; 16],
        /// The source register `FROM` / `WITH` selected.
        src: u8,
        /// The destination register `TO` / `WITH` selected.
        dst: u8,
        /// The status flags register SFR.
        sfr: u16,
        /// Program bank.
        pbr: u8,
        /// ROM data bank.
        rombr: u8,
        /// RAM data bank.
        rambr: u8,
    },
}

/// One data access an instruction made on the coprocessor's bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoprocAccess {
    /// The address on the chip's bus (an offset in the watched memory in a
    /// [`CoprocHit`]).
    pub addr: u32,
    /// The byte read or written.
    pub value: u8,
    /// A write (else a read).
    pub write: bool,
}

/// A coprocessor seen by a breakpoint condition: its registers and its
/// own bus, without side effects.
pub trait CoprocView {
    /// The registers.
    fn regs(&self) -> CoprocRegs;
    /// The byte at `addr` on the chip's bus (0 where nothing answers).
    fn peek(&self, addr: u32) -> u8;
}

/// A condition a coprocessor breakpoint fires on, given the chip, for a
/// watchpoint the access at its bus address, and the instruction's
/// address.
pub type CoprocCondition =
    Arc<dyn Fn(&dyn CoprocView, Option<CoprocAccess>, u32) -> bool + Send + Sync>;

/// One exec breakpoint.
#[derive(Clone)]
pub struct CoprocBreak {
    /// The debugger's id for it.
    pub id: u32,
    /// The instruction's address.
    pub pc: u32,
    /// Fire only when this holds; `None` fires every time.
    pub condition: Option<CoprocCondition>,
}

/// One memory watchpoint (an inclusive range of the chip's bus).
#[derive(Clone)]
pub struct CoprocWatch {
    /// The debugger's id for it.
    pub id: u32,
    /// The range start.
    pub lo: u32,
    /// The inclusive range end.
    pub hi: u32,
    /// Fire on reads.
    pub read: bool,
    /// Fire on writes.
    pub write: bool,
    /// Fire only when this holds; `None` fires every time.
    pub condition: Option<CoprocCondition>,
}

/// What froze the coprocessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoprocHit {
    /// The breakpoint's id.
    pub id: u32,
    /// The instruction's address: the one about to run for an exec
    /// breakpoint, the one that made the access for a watchpoint.
    pub pc: u32,
    /// A watchpoint's access.
    pub access: Option<CoprocAccess>,
}

/// Most accesses one instruction records for its watchpoints.
const ACCESS_MAX: usize = 64;

/// A coprocessor's armed breakpoints and the hit that froze it.
#[derive(Default)]
pub struct CoprocDebug {
    /// Exec breakpoints.
    pub breaks: Vec<CoprocBreak>,
    /// Memory watchpoints.
    pub watches: Vec<CoprocWatch>,
    /// The hit the chip stands on, if any: it does not run while set.
    pub hit: Option<CoprocHit>,
    /// The PC a resume steps over once.
    resume: Option<u32>,
    /// The current instruction's data accesses, while watchpoints are armed.
    accesses: Vec<CoprocAccess>,
}

impl CoprocDebug {
    /// Nothing armed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            breaks: Vec::new(),
            watches: Vec::new(),
            hit: None,
            resume: None,
            accesses: Vec::new(),
        }
    }

    /// Whether anything is armed.
    #[must_use]
    pub const fn armed(&self) -> bool {
        !self.breaks.is_empty() || !self.watches.is_empty()
    }

    /// Whether the chip's bus should record its data accesses.
    #[must_use]
    pub const fn watching(&self) -> bool {
        !self.watches.is_empty()
    }

    /// Disarm the breakpoints after a run. A hit the chip stands on stays
    /// until [`Self::resume`].
    pub fn disarm(&mut self) {
        self.breaks.clear();
        self.watches.clear();
        self.accesses.clear();
    }

    /// Leave the hit the chip stands on: it runs again, and an exec
    /// breakpoint it stood on does not fire again at once.
    pub const fn resume(&mut self) {
        if let Some(CoprocHit {
            pc, access: None, ..
        }) = self.hit.take()
        {
            self.resume = Some(pc);
        }
    }

    /// Record one data access of the running instruction.
    pub fn record(&mut self, access: CoprocAccess) {
        if self.accesses.len() < ACCESS_MAX {
            self.accesses.push(access);
        }
    }

    /// At an instruction start (`pc`): freeze on an exec breakpoint there
    /// whose condition holds. Returns whether it froze.
    pub fn check_exec(&mut self, pc: u32, view: &dyn CoprocView) -> bool {
        if self.resume.take() == Some(pc) {
            return false;
        }
        let hit = self
            .breaks
            .iter()
            .find(|b| b.pc == pc && b.condition.as_ref().is_none_or(|c| c(view, None, pc)))
            .map(|b| b.id);
        if let Some(id) = hit {
            self.hit = Some(CoprocHit {
                id,
                pc,
                access: None,
            });
        }
        hit.is_some()
    }

    /// After the instruction at `pc`: freeze on the first recorded access
    /// a watchpoint matches whose condition holds. Returns whether it froze.
    pub fn check_accesses(&mut self, pc: u32, view: &dyn CoprocView) -> bool {
        let accesses = std::mem::take(&mut self.accesses);
        let hit = accesses.iter().find_map(|&a| {
            self.watches.iter().find_map(|w| {
                let wanted = if a.write { w.write } else { w.read };
                (wanted
                    && (w.lo..=w.hi).contains(&a.addr)
                    && w.condition.as_ref().is_none_or(|c| c(view, Some(a), pc)))
                .then_some((w.id, a))
            })
        });
        if let Some((id, access)) = hit {
            self.hit = Some(CoprocHit {
                id,
                pc,
                access: Some(access),
            });
        }
        hit.is_some()
    }
}
