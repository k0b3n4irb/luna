//! Group a raw DSP-1 port trace into command transactions (luna issue #158).
//!
//! The raw `--dsp1-trace` stream is byte-level: individual `DR` reads and
//! writes plus `SR` polls. What a driver author actually reasons about is a
//! *transaction* — a command byte, the input words it consumed, the output
//! words it produced. This module reconstructs that.
//!
//! # The table is a prediction; the stream is the truth
//!
//! Word counts per command come from the `OpenSNES` `dsp1` module. They are
//! **never** used to decide where a transaction ends — the boundaries come
//! from the observable protocol (see [`decode`]). The table only supplies an
//! *expectation*, which is then compared against what actually happened.
//!
//! That distinction is the whole safety property. A wrong table entry here
//! must not silently mis-group a handshake and send someone chasing a
//! phantom emulator bug: it surfaces as [`TxStatus::Mismatch`] on that one
//! transaction, with both counts printed, and every other transaction stays
//! correct.
//!
//! Counts carry a [`Confidence`] for the same reason — a `Provisional` row
//! that disagrees with the stream is far more likely to be a stale table
//! than an emulator defect, and the output says so rather than implying a
//! verdict it has not earned.

use crate::{Dsp1TraceEvent, Dsp1TraceKind};

/// How much the word counts for a command can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Verified against luna by the `OpenSNES` team with `dsp1b.rom`.
    Verified,
    /// From the consolidated docs (fullsnes / snes9x / `SnesLab`).
    Documented,
    /// Provisional — do not read a mismatch here as an emulator bug.
    Provisional,
    /// Output length is not a fixed count (see [`Operation::bounded`]).
    Unbounded,
    /// Command byte not in the table at all.
    Unknown,
}

/// Outcome of matching one transaction against the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxStatus {
    /// Observed word counts match the table.
    Ok,
    /// Observed counts disagree with the table — reported, never "fixed".
    Mismatch,
    /// Open-ended output; the observed count is reported, nothing asserted.
    Unbounded,
    /// The capture ended mid-transaction, so the counts are a lower bound.
    Truncated,
    /// Command byte not in the table; counts are observations only.
    Unknown,
}

/// One decoded DSP-1 command.
#[derive(Debug, Clone)]
pub struct Transaction {
    /// Index of the command byte within the raw event stream.
    pub seq: usize,
    /// The command byte the master wrote in 8-bit (`DRC`) mode.
    pub command: u8,
    /// Human-readable operation name, or `"?"` when unknown.
    pub name: &'static str,
    /// Microcode PC when the command byte landed.
    pub pc: u16,
    /// Input words the master supplied, in order.
    pub in_words: Vec<u16>,
    /// Output words the master read back, in order.
    pub out_words: Vec<u16>,
    /// Word counts the table predicts (`None` when unknown/unbounded).
    pub expected_in: Option<u8>,
    /// Expected output words (`None` when unknown/unbounded).
    pub expected_out: Option<u8>,
    /// Trust level of the table row this was matched against.
    pub confidence: Confidence,
    /// How the observation compared to the prediction.
    pub status: TxStatus,
}

/// A table row: one logical operation, covering all its opcode aliases.
struct Operation {
    name: &'static str,
    opcodes: &'static [u8],
    words_in: u8,
    words_out: u8,
    confidence: Confidence,
    /// `false` for operations that stream an open-ended result (Raster,
    /// ROM dump): `words_out` is then not an assertion.
    bounded: bool,
}

/// The `OpenSNES` command table (issue #158). Keyed on the **operation**:
/// the A/B/C matrix-slot variants and the `$x0` mirrors share identical
/// word counts, so one row covers every opcode that selects it.
const OPERATIONS: &[Operation] = &[
    op("Multiply", &[0x00, 0x20], 2, 1, Confidence::Verified),
    op("Triangle", &[0x04, 0x24], 2, 2, Confidence::Verified),
    op("Rotate", &[0x0C, 0x2C], 3, 2, Confidence::Verified),
    op("Attitude", &[0x01, 0x11, 0x21], 4, 0, Confidence::Verified),
    op("Objective", &[0x0D, 0x1D, 0x2D], 3, 3, Confidence::Verified),
    op(
        "Subjective",
        &[0x03, 0x13, 0x23],
        3,
        3,
        Confidence::Documented,
    ),
    op("Scalar", &[0x0B, 0x1B, 0x2B], 3, 1, Confidence::Documented),
    op("Inverse", &[0x10, 0x30], 2, 2, Confidence::Documented),
    op("Radius", &[0x08], 3, 2, Confidence::Documented),
    op("Range", &[0x18, 0x38], 4, 1, Confidence::Documented),
    op("Distance", &[0x28], 3, 1, Confidence::Documented),
    op("Polar", &[0x1C, 0x3C], 6, 3, Confidence::Documented),
    op(
        "Project",
        &[0x06, 0x16, 0x26, 0x36],
        3,
        3,
        Confidence::Documented,
    ),
    op(
        "Target",
        &[0x0E, 0x1E, 0x2E, 0x3E],
        2,
        2,
        Confidence::Documented,
    ),
    op("Gyrate", &[0x14, 0x34], 6, 3, Confidence::Provisional),
    // Parameter's output count was flagged provisional ("~4") by OpenSNES.
    // luna observes a stable 7-in/4-out over 112 consecutive Super Mario
    // Kart transactions, so it is carried here as 4 — still Provisional
    // until their module confirms it from the other side.
    op(
        "Parameter",
        &[0x02, 0x12, 0x22, 0x32],
        7,
        4,
        Confidence::Provisional,
    ),
    op("RamTest", &[0x0F, 0x07], 1, 2, Confidence::Provisional),
    op("RomVersion", &[0x2F, 0x27], 1, 2, Confidence::Provisional),
    // Open-ended: Raster emits a per-scanline matrix stream until the
    // master stops it, and ROM dump streams the whole 2048-word ROM.
    // OpenSNES gave no word counts for either, so neither is claimed —
    // the observed counts are reported and nothing is asserted.
    unbounded("Raster", &[0x0A, 0x1A, 0x2A, 0x3A]),
    unbounded("RomDump", &[0x1F, 0x17, 0x37, 0x3F]),
];

const fn op(
    name: &'static str,
    opcodes: &'static [u8],
    words_in: u8,
    words_out: u8,
    confidence: Confidence,
) -> Operation {
    Operation {
        name,
        opcodes,
        words_in,
        words_out,
        confidence,
        bounded: true,
    }
}

/// An operation with no word counts on record: both are reported from the
/// stream, neither is asserted.
const fn unbounded(name: &'static str, opcodes: &'static [u8]) -> Operation {
    Operation {
        name,
        opcodes,
        words_in: 0,
        words_out: 0,
        confidence: Confidence::Unbounded,
        bounded: false,
    }
}

fn lookup(command: u8) -> Option<&'static Operation> {
    OPERATIONS.iter().find(|o| o.opcodes.contains(&command))
}

/// `SR` bit 10 — `DRC`, set while the port is in 8-bit mode.
const SR_DRC: u16 = 1 << 10;
/// `SR` bit 12 — `DRS`, set after the low byte of a 16-bit transfer.
const SR_DRS: u16 = 1 << 12;

/// Does this port event complete a whole word?
///
/// In 8-bit mode every access is complete. In 16-bit mode the low byte
/// leaves `DRS` set and the high byte clears it, so a cleared `DRS` marks
/// the access that finished the word — and `dr`, captured after the event,
/// then holds all 16 bits.
const fn completes_word(sr: u16) -> bool {
    sr & SR_DRC != 0 || sr & SR_DRS == 0
}

/// Group a raw port trace into command transactions.
///
/// Boundaries come from the protocol, never from the table: the master
/// writes a command byte with the port in **8-bit (`DRC`) mode**, and every
/// word until the next such write belongs to that command. This is what
/// makes an open-ended Raster stream and a wrong table entry both harmless
/// — neither can shift where the next transaction starts.
///
/// `truncated` marks the capture as having hit its event cap, which makes
/// the final transaction's counts a lower bound rather than a disagreement.
#[must_use]
pub fn decode(events: &[Dsp1TraceEvent], truncated: bool) -> Vec<Transaction> {
    let mut out: Vec<Transaction> = Vec::new();

    for (seq, ev) in events.iter().enumerate() {
        match ev.kind {
            // A command byte: 8-bit write. Opens a new transaction.
            Dsp1TraceKind::DrWrite if ev.sr & SR_DRC != 0 => {
                let op = lookup(ev.value);
                out.push(Transaction {
                    seq,
                    command: ev.value,
                    name: op.map_or("?", |o| o.name),
                    pc: ev.pc,
                    in_words: Vec::new(),
                    out_words: Vec::new(),
                    expected_in: op.and_then(|o| o.bounded.then_some(o.words_in)),
                    expected_out: op.and_then(|o| o.bounded.then_some(o.words_out)),
                    confidence: op.map_or(Confidence::Unknown, |o| o.confidence),
                    status: TxStatus::Ok,
                });
            }
            // Payload words, attributed to the open transaction.
            Dsp1TraceKind::DrWrite | Dsp1TraceKind::DrRead if completes_word(ev.sr) => {
                if let Some(tx) = out.last_mut() {
                    if ev.kind == Dsp1TraceKind::DrWrite {
                        tx.in_words.push(ev.dr);
                    } else {
                        tx.out_words.push(ev.dr);
                    }
                }
            }
            _ => {}
        }
    }

    let last = out.len().saturating_sub(1);
    for (i, tx) in out.iter_mut().enumerate() {
        tx.status = classify(tx, truncated && i == last);
    }
    out
}

/// Compare one transaction's observation against its table row.
fn classify(tx: &Transaction, truncated: bool) -> TxStatus {
    if truncated {
        return TxStatus::Truncated;
    }
    match tx.confidence {
        Confidence::Unknown => TxStatus::Unknown,
        Confidence::Unbounded => TxStatus::Unbounded,
        _ => {
            let ok = tx
                .expected_in
                .is_some_and(|n| usize::from(n) == tx.in_words.len())
                && tx
                    .expected_out
                    .is_some_and(|n| usize::from(n) == tx.out_words.len());
            if ok { TxStatus::Ok } else { TxStatus::Mismatch }
        }
    }
}

impl Confidence {
    /// Single-word tag for CSV / report output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Documented => "documented",
            Self::Provisional => "provisional",
            Self::Unbounded => "unbounded",
            Self::Unknown => "unknown",
        }
    }
}

impl TxStatus {
    /// Single-word tag for CSV / report output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Mismatch => "mismatch",
            Self::Unbounded => "unbounded",
            Self::Truncated => "truncated",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: Dsp1TraceKind, value: u8, dr: u16, sr: u16) -> Dsp1TraceEvent {
        Dsp1TraceEvent {
            kind,
            pc: 0x0004,
            opcode: 0,
            value,
            a: 0,
            b: 0,
            dr,
            sr,
            rqm: false,
        }
    }

    /// A command byte: 8-bit write, `DRC` set.
    fn cmd(v: u8) -> Dsp1TraceEvent {
        ev(Dsp1TraceKind::DrWrite, v, u16::from(v), SR_DRC)
    }

    /// A whole 16-bit word, as the two accesses the master really makes.
    fn word(kind: Dsp1TraceKind, w: u16) -> [Dsp1TraceEvent; 2] {
        [
            ev(kind, w as u8, w & 0x00FF, SR_DRS),
            ev(kind, (w >> 8) as u8, w, 0),
        ]
    }

    fn stream(parts: &[&[Dsp1TraceEvent]]) -> Vec<Dsp1TraceEvent> {
        parts.iter().flat_map(|p| p.iter().copied()).collect()
    }

    #[test]
    fn a_bounded_command_matches_its_table_row() {
        // Multiply: 2 in, 1 out.
        let s = stream(&[
            &[cmd(0x00)],
            &word(Dsp1TraceKind::DrWrite, 0x1234),
            &word(Dsp1TraceKind::DrWrite, 0x5678),
            &word(Dsp1TraceKind::DrRead, 0xABCD),
        ]);
        let tx = decode(&s, false);
        assert_eq!(tx.len(), 1);
        assert_eq!(tx[0].name, "Multiply");
        assert_eq!(tx[0].in_words, vec![0x1234, 0x5678]);
        assert_eq!(tx[0].out_words, vec![0xABCD]);
        assert_eq!(tx[0].status, TxStatus::Ok);
    }

    /// The safety property: a count that disagrees is REPORTED, and the
    /// next transaction still starts in the right place.
    #[test]
    fn a_wrong_count_is_reported_and_does_not_shift_the_next_command() {
        let s = stream(&[
            &[cmd(0x00)],
            &word(Dsp1TraceKind::DrWrite, 0x0001), // only 1 in, table says 2
            &word(Dsp1TraceKind::DrRead, 0x0002),
            &[cmd(0x04)], // Triangle, 2 in / 2 out
            &word(Dsp1TraceKind::DrWrite, 0x0003),
            &word(Dsp1TraceKind::DrWrite, 0x0004),
            &word(Dsp1TraceKind::DrRead, 0x0005),
            &word(Dsp1TraceKind::DrRead, 0x0006),
        ]);
        let tx = decode(&s, false);
        assert_eq!(tx.len(), 2);
        assert_eq!(tx[0].status, TxStatus::Mismatch);
        assert_eq!(tx[0].expected_in, Some(2));
        assert_eq!(tx[0].in_words.len(), 1);
        // Unaffected by its neighbour's disagreement.
        assert_eq!(tx[1].name, "Triangle");
        assert_eq!(tx[1].status, TxStatus::Ok);
    }

    #[test]
    fn raster_streams_without_asserting_a_length() {
        let mut parts: Vec<Dsp1TraceEvent> = vec![cmd(0x0A)];
        for i in 0..300u16 {
            parts.extend(word(Dsp1TraceKind::DrRead, i));
        }
        let tx = decode(&parts, false);
        assert_eq!(tx[0].name, "Raster");
        assert_eq!(tx[0].status, TxStatus::Unbounded);
        assert_eq!(tx[0].expected_out, None);
        assert_eq!(tx[0].out_words.len(), 300);
    }

    #[test]
    fn all_opcode_aliases_resolve_to_one_operation() {
        for (a, b) in [(0x01, 0x21), (0x0D, 0x2D), (0x02, 0x32), (0x0A, 0x3A)] {
            assert_eq!(lookup(a).unwrap().name, lookup(b).unwrap().name);
            assert_eq!(lookup(a).unwrap().words_in, lookup(b).unwrap().words_in);
        }
    }

    #[test]
    fn an_unknown_command_is_observed_not_guessed() {
        // $80: Super Mario Kart writes it 128x at boot with no payload.
        let tx = decode(&[cmd(0x80)], false);
        assert_eq!(tx[0].name, "?");
        assert_eq!(tx[0].status, TxStatus::Unknown);
        assert_eq!(tx[0].expected_in, None);
    }

    #[test]
    fn a_capped_capture_marks_only_the_last_transaction() {
        let s = stream(&[
            &[cmd(0x00)],
            &word(Dsp1TraceKind::DrWrite, 1),
            &word(Dsp1TraceKind::DrWrite, 2),
            &word(Dsp1TraceKind::DrRead, 3),
            &[cmd(0x00)],
            &word(Dsp1TraceKind::DrWrite, 4),
        ]);
        let tx = decode(&s, true);
        assert_eq!(tx[0].status, TxStatus::Ok);
        assert_eq!(tx[1].status, TxStatus::Truncated);
    }

    /// Exec and SR-poll rows must not be mistaken for payload.
    #[test]
    fn exec_and_poll_rows_are_ignored() {
        let s = stream(&[
            &[cmd(0x00)],
            &[ev(Dsp1TraceKind::SrRead, 0x80, 0, 0x8000)],
            &word(Dsp1TraceKind::DrWrite, 1),
            &[ev(Dsp1TraceKind::Exec, 0, 0, 0)],
            &word(Dsp1TraceKind::DrWrite, 2),
            &word(Dsp1TraceKind::DrRead, 3),
        ]);
        let tx = decode(&s, false);
        assert_eq!(tx[0].in_words, vec![1, 2]);
        assert_eq!(tx[0].status, TxStatus::Ok);
    }

    /// Payload arriving before any command byte must not panic.
    #[test]
    fn payload_before_any_command_is_dropped() {
        let s = stream(&[&word(Dsp1TraceKind::DrRead, 0xFFFF), &[cmd(0x00)]]);
        let tx = decode(&s, false);
        assert_eq!(tx.len(), 1);
        assert!(tx[0].out_words.is_empty());
    }
}
