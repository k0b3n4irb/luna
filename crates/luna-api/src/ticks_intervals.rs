//! How long each tick lasted ([`tick_intervals`]): the distance between
//! two consecutive arrivals on the tick routine, read from a `tick-record`
//! file in frames and milliseconds.
//!
//! This is NOT what `luna profile` calls a lag frame (a frame whose NMI
//! found the CPU executing): a tick of two frames lags every other frame
//! by design. The duration of a tick is measured here, from the record's
//! own `cycles` clock. Nothing here needs an emulator: the record may have
//! been written by luna or by any other program that writes the format.
//!
//! A cycle count only becomes frames with the length of a frame in those
//! cycles. The header names the unit (`clock.cycles`) and a rate
//! (`clock.hz`); a record of luna (`snes_master`) says enough to derive
//! the frame length, any other unit is refused unless the caller gives
//! `frame_cycles` itself.

use std::fmt::Write as _;
use std::io::{BufRead, Write};

use crate::TICK_RECORD_VERSION;

/// The master clock of an NTSC SNES, as the header writes it, and the
/// master cycles of one of its frames on average (262 lines of 1364, minus
/// the short line of every other frame).
const SNES_NTSC: (u64, u64) = (21_477_272, 357_366);
/// The same for PAL: 312 lines of 1364.
const SNES_PAL: (u64, u64) = (21_281_370, 425_568);

/// What to measure.
#[derive(Debug, Clone, Default)]
pub struct IntervalOptions {
    /// Cycles of one frame, in the record's `cycles` unit. Required unless
    /// the record is luna's (`snes_master`, a known `hz`).
    pub frame_cycles: Option<u64>,
    /// Cycles per second of the record's `cycles` unit, for the
    /// milliseconds. Defaults to the header's `hz` for `snes_master`;
    /// otherwise the milliseconds are left out.
    pub cycle_hz: Option<u64>,
    /// List the ticks lasting more than this many frames (the fractional
    /// count, so `2.5` lists the ticks that spilled into a third frame).
    pub over: Option<f64>,
}

/// One tick: from its arrival to the next one.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct TickInterval {
    /// The tick that starts at this arrival.
    pub t: u64,
    /// Frame boundaries crossed (`frame` of the next arrival minus this
    /// one's): a whole number, off by one with the beam's phase.
    pub frame_delta: u64,
    /// Cycles to the next arrival, in the record's unit.
    pub cycles: u64,
    /// `cycles` over the frame length.
    pub frames: f64,
    /// `cycles` over the rate; absent when the record's rate is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<f64>,
}

/// The ticks above the limit.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct IntervalsOver {
    /// The limit, in frames.
    pub frames: f64,
    /// How many ticks last more.
    pub count: u64,
    /// Those ticks, in order.
    pub ticks: Vec<TickInterval>,
}

/// The verdict of [`tick_intervals`].
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct TickIntervals {
    /// The header's `clock.cycles`.
    pub cycles_unit: String,
    /// Cycles of one frame used to convert.
    pub frame_cycles: u64,
    /// Cycles per second used for the milliseconds, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_hz: Option<u64>,
    /// Tick lines read.
    pub ticks: u64,
    /// Ticks with a duration: every one but the last (it has no next
    /// arrival).
    pub measured: u64,
    /// Mean duration, in frames (0 when nothing was measured).
    pub mean_frames: f64,
    /// The longest tick.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<TickInterval>,
    /// The ticks above `over`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over: Option<IntervalsOver>,
    /// The record has no end line.
    pub truncated: bool,
    /// Every measured tick (the CSV's rows).
    #[serde(skip)]
    pub intervals: Vec<TickInterval>,
}

impl TickIntervals {
    /// A few lines saying what was found.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} tick(s), {} measured (arrival to arrival, NOT lag frames); frame = {} {} cycles",
            self.ticks, self.measured, self.frame_cycles, self.cycles_unit
        );
        if let Some(m) = &self.max {
            let _ = write!(
                s,
                "\nmean {:.3} frame(s); longest: tick {} with {:.3} frame(s){}",
                self.mean_frames,
                m.t,
                m.frames,
                m.ms.map_or_else(String::new, |ms| format!(" ({ms:.2} ms)"))
            );
        }
        if let Some(o) = &self.over {
            let _ = write!(
                s,
                "\n{} tick(s) over {} frame(s){}",
                o.count,
                o.frames,
                if o.count == 0 { "" } else { ":" }
            );
            for t in &o.ticks {
                let _ = write!(s, "\n  tick {}: {:.3} frame(s)", t.t, t.frames);
            }
        }
        if self.truncated {
            s.push_str("\n(no end line: the record was cut short)");
        }
        s
    }

    /// Every measured tick as CSV: `t,frame_delta,cycles,frames,ms,over`
    /// (`ms` empty when unknown; `over` is 1 when above the limit, 0 else,
    /// empty without a limit).
    pub fn write_csv(&self, out: &mut dyn Write) -> std::io::Result<()> {
        writeln!(out, "t,frame_delta,cycles,frames,ms,over")?;
        let limit = self.over.as_ref().map(|o| o.frames);
        for i in &self.intervals {
            writeln!(
                out,
                "{},{},{},{:.4},{},{}",
                i.t,
                i.frame_delta,
                i.cycles,
                i.frames,
                i.ms.map_or_else(String::new, |m| format!("{m:.3}")),
                limit.map_or_else(String::new, |l| u8::from(i.frames > l).to_string())
            )?;
        }
        Ok(())
    }
}

fn bad(line_no: usize, what: impl std::fmt::Display) -> String {
    format!("record, line {line_no}: {what}")
}

/// Measure the tick durations of a `tick-record` file. `Err` means they
/// cannot be measured (unreadable, not a version-1 `tick-record`, the
/// frame length unknown, a clock that goes backwards, a missing tick).
pub fn tick_intervals<R: BufRead>(
    reader: R,
    opts: &IntervalOptions,
) -> Result<TickIntervals, String> {
    if let Some(o) = opts.over
        && !(o.is_finite() && o >= 0.0)
    {
        return Err(format!("--over {o}: a number of frames, 0 or more"));
    }
    let mut lines = reader.lines();
    let first = lines
        .next()
        .ok_or("record: empty")?
        .map_err(|e| format!("record: {e}"))?;
    let h: serde_json::Value =
        serde_json::from_str(&first).map_err(|e| bad(1, format!("not JSON: {e}")))?;
    if h["format"] != "tick-record" {
        return Err(bad(1, "not a tick-record header (`format`)"));
    }
    if h["version"] != TICK_RECORD_VERSION {
        return Err(bad(
            1,
            format!(
                "version {} (this luna reads version {TICK_RECORD_VERSION})",
                h["version"]
            ),
        ));
    }
    let unit = h["clock"]["cycles"].as_str().unwrap_or("").to_string();
    let hz = h["clock"]["hz"].as_u64();
    let known = if unit == "snes_master" {
        [SNES_NTSC, SNES_PAL].into_iter().find(|k| Some(k.0) == hz)
    } else {
        None
    };
    let frame_cycles = match (opts.frame_cycles, known) {
        (Some(0), _) => return Err("--frame-cycles 0: a frame has cycles".to_string()),
        (Some(c), _) => c,
        (None, Some((_, c))) => c,
        (None, None) => {
            return Err(format!(
                "the header's cycles are `{}` at {} Hz: the frame length in that unit is not \
                 known, give --frame-cycles",
                if unit.is_empty() { "?" } else { &unit },
                hz.map_or_else(|| "?".to_string(), |h| h.to_string())
            ));
        }
    };
    let cycle_hz = opts
        .cycle_hz
        .or(if unit == "snes_master" { hz } else { None });

    let mut prev: Option<(u64, u64, u64)> = None; // (t, frame, cycles)
    let mut ticks = 0u64;
    let mut intervals: Vec<TickInterval> = Vec::new();
    let mut ended = false;
    for (n, line) in lines.enumerate() {
        let line_no = n + 2;
        let line = line.map_err(|e| bad(line_no, e))?;
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| bad(line_no, format!("not JSON: {e}")))?;
        if v.get("end").is_some() {
            ended = true;
            break;
        }
        let Some(t) = v["t"].as_u64() else {
            continue;
        };
        let (Some(frame), Some(cycles)) = (v["frame"].as_u64(), v["cycles"].as_u64()) else {
            return Err(bad(line_no, format!("tick {t}: no `frame` or `cycles`")));
        };
        ticks += 1;
        if let Some((pt, pf, pc)) = prev {
            if t != pt + 1 {
                return Err(bad(
                    line_no,
                    format!("tick {t} after tick {pt}: a tick is missing, the interval is unknown"),
                ));
            }
            if cycles < pc || frame < pf {
                return Err(bad(line_no, format!("tick {t}: the clock goes backwards")));
            }
            let dc = cycles - pc;
            let frames = dc as f64 / frame_cycles as f64;
            intervals.push(TickInterval {
                t: pt,
                frame_delta: frame - pf,
                cycles: dc,
                frames,
                ms: cycle_hz.map(|hz| dc as f64 * 1000.0 / hz as f64),
            });
        }
        prev = Some((t, frame, cycles));
    }
    let measured = intervals.len() as u64;
    let mean_frames = if intervals.is_empty() {
        0.0
    } else {
        intervals.iter().map(|i| i.frames).sum::<f64>() / intervals.len() as f64
    };
    let max = intervals
        .iter()
        .fold(None::<&TickInterval>, |m, i| match m {
            Some(b) if b.frames >= i.frames => Some(b),
            _ => Some(i),
        })
        .cloned();
    let over = opts.over.map(|limit| {
        let ticks: Vec<TickInterval> = intervals
            .iter()
            .filter(|i| i.frames > limit)
            .cloned()
            .collect();
        IntervalsOver {
            frames: limit,
            count: ticks.len() as u64,
            ticks,
        }
    });
    Ok(TickIntervals {
        cycles_unit: unit,
        frame_cycles,
        cycle_hz,
        ticks,
        measured,
        mean_frames,
        max,
        over,
        truncated: !ended,
        intervals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record whose tick `i` falls at `cycles[i]`.
    fn rec(unit: &str, hz: u64, cycles: &[u64], end: bool) -> String {
        let mut s = format!(
            r#"{{"format":"tick-record","version":1,"endian":"little","clock":{{"frame":"x","cycles":"{unit}","hz":{hz},"beam":"b"}},"zones":[]}}"#
        );
        s.push('\n');
        for (i, c) in cycles.iter().enumerate() {
            let _ = write!(
                s,
                r#"{{"t":{},"frame":{},"cycles":{c},"beam":[0,0],"z":{{}}}}"#,
                i + 1,
                c / 357_366
            );
            s.push('\n');
        }
        if end {
            let _ = write!(
                s,
                r#"{{"end":{{"ticks":{},"reason":"limit"}}}}"#,
                cycles.len()
            );
            s.push('\n');
        }
        s
    }

    fn run(s: &str, o: &IntervalOptions) -> Result<TickIntervals, String> {
        tick_intervals(s.as_bytes(), o)
    }

    #[test]
    fn a_luna_record_converts_with_the_header() {
        // Intervals of 2, 2 and 3 frames of 357 366 cycles.
        let f = 357_366;
        let r = rec(
            "snes_master",
            21_477_272,
            &[0, 2 * f, 4 * f, 7 * f, 7 * f + 10],
            true,
        );
        let i = run(
            &r,
            &IntervalOptions {
                over: Some(2.5),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((i.ticks, i.measured, i.frame_cycles), (5, 4, f));
        assert!(!i.truncated);
        let m = i.max.as_ref().unwrap();
        assert_eq!((m.t, m.frame_delta), (3, 3));
        assert!((m.frames - 3.0).abs() < 1e-9);
        // 3 frames at 21 477 272 Hz = 49.9 ms.
        assert!((m.ms.unwrap() - 49.915).abs() < 0.01);
        let o = i.over.as_ref().unwrap();
        assert_eq!((o.count, o.ticks[0].t), (1, 3));
        let mut csv = Vec::new();
        i.write_csv(&mut csv).unwrap();
        let csv = String::from_utf8(csv).unwrap();
        assert!(csv.starts_with("t,frame_delta,cycles,frames,ms,over\n1,2,714732,2.0000,"));
        assert!(csv.lines().nth(3).unwrap().ends_with(",1"));
    }

    #[test]
    fn pal_header_gives_the_pal_frame() {
        let r = rec("snes_master", 21_281_370, &[0, 425_568], true);
        assert_eq!(
            run(&r, &IntervalOptions::default()).unwrap().frame_cycles,
            425_568
        );
    }

    #[test]
    fn a_foreign_unit_needs_the_frame_length() {
        let r = rec("other_master", 28_375_160, &[0, 100, 300], false);
        let e = run(&r, &IntervalOptions::default()).unwrap_err();
        assert!(e.contains("give --frame-cycles"), "{e}");
        let i = run(
            &r,
            &IntervalOptions {
                frame_cycles: Some(100),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(i.cycles_unit, "other_master");
        assert!((i.max.unwrap().frames - 2.0).abs() < 1e-9);
        // No rate known for that unit: no milliseconds, unless given.
        assert!(i.intervals[0].ms.is_none() && i.truncated);
        let i = run(
            &r,
            &IntervalOptions {
                frame_cycles: Some(100),
                cycle_hz: Some(1000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!((i.intervals[1].ms.unwrap() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn refusals() {
        let ok = rec("snes_master", 21_477_272, &[0, 5], true);
        assert!(run("", &IntervalOptions::default()).is_err());
        assert!(run("{\"format\":\"x\"}\n", &IntervalOptions::default()).is_err());
        let o = IntervalOptions {
            frame_cycles: Some(0),
            ..Default::default()
        };
        assert!(run(&ok, &o).is_err());
        let o = IntervalOptions {
            over: Some(f64::NAN),
            ..Default::default()
        };
        assert!(run(&ok, &o).is_err());
        let back = rec("snes_master", 21_477_272, &[10, 5], true);
        assert!(
            run(&back, &IntervalOptions::default())
                .unwrap_err()
                .contains("backwards")
        );
        let gap = ok.replace("\"t\":2", "\"t\":4");
        assert!(
            run(&gap, &IntervalOptions::default())
                .unwrap_err()
                .contains("missing")
        );
        // Unknown rate: refused rather than guessed.
        let odd = rec("snes_master", 1, &[0, 5], true);
        assert!(run(&odd, &IntervalOptions::default()).is_err());
    }
}
