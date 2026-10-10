//! `luna profile`, frame by frame (issue #270): the series of completed
//! frames, the heaviest ones with what ran in them, and the gates a game
//! that must never drop a tick puts in CI.

use std::fmt::Write as _;

use luna_api::{ProfileFrame, ProfileWorstFrame};

/// The `--max-*` gates over the frame series.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameGates {
    /// `--max-frame-mclk`: no frame may use more active master cycles.
    pub frame_mclk: Option<u64>,
    /// `--max-lag-frames`: at most this many lag frames in the window.
    pub lag_frames: Option<u64>,
    /// `--max-lag-run`: at most this many lag frames in a row.
    pub lag_run: Option<u64>,
}

/// The outcome of one frame gate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct GateVerdict {
    /// The option, without its dashes.
    gate: &'static str,
    limit: u64,
    /// What the window measured.
    value: u64,
    /// The frame that measured it (the first one of a run).
    frame: Option<u64>,
    ok: bool,
}

/// The frame series in a few numbers, plus the gate verdicts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FrameSummary {
    /// Completed frames in the window.
    frames: u64,
    /// Mean `active_mclk` over them.
    active_mean: u64,
    /// The heaviest frame's `active_mclk`…
    active_max: u64,
    /// …and which frame it was (the first one, on a tie).
    active_max_frame: Option<u64>,
    /// Mean `total_mclk`: what one frame lasts.
    total_mean: u64,
    /// Frames whose NMI found the CPU executing.
    lag_frames: u64,
    /// The longest run of consecutive lag frames…
    lag_run: u64,
    /// …and its first frame.
    lag_run_frame: Option<u64>,
    /// Every run of two or more lag frames in a row, in order — two is
    /// where a tick that may take two frames has taken a third. "How
    /// many, and how regular" without a pass over the series.
    lag_runs: Vec<LagRun>,
    /// One verdict per gate asked for.
    gates: Vec<GateVerdict>,
}

/// One run of consecutive lag frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct LagRun {
    /// Its first frame.
    frame: u64,
    /// How many lag frames in a row.
    length: u64,
}

impl FrameSummary {
    /// No gate failed.
    pub(crate) fn ok(&self) -> bool {
        self.gates.iter().all(|g| g.ok)
    }
}

/// Fold the series into its summary and judge the gates.
pub(crate) fn summarize(series: &[ProfileFrame], gates: &FrameGates) -> FrameSummary {
    let frames = series.len() as u64;
    let heaviest = series.iter().max_by(|a, b| {
        a.active_mclk
            .cmp(&b.active_mclk)
            .then(b.frame.cmp(&a.frame))
    });
    let mean = |f: fn(&ProfileFrame) -> u64| series.iter().map(f).sum::<u64>() / frames.max(1);
    let (mut lag_run, mut lag_run_frame) = (0u64, None);
    let (mut run, mut run_start) = (0u64, 0u64);
    let mut lag_runs: Vec<LagRun> = Vec::new();
    for f in series {
        if f.lag {
            if run == 0 {
                run_start = f.frame;
            }
            run += 1;
            if run > lag_run {
                lag_run = run;
                lag_run_frame = Some(run_start);
            }
            match lag_runs.last_mut().filter(|r| r.frame == run_start) {
                Some(r) => r.length = run,
                None if run == 2 => lag_runs.push(LagRun {
                    frame: run_start,
                    length: run,
                }),
                None => {}
            }
        } else {
            run = 0;
        }
    }
    let lag_frames = series.iter().filter(|f| f.lag).count() as u64;
    let active_max = heaviest.map_or(0, |f| f.active_mclk);
    let active_max_frame = heaviest.map(|f| f.frame);
    let first_lag = series.iter().find(|f| f.lag).map(|f| f.frame);
    let verdict = |gate, limit: Option<u64>, value: u64, frame| {
        limit.map(|limit| GateVerdict {
            gate,
            limit,
            value,
            frame,
            ok: value <= limit,
        })
    };
    let gates = [
        verdict(
            "max-frame-mclk",
            gates.frame_mclk,
            active_max,
            active_max_frame,
        ),
        verdict("max-lag-frames", gates.lag_frames, lag_frames, first_lag),
        verdict("max-lag-run", gates.lag_run, lag_run, lag_run_frame),
    ]
    .into_iter()
    .flatten()
    .collect();
    FrameSummary {
        frames,
        active_mean: mean(|f| f.active_mclk),
        active_max,
        active_max_frame,
        total_mean: mean(|f| f.total_mclk),
        lag_frames,
        lag_run,
        lag_run_frame,
        lag_runs,
        gates,
    }
}

/// How many starts of lag runs the summary line lists before `…`.
const LAG_RUNS_SHOWN: usize = 8;

/// Print the summary line and one line per gate.
pub(crate) fn print_summary(s: &FrameSummary) {
    if s.frames == 0 {
        println!("frames: none completed in the window");
    } else {
        let lag = match s.lag_run_frame {
            Some(frame) => format!(
                "{} lag frame(s), longest run {} (from frame {frame})",
                s.lag_frames, s.lag_run
            ),
            None => "no lag frame".to_string(),
        };
        // No `%` on this line: a harness that picks the table's rows by it
        // must not pick this one.
        println!(
            "frames: {} completed, active mean {} mclk of {} a frame, max {} (frame {}); {lag}",
            s.frames,
            s.active_mean,
            s.total_mean,
            s.active_max,
            s.active_max_frame.unwrap_or(0),
        );
    }
    if !s.lag_runs.is_empty() {
        let starts: Vec<String> = s
            .lag_runs
            .iter()
            .take(LAG_RUNS_SHOWN)
            .map(|r| r.frame.to_string())
            .collect();
        println!(
            "lag runs of 2 or more: {} (from frame {}{})",
            s.lag_runs.len(),
            starts.join(", "),
            if s.lag_runs.len() > LAG_RUNS_SHOWN {
                ", …"
            } else {
                ""
            }
        );
    }
    for g in &s.gates {
        let at = g
            .frame
            .map_or_else(String::new, |f| format!(" (frame {f})"));
        println!(
            "gate: {} {}{at} {} {} — {}",
            g.gate,
            g.value,
            if g.ok { "<=" } else { ">" },
            g.limit,
            if g.ok { "ok" } else { "OVER" }
        );
    }
}

/// Print each of the heaviest frames with its `top` costliest symbols.
pub(crate) fn print_worst(worst: &[ProfileWorstFrame], top: usize) {
    for w in worst {
        let t = &w.time;
        println!(
            "worst frame {}: active {} mclk (cpu {}, dma {}), idle {}, hdma {}{}",
            t.frame,
            t.active_mclk,
            t.cpu_mclk,
            t.dma_mclk,
            t.idle_mclk,
            t.hdma_mclk,
            if t.lag { ", lag" } else { "" }
        );
        for e in w.entries.iter().take(top) {
            println!(
                "{:>6.2}%  {:>14}  {}",
                e.mclk as f64 * 100.0 / t.total_mclk.max(1) as f64,
                e.mclk,
                e.symbol
            );
        }
        if w.entries.len() > top {
            println!(
                "… {} more (raise --top or read --out)",
                w.entries.len() - top
            );
        }
    }
}

/// The `--frames-out` CSV: one row per completed frame.
pub(crate) fn csv(series: &[ProfileFrame]) -> String {
    let mut out = String::from(
        "frame,active_mclk,idle_mclk,cpu_mclk,dma_mclk,hdma_mclk,refresh_mclk,total_mclk,nmi,lag\n",
    );
    for f in series {
        let _ = writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{}",
            f.frame,
            f.active_mclk,
            f.idle_mclk,
            f.cpu_mclk,
            f.dma_mclk,
            f.hdma_mclk,
            f.refresh_mclk,
            f.total_mclk,
            u8::from(f.nmi),
            u8::from(f.lag)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(frame: u64, active: u64, lag: bool) -> ProfileFrame {
        ProfileFrame {
            frame,
            active_mclk: active,
            idle_mclk: 1000 - active,
            cpu_mclk: active,
            dma_mclk: 0,
            hdma_mclk: 0,
            refresh_mclk: 0,
            total_mclk: 1000,
            nmi: true,
            lag,
        }
    }

    #[test]
    fn the_summary_finds_the_heaviest_frame_and_the_longest_lag_run() {
        // A tick over two frames lags once; frames 14..=15 are the tick
        // that spilled into a third.
        let series = [
            frame(10, 900, true),
            frame(11, 300, false),
            frame(12, 950, true),
            frame(13, 200, false),
            frame(14, 1000, true),
            frame(15, 1000, true),
            frame(16, 100, false),
        ];
        let s = summarize(&series, &FrameGates::default());
        assert_eq!(s.frames, 7);
        assert_eq!((s.active_max, s.active_max_frame), (1000, Some(14)));
        assert_eq!(s.lag_frames, 4);
        assert_eq!((s.lag_run, s.lag_run_frame), (2, Some(14)));
        assert_eq!(
            s.lag_runs,
            [LagRun {
                frame: 14,
                length: 2
            }],
            "single lag frames are not runs"
        );
        assert!(s.ok(), "no gate asked, none failed");
    }

    #[test]
    fn a_run_gate_tells_a_two_frame_tick_from_a_three_frame_one() {
        let ok = [
            frame(1, 900, true),
            frame(2, 300, false),
            frame(3, 900, true),
        ];
        let over = [
            frame(1, 900, true),
            frame(2, 900, true),
            frame(3, 300, false),
        ];
        let gates = FrameGates {
            lag_run: Some(1),
            ..FrameGates::default()
        };
        assert!(summarize(&ok, &gates).ok());
        let s = summarize(&over, &gates);
        assert!(!s.ok());
        assert_eq!(
            s.gates,
            vec![GateVerdict {
                gate: "max-lag-run",
                limit: 1,
                value: 2,
                frame: Some(1),
                ok: false
            }]
        );
    }

    #[test]
    fn the_other_gates_compare_the_heaviest_frame_and_the_lag_count() {
        let series = [frame(1, 900, true), frame(2, 300, false)];
        let gates = FrameGates {
            frame_mclk: Some(899),
            lag_frames: Some(1),
            lag_run: None,
        };
        let s = summarize(&series, &gates);
        assert_eq!(s.gates.len(), 2);
        assert!(!s.gates[0].ok && s.gates[0].frame == Some(1));
        assert!(s.gates[1].ok);
    }

    #[test]
    fn an_empty_window_passes_and_the_csv_has_one_row_per_frame() {
        let gates = FrameGates {
            lag_run: Some(0),
            ..FrameGates::default()
        };
        assert!(summarize(&[], &gates).ok());
        let text = csv(&[frame(7, 250, true)]);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "frame,active_mclk,idle_mclk,cpu_mclk,dma_mclk,hdma_mclk,refresh_mclk,total_mclk,nmi,lag",
                "7,250,750,250,0,0,0,1000,1,1"
            ]
        );
    }
}
