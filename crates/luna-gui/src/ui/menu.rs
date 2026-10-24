//! Menu bar, its video menus, and the force-mapper prompt.

use super::{MenuAction, UiState};

/// Inline "couldn't detect the mapper — load as…?" prompt (issue #88), shown
/// centered when a ROM fails auto-detection. Each button forces that mapper and
/// retries the pending load in one click; Cancel dismisses it.
pub(super) fn draw_force_mapper_prompt<F: FnMut(MenuAction)>(
    ctx: &egui::Context,
    file: &str,
    emit: &mut F,
) {
    use luna_api::MapperKind;
    egui::Window::new("Couldn't detect the cartridge mapper")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!(
                "\u{201C}{file}\u{201D} has no valid internal checksum, so luna can't \
                 tell LoROM from HiROM. Load it as:"
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (label, kind) in [
                    ("LoROM", MapperKind::LoRom),
                    ("HiROM", MapperKind::HiRom),
                    ("ExHiROM", MapperKind::ExHiRom),
                    ("SA-1", MapperKind::Sa1),
                ] {
                    if ui.button(label).clicked() {
                        emit(MenuAction::SetForcedMapper(Some(kind)));
                    }
                }
                ui.separator();
                if ui.button("Cancel").clicked() {
                    emit(MenuAction::DismissForcePrompt);
                }
            });
        });
}

/// Settings ▸ Video Size, Video Filter and Aspect Ratio, in Mesen2's order
/// and wording (`MainMenuViewModel.cs`), separators between its sections.
fn draw_video_menus<F: FnMut(MenuAction)>(ui: &mut egui::Ui, state: &UiState<'_>, emit: &mut F) {
    use luna_api::video::{AspectRatio, CUSTOM_ASPECT_RANGE, VideoFilter};
    ui.menu_button("Video Size", |ui| {
        for n in 1u8..=10 {
            let key = n % 10;
            if ui
                .radio(
                    state.checked_scale == u32::from(n),
                    format!("{n}x (Alt+{key})"),
                )
                .clicked()
            {
                emit(MenuAction::SetVideoScale(n));
                ui.close();
            }
        }
        ui.separator();
        let fs_key = state
            .key_bindings
            .get_hotkey(crate::input::Hotkey::Fullscreen);
        if ui.button(format!("Fullscreen ({fs_key:?})")).clicked() {
            emit(MenuAction::ToggleFullscreen);
            ui.close();
        }
    });
    ui.menu_button("Video Filter", |ui| {
        let mut group = 0;
        for filter in VideoFilter::ALL {
            if filter.group() != group {
                ui.separator();
                group = filter.group();
            }
            let radio = egui::RadioButton::new(state.video.filter == filter, filter.label());
            if ui
                .add_enabled(filter.is_available(), radio)
                .on_disabled_hover_text("Not in luna: Mesen2 offers this filter for the NES only")
                .clicked()
            {
                emit(MenuAction::SetVideoFilter(filter));
                ui.close();
            }
        }
        ui.separator();
        let mut bilinear = state.video.bilinear;
        if ui
            .checkbox(&mut bilinear, "Use bilinear interpolation")
            .clicked()
        {
            emit(MenuAction::ToggleBilinear);
            ui.close();
        }
    });
    ui.menu_button("Aspect Ratio", |ui| {
        let mut group = 0;
        for aspect in AspectRatio::ALL {
            if aspect.group() != group {
                ui.separator();
                group = aspect.group();
            }
            if ui
                .radio(state.video.aspect == aspect, aspect.label())
                .clicked()
            {
                emit(MenuAction::SetAspectRatio(aspect));
                ui.close();
            }
        }
        // Mesen2 sets this ratio in its Video options; luna has no such
        // window, so it sits under the Custom entry.
        let mut custom = state.video.custom_aspect;
        if ui
            .add(
                egui::DragValue::new(&mut custom)
                    .range(CUSTOM_ASPECT_RANGE)
                    .speed(0.01)
                    .fixed_decimals(3)
                    .prefix("Custom ratio: "),
            )
            .changed()
        {
            emit(MenuAction::SetCustomAspect((custom * 1000.0).round() as u32));
        }
    });
}

/// Draws the top menu strip. Returns the strip's bottom edge in logical
/// points — the game-frame layout anchors just below it.
// egui deprecated the ctx-level `TopBottomPanel::show` for `Panel::show_inside`,
// which needs a parent `Ui`; luna draws this strip straight on the `Context`
// (no root `Ui` exists in the hand-rolled winit loop). Revisit with the next
// egui bump that removes the alias.
#[allow(deprecated)]
pub(super) fn draw_menu_bar<F: FnMut(MenuAction)>(
    ctx: &egui::Context,
    state: &UiState<'_>,
    emit: &mut F,
) -> f32 {
    let response = egui::TopBottomPanel::top("luna-menu")
        .exact_height(28.0)
        .show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open ROM…").clicked() {
                        emit(MenuAction::OpenRom);
                        ui.close();
                    }
                    // Force the cartridge mapper on the next load (issue #88) —
                    // lets checksum-invalid homebrew/test ROMs that fail
                    // auto-detection load. Auto is the default.
                    ui.menu_button("Force mapper", |ui| {
                        use luna_api::MapperKind;
                        let options: [(&str, Option<MapperKind>); 5] = [
                            ("Auto-detect", None),
                            ("LoROM", Some(MapperKind::LoRom)),
                            ("HiROM", Some(MapperKind::HiRom)),
                            ("ExHiROM", Some(MapperKind::ExHiRom)),
                            ("SA-1", Some(MapperKind::Sa1)),
                        ];
                        for (label, kind) in options {
                            let selected = state.forced_mapper == kind;
                            if ui.radio(selected, label).clicked() {
                                emit(MenuAction::SetForcedMapper(kind));
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    // Reload the ROM in place + watch-mode auto-reload (issue #93).
                    if ui.button("Reload ROM").clicked() {
                        emit(MenuAction::ReloadRom);
                        ui.close();
                    }
                    let mut auto_reload = state.auto_reload;
                    if ui
                        .checkbox(&mut auto_reload, "Auto-reload on file change")
                        .clicked()
                    {
                        emit(MenuAction::ToggleAutoReload);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        emit(MenuAction::Quit);
                        ui.close();
                    }
                });
                ui.menu_button("Emulation", |ui| {
                    let pause_key = state.key_bindings.get_hotkey(crate::input::Hotkey::Pause);
                    let reset_key = state.key_bindings.get_hotkey(crate::input::Hotkey::Reset);
                    let verb = if state.paused { "Resume" } else { "Pause" };
                    if ui.button(format!("{verb} ({pause_key:?})")).clicked() {
                        emit(MenuAction::PauseToggle);
                        ui.close();
                    }
                    let fs_key = state
                        .key_bindings
                        .get_hotkey(crate::input::Hotkey::Fullscreen);
                    if ui.button(format!("Fullscreen ({fs_key:?})")).clicked() {
                        emit(MenuAction::ToggleFullscreen);
                        ui.close();
                    }
                    let step_key = state
                        .key_bindings
                        .get_hotkey(crate::input::Hotkey::StepInstruction);
                    if ui
                        .button(format!("Step instruction ({step_key:?})"))
                        .clicked()
                    {
                        emit(MenuAction::StepInstruction);
                        ui.close();
                    }
                    let stepf_key = state
                        .key_bindings
                        .get_hotkey(crate::input::Hotkey::StepFrame);
                    if ui.button(format!("Step frame ({stepf_key:?})")).clicked() {
                        emit(MenuAction::StepFrame);
                        ui.close();
                    }
                    if ui.button(format!("Reset ({reset_key:?})")).clicked() {
                        emit(MenuAction::Reset);
                        ui.close();
                    }
                    ui.separator();
                    // Input recording (issue #83): capture joypad play, export
                    // it as a replayable `frame:mask` --input script.
                    let rec_label = if state.recording_input {
                        "\u{23FA} Stop recording (export --input script)"
                    } else {
                        "\u{23FA} Record input"
                    };
                    if ui.button(rec_label).clicked() {
                        emit(MenuAction::ToggleInputRecording);
                        ui.close();
                    }
                    ui.separator();
                    let save_key = state
                        .key_bindings
                        .get_hotkey(crate::input::Hotkey::SaveState);
                    let load_key = state
                        .key_bindings
                        .get_hotkey(crate::input::Hotkey::LoadState);
                    ui.menu_button(format!("Save state ({save_key:?})"), |ui| {
                        for slot in 1u8..=9 {
                            let occupied = state
                                .occupied_slots
                                .get(usize::from(slot - 1))
                                .copied()
                                .unwrap_or(false);
                            let label = if occupied {
                                format!("Slot {slot} \u{25cf}")
                            } else {
                                format!("Slot {slot}")
                            };
                            if ui.button(label).clicked() {
                                emit(MenuAction::SaveState(slot));
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button(format!("Load state ({load_key:?})"), |ui| {
                        for slot in 1u8..=9 {
                            let occupied = state
                                .occupied_slots
                                .get(usize::from(slot - 1))
                                .copied()
                                .unwrap_or(false);
                            let label = if occupied {
                                format!("Slot {slot} \u{25cf}")
                            } else {
                                format!("Slot {slot}")
                            };
                            if ui.button(label).clicked() {
                                emit(MenuAction::LoadState(slot));
                                ui.close();
                            }
                        }
                    });
                });
                ui.menu_button("Settings", |ui| {
                    // Grouped by area (Mesen2-style).
                    ui.label(egui::RichText::new("Audio").weak().small());
                    let mut vol = i32::from(state.volume_percent);
                    if ui
                        .add(egui::Slider::new(&mut vol, 0..=100).text("Volume"))
                        .changed()
                    {
                        emit(MenuAction::SetVolume(vol.clamp(0, 100) as u8));
                    }
                    if ui.selectable_label(state.muted, "Mute").clicked() {
                        emit(MenuAction::ToggleMute);
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Video").weak().small());
                    draw_video_menus(ui, state, emit);
                    if ui
                        .selectable_label(state.video.show_fps, "Show FPS")
                        .clicked()
                    {
                        emit(MenuAction::ToggleShowFps);
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Input").weak().small());
                    if ui
                        .selectable_label(state.show_input_config, "Controller…")
                        .clicked()
                    {
                        emit(MenuAction::ToggleInputConfig);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Hotkeys").weak().small());
                    if ui
                        .selectable_label(state.show_hotkey_config, "Hotkeys…")
                        .clicked()
                    {
                        emit(MenuAction::ToggleHotkeyConfig);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Devices").weak().small());
                    for port in 0u8..2 {
                        let cur = match state.port_device[port as usize] {
                            luna_api::PortDevice::Pad => "Gamepad",
                            luna_api::PortDevice::Mouse => "Mouse",
                            luna_api::PortDevice::SuperScope => "Super Scope",
                            luna_api::PortDevice::Multitap => "Super Multitap",
                            luna_api::PortDevice::None => "Nothing",
                        };
                        ui.menu_button(format!("Port {}: {cur}", port + 1), |ui| {
                            for (label, dev) in [
                                ("Gamepad", luna_api::PortDevice::Pad),
                                ("Mouse", luna_api::PortDevice::Mouse),
                                ("Super Scope", luna_api::PortDevice::SuperScope),
                                ("Super Multitap", luna_api::PortDevice::Multitap),
                                ("Nothing (unplugged)", luna_api::PortDevice::None),
                            ] {
                                if ui
                                    .radio(state.port_device[port as usize] == dev, label)
                                    .clicked()
                                {
                                    emit(MenuAction::SetPortDevice(port, dev));
                                    ui.close();
                                }
                            }
                        });
                    }
                });
                ui.menu_button("Tools", |ui| {
                    let key = state
                        .key_bindings
                        .get_hotkey(crate::input::Hotkey::Screenshot);
                    if ui.button(format!("Take screenshot ({key:?})")).clicked() {
                        emit(MenuAction::TakeScreenshot);
                        ui.close();
                    }
                });
                ui.menu_button("Debug", |ui| {
                    // Grouped by subsystem (CPU / SPC700 / PPU), separated by
                    // a light rule, Mesen2-style.
                    ui.label(egui::RichText::new("CPU").weak().small());
                    if ui
                        .selectable_label(state.show_cpu_state, "CPU state")
                        .clicked()
                    {
                        emit(MenuAction::ToggleCpuState);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_cpu_memory, "CPU memory")
                        .clicked()
                    {
                        emit(MenuAction::ToggleCpuMemory);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_cpu_disasm, "CPU disassembly")
                        .clicked()
                    {
                        emit(MenuAction::ToggleCpuDisasm);
                        ui.close();
                    }
                    if ui.button("Breakpoints").clicked() {
                        emit(MenuAction::ToggleBreakpoints);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_debugger, "Debugger")
                        .clicked()
                    {
                        emit(MenuAction::ToggleDebugger);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_spc_debugger, "SPC Debugger")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpcDebugger);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_memory_tools, "Memory Tools")
                        .clicked()
                    {
                        emit(MenuAction::ToggleMemoryTools);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_memory_search, "Memory Search")
                        .clicked()
                    {
                        emit(MenuAction::ToggleMemorySearch);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("SPC700").weak().small());
                    if ui
                        .selectable_label(state.show_spc700, "SPC700 state")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpc700);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_spc700_memory, "SPC700 memory")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpc700Memory);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_spc700_disasm, "SPC700 disassembly")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSpc700Disasm);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("PPU").weak().small());
                    if ui
                        .selectable_label(state.show_sprites, "Sprites (OAM)")
                        .clicked()
                    {
                        emit(MenuAction::ToggleSprites);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_palette, "Palette (CGRAM)")
                        .clicked()
                    {
                        emit(MenuAction::TogglePalette);
                        ui.close();
                    }
                    if ui.selectable_label(state.show_tilemap, "Tilemap").clicked() {
                        emit(MenuAction::ToggleTilemap);
                        ui.close();
                    }
                    if ui
                        .selectable_label(state.show_event_viewer, "Event Viewer")
                        .clicked()
                    {
                        emit(MenuAction::ToggleEventViewer);
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("System").weak().small());
                    if ui
                        .selectable_label(state.show_registers, "Registers")
                        .clicked()
                    {
                        emit(MenuAction::ToggleRegisters);
                        ui.close();
                    }
                });
                if let Some(title) = state.rom_title.as_deref() {
                    ui.add_space(20.0);
                    ui.label(
                        egui::RichText::new(title)
                            .color(egui::Color32::from_rgb(150, 150, 170))
                            .italics(),
                    );
                }
                if let Some(status) = state.screenshot_status.as_deref() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(status).color(egui::Color32::from_rgb(120, 200, 120)),
                    );
                }
                if let Some(msg) = state.break_status.as_deref() {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(240, 140, 60), msg);
                }
                if let Some(status) = state.save_state_status.as_deref() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(status).color(egui::Color32::from_rgb(120, 200, 120)),
                    );
                }
                if state.recording_input {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("\u{23FA} REC")
                            .color(egui::Color32::from_rgb(230, 70, 70))
                            .strong(),
                    );
                } else if let Some(status) = state.input_record_status.as_deref() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(status).color(egui::Color32::from_rgb(120, 200, 120)),
                    );
                }
                // Forced-mapper indicator + load-failure hint (issue #88).
                if let Some(kind) = state.forced_mapper {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(format!("mapper: {kind:?}"))
                            .color(egui::Color32::from_rgb(150, 150, 170))
                            .italics(),
                    );
                }
                if let Some(notice) = state.load_notice.as_deref() {
                    ui.add_space(16.0);
                    ui.colored_label(egui::Color32::from_rgb(240, 140, 60), notice);
                }
            });
        });
    response.response.rect.bottom()
}
