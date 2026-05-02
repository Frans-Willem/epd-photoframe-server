use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use async_trait::async_trait;
use chrono::{DateTime, Datelike};
use chrono_tz::Tz;
use tiny_skia::Pixmap;

use super::{Overlay, OverlayContext, ReadyOverlay};
use crate::config::{InfoboxConfig, Units};
use crate::draw::{draw_line, line_width, paint_rounded_rect, place};
use crate::weather::{self, DailyWeather};

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});
static ICON_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/WeatherIcons-Regular.ttf"))
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

/// Date / time / weather overlay. Captures its config at construction;
/// per request `preprocess` does the weather fetch and snapshots the
/// current time from the [`OverlayContext`].
pub struct Infobox {
    cfg: InfoboxConfig,
}

impl Infobox {
    pub fn new(cfg: InfoboxConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Overlay for Infobox {
    async fn preprocess(&self, ctx: &OverlayContext<'_>) -> Box<dyn ReadyOverlay + Send> {
        // `ctx.now` is already in the screen's timezone (set by the request
        // handler); pull the tz name for the weather query off of it.
        let tz_name = ctx.now.timezone().name();
        let weather = match weather::daily(
            ctx.http,
            self.cfg.latitude,
            self.cfg.longitude,
            tz_name,
            self.cfg.units,
        )
        .await
        {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "weather fetch failed; infobox will show error");
                None
            }
        };
        Box::new(ReadyInfobox {
            cfg: self.cfg.clone(),
            now: ctx.now,
            weather,
        })
    }
}

struct ReadyInfobox {
    cfg: InfoboxConfig,
    now: DateTime<Tz>,
    weather: Option<DailyWeather>,
}

impl ReadyOverlay for ReadyInfobox {
    fn render(&self, canvas: &mut Pixmap) {
        let cfg = &self.cfg;
        let day_text = self.now.format("%A").to_string();
        let date_text = format!(
            "{} {} {}",
            self.now.day(),
            MONTHS[self.now.month0() as usize],
            self.now.year()
        );
        // On weather-fetch failure, keep the line shape (icon + text) but show
        // a short status string instead of the temperature range. The full
        // error goes to the server-side log; the box is too narrow for a
        // useful detail.
        let (icon_text, weather_text) = match self.weather {
            Some(w) => (
                wmo_icon(Some(w.weather_code)).to_string(),
                format!(
                    "{:.0}–{:.0}{}",
                    w.temperature_min.round(),
                    w.temperature_max.round(),
                    cfg.units.temperature_suffix()
                ),
            ),
            None => (wmo_icon(None).to_string(), "Weather error".to_string()),
        };

        let scr_min = canvas.width().min(canvas.height()) as f32;
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
        let weather_text_w = line_width(text_font, text_scale, &weather_text);
        let weather_w = icon_w + icon_gap + weather_text_w;

        let content_w = day_w.max(date_w).max(weather_w);
        let content_h = text_line_h + line_gap + text_line_h + line_gap + weather_line_h;

        let box_w = (content_w + 2.0 * internal_pad).ceil() as u32;
        let box_h = (content_h + 2.0 * internal_pad).ceil() as u32;

        let bg = cfg.background;
        let fg = cfg.foreground;

        let (px, py) = place(
            canvas.width(),
            canvas.height(),
            box_w,
            box_h,
            cfg.position,
            edge,
        );

        paint_rounded_rect(
            canvas,
            px as f32,
            py as f32,
            box_w as f32,
            box_h as f32,
            radius,
            bg,
        );

        let ox = px as f32 + internal_pad;
        let mut slot_top = py as f32 + internal_pad;
        let fg_ts = fg.to_tiny_skia();
        draw_line(
            canvas,
            text_font,
            text_scale,
            ox,
            slot_top + text_ascent,
            &day_text,
            fg_ts,
            None,
        );
        slot_top += text_line_h + line_gap;
        draw_line(
            canvas,
            text_font,
            text_scale,
            ox,
            slot_top + text_ascent,
            &date_text,
            fg_ts,
            None,
        );
        slot_top += text_line_h + line_gap;
        // Weather line: share a baseline so icon and temperature align visually.
        let baseline = slot_top + text_ascent.max(icon_ascent);
        draw_line(
            canvas, icon_font, icon_scale, ox, baseline, &icon_text, fg_ts, None,
        );
        draw_line(
            canvas,
            text_font,
            text_scale,
            ox + icon_w + icon_gap,
            baseline,
            &weather_text,
            fg_ts,
            None,
        );
    }

    fn degraded(&self) -> bool {
        // Weather fetch failed → render shows "Weather error", retry sooner.
        self.weather.is_none()
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Maps an Open-Meteo (WMO 4677) weather code to a Weather Icons glyph.
/// Neutral (non-day/night) icons, since the infobox summarises the whole day.
/// `None` (no weather data at all — e.g. fetch failed) falls through to the
/// same `wi-na` glyph used for unrecognised codes.
fn wmo_icon(code: Option<u32>) -> char {
    match code {
        Some(0) => '\u{F00D}',                      // wi-day-sunny
        Some(1) => '\u{F00C}',                      // wi-day-sunny-overcast
        Some(2) => '\u{F002}',                      // wi-day-cloudy
        Some(3) => '\u{F013}',                      // wi-cloudy
        Some(45 | 48) => '\u{F014}',                // wi-fog
        Some(51 | 53) => '\u{F01C}',                // wi-sprinkle
        Some(55) => '\u{F01A}',                     // wi-showers
        Some(56 | 57 | 66 | 67) => '\u{F017}',      // wi-rain-mix
        Some(61 | 63 | 65) => '\u{F019}',           // wi-rain
        Some(71 | 73 | 75 | 85 | 86) => '\u{F01B}', // wi-snow
        Some(77) => '\u{F076}',                     // wi-snowflake-cold
        Some(80..=82) => '\u{F01A}',                // wi-showers
        Some(95 | 96 | 99) => '\u{F01E}',           // wi-thunderstorm
        _ => '\u{F07B}',                            // wi-na
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ColorConfig, Position};
    use chrono::TimeZone;
    use chrono_tz::UTC;

    fn cfg() -> InfoboxConfig {
        InfoboxConfig {
            position: Position::BottomLeft,
            background: ColorConfig::rgba(255, 255, 255, 220),
            foreground: ColorConfig::rgb(0, 0, 0),
            latitude: 0.0,
            longitude: 0.0,
            units: Units::Metric,
        }
    }

    /// Fresh transparent canvas — snapshots only contain pixels the
    /// overlay actually drew.
    fn canvas(w: u32, h: u32) -> Pixmap {
        Pixmap::new(w, h).expect("valid size")
    }

    fn ready(weather: Option<DailyWeather>) -> ReadyInfobox {
        ReadyInfobox {
            cfg: cfg(),
            now: UTC.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap(),
            weather,
        }
    }

    #[test]
    fn renders_with_weather() {
        let mut pm = canvas(800, 600);
        let weather = DailyWeather {
            temperature_min: 8.0,
            temperature_max: 18.0,
            weather_code: 3,
        };
        ready(Some(weather)).render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/with_weather");
    }

    #[test]
    fn renders_without_weather() {
        let mut pm = canvas(800, 600);
        let r = ready(None);
        assert!(r.degraded());
        r.render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/without_weather");
    }

    #[test]
    fn covers_all_wmo_categories() {
        for code in [
            0u32, 1, 2, 3, 45, 48, 51, 55, 61, 66, 71, 77, 80, 95, 96, 999,
        ] {
            let _ = wmo_icon(Some(code));
        }
        let _ = wmo_icon(None);
    }
}
