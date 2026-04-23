use serde::Deserialize;
use std::str::FromStr;
use std::time::Duration;
use tiny_skia::ColorU8;

// ----- Top-level config -----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Config {
    pub screens: Vec<ScreenConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ScreenConfig {
    pub name: String,
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
    /// IANA timezone name (e.g. `Europe/Amsterdam`) used for rotation
    /// scheduling and the infobox. Defaults to the system timezone.
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub dither: DitherConfig,
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
        Err(format!("expected `#RRGGBB`, `rgb(...)`, or `rgba(...)`, got `{s}`"))
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
    s.parse::<u8>().map_err(|_| format!("invalid 0-255 byte `{s}`"))
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
            Raw::Natural(s) => cron_lingo::Schedule::from_str(&s).map(Rotate::Natural).map_err(
                |e| {
                    serde::de::Error::custom(format!(
                        "invalid natural-language schedule `{s}`: {e:?}"
                    ))
                },
            ),
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    #[default]
    OctahedronClosest,
    OctahedronFurthest,
    NaiveMix,
    NaiveDominant,
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
    pub fn to_boxed_matrix(&self) -> Box<dyn epd_dither::dither::diffusion_matrix::DiffusionMatrix> {
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
}

impl Palette {
    pub fn colors(&self) -> &[image::Rgb<u8>] {
        use image::Rgb;
        match self {
            Self::Naive => &[
                Rgb([0, 0, 0]),
                Rgb([255, 255, 255]),
                Rgb([255, 255, 0]),
                Rgb([255, 0, 0]),
                Rgb([0, 0, 255]),
                Rgb([0, 255, 0]),
            ],
            Self::Spectra6 => &[
                Rgb([58, 0, 66]),
                Rgb([179, 208, 200]),
                Rgb([215, 233, 0]),
                Rgb([151, 38, 44]),
                Rgb([61, 38, 152]),
                Rgb([96, 104, 86]),
            ],
            Self::Epdoptimize => &[
                Rgb([0x19, 0x1e, 0x21]),
                Rgb([0xe8, 0xe8, 0xe8]),
                Rgb([0xef, 0xde, 0x44]),
                Rgb([0xb2, 0x13, 0x18]),
                Rgb([0x21, 0x57, 0xba]),
                Rgb([0x12, 0x5f, 0x20]),
            ],
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DitherConfig {
    #[serde(default)]
    pub noise: NoiseSource,
    #[serde(default)]
    pub strategy: Strategy,
    #[serde(default)]
    pub diffuse: DiffuseMethod,
    #[serde(default)]
    pub dither_palette: Palette,
    #[serde(default)]
    pub output_palette: Palette,
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
    }

    #[test]
    fn color_hex_opaque() {
        assert_eq!("#ff0000".parse::<ColorConfig>().unwrap(), ColorConfig::rgba(255, 0, 0, 255));
        assert_eq!("#00ff80".parse::<ColorConfig>().unwrap(), ColorConfig::rgba(0, 255, 128, 255));
    }

    #[test]
    fn color_rgb_opaque() {
        assert_eq!("rgb(255, 0, 0)".parse::<ColorConfig>().unwrap(), ColorConfig::rgba(255, 0, 0, 255));
        assert_eq!("rgb(1,2,3)".parse::<ColorConfig>().unwrap(), ColorConfig::rgba(1, 2, 3, 255));
    }

    #[test]
    fn color_rgba_float_alpha() {
        assert_eq!("rgba(255, 0, 0, 1.0)".parse::<ColorConfig>().unwrap(), ColorConfig::rgba(255, 0, 0, 255));
        assert_eq!("rgba(255, 0, 0, 0)".parse::<ColorConfig>().unwrap(), ColorConfig::rgba(255, 0, 0, 0));
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
}
