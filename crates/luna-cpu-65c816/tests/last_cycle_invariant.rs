//! Every instruction polls the interrupt lines exactly once.
//!
//! ares marks the cycle before an instruction's final bus access with `L`
//! (`#define L lastCycle();`, `wdc65816/registers.hpp:30`) and samples
//! NMI/IRQ there. luna spells that `Cpu::last_cycle` and its `last_*`
//! access helpers. Because the interrupt is delivered **only** through
//! that poll, a handler that forgets its marker would never take an
//! interrupt again — a hang, in one game, months later. This test makes
//! that impossible: it drives all 256 opcodes in every width mode and
//! asserts the poll ran exactly once per instruction.
//!
//! It is a structural invariant, not a timing check: where the marker
//! sits inside the instruction is the reference's business (see
//! `docs/luna_65c816_gaps.md`), that it is there at all is this test's.

use luna_bus::{Addr24, Bus, InterruptSample, MCycles};
use luna_cpu_65c816::Cpu;
use luna_cpu_65c816::flags::bit;

/// A flat-RAM bus that counts `last_cycle` polls and never reports an
/// interrupt (so no instruction is diverted into a service sequence).
struct CountingBus {
    mem: Vec<u8>,
    polls: u32,
}

impl CountingBus {
    fn new() -> Self {
        Self {
            mem: vec![0; 0x100_0000],
            polls: 0,
        }
    }
}

impl Bus for CountingBus {
    fn read(&mut self, addr: Addr24) -> u8 {
        self.mem[addr as usize & 0x00FF_FFFF]
    }
    fn write(&mut self, addr: Addr24, value: u8) {
        self.mem[addr as usize & 0x00FF_FFFF] = value;
    }
    fn io_cycle(&mut self, _mcycles: MCycles) {}
    fn last_cycle(&mut self, _i_flag: bool) -> InterruptSample {
        self.polls += 1;
        InterruptSample::NONE
    }
}

/// One CPU configuration to run every opcode under.
struct Mode {
    name: &'static str,
    e: bool,
    /// `M`/`X` (native only): 8-bit when set.
    acc8: bool,
    idx8: bool,
    /// Non-zero direct-page low byte — exercises the `idle2` path.
    dp: u16,
    /// Forces the opposite outcome for conditional branches.
    flip_branches: bool,
}

const MODES: &[Mode] = &[
    Mode {
        name: "emulation",
        e: true,
        acc8: true,
        idx8: true,
        dp: 0x0000,
        flip_branches: false,
    },
    Mode {
        name: "native m8 x8",
        e: false,
        acc8: true,
        idx8: true,
        dp: 0x0000,
        flip_branches: false,
    },
    Mode {
        name: "native m16 x16",
        e: false,
        acc8: false,
        idx8: false,
        dp: 0x0000,
        flip_branches: false,
    },
    Mode {
        name: "native m16 x8, dp != 0",
        e: false,
        acc8: false,
        idx8: true,
        dp: 0x0123,
        flip_branches: false,
    },
    Mode {
        name: "native m8 x16, branches flipped",
        e: false,
        acc8: true,
        idx8: false,
        dp: 0x0000,
        flip_branches: true,
    },
];

fn setup(mode: &Mode, opcode: u8) -> (Cpu, CountingBus) {
    let mut cpu = Cpu::new();
    let mut bus = CountingBus::new();

    cpu.e = mode.e;
    if mode.e {
        cpu.sp = 0x01FF;
    } else {
        cpu.sp = 0x1FFF;
        cpu.p.set(bit::M, mode.acc8);
        cpu.p.set(bit::X, mode.idx8);
    }
    // Every branch condition flag at once: `flip_branches` swaps which
    // side of each conditional is taken, so both paths get covered.
    cpu.p.set(bit::C, mode.flip_branches);
    cpu.p.set(bit::Z, mode.flip_branches);
    cpu.p.set(bit::N, mode.flip_branches);
    cpu.p.set(bit::V, mode.flip_branches);
    cpu.dp = mode.dp;
    cpu.db = 0x00;
    cpu.pb = 0x00;
    cpu.pc = 0x8000;
    cpu.x = 0x0004;
    cpu.y = 0x0004;
    // MVN/MVP move one byte per step; a zero counter keeps this to the
    // single iteration we are measuring.
    cpu.a = 0x0000;

    // The opcode plus operand bytes. $10 keeps every operand address
    // inside plain RAM and away from the code being executed.
    bus.mem[0x00_8000] = opcode;
    bus.mem[0x00_8001] = 0x10;
    bus.mem[0x00_8002] = 0x10;
    bus.mem[0x00_8003] = 0x10;
    // A non-zero indirect pointer target, so indirect modes land somewhere
    // harmless rather than back on the code.
    bus.mem[0x00_0010] = 0x00;
    bus.mem[0x00_0011] = 0x20;
    bus.mem[0x00_0012] = 0x00;
    (cpu, bus)
}

#[test]
fn every_opcode_polls_the_interrupt_lines_exactly_once() {
    let mut failures: Vec<String> = Vec::new();

    for mode in MODES {
        for opcode in 0u16..=0xFF {
            let opcode = opcode as u8;
            let (mut cpu, mut bus) = setup(mode, opcode);
            cpu.step(&mut bus);
            if bus.polls != 1 {
                failures.push(format!(
                    "  ${opcode:02X} in {}: {} polls",
                    mode.name, bus.polls
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} opcode/mode combinations did not poll exactly once \
         (ares marks every instruction's last cycle with `L`):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `WAI` polls from inside its wait loop (ares `instructionWait`:
/// `while(r.wai && ...) { L idle(); }`), so a parked CPU keeps sampling —
/// that is how a `WAI` ever wakes up.
#[test]
fn wai_polls_on_every_stalled_step() {
    let mode = &MODES[1];
    let (mut cpu, mut bus) = setup(mode, 0xCB);
    for step in 1..=4u32 {
        cpu.step(&mut bus);
        assert_eq!(
            bus.polls, step,
            "expected one poll per stalled step, got {} after {step} steps",
            bus.polls
        );
    }
}

/// `STP` polls once, on the step that executes it. ares then loops
/// `L idle()` forever, but a luna `Cpu` that has stopped does no bus work
/// at all — the system loop (`Snes::step`) owns the halted clock instead,
/// so there is no further poll to make.
#[test]
fn stp_polls_once_then_the_cpu_is_halted() {
    let mode = &MODES[1];
    let (mut cpu, mut bus) = setup(mode, 0xDB);
    cpu.step(&mut bus);
    assert_eq!(bus.polls, 1);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(bus.polls, 1, "a halted CPU makes no further accesses");
}
