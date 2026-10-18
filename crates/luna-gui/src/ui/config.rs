//! Input and hotkey configuration windows.

use super::{MenuAction, UiState};

pub(super) fn draw_input_config<F: FnMut(MenuAction)>(
    ctx: &egui::Context,
    state: &UiState<'_>,
    emit: &mut F,
) {
    use crate::input::{NUM_PLAYERS, SnesButton};
    // Which player tab is showing. Kept in egui memory so the window owns it
    // locally (no round-trip through app state for a pure view toggle).
    let tab_id = egui::Id::new("luna-input-player-tab");
    let mut player = ctx
        .data_mut(|d| d.get_temp::<usize>(tab_id))
        .unwrap_or(0)
        .min(NUM_PLAYERS - 1);
    // `open` drives egui's title-bar ✕ (top-right close, like the Debug
    // windows); a click sets it false and we toggle the panel off below.
    let mut open = true;
    egui::Window::new("Controller bindings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .max_height(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            // Player 1 / Player 2 tabs. Both ports are fully driven by the
            // emulator ($4017 + auto-read JOY2).
            ui.horizontal(|ui| {
                for p in 0..NUM_PLAYERS {
                    if ui
                        .selectable_label(player == p, format!("Player {}", p + 1))
                        .clicked()
                    {
                        player = p;
                    }
                }
            });
            ui.separator();
            let hint = if player == 0 {
                "Click a row's button to rebind it. Player 1 defaults to the \
                 Mesen2 \"Arrow keys\" preset."
            } else {
                "Click a row's button to rebind it. Player 2 defaults to the \
                 numeric-keypad d-pad + the IJKL/UO/HN cluster."
            };
            ui.label(egui::RichText::new(hint).color(egui::Color32::from_rgb(160, 160, 180)));
            ui.add_space(6.0);
            // One-click layout presets for the active player.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Preset:").weak().small());
                for preset in crate::input::KeyPreset::ALL {
                    if ui.button(preset.label()).clicked() {
                        emit(MenuAction::ApplyPreset(player, preset));
                    }
                }
            });
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    egui::Grid::new("luna-bindings-grid")
                        .num_columns(3)
                        .spacing([16.0, 6.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for &button in &SnesButton::ALL {
                                ui.label(egui::RichText::new(button.label()).strong());
                                let key = state.key_bindings.get(player, button);
                                let label = if state.pending_rebind == Some((player, button)) {
                                    "Press a key…".to_string()
                                } else {
                                    format!("{key:?}")
                                };
                                if ui
                                    .button(egui::RichText::new(label).monospace())
                                    .on_hover_text("Click to rebind")
                                    .clicked()
                                {
                                    emit(MenuAction::StartRebind(player, button));
                                }
                                ui.allocate_space(egui::vec2(1.0, 1.0));
                                ui.end_row();
                            }
                        });
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save & close").clicked() {
                    emit(MenuAction::SaveBindings);
                    emit(MenuAction::ToggleInputConfig);
                }
                ui.add_space(12.0);
                if ui
                    .button("Reset to defaults")
                    .on_hover_text("Restore this player's default bindings")
                    .clicked()
                {
                    emit(MenuAction::ResetBindings(player));
                }
            });
        });
    ctx.data_mut(|d| d.insert_temp(tab_id, player));
    if !open {
        emit(MenuAction::ToggleInputConfig);
    }
}

/// The Hotkeys rebind window (Settings → Hotkeys → Hotkeys…). Split out of
/// the controller-bindings window so emulator hotkeys (screenshot, …) live
/// in their own dedicated dialog. Shares the same `KeyBindings` store, so
/// "Save & close" persists pad + hotkey bindings together.
pub(super) fn draw_hotkey_config<F: FnMut(MenuAction)>(
    ctx: &egui::Context,
    state: &UiState<'_>,
    emit: &mut F,
) {
    use crate::input::Hotkey;
    // `open` drives egui's title-bar ✕ (top-right close, like the Debug
    // windows); a click sets it false and we toggle the panel off below.
    let mut open = true;
    egui::Window::new("Hotkeys")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .max_height(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new("Click a row's button to rebind an emulator hotkey.")
                    .color(egui::Color32::from_rgb(160, 160, 180)),
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    egui::Grid::new("luna-hotkeys-grid")
                        .num_columns(3)
                        .spacing([16.0, 6.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for &hotkey in &Hotkey::ALL {
                                ui.label(egui::RichText::new(hotkey.label()).strong());
                                let key = state.key_bindings.get_hotkey(hotkey);
                                let label = if state.pending_hotkey_rebind == Some(hotkey) {
                                    "Press a key…".to_string()
                                } else {
                                    format!("{key:?}")
                                };
                                if ui
                                    .button(egui::RichText::new(label).monospace())
                                    .on_hover_text("Click to rebind")
                                    .clicked()
                                {
                                    emit(MenuAction::StartRebindHotkey(hotkey));
                                }
                                ui.allocate_space(egui::vec2(1.0, 1.0));
                                ui.end_row();
                            }
                        });
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save & close").clicked() {
                    emit(MenuAction::SaveBindings);
                    emit(MenuAction::ToggleHotkeyConfig);
                }
                ui.add_space(12.0);
                if ui
                    .button("Reset to defaults")
                    .on_hover_text(
                        "Restore the factory hotkeys (Screenshot = F12, Save = F5, Load = F9, Pause = F2, Reset = F3)",
                    )
                    .clicked()
                {
                    emit(MenuAction::ResetHotkeys);
                }
            });
        });
    if !open {
        emit(MenuAction::ToggleHotkeyConfig);
    }
}
