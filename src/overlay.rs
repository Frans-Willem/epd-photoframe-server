use ab_glyph::{Font, GlyphId, PxScale, ScaleFont, point};
use image::RgbImage;
use tiny_skia::{
    Color, FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, PremultipliedColorU8, Shader,
    Transform,
};

use crate::config::{ColorConfig, Position};

pub fn line_width<F: Font>(font: &F, scale: PxScale, text: &str) -> f32 {
    let s = font.as_scaled(scale);
    let mut prev: Option<GlyphId> = None;
    let mut w = 0.0;
    for c in text.chars() {
        let g = s.glyph_id(c);
        if let Some(p) = prev {
            w += s.kern(p, g);
        }
        w += s.h_advance(g);
        prev = Some(g);
    }
    w
}

pub fn paint_rounded_rect(
    pm: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    bg: ColorConfig,
) {
    if w <= 0.0 || h <= 0.0 || bg.0.alpha() == 0 {
        return;
    }
    let Some(path) = rounded_rect_path(x, y, w, h, radius) else {
        return;
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.shader = Shader::SolidColor(bg.to_tiny_skia());
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// Rasterize each glyph via `ab_glyph` into a small premul-RGBA pixmap, then
/// composite it onto `pm` with tiny-skia's source-over. If `mask` is provided,
/// the glyph composite is clipped to it.
#[allow(clippy::too_many_arguments)]
pub fn draw_line<F: Font>(
    pm: &mut Pixmap,
    font: &F,
    scale: PxScale,
    x: f32,
    baseline_y: f32,
    text: &str,
    fg: Color,
    mask: Option<&Mask>,
) {
    let s = font.as_scaled(scale);
    let mut cursor = x;
    let mut prev: Option<GlyphId> = None;
    for c in text.chars() {
        let g = s.glyph_id(c);
        if let Some(p) = prev {
            cursor += s.kern(p, g);
        }
        let glyph = g.with_scale_and_position(scale, point(cursor, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            let w = bounds.width().ceil() as u32;
            let h = bounds.height().ceil() as u32;
            if let Some(mut glyph_pm) = Pixmap::new(w, h) {
                let (fr, fg_, fb, fa) = (fg.red(), fg.green(), fg.blue(), fg.alpha());
                let pixels = glyph_pm.pixels_mut();
                outlined.draw(|gx, gy, coverage| {
                    if gx >= w || gy >= h {
                        return;
                    }
                    let a = (coverage * fa).clamp(0.0, 1.0);
                    let a8 = (a * 255.0).round() as u8;
                    let r8 = (fr * a * 255.0).round().min(a8 as f32) as u8;
                    let g8 = (fg_ * a * 255.0).round().min(a8 as f32) as u8;
                    let b8 = (fb * a * 255.0).round().min(a8 as f32) as u8;
                    let idx = (gy * w + gx) as usize;
                    if let Some(c) = PremultipliedColorU8::from_rgba(r8, g8, b8, a8) {
                        pixels[idx] = c;
                    }
                });
                pm.draw_pixmap(
                    bounds.min.x as i32,
                    bounds.min.y as i32,
                    glyph_pm.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    mask,
                );
            }
        }
        cursor += s.h_advance(g);
        prev = Some(g);
    }
}

pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    asymmetric_rounded_rect_path(x, y, w, h, radius, radius)
}

/// Rounded rectangle with independent radii on the left and right corner pairs.
/// Used by the battery indicator to reproduce the slightly asymmetric body
/// shape of the Android 16 battery vector drawable.
pub fn asymmetric_rounded_rect_path(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    left_r: f32,
    right_r: f32,
) -> Option<tiny_skia::Path> {
    const K: f32 = 0.552_284_7;
    let max_r = (w / 2.0).min(h / 2.0);
    let lr = left_r.clamp(0.0, max_r);
    let rr = right_r.clamp(0.0, max_r);
    let lcp = lr * K;
    let rcp = rr * K;
    let mut pb = PathBuilder::new();
    pb.move_to(x + lr, y);
    pb.line_to(x + w - rr, y);
    pb.cubic_to(x + w - rr + rcp, y, x + w, y + rr - rcp, x + w, y + rr);
    pb.line_to(x + w, y + h - rr);
    pb.cubic_to(
        x + w,
        y + h - rr + rcp,
        x + w - rr + rcp,
        y + h,
        x + w - rr,
        y + h,
    );
    pb.line_to(x + lr, y + h);
    pb.cubic_to(x + lr - lcp, y + h, x, y + h - lr + lcp, x, y + h - lr);
    pb.line_to(x, y + lr);
    pb.cubic_to(x, y + lr - lcp, x + lr - lcp, y, x + lr, y);
    pb.close();
    pb.finish()
}

pub fn rgb_to_pixmap(img: &RgbImage) -> Option<Pixmap> {
    let mut pm = Pixmap::new(img.width(), img.height())?;
    let src = img.as_raw();
    let dst = pm.pixels_mut();
    for (i, pixel) in dst.iter_mut().enumerate() {
        // Opaque source → premul == straight RGBA.
        *pixel = PremultipliedColorU8::from_rgba(src[i * 3], src[i * 3 + 1], src[i * 3 + 2], 255)
            .expect("alpha=255, always valid");
    }
    Some(pm)
}

pub fn pixmap_to_rgb(pm: &Pixmap, img: &mut RgbImage) {
    // Compositing onto an opaque base keeps alpha at 255, so premul == straight.
    let src = pm.pixels();
    let dst = img.as_mut();
    for (i, p) in src.iter().enumerate() {
        dst[i * 3] = p.red();
        dst[i * 3 + 1] = p.green();
        dst[i * 3 + 2] = p.blue();
    }
}

pub fn place(
    scr_w: u32,
    scr_h: u32,
    box_w: u32,
    box_h: u32,
    pos: Position,
    edge: u32,
) -> (i32, i32) {
    let (sw, sh, bw, bh, e) = (
        scr_w as i32,
        scr_h as i32,
        box_w as i32,
        box_h as i32,
        edge as i32,
    );
    let left = e;
    let right = (sw - bw - e).max(0);
    let top = e;
    let bottom = (sh - bh - e).max(0);
    let hcenter = ((sw - bw) / 2).max(0);
    let vcenter = ((sh - bh) / 2).max(0);
    match pos {
        Position::TopLeft => (left, top),
        Position::Top => (hcenter, top),
        Position::TopRight => (right, top),
        Position::Left => (left, vcenter),
        Position::Right => (right, vcenter),
        Position::BottomLeft => (left, bottom),
        Position::Bottom => (hcenter, bottom),
        Position::BottomRight => (right, bottom),
    }
}
