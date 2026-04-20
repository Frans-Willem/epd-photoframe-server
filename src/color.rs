use serde::Deserialize;
use std::str::FromStr;

/// An sRGB color with an optional alpha channel. Alpha defaults to 255 (opaque)
/// when parsed from a form that doesn't specify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[allow(dead_code)]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn to_rgb(self) -> image::Rgb<u8> {
        image::Rgb([self.r, self.g, self.b])
    }
}

impl FromStr for Color {
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
            return Ok(Self { r, g, b, a });
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

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn parse_hex(hex: &str) -> Option<Color> {
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::rgb(r, g, b))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_opaque() {
        assert_eq!("#ff0000".parse::<Color>().unwrap(), Color::rgba(255, 0, 0, 255));
        assert_eq!("#00ff80".parse::<Color>().unwrap(), Color::rgba(0, 255, 128, 255));
    }

    #[test]
    fn rgb_opaque() {
        assert_eq!("rgb(255, 0, 0)".parse::<Color>().unwrap(), Color::rgba(255, 0, 0, 255));
        assert_eq!("rgb(1,2,3)".parse::<Color>().unwrap(), Color::rgba(1, 2, 3, 255));
    }

    #[test]
    fn rgba_float_alpha() {
        assert_eq!("rgba(255, 0, 0, 1.0)".parse::<Color>().unwrap(), Color::rgba(255, 0, 0, 255));
        assert_eq!("rgba(255, 0, 0, 0)".parse::<Color>().unwrap(), Color::rgba(255, 0, 0, 0));
        let half = "rgba(0, 0, 0, 0.5)".parse::<Color>().unwrap();
        assert_eq!(half, Color::rgba(0, 0, 0, 128));
    }

    #[test]
    fn rejects_bad_inputs() {
        assert!("#ff".parse::<Color>().is_err());
        assert!("#gggggg".parse::<Color>().is_err());
        assert!("rgb(256, 0, 0)".parse::<Color>().is_err());
        assert!("rgba(0, 0, 0, 2.0)".parse::<Color>().is_err());
        assert!("rgb(1, 2)".parse::<Color>().is_err());
        assert!("hsl(0, 0, 0)".parse::<Color>().is_err());
    }
}
