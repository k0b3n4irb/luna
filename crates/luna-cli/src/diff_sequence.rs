//! `luna diff --sequence` — same pictures, another cadence?
//!
//! `luna diff --frames F --tolerance N` asks whether B shows at `F ± N`
//! what A shows at `F`. Two benign changes escape it: a boot that moved
//! by more than the tolerance, and a free-running loop that now fits in a
//! frame more often, so that no single offset lines the two ROMs up.
//!
//! This comparison ignores time. Every frame of a range is hashed on both
//! machines, each run of identical frames is collapsed into one
//! *picture*, and the longest run of pictures the two ROMs show in the
//! same order is measured. Most of the shorter sequence found in the
//! other = the same animation at another cadence or offset; a common run
//! of one picture = different pictures. Specified by `OpenSNES`'s prototype
//! (report of 2026-10-08).

use std::process::ExitCode;

use crate::diff::{DiffOptions, Machine, drive};
use crate::output::write_json_report;
use crate::parsers::{InputFlags, input_script};

/// The share of the shorter sequence the common run must reach when the
/// caller names no threshold.
const DEFAULT_MIN_COMMON_PCT: f64 = 90.0;

/// The threshold a common run is judged against.
#[derive(Clone, Copy)]
pub(crate) enum MinCommon {
    /// At least this many pictures.
    Pictures(usize),
    /// At least this percentage of the pictures of the ROM that shows
    /// fewer of them.
    Percent(f64),
}

/// A run of identical frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Picture {
    hash: u64,
    /// The first frame showing it.
    first_frame: u64,
    /// How many frames in a row show it.
    frames: u64,
}

/// Collapse the hashes of consecutive frames, starting at frame `from`,
/// into pictures.
fn pictures(hashes: &[u64], from: u64) -> Vec<Picture> {
    let mut out: Vec<Picture> = Vec::new();
    for (frame, &hash) in (from..).zip(hashes) {
        match out.last_mut() {
            Some(last) if last.hash == hash => last.frames += 1,
            _ => out.push(Picture {
                hash,
                first_frame: frame,
                frames: 1,
            }),
        }
    }
    out
}

/// The distinct durations, in frames, of the pictures of a sequence. The
/// first and the last are cut by the range, so they are left out.
fn durations(p: &[Picture]) -> Vec<u64> {
    let inner = p.get(1..p.len().saturating_sub(1)).unwrap_or_default();
    let mut d: Vec<u64> = inner.iter().map(|p| p.frames).collect();
    d.sort_unstable();
    d.dedup();
    d
}

/// The longest run of pictures both sequences show in the same order, as
/// `(length, index in a, index in b)`; the earliest in A, then in B, when
/// several are as long. `(0, 0, 0)` when they share no picture.
fn longest_common_run(a: &[Picture], b: &[Picture]) -> (usize, usize, usize) {
    // run[j + 1] = length of the common run ending at a[i], b[j].
    let mut run = vec![0usize; b.len() + 1];
    let mut best = (0, 0, 0);
    for (i, pa) in a.iter().enumerate() {
        // Right to left, so run[j] still holds the previous row's value.
        for (j, pb) in b.iter().enumerate().rev() {
            run[j + 1] = if pa.hash == pb.hash { run[j] + 1 } else { 0 };
            let len = run[j + 1];
            let start = (i + 1 - len, j + 1 - len);
            if len > best.0 || (len == best.0 && len > 0 && start < (best.1, best.2)) {
                best = (len, start.0, start.1);
            }
        }
    }
    best
}

/// One ROM's side of the report.
#[derive(serde::Serialize)]
struct Side {
    pictures: usize,
    /// Distinct picture durations in frames, first and last picture
    /// excluded.
    frames_per_picture: Vec<u64>,
}

/// The longest common run.
#[derive(serde::Serialize)]
struct CommonRun {
    pictures: usize,
    /// The frame it starts at in each ROM (`null` when there is none).
    a_frame: Option<u64>,
    b_frame: Option<u64>,
    /// `b_frame - a_frame`.
    offset: Option<i64>,
    /// Its length as a percentage of the shorter sequence.
    pct_of_shorter: f64,
}

/// The `--out` JSON report.
#[derive(serde::Serialize)]
struct Report<'a> {
    a: &'a std::path::Path,
    b: &'a std::path::Path,
    from: u64,
    to: u64,
    side_a: Side,
    side_b: Side,
    common_run: CommonRun,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_common: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_common_pct: Option<f64>,
    /// `"same-sequence"` or `"diff"`.
    status: &'static str,
}

/// Run `rom` to frame `to` and return its hash at every frame of
/// `from..=to`.
fn hashes(
    rom: &std::path::Path,
    o: &DiffOptions<'_>,
    script: luna_api::InputScript,
    (from, to): (u64, u64),
) -> Result<Vec<u64>, String> {
    let mut m = Machine::load(rom, o)?;
    m.script = script;
    drive(&mut m, o, &[], false, to, true)?;
    (from..=to)
        .map(|f| {
            m.hashes
                .get(&f)
                .copied()
                .ok_or_else(|| format!("{}: the machine stopped before frame {f}", rom.display()))
        })
        .collect()
}

/// `luna diff --sequence` entry point.
pub(crate) fn run_sequence_diff(
    rom_a: &std::path::Path,
    rom_b: &std::path::Path,
    (from, to): (u64, u64),
    min_common: Option<MinCommon>,
    o: &DiffOptions<'_>,
) -> ExitCode {
    if from > to {
        eprintln!("error: --from {from} is past --to {to}");
        return ExitCode::from(2);
    }
    if let Some(MinCommon::Percent(p)) = min_common
        && !(p.is_finite() && (0.0..=100.0).contains(&p))
    {
        eprintln!("error: --min-common-pct must be a percentage from 0 to 100");
        return ExitCode::from(2);
    }
    let script = match input_script(&InputFlags::pad1(o.input_script)) {
        Ok(script) => script,
        Err(code) => return ExitCode::from(code),
    };
    let range = (from, to);
    let (ha, hb) = match (
        hashes(rom_a, o, script.clone(), range),
        hashes(rom_b, o, script, range),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    let (pa, pb) = (pictures(&ha, from), pictures(&hb, from));
    let (len, ia, ib) = longest_common_run(&pa, &pb);
    let shorter = pa.len().min(pb.len());
    let pct = len as f64 * 100.0 / shorter.max(1) as f64;
    let threshold = min_common.unwrap_or(MinCommon::Percent(DEFAULT_MIN_COMMON_PCT));
    let (same, asked) = match threshold {
        MinCommon::Pictures(n) => (len >= n, format!("at least {n} asked")),
        MinCommon::Percent(p) => (pct >= p, format!("at least {p}% asked")),
    };
    let status = if same { "same-sequence" } else { "diff" };

    let show = |d: &[u64]| d.iter().map(u64::to_string).collect::<Vec<_>>().join(", ");
    let (da, db) = (durations(&pa), durations(&pb));
    println!("frames {from}-{to}");
    println!(
        "A: {} pictures, frames per picture [{}]",
        pa.len(),
        show(&da)
    );
    println!(
        "B: {} pictures, frames per picture [{}]",
        pb.len(),
        show(&db)
    );
    let (a_frame, b_frame) = if len == 0 {
        println!("longest common run: 0 pictures");
        (None, None)
    } else {
        let (fa, fb) = (pa[ia].first_frame, pb[ib].first_frame);
        println!(
            "longest common run: {len} pictures in the same order \
             (from frame {fa} in A, frame {fb} in B, offset {:+})",
            fb as i64 - fa as i64
        );
        (Some(fa), Some(fb))
    };
    println!(
        "{len} of {shorter} pictures ({pct:.1}% of the shorter sequence, {asked}): {}",
        status.to_ascii_uppercase()
    );

    if let Some(path) = o.out {
        let report = Report {
            a: rom_a,
            b: rom_b,
            from,
            to,
            side_a: Side {
                pictures: pa.len(),
                frames_per_picture: da,
            },
            side_b: Side {
                pictures: pb.len(),
                frames_per_picture: db,
            },
            common_run: CommonRun {
                pictures: len,
                a_frame,
                b_frame,
                offset: a_frame.zip(b_frame).map(|(a, b)| b as i64 - a as i64),
                pct_of_shorter: pct,
            },
            min_common: match threshold {
                MinCommon::Pictures(n) => Some(n),
                MinCommon::Percent(_) => None,
            },
            min_common_pct: match threshold {
                MinCommon::Pictures(_) => None,
                MinCommon::Percent(p) => Some(p),
            },
            status,
        };
        if let Err(code) = write_json_report(path, &report) {
            return code;
        }
    }
    if same {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(hashes: &[u64]) -> Vec<Picture> {
        pictures(hashes, 1)
    }

    #[test]
    fn identical_frames_collapse_into_one_picture() {
        let p = pictures(&[7, 7, 7, 9, 7, 7], 10);
        assert_eq!(
            p,
            [
                Picture {
                    hash: 7,
                    first_frame: 10,
                    frames: 3
                },
                Picture {
                    hash: 9,
                    first_frame: 13,
                    frames: 1
                },
                Picture {
                    hash: 7,
                    first_frame: 14,
                    frames: 2
                },
            ]
        );
        assert!(pictures(&[], 1).is_empty());
    }

    #[test]
    fn the_durations_leave_out_the_two_pictures_the_range_cuts() {
        // 1 frame, then 2, 2, 1, then 3 at the end of the range.
        let p = seq(&[1, 2, 2, 3, 3, 4, 5, 5, 5]);
        assert_eq!(durations(&p), [1, 2]);
        assert!(durations(&seq(&[1, 1, 2])).is_empty());
    }

    #[test]
    fn the_same_animation_at_another_cadence_is_one_long_common_run() {
        // A shows each picture for two frames, B for one, after a boot
        // screen (0) of different lengths.
        let a = seq(&[0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4]);
        let b = seq(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(longest_common_run(&a, &b), (5, 0, 0));
        assert_eq!((a[0].first_frame, b[0].first_frame), (1, 1));
    }

    #[test]
    fn a_later_boot_moves_the_start_of_the_run() {
        let a = seq(&[9, 1, 2, 3, 4, 5]);
        let b = seq(&[8, 8, 8, 1, 2, 3, 4, 5]);
        let (len, ia, ib) = longest_common_run(&a, &b);
        assert_eq!(len, 5);
        assert_eq!((a[ia].first_frame, b[ib].first_frame), (2, 4));
    }

    #[test]
    fn a_run_must_be_in_the_same_order_and_unbroken() {
        let a = seq(&[1, 2, 3, 4]);
        assert_eq!(longest_common_run(&a, &seq(&[4, 3, 2, 1])).0, 1);
        assert_eq!(longest_common_run(&a, &seq(&[1, 2, 9, 3, 4])).0, 2);
        assert_eq!(longest_common_run(&a, &seq(&[5, 6, 7])), (0, 0, 0));
        assert_eq!(longest_common_run(&a, &[]), (0, 0, 0));
    }

    #[test]
    fn of_two_runs_as_long_the_earliest_in_a_is_reported() {
        let a = seq(&[1, 2, 9, 1, 2]);
        let b = seq(&[7, 1, 2]);
        assert_eq!(longest_common_run(&a, &b), (2, 0, 1));
    }
}
