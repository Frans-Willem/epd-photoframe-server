use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, point};
use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;
use image::RgbImage;
use reqwest::Client;
use serde::Deserialize;

use crate::color::Color;
use crate::weather::{self, DailyWeather};

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});
static ICON_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/WeatherIcons-Regular.ttf"))
        .expect("bundled icon font is invalid")
});

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoboxConfig {
    pub position: Position,
    pub background: Color,
    pub foreground: Color,
    pub latitude: f32,
    pub longitude: f32,
    #[serde(default)]
    pub units: Units,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Units {
    #[default]
    Metric,
    Imperial,
}

impl Units {
    fn temp_suffix(self) -> &'static str {
        match self {
            Units::Metric => "°C",
            Units::Imperial => "°F",
        }
    }
}

pub async fn apply(
    img: &mut RgbImage,
    cfg: &InfoboxConfig,
    tz: &Tz,
    client: &Client,
) -> anyhow::Result<()> {
    let now = Utc::now().with_timezone(tz);
    let weather = weather::daily(client, cfg.latitude, cfg.longitude, tz.name(), cfg.units).await?;
    render(img, cfg, now, weather);
    Ok(())
}

fn render<T>(img: &mut RgbImage, cfg: &InfoboxConfig, now: DateTime<T>, weather: DailyWeather)
where
    T: chrono::TimeZone,
    T::Offset: std::fmt::Display,
{
    let day_text = now.format("%A").to_string();
    let date_text = format!("{} {} {}", now.day(), MONTHS[now.month0() as usize], now.year());
    let temp_text = format!(
        "{:.0}–{:.0}{}",
        weather.temp_min.round(),
        weather.temp_max.round(),
        cfg.units.temp_suffix()
    );
    let icon_text = wmo_icon(weather.weather_code).to_string();

    let scr_min = img.width().min(img.height()) as f32;
    let text_px = (scr_min * 0.05).max(12.0);
    let icon_px = text_px * 1.3;
    let internal_pad = text_px * 0.6;
    let line_gap = text_px * 0.2;
    let icon_gap = text_px * 0.3;
    let edge = (scr_min * 0.03).round() as u32;
    let radius = text_px * 0.6;

    let text_font: &FontRef<'static> = &TEXT_FONT;
    let icon_font: &FontRef<'static> = &ICON_FONT;
    let text_scale = PxScale::from(text_px);
    let icon_scale = PxScale::from(icon_px);
    let text_s = text_font.as_scaled(text_scale);
    let icon_s = icon_font.as_scaled(icon_scale);

    let text_line_h = text_s.height();
    let text_ascent = text_s.ascent();
    let icon_line_h = icon_s.height();
    let icon_ascent = icon_s.ascent();
    let weather_line_h = text_line_h.max(icon_line_h);

    let day_w = line_width(text_font, text_scale, &day_text);
    let date_w = line_width(text_font, text_scale, &date_text);
    let icon_w = line_width(icon_font, icon_scale, &icon_text);
    let temp_w = line_width(text_font, text_scale, &temp_text);
    let weather_w = icon_w + icon_gap + temp_w;

    let content_w = day_w.max(date_w).max(weather_w);
    let content_h = text_line_h + line_gap + text_line_h + line_gap + weather_line_h;

    let box_w = (content_w + 2.0 * internal_pad).ceil() as u32;
    let box_h = (content_h + 2.0 * internal_pad).ceil() as u32;

    let bg = cfg.background;
    let fg = cfg.foreground;

    let (px, py) = place(img.width(), img.height(), box_w, box_h, cfg.position, edge);
    paint_rounded_rect(img, px, py, box_w, box_h, radius, bg);

    let ox = px as f32 + internal_pad;
    let mut slot_top = py as f32 + internal_pad;
    // Day
    draw_line(img, text_font, text_scale, ox, slot_top + text_ascent, &day_text, fg);
    slot_top += text_line_h + line_gap;
    // Date
    draw_line(img, text_font, text_scale, ox, slot_top + text_ascent, &date_text, fg);
    slot_top += text_line_h + line_gap;
    // Weather line: share a baseline so icon and temp align visually.
    let baseline = slot_top + text_ascent.max(icon_ascent);
    draw_line(img, icon_font, icon_scale, ox, baseline, &icon_text, fg);
    draw_line(img, text_font, text_scale, ox + icon_w + icon_gap, baseline, &temp_text, fg);
}

fn line_width<F: Font>(font: &F, scale: PxScale, text: &str) -> f32 {
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

fn draw_line<F: Font>(
    dst: &mut RgbImage,
    font: &F,
    scale: PxScale,
    x: f32,
    baseline_y: f32,
    text: &str,
    fg: Color,
) {
    let s = font.as_scaled(scale);
    let mut cursor = x;
    let mut prev: Option<GlyphId> = None;
    let fg_a = fg.a as f32 / 255.0;
    for c in text.chars() {
        let g = s.glyph_id(c);
        if let Some(p) = prev {
            cursor += s.kern(p, g);
        }
        let glyph = g.with_scale_and_position(scale, point(cursor, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            let ox = bounds.min.x as i32;
            let oy = bounds.min.y as i32;
            outlined.draw(|gx, gy, coverage| {
                let px = ox + gx as i32;
                let py = oy + gy as i32;
                if px < 0 || py < 0 {
                    return;
                }
                let (px, py) = (px as u32, py as u32);
                if px >= dst.width() || py >= dst.height() {
                    return;
                }
                let a = coverage * fg_a;
                if a <= 0.0 {
                    return;
                }
                let pixel = dst.get_pixel_mut(px, py);
                pixel[0] = lerp_u8(pixel[0], fg.r, a);
                pixel[1] = lerp_u8(pixel[1], fg.g, a);
                pixel[2] = lerp_u8(pixel[2], fg.b, a);
            });
        }
        cursor += s.h_advance(g);
        prev = Some(g);
    }
}

fn paint_rounded_rect(
    img: &mut RgbImage,
    x0: i32,
    y0: i32,
    w: u32,
    h: u32,
    radius: f32,
    bg: Color,
) {
    let bg_a = bg.a as f32 / 255.0;
    if w == 0 || h == 0 || bg_a <= 0.0 {
        return;
    }
    let r = radius.max(0.0).min(w as f32 / 2.0).min(h as f32 / 2.0);
    let (wf, hf) = (w as f32, h as f32);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Compute coverage against the rounded-rect boundary.
            let cov = if r <= 0.0 {
                1.0
            } else {
                let (cx, cy) = if fx < r && fy < r {
                    (r, r)
                } else if fx > wf - r && fy < r {
                    (wf - r, r)
                } else if fx < r && fy > hf - r {
                    (r, hf - r)
                } else if fx > wf - r && fy > hf - r {
                    (wf - r, hf - r)
                } else {
                    // Interior / straight edges — fully covered.
                    blend_pixel(img, x0 + x as i32, y0 + y as i32, bg, bg_a);
                    continue;
                };
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                (r - d + 0.5).clamp(0.0, 1.0)
            };
            if cov > 0.0 {
                blend_pixel(img, x0 + x as i32, y0 + y as i32, bg, cov * bg_a);
            }
        }
    }
}

fn blend_pixel(img: &mut RgbImage, x: i32, y: i32, src: Color, src_a: f32) {
    if src_a <= 0.0 || x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    p[0] = lerp_u8(p[0], src.r, src_a);
    p[1] = lerp_u8(p[1], src.g, src_a);
    p[2] = lerp_u8(p[2], src.b, src_a);
}

fn place(
    scr_w: u32,
    scr_h: u32,
    box_w: u32,
    box_h: u32,
    pos: Position,
    edge: u32,
) -> (i32, i32) {
    let (sw, sh, bw, bh, e) = (scr_w as i32, scr_h as i32, box_w as i32, box_h as i32, edge as i32);
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

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 * (1.0 - t) + b as f32 * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
];

/// Maps an Open-Meteo (WMO 4677) weather code to a Weather Icons glyph.
/// Neutral (non-day/night) icons, since the infobox summarises the whole day.
fn wmo_icon(code: u32) -> char {
    match code {
        0 => '\u{F00D}',                   // wi-day-sunny
        1 => '\u{F00C}',                   // wi-day-sunny-overcast
        2 => '\u{F002}',                   // wi-day-cloudy
        3 => '\u{F013}',                   // wi-cloudy
        45 | 48 => '\u{F014}',             // wi-fog
        51 | 53 => '\u{F01C}',             // wi-sprinkle
        55 => '\u{F01A}',                  // wi-showers
        56 | 57 | 66 | 67 => '\u{F017}',   // wi-rain-mix
        61 | 63 | 65 => '\u{F019}',        // wi-rain
        71 | 73 | 75 | 85 | 86 => '\u{F01B}', // wi-snow
        77 => '\u{F076}',                  // wi-snowflake-cold
        80..=82 => '\u{F01A}',             // wi-showers
        95 | 96 | 99 => '\u{F01E}',        // wi-thunderstorm
        _ => '\u{F07B}',                   // wi-na
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use image::Rgb;

    #[test]
    fn renders_without_panicking() {
        let mut img = RgbImage::from_pixel(800, 600, Rgb([120, 120, 120]));
        let cfg = InfoboxConfig {
            position: Position::BottomLeft,
            background: Color::rgba(255, 255, 255, 220),
            foreground: Color::rgb(0, 0, 0),
            latitude: 0.0,
            longitude: 0.0,
            units: Units::Metric,
        };
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let weather = DailyWeather { temp_min: 8.0, temp_max: 18.0, weather_code: 3 };
        render(&mut img, &cfg, now, weather);
        // A corner pixel inside the box should no longer be the original grey.
        let corner = img.get_pixel(50, 550);
        assert_ne!(corner, &Rgb([120, 120, 120]));
    }

    #[test]
    fn covers_all_wmo_categories() {
        for code in [0u32, 1, 2, 3, 45, 48, 51, 55, 61, 66, 71, 77, 80, 95, 96, 999] {
            let _ = wmo_icon(code);
        }
    }
}
