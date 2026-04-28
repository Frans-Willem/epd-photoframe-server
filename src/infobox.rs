use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;
use image::RgbImage;
use reqwest::Client;

use crate::config::{InfoboxConfig, Units};
use crate::overlay::{draw_line, line_width, paint_rounded_rect, pixmap_to_rgb, place, rgb_to_pixmap};
use crate::weather::{self, DailyWeather};

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});
static ICON_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/WeatherIcons-Regular.ttf"))
        .expect("bundled icon font is invalid")
});

impl Units {
    fn temperature_suffix(self) -> &'static str {
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
    let temperature_text = format!(
        "{:.0}–{:.0}{}",
        weather.temperature_min.round(),
        weather.temperature_max.round(),
        cfg.units.temperature_suffix()
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
    let temperature_w = line_width(text_font, text_scale, &temperature_text);
    let weather_w = icon_w + icon_gap + temperature_w;

    let content_w = day_w.max(date_w).max(weather_w);
    let content_h = text_line_h + line_gap + text_line_h + line_gap + weather_line_h;

    let box_w = (content_w + 2.0 * internal_pad).ceil() as u32;
    let box_h = (content_h + 2.0 * internal_pad).ceil() as u32;

    let bg = cfg.background;
    let fg = cfg.foreground;

    let (px, py) = place(img.width(), img.height(), box_w, box_h, cfg.position, edge);

    let Some(mut pm) = rgb_to_pixmap(img) else {
        return;
    };
    paint_rounded_rect(&mut pm, px as f32, py as f32, box_w as f32, box_h as f32, radius, bg);

    let ox = px as f32 + internal_pad;
    let mut slot_top = py as f32 + internal_pad;
    let fg_ts = fg.to_tiny_skia();
    draw_line(&mut pm, text_font, text_scale, ox, slot_top + text_ascent, &day_text, fg_ts, None);
    slot_top += text_line_h + line_gap;
    draw_line(&mut pm, text_font, text_scale, ox, slot_top + text_ascent, &date_text, fg_ts, None);
    slot_top += text_line_h + line_gap;
    // Weather line: share a baseline so icon and temperature align visually.
    let baseline = slot_top + text_ascent.max(icon_ascent);
    draw_line(&mut pm, icon_font, icon_scale, ox, baseline, &icon_text, fg_ts, None);
    draw_line(&mut pm, text_font, text_scale, ox + icon_w + icon_gap, baseline, &temperature_text, fg_ts, None);

    pixmap_to_rgb(&pm, img);
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
    use crate::config::{ColorConfig, Position};
    use chrono::TimeZone;
    use image::Rgb;

    #[test]
    fn renders_without_panicking() {
        let mut img = RgbImage::from_pixel(800, 600, Rgb([120, 120, 120]));
        let cfg = InfoboxConfig {
            position: Position::BottomLeft,
            background: ColorConfig::rgba(255, 255, 255, 220),
            foreground: ColorConfig::rgb(0, 0, 0),
            latitude: 0.0,
            longitude: 0.0,
            units: Units::Metric,
        };
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let weather = DailyWeather { temperature_min: 8.0, temperature_max: 18.0, weather_code: 3 };
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
