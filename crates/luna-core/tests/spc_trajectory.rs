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

    // --- Phase B: re-run to just before the derail, freeze the mailbox,
    //     free-run only the APU, and see whether it still derails. ---
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
    let frozen = snes.apu_real.to_spc_ports;
    let mut derailed_free = None;
    for i in 0..2_000_000usize {
        snes.apu_real.step(84); // ~4 SPC cycles ≈ 1-2 SPC instrs/call
        if derailed(&snes) {
            derailed_free = Some(i);
            break;
        }
    }
    assert_eq!(
        frozen, snes.apu_real.to_spc_ports,
        "mailbox changed during free-run — the CPU must not have run"
    );

    match derailed_free {
        Some(i) => eprintln!(
            "\n=== DERAIL REPRODUCED mailbox-free (free-run call {i}, spc_pc={:#06X}) ===\n\
             ⇒ INTERNAL to the SPC (engine / timer / sequence playback), NOT mailbox-dependent.\n\
             ⇒ a synchronized luna↔ares SMP trajectory diff is VALID (Phase 2).",
            snes.apu_real.cpu.pc
        ),
        None => eprintln!(
            "\n=== NO derail in the mailbox-frozen free-run (started {margin} steps before) ===\n\
             ⇒ the derail needs live CPU mailbox input within the last {margin} steps.\n\
             ⇒ Phase 2 must be a CPU-coupled SMP trace, not a free-run trajectory."
        ),
    }
}
