//! Battery indicator overlay, modelled on the Android 16 status bar battery.
//!
//! Geometry comes from
//! `frameworks/base/packages/SystemUI/res/drawable/battery_unified_frame.xml`
//! (24 × 14 viewport, body path in `battery_unified_frame_path_string`).
//! The Android source draws the body as a stroke of width 1.5 over a fill
//! (`battery_unified_frame_bg.xml`) sharing the same path. To reproduce the
//! visible silhouette as a single fill, the body coordinates here are the
//! source path expanded outward by half-stroke (0.75 viewport units), so
//! corner radii become `path_radius + 0.75`. The cap path is filled in the
//! source and used verbatim. The source draws the cap on the *left* and
//! relies on `isAutoMirrored = true` to flip it for LTR locales; we apply
//! that mirror here so the cap ends up on the right.

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::RgbImage;
use tiny_skia::{FillRule, Mask, Paint, Path, PathBuilder, Pixmap, Rect, Shader, Transform};

use crate::config::{BatteryIndicatorConfig, BatteryStyle, ColorConfig};
use crate::overlay::{
    asymmetric_rounded_rect_path, draw_line, line_width, pixmap_to_rgb, place, rgb_to_pixmap,
};

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});

// Viewport geometry, mirrored so the cap is on the right. Body extents are
// the source path expanded outward by half-stroke (0.75 viewport units).
const VIEWPORT_W: f32 = 24.0;
const VIEWPORT_H: f32 = 14.0;
const BODY_LEFT: f32 = 0.0;
const BODY_RIGHT: f32 = 22.0;
const BODY_TOP: f32 = 0.0;
const BODY_BOTTOM: f32 = 14.0;
const BODY_LEFT_R: f32 = 4.0; // away-from-cap side
const BODY_RIGHT_R: f32 = 3.0; // cap side
const CAP_LEFT: f32 = 22.5;
const CAP_RIGHT: f32 = 24.0;
const CAP_TOP: f32 = 3.0;
const CAP_BOTTOM: f32 = 11.0;
const CAP_R: f32 = 1.0;

// Inner-text geometry from BatteryPercentTextOnlyDrawable.kt. Source insets
// are LEFT=4, RIGHT=2 (so the canvas is centred over the body, not the full
// viewport); after the cap-to-right mirror they swap to 2 / 4. TEXT_SIZE and
// TEXT_VERTICAL_NUDGE are nudged up from the source's 10 / 1.5 to compensate
// for LiberationSans Bold's heavier metrics vs. Google Sans Bold.
const TEXT_CANVAS_LEFT: f32 = 2.0;
const TEXT_CANVAS_TOP: f32 = 2.0;
const TEXT_CANVAS_W: f32 = 18.0;
const TEXT_CANVAS_H: f32 = 10.0;
const TEXT_SIZE: f32 = 11.5;
const TEXT_VERTICAL_NUDGE: f32 = 2.5;

pub fn apply(img: &mut RgbImage, cfg: &BatteryIndicatorConfig, pct: u8) {
    render(img, cfg, pct.min(100));
}

/// Pixel-space layout of the icon. `ox`/`oy` are the top-left of the
/// viewport in the destination pixmap; `scale` converts viewport units to
/// pixels.
#[derive(Copy, Clone)]
struct Layout {
    ox: f32,
    oy: f32,
    scale: f32,
}

impl Layout {
    fn body_x(&self) -> f32 { self.ox + BODY_LEFT * self.scale }
    fn body_y(&self) -> f32 { self.oy + BODY_TOP * self.scale }
    fn body_w(&self) -> f32 { (BODY_RIGHT - BODY_LEFT) * self.scale }
    fn body_h(&self) -> f32 { (BODY_BOTTOM - BODY_TOP) * self.scale }
    fn level_fill_w(&self, pct: u8) -> f32 { self.body_w() * (pct as f32 / 100.0) }

    fn body_path(&self) -> Option<Path> {
        asymmetric_rounded_rect_path(
            self.body_x(), self.body_y(), self.body_w(), self.body_h(),
            BODY_LEFT_R * self.scale, BODY_RIGHT_R * self.scale,
        )
    }

    fn cap_path(&self) -> Option<Path> {
        asymmetric_rounded_rect_path(
            self.ox + CAP_LEFT * self.scale,
            self.oy + CAP_TOP * self.scale,
            (CAP_RIGHT - CAP_LEFT) * self.scale,
            (CAP_BOTTOM - CAP_TOP) * self.scale,
            0.0,
            CAP_R * self.scale,
        )
    }
}

/// Picks the level-fill colour for `pct`: the most restrictive (lowest
/// `below`) matching threshold, or `cfg.foreground` if none match.
fn effective_fg(cfg: &BatteryIndicatorConfig, pct: u8) -> ColorConfig {
    cfg.thresholds
        .iter()
        .filter(|t| pct < t.below)
        .min_by_key(|t| t.below)
        .map(|t| t.color)
        .unwrap_or(cfg.foreground)
}

fn solid_paint(c: ColorConfig) -> Paint<'static> {
    let mut p = Paint::default();
    p.anti_alias = true;
    p.shader = Shader::SolidColor(c.to_tiny_skia());
    p
}

fn render(img: &mut RgbImage, cfg: &BatteryIndicatorConfig, pct: u8) {
    let scr_min = img.width().min(img.height()) as f32;
    let outer_text_px = (scr_min * 0.035).max(12.0);
    let edge = (scr_min * 0.03).round() as u32;
    let scale = (outer_text_px * 1.4) / VIEWPORT_H;
    let font = &*TEXT_FONT;

    let (content_w, content_h): (f32, f32) = match cfg.style {
        BatteryStyle::Icon | BatteryStyle::Both => (VIEWPORT_W * scale, VIEWPORT_H * scale),
        BatteryStyle::Text => {
            let text = format!("{pct}%");
            let s = PxScale::from(outer_text_px);
            (line_width(font, s, &text), font.as_scaled(s).height())
        }
    };

    let (px, py) = place(
        img.width(), img.height(),
        content_w.ceil() as u32, content_h.ceil() as u32,
        cfg.position, edge,
    );

    let Some(mut pm) = rgb_to_pixmap(img) else { return };
    let layout = Layout { ox: px as f32, oy: py as f32, scale };
    let fg = effective_fg(cfg, pct);

    match cfg.style {
        BatteryStyle::Icon => {
            draw_silhouette(&mut pm, &layout, pct, fg, cfg.empty_color);
        }
        BatteryStyle::Text => {
            let text = format!("{pct}%");
            let s = PxScale::from(outer_text_px);
            let baseline = layout.oy + font.as_scaled(s).ascent();
            draw_line(&mut pm, font, s, layout.ox, baseline, &text, fg.to_tiny_skia(), None);
        }
        BatteryStyle::Both => {
            draw_silhouette(&mut pm, &layout, pct, fg, cfg.empty_color);
            draw_inverted_text(&mut pm, font, &layout, pct, fg, cfg.empty_color);
        }
    }

    pixmap_to_rgb(&pm, img);
}

/// Body silhouette filled with `empty`, level fill from the left in `fg`,
/// cap in `fg` at 100 % (the "fully charged" Android flourish) or `empty`
/// otherwise.
fn draw_silhouette(
    pm: &mut Pixmap,
    layout: &Layout,
    pct: u8,
    fg: ColorConfig,
    empty: ColorConfig,
) {
    let body = layout.body_path();
    let cap = layout.cap_path();
    let cap_color = if pct == 100 { fg } else { empty };

    if let (Some(p), true) = (body.as_ref(), empty.0.alpha() > 0) {
        pm.fill_path(p, &solid_paint(empty), FillRule::Winding, Transform::identity(), None);
    }
    if let (Some(p), true) = (cap.as_ref(), cap_color.0.alpha() > 0) {
        pm.fill_path(p, &solid_paint(cap_color), FillRule::Winding, Transform::identity(), None);
    }

    // Level fill: a flat rect masked by the body so the right edge stays a
    // sharp vertical line at any pct.
    let fill_w = layout.level_fill_w(pct);
    if fill_w > 0.0 {
        if let (Some(path), Some(rect), Some(mut mask)) = (
            body.as_ref(),
            Rect::from_xywh(layout.body_x(), layout.body_y(), fill_w, layout.body_h()),
            Mask::new(pm.width(), pm.height()),
        ) {
            mask.fill_path(path, FillRule::Winding, true, Transform::identity());
            pm.fill_path(
                &PathBuilder::from_rect(rect),
                &solid_paint(fg),
                FillRule::Winding, Transform::identity(), Some(&mask),
            );
        }
    }
}

/// Draws the percentage number centred over the body, painted twice with
/// inverted clip masks so the digits flip between `fg` (over the empty
/// area) and `empty` (over the level fill) at the boundary.
fn draw_inverted_text(
    pm: &mut Pixmap,
    font: &FontRef<'_>,
    layout: &Layout,
    pct: u8,
    fg: ColorConfig,
    empty: ColorConfig,
) {
    let text_px = TEXT_SIZE * layout.scale;
    let text = format!("{pct}");
    let px_scale = PxScale::from(text_px);
    let text_w = line_width(font, px_scale, &text);

    // Centre in the 18×10 sub-canvas. Vertical baseline mirrors
    // BatteryPercentTextOnlyDrawable.kt: (canvas_h + text_size)/2 - nudge.
    let canvas_x = layout.ox + TEXT_CANVAS_LEFT * layout.scale;
    let canvas_y = layout.oy + TEXT_CANVAS_TOP * layout.scale;
    let canvas_w = TEXT_CANVAS_W * layout.scale;
    let text_x = canvas_x + (canvas_w - text_w) / 2.0;
    let baseline = canvas_y
        + (TEXT_CANVAS_H * layout.scale + text_px) / 2.0
        - TEXT_VERTICAL_NUDGE * layout.scale;

    let Some(body) = layout.body_path() else { return };
    let Some(mut body_mask) = Mask::new(pm.width(), pm.height()) else { return };
    body_mask.fill_path(&body, FillRule::Winding, true, Transform::identity());

    // Pass 1: fg everywhere in the body. Inside the level-fill area this is
    // fg-on-fg (invisible) — pass 2 overpaints those pixels in `empty`.
    draw_line(pm, font, px_scale, text_x, baseline, &text, fg.to_tiny_skia(), Some(&body_mask));

    let fill_w = layout.level_fill_w(pct);
    if fill_w <= 0.0 { return }
    let Some(level_rect) = Rect::from_xywh(
        layout.body_x(), layout.body_y(), fill_w, layout.body_h(),
    ) else { return };
    let Some(mut level_mask) = Mask::new(pm.width(), pm.height()) else { return };
    level_mask.fill_path(
        &PathBuilder::from_rect(level_rect),
        FillRule::Winding, true, Transform::identity(),
    );
    intersect_mask(&mut level_mask, &body_mask);

    draw_line(pm, font, px_scale, text_x, baseline, &text, empty.to_tiny_skia(), Some(&level_mask));
}

fn intersect_mask(dst: &mut Mask, other: &Mask) {
    for (d, o) in dst.data_mut().iter_mut().zip(other.data().iter()) {
        *d = ((*d as u16 * *o as u16) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ColorConfig, Position};
    use image::Rgb;

    fn cfg(style: BatteryStyle) -> BatteryIndicatorConfig {
        BatteryIndicatorConfig {
            position: Position::TopLeft,
            foreground: ColorConfig::rgb(255, 255, 255),
            empty_color: ColorConfig::rgba(0, 0, 0, 200),
            style,
            thresholds: vec![],
        }
    }

    fn any_pixel_changed(img: &RgbImage, original: Rgb<u8>) -> bool {
        img.pixels().any(|p| p != &original)
    }

    #[test]
    fn renders_icon_only() {
        let mut img = RgbImage::from_pixel(800, 600, Rgb([120, 120, 120]));
        render(&mut img, &cfg(BatteryStyle::Icon), 75);
        assert!(any_pixel_changed(&img, Rgb([120, 120, 120])));
    }

    #[test]
    fn renders_text_only() {
        let mut img = RgbImage::from_pixel(800, 600, Rgb([120, 120, 120]));
        render(&mut img, &cfg(BatteryStyle::Text), 50);
        assert!(any_pixel_changed(&img, Rgb([120, 120, 120])));
    }

    #[test]
    fn renders_both() {
        let mut img = RgbImage::from_pixel(800, 600, Rgb([120, 120, 120]));
        render(&mut img, &cfg(BatteryStyle::Both), 100);
        assert!(any_pixel_changed(&img, Rgb([120, 120, 120])));
    }

    #[test]
    fn clamps_above_100() {
        let mut img = RgbImage::from_pixel(800, 600, Rgb([120, 120, 120]));
        apply(&mut img, &cfg(BatteryStyle::Both), 250);
    }

    #[test]
    fn threshold_picks_lowest_matching() {
        use crate::config::BatteryThreshold;
        let red = ColorConfig::rgb(255, 0, 0);
        let yellow = ColorConfig::rgb(255, 192, 0);
        let white = ColorConfig::rgb(255, 255, 255);
        let mut c = cfg(BatteryStyle::Icon);
        c.foreground = white;
        c.thresholds = vec![
            BatteryThreshold { below: 20, color: yellow },
            BatteryThreshold { below: 5, color: red },
        ];
        assert_eq!(effective_fg(&c, 100), white);
        assert_eq!(effective_fg(&c, 50), white);
        assert_eq!(effective_fg(&c, 20), white); // not below 20
        assert_eq!(effective_fg(&c, 19), yellow);
        assert_eq!(effective_fg(&c, 5), yellow); // not below 5
        assert_eq!(effective_fg(&c, 4), red);
        assert_eq!(effective_fg(&c, 0), red);
    }
}
