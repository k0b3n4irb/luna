//! The interface's icons: a few Lucide SVGs (ISC, see `NOTICE`), rasterised
//! once per size and screen scale into white textures that the widgets tint.

use std::collections::HashMap;

use egui::load::SizeHint;
use egui::{Color32, ColorImage, Context, Id, Image, TextureHandle, TextureOptions, Vec2};

/// An icon of the interface.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Icon {
    /// Continue.
    Play,
    /// Break.
    Pause,
    /// Step into.
    StepInto,
    /// Step over.
    StepOver,
    /// Step out.
    StepOut,
    /// The settings gear.
    Settings,
    /// A stop or a recording dot.
    Dot,
    /// A stopwatch.
    Timer,
}

impl Icon {
    /// Every icon, for the tests.
    #[cfg(test)]
    const ALL: [Self; 8] = [
        Self::Play,
        Self::Pause,
        Self::StepInto,
        Self::StepOver,
        Self::StepOut,
        Self::Settings,
        Self::Dot,
        Self::Timer,
    ];

    const fn svg(self) -> &'static str {
        match self {
            Self::Play => include_str!("../../assets/icons/play.svg"),
            Self::Pause => include_str!("../../assets/icons/pause.svg"),
            Self::StepInto => include_str!("../../assets/icons/corner-right-down.svg"),
            Self::StepOver => include_str!("../../assets/icons/redo-2.svg"),
            Self::StepOut => include_str!("../../assets/icons/corner-right-up.svg"),
            Self::Settings => include_str!("../../assets/icons/settings.svg"),
            Self::Dot => include_str!("../../assets/icons/circle.svg"),
            Self::Timer => include_str!("../../assets/icons/timer.svg"),
        }
    }

    /// The icon at `px` physical pixels square, white on transparent.
    // `usvg::Options` is not reachable from `egui_extras`, so it cannot be named here.
    #[allow(clippy::default_trait_access)]
    fn raster(self, px: u32) -> Result<ColorImage, String> {
        // Lucide strokes with `currentColor`: pin it to white, tinted at paint.
        let svg = self.svg().replace("currentColor", "#ffffff");
        egui_extras::image::load_svg_bytes_with_size(
            svg.as_bytes(),
            SizeHint::Size {
                width: px,
                height: px,
                maintain_aspect_ratio: true,
            },
            &Default::default(),
        )
    }
}

type Cache = HashMap<(Icon, u32), TextureHandle>;

/// `icon` as an image of `size` points, tinted `tint`.
pub(crate) fn image(ctx: &Context, icon: Icon, size: f32, tint: Color32) -> Image<'static> {
    let px = (size * ctx.pixels_per_point()).round().max(1.0) as u32;
    let tex = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_insert_with::<Cache>(Id::new("luna-icons"), Cache::default);
        cache.get(&(icon, px)).cloned()
    });
    let tex = tex.unwrap_or_else(|| {
        let img = icon.raster(px).unwrap_or_else(|_| ColorImage::example());
        let tex = ctx.load_texture(format!("icon-{icon:?}-{px}"), img, TextureOptions::LINEAR);
        ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with::<Cache>(Id::new("luna-icons"), Cache::default)
                .insert((icon, px), tex.clone());
        });
        tex
    });
    Image::new((tex.id(), Vec2::splat(size))).tint(tint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_rasterises_to_visible_pixels() {
        for icon in Icon::ALL {
            let img = icon.raster(32).unwrap_or_else(|e| panic!("{icon:?}: {e}"));
            assert_eq!(img.size, [32, 32], "{icon:?}");
            assert!(
                img.pixels.iter().any(|p| p.a() > 0),
                "{icon:?} drew nothing"
            );
        }
    }
}
