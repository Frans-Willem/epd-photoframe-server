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

/// Number of days the configured `weather_layout` wants from
/// `weather::forecast`. Returns 0 when no weather is shown — used to
/// skip the network call entirely.
fn forecast_days(layout: WeatherLayout) -> u32 {
    match layout {
        WeatherLayout::None => 0,
        WeatherLayout::One => 1,
        WeatherLayout::OnePlusFour | WeatherLayout::Five => 5,
    }
}

#[async_trait]
impl Overlay for Infobox {
    async fn preprocess(&self, ctx: &OverlayContext<'_>) -> Box<dyn ReadyOverlay + Send> {
        let days = forecast_days(self.cfg.weather_layout);
        let weather = if days == 0 {
            Vec::new()
        } else {
            // `ctx.now` is already in the screen's timezone (set by the request
            // handler); pull the tz name for the weather query off of it.
            let tz_name = ctx.now.timezone().name();
            match weather::forecast(
                ctx.http,
                self.cfg.latitude,
                self.cfg.longitude,
                tz_name,
                self.cfg.units,
                days,
            )
            .await
            {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "weather fetch failed; infobox will show error");
                    Vec::new()
                }
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
    /// One entry per day starting from "today" (index 0). Empty when
    /// the configured layout doesn't request weather, or when the
    /// fetch failed.
    weather: Vec<DailyWeather>,
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

        // Weather panel. Today's icon+range line — used by `One` directly,
        // and as the top of the `OnePlusFour` block.
        let today_line = |tree: &mut TaffyTree<Drawable>| {
            // On weather-fetch failure, keep the line shape (icon + text) but
            // show a short status string instead of the temperature range.
            // The full error goes to the server-side log.
            let (icon_glyph, weather_text) = match self.weather.first() {
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
            .expect("create weather leaf")
        };

        match cfg.weather_layout {
            WeatherLayout::None => (),
            WeatherLayout::One => {
                children.push(today_line(&mut tree));
            }
            WeatherLayout::OnePlusFour => {
                children.push(today_line(&mut tree));
                let style = CellStyle::one_plus_four(text_px);
                let row = compact_cell_row(
                    &mut tree,
                    &style,
                    self.now + chrono::Duration::days(1),
                    (0..4).map(|i| self.weather.get(i + 1)),
                    fg,
                    cfg.units,
                );
                children.push(row);
            }
            WeatherLayout::Five => {
                let style = CellStyle::five(text_px);
                let row = compact_cell_row(
                    &mut tree,
                    &style,
                    self.now,
                    (0..5).map(|i| self.weather.get(i)),
                    fg,
                    cfg.units,
                );
                children.push(row);
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
        // Catches both the "fetch failed" case (`weather` is empty) and the
        // "fetch returned fewer days than the layout asked for" case (e.g.
        // Open-Meteo returns 3 of 5 requested days). Layouts that don't
        // request weather (`forecast_days == 0`) are never degraded.
        self.weather.len() < forecast_days(self.cfg.weather_layout) as usize
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

/// Sizing for one row of compact day-cells. The two multi-day
/// layouts (`one-plus-four` and `five`) use slightly different
/// proportions; this struct keeps the rendering helpers shape-agnostic.
struct CellStyle {
    weekday_size: f32,
    icon_size: f32,
    temp_size: f32,
    gap_after_weekday: f32,
    gap_after_icon: f32,
    gap_after_max: f32,
    /// Horizontal space between adjacent cells in a row.
    cell_gap: f32,
    /// Extra top margin on the row beyond the parent flex's
    /// inter-section `line_gap`. Used by `one-plus-four` to give the
    /// row a 16 px (instead of 12 px) clearance from the today line.
    row_extra_top: f32,
}

impl CellStyle {
    /// Sizing for the row in `one-plus-four`. Per the Phase 2 spec
    /// at E1004 (`text_px = 60`): weekday 44, icon 56, temps 32,
    /// 12 px cell-to-cell gap, 16 px gap above the row.
    fn one_plus_four(text_px: f32) -> Self {
        Self {
            weekday_size: text_px * 0.73,
            icon_size: text_px * 0.93,
            temp_size: text_px * 0.53,
            gap_after_weekday: text_px * 0.13,
            gap_after_icon: text_px * 0.10,
            gap_after_max: text_px * 0.07,
            cell_gap: text_px * 0.20,
            row_extra_top: text_px * 0.07,
        }
    }

    /// Sizing for `five` — the 5-cell symmetric row. Per the Phase 2
    /// spec at E1004: weekday 36, icon 48, temps 28, 10 px cell-to-
    /// cell gap, no extra top margin (the row is the only weather
    /// section, separated from the header by the standard `line_gap`).
    fn five(text_px: f32) -> Self {
        Self {
            weekday_size: text_px * 0.60,
            icon_size: text_px * 0.80,
            temp_size: text_px * 0.47,
            gap_after_weekday: text_px * 0.13,
            gap_after_icon: text_px * 0.10,
            gap_after_max: text_px * 0.07,
            cell_gap: text_px * 0.17,
            row_extra_top: 0.0,
        }
    }
}

/// Build one compact day-cell — vertical stack of weekday letter,
/// weather icon, max temperature, min temperature. Sizes come from
/// `style` so the same builder serves both multi-day layouts.
fn compact_cell(
    tree: &mut TaffyTree<Drawable>,
    style: &CellStyle,
    weekday: String,
    icon_glyph: char,
    max_temp: String,
    min_temp: String,
    color: crate::config::ColorConfig,
) -> NodeId {
    let weekday_node = text_leaf(tree, weekday, style.weekday_size, color);
    let icon_node = tree
        .new_leaf_with_context(
            Style::default(),
            Drawable::Icon {
                glyph: icon_glyph,
                size: style.icon_size,
                color,
            },
        )
        .expect("create icon leaf");
    let max_node = text_leaf(tree, max_temp, style.temp_size, color);
    let min_node = text_leaf(tree, min_temp, style.temp_size, color);

    // Centre each row inside the cell so weekdays/temps of different
    // widths don't shift left.
    let mut centre = |node: NodeId, top_margin: f32| {
        tree.set_style(
            node,
            Style {
                margin: Rect {
                    top: length(top_margin),
                    left: zero(),
                    right: zero(),
                    bottom: zero(),
                },
                align_self: Some(AlignItems::Center),
                ..Default::default()
            },
        )
        .expect("set cell-child style");
    };
    centre(weekday_node, 0.0);
    centre(icon_node, style.gap_after_weekday);
    centre(max_node, style.gap_after_icon);
    centre(min_node, style.gap_after_max);

    tree.new_with_children(
        Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            align_items: Some(AlignItems::Center),
            ..Default::default()
        },
        &[weekday_node, icon_node, max_node, min_node],
    )
    .expect("create cell")
}

/// Horizontal row of compact day-cells. `first_date` is the date for
/// the first cell — `one-plus-four` passes `today + 1 day`; `five`
/// passes `today` itself. `weather` is an iterator of optional per-cell
/// data: missing entries render with a `wi-na` icon and `—` temperatures
/// so the row's structure stays consistent regardless of fetch outcome
/// (and tolerates Open-Meteo returning fewer or more days than asked).
fn compact_cell_row<'a>(
    tree: &mut TaffyTree<Drawable>,
    style: &CellStyle,
    first_date: DateTime<Tz>,
    weather: impl Iterator<Item = Option<&'a DailyWeather>>,
    color: crate::config::ColorConfig,
    units: Units,
) -> NodeId {
    let placeholder_temp = format!("—{}", units.temperature_suffix());
    let cells: Vec<NodeId> = weather
        .enumerate()
        .map(|(i, w)| {
            let date = first_date + chrono::Duration::days(i as i64);
            let weekday = date.format("%a").to_string();
            let fmt_temp = |t: f32| format!("{:.0}{}", t.round(), units.temperature_suffix());
            let max_temp =
                w.map_or_else(|| placeholder_temp.clone(), |w| fmt_temp(w.temperature_max));
            let min_temp =
                w.map_or_else(|| placeholder_temp.clone(), |w| fmt_temp(w.temperature_min));
            compact_cell(
                tree,
                style,
                weekday,
                wmo_icon(w.map(|w| w.weather_code)),
                max_temp,
                min_temp,
                color,
            )
        })
        .collect();

    tree.new_with_children(
        Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            gap: Size {
                width: length(style.cell_gap),
                height: length(0.0),
            },
            margin: Rect {
                top: length(style.row_extra_top),
                left: zero(),
                right: zero(),
                bottom: zero(),
            },
            ..Default::default()
        },
        &cells,
    )
    .expect("create cell row")
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
        weather: Vec<DailyWeather>,
    ) -> ReadyInfobox {
        ReadyInfobox {
            cfg: InfoboxConfig {
                header_layout: header,
                weather_layout,
                ..cfg()
            },
            now: UTC.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap(),
            weather,
        }
    }

    /// 5 days of sample weather: today + 4 future days. Different
    /// codes per day so the icons in the cells are visibly different.
    fn sample_forecast() -> Vec<DailyWeather> {
        [
            (8.0, 18.0, 1),   // today: partly cloudy
            (6.0, 14.0, 3),   // wed: cloudy
            (9.0, 19.0, 0),   // thu: sunny
            (10.0, 16.0, 61), // fri: rain
            (8.0, 13.0, 80),  // sat: showers
        ]
        .into_iter()
        .map(|(min, max, code)| DailyWeather {
            temperature_min: min,
            temperature_max: max,
            weather_code: code,
        })
        .collect()
    }

    #[test]
    fn renders_with_weather() {
        let mut pm = canvas(800, 600);
        ready_with(
            HeaderLayout::DayDate,
            WeatherLayout::One,
            vec![sample_weather()],
        )
        .render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/with_weather");
    }

    #[test]
    fn renders_without_weather() {
        let mut pm = canvas(800, 600);
        let r = ready_with(HeaderLayout::DayDate, WeatherLayout::One, Vec::new());
        assert!(r.degraded());
        r.render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/without_weather");
    }

    #[test]
    fn renders_header_only() {
        let mut pm = canvas(800, 600);
        let r = ready_with(HeaderLayout::DayDate, WeatherLayout::None, Vec::new());
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
            vec![sample_weather()],
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
            vec![sample_weather()],
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
            vec![sample_weather()],
        )
        .render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/date_only_header");
    }

    #[test]
    fn empty_layout_is_a_noop() {
        let mut pm = canvas(800, 600);
        ready_with(HeaderLayout::None, WeatherLayout::None, Vec::new()).render(&mut pm);
        // Fresh Pixmap is fully transparent; render with both sections off
        // must leave it that way.
        assert!(pm.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn renders_one_plus_four() {
        // The multi-day cells expect more horizontal room than 800×600 gives;
        // use a portrait E1004-shaped canvas so the row of 4 future-day cells
        // has somewhere to land without clipping.
        let mut pm = canvas(1200, 1600);
        ready_with(
            HeaderLayout::DayDate,
            WeatherLayout::OnePlusFour,
            sample_forecast(),
        )
        .render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/one_plus_four");
    }

    #[test]
    fn renders_five() {
        // `five` has no special today block — all 5 days share the compact
        // cell format. Same E1004-shaped canvas as `one_plus_four`.
        let mut pm = canvas(1200, 1600);
        ready_with(
            HeaderLayout::DayDate,
            WeatherLayout::Five,
            sample_forecast(),
        )
        .render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/five");
    }

    #[test]
    fn renders_one_plus_four_on_failed_fetch() {
        // Empty `weather` Vec simulates a fetch failure (or `weather_layout`
        // wanting more days than the API returned). The today line shows
        // "Weather error" and the 4 future cells render with `wi-na` icons
        // and `—` placeholder temperatures — structural shape preserved.
        let mut pm = canvas(1200, 1600);
        let r = ready_with(
            HeaderLayout::DayDate,
            WeatherLayout::OnePlusFour,
            Vec::new(),
        );
        assert!(r.degraded());
        r.render(&mut pm);
        crate::test_snapshot::assert_matches(&pm, "infobox/one_plus_four_failed");
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
