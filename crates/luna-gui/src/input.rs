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
//! The binding is persisted to a JSON file under the platform's
//! standard config directory. The runtime model is winit's
//! `KeyCode` (physical, layout-agnostic key) so that QWERTY default
//! bindings survive a user remapping to AZERTY or DVORAK at the OS
//! level — the *physical* `KeyA` on the keyboard still means SNES B.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use winit::keyboard::KeyCode;

/// One of the 12 SNES controller buttons. Bit positions match the
/// `JOY1L`/`JOY1H` 16-bit shift register layout.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash, Serialize, Deserialize)]
pub(crate) enum SnesButton {
    B,
    Y,
    Select,
    Start,
    Up,
    Down,
    Left,
    Right,
    A,
    X,
    L,
    R,
}

impl SnesButton {
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

    /// Display label for the rebind UI.
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
}

/// One pair `(SnesButton, KeyCode)` flattened for JSON.
#[derive(Serialize, Deserialize)]
struct Binding {
    button: SnesButton,
    key: KeyCode,
}

/// Player-1 keyboard binding map. Each entry pairs a SNES button to a
/// winit `KeyCode` (physical key). Defaults to the Mesen2 "Arrow keys"
/// preset.
#[derive(Clone)]
pub(crate) struct KeyBindings {
    bindings: [(SnesButton, KeyCode); 12],
}

impl Default for KeyBindings {
    fn default() -> Self {
        // Mesen2 "Arrow keys" preset for SNES (verified against
        // `UI/Config/KeyPresets.cs::ApplyArrowLayout`).
        Self {
            bindings: [
                (SnesButton::B, KeyCode::KeyA),
                (SnesButton::Y, KeyCode::KeyZ),
                (SnesButton::Select, KeyCode::KeyE),
                (SnesButton::Start, KeyCode::KeyD),
                (SnesButton::Up, KeyCode::ArrowUp),
                (SnesButton::Down, KeyCode::ArrowDown),
                (SnesButton::Left, KeyCode::ArrowLeft),
                (SnesButton::Right, KeyCode::ArrowRight),
                (SnesButton::A, KeyCode::KeyS),
                (SnesButton::X, KeyCode::KeyX),
                (SnesButton::L, KeyCode::KeyQ),
                (SnesButton::R, KeyCode::KeyW),
            ],
        }
    }
}

impl KeyBindings {
    #[must_use]
    pub(crate) fn get(&self, button: SnesButton) -> KeyCode {
        self.bindings
            .iter()
            .find(|(b, _)| *b == button)
            .map(|(_, k)| *k)
            .unwrap_or(KeyCode::Space)
    }

    /// Rebind `button` to `key`. Multiple SNES buttons sharing one
    /// keyboard key is harmless on the SNES side.
    pub(crate) fn set(&mut self, button: SnesButton, key: KeyCode) {
        for slot in self.bindings.iter_mut() {
            if slot.0 == button {
                slot.1 = key;
                return;
            }
        }
    }

    /// Persist the binding to `~/.config/luna/input.json`. Best-effort:
    /// I/O errors bubble up so the caller can log but not panic.
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
                key: *k,
            })
            .collect();
        let json = serde_json::to_string_pretty(&entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&path, json)?;
        Ok(path)
    }

    /// Build the 16-bit `JOY1` mask from the current set of held keys.
    #[must_use]
    pub(crate) fn mask_from_pressed(&self, pressed: &HashSet<KeyCode>) -> u16 {
        let mut m: u16 = 0;
        for (button, key) in self.bindings.iter() {
            if pressed.contains(key) {
                m |= button.mask();
            }
        }
        m
    }

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
            for slot in out.bindings.iter_mut() {
                if slot.0 == entry.button {
                    slot.1 = entry.key;
                }
            }
        }
        out
    }
}

/// `~/.config/luna/input.json` on Linux / equivalent on macOS &
/// Windows.
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
        assert_eq!(b.get(SnesButton::B), KeyCode::KeyA);
        assert_eq!(b.get(SnesButton::Y), KeyCode::KeyZ);
        assert_eq!(b.get(SnesButton::A), KeyCode::KeyS);
        assert_eq!(b.get(SnesButton::X), KeyCode::KeyX);
        assert_eq!(b.get(SnesButton::L), KeyCode::KeyQ);
        assert_eq!(b.get(SnesButton::R), KeyCode::KeyW);
        assert_eq!(b.get(SnesButton::Select), KeyCode::KeyE);
        assert_eq!(b.get(SnesButton::Start), KeyCode::KeyD);
        assert_eq!(b.get(SnesButton::Up), KeyCode::ArrowUp);
        assert_eq!(b.get(SnesButton::Down), KeyCode::ArrowDown);
        assert_eq!(b.get(SnesButton::Left), KeyCode::ArrowLeft);
        assert_eq!(b.get(SnesButton::Right), KeyCode::ArrowRight);
    }

    #[test]
    fn snes_button_masks_match_joy1_bit_layout() {
        let all = SnesButton::ALL
            .iter()
            .map(|b| b.mask())
            .fold(0u16, |acc, m| acc | m);
        assert_eq!(all, 0xFFF0);
    }
}
