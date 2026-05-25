//! Keyboard input bindings for the SNES gamepad.
//!
//! Defaults match **Mesen2's "Arrow keys" preset**
//! (`UI/Config/KeyPresets.cs::ApplyArrowLayout` for
//! `ControllerType.SnesController`): D-pad on the arrow cluster,
//! face buttons + shoulders on the left-hand QWERTY block, Start /
//! Select on `D` / `E`. ares ships no hard-coded keyboard defaults
//! — its `desktop-ui/input/input.cpp` `VirtualPad` exposes named
//! slots and the user binds them at first launch.
//!
//! The binding can be remapped per-button via the in-GUI Input
//! menu and is persisted to a JSON file under the platform's
//! standard config directory.

use egui::Key;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// One of the 12 SNES controller buttons. Bit positions match the
/// `JOY1L`/`JOY1H` 16-bit shift register layout (per ares'
/// `gamepad.cpp` shift order — see `CLAUDE.md` controller bindings
/// table).
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash, Serialize, Deserialize)]
pub(crate) enum SnesButton {
    /// SNES "B" face button — JOY1 bit 15.
    B,
    /// SNES "Y" face button — JOY1 bit 14.
    Y,
    /// `Select` — JOY1 bit 13.
    Select,
    /// `Start` — JOY1 bit 12.
    Start,
    /// D-pad up — JOY1 bit 11.
    Up,
    /// D-pad down — JOY1 bit 10.
    Down,
    /// D-pad left — JOY1 bit 9.
    Left,
    /// D-pad right — JOY1 bit 8.
    Right,
    /// SNES "A" face button — JOY1 bit 7.
    A,
    /// SNES "X" face button — JOY1 bit 6.
    X,
    /// Left shoulder — JOY1 bit 5.
    L,
    /// Right shoulder — JOY1 bit 4.
    R,
}

impl SnesButton {
    /// All 12 buttons in the canonical shift order (B, Y, Select,
    /// Start, Up, Down, Left, Right, A, X, L, R) — useful for the
    /// remap UI and serialization.
    pub(crate) const ALL: [SnesButton; 12] = [
        SnesButton::B,
        SnesButton::Y,
        SnesButton::Select,
        SnesButton::Start,
        SnesButton::Up,
        SnesButton::Down,
        SnesButton::Left,
        SnesButton::Right,
        SnesButton::A,
        SnesButton::X,
        SnesButton::L,
        SnesButton::R,
    ];

    /// Bitmask within the 16-bit `JOY1` shift register.
    #[must_use]
    pub(crate) fn mask(self) -> u16 {
        match self {
            SnesButton::B => 0x8000,
            SnesButton::Y => 0x4000,
            SnesButton::Select => 0x2000,
            SnesButton::Start => 0x1000,
            SnesButton::Up => 0x0800,
            SnesButton::Down => 0x0400,
            SnesButton::Left => 0x0200,
            SnesButton::Right => 0x0100,
            SnesButton::A => 0x0080,
            SnesButton::X => 0x0040,
            SnesButton::L => 0x0020,
            SnesButton::R => 0x0010,
        }
    }

    /// Display label for the remap UI.
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            SnesButton::B => "B",
            SnesButton::Y => "Y",
            SnesButton::Select => "Select",
            SnesButton::Start => "Start",
            SnesButton::Up => "Up",
            SnesButton::Down => "Down",
            SnesButton::Left => "Left",
            SnesButton::Right => "Right",
            SnesButton::A => "A",
            SnesButton::X => "X",
            SnesButton::L => "L",
            SnesButton::R => "R",
        }
    }
}

/// One pair `(SnesButton, egui::Key)` flattened for JSON. We
/// serialize `Key` via its canonical name (`Key::name()` /
/// `Key::from_name()` — egui's own round-trip) so the file is
/// human-readable and doesn't depend on the enum's numeric layout.
#[derive(Serialize, Deserialize)]
struct Binding {
    button: SnesButton,
    key: String,
}

/// Player-1 keyboard binding map. Each entry pairs a SNES button to
/// an `egui::Key`. Defaults to the Mesen2 "Arrow keys" preset (see
/// the file-level doc).
#[derive(Clone)]
pub(crate) struct KeyBindings {
    /// 12 bindings indexed by [`SnesButton::ALL`].
    bindings: [(SnesButton, Key); 12],
}

impl Default for KeyBindings {
    fn default() -> Self {
        // Mesen2 "Arrow keys" preset for SNES (verified against
        // `UI/Config/KeyPresets.cs::ApplyArrowLayout`).
        Self {
            bindings: [
                (SnesButton::B, Key::A),
                (SnesButton::Y, Key::Z),
                (SnesButton::Select, Key::E),
                (SnesButton::Start, Key::D),
                (SnesButton::Up, Key::ArrowUp),
                (SnesButton::Down, Key::ArrowDown),
                (SnesButton::Left, Key::ArrowLeft),
                (SnesButton::Right, Key::ArrowRight),
                (SnesButton::A, Key::S),
                (SnesButton::X, Key::X),
                (SnesButton::L, Key::Q),
                (SnesButton::R, Key::W),
            ],
        }
    }
}

impl KeyBindings {
    /// Return the key currently bound to `button`.
    #[must_use]
    pub(crate) fn get(&self, button: SnesButton) -> Key {
        self.bindings
            .iter()
            .find(|(b, _)| *b == button)
            .map(|(_, k)| *k)
            .unwrap_or(Key::Space)
    }

    /// Rebind `button` to `key`. If another button was previously
    /// bound to the same key it is left in place — multiple SNES
    /// buttons sharing one keyboard key is harmless on the SNES
    /// side (they just light up together).
    pub(crate) fn set(&mut self, button: SnesButton, key: Key) {
        for slot in self.bindings.iter_mut() {
            if slot.0 == button {
                slot.1 = key;
                return;
            }
        }
    }

    /// Build the 16-bit `JOY1` mask from the current key-down state.
    /// Bit layout matches [`SnesButton::mask`].
    #[must_use]
    pub(crate) fn mask_from_input(&self, input: &egui::InputState) -> u16 {
        let mut m: u16 = 0;
        for (button, key) in self.bindings.iter() {
            if input.key_down(*key) {
                m |= button.mask();
            }
        }
        m
    }

    /// Persist the binding to disk as JSON. Returns the path written.
    pub(crate) fn save(&self) -> std::io::Result<PathBuf> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let entries: Vec<Binding> = self
            .bindings
            .iter()
            .map(|(b, k)| Binding {
                button: *b,
                key: k.name().to_owned(),
            })
            .collect();
        let json = serde_json::to_string_pretty(&entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&path, json)?;
        Ok(path)
    }

    /// Load the binding from the on-disk JSON file, or return the
    /// Mesen2 default preset if the file doesn't exist / fails to
    /// parse.
    #[must_use]
    pub(crate) fn load_or_default() -> Self {
        let path = match config_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        let Ok(json) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let entries: Vec<Binding> = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        let mut out = Self::default();
        for entry in entries {
            if let Some(key) = Key::from_name(&entry.key) {
                out.set(entry.button, key);
            }
        }
        out
    }
}

/// `~/.config/luna/input.json` on Linux / equivalent on macOS &
/// Windows. We avoid pulling in the `dirs` crate by reading
/// `$XDG_CONFIG_HOME` / `$HOME` manually — keeps the dependency
/// graph small.
fn config_path() -> std::io::Result<PathBuf> {
    let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no $HOME / $XDG_CONFIG_HOME",
        ));
    };
    Ok(base.join("luna").join("input.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_mesen2_arrow_preset() {
        let b = KeyBindings::default();
        // Mesen2 KeyPresets.cs::ApplyArrowLayout for SnesController:
        //   A = S, B = A, Up/Down/Left/Right = arrow keys,
        //   X = X, Y = Z, L = Q, R = W, Select = E, Start = D.
        assert_eq!(b.get(SnesButton::B), Key::A);
        assert_eq!(b.get(SnesButton::Y), Key::Z);
        assert_eq!(b.get(SnesButton::A), Key::S);
        assert_eq!(b.get(SnesButton::X), Key::X);
        assert_eq!(b.get(SnesButton::L), Key::Q);
        assert_eq!(b.get(SnesButton::R), Key::W);
        assert_eq!(b.get(SnesButton::Select), Key::E);
        assert_eq!(b.get(SnesButton::Start), Key::D);
        assert_eq!(b.get(SnesButton::Up), Key::ArrowUp);
        assert_eq!(b.get(SnesButton::Down), Key::ArrowDown);
        assert_eq!(b.get(SnesButton::Left), Key::ArrowLeft);
        assert_eq!(b.get(SnesButton::Right), Key::ArrowRight);
    }

    #[test]
    fn snes_button_masks_match_joy1_bit_layout() {
        // The 12 button masks together (without the 4-bit signature
        // gap) must equal $FFF0 — the same constant the `cpu_regs`
        // crate's `joypad_bit_layout_byss_udlr_axlr` test pins from
        // the bus side. If a future bit flip diverges these, both
        // tests fire.
        let all = SnesButton::ALL
            .iter()
            .map(|b| b.mask())
            .fold(0u16, |acc, m| acc | m);
        assert_eq!(all, 0xFFF0);
    }

    #[test]
    fn set_rebinds_a_single_button() {
        let mut b = KeyBindings::default();
        assert_eq!(b.get(SnesButton::B), Key::A);
        b.set(SnesButton::B, Key::Space);
        assert_eq!(b.get(SnesButton::B), Key::Space);
        // Y untouched.
        assert_eq!(b.get(SnesButton::Y), Key::Z);
    }
}
