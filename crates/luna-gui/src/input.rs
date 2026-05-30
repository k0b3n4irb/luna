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
    pub(crate) const ALL: [Self; 12] = [
        Self::B,
        Self::Y,
        Self::Select,
        Self::Start,
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::A,
        Self::X,
        Self::L,
        Self::R,
    ];

    /// Display label for the rebind UI.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::B => "B",
            Self::Y => "Y",
            Self::Select => "Select",
            Self::Start => "Start",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::A => "A",
            Self::X => "X",
            Self::L => "L",
            Self::R => "R",
        }
    }

    /// Bitmask within the 16-bit `JOY1` shift register.
    #[must_use]
    pub(crate) const fn mask(self) -> u16 {
        match self {
            Self::B => 0x8000,
            Self::Y => 0x4000,
            Self::Select => 0x2000,
            Self::Start => 0x1000,
            Self::Up => 0x0800,
            Self::Down => 0x0400,
            Self::Left => 0x0200,
            Self::Right => 0x0100,
            Self::A => 0x0080,
            Self::X => 0x0040,
            Self::L => 0x0020,
            Self::R => 0x0010,
        }
    }
}

/// One pair `(SnesButton, KeyCode)` flattened for JSON.
#[derive(Serialize, Deserialize)]
struct Binding {
    button: SnesButton,
    key: KeyCode,
}

/// Non-gamepad emulator hotkeys (screenshot, …). Remappable through
/// the same Input → Configure controller… modal as the pad buttons
/// and persisted next to them, but kept in their own file so an older
/// `input.json` without hotkeys keeps loading unchanged.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash, Serialize, Deserialize)]
pub(crate) enum Hotkey {
    /// Save a PNG of the current frame (default `F12`, like Mesen2).
    Screenshot,
}

impl Hotkey {
    pub(crate) const ALL: [Self; 1] = [Self::Screenshot];

    /// Display label for the rebind UI.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Screenshot => "Screenshot",
        }
    }

    /// Factory default key. Mesen2 binds screenshot to `F12`.
    const fn default_key(self) -> KeyCode {
        match self {
            Self::Screenshot => KeyCode::F12,
        }
    }
}

/// One pair `(Hotkey, KeyCode)` flattened for JSON.
#[derive(Serialize, Deserialize)]
struct HotkeyBinding {
    hotkey: Hotkey,
    key: KeyCode,
}

/// Player-1 keyboard binding map. Each entry pairs a SNES button to a
/// winit `KeyCode` (physical key). Defaults to the Mesen2 "Arrow keys"
/// preset.
#[derive(Clone)]
pub(crate) struct KeyBindings {
    bindings: [(SnesButton, KeyCode); 12],
    hotkeys: [(Hotkey, KeyCode); 1],
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
            hotkeys: [(Hotkey::Screenshot, Hotkey::Screenshot.default_key())],
        }
    }
}

impl KeyBindings {
    #[must_use]
    pub(crate) fn get(&self, button: SnesButton) -> KeyCode {
        self.bindings
            .iter()
            .find(|(b, _)| *b == button)
            .map_or(KeyCode::Space, |(_, k)| *k)
    }

    /// Rebind `button` to `key`. Multiple SNES buttons sharing one
    /// keyboard key is harmless on the SNES side.
    pub(crate) fn set(&mut self, button: SnesButton, key: KeyCode) {
        for slot in &mut self.bindings {
            if slot.0 == button {
                slot.1 = key;
                return;
            }
        }
    }

    /// Key currently bound to `hotkey`.
    #[must_use]
    pub(crate) fn get_hotkey(&self, hotkey: Hotkey) -> KeyCode {
        self.hotkeys
            .iter()
            .find(|(h, _)| *h == hotkey)
            .map_or_else(|| hotkey.default_key(), |(_, k)| *k)
    }

    /// Rebind `hotkey` to `key`.
    pub(crate) fn set_hotkey(&mut self, hotkey: Hotkey, key: KeyCode) {
        for slot in &mut self.hotkeys {
            if slot.0 == hotkey {
                slot.1 = key;
                return;
            }
        }
    }

    /// Reverse lookup: the hotkey bound to `key`, if any. Used by the
    /// event loop to dispatch a key press to its emulator action.
    #[must_use]
    pub(crate) fn hotkey_for(&self, key: KeyCode) -> Option<Hotkey> {
        self.hotkeys
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(h, _)| *h)
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

        // Hotkeys live in their own file so a pre-existing `input.json`
        // (pad bindings only) keeps round-tripping untouched.
        let hk_path = hotkeys_path()?;
        let hk_entries: Vec<HotkeyBinding> = self
            .hotkeys
            .iter()
            .map(|(h, k)| HotkeyBinding {
                hotkey: *h,
                key: *k,
            })
            .collect();
        let hk_json = serde_json::to_string_pretty(&hk_entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&hk_path, hk_json)?;
        Ok(path)
    }

    /// Build the 16-bit `JOY1` mask from the current set of held keys.
    #[must_use]
    pub(crate) fn mask_from_pressed(&self, pressed: &HashSet<KeyCode>) -> u16 {
        let mut m: u16 = 0;
        for (button, key) in &self.bindings {
            if pressed.contains(key) {
                m |= button.mask();
            }
        }
        m
    }

    pub(crate) fn load_or_default() -> Self {
        let Ok(path) = config_path() else {
            return Self::default();
        };
        let Ok(json) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(entries) = serde_json::from_str::<Vec<Binding>>(&json) else {
            return Self::default();
        };
        let mut out = Self::default();
        for entry in entries {
            for slot in &mut out.bindings {
                if slot.0 == entry.button {
                    slot.1 = entry.key;
                }
            }
        }
        // Hotkeys are optional: a missing / unparsable file leaves the
        // factory defaults (Screenshot = F12) in place.
        if let Ok(hk_path) = hotkeys_path()
            && let Ok(hk_json) = fs::read_to_string(&hk_path)
            && let Ok(hk_entries) = serde_json::from_str::<Vec<HotkeyBinding>>(&hk_json)
        {
            for entry in hk_entries {
                for slot in &mut out.hotkeys {
                    if slot.0 == entry.hotkey {
                        slot.1 = entry.key;
                    }
                }
            }
        }
        out
    }
}

/// `~/.config/luna/<file>` on Linux / equivalent on macOS & Windows.
fn config_file(file: &str) -> std::io::Result<PathBuf> {
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
    Ok(base.join("luna").join(file))
}

/// `~/.config/luna/input.json` — pad bindings.
fn config_path() -> std::io::Result<PathBuf> {
    config_file("input.json")
}

/// `~/.config/luna/hotkeys.json` — remappable emulator hotkeys.
fn hotkeys_path() -> std::io::Result<PathBuf> {
    config_file("hotkeys.json")
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
    fn screenshot_hotkey_defaults_to_f12_and_reverse_resolves() {
        let mut b = KeyBindings::default();
        assert_eq!(b.get_hotkey(Hotkey::Screenshot), KeyCode::F12);
        assert_eq!(b.hotkey_for(KeyCode::F12), Some(Hotkey::Screenshot));
        // Remap and confirm both directions follow.
        b.set_hotkey(Hotkey::Screenshot, KeyCode::F2);
        assert_eq!(b.get_hotkey(Hotkey::Screenshot), KeyCode::F2);
        assert_eq!(b.hotkey_for(KeyCode::F2), Some(Hotkey::Screenshot));
        assert_eq!(b.hotkey_for(KeyCode::F12), None);
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
