use async_trait::async_trait;
use chrono::{DateTime, Datelike};
use chrono_tz::Tz;
use taffy::prelude::*;
use tiny_skia::Pixmap;

use super::drawable::{Drawable, walk};
use super::{Overlay, OverlayContext, ReadyOverlay};
use crate::config::{InfoboxConfig, Units};
use crate::draw::place;
use crate::weather::{self, DailyWeather};

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
        let (icon_glyph, weather_text) = match self.weather {
            Some(w) => (
                wmo_icon(Some(w.weather_code)),
                format!(
                    "{:.0}–{:.0}{}",
                    w.temperature_min.round(),
                    w.temperature_max.round(),
                    cfg.units.temperature_suffix()
                ),
            ),
            None => (wmo_icon(None), "Weather error".to_string()),
        };

        let scr_min = canvas.width().min(canvas.height()) as f32;
        let text_px = (scr_min * 0.05).max(12.0);
        let icon_px = text_px * 1.3;
        let internal_pad = text_px * 0.6;
        let line_gap = text_px * 0.2;
        let icon_gap = text_px * 0.3;
        let edge = (scr_min * 0.03).round() as u32;
        let radius = text_px * 0.6;
        let fg = cfg.foreground;

        // Build the layout tree: a Column flex with three children (day, date,
        // weather line), with a rounded background attached to the root so it
        // paints first during the walk.
        let mut tree: TaffyTree<Drawable> = TaffyTree::new();
        let day = tree
            .new_leaf_with_context(
                Style::default(),
                Drawable::Text {
                    content: day_text,
                    size: text_px,
                    color: fg,
                },
            )
            .expect("create day leaf");
        let date = tree
            .new_leaf_with_context(
                Style::default(),
                Drawable::Text {
                    content: date_text,
                    size: text_px,
                    color: fg,
                },
            )
            .expect("create date leaf");
        let weather = tree
            .new_leaf_with_context(
                Style::default(),
                Drawable::IconText {
                    icon: icon_glyph,
                    icon_size: icon_px,
                    gap: icon_gap,
                    text: weather_text,
                    text_size: text_px,
                    color: fg,
                },
            )
            .expect("create weather leaf");
        let root = tree
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    padding: Rect::length(internal_pad),
                    gap: Size {
                        width: length(0.0),
                        height: length(line_gap),
                    },
                    ..Default::default()
                },
                &[day, date, weather],
            )
            .expect("create root");
        tree.set_node_context(
            root,
            Some(Drawable::Background {
                color: cfg.background,
                radius,
            }),
        )
        .expect("attach background context");

        tree.compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::MaxContent,
                height: AvailableSpace::MaxContent,
            },
            |_known, _avail, _id, ctx, _style| {
                ctx.map(|d: &mut Drawable| d.measure())
                    .unwrap_or(Size::ZERO)
            },
        )
        .expect("compute layout");

        let bbox = tree.layout(root).expect("root layout").size;
        let box_w = bbox.width.ceil() as u32;
        let box_h = bbox.height.ceil() as u32;
        let (px, py) = place(
            canvas.width(),
            canvas.height(),
            box_w,
            box_h,
            cfg.position,
            edge,
        );

        walk(&tree, root, px as f32, py as f32, &mut |x, y, w, h, d| {
            d.draw(canvas, x, y, w, h);
        });
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
