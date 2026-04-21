use serde::Deserialize;
use std::str::FromStr;

use crate::color::ColorConfig;
use crate::infobox::InfoboxConfig;
use crate::screen_state::Rotate;

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
    /// IANA timezone name (e.g. `Europe/Amsterdam`) used for rotation
    /// scheduling and the infobox. Defaults to the system timezone.
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub dither: DitherConfig,
}

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

impl Config {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen_state::Rotate;

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
    }
}
