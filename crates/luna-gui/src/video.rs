//! Settings ▸ Video Size / Video Filter / Aspect Ratio: the saved settings
//! and the window sizing of Mesen2's `MainWindow.axaml.cs` (`SetScale`,
//! `RendererPanel_LayoutUpdated`, `ProcessResolutionChange`). The filters and
//! the ratios themselves are `luna_api::video`.

use luna_api::video::{AspectRatio, CUSTOM_ASPECT_RANGE, VideoFilter};

/// The video settings, persisted to `video.json`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VideoSettings {
    /// On-screen frame-rate counter.
    pub show_fps: bool,
    /// Settings ▸ Video Filter.
    pub filter: VideoFilter,
    /// Settings ▸ Aspect Ratio.
    pub aspect: AspectRatio,
    /// The ratio `AspectRatio::Custom` shows (Mesen2 default 1.0).
    pub custom_aspect: f64,
    /// Settings ▸ Video Filter ▸ Use bilinear interpolation.
    pub bilinear: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            show_fps: false,
            filter: VideoFilter::None,
            aspect: AspectRatio::NoStretching,
            custom_aspect: 1.0,
            bilinear: false,
        }
    }
}

impl VideoSettings {
    /// The settings a `video.json` text holds; each absent, unreadable or
    /// unavailable value falls back to its default.
    pub(crate) fn parse(json: &str) -> Self {
        let mut s = Self::default();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
            return s;
        };
        if let Some(b) = v.get("show_fps").and_then(serde_json::Value::as_bool) {
            s.show_fps = b;
        }
        if let Some(f) = v
            .get("filter")
            .and_then(serde_json::Value::as_str)
            .and_then(VideoFilter::from_key)
            .filter(|f| f.is_available())
        {
            s.filter = f;
        }
        if let Some(a) = v
            .get("aspect_ratio")
            .and_then(serde_json::Value::as_str)
            .and_then(AspectRatio::from_key)
        {
            s.aspect = a;
        }
        if let Some(c) = v
            .get("custom_aspect_ratio")
            .and_then(serde_json::Value::as_f64)
            && c.is_finite()
        {
            s.custom_aspect = c.clamp(*CUSTOM_ASPECT_RANGE.start(), *CUSTOM_ASPECT_RANGE.end());
        }
        if let Some(b) = v.get("bilinear").and_then(serde_json::Value::as_bool) {
            s.bilinear = b;
        }
        s
    }

    /// The `video.json` text of these settings.
    pub(crate) fn to_json(&self) -> String {
        format!(
            "{{\n  \"show_fps\": {},\n  \"filter\": \"{}\",\n  \"aspect_ratio\": \"{}\",\n  \
             \"custom_aspect_ratio\": {},\n  \"bilinear\": {}\n}}\n",
            self.show_fps,
            self.filter.key(),
            self.aspect.key(),
            self.custom_aspect,
            self.bilinear
        )
    }

    /// Load from `video.json` in the config folder; defaults without it.
    pub(crate) fn load() -> Self {
        crate::input::config_file("video.json")
            .and_then(std::fs::read_to_string)
            .map_or_else(|_| Self::default(), |json| Self::parse(&json))
    }

    /// Persist next to the audio settings.
    pub(crate) fn save(&self) -> std::io::Result<()> {
        let path = crate::input::config_file("video.json")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_json())
    }

    /// Width over height of the shown picture for a `width` × `height` frame.
    pub(crate) fn ratio(&self, pal: bool, width: usize, height: usize) -> f64 {
        self.aspect.ratio(self.custom_aspect, pal, width, height)
    }
}

/// The picture's size inside an area: as tall as the area, or as wide when
/// that height would crop it (`RendererPanel_LayoutUpdated`).
pub(crate) fn fit(area_w: f64, area_h: f64, ratio: f64) -> (f64, f64) {
    let (mut w, mut h) = (area_h * ratio, area_h);
    if w.round() > area_w.round() {
        w = area_w;
        h = w / ratio;
    }
    (w, h)
}

/// Physical client size of a window showing the picture at `scale`, the
/// menu strip `menu_px` tall above it, never under `min` (`SetScale`,
/// windowed).
pub(crate) fn window_size(
    base_h: f64,
    ratio: f64,
    scale: f64,
    menu_px: f64,
    min: (f64, f64),
) -> (u32, u32) {
    let w = (base_h * ratio * scale).round().max(min.0);
    let h = (base_h * scale).max(min.1) + menu_px;
    (w.round() as u32, h.round() as u32)
}

/// Physical size of the picture at `scale` in a maximised or fullscreen
/// window (`SetScale`; Mesen2 multiplies the width by the ratio here, so the
/// height is what limits the picture).
pub(crate) fn renderer_size(base_w: f64, base_h: f64, ratio: f64, scale: f64) -> (f64, f64) {
    ((base_w * scale * ratio).round(), (base_h * scale).round())
}

/// The scale a windowed resolution change keeps: the whole scale the
/// window's game area is nearest to (`ProcessResolutionChange`; .NET
/// `Math.Round` rounds halves to even).
pub(crate) fn windowed_rescale(area_w: f64, area_h: f64, base_w: f64, base_h: f64) -> f64 {
    (area_w / base_w)
        .round_ties_even()
        .min((area_h / base_h).round_ties_even())
}

/// The scale a maximised or fullscreen resolution change keeps, to two
/// decimals (`ProcessResolutionChange`).
pub(crate) fn fullscreen_rescale(size: (f64, f64), base_w: f64, base_h: f64) -> f64 {
    let two = |x: f64| (x * 100.0).round_ties_even() / 100.0;
    two(size.0 / base_w).min(two(size.1 / base_h))
}

/// The Video Size entry ticked for a picture `height_px` physical pixels
/// tall (`GetScaleMenuItem`: the whole part of the scale).
pub(crate) fn checked_scale(height_px: f64, base_h: f64) -> u32 {
    (height_px / base_h) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_fall_back_one_by_one() {
        let s = VideoSettings {
            show_fps: true,
            filter: VideoFilter::Scale3x,
            aspect: AspectRatio::Custom,
            custom_aspect: 1.25,
            bilinear: true,
        };
        assert_eq!(VideoSettings::parse(&s.to_json()), s);
        assert_eq!(VideoSettings::parse("not json"), VideoSettings::default());
        // The file written before this change only held the counter.
        let old = VideoSettings::parse("{\n  \"show_fps\": true\n}\n");
        assert!(old.show_fps);
        assert_eq!(old.filter, VideoFilter::None);
        // A filter luna does not implement is not restored.
        assert_eq!(
            VideoSettings::parse("{\"filter\": \"HQ2x\"}").filter,
            VideoFilter::None
        );
        assert!(
            (VideoSettings::parse("{\"custom_aspect_ratio\": 50}").custom_aspect - 5.0).abs()
                < 1e-12
        );
        assert!(!VideoSettings::parse("{\"show_fps\": 1}").show_fps);
    }

    #[test]
    fn the_picture_is_height_led_unless_it_would_crop() {
        assert_eq!(fit(1000.0, 448.0, 256.0 / 224.0), (512.0, 448.0));
        let (w, h) = fit(400.0, 448.0, 2.0);
        assert!((w - 400.0).abs() < 1e-9 && (h - 200.0).abs() < 1e-9);
    }

    #[test]
    fn video_size_sets_the_window_from_the_height() {
        // 3x of a 224-row frame at 4:3: 672 rows, 896 columns, + the menu.
        assert_eq!(
            window_size(224.0, 4.0 / 3.0, 3.0, 28.0, (256.0, 224.0)),
            (896, 700)
        );
        // 1x under the minimum size is held at it.
        assert_eq!(
            window_size(224.0, 8.0 / 7.0, 1.0, 56.0, (512.0, 448.0)),
            (512, 504)
        );
        assert_eq!(renderer_size(256.0, 224.0, 1.0, 2.0), (512.0, 448.0));
    }

    #[test]
    fn a_resolution_change_keeps_the_nearest_whole_scale() {
        // 896 / 256 = 3.5 rounds to 4 (even), 672 / 224 = 3: 3 is kept.
        assert!((windowed_rescale(896.0, 672.0, 256.0, 224.0) - 3.0).abs() < 1e-12);
        // 224 → 239 rows at 3x: 672 / 239 = 2.81 → 3.
        assert!((windowed_rescale(768.0, 672.0, 256.0, 239.0) - 3.0).abs() < 1e-12);
        assert!((fullscreen_rescale((1024.0, 896.0), 256.0, 224.0) - 4.0).abs() < 1e-12);
        assert_eq!(checked_scale(671.9, 224.0), 2);
        assert_eq!(checked_scale(672.0, 224.0), 3);
    }
}
