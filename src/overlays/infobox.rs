use async_trait::async_trait;
use chrono::{DateTime, Datelike};
use chrono_tz::Tz;
use taffy::prelude::*;
use tiny_skia::Pixmap;

use super::drawable::{Drawable, walk};
use super::{Overlay, OverlayContext, ReadyOverlay};
use crate::config::{HeaderLayout, InfoboxConfig, Units, WeatherLayout};
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
        // Skip the network round-trip entirely when the configured layout
        // doesn't ask for weather.
        let (weather, weather_failed) = if matches!(self.cfg.weather_layout, WeatherLayout::None) {
            (None, false)
        } else {
            // `ctx.now` is already in the screen's timezone (set by the request
            // handler); pull the tz name for the weather query off of it.
            let tz_name = ctx.now.timezone().name();
            match weather::daily(
                ctx.http,
                self.cfg.latitude,
                self.cfg.longitude,
                tz_name,
                self.cfg.units,
            )
            .await
            {
                Ok(w) => (Some(w), false),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "weather fetch failed; infobox will show error");
                    (None, true)
                }
            }
        };
        Box::new(ReadyInfobox {
            cfg: self.cfg.clone(),
            now: ctx.now,
            weather,
            weather_failed,
        })
    }
}

struct ReadyInfobox {
    cfg: InfoboxConfig,
    now: DateTime<Tz>,
    weather: Option<DailyWeather>,
    /// True iff a weather fetch was attempted *and* failed. Distinguishes
    /// "weather not requested" (no degradation) from "weather fetch
    /// failed" (degraded — retry sooner).
    weather_failed: bool,
}

impl ReadyOverlay for ReadyInfobox {
    fn render(&self, canvas: &mut Pixmap) {
        let cfg = &self.cfg;
        let scr_min = canvas.width().min(canvas.height()) as f32;
        let text_px = (scr_min * 0.05).max(12.0);
        let icon_px = text_px * 1.3;
        let internal_pad = text_px * 0.6;
        let line_gap = text_px * 0.2;
        let icon_gap = text_px * 0.3;
        let edge = (scr_min * 0.03).round() as u32;
        let radius = text_px * 0.6;
        let fg = cfg.foreground;

        let mut tree: TaffyTree<Drawable> = TaffyTree::new();
        let mut children: Vec<NodeId> = Vec::new();

        // Header: zero, one, or two text lines.
        if matches!(cfg.header_layout, HeaderLayout::Day | HeaderLayout::DayDate) {
            let day_text = self.now.format("%A").to_string();
            children.push(text_leaf(&mut tree, day_text, text_px, fg));
        }
        if matches!(
            cfg.header_layout,
            HeaderLayout::Date | HeaderLayout::DayDate
        ) {
            let date_text = format!(
                "{} {} {}",
                self.now.day(),
                MONTHS[self.now.month0() as usize],
                self.now.year()
            );
            children.push(text_leaf(&mut tree, date_text, text_px, fg));
        }

        // Weather panel. `OnePlusFour` and `Five` aren't implemented yet —
        // they fall through to the same single-day line as `One` (which is
        // also the previous behaviour, before the layout fields existed).
        // Stage 4 steps 7–8 will replace these arms with the multi-day
        // tree-builders.
        match cfg.weather_layout {
            WeatherLayout::None => (),
            WeatherLayout::One | WeatherLayout::OnePlusFour | WeatherLayout::Five => {
                // On weather-fetch failure, keep the line shape (icon + text)
                // but show a short status string instead of the temperature
                // range. The full error goes to the server-side log.
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
                children.push(
                    tree.new_leaf_with_context(
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
                    .expect("create weather leaf"),
                );
            }
        }

        // Both sections empty → no infobox at all.
        if children.is_empty() {
            return;
        }

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
                &children,
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
        self.weather_failed
    }
}

fn text_leaf(
    tree: &mut TaffyTree<Drawable>,
    content: String,
    size: f32,
    color: crate::config::ColorConfig,
) -> NodeId {
    tree.new_leaf_with_context(
        Style::default(),
        Drawable::Text {
            content,
            size,
            color,
        },
    )
    .expect("create text leaf")
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
            header_layout: HeaderLayout::DayDate,
            weather_layout: WeatherLayout::One,
        }
    }

    /// Fresh transparent canvas — snapshots only contain pixels the
    /// overlay actually drew.
    fn canvas(w: u32, h: u32) -> Pixmap {
        Pixmap::new(w, h).expect("valid size")
    }

    fn sample_weather() -> DailyWeather {
        DailyWeather {
            temperature_min: 8.0,
            temperature_max: 18.0,
            weather_code: 3,
        }
    }

    fn ready_with(
        header: HeaderLayout,
        weather_layout: WeatherLayout,
        weather: Option<DailyWeather>,
        weather_failed: bool,
    ) -> ReadyInfobox {
        ReadyInfobox {
            cfg: InfoboxConfig {
                header_layout: header,
                weather_layout,
                ..cfg()
            },
            now: UTC.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap(),
            weather,
            weather_failed,
        }
    }

    #[test]
    fn renders_with_weather() {
        let mut pm = canvas(800, 600);
        ready_with(
            HeaderLayout::DayDate,
            WeatherLayout::One,
            Some(sample_weather()),
            false,
        )
        .render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/with_weather");
    }

    #[test]
    fn renders_without_weather() {
        let mut pm = canvas(800, 600);
        let r = ready_with(HeaderLayout::DayDate, WeatherLayout::One, None, true);
        assert!(r.degraded());
        r.render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/without_weather");
    }

    #[test]
    fn renders_header_only() {
        let mut pm = canvas(800, 600);
        let r = ready_with(HeaderLayout::DayDate, WeatherLayout::None, None, false);
        // Weather not requested → not degraded even though `weather` is None.
        assert!(!r.degraded());
        r.render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/header_only");
    }

    #[test]
    fn renders_weather_only() {
        let mut pm = canvas(800, 600);
        let r = ready_with(
            HeaderLayout::None,
            WeatherLayout::One,
            Some(sample_weather()),
            false,
        );
        r.render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/weather_only");
    }

    #[test]
    fn renders_day_only_header() {
        let mut pm = canvas(800, 600);
        ready_with(
            HeaderLayout::Day,
            WeatherLayout::One,
            Some(sample_weather()),
            false,
        )
        .render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/day_only_header");
    }

    #[test]
    fn renders_date_only_header() {
        let mut pm = canvas(800, 600);
        ready_with(
            HeaderLayout::Date,
            WeatherLayout::One,
            Some(sample_weather()),
            false,
        )
        .render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/date_only_header");
    }

    #[test]
    fn empty_layout_is_a_noop() {
        let mut pm = canvas(800, 600);
        ready_with(HeaderLayout::None, WeatherLayout::None, None, false).render(&mut pm);
        // Fresh Pixmap is fully transparent; render with both sections off
        // must leave it that way.
        assert!(pm.pixels().iter().all(|p| p.alpha() == 0));
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
