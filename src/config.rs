use serde::Deserialize;
use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;
use tiny_skia::ColorU8;

// ----- Top-level config -----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Optional MQTT broker. When present, each screen's `publish` list
    /// selects which device-supplied sensor values get forwarded; one Home
    /// Assistant discovery config is emitted per enabled sensor on startup.
    #[serde(default)]
    pub mqtt: Option<MqttConfig>,
    pub screens: Vec<ScreenConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MqttConfig {
    pub broker: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "default_mqtt_client_id")]
    pub client_id: String,
    #[serde(default = "default_mqtt_discovery_prefix")]
    pub discovery_prefix: String,
    #[serde(default = "default_mqtt_state_prefix")]
    pub state_prefix: String,
}

fn default_mqtt_port() -> u16 {
    1883
}
fn default_mqtt_client_id() -> String {
    "epd-photoframe-server".into()
}
fn default_mqtt_discovery_prefix() -> String {
    "homeassistant".into()
}
fn default_mqtt_state_prefix() -> String {
    "epd-photoframe".into()
}

#[derive(Debug, Deserialize)]
pub struct ScreenConfig {
    pub name: String,
    /// Human-readable label published over MQTT (used as the Home Assistant
    /// device name and prefixed onto each entity name). Defaults to `name` —
    /// set this when the URL slug isn't a nice label (e.g.
    /// `name = "living-room"`, `mqtt_name = "Photoframe Livingroom"`).
    #[serde(default)]
    pub mqtt_name: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Public Google Photos album share URL (e.g. `https://photos.app.goo.gl/...`
    /// or `https://photos.google.com/share/...`).
    pub share_url: String,
    /// How Google should resize the image to fit the screen.
    #[serde(default)]
    pub fit: FitMethod,
    /// What to put around the image if the returned image is smaller than the screen
    /// on either axis.
    #[serde(default)]
    pub background: BackgroundMethod,
    /// Optional overlay showing day/date/weather.
    #[serde(default)]
    pub infobox: Option<InfoboxConfig>,
    /// Optional overlay showing the device's reported battery level.
    /// Only renders when the device has reported a `battery_pct` value.
    #[serde(default)]
    pub battery_indicator: Option<BatteryIndicatorConfig>,
    /// When the screen should reshuffle (a new seed + cursor reset).
    /// Either `{ cron = "<expr>" }` (Quartz-style 7-field cron) or
    /// `{ natural = "<phrase>" }` (cron-lingo, e.g. "at 2 AM and 2 PM").
    /// If unset, the shuffle persists until the process restarts.
    #[serde(default)]
    pub rotate: Option<Rotate>,
    /// How much later than the next scheduled rotation the device is
    /// instructed to fetch the new image. Absorbs client-clock drift so a
    /// single scheduled rotation only needs a single wake. Accepts a
    /// humantime string, e.g. `"30s"`, `"15m"`, `"1h 30m"`. Defaults to zero.
    #[serde(default, deserialize_with = "deserialize_duration")]
    pub wake_delay: Duration,
    /// How long to ask the device to wait before retrying after a failed or
    /// partial render. Used as the Refresh-header value for both soft-failure
    /// (placeholder image) and hard-failure (HTTP 500) responses, clamped
    /// against `next_rotation + wake_delay` (the device's normal next-fetch
    /// target) so it never extends past one. Same humantime format as
    /// `wake_delay`. Defaults to `"1h"`.
    #[serde(
        default = "default_error_refresh",
        deserialize_with = "deserialize_duration"
    )]
    pub error_refresh: Duration,
    /// IANA timezone name (e.g. `Europe/Amsterdam`) used for rotation
    /// scheduling and the infobox. Defaults to the system timezone.
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub dither: DitherConfig,
    /// Sensors to forward to MQTT for this screen. Each entry maps to one or
    /// two Home Assistant sensors (battery covers both `battery_pct` and
    /// `battery_mv`); `last_seen` is a timestamp updated on every request.
    /// Duplicates in the TOML array are silently collapsed.
    #[serde(default = "default_publish")]
    pub publish: HashSet<Publish>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Publish {
    /// `?battery_mv=` and `?battery_pct=` — two Home Assistant sensors per
    /// screen.
    Battery,
    /// `?temp_c=` as a temperature sensor (°C).
    Temperature,
    /// `?humidity_pct=` as a humidity sensor (%).
    Humidity,
    /// `?power=battery|charging|full|fault` as an enum sensor.
    Power,
    /// Server-side wall-clock at request time, as a Home Assistant timestamp
    /// sensor. Useful as a heartbeat for screens that publish no other sensors.
    LastSeen,
}

fn default_publish() -> HashSet<Publish> {
    [Publish::Battery, Publish::LastSeen].into_iter().collect()
}

fn default_error_refresh() -> Duration {
    Duration::from_secs(3600)
}

fn deserialize_duration<'de, D>(d: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    humantime::parse_duration(&s).map_err(serde::de::Error::custom)
}

impl Config {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }
}

// ----- Fit / background -----------------------------------------------------

/// Server-side resize strategy — controls the Google URL suffix.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FitMethod {
    /// Centre-crop to requested size (`-c`).
    #[default]
    Crop,
    /// Content-aware crop to requested size (`-p`).
    SmartCrop,
    /// Stretch to requested size, ignoring aspect ratio (`-s`).
    Resize,
    /// Fit within requested size, preserving aspect ratio (no suffix).
    Contain,
}

/// Local padding strategy when the returned image is smaller than the screen.
#[derive(Debug, Clone)]
pub enum BackgroundMethod {
    /// Pad with a solid colour. Alpha is ignored.
    Solid(ColorConfig),
    /// Pad with a blurred cover-sized copy of the photo.
    Blur,
}

impl Default for BackgroundMethod {
    fn default() -> Self {
        Self::Solid(ColorConfig::rgb(255, 255, 255))
    }
}

impl FromStr for BackgroundMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "blur" {
            return Ok(Self::Blur);
        }
        ColorConfig::from_str(s).map(Self::Solid)
    }
}

impl<'de> Deserialize<'de> for BackgroundMethod {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ----- Colour --------------------------------------------------------------

/// An sRGB colour with an optional alpha channel, wrapping tiny-skia's exact
/// u8 representation. Alpha defaults to 255 (opaque) when parsed from a form
/// that doesn't specify it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorConfig(pub ColorU8);

impl ColorConfig {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(ColorU8::from_rgba(r, g, b, 255))
    }

    #[allow(dead_code)]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(ColorU8::from_rgba(r, g, b, a))
    }

    pub fn to_rgb(self) -> image::Rgb<u8> {
        image::Rgb([self.0.red(), self.0.green(), self.0.blue()])
    }

    /// Convert to tiny-skia's f32-normalised `Color` (what `Paint::shader` needs).
    pub fn to_tiny_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.0.red(), self.0.green(), self.0.blue(), self.0.alpha())
    }
}

impl FromStr for ColorConfig {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex).ok_or_else(|| format!("invalid hex colour `{s}`"));
        }
        if let Some(inner) = strip_call(s, "rgba") {
            let parts = split_args(inner);
            if parts.len() != 4 {
                return Err(format!("rgba() takes 4 components, got {}", parts.len()));
            }
            let r = parse_byte(parts[0])?;
            let g = parse_byte(parts[1])?;
            let b = parse_byte(parts[2])?;
            let a = parse_alpha(parts[3])?;
            return Ok(Self::rgba(r, g, b, a));
        }
        if let Some(inner) = strip_call(s, "rgb") {
            let parts = split_args(inner);
            if parts.len() != 3 {
                return Err(format!("rgb() takes 3 components, got {}", parts.len()));
            }
            let r = parse_byte(parts[0])?;
            let g = parse_byte(parts[1])?;
            let b = parse_byte(parts[2])?;
            return Ok(Self::rgb(r, g, b));
        }
        Err(format!(
            "expected `#RRGGBB`, `rgb(...)`, or `rgba(...)`, got `{s}`"
        ))
    }
}

impl<'de> Deserialize<'de> for ColorConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn parse_hex(hex: &str) -> Option<ColorConfig> {
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(ColorConfig::rgb(r, g, b))
}

fn strip_call<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let s = s.strip_prefix(name)?.trim_start();
    let s = s.strip_prefix('(')?;
    s.strip_suffix(')')
}

fn split_args(s: &str) -> Vec<&str> {
    s.split(',').map(|p| p.trim()).collect()
}

fn parse_byte(s: &str) -> Result<u8, String> {
    s.parse::<u8>()
        .map_err(|_| format!("invalid 0-255 byte `{s}`"))
}

/// CSS-style alpha: a float in `[0.0, 1.0]`, mapped to `[0, 255]`.
fn parse_alpha(s: &str) -> Result<u8, String> {
    let f: f32 = s.parse().map_err(|_| format!("invalid alpha `{s}`"))?;
    if !(0.0..=1.0).contains(&f) {
        return Err(format!("alpha {f} out of range [0.0, 1.0]"));
    }
    Ok((f * 255.0).round() as u8)
}

// ----- Rotation schedule ---------------------------------------------------

/// A rotation schedule, parsed either from standard cron syntax (Quartz-style:
/// `sec min hour dom mon dow [year]`) or from a human-readable cron-lingo
/// expression (e.g. `at 2 AM and 2 PM on Mondays`).
#[derive(Debug, Clone)]
pub enum Rotate {
    Cron(cron::Schedule),
    Natural(cron_lingo::Schedule),
}

impl<'de> Deserialize<'de> for Rotate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "lowercase", deny_unknown_fields)]
        enum Raw {
            Cron(String),
            Natural(String),
        }
        match Raw::deserialize(d)? {
            Raw::Cron(s) => cron::Schedule::from_str(&s)
                .map(Rotate::Cron)
                .map_err(|e| serde::de::Error::custom(format!("invalid cron `{s}`: {e}"))),
            Raw::Natural(s) => cron_lingo::Schedule::from_str(&s)
                .map(Rotate::Natural)
                .map_err(|e| {
                    serde::de::Error::custom(format!(
                        "invalid natural-language schedule `{s}`: {e:?}"
                    ))
                }),
        }
    }
}

// ----- Infobox --------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoboxConfig {
    pub position: Position,
    pub background: ColorConfig,
    pub foreground: ColorConfig,
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

// ----- Battery indicator ----------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatteryIndicatorConfig {
    pub position: Position,
    /// Outline of the battery cell, the terminal nub, the filled portion of
    /// the level bar, and (for `text` / `both` styles) the percentage text.
    pub foreground: ColorConfig,
    /// Fill of the empty (depleted) portion of the level bar inside the
    /// battery cell. Use a translucent value to let the photo show through.
    pub empty_color: ColorConfig,
    #[serde(default)]
    pub style: BatteryStyle,
    /// Optional low-charge fill colours. When the reported percentage is
    /// `< below` for any entry, the most restrictive (lowest `below`) match
    /// replaces `foreground` for the level fill, the percentage text, and
    /// the cap-at-100 highlight. Order is ignored.
    #[serde(default)]
    pub thresholds: Vec<BatteryThreshold>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatteryThreshold {
    pub below: u8,
    pub color: ColorConfig,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BatteryStyle {
    /// Battery glyph only.
    Icon,
    /// Percentage text only (e.g. `85%`).
    Text,
    /// Glyph and percentage text side by side.
    #[default]
    Both,
}

// ----- Dither ---------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub enum NoiseSource {
    None,
    Bayer(Option<usize>),
    #[default]
    InterleavedGradient,
    White,
}

impl FromStr for NoiseSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "bayer" => Ok(Self::Bayer(None)),
            "ign" | "interleaved-gradient-noise" => Ok(Self::InterleavedGradient),
            "white" => Ok(Self::White),
            _ if s.starts_with("bayer:") => {
                let n = s["bayer:".len()..]
                    .parse::<usize>()
                    .map_err(|_| format!("invalid bayer depth in `{s}`"))?;
                Ok(Self::Bayer(Some(n)))
            }
            _ => Err(format!("unknown noise source `{s}`")),
        }
    }
}

impl<'de> Deserialize<'de> for NoiseSource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Default)]
pub enum Strategy {
    #[default]
    OctahedronClosest,
    OctahedronFurthest,
    NaiveMix,
    NaiveDominant,
    /// 1-D grayscale, no spread (`PureSpreadGrayDecomposer` with
    /// `spread_ratio = 0`). Requires a grayscale `dither_palette`.
    Grayscale,
    /// 1-D grayscale with a pure-spread ratio in `[0, 1]`. Encoded as
    /// `gray-pure-spread:<r>` in TOML. Requires a grayscale `dither_palette`.
    GrayPureSpread(f32),
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "octahedron-closest" => Ok(Self::OctahedronClosest),
            "octahedron-furthest" => Ok(Self::OctahedronFurthest),
            "naive-mix" => Ok(Self::NaiveMix),
            "naive-dominant" => Ok(Self::NaiveDominant),
            "grayscale" => Ok(Self::Grayscale),
            _ if s.starts_with("gray-pure-spread:") => {
                let rest = &s["gray-pure-spread:".len()..];
                let r: f32 = rest
                    .parse()
                    .map_err(|_| format!("invalid spread ratio in `{s}`"))?;
                if !(0.0..=1.0).contains(&r) {
                    return Err(format!("spread ratio {r} out of range [0.0, 1.0]"));
                }
                Ok(Self::GrayPureSpread(r))
            }
            _ => Err(format!("unknown strategy `{s}`")),
        }
    }
}

impl<'de> Deserialize<'de> for Strategy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffuseMethod {
    None,
    #[default]
    FloydSteinberg,
    JarvisJudiceAndNinke,
    Atkinson,
    Sierra,
}

impl DiffuseMethod {
    pub fn to_boxed_matrix(
        &self,
    ) -> Box<dyn epd_dither::dither::diffusion_matrix::DiffusionMatrix> {
        match self {
            Self::None => Box::new(epd_dither::dither::diffusion_matrix::NoDiffuse),
            Self::FloydSteinberg => Box::new(epd_dither::dither::diffusion_matrix::FloydSteinberg),
            Self::JarvisJudiceAndNinke => {
                Box::new(epd_dither::dither::diffusion_matrix::JarvisJudiceAndNinke)
            }
            Self::Atkinson => Box::new(epd_dither::dither::diffusion_matrix::Atkinson),
            Self::Sierra => Box::new(epd_dither::dither::diffusion_matrix::Sierra),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Palette {
    Naive,
    #[default]
    Spectra6,
    Epdoptimize,
    /// 2-level linearly-spaced grayscale (black/white). Pair with a `gray*`
    /// strategy.
    Grayscale2,
    /// 4-level linearly-spaced grayscale.
    Grayscale4,
    /// 16-level linearly-spaced grayscale.
    Grayscale16,
}

impl Palette {
    pub fn colors(&self) -> Vec<image::Rgb<u8>> {
        match self {
            Self::Naive => rgb_triples(&epd_dither::decompose::octahedron::NAIVE_RGB6),
            Self::Spectra6 => rgb_triples(&epd_dither::decompose::octahedron::SPECTRA6),
            Self::Epdoptimize => rgb_triples(&epd_dither::decompose::naive::EPDOPTIMIZE),
            Self::Grayscale2 => gray_triples(&epd_dither::decompose::gray::GRAYSCALE2),
            Self::Grayscale4 => gray_triples(&epd_dither::decompose::gray::GRAYSCALE4),
            Self::Grayscale16 => gray_triples(&epd_dither::decompose::gray::GRAYSCALE16),
        }
    }
}

fn rgb_triples(raw: &[[u8; 3]]) -> Vec<image::Rgb<u8>> {
    raw.iter().copied().map(image::Rgb).collect()
}

fn gray_triples(levels: &[u8]) -> Vec<image::Rgb<u8>> {
    levels.iter().map(|&v| image::Rgb([v, v, v])).collect()
}

#[derive(Debug, Clone)]
pub struct DitherConfig {
    pub noise: NoiseSource,
    pub strategy: Strategy,
    pub diffuse: DiffuseMethod,
    pub dither_palette: Palette,
    pub output_palette: Palette,
}

/// Strategy-driven default palette: grayscale strategies pick `grayscale4`,
/// colour strategies pick `spectra6`. Used when neither `dither_palette` nor
/// `output_palette` is specified in TOML.
fn default_palette_for(strategy: &Strategy) -> Palette {
    match strategy {
        Strategy::Grayscale | Strategy::GrayPureSpread(_) => Palette::Grayscale4,
        _ => Palette::Spectra6,
    }
}

impl Default for DitherConfig {
    fn default() -> Self {
        let strategy = Strategy::default();
        let palette = default_palette_for(&strategy);
        Self {
            noise: Default::default(),
            strategy,
            diffuse: Default::default(),
            dither_palette: palette.clone(),
            output_palette: palette,
        }
    }
}

impl<'de> Deserialize<'de> for DitherConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(default)]
            noise: NoiseSource,
            #[serde(default)]
            strategy: Strategy,
            #[serde(default)]
            diffuse: DiffuseMethod,
            dither_palette: Option<Palette>,
            output_palette: Option<Palette>,
        }
        let raw = Raw::deserialize(d)?;
        // Whichever palette the user named applies to both halves. Naming
        // both keeps the explicit pair; naming neither falls back to a
        // strategy-appropriate default (spectra6 for colour, grayscale4 for
        // gray), so a `strategy = "grayscale"` line "just works".
        let (dither_palette, output_palette) = match (raw.dither_palette, raw.output_palette) {
            (Some(d), Some(o)) => (d, o),
            (Some(d), None) => (d.clone(), d),
            (None, Some(o)) => (o.clone(), o),
            (None, None) => {
                let p = default_palette_for(&raw.strategy);
                (p.clone(), p)
            }
        };
        Ok(Self {
            noise: raw.noise,
            strategy: raw.strategy,
            diffuse: raw.diffuse,
            dither_palette,
            output_palette,
        })
    }
}

// ----- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.toml"),
        )
        .unwrap();
        let cfg: Config = toml::from_str(&text).expect("config.example.toml should parse");
        assert_eq!(cfg.screens.len(), 2);
        assert!(matches!(cfg.screens[0].rotate, Some(Rotate::Cron(_))));
        assert!(matches!(cfg.screens[1].rotate, Some(Rotate::Natural(_))));
        assert_eq!(cfg.screens[0].wake_delay, Duration::from_secs(3600));
        assert_eq!(cfg.screens[1].wake_delay, Duration::ZERO);
        // bedroom screen uses the default (1h); living-room sets it explicitly.
        assert_eq!(cfg.screens[1].error_refresh, Duration::from_secs(3600));
    }

    #[test]
    fn color_hex_opaque() {
        assert_eq!(
            "#ff0000".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 255)
        );
        assert_eq!(
            "#00ff80".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(0, 255, 128, 255)
        );
    }

    #[test]
    fn color_rgb_opaque() {
        assert_eq!(
            "rgb(255, 0, 0)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 255)
        );
        assert_eq!(
            "rgb(1,2,3)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(1, 2, 3, 255)
        );
    }

    #[test]
    fn color_rgba_float_alpha() {
        assert_eq!(
            "rgba(255, 0, 0, 1.0)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 255)
        );
        assert_eq!(
            "rgba(255, 0, 0, 0)".parse::<ColorConfig>().unwrap(),
            ColorConfig::rgba(255, 0, 0, 0)
        );
        let half = "rgba(0, 0, 0, 0.5)".parse::<ColorConfig>().unwrap();
        assert_eq!(half, ColorConfig::rgba(0, 0, 0, 128));
    }

    #[test]
    fn color_rejects_bad_inputs() {
        assert!("#ff".parse::<ColorConfig>().is_err());
        assert!("#gggggg".parse::<ColorConfig>().is_err());
        assert!("rgb(256, 0, 0)".parse::<ColorConfig>().is_err());
        assert!("rgba(0, 0, 0, 2.0)".parse::<ColorConfig>().is_err());
        assert!("rgb(1, 2)".parse::<ColorConfig>().is_err());
        assert!("hsl(0, 0, 0)".parse::<ColorConfig>().is_err());
    }

    #[test]
    fn rotate_deserialises_cron_variant() {
        let r: Rotate = toml::from_str(r#"cron = "0 0 2,14 * * *""#).unwrap();
        assert!(matches!(r, Rotate::Cron(_)));
    }

    #[test]
    fn rotate_deserialises_natural_variant() {
        let r: Rotate = toml::from_str(r#"natural = "at 2 AM and 2 PM""#).unwrap();
        assert!(matches!(r, Rotate::Natural(_)));
    }

    #[test]
    fn rotate_rejects_unknown_key() {
        let r: Result<Rotate, _> = toml::from_str(r#"regex = "xyz""#);
        assert!(r.is_err());
    }

    #[test]
    fn rotate_rejects_invalid_cron() {
        let r: Result<Rotate, _> = toml::from_str(r#"cron = "not a schedule""#);
        assert!(r.is_err());
    }

    #[test]
    fn strategy_parses_unit_variants() {
        assert!(matches!(
            "octahedron-closest".parse(),
            Ok(Strategy::OctahedronClosest)
        ));
        assert!(matches!("naive-mix".parse(), Ok(Strategy::NaiveMix)));
        assert!(matches!("grayscale".parse(), Ok(Strategy::Grayscale)));
    }

    #[test]
    fn strategy_parses_gray_pure_spread() {
        match "gray-pure-spread:0.25".parse::<Strategy>() {
            Ok(Strategy::GrayPureSpread(r)) => assert!((r - 0.25).abs() < 1e-6),
            other => panic!("expected GrayPureSpread(0.25), got {other:?}"),
        }
    }

    #[test]
    fn strategy_rejects_out_of_range_spread() {
        assert!("gray-pure-spread:1.5".parse::<Strategy>().is_err());
        assert!("gray-pure-spread:-0.1".parse::<Strategy>().is_err());
        assert!("gray-pure-spread:nope".parse::<Strategy>().is_err());
        assert!("gray-banana".parse::<Strategy>().is_err());
    }

    #[test]
    fn dither_palette_only_dither_specified_mirrors_to_output() {
        let cfg: DitherConfig = toml::from_str(r#"dither_palette = "grayscale16""#).unwrap();
        assert!(matches!(cfg.dither_palette, Palette::Grayscale16));
        assert!(matches!(cfg.output_palette, Palette::Grayscale16));
    }

    #[test]
    fn dither_palette_only_output_specified_mirrors_to_dither() {
        let cfg: DitherConfig = toml::from_str(r#"output_palette = "epdoptimize""#).unwrap();
        assert!(matches!(cfg.dither_palette, Palette::Epdoptimize));
        assert!(matches!(cfg.output_palette, Palette::Epdoptimize));
    }

    #[test]
    fn dither_palette_both_specified_kept_distinct() {
        let cfg: DitherConfig = toml::from_str(
            r#"
                dither_palette = "epdoptimize"
                output_palette = "spectra6"
            "#,
        )
        .unwrap();
        assert!(matches!(cfg.dither_palette, Palette::Epdoptimize));
        assert!(matches!(cfg.output_palette, Palette::Spectra6));
    }

    #[test]
    fn dither_palette_default_for_colour_strategy_is_spectra6() {
        let cfg: DitherConfig = toml::from_str("").unwrap();
        assert!(matches!(cfg.dither_palette, Palette::Spectra6));
        assert!(matches!(cfg.output_palette, Palette::Spectra6));
    }

    #[test]
    fn dither_palette_default_for_gray_strategy_is_grayscale4() {
        let cfg: DitherConfig = toml::from_str(r#"strategy = "grayscale""#).unwrap();
        assert!(matches!(cfg.dither_palette, Palette::Grayscale4));
        assert!(matches!(cfg.output_palette, Palette::Grayscale4));
    }

    #[test]
    fn dither_palette_default_for_gray_pure_spread_is_grayscale4() {
        let cfg: DitherConfig = toml::from_str(r#"strategy = "gray-pure-spread:0.25""#).unwrap();
        assert!(matches!(cfg.dither_palette, Palette::Grayscale4));
        assert!(matches!(cfg.output_palette, Palette::Grayscale4));
    }
}
