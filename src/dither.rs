use epd_dither::decompose::gray::PureSpreadGrayDecomposer;
use epd_dither::decompose::naive::{NaiveDecomposer, NaiveDecomposerStrategy};
use epd_dither::decompose::octahedron::{OctahedronDecomposer, OctahedronDecomposerAxisStrategy};
use epd_dither::dither::DecomposingDitherStrategy;
use epd_dither::dither::diffuse::{ImageWriter, diffuse_dither};
use epd_dither::image_adapter::PaletteDitheringWithNoise;
use image::{Rgb, RgbImage};
use nalgebra::geometry::Point3;
use png::BitDepth;
use rand::distr::StandardUniform;
use rand::prelude::*;

use crate::config::{DitherConfig, NoiseSource, Strategy};

/// Smallest indexed PNG bit depth that can hold `palette_size` distinct indices.
fn bit_depth_for(palette_size: usize) -> BitDepth {
    match palette_size {
        0..=2 => BitDepth::One,
        3..=4 => BitDepth::Two,
        5..=16 => BitDepth::Four,
        _ => BitDepth::Eight,
    }
}

/// Number of bits each pixel occupies for a given PNG bit depth.
fn bits_per_pixel(depth: BitDepth) -> u8 {
    depth as u8
}

fn color_to_point(color: Rgb<u8>) -> Point3<f32> {
    let [r, g, b] = color.0;
    Point3::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

/// BT.709 luma applied directly in sRGB (no gamma round-trip), matching the
/// upstream `dither` binary so behaviour aligns with the reference output.
fn rgb_to_brightness(color: Rgb<u8>) -> f32 {
    let [r, g, b] = color.0;
    0.2126 * (r as f32 / 255.0) + 0.7152 * (g as f32 / 255.0) + 0.0722 * (b as f32 / 255.0)
}

/// Extract strictly-ascending grayscale levels from an achromatic palette.
/// Errors if any palette entry has `r != g != b` or if the levels aren't
/// sorted ascending — `PureSpreadGrayDecomposer::new` would reject them
/// anyway, but we want a clear message naming the offending entry.
fn grayscale_levels(palette: &[Rgb<u8>]) -> anyhow::Result<Vec<f32>> {
    let levels: Vec<f32> = palette
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let [r, g, b] = p.0;
            if !(r == g && g == b) {
                anyhow::bail!(
                    "grayscale strategy requires r == g == b for every dither_palette entry; \
                     entry {i} = {p:?} is not achromatic"
                );
            }
            Ok(r as f32 / 255.0)
        })
        .collect::<anyhow::Result<_>>()?;
    for w in levels.windows(2) {
        if w[0] >= w[1] {
            anyhow::bail!(
                "grayscale dither_palette must be sorted strictly ascending by brightness"
            );
        }
    }
    Ok(levels)
}

/// `ImageWriter<usize>` sink that packs palette indices MSB-first into the
/// indexed-PNG byte layout (each scanline byte-aligned). Pre-allocated to the
/// packed size on construction.
struct PackedIndexWriter {
    width: usize,
    bits_per_pixel: u8,
    target: Vec<u8>,
}

impl PackedIndexWriter {
    fn new(width: usize, height: usize, bits_per_pixel: u8) -> Self {
        let px_per_byte = 8 / bits_per_pixel as usize;
        let row_bytes = width.div_ceil(px_per_byte);
        Self {
            width,
            bits_per_pixel,
            target: vec![0u8; row_bytes * height],
        }
    }

    fn row_bytes(&self) -> usize {
        let px_per_byte = 8 / self.bits_per_pixel as usize;
        self.width.div_ceil(px_per_byte)
    }
}

impl ImageWriter<usize> for PackedIndexWriter {
    fn put_pixel(&mut self, x: usize, y: usize, pixel: usize) {
        let bpp = self.bits_per_pixel as usize;
        let px_per_byte = 8 / bpp;
        let row_bytes = self.row_bytes();
        let byte_x = x / px_per_byte;
        // MSB-first: the leftmost pixel occupies the highest bits.
        let shift = (px_per_byte - 1 - (x % px_per_byte)) * bpp;
        let mask = ((1u32 << bpp) - 1) as u8;
        let byte = &mut self.target[y * row_bytes + byte_x];
        *byte = (*byte & !(mask << shift)) | (((pixel as u8) & mask) << shift);
    }
}

/// Dither `img` and return an indexed PNG at the image's own dimensions.
pub fn process(img: RgbImage, config: &DitherConfig) -> anyhow::Result<Vec<u8>> {
    let palette_points: Vec<Point3<f32>> = config
        .dither_palette
        .colors()
        .iter()
        .copied()
        .map(color_to_point)
        .collect();

    let noise = config.noise.clone();
    let noise_fn = move |x: usize, y: usize| -> Option<f32> {
        match &noise {
            NoiseSource::None => None,
            NoiseSource::Bayer(Some(n)) => Some(epd_dither::noise::bayer(x, y, *n)),
            NoiseSource::Bayer(None) => Some(epd_dither::noise::bayer_inf(x, y)),
            NoiseSource::InterleavedGradient => Some(
                epd_dither::noise::interleaved_gradient_noise(x as f32, y as f32),
            ),
            NoiseSource::White => Some(rand::rng().sample(StandardUniform)),
        }
    };

    let (width, height) = (img.width() as usize, img.height() as usize);
    let bit_depth = bit_depth_for(palette_points.len());
    let bpp = bits_per_pixel(bit_depth);
    let mut inout = PaletteDitheringWithNoise {
        image: img,
        noise_fn,
        writer: PackedIndexWriter::new(width, height, bpp),
    };
    let matrix = config.diffuse.to_boxed_matrix();

    match config.strategy {
        Strategy::OctahedronClosest => {
            let decomposer = OctahedronDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build OctahedronDecomposer"))?
                .with_strategy(OctahedronDecomposerAxisStrategy::Closest);
            diffuse_dither(
                DecomposingDitherStrategy::new(decomposer, color_to_point),
                matrix,
                &mut inout,
                true,
            );
        }
        Strategy::OctahedronFurthest => {
            let decomposer = OctahedronDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build OctahedronDecomposer"))?
                .with_strategy(OctahedronDecomposerAxisStrategy::Furthest);
            diffuse_dither(
                DecomposingDitherStrategy::new(decomposer, color_to_point),
                matrix,
                &mut inout,
                true,
            );
        }
        Strategy::NaiveMix => {
            let decomposer = NaiveDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build NaiveDecomposer"))?
                .with_strategy(NaiveDecomposerStrategy::FavorMix);
            diffuse_dither(
                DecomposingDitherStrategy::new(decomposer, color_to_point),
                matrix,
                &mut inout,
                true,
            );
        }
        Strategy::NaiveDominant => {
            let decomposer = NaiveDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build NaiveDecomposer"))?
                .with_strategy(NaiveDecomposerStrategy::FavorDominant);
            diffuse_dither(
                DecomposingDitherStrategy::new(decomposer, color_to_point),
                matrix,
                &mut inout,
                true,
            );
        }
        Strategy::Grayscale | Strategy::GrayPureSpread(_) => {
            let spread = match config.strategy {
                Strategy::Grayscale => 0.0,
                Strategy::GrayPureSpread(r) => r,
                _ => unreachable!(),
            };
            let levels = grayscale_levels(config.dither_palette.colors().as_slice())?;
            let decomposer = PureSpreadGrayDecomposer::new(levels)
                .ok_or_else(|| anyhow::anyhow!("failed to build PureSpreadGrayDecomposer"))?
                .with_spread_ratio(spread);
            diffuse_dither(
                DecomposingDitherStrategy::new(decomposer, rgb_to_brightness),
                matrix,
                &mut inout,
                true,
            );
        }
    }

    // Encode as indexed PNG; writer.target is already packed at `bit_depth`.
    let mut png_bytes: Vec<u8> = Vec::new();
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(&mut png_bytes),
        inout.image.width(),
        inout.image.height(),
    );
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(bit_depth);
    let palette_bytes: Vec<u8> = config
        .output_palette
        .colors()
        .iter()
        .flat_map(|rgb| rgb.0)
        .collect();
    encoder.set_palette(palette_bytes);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&inout.writer.target)?;
    drop(writer);

    Ok(png_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(width: u32, height: u32, bpp: u8) -> PackedIndexWriter {
        PackedIndexWriter::new(width as usize, height as usize, bpp)
    }

    #[test]
    fn bit_depth_for_palette_sizes() {
        assert_eq!(bit_depth_for(2), BitDepth::One);
        assert_eq!(bit_depth_for(4), BitDepth::Two);
        assert_eq!(bit_depth_for(6), BitDepth::Four);
        assert_eq!(bit_depth_for(16), BitDepth::Four);
        assert_eq!(bit_depth_for(17), BitDepth::Eight);
    }

    #[test]
    fn pack_8bit_writes_one_byte_per_pixel() {
        let mut w = writer(3, 2, 8);
        w.put_pixel(0, 0, 0xAA);
        w.put_pixel(2, 1, 0xBB);
        assert_eq!(w.target, vec![0xAA, 0, 0, 0, 0, 0xBB]);
    }

    #[test]
    fn pack_4bit_msb_first_and_rounds_rows() {
        let mut w = writer(3, 1, 4);
        w.put_pixel(0, 0, 0x0);
        w.put_pixel(1, 0, 0xA);
        w.put_pixel(2, 0, 0x5);
        // Row has 3 pixels at 4 bpp => 2 bytes (second byte's low nibble is padding).
        assert_eq!(w.target, vec![0x0A, 0x50]);
    }

    #[test]
    fn pack_2bit_packs_four_per_byte() {
        let mut w = writer(5, 1, 2);
        for (x, v) in [0usize, 1, 2, 3, 1].iter().enumerate() {
            w.put_pixel(x, 0, *v);
        }
        // 00 01 10 11 | 01 00 00 00  =>  0b00011011, 0b01000000
        assert_eq!(w.target, vec![0b00_01_10_11, 0b01_00_00_00]);
    }

    #[test]
    fn pack_1bit_msb_first() {
        let mut w = writer(9, 1, 1);
        for (x, v) in [1usize, 0, 1, 1, 0, 0, 1, 0, 1].iter().enumerate() {
            w.put_pixel(x, 0, *v);
        }
        // 10110010 | 10000000
        assert_eq!(w.target, vec![0b1011_0010, 0b1000_0000]);
    }

    #[test]
    fn put_pixel_overwrites_on_repeat() {
        let mut w = writer(2, 1, 4);
        w.put_pixel(0, 0, 0xF);
        w.put_pixel(0, 0, 0x3);
        assert_eq!(w.target, vec![0x30]);
    }

    #[test]
    fn grayscale_levels_rejects_chromatic_entries() {
        let palette = [Rgb([0, 0, 0]), Rgb([100, 50, 50]), Rgb([255, 255, 255])];
        assert!(grayscale_levels(&palette).is_err());
    }

    #[test]
    fn grayscale_levels_rejects_unsorted_palette() {
        let palette = [Rgb([0, 0, 0]), Rgb([255, 255, 255]), Rgb([128, 128, 128])];
        assert!(grayscale_levels(&palette).is_err());
    }

    #[test]
    fn process_gray_pipeline_produces_indexed_png() {
        use crate::config::{DiffuseMethod, NoiseSource, Palette, Strategy};

        let img = RgbImage::from_pixel(8, 4, Rgb([128, 128, 128]));
        let cfg = DitherConfig {
            noise: NoiseSource::None,
            strategy: Strategy::GrayPureSpread(0.25),
            diffuse: DiffuseMethod::FloydSteinberg,
            dither_palette: Palette::Grayscale4,
            output_palette: Palette::Grayscale4,
        };
        let png_bytes = process(img, &cfg).expect("gray pipeline should succeed");
        // PNG magic.
        assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
