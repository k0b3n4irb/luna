//! SPC700 trajectory differential harness — Tales of Phantasia OP derail.
//!
//! Background (see the `project_tales_op_battle_freeze` memory): with the
//! no-overshoot SPC catch-up fix, Tales' OP plays with sound but the SPC
//! music driver later **derails** — its command dispatch (`JMP [!$1959+X]`
//! at `$1662`) reads an out-of-range command byte (`$FE` from `[$4BAB]`),
//! jumps past the 13-entry table into garbage at `$E8FC`, and hits `SLEEP`
//! (`$EF`) → frozen at `$E90C`. The CPU then deadlocks waiting for the
//! asleep SPC. The ares ARAM diff proved the driver code + jump table are
//! byte-identical, so it is a **dynamic** pointer/state divergence.
//!
//! # Phase 1 — find the derail, then test mailbox-dependence
//!
//! 1. Scan luna's own full-system run to locate the derail step (printing
//!    `spc_pc` checkpoints, to diagnose any divergence from the CLI).
//! 2. Re-run to just before that step, freeze the CPU mailbox, and free-run
//!    ONLY the APU: if it still derails, the bug is internal to the SPC
//!    (engine / timer / sequence playback) and a luna↔ares SMP trajectory
//!    diff is valid (Phase 2); if not, the derail needs live CPU mailbox
//!    input and Phase 2 must be CPU-coupled.
//!
//! Run (release; ignored — needs the gitignored Tales ROM):
//! ```text
//! cargo test --release -p luna-core --test spc_trajectory \
//!     spc_freerun_reproduces_derail -- --ignored --nocapture
//! ```

use std::fmt::Write as _;

use luna_cartridge::Cartridge;
use luna_core::Snes;

const ROM: &str = "../../tests/roms/Tales of Phantasia (Japan).sfc";
/// PC range the derailed SPC executes (garbage past the jump table, up to
/// the `SLEEP` at `$E90B` → freeze at `$E90C`).
const DERAIL_LO: u16 = 0xE800;
const DERAIL_HI: u16 = 0xEA00;
/// Hard ceiling on the scan, so a non-reproducing run can't spin forever.
const SCAN_CAP: u64 = 60_000_000;

fn derailed(snes: &Snes) -> bool {
    let cpu = &snes.apu_real.cpu;
    cpu.sleeping || cpu.stopped || (DERAIL_LO..DERAIL_HI).contains(&cpu.pc)
}

fn boot() -> Snes {
    let cart = Cartridge::load(ROM).expect("Tales load (auto-detect ExHiRom + SRAM)");
    let mut snes = Snes::from_cartridge(cart);
    // Match `Emulator::load_rom` exactly: it calls `reset()` after
    // construction. Without it the power-on state differs and the IPL
    // upload handshake never completes (SPC stuck in the IPL poll loop).
    snes.reset();
    snes
}

/// Drift-trace step 1: report the SPC's total opcode-cycle count (and its
/// T0/T1 + T2 timer phases) at the moment it first leaves the IPL ROM (the
/// JMP into the uploaded driver). Compared against ares' `lunaClocks/2` at
/// the same event, this tests whether the IPL-upload exit timing seeds the
/// Tales derail's timer-phase offset (luna exits the upload ~1883 poll-spins
/// early vs ares). `timer_subdivider` counts one per SPC opcode-cycle.
#[test]
#[ignore = "manual; needs the gitignored Tales ROM + release build"]
fn spc_ipl_exit_timer_phase() {
    if !std::path::Path::new(ROM).exists() {
        eprintln!("[skip] Tales ROM absent at {ROM}");
        return;
    }
    let mut snes = boot();
    // The SPC boots inside the IPL ROM ($FFC0+); the first pc < $FFC0 after
    // it has run IPL code is the JMP into the uploaded driver. Capture the
    // pre-state there (consistent with an ares hook at the top of SMP::main
    // before executing the instruction).
    let mut seen_ipl = false;
    for i in 0..5_000_000u64 {
        let pc = snes.apu_real.cpu.pc;
        if pc >= 0xFFC0 {
            seen_ipl = true;
        }
        if seen_ipl && pc < 0xFFC0 {
            let sub = snes.apu_real.timer_subdivider;
            eprintln!(
                "luna IPL-exit @ CPU instr {i}: spc_pc={:#06X} total_spc_cycles(timer_subdivider)={sub}  \
                 T0/T1 phase = {} /128,  T2 phase = {} /16",
                snes.apu_real.cpu.pc,
                sub % 128,
                sub % 16
            );
            return;
        }
        snes.step();
    }
    eprintln!("luna never left the IPL ROM in 5M instrs");
}

#[test]
#[ignore = "manual SPC trajectory harness; needs the gitignored Tales ROM + release build"]
fn spc_freerun_reproduces_derail() {
    if !std::path::Path::new(ROM).exists() {
        eprintln!("[skip] Tales ROM absent at {ROM}");
        return;
    }

    // --- Phase A: scan luna's own run for the derail. ---
    let mut snes = boot();
    let checkpoints = [1_000_000u64, 10_000_000, 20_000_000, 30_000_000, 39_480_000];
    let mut ci = 0usize;
    let mut derail_step: Option<u64> = None;
    eprintln!("[scan] looking for the derail (spc_pc checkpoints to compare vs CLI):");
    for i in 0..SCAN_CAP {
        if ci < checkpoints.len() && i == checkpoints[ci] {
            eprintln!("  @{i:>9}: spc_pc={:#06X}", snes.apu_real.cpu.pc);
            ci += 1;
        }
        snes.step();
        if derailed(&snes) {
            derail_step = Some(i);
            break;
        }
    }
    let Some(d) = derail_step else {
        eprintln!(
            "\n=== harness did NOT reproduce the derail in {SCAN_CAP} steps ===\n\
             ⇒ luna's raw-Snes run diverges from the CLI's; investigate the boot path \
             (the spc_pc checkpoints above should match `luna state -n <count>`)."
        );
        return;
    };
    eprintln!(
        "[scan] DERAIL at step {d}: spc_pc={:#06X} sleeping={} stopped={}",
        snes.apu_real.cpu.pc, snes.apu_real.cpu.sleeping, snes.apu_real.cpu.stopped
    );

    // --- Phase B: re-run to just before the derail, dump the injection
    //     state (SMP regs + raw ARAM) for ares, freeze the mailbox, and
    //     free-run the APU one instruction at a time, logging the per-
    //     instruction trajectory until the derail. ---
    let margin = 40_000u64;
    let start = d.saturating_sub(margin);
    let mut snes = boot();
    for _ in 0..start {
        snes.step();
    }
    assert!(
        !derailed(&snes),
        "margin too small — already derailed at the free-run start"
    );
    // Phase-vs-bug control (LUNA_SPC_RESET_TIMER): reset BOTH timer phases
    // to an identical aligned state (subdivider + per-timer divider = 0) so
    // luna's free-run and ares' injected run start from the SAME timer
    // phase (ares zeros stage0/stage1/stage2 to match). RESULT (settled
    // 2026-06-15): WITH the reset the trajectories agree for all 12k traced
    // instrs ⇒ the SPC700 engine + timer MODEL are faithful; WITHOUT it the
    // first divergence is a T1OUT poll (ares' T1 ticks one poll before
    // luna's) ⇒ the Tales derail is an accumulated timer-PHASE drift, not a
    // model bug. Output counters / targets / enables are always preserved.
    if std::env::var("LUNA_SPC_RESET_TIMER").is_ok() {
        snes.apu_real.timer_subdivider = 0;
        snes.apu_real.timer_internal = [0; 3];
    }
    let cpu = &snes.apu_real.cpu;
    // Injection state for ares: registers + raw 64 KB ARAM + frozen mailbox
    // + timer config (now phase-aligned to zero). ares overwrites its SMP
    // regs/apuram/timers with these so the two trajectories share an origin.
    let mb = snes.apu_real.to_spc_ports;
    let tout = snes.apu_real.timer_output;
    let tdiv = snes.apu_real.timer_reload;
    let ten = snes.apu_real.timer_enabled;
    let regs = format!(
        "pc={}\na={}\nx={}\ny={}\nsp={}\npsw={}\ncontrol={}\ntest={}\n\
         cpu0={}\ncpu1={}\ncpu2={}\ncpu3={}\n\
         t0out={}\nt1out={}\nt2out={}\n\
         t0div={}\nt1div={}\nt2div={}\n\
         t0en={}\nt1en={}\nt2en={}\n",
        cpu.pc,
        cpu.a,
        cpu.x,
        cpu.y,
        cpu.sp,
        cpu.psw.0,
        snes.apu_real.control,
        snes.apu_real.test,
        mb[0],
        mb[1],
        mb[2],
        mb[3],
        tout[0],
        tout[1],
        tout[2],
        tdiv[0],
        tdiv[1],
        tdiv[2],
        u8::from(ten[0]),
        u8::from(ten[1]),
        u8::from(ten[2]),
    );
    std::fs::write("/tmp/luna_spc_inject.txt", regs).expect("write inject regs");
    std::fs::write("/tmp/luna_spc_inject_aram.bin", &snes.apu_real.aram[..])
        .expect("write inject aram");
    eprintln!(
        "[inject] dumped SMP state @ step {start}: pc={:#06X} a={:02X} x={:02X} y={:02X} sp={:02X}",
        cpu.pc, cpu.a, cpu.x, cpu.y, cpu.sp
    );

    let frozen = snes.apu_real.to_spc_ports;
    let mut traj = String::new();
    let mut derailed_free = None;
    for i in 0..200_000usize {
        let (pc, a, x, y, sp, psw) = snes.apu_real.trace_step_one();
        let _ = writeln!(traj, "{pc:04X} {a:02X} {x:02X} {y:02X} {sp:02X} {psw:02X}");
        if derailed(&snes) {
            derailed_free = Some(i);
            break;
        }
    }
    std::fs::write("/tmp/luna_spc_traj.txt", &traj).expect("write trajectory");
    assert_eq!(
        frozen, snes.apu_real.to_spc_ports,
        "mailbox changed during free-run — the CPU must not have run"
    );

    match derailed_free {
        Some(i) => eprintln!(
            "\n=== DERAIL REPRODUCED mailbox-free (free-run instr {i}, spc_pc={:#06X}) ===\n\
             luna trajectory ({} instrs) -> /tmp/luna_spc_traj.txt\n\
             injection state -> /tmp/luna_spc_inject.txt + /tmp/luna_spc_inject_aram.bin\n\
             ⇒ inject these into ares, free-run + trace, diff to the first diverging op (Phase 2).",
            snes.apu_real.cpu.pc,
            traj.lines().count()
        ),
        None => eprintln!(
            "\n=== NO derail in the mailbox-frozen free-run (started {margin} steps before) ===\n\
             ⇒ the derail needs live CPU mailbox input within the last {margin} steps."
        ),
    }
}
