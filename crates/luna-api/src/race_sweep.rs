//! `luna race-sweep` ([`Emulator::race_sweep`]): search the interrupt races
//! of a routine by forcing an NMI (or an IRQ) before **each** instruction of
//! it, from one snapshot, and comparing the outcome with the run that was
//! not interrupted.
//!
//! The method, in the order the code does it:
//!
//! 1. The machine stands where the sweep starts (the caller warmed it up).
//!    A snapshot `S` is taken there.
//! 2. **Baseline**: from `S`, run to the end condition (`RaceEnd`: the N-th
//!    arrival on a routine, or a frame), reading the `compare` bytes there
//!    and noting every instruction address executed in `[from, to]` with the
//!    number of arrivals. The baseline is run a second time from `S`: a run
//!    that does not reproduce from its snapshot cannot be compared, and the
//!    sweep refuses rather than report noise.
//! 3. **Trials**: for each address `A` seen at least `hit` times, and each
//!    kind `K`: restore `S`, run to the `hit`-th arrival on `A`, force `K`
//!    there (before the instruction), run to the same end, compare.
//!
//! Verdict per trial: [`RaceVerdict::Identical`]; `Differs` (the bytes are
//! listed); `NotTaken` (the injection was refused: `NMITIMEN.7` clear, `I`
//! set; never counted as a pass); `NotReached` (the arrival never came);
//! `EndNotReached` (the end condition was not reached within the budget: a
//! hang or a lost wake-up, a finding in itself).
//!
//! What it proves and what it does not: it forces the interrupt on the
//! *instruction boundaries* of the routine, which is where a real NMI can
//! land (the CPU polls at the last cycle of an instruction). The injected
//! edge is synthetic (see [`Emulator::inject_interrupt`]), and a frame can
//! see it twice. A clean sweep says no boundary of the range loses the
//! invariants you compared, from this warm state; the end condition is the
//! one place a legitimate extra interrupt must not move.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::{ApiError, Emulator, InterruptKind};

/// The version of the JSON report; it changes when a field is renamed or
/// removed, not when one is added.
pub const RACE_SWEEP_SCHEMA: u32 = 1;

/// Frames the warm-up may run before it gives up on reaching its routine.
const WARM_MAX_FRAMES: u64 = 3600;

/// Where each trial (and the baseline) ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RaceEnd {
    /// The `hit`-th arrival on `address` (`pb << 16 | pc`, the bank-bit-7
    /// mirror included), counted from the snapshot: a machine that stands on
    /// it already counts that as the first. An arrival displaced by the
    /// injected interrupt is counted once, after the `RTI`.
    Pc {
        /// 24-bit address.
        address: u32,
        /// Which arrival (from 1).
        hit: u64,
    },
    /// The PPU frame counter reaches `frame` (the number `state` prints).
    Frame {
        /// Absolute frame number; must be after the snapshot's.
        frame: u64,
    },
}

/// A byte range that must be equal at the end of a trial and of the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RaceCompare {
    /// 24-bit address of the first byte (side-effect-free read).
    pub address: u32,
    /// Bytes to compare (at least 1, within the bank).
    pub count: u32,
}

/// What a sweep is asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RaceSweepSpec {
    /// First address of the swept range (24-bit, inclusive).
    pub from: u32,
    /// Last address of the swept range (inclusive).
    pub to: u32,
    /// Interrupts to force at each address.
    pub kinds: Vec<InterruptKind>,
    /// Inject at this arrival of each address (default 1); an address that
    /// the baseline reaches fewer times is not swept.
    pub hit: u64,
    /// End of each run.
    pub end: RaceEnd,
    /// Bytes that must be equal at the end.
    pub compare: Vec<RaceCompare>,
    /// Instruction budget of the baseline (and the cap of a trial's).
    pub max_steps: u64,
}

/// Where the sweep starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaceWarm {
    /// Where the machine stands.
    None,
    /// The `hit`-th arrival on a routine (a symbol, `BANK:OFFSET`, `+N`).
    Pc {
        /// Address key, as [`Emulator::resolve_address`] reads it.
        spec: String,
        /// Which arrival (from 1).
        hit: u64,
    },
    /// Run until this absolute frame number.
    Frame(u64),
}

/// A sweep as a front-end spells it: keys to resolve, not addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaceSweepRequest {
    /// Start of the range (address key).
    pub from: String,
    /// End of the range (address key, inclusive).
    pub to: String,
    /// Interrupts to force (empty = NMI).
    pub kinds: Vec<InterruptKind>,
    /// Arrival to inject at.
    pub hit: u64,
    /// End by arrival on this routine (address key) ...
    pub until_pc: Option<String>,
    /// ... its arrival, or
    pub until_hit: u64,
    /// ... end at this frame.
    pub until_frame: Option<u64>,
    /// `SPEC` or `SPEC:COUNT`, see [`Emulator::resolve_race_compare`].
    pub compare: Vec<String>,
    /// Instruction budget.
    pub max_steps: u64,
}

/// What the unperturbed run gave, and the snapshot every trial restores.
#[derive(Debug, Clone)]
pub struct RaceBaseline {
    /// The machine where the sweep starts (a save state).
    pub snapshot: Vec<u8>,
    /// The `compare` bytes at the end, in order.
    pub values: Vec<u8>,
    /// Instructions the baseline took.
    pub steps: u64,
    /// Instructions a trial may take before its end is declared lost.
    pub trial_budget: u64,
    /// The addresses to sweep (ascending).
    pub addresses: Vec<u32>,
}

impl RaceBaseline {
    /// The trials to run, in report order: each address, each kind.
    #[must_use]
    pub fn tasks(&self, spec: &RaceSweepSpec) -> Vec<(u32, InterruptKind)> {
        let kinds = distinct_kinds(&spec.kinds);
        self.addresses
            .iter()
            .flat_map(|&a| kinds.iter().map(move |&k| (a, k)))
            .collect()
    }
}

/// How a trial ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RaceVerdict {
    /// The compared bytes equal the baseline's.
    Identical,
    /// At least one compared byte differs.
    Differs,
    /// The injection was refused (the reason says why).
    NotTaken,
    /// The arrival to inject at never came.
    NotReached,
    /// The end condition was not reached within the budget.
    EndNotReached,
}

impl RaceVerdict {
    /// The word printed.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::Differs => "DIFFERS",
            Self::NotTaken => "not taken",
            Self::NotReached => "not reached",
            Self::EndNotReached => "END NOT REACHED",
        }
    }
}

/// One compared byte that differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct RaceDiff {
    /// 24-bit address.
    pub address: u32,
    /// `$BB:OOOO`.
    pub address_hex: String,
    /// Nearest symbol (`name` or `name+0xNN`), when a `.sym` is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The byte of the run without interruption.
    pub baseline: u8,
    /// The byte after the injection.
    pub injected: u8,
}

/// One interrupt forced before one address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct RaceTrial {
    /// Instruction the interrupt landed before.
    pub address: u32,
    /// `$BB:OOOO`.
    pub address_hex: String,
    /// Nearest symbol, when a `.sym` is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The interrupt forced.
    pub kind: InterruptKind,
    /// How it ended.
    pub verdict: RaceVerdict,
    /// Why, for a verdict other than `identical` and `differs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The bytes that differ (`differs`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diffs: Vec<RaceDiff>,
    /// Instructions the trial took.
    pub steps: u64,
}

/// A compared range, as reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct RaceCompareOut {
    /// 24-bit address.
    pub address: u32,
    /// `$BB:OOOO`.
    pub address_hex: String,
    /// Bytes.
    pub count: u32,
    /// Nearest symbol, when a `.sym` is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The bytes the baseline ended with, in hex (memory order): what every
    /// trial is compared to. Check it is the *right* value, not only the
    /// same one: a baseline that is itself wrong makes a race invisible.
    pub baseline_hex: String,
}

/// The figures of a sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct RaceSummary {
    /// Distinct instruction addresses swept.
    pub addresses_swept: usize,
    /// Trials run (addresses times kinds).
    pub trials: usize,
    /// Trials whose bytes equal the baseline's.
    pub identical: usize,
    /// Trials whose bytes differ.
    pub differs: usize,
    /// Trials whose injection was refused. Not a pass: nothing was tested.
    pub not_taken: usize,
    /// Trials whose arrival never came.
    pub not_reached: usize,
    /// Trials whose end was not reached.
    pub end_not_reached: usize,
    /// Addresses where at least one trial `differs` or lost its end.
    pub racy_addresses: usize,
}

/// A whole sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct RaceSweepReport {
    /// [`RACE_SWEEP_SCHEMA`].
    pub schema: u32,
    /// First address swept.
    pub from: u32,
    /// Last address swept.
    pub to: u32,
    /// Kinds forced.
    pub kinds: Vec<InterruptKind>,
    /// Arrival injected at.
    pub hit: u64,
    /// End of every run.
    pub end: RaceEnd,
    /// The compared ranges.
    pub compare: Vec<RaceCompareOut>,
    /// Instructions the baseline took.
    pub baseline_steps: u64,
    /// Instructions a trial could take.
    pub trial_budget: u64,
    /// The figures.
    pub summary: RaceSummary,
    /// Every trial, by address then kind.
    pub trials: Vec<RaceTrial>,
}

/// `$BB:OOOO`.
fn hex24(a: u32) -> String {
    format!("${:02X}:{:04X}", (a >> 16) & 0xFF, a & 0xFFFF)
}

const fn kind_rank(k: InterruptKind) -> u8 {
    match k {
        InterruptKind::Nmi => 0,
        InterruptKind::Irq => 1,
    }
}

/// The kinds, each once, NMI first; empty means NMI.
fn distinct_kinds(kinds: &[InterruptKind]) -> Vec<InterruptKind> {
    let mut out: Vec<InterruptKind> = Vec::new();
    for &k in kinds {
        if !out.contains(&k) {
            out.push(k);
        }
    }
    if out.is_empty() {
        out.push(InterruptKind::Nmi);
    }
    out.sort_by_key(|&k| kind_rank(k));
    out
}

/// Where a leg of a run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leg {
    Inject,
    End,
    Budget,
}

/// Arrival counters that live across the two legs of a trial.
#[derive(Default)]
struct LegState {
    /// Address and arrival to stop at for the injection.
    inject: Option<(u32, u64)>,
    inj_seen: u64,
    end_hits: u64,
}

/// The baseline's census of the swept range.
struct Census {
    from: u32,
    to: u32,
    /// Folded address -> (arrivals, the address as first executed).
    arrivals: BTreeMap<u32, (u64, u32)>,
}

/// An address with its bank-bit-7 mirror folded (not for WRAM nor `ExHiROM`).
const fn fold(a: u32, mirrored: bool) -> u32 {
    if mirrored && !matches!((a >> 16) & 0x7F, 0x7E | 0x7F) {
        a & 0x7F_FFFF
    } else {
        a
    }
}

impl Emulator {
    /// Resolve `SPEC` or `SPEC:COUNT` to a [`RaceCompare`]. `SPEC` is a
    /// loaded symbol, `BANK:OFFSET` in hex, or either with `+N` / `-N`; the
    /// count defaults to the size the `.sym` records for the symbol, else 1.
    /// A lone `XX:YYYY` (2 and 4 hex digits) is an address, so a count after
    /// it is written `BANK:OFFSET:COUNT`.
    pub fn resolve_race_compare(&self, text: &str) -> Result<RaceCompare, String> {
        let is_hex = |t: &str, n: usize| t.len() == n && t.bytes().all(|b| b.is_ascii_hexdigit());
        let (spec, count) = match text.rsplit_once(':') {
            Some((left, right))
                if !left.is_empty()
                    && !right.is_empty()
                    && right.bytes().all(|b| b.is_ascii_digit())
                    && (left.contains(':') || !is_hex(left, 2) || !is_hex(right, 4)) =>
            {
                let n = right
                    .parse::<u32>()
                    .ok()
                    .filter(|&n| n >= 1)
                    .ok_or_else(|| {
                        format!("compare `{text}`: the count after `:` counts from 1")
                    })?;
                (left, Some(n))
            }
            _ => (text, None),
        };
        let address = self
            .resolve_address(spec)
            .map_err(|e| format!("compare `{text}`: {e}"))?;
        let count = count
            .or_else(|| self.symbol_size(spec))
            .filter(|&n| n >= 1)
            .unwrap_or(1);
        Ok(RaceCompare { address, count })
    }

    /// Resolve a [`RaceSweepRequest`] to the [`RaceSweepSpec`] it means.
    pub fn race_sweep_spec(&self, req: &RaceSweepRequest) -> Result<RaceSweepSpec, ApiError> {
        let key = |what: &str, k: &str| {
            self.resolve_address(k)
                .map_err(|e| ApiError::BadArg(format!("{what} `{k}`: {e}")))
        };
        let end = match (&req.until_pc, req.until_frame) {
            (Some(pc), None) => RaceEnd::Pc {
                address: key("until-pc", pc)?,
                hit: req.until_hit.max(1),
            },
            (None, Some(frame)) => RaceEnd::Frame { frame },
            (Some(_), Some(_)) => {
                return Err(ApiError::BadArg(
                    "give the end once: until-pc or until-frame, not both".into(),
                ));
            }
            (None, None) => {
                return Err(ApiError::BadArg(
                    "no end condition: give until-pc or until-frame".into(),
                ));
            }
        };
        let compare = req
            .compare
            .iter()
            .map(|c| self.resolve_race_compare(c).map_err(ApiError::BadArg))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RaceSweepSpec {
            from: key("from", &req.from)?,
            to: key("to", &req.to)?,
            kinds: distinct_kinds(&req.kinds),
            hit: req.hit.max(1),
            end,
            compare,
            max_steps: req.max_steps,
        })
    }

    /// Bring the machine to where the sweep starts.
    pub fn race_sweep_warm(&mut self, warm: &RaceWarm) -> Result<(), ApiError> {
        match warm {
            RaceWarm::None => Ok(()),
            RaceWarm::Frame(target) => {
                while self.frame_count()? < *target {
                    self.step_until_frame(50_000_000)?;
                }
                Ok(())
            }
            RaceWarm::Pc { spec, hit } => {
                let pc = self
                    .resolve_address(spec)
                    .map_err(|e| ApiError::BadArg(format!("warm-until-pc `{spec}`: {e}")))?;
                let start = self.frame_count()?;
                let mut seen = 0u64;
                let mut skip = false;
                while self.frame_count()?.saturating_sub(start) < WARM_MAX_FRAMES {
                    match self.run_to_pc_or_frame(&[pc], 200_000, skip)? {
                        crate::PcStop::Pc(_) => {
                            seen += 1;
                            skip = true;
                            if seen == *hit {
                                return Ok(());
                            }
                        }
                        crate::PcStop::Frame | crate::PcStop::Budget => skip = false,
                    }
                }
                Err(ApiError::BadArg(format!(
                    "warm-until-pc `{spec}`: arrival {hit} not reached within \
                     {WARM_MAX_FRAMES} frames (reached {seen} time(s))"
                )))
            }
        }
    }

    /// One leg of a run, from where the machine stands: until the injection
    /// arrival (when `st.inject` is set), the end condition, or `budget`
    /// instructions. `census` notes the arrivals in a range (the baseline).
    fn race_leg(
        &mut self,
        end: &RaceEnd,
        st: &mut LegState,
        mut census: Option<&mut Census>,
        budget: u64,
    ) -> Result<(Leg, u64), ApiError> {
        let mirrored = self.rom_info.as_ref().is_none_or(|r| r.mapper != "ExHiRom");
        let run = self.run_core_with(
            false,
            budget,
            None,
            |snes, _| {
                if let RaceEnd::Frame { frame } = end
                    && snes.frame_count >= *frame
                {
                    return Some(Leg::End);
                }
                // The same arrival rule as `run_to_pc_or_frame`: a CPU
                // parked in `WAI`, or about to take an interrupt instead,
                // is not on the instruction its PC names.
                if snes.cpu.waiting || snes.cpu.pending_nmi || snes.cpu.pending_irq {
                    return None;
                }
                let cur = (u32::from(snes.cpu.pb) << 16) | u32::from(snes.cpu.pc);
                if let Some(c) = census.as_deref_mut() {
                    let k = fold(cur, mirrored);
                    if (c.from..=c.to).contains(&k) {
                        c.arrivals.entry(k).or_insert((0, cur)).0 += 1;
                    }
                }
                if let Some((addr, hit)) = st.inject
                    && Self::exec_at(cur, addr, mirrored)
                {
                    st.inj_seen += 1;
                    if st.inj_seen == hit {
                        return Some(Leg::Inject);
                    }
                }
                if let RaceEnd::Pc { address, hit } = end
                    && Self::exec_at(cur, *address, mirrored)
                {
                    st.end_hits += 1;
                    if st.end_hits == *hit {
                        return Some(Leg::End);
                    }
                }
                None
            },
            |_| None,
        )?;
        Ok((run.stop.unwrap_or(Leg::Budget), run.steps))
    }

    /// The bytes of the compared ranges, in order.
    fn race_read(&mut self, compare: &[RaceCompare]) -> Result<Vec<u8>, ApiError> {
        let mut out = Vec::new();
        for c in compare {
            let bytes =
                self.peek_memory((c.address >> 16) as u8, c.address as u16, c.count as u16)?;
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }

    fn race_validate(&self, spec: &RaceSweepSpec) -> Result<(), ApiError> {
        let bad = |m: String| Err(ApiError::BadArg(m));
        if spec.from > spec.to {
            return bad(format!(
                "range starts after it ends ({} > {})",
                hex24(spec.from),
                hex24(spec.to)
            ));
        }
        if spec.hit == 0 {
            return bad("hit counts from 1".into());
        }
        if spec.max_steps == 0 {
            return bad("max_steps must be at least 1".into());
        }
        for c in &spec.compare {
            if c.count == 0 || (c.address & 0xFFFF) + c.count > 0x1_0000 {
                return bad(format!(
                    "compare {} x{}: the range must be 1.. bytes and stay in its bank",
                    hex24(c.address),
                    c.count
                ));
            }
        }
        match spec.end {
            RaceEnd::Pc { hit: 0, .. } => bad("until-hit counts from 1".into()),
            RaceEnd::Frame { frame } if frame <= self.frame_count()? => bad(format!(
                "until-frame {frame} is not after the sweep's start (frame {})",
                self.frame_count()?
            )),
            _ => Ok(()),
        }
    }

    /// Snapshot the machine where it stands, run the baseline, and list the
    /// addresses to sweep. The machine is back at the snapshot on return.
    pub fn race_sweep_baseline(&mut self, spec: &RaceSweepSpec) -> Result<RaceBaseline, ApiError> {
        self.race_validate(spec)?;
        let snapshot = self.save_state()?;
        let mirrored = self.rom_info.as_ref().is_none_or(|r| r.mapper != "ExHiRom");
        let mut census = Census {
            from: fold(spec.from, mirrored),
            to: fold(spec.to, mirrored),
            arrivals: BTreeMap::new(),
        };
        let lost = |steps: u64| {
            ApiError::BadArg(format!(
                "the baseline did not reach its end within {steps} instructions \
                 (raise max-steps, or check the end condition)"
            ))
        };
        let (leg, steps) = self.race_leg(
            &spec.end,
            &mut LegState::default(),
            Some(&mut census),
            spec.max_steps,
        )?;
        if leg != Leg::End {
            return Err(lost(spec.max_steps));
        }
        let values = self.race_read(&spec.compare)?;
        // The snapshot must reproduce the run, or no trial means anything.
        self.load_state(&snapshot)?;
        let (leg2, steps2) =
            self.race_leg(&spec.end, &mut LegState::default(), None, spec.max_steps)?;
        if leg2 != Leg::End || steps2 != steps || self.race_read(&spec.compare)? != values {
            return Err(ApiError::BadArg(
                "the baseline does not reproduce from its snapshot (a second run from the \
                 same state ended differently): the run depends on something a save state \
                 does not hold, so no comparison would be sound"
                    .into(),
            ));
        }
        self.load_state(&snapshot)?;
        let addresses = census
            .arrivals
            .into_iter()
            .filter(|&(_, (n, _))| n >= spec.hit)
            .map(|(_, (_, first))| first)
            .collect();
        Ok(RaceBaseline {
            snapshot,
            values,
            steps,
            trial_budget: spec
                .max_steps
                .min(steps.saturating_mul(4).saturating_add(100_000)),
            addresses,
        })
    }

    /// One trial: restore the snapshot, force `kind` before the `hit`-th
    /// arrival on `address`, run to the end, compare with the baseline.
    pub fn race_sweep_trial(
        &mut self,
        spec: &RaceSweepSpec,
        base: &RaceBaseline,
        address: u32,
        kind: InterruptKind,
    ) -> Result<RaceTrial, ApiError> {
        self.load_state(&base.snapshot)?;
        let mut trial = RaceTrial {
            address,
            address_hex: hex24(address),
            symbol: None,
            kind,
            verdict: RaceVerdict::NotReached,
            reason: None,
            diffs: Vec::new(),
            steps: 0,
        };
        let mut st = LegState {
            inject: Some((address, spec.hit)),
            ..LegState::default()
        };
        let (leg, steps1) = self.race_leg(&spec.end, &mut st, None, base.trial_budget)?;
        trial.steps = steps1;
        if leg != Leg::Inject {
            trial.reason = Some(match leg {
                Leg::End => "the end was reached before the arrival".to_string(),
                _ => "the arrival never came within the budget".to_string(),
            });
            return Ok(trial);
        }
        let outcome = self.inject_interrupt(kind)?;
        if !outcome.delivered {
            trial.verdict = RaceVerdict::NotTaken;
            trial.reason = outcome.reason;
            return Ok(trial);
        }
        st.inject = None;
        let (leg, steps2) = self.race_leg(
            &spec.end,
            &mut st,
            None,
            base.trial_budget.saturating_sub(steps1),
        )?;
        trial.steps += steps2;
        if leg != Leg::End {
            trial.verdict = RaceVerdict::EndNotReached;
            trial.reason = Some(format!(
                "the end was not reached within {} instructions (baseline: {})",
                base.trial_budget, base.steps
            ));
            return Ok(trial);
        }
        let got = self.race_read(&spec.compare)?;
        let mut i = 0usize;
        for c in &spec.compare {
            for off in 0..c.count {
                if got[i] != base.values[i] {
                    let a = c.address + off;
                    trial.diffs.push(RaceDiff {
                        address: a,
                        address_hex: hex24(a),
                        symbol: None,
                        baseline: base.values[i],
                        injected: got[i],
                    });
                }
                i += 1;
            }
        }
        trial.verdict = if trial.diffs.is_empty() {
            RaceVerdict::Identical
        } else {
            RaceVerdict::Differs
        };
        Ok(trial)
    }

    /// Gather the trials (in any order, from any number of workers) into the
    /// report: sorted by address then kind, named by the loaded symbols.
    #[must_use]
    pub fn race_sweep_report(
        &self,
        spec: &RaceSweepSpec,
        base: &RaceBaseline,
        mut trials: Vec<RaceTrial>,
    ) -> RaceSweepReport {
        trials.sort_by_key(|t| (t.address, kind_rank(t.kind)));
        for t in &mut trials {
            t.symbol = self.symbol_for_addr(t.address);
            for d in &mut t.diffs {
                d.symbol = self.symbol_for_addr(d.address);
            }
        }
        let count = |v: RaceVerdict| trials.iter().filter(|t| t.verdict == v).count();
        let mut racy: Vec<u32> = trials
            .iter()
            .filter(|t| matches!(t.verdict, RaceVerdict::Differs | RaceVerdict::EndNotReached))
            .map(|t| t.address)
            .collect();
        racy.dedup();
        let summary = RaceSummary {
            addresses_swept: base.addresses.len(),
            trials: trials.len(),
            identical: count(RaceVerdict::Identical),
            differs: count(RaceVerdict::Differs),
            not_taken: count(RaceVerdict::NotTaken),
            not_reached: count(RaceVerdict::NotReached),
            end_not_reached: count(RaceVerdict::EndNotReached),
            racy_addresses: racy.len(),
        };
        RaceSweepReport {
            schema: RACE_SWEEP_SCHEMA,
            from: spec.from,
            to: spec.to,
            kinds: distinct_kinds(&spec.kinds),
            hit: spec.hit,
            end: spec.end,
            compare: {
                let mut at = 0usize;
                spec.compare
                    .iter()
                    .map(|c| {
                        let n = c.count as usize;
                        let hex = base.values[at..at + n]
                            .iter()
                            .fold(String::new(), |mut s, b| {
                                let _ = write!(s, "{b:02x}");
                                s
                            });
                        at += n;
                        RaceCompareOut {
                            address: c.address,
                            address_hex: hex24(c.address),
                            count: c.count,
                            symbol: self.symbol_for_addr(c.address),
                            baseline_hex: hex,
                        }
                    })
                    .collect()
            },
            baseline_steps: base.steps,
            trial_budget: base.trial_budget,
            summary,
            trials,
        }
    }

    /// The whole sweep on this emulator, sequentially, from where it stands;
    /// it stands there again when this returns.
    pub fn race_sweep(&mut self, spec: &RaceSweepSpec) -> Result<RaceSweepReport, ApiError> {
        let base = self.race_sweep_baseline(spec)?;
        let mut trials = Vec::new();
        for (address, kind) in base.tasks(spec) {
            trials.push(self.race_sweep_trial(spec, &base, address, kind)?);
        }
        self.load_state(&base.snapshot)?;
        Ok(self.race_sweep_report(spec, &base, trials))
    }
}

impl RaceSweepReport {
    /// Whether the sweep found something: a trial that differs or lost its
    /// end. What `--fail-on-race` turns into an exit code.
    #[must_use]
    pub const fn racy(&self) -> bool {
        self.summary.racy_addresses > 0
    }

    /// Whether no trial tested anything although there were some (every
    /// injection refused): a sweep that proves nothing.
    #[must_use]
    pub const fn untested(&self) -> bool {
        self.summary.trials > 0 && self.summary.not_taken == self.summary.trials
    }

    /// The text report: the header, one line per trial that is not
    /// identical, the summary.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut s = String::new();
        let kinds: Vec<&str> = self.kinds.iter().map(|k| k.name()).collect();
        let end = match self.end {
            RaceEnd::Pc { address, hit } => format!("pc {} hit {hit}", hex24(address)),
            RaceEnd::Frame { frame } => format!("frame {frame}"),
        };
        let _ = writeln!(
            s,
            "race-sweep {}..{}  {}  inject at arrival {}  end: {end}  baseline {} instructions",
            hex24(self.from),
            hex24(self.to),
            kinds.join("+"),
            self.hit,
            self.baseline_steps
        );
        for c in &self.compare {
            let _ = writeln!(
                s,
                "  compare {} x{} = {}{}",
                c.address_hex,
                c.count,
                c.baseline_hex,
                c.symbol
                    .as_ref()
                    .map_or(String::new(), |n| format!("  ({n})"))
            );
        }
        let shown: Vec<&RaceTrial> = self
            .trials
            .iter()
            .filter(|t| t.verdict != RaceVerdict::Identical)
            .collect();
        if !shown.is_empty() {
            let _ = writeln!(
                s,
                "  {:<10} {:<24} {:<4} VERDICT",
                "ADDRESS", "SYMBOL", "KIND"
            );
        }
        for t in shown {
            let detail = match t.verdict {
                RaceVerdict::Differs => {
                    let mut parts: Vec<String> = t
                        .diffs
                        .iter()
                        .take(4)
                        .map(|d| {
                            format!(
                                "{}{} {:02X} -> {:02X}",
                                d.address_hex,
                                d.symbol.as_ref().map_or(String::new(), |n| format!(" {n}")),
                                d.baseline,
                                d.injected
                            )
                        })
                        .collect();
                    if t.diffs.len() > 4 {
                        parts.push(format!("+{} more", t.diffs.len() - 4));
                    }
                    parts.join("; ")
                }
                _ => t.reason.clone().unwrap_or_default(),
            };
            let _ = writeln!(
                s,
                "  {:<10} {:<24} {:<4} {}: {detail}",
                t.address_hex,
                t.symbol.as_deref().unwrap_or("-"),
                t.kind.name(),
                t.verdict.name()
            );
        }
        let m = &self.summary;
        let _ = write!(
            s,
            "{} addresses swept, {} racy ({} trials: {} identical, {} differ, {} end not reached",
            m.addresses_swept,
            m.racy_addresses,
            m.trials,
            m.identical,
            m.differs,
            m.end_not_reached
        );
        if m.not_taken > 0 {
            let _ = write!(s, ", {} NOT TAKEN: not tested", m.not_taken);
        }
        if m.not_reached > 0 {
            let _ = write!(s, ", {} arrival not reached", m.not_reached);
        }
        s.push(')');
        s
    }
}
