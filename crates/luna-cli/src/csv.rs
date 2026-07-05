//! Trace-CSV writers shared by the `state` diagnostics — one
//! writer per trace kind over a common `write_csv` skeleton.

use crate::fmt::fmt_pc;

/// Master clocks per NTSC frame: 262 scanlines × 1364 mclk = 357 368.
const NTSC_MCLK_PER_FRAME: u64 = 1364 * 262;

/// Shared skeleton for the trace CSV writers: create `path`, write the
/// `header` line, then format each event via `row` (handed the writer,
/// the event index, and the event). Centralises the
/// File/BufWriter/header boilerplate every writer repeated.
fn write_csv<T>(
    path: &std::path::Path,
    header: &str,
    rows: &[T],
    mut row: impl FnMut(&mut dyn std::io::Write, usize, &T) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(f, "{header}")?;
    for (i, ev) in rows.iter().enumerate() {
        row(&mut f, i, ev)?;
    }
    f.flush()
}

/// Write APU mailbox events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc_bank_offset, kind, port, value_hex`.
/// `frame_ntsc` assumes NTSC (262 lines × 1364 mclk = 357 368 mclk/frame).
pub(crate) fn write_mailbox_log_csv(
    path: &std::path::Path,
    events: &[luna_api::MailboxEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,kind,port,value",
        events,
        |f, _, ev| {
            let kind = match ev.kind {
                luna_api::MailboxEventKind::Read => "R",
                luna_api::MailboxEventKind::Write => "W",
            };
            writeln!(
                f,
                "{},{},{},{},{},${:02X}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                kind,
                ev.port,
                ev.value
            )
        },
    )
}

/// Write SA-1 MMIO events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc, kind, reg, value`. `reg` is the register
/// address (`$2200-$23FF`); `frame_ntsc` assumes NTSC.
pub(crate) fn write_sa1_log_csv(
    path: &std::path::Path,
    events: &[luna_api::Sa1LogEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,kind,reg,value",
        events,
        |f, _, ev| {
            let kind = match ev.kind {
                luna_api::MailboxEventKind::Read => "R",
                luna_api::MailboxEventKind::Write => "W",
            };
            writeln!(
                f,
                "{},{},{},{},${:04X},${:02X}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                kind,
                ev.reg,
                ev.value
            )
        },
    )
}

/// Write SA-1-side execution events as CSV. Columns:
/// `seq, sa1_pc, kind, reg, value`. `seq` is the event index (the SA-1
/// side has no master-clock handle); ordering is what reveals the loop.
pub(crate) fn write_sa1_side_log_csv(
    path: &std::path::Path,
    events: &[luna_api::Sa1SideEvent],
) -> std::io::Result<()> {
    write_csv(path, "seq,sa1_pc,kind,reg,value", events, |f, i, ev| {
        writeln!(
            f,
            "{},{},{},${:04X},${:02X}",
            i,
            fmt_pc(ev.sa1_pc),
            if ev.write { "W" } else { "R" },
            ev.reg,
            ev.value
        )
    })
}

/// Write the full SA-1 instruction trace as CSV. Columns:
/// `seq, pc, a, x, y, sp, p, db, dp, e`. Diff the `pc` column against a
/// reference (bsnes/Mesen2) SA-1 trace to find the first divergence.
pub(crate) fn write_sa1_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::Sa1TraceEvent],
) -> std::io::Result<()> {
    write_csv(path, "seq,pc,a,x,y,sp,p,db,dp,e", events, |f, i, ev| {
        writeln!(
            f,
            "{},{},${:04X},${:04X},${:04X},${:04X},${:02X},${:02X},${:04X},{}",
            i,
            fmt_pc(ev.pc_full),
            ev.a,
            ev.x,
            ev.y,
            ev.sp,
            ev.p,
            ev.db,
            ev.dp,
            u8::from(ev.e),
        )
    })
}

/// Write per-opcode SPC700 trace events as CSV. Columns:
/// `seq, pc, a, x, y, sp, psw`. Diff the `pc` column against a Mesen2
/// SPC700 trace to find the first divergence in the audio driver.
pub(crate) fn write_spc_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::Spc700TraceEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "seq,pc,a,x,y,sp,psw,spc_cycle,t2_int,t2_out",
        events,
        |f, i, ev| {
            writeln!(
                f,
                "{},${:04X},${:02X},${:02X},${:02X},${:02X},${:02X},{},{},{}",
                i, ev.pc, ev.a, ev.x, ev.y, ev.sp, ev.psw, ev.spc_cycle, ev.t2_int, ev.t2_out,
            )
        },
    )
}

/// Write per-opcode Super FX (GSU) trace events as CSV. Columns:
/// `seq, pc, opcode, sfr, r0..r15`. Diff the `pc` / register columns
/// against a reference (bsnes / siena) GSU trace to find the first
/// divergence in the rendering.
pub(crate) fn write_superfx_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::SuperFxTraceEvent],
) -> std::io::Result<()> {
    let mut header = String::from("seq,mclk,go,stop,pc,opcode,sfr");
    for n in 0..16 {
        header.push_str(",r");
        header.push_str(&n.to_string());
    }
    write_csv(path, &header, events, |f, i, ev| {
        write!(
            f,
            "{},{},{},{},{},${:02X},${:04X}",
            i,
            ev.mclk,
            u8::from(ev.go_start),
            u8::from(ev.stop),
            fmt_pc(ev.pc_full),
            ev.opcode,
            ev.sfr,
        )?;
        for r in ev.r {
            write!(f, ",${r:04X}")?;
        }
        writeln!(f)
    })
}

/// Write DMA→VRAM transfer-time trace events as CSV. Columns:
/// `seq,frame,line,blank,force_blank,src,vram_word,reg,value` — `blank` is the
/// V-blank flag, `force_blank` INIDISP (`$2100`) bit 7 at the write (safe iff
/// `blank||force_blank`), `src` the 24-bit A-bus source (`bank:offset`),
/// `vram_word` the VMADD word the byte landed at, `reg` the B-bus port ($2118
/// low / $2119 high), `value` the transferred byte.
pub(crate) fn write_dma_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::DmaTraceEvent],
) -> std::io::Result<()> {
    // This tracer is VRAM-focused (the double-buffer / per-VBlank budget
    // check): the broad B-bus capture now also records OAM/CGRAM/etc. DMA
    // writes for the Event Viewer, so filter to the VRAM data ports here.
    let vram: Vec<luna_api::DmaTraceEvent> = events
        .iter()
        .copied()
        .filter(|ev| matches!(ev.b_offset, 0x18 | 0x19))
        .collect();
    write_csv(
        path,
        "seq,frame,line,blank,force_blank,src,vram_word,reg,value",
        &vram,
        |f, i, ev| {
            writeln!(
                f,
                "{},{},{},{},{},{},${:04X},${:02X},${:02X}",
                i,
                ev.frame,
                ev.line,
                u8::from(ev.blank),
                u8::from(ev.force_blank),
                fmt_pc(ev.src_full),
                ev.vram_word,
                ev.b_offset,
                ev.value,
            )
        },
    )
}

/// Write per-instruction CPU trace events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc, a, x, y, sp, p_hex, db, dp, e`.
pub(crate) fn write_cpu_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::CpuTraceEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,a,x,y,sp,p,db,dp,e",
        events,
        |f, _, ev| {
            writeln!(
                f,
                "{},{},{},${:04X},${:04X},${:04X},${:04X},${:02X},${:02X},${:04X},{}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                ev.a,
                ev.x,
                ev.y,
                ev.sp,
                ev.p,
                ev.db,
                ev.dp,
                ev.e as u8
            )
        },
    )
}

/// Write per-access memory trace events as CSV. Columns:
/// `mclk_total, frame_ntsc, pc, addr, kind, value_hex`.
pub(crate) fn write_mem_trace_csv(
    path: &std::path::Path,
    events: &[luna_api::MemTraceEvent],
) -> std::io::Result<()> {
    write_csv(
        path,
        "mclk_total,frame_ntsc,pc,addr,kind,value,line,hclock,blank,force_blank",
        events,
        |f, _, ev| {
            let kind = match ev.kind {
                luna_api::MemEventKind::Read => "R",
                luna_api::MemEventKind::Write => "W",
                // Synthetic delivery-timing markers (P0 harness).
                luna_api::MemEventKind::NmiSignal => "N",
                luna_api::MemEventKind::IrqSignal => "I",
            };
            writeln!(
                f,
                "{},{},{},{},{},${:02X},{},{},{},{}",
                ev.mclk_total,
                ev.mclk_total / NTSC_MCLK_PER_FRAME,
                fmt_pc(ev.pc_full),
                fmt_pc(ev.addr_full),
                kind,
                ev.value,
                ev.line,
                ev.hclock,
                u8::from(ev.blank),
                u8::from(ev.force_blank),
            )
        },
    )
}
