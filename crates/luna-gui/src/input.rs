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

/// One pair `(SnesButton, KeyCode)` flattened for JSON. `player` is
/// `#[serde(default)]` = 0, so an older `input.json` (Player-1-only, no
/// `player` field) still loads as Player 1 untouched.
#[derive(Serialize, Deserialize)]
struct Binding {
    #[serde(default)]
    player: usize,
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

/// Number of controller ports luna drives from the keyboard: Player 1 and
/// Player 2 (both fully handled by the emulator — `$4017` + auto-read JOY2).
pub(crate) const NUM_PLAYERS: usize = 2;

/// "Arrow keys" preset (Mesen2 `UI/Config/KeyPresets.cs::ApplyArrowLayout`):
/// d-pad on the arrow cluster, buttons on the left-hand QWERTY block. This is
/// the Player-1 factory default.
const P1_ARROWS: [(SnesButton, KeyCode); 12] = [
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
];

/// "WASD" preset: d-pad on `WASD`, buttons on the cluster around it
/// (`Q`/`E` = Y/X, `R`/`T` = L/R, `F`/`G` = B/A, `C`/`V` = Select/Start).
/// Disjoint from the Player-2 default so the two pads can coexist.
const P1_WASD: [(SnesButton, KeyCode); 12] = [
    (SnesButton::B, KeyCode::KeyF),
    (SnesButton::Y, KeyCode::KeyQ),
    (SnesButton::Select, KeyCode::KeyC),
    (SnesButton::Start, KeyCode::KeyV),
    (SnesButton::Up, KeyCode::KeyW),
    (SnesButton::Down, KeyCode::KeyS),
    (SnesButton::Left, KeyCode::KeyA),
    (SnesButton::Right, KeyCode::KeyD),
    (SnesButton::A, KeyCode::KeyG),
    (SnesButton::X, KeyCode::KeyE),
    (SnesButton::L, KeyCode::KeyR),
    (SnesButton::R, KeyCode::KeyT),
];

/// A named keyboard layout a player can switch to with one click (Mesen2
/// ships a similar preset dropdown). Distinct from "Reset to defaults",
/// which restores the player's factory binding (P1 = Arrows, P2 = numpad).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(crate) enum KeyPreset {
    Arrows,
    Wasd,
}

impl KeyPreset {
    pub(crate) const ALL: [Self; 2] = [Self::Arrows, Self::Wasd];

    /// Display label for the preset buttons.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Arrows => "Arrows",
            Self::Wasd => "WASD",
        }
    }

    /// The 12-button layout this preset applies.
    #[must_use]
    pub(crate) const fn layout(self) -> [(SnesButton, KeyCode); 12] {
        match self {
            Self::Arrows => P1_ARROWS,
            Self::Wasd => P1_WASD,
        }
    }
}

/// Player-2 default. Mesen2 ships no P2 keyboard preset (it leaves the
/// second pad unbound), so this is luna's own: the numeric-keypad d-pad
/// plus the right-hand `IJKL`/`UO`/`HN` cluster — chosen to never collide
/// with the Player-1 keys above so both pads work out of the box.
const P2_DEFAULT: [(SnesButton, KeyCode); 12] = [
    (SnesButton::B, KeyCode::KeyK),
    (SnesButton::Y, KeyCode::KeyJ),
    (SnesButton::Select, KeyCode::KeyH),
    (SnesButton::Start, KeyCode::KeyN),
    (SnesButton::Up, KeyCode::Numpad8),
    (SnesButton::Down, KeyCode::Numpad2),
    (SnesButton::Left, KeyCode::Numpad4),
    (SnesButton::Right, KeyCode::Numpad6),
    (SnesButton::A, KeyCode::KeyL),
    (SnesButton::X, KeyCode::KeyI),
    (SnesButton::L, KeyCode::KeyU),
    (SnesButton::R, KeyCode::KeyO),
];

/// Keyboard binding map for both controller ports. `pads[0]` = Player 1,
/// `pads[1]` = Player 2. Each entry pairs a SNES button to a winit
/// `KeyCode` (physical key, layout-agnostic).
#[derive(Clone)]
pub(crate) struct KeyBindings {
    pads: [[(SnesButton, KeyCode); 12]; NUM_PLAYERS],
    hotkeys: [(Hotkey, KeyCode); 1],
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            pads: [P1_ARROWS, P2_DEFAULT],
            hotkeys: [(Hotkey::Screenshot, Hotkey::Screenshot.default_key())],
        }
    }
}

impl KeyBindings {
    #[must_use]
    pub(crate) fn get(&self, player: usize, button: SnesButton) -> KeyCode {
        self.pads
            .get(player)
            .and_then(|p| p.iter().find(|(b, _)| *b == button))
            .map_or(KeyCode::Space, |(_, k)| *k)
    }

    /// Rebind `player`'s `button` to `key`. Multiple SNES buttons sharing
    /// one keyboard key is harmless on the SNES side.
    pub(crate) fn set(&mut self, player: usize, button: SnesButton, key: KeyCode) {
        if let Some(pad) = self.pads.get_mut(player) {
            for slot in pad {
                if slot.0 == button {
                    slot.1 = key;
                    return;
                }
            }
        }
    }

    /// Restore one player's 12 pad bindings to the factory defaults
    /// (P1 = Mesen2 "Arrow keys"; P2 = luna's numpad cluster), leaving the
    /// other player + hotkeys untouched. Like Mesen2's "Reset to Default":
    /// applies in-memory; persist via [`Self::save`].
    pub(crate) fn reset_bindings(&mut self, player: usize) {
        if let Some(pad) = self.pads.get_mut(player) {
            *pad = Self::default().pads[player];
        }
    }

    /// Apply a named [`KeyPreset`] (Arrows / WASD) to `player`'s pad.
    /// Applies in-memory; persist via [`Self::save`].
    pub(crate) fn apply_preset(&mut self, player: usize, preset: KeyPreset) {
        if let Some(pad) = self.pads.get_mut(player) {
            *pad = preset.layout();
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

    /// Restore emulator hotkeys to their factory defaults (Screenshot =
    /// `F12`), leaving pad bindings untouched. Applies in-memory; persist
    /// via [`Self::save`].
    pub(crate) fn reset_hotkeys(&mut self) {
        self.hotkeys = Self::default().hotkeys;
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
            .pads
            .iter()
            .enumerate()
            .flat_map(|(player, pad)| {
                pad.iter().map(move |(b, k)| Binding {
                    player,
                    button: *b,
                    key: *k,
                })
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

    /// Build the 16-bit JOY mask for `player` from the current set of held
    /// keys. Player 0 → `set_joypad(0, …)`, Player 1 → `set_joypad(1, …)`.
    #[must_use]
    pub(crate) fn mask_from_pressed(&self, player: usize, pressed: &HashSet<KeyCode>) -> u16 {
        let Some(pad) = self.pads.get(player) else {
            return 0;
        };
        let mut m: u16 = 0;
        for (button, key) in pad {
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
            if let Some(pad) = out.pads.get_mut(entry.player) {
                for slot in pad {
                    if slot.0 == entry.button {
                        slot.1 = entry.key;
                    }
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
    fn default_p1_matches_mesen2_arrow_preset() {
        let b = KeyBindings::default();
        assert_eq!(b.get(0, SnesButton::B), KeyCode::KeyA);
        assert_eq!(b.get(0, SnesButton::Y), KeyCode::KeyZ);
        assert_eq!(b.get(0, SnesButton::A), KeyCode::KeyS);
        assert_eq!(b.get(0, SnesButton::X), KeyCode::KeyX);
        assert_eq!(b.get(0, SnesButton::L), KeyCode::KeyQ);
        assert_eq!(b.get(0, SnesButton::R), KeyCode::KeyW);
        assert_eq!(b.get(0, SnesButton::Select), KeyCode::KeyE);
        assert_eq!(b.get(0, SnesButton::Start), KeyCode::KeyD);
        assert_eq!(b.get(0, SnesButton::Up), KeyCode::ArrowUp);
        assert_eq!(b.get(0, SnesButton::Down), KeyCode::ArrowDown);
        assert_eq!(b.get(0, SnesButton::Left), KeyCode::ArrowLeft);
        assert_eq!(b.get(0, SnesButton::Right), KeyCode::ArrowRight);
    }

    #[test]
    fn default_p2_is_numpad_cluster_and_disjoint_from_p1() {
        let b = KeyBindings::default();
        assert_eq!(b.get(1, SnesButton::Up), KeyCode::Numpad8);
        assert_eq!(b.get(1, SnesButton::Down), KeyCode::Numpad2);
        assert_eq!(b.get(1, SnesButton::B), KeyCode::KeyK);
        // P1 and P2 must not share any physical key, or one press would
        // drive both pads.
        for &btn in &SnesButton::ALL {
            for &btn2 in &SnesButton::ALL {
                assert_ne!(
                    b.get(0, btn),
                    b.get(1, btn2),
                    "P1 {btn:?} and P2 {btn2:?} collide on one key"
                );
            }
        }
    }

    #[test]
    fn wasd_preset_applies_and_stays_disjoint_from_p2_default() {
        let mut b = KeyBindings::default();
        b.apply_preset(0, KeyPreset::Wasd);
        assert_eq!(b.get(0, SnesButton::Up), KeyCode::KeyW);
        assert_eq!(b.get(0, SnesButton::Left), KeyCode::KeyA);
        assert_eq!(b.get(0, SnesButton::B), KeyCode::KeyF);
        // P1 on WASD must still not collide with the P2 default pad.
        for &p1 in &SnesButton::ALL {
            for &p2 in &SnesButton::ALL {
                assert_ne!(b.get(0, p1), b.get(1, p2), "WASD P1 {p1:?} vs P2 {p2:?}");
            }
        }
        // Switching back to the Arrows preset restores the arrow d-pad.
        b.apply_preset(0, KeyPreset::Arrows);
        assert_eq!(b.get(0, SnesButton::Up), KeyCode::ArrowUp);
    }

    #[test]
    fn mask_is_per_player() {
        let b = KeyBindings::default();
        let mut p1 = HashSet::new();
        p1.insert(KeyCode::KeyS); // P1 A
        assert_eq!(b.mask_from_pressed(0, &p1), SnesButton::A.mask());
        assert_eq!(b.mask_from_pressed(1, &p1), 0, "P1's key drives no P2 bit");
        let mut p2 = HashSet::new();
        p2.insert(KeyCode::Numpad8); // P2 Up
        assert_eq!(b.mask_from_pressed(1, &p2), SnesButton::Up.mask());
        assert_eq!(b.mask_from_pressed(0, &p2), 0);
    }

    #[test]
    fn bindings_round_trip_player_index_through_serde() {
        // A Binding without a `player` field (legacy input.json) loads as 0.
        let legacy: Binding = serde_json::from_str(r#"{"button":"B","key":"KeyA"}"#).unwrap();
        assert_eq!(legacy.player, 0);
        // A P2 binding serialises and parses back with its player index.
        let b = Binding {
            player: 1,
            button: SnesButton::Up,
            key: KeyCode::Numpad8,
        };
        let json = serde_json::to_string(&b).unwrap();
        let back: Binding = serde_json::from_str(&json).unwrap();
        assert_eq!(back.player, 1);
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
    fn reset_bindings_restores_defaults_per_player_and_leaves_hotkeys() {
        let mut b = KeyBindings::default();
        b.set(0, SnesButton::B, KeyCode::KeyP);
        b.set(1, SnesButton::B, KeyCode::KeyP);
        b.set_hotkey(Hotkey::Screenshot, KeyCode::F2);
        b.reset_bindings(0);
        assert_eq!(
            b.get(0, SnesButton::B),
            KeyCode::KeyA,
            "P1 reset to default"
        );
        assert_eq!(b.get(1, SnesButton::B), KeyCode::KeyP, "P2 untouched");
        assert_eq!(
            b.get_hotkey(Hotkey::Screenshot),
            KeyCode::F2,
            "hotkeys untouched by a pad reset"
        );
        b.reset_bindings(1);
        assert_eq!(
            b.get(1, SnesButton::B),
            KeyCode::KeyK,
            "P2 reset to default"
        );
    }

    #[test]
    fn reset_hotkeys_restores_defaults_and_leaves_pad() {
        let mut b = KeyBindings::default();
        b.set(0, SnesButton::B, KeyCode::KeyP);
        b.set_hotkey(Hotkey::Screenshot, KeyCode::F2);
        b.reset_hotkeys();
        assert_eq!(
            b.get_hotkey(Hotkey::Screenshot),
            KeyCode::F12,
            "hotkeys reset to default"
        );
        assert_eq!(
            b.get(0, SnesButton::B),
            KeyCode::KeyP,
            "pad untouched by a hotkey reset"
        );
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
