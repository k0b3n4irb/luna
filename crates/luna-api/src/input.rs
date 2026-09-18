//! Scripted input — the one definition of how a `frame:mask` script drives
//! the machine, shared by every front-end (`luna state` / `run` / `frames`
//! / `test` / `diff` / `profile` / `bench`, and anything built on the API).
//!
//! It used to live in the CLI, re-implemented per subcommand with drifting
//! semantics (issue #126 fixed `state` and the dumps, `frames` kept the old
//! pre-roll). The grammar, the event order and the budget rule now exist
//! once, here.

use crate::{ApiError, Emulator};

/// Per-call instruction cap while chasing a frame: large enough for any real
/// frame (a slow one is ~40k instructions, SA-1 / Super FX included), small
/// enough that a budget check between frames stays responsive. Front-ends use
/// it as their `step_until_frame` safety belt too.
pub const FRAME_STEP_BUDGET: u64 = 200_000;

/// One scripted input change, applied at the start of its frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Joypad `port` (0 = pad 1, 1 = pad 2) takes this button mask.
    Pad {
        /// Controller port, 0-based.
        port: u8,
        /// SNES button mask (`B Y Sel St ↑ ↓ ← → A X L R` from bit 15).
        mask: u16,
    },
    /// SNES Mouse displacement + buttons for the next auto-read.
    Mouse {
        /// Horizontal displacement (signed).
        dx: i32,
        /// Vertical displacement (signed).
        dy: i32,
        /// bit 0 = left, bit 1 = right.
        buttons: u8,
    },
    /// Super Scope aim + buttons.
    Scope {
        /// Screen X the scope points at.
        x: i32,
        /// Screen Y the scope points at.
        y: i32,
        /// Button bitmask.
        buttons: u8,
    },
}

/// How far a scripted run may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptBound {
    /// A total instruction budget: chasing a checkpoint's frame spends from
    /// it, so a checkpoint the run never reaches does not fire and the run is
    /// exactly this long (issue #126).
    Steps(u64),
    /// A PPU frame: checkpoints up to it fire (each frame chased with the
    /// [`FRAME_STEP_BUDGET`] allowance), later ones are dropped.
    Frame(u64),
}

/// A frame-sorted stream of [`InputEvent`]s with a replay cursor.
#[derive(Debug, Clone, Default)]
pub struct InputScript {
    events: Vec<(u64, InputEvent)>,
    next: usize,
}

impl InputScript {
    /// An empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add events, keeping the stream frame-sorted. The sort is stable:
    /// events on the same frame apply in the order they were added.
    pub fn extend(&mut self, events: impl IntoIterator<Item = (u64, InputEvent)>) {
        self.events.extend(events);
        self.events.sort_by_key(|&(frame, _)| frame);
    }

    /// Add a joypad script (see [`parse_pad_script`]) for `port`.
    pub fn add_pad(&mut self, port: u8, body: &str) -> Result<(), String> {
        let v = parse_pad_script(body)?;
        self.extend(
            v.into_iter()
                .map(|(f, mask)| (f, InputEvent::Pad { port, mask })),
        );
        Ok(())
    }

    /// Add a mouse script (see [`parse_pointer_script`]).
    pub fn add_mouse(&mut self, body: &str) -> Result<(), String> {
        let v = parse_pointer_script(body)?;
        self.extend(
            v.into_iter()
                .map(|(f, (dx, dy, buttons))| (f, InputEvent::Mouse { dx, dy, buttons })),
        );
        Ok(())
    }

    /// Add a Super Scope script (same grammar as the mouse: `x,y,buttons`).
    pub fn add_scope(&mut self, body: &str) -> Result<(), String> {
        let v = parse_pointer_script(body)?;
        self.extend(
            v.into_iter()
                .map(|(f, (x, y, buttons))| (f, InputEvent::Scope { x, y, buttons })),
        );
        Ok(())
    }

    /// No events at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Every event, frame-sorted.
    #[must_use]
    pub fn events(&self) -> &[(u64, InputEvent)] {
        &self.events
    }

    /// The script drives the SNES Mouse (the port needs one plugged in).
    #[must_use]
    pub fn uses_mouse(&self) -> bool {
        self.events
            .iter()
            .any(|(_, e)| matches!(e, InputEvent::Mouse { .. }))
    }

    /// The script drives the Super Scope.
    #[must_use]
    pub fn uses_scope(&self) -> bool {
        self.events
            .iter()
            .any(|(_, e)| matches!(e, InputEvent::Scope { .. }))
    }

    /// Frame of the next event not yet applied.
    #[must_use]
    pub fn next_frame(&self) -> Option<u64> {
        self.events.get(self.next).map(|&(f, _)| f)
    }

    /// Apply every not-yet-applied event scheduled at or before `frame`.
    /// Call it once per frame before stepping that frame (a frame-by-frame
    /// driver), or let [`Emulator::run_input_script`] chase the frames.
    pub fn apply_due(&mut self, em: &mut Emulator, frame: u64) -> Result<(), ApiError> {
        while let Some(&(at, ev)) = self.events.get(self.next) {
            if at > frame {
                break;
            }
            em.apply_input(ev)?;
            self.next += 1;
        }
        Ok(())
    }
}

/// Parse a joypad script body into `(frame, mask)` checkpoints sorted by
/// frame: comma- **and** newline-separated `frame:hex` entries, `#` starts a
/// comment to end-of-line, the frame is decimal, the mask 16-bit hex with an
/// optional `0x`. (Reading an `@file` is the front-end's job.)
pub fn parse_pad_script(body: &str) -> Result<Vec<(u64, u16)>, String> {
    let mut out: Vec<(u64, u16)> = Vec::new();
    for line in body.lines() {
        // Drop a `#` comment (whole-line or trailing) before splitting.
        let line = line.split('#').next().unwrap_or("");
        for entry in line.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (frame_str, mask_str) = entry
                .split_once(':')
                .ok_or_else(|| format!("missing ':' in entry `{entry}`"))?;
            let frame: u64 = frame_str
                .trim()
                .parse()
                .map_err(|e| format!("bad frame `{frame_str}`: {e}"))?;
            let mask_str = mask_str
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            let mask: u16 = u16::from_str_radix(mask_str, 16)
                .map_err(|e| format!("bad hex mask `{mask_str}`: {e}"))?;
            out.push((frame, mask));
        }
    }
    out.sort_by_key(|(f, _)| *f);
    Ok(out)
}

/// A scripted pointer checkpoint: `(frame, (a, b, buttons))` — mouse
/// `dx, dy` or Super Scope `x, y`.
pub type PointerCheckpoint = (u64, (i32, i32, u8));

/// Parse a mouse / Super Scope script: `;`-separated `frame:a,b,buttons`
/// entries (signed `a`/`b` — mouse `dx,dy`, scope `x,y`; `buttons` decimal).
/// Sorted by frame.
pub fn parse_pointer_script(body: &str) -> Result<Vec<PointerCheckpoint>, String> {
    let mut out = Vec::new();
    for entry in body.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (frame, rest) = entry
            .split_once(':')
            .ok_or_else(|| format!("`{entry}`: expected `frame:dx,dy,buttons`"))?;
        let frame: u64 = frame
            .trim()
            .parse()
            .map_err(|_| format!("`{entry}`: bad frame"))?;
        let p: Vec<&str> = rest.split(',').map(str::trim).collect();
        if p.len() != 3 {
            return Err(format!("`{entry}`: expected `dx,dy,buttons` (3 values)"));
        }
        let a: i32 = p[0].parse().map_err(|_| format!("`{entry}`: bad dx"))?;
        let b: i32 = p[1].parse().map_err(|_| format!("`{entry}`: bad dy"))?;
        let buttons: u8 = p[2]
            .parse()
            .map_err(|_| format!("`{entry}`: bad buttons"))?;
        out.push((frame, (a, b, buttons)));
    }
    out.sort_by_key(|(f, _)| *f);
    Ok(out)
}

impl Emulator {
    /// Apply one scripted input change to its device.
    pub fn apply_input(&mut self, ev: InputEvent) -> Result<(), ApiError> {
        match ev {
            InputEvent::Pad { port, mask } => self.set_joypad(port, mask),
            InputEvent::Mouse { dx, dy, buttons } => self.set_mouse(dx, dy, buttons),
            InputEvent::Scope { x, y, buttons } => self.set_superscope(x, y, buttons),
        }
    }

    /// Step until PPU frame `frame` is reached, spending **at most** `budget`
    /// instructions (the last partial frame stops exactly on the limit).
    /// Returns the instructions consumed; stops early on a halted core, so a
    /// dead ROM cannot spin here.
    pub fn step_to_frame_bounded(&mut self, frame: u64, budget: u64) -> u64 {
        let start = self.instructions_executed();
        while self.frame_count().unwrap_or(0) < frame {
            let spent = self.instructions_executed().saturating_sub(start);
            let left = budget.saturating_sub(spent);
            if left == 0 {
                break;
            }
            if self
                .step_until_frame(left.min(FRAME_STEP_BUDGET))
                .unwrap_or(0)
                == 0
            {
                break;
            }
        }
        self.instructions_executed().saturating_sub(start)
    }

    /// Replay `script` from its cursor within `bound`: chase each event's
    /// frame, apply it on arrival, stop at the first event the bound does
    /// not reach. Returns the instructions consumed — with
    /// [`ScriptBound::Steps`] the caller spends the rest of its budget, so
    /// the whole run is the budget with or without a script.
    pub fn run_input_script(
        &mut self,
        script: &mut InputScript,
        bound: ScriptBound,
    ) -> Result<u64, ApiError> {
        let start = self.instructions_executed();
        while let Some(frame) = script.next_frame() {
            let left = match bound {
                ScriptBound::Frame(target) if frame > target => break,
                ScriptBound::Frame(_) => FRAME_STEP_BUDGET
                    .saturating_mul(frame.saturating_sub(self.frame_count().unwrap_or(0)).max(1)),
                ScriptBound::Steps(total) => {
                    let spent = self.instructions_executed().saturating_sub(start);
                    match total.checked_sub(spent).filter(|l| *l > 0) {
                        Some(l) => l,
                        None => break, // budget exhausted — later events never happen
                    }
                }
            };
            self.step_to_frame_bounded(frame, left);
            let now = self.frame_count()?;
            if now < frame {
                break; // ran out before reaching this event's frame
            }
            script.apply_due(self, now)?;
        }
        Ok(self.instructions_executed().saturating_sub(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_scripts_parse_comment_and_sort() {
        let v = parse_pad_script("1610:0,1600:0x1000 # start\n# whole line\n20:0X8000").unwrap();
        assert_eq!(v, vec![(20, 0x8000), (1600, 0x1000), (1610, 0)]);
        assert!(parse_pad_script("12").is_err());
        assert!(parse_pad_script("x:1").is_err());
        assert!(parse_pad_script("1:10000").is_err());
    }

    #[test]
    fn pointer_scripts_parse_signed_values() {
        assert_eq!(
            parse_pointer_script("5:-3,4,1; 2:0,0,0").unwrap(),
            vec![(2, (0, 0, 0)), (5, (-3, 4, 1))]
        );
        assert!(parse_pointer_script("5:1,2").is_err());
    }

    /// A 32 KB `LoROM` that spins forever (`BRA -2`), so frames advance.
    fn spin_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[..2].copy_from_slice(&[0x80, 0xFE]);
        rom[0x7FFC..0x7FFE].copy_from_slice(&[0x00, 0x80]);
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"LUNA INPUT SCRIPT    ");
        rom[0x7FD5] = 0x20;
        rom[0x7FD7] = 0x05;
        let sum: u32 = rom
            .iter()
            .enumerate()
            .filter(|(i, _)| !(0x7FDC..=0x7FDF).contains(i))
            .map(|(_, b)| u32::from(*b))
            .sum();
        let checksum = (sum & 0xFFFF) as u16;
        rom[0x7FDC..0x7FDE].copy_from_slice(&(!checksum).to_le_bytes());
        rom[0x7FDE..0x7FE0].copy_from_slice(&checksum.to_le_bytes());
        rom
    }

    #[test]
    fn a_step_budget_bounds_the_chase_and_unreached_events_never_fire() {
        // Issue #126: `-n` is the whole run, script or not.
        let mut em = Emulator::new();
        em.load_rom_bytes(spin_rom()).unwrap();
        let mut s = InputScript::new();
        s.add_pad(0, "2:0x1000,900:0x8000").unwrap();
        let spent = em
            .run_input_script(&mut s, ScriptBound::Steps(100_000))
            .unwrap();
        assert!(spent <= 100_000, "spent {spent}");
        assert_eq!(
            s.next_frame(),
            Some(900),
            "frame-2 event fired, frame-900 did not"
        );
        assert!(em.frame_count().unwrap() < 900);
    }

    #[test]
    fn a_frame_bound_fires_up_to_it_and_drops_the_rest() {
        let mut em = Emulator::new();
        em.load_rom_bytes(spin_rom()).unwrap();
        let mut s = InputScript::new();
        s.add_pad(0, "3:0x1000,5:0,40:0x8000").unwrap();
        em.run_input_script(&mut s, ScriptBound::Frame(10)).unwrap();
        assert_eq!(s.next_frame(), Some(40));
        assert_eq!(
            em.frame_count().unwrap(),
            5,
            "chased to the last due event only"
        );
    }

    #[test]
    fn same_frame_events_keep_insertion_order() {
        let mut s = InputScript::new();
        s.add_pad(0, "3:1").unwrap();
        s.add_pad(1, "3:2").unwrap();
        s.add_mouse("1:1,1,0").unwrap();
        let ports: Vec<_> = s.events().iter().map(|(f, e)| (*f, *e)).collect();
        assert_eq!(ports[0].0, 1);
        assert_eq!(ports[1].1, InputEvent::Pad { port: 0, mask: 1 });
        assert_eq!(ports[2].1, InputEvent::Pad { port: 1, mask: 2 });
        assert!(s.uses_mouse() && !s.uses_scope());
    }
}
