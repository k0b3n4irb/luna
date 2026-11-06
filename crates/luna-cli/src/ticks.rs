//! `luna ticks`: the per-tick record of `luna-api`
//! ([`luna_api::Emulator::record_ticks`]) from a TOML spec, from power-on.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

/// `luna ticks --schema`: the JSON Schema of each kind of line.
pub(crate) fn schema_json() -> String {
    serde_json::to_string_pretty(&luna_api::tick_record_schema()).expect("schema serialises")
}

/// `luna ticks` options.
pub(crate) struct TicksOptions<'a> {
    pub spec: &'a Path,
    pub out: &'a Path,
    pub shots_dir: Option<&'a Path>,
    pub audio_out: Option<&'a Path>,
    pub sym: Option<&'a Path>,
    pub force_mapper: Option<&'a str>,
    pub force_region: Option<&'a str>,
    pub power_on: Option<&'a str>,
}

/// `luna ticks`: exit 0 when the record is complete, 1 when the run fell
/// short of it, 2 when the spec is refused (nothing is run).
pub(crate) fn run_ticks(rom: &Path, o: &TicksOptions<'_>) -> ExitCode {
    match record(rom, o) {
        Ok(outcome) => {
            for f in &outcome.failures {
                eprintln!("error: {f}");
            }
            eprintln!("{} tick(s) recorded ({})", outcome.ticks, outcome.reason);
            if outcome.complete() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

/// Everything that refuses the spec returns `Err` (exit 2), before the
/// record file is created.
fn record(rom: &Path, o: &TicksOptions<'_>) -> Result<luna_api::TickOutcome, String> {
    let text = std::fs::read_to_string(o.spec)
        .map_err(|e| format!("reading {}: {e}", o.spec.display()))?;
    let spec: luna_api::TickSpec =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", o.spec.display()))?;
    let to_stdout = o.out == Path::new("-");
    let default_shots = o.out.with_extension("shots");
    let shots_dir = match (o.shots_dir, &spec.shots) {
        (Some(d), _) => Some(d),
        (None, Some(_)) if to_stdout => {
            return Err("[shots] with --out -: give --shots-dir".to_string());
        }
        (None, Some(_)) => Some(default_shots.as_path()),
        (None, None) => None,
    };

    let mut em = luna_api::Emulator::new();
    crate::rom::load_rom_into(
        &mut em,
        rom,
        o.force_mapper,
        o.force_region,
        None,
        o.power_on,
    )?;
    if let Some(sym) = o.sym {
        em.load_symbols(sym)
            .map_err(|e| format!("--sym {}: {e}", sym.display()))?;
    }
    let options = luna_api::TickRecordOptions {
        base_dir: o.spec.parent().unwrap_or_else(|| Path::new(".")),
        shots_dir,
        audio_out: o.audio_out,
    };
    // The record goes to a buffer until the spec is accepted, so a refused
    // spec leaves no file behind; then straight to its destination.
    let mut out: Box<dyn Write> = if to_stdout {
        Box::new(std::io::BufWriter::new(std::io::stdout()))
    } else {
        Box::new(Deferred {
            path: o.out,
            file: None,
        })
    };
    em.record_ticks(&spec, &options, &mut out)
}

/// A file created at its first write.
struct Deferred<'a> {
    path: &'a Path,
    file: Option<std::io::BufWriter<std::fs::File>>,
}

impl Write for Deferred<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.file.is_none() {
            self.file = Some(std::io::BufWriter::new(std::fs::File::create(self.path)?));
        }
        self.file.as_mut().expect("just created").write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

/// `luna ticks --compare A B`: exit 0 when the records are identical, 1
/// when they diverge, 2 when they cannot be compared.
pub(crate) fn run_compare(a: &Path, b: &Path, csv: Option<&Path>, zones: &[String]) -> ExitCode {
    match compare(a, b, csv, zones) {
        Ok(c) => {
            eprintln!("{}", c.summary());
            if c.identical {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn open(path: &Path) -> Result<Box<dyn std::io::BufRead>, String> {
    if path == Path::new("-") {
        return Ok(Box::new(std::io::BufReader::new(std::io::stdin())));
    }
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Box::new(std::io::BufReader::new(f)))
}

fn compare(
    a: &Path,
    b: &Path,
    csv: Option<&Path>,
    zones: &[String],
) -> Result<luna_api::TickComparison, String> {
    let c = luna_api::compare_tick_records(
        open(a)?,
        open(b)?,
        &luna_api::CompareOptions {
            zones: zones.to_vec(),
        },
    )?;
    if let Some(path) = csv {
        let mut out: Box<dyn Write> = if path == Path::new("-") {
            Box::new(std::io::stdout())
        } else {
            Box::new(std::io::BufWriter::new(
                std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?,
            ))
        };
        c.write_csv(&mut out)
            .and_then(|()| out.flush())
            .map_err(|e| format!("writing the CSV: {e}"))?;
    }
    Ok(c)
}

/// `luna ticks --intervals RECORD`: the duration of each tick. Exit 0 when
/// measured, 2 when the record cannot be measured.
pub(crate) fn run_intervals(
    record: &Path,
    frame_cycles: Option<u64>,
    cycle_hz: Option<u64>,
    over: Option<f64>,
    csv: Option<&Path>,
) -> ExitCode {
    match intervals(record, frame_cycles, cycle_hz, over, csv) {
        Ok(i) => {
            println!("{}", i.summary());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn intervals(
    record: &Path,
    frame_cycles: Option<u64>,
    cycle_hz: Option<u64>,
    over: Option<f64>,
    csv: Option<&Path>,
) -> Result<luna_api::TickIntervals, String> {
    let i = luna_api::tick_intervals(
        open(record)?,
        &luna_api::IntervalOptions {
            frame_cycles,
            cycle_hz,
            over,
        },
    )?;
    if let Some(path) = csv {
        // Beside the summary on stdout, `-` would mix the two: the CSV goes
        // to a file.
        if path == Path::new("-") {
            return Err("--csv -: the summary is on stdout, give a file".to_string());
        }
        let mut out = std::io::BufWriter::new(
            std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?,
        );
        i.write_csv(&mut out)
            .and_then(|()| out.flush())
            .map_err(|e| format!("writing the CSV: {e}"))?;
    }
    Ok(i)
}
