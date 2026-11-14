//! luna's look: colour tokens, radii, spacing, type scale, fonts.
//!
//! One [`Tokens`] value names every colour and measure the interface uses;
//! [`apply`] turns it into egui's `Visuals` and `Style`, and [`install_fonts`]
//! puts Inter (text) and `JetBrains Mono` (addresses, registers) in front of
//! egui's built-in fonts, which stay behind as the fallback for symbols.
//! Both run once per egui context (the main window and each debugger window
//! own one).

use std::sync::Arc;

use egui::epaint::Shadow;
use egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke,
    TextStyle, Visuals,
};

/// The family of Inter's semi-bold weight, for headings.
pub(crate) const SEMIBOLD: &str = "inter-semibold";

/// Every colour and measure of the interface.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    /// Panels and the window body.
    pub chrome: Color32,
    /// Raised cards, popups and menus.
    pub card: Color32,
    /// Text fields, code and data wells.
    pub field: Color32,
    /// Hairlines between and around surfaces.
    pub border: Color32,
    /// A resting button.
    pub control: Color32,
    /// A button under the pointer.
    pub hover: Color32,
    /// A button being pressed.
    pub pressed: Color32,
    /// Body text.
    pub text: Color32,
    /// Secondary text.
    pub text_dim: Color32,
    /// Disabled and placeholder text.
    pub text_faint: Color32,
    /// The accent: selection, focus, the primary action.
    pub accent: Color32,
    /// An error or a stop.
    pub danger: Color32,
    /// A caution.
    pub warning: Color32,
    /// Small corners: buttons, fields, chips.
    pub radius_sm: u8,
    /// Menus and cards.
    pub radius: u8,
    /// Windows.
    pub radius_lg: u8,
}

impl Tokens {
    /// The only theme: quiet violet-blue on near-black.
    pub(crate) const fn luna() -> Self {
        Self {
            chrome: Color32::from_rgb(18, 18, 23),
            card: Color32::from_rgb(25, 25, 32),
            field: Color32::from_rgb(14, 14, 19),
            border: Color32::from_rgb(42, 42, 53),
            control: Color32::from_rgb(33, 33, 42),
            hover: Color32::from_rgb(45, 45, 58),
            pressed: Color32::from_rgb(57, 57, 74),
            text: Color32::from_rgb(228, 228, 238),
            text_dim: Color32::from_rgb(164, 164, 182),
            text_faint: Color32::from_rgb(104, 104, 122),
            accent: Color32::from_rgb(140, 110, 255),
            danger: Color32::from_rgb(240, 98, 110),
            warning: Color32::from_rgb(236, 180, 76),
            radius_sm: 4,
            radius: 6,
            radius_lg: 10,
        }
    }
}

/// The compiled-in fonts: (egui name, bytes).
const FONTS: [(&str, &[u8]); 4] = [
    (
        "inter",
        include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    ),
    (
        "inter-medium",
        include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    ),
    (
        SEMIBOLD,
        include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    ),
    (
        "jetbrains-mono",
        include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"),
    ),
];

/// Makes Inter the proportional face and `JetBrains Mono` the monospace one,
/// with egui's own fonts kept behind them for the glyphs they lack.
pub(crate) fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in FONTS {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let front = |fonts: &mut FontDefinitions, family: FontFamily, name: &str| {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.to_owned());
    };
    front(&mut fonts, FontFamily::Proportional, "inter");
    front(&mut fonts, FontFamily::Monospace, "jetbrains-mono");
    fonts.families.insert(
        FontFamily::Name(SEMIBOLD.into()),
        vec![SEMIBOLD.to_owned(), "inter".to_owned()],
    );
    ctx.set_fonts(fonts);
}

/// A semi-bold Inter at `size`, for headings.
pub(crate) fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Sets egui's visuals and style from `t`.
pub(crate) fn apply(ctx: &Context, t: &Tokens) {
    let rsm = CornerRadius::same(t.radius_sm);
    let mut v = Visuals::dark();
    v.panel_fill = t.chrome;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0_f32, t.border);
    v.faint_bg_color = t.card;
    v.extreme_bg_color = t.field;
    v.code_bg_color = t.field;
    v.override_text_color = Some(t.text);
    v.hyperlink_color = t.accent;
    v.weak_text_color = Some(t.text_faint);
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.danger;
    v.selection.bg_fill = t.accent.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, t.accent);
    v.window_corner_radius = CornerRadius::same(t.radius_lg);
    v.menu_corner_radius = CornerRadius::same(t.radius);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(150),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.chrome;
    w.noninteractive.weak_bg_fill = t.chrome;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.text_dim);
    w.inactive.bg_fill = t.control;
    w.inactive.weak_bg_fill = t.control;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, t.border);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, t.text);
    w.hovered.bg_fill = t.hover;
    w.hovered.weak_bg_fill = t.hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, t.accent.linear_multiply(0.55));
    w.hovered.fg_stroke = Stroke::new(1.0_f32, t.text);
    w.active.bg_fill = t.pressed;
    w.active.weak_bg_fill = t.pressed;
    w.active.bg_stroke = Stroke::new(1.0_f32, t.accent);
    w.active.fg_stroke = Stroke::new(1.0_f32, t.text);
    w.open.bg_fill = t.pressed;
    w.open.weak_bg_fill = t.pressed;
    w.open.bg_stroke = Stroke::new(1.0_f32, t.accent.linear_multiply(0.55));
    for wv in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        wv.corner_radius = rsm;
    }
    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Heading, semibold(15.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 5.0);
        s.spacing.button_padding = egui::vec2(9.0, 3.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.interact_size = egui::vec2(24.0, 20.0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_installs_its_fonts_and_type_scale() {
        let ctx = Context::default();
        install_fonts(&ctx);
        apply(&ctx, &Tokens::luna());
        // A frame must lay text out in every family without panicking.
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("Inter 0123 ▶");
            ui.monospace("$80817C");
            ui.heading("Debugger");
        });
        let style = ctx.global_style();
        assert!((style.text_styles[&TextStyle::Body].size - 13.0).abs() < f32::EPSILON);
        assert_eq!(style.visuals.window_corner_radius, CornerRadius::same(10));
    }
}
