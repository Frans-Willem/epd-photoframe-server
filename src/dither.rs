use epd_dither::decompose::naive::{NaiveDecomposer, NaiveDecomposerStrategy};
use epd_dither::decompose::octahedron::{OctahedronDecomposer, OctahedronDecomposerAxisStrategy};
use image::{Rgb, RgbImage};
use nalgebra::{DVector, geometry::Point3};
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

fn owned_to_dynamic_vector<T: nalgebra::Scalar, const N: usize>(
    vec: nalgebra::SVector<T, N>,
) -> DVector<T> {
    DVector::from_column_slice(vec.as_slice())
}

// --- ImageSize / ImageReader / ImageWriter impls ---

struct DitherBuffer<F: Fn(usize, usize) -> Option<f32>> {
    image: RgbImage,
    noise_fn: F,
    /// Palette-indexed output, packed MSB-first at `bits_per_pixel`; each
    /// scanline starts on a byte boundary. Pre-allocated to the packed size.
    target: Vec<u8>,
    bits_per_pixel: u8,
}

impl<F: Fn(usize, usize) -> Option<f32>> DitherBuffer<F> {
    fn row_bytes(&self) -> usize {
        let px_per_byte = 8 / self.bits_per_pixel as usize;
        (self.image.width() as usize).div_ceil(px_per_byte)
    }

    fn packed_size(&self) -> usize {
        self.row_bytes() * self.image.height() as usize
    }
}

impl<F: Fn(usize, usize) -> Option<f32>> epd_dither::dither::diffuse::ImageSize
    for DitherBuffer<F>
{
    fn width(&self) -> usize {
        self.image.width() as usize
    }
    fn height(&self) -> usize {
        self.image.height() as usize
    }
}

impl<F: Fn(usize, usize) -> Option<f32>>
    epd_dither::dither::diffuse::ImageReader<(Rgb<u8>, Option<f32>)> for DitherBuffer<F>
{
    fn get_pixel(&self, x: usize, y: usize) -> (Rgb<u8>, Option<f32>) {
        (
            *self.image.get_pixel(x as u32, y as u32),
            (self.noise_fn)(x, y),
        )
    }
}

impl<F: Fn(usize, usize) -> Option<f32>> epd_dither::dither::diffuse::ImageWriter<u8>
    for DitherBuffer<F>
{
    fn put_pixel(&mut self, x: usize, y: usize, pixel: u8) {
        // Defensive: no-op when already sized. Keeps put_pixel self-contained.
        self.target.resize(self.packed_size(), 0);
        let bpp = self.bits_per_pixel as usize;
        let px_per_byte = 8 / bpp;
        let row_bytes = self.row_bytes();
        let byte_x = x / px_per_byte;
        // MSB-first: the leftmost pixel occupies the highest bits.
        let shift = (px_per_byte - 1 - (x % px_per_byte)) * bpp;
        let mask = ((1u32 << bpp) - 1) as u8;
        let byte = &mut self.target[y * row_bytes + byte_x];
        *byte = (*byte & !(mask << shift)) | ((pixel & mask) << shift);
    }
}

// --- PixelStrategy ---

struct DecomposingDitherStrategy {
    decompose_fn: Box<dyn Fn(Point3<f32>) -> DVector<f32>>,
}

#[derive(Clone, Default)]
struct DecomposedQuantizationError(Option<DVector<f32>>);

impl core::ops::Mul<usize> for DecomposedQuantizationError {
    type Output = Self;
    fn mul(self, rhs: usize) -> Self {
        Self(self.0.map(|x| x * (rhs as f32)))
    }
}

impl core::ops::Div<usize> for DecomposedQuantizationError {
    type Output = Self;
    fn div(self, rhs: usize) -> Self {
        Self(self.0.map(|x| x / (rhs as f32)))
    }
}

impl core::ops::AddAssign for DecomposedQuantizationError {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = match (core::mem::take(&mut self.0), rhs.0) {
            (a, None) => a,
            (None, b) => b,
            (Some(a), Some(b)) => Some(a + b),
        };
    }
}

impl epd_dither::dither::diffuse::PixelStrategy for DecomposingDitherStrategy {
    type Source = (Rgb<u8>, Option<f32>);
    type Target = u8;
    type QuantizationError = DecomposedQuantizationError;

    fn quantize(
        &self,
        source: Self::Source,
        error: Self::QuantizationError,
    ) -> (Self::Target, Self::QuantizationError) {
        let (source, noise) = source;
        let decomposed = (self.decompose_fn)(color_to_point(source));
        let decomposed = match error.0 {
            None => decomposed,
            Some(e) => decomposed + e,
        };
        let clipped = decomposed.map(|x| x.max(0.0));
        let clipped_sum = clipped.sum();

        let index = if let Some(noise) = noise
            && clipped_sum > 0.0
        {
            let mut noise = noise * clipped_sum;
            let mut i = 0;
            while i + 1 < clipped.nrows() && noise >= clipped[i] {
                noise -= clipped[i];
                i += 1;
            }
            i
        } else {
            decomposed.argmax().0
        };

        let mut err = decomposed;
        err[index] -= 1.0;
        // Palette size is bounded by u8 (≤6 entries in practice); cast is safe.
        (index as u8, DecomposedQuantizationError(Some(err)))
    }
}

// --- Public entry point ---

/// Dither `img` and return an indexed PNG at the image's own dimensions.
pub fn process(img: RgbImage, config: &DitherConfig) -> anyhow::Result<Vec<u8>> {
    let palette_points: Vec<Point3<f32>> =
        config.dither_palette.colors().iter().copied().map(color_to_point).collect();

    let decompose: Box<dyn Fn(Point3<f32>) -> DVector<f32>> = match config.strategy {
        Strategy::OctahedronClosest => {
            let d = OctahedronDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build OctahedronDecomposer"))?;
            Box::new(move |x| {
                owned_to_dynamic_vector(d.decompose(&x, OctahedronDecomposerAxisStrategy::Closest))
            })
        }
        Strategy::OctahedronFurthest => {
            let d = OctahedronDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build OctahedronDecomposer"))?;
            Box::new(move |x| {
                owned_to_dynamic_vector(
                    d.decompose(&x, OctahedronDecomposerAxisStrategy::Furthest),
                )
            })
        }
        Strategy::NaiveMix => {
            let d = NaiveDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build NaiveDecomposer"))?;
            Box::new(move |x| d.decompose(&x, NaiveDecomposerStrategy::FavorMix))
        }
        Strategy::NaiveDominant => {
            let d = NaiveDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow::anyhow!("failed to build NaiveDecomposer"))?;
            Box::new(move |x| d.decompose(&x, NaiveDecomposerStrategy::FavorDominant))
        }
    };

    let noise = config.noise.clone();
    let noise_fn = move |x: usize, y: usize| -> Option<f32> {
        match &noise {
            NoiseSource::None => None,
            NoiseSource::Bayer(Some(n)) => Some(epd_dither::noise::bayer(x, y, *n)),
            NoiseSource::Bayer(None) => Some(epd_dither::noise::bayer_inf(x, y)),
            NoiseSource::InterleavedGradient => {
                Some(epd_dither::noise::interleaved_gradient_noise(x as f32, y as f32))
            }
            NoiseSource::White => Some(rand::rng().sample(StandardUniform)),
        }
    };

    let bit_depth = bit_depth_for(palette_points.len());
    let bpp = bits_per_pixel(bit_depth);
    let mut buf = DitherBuffer {
        image: img,
        noise_fn,
        target: Vec::new(),
        bits_per_pixel: bpp,
    };
    buf.target = vec![0u8; buf.packed_size()];

    epd_dither::dither::diffuse::diffuse_dither(
        DecomposingDitherStrategy { decompose_fn: decompose },
        config.diffuse.to_boxed_matrix(),
        &mut buf,
        true,
    );

    // Encode as indexed PNG; buf.target is already packed at `bit_depth`.
    let mut png_bytes: Vec<u8> = Vec::new();
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(&mut png_bytes),
        buf.image.width(),
        buf.image.height(),
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
    writer.write_image_data(&buf.target)?;
    drop(writer);

    Ok(png_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use epd_dither::dither::diffuse::ImageWriter;

    fn buf(width: u32, height: u32, bpp: u8) -> DitherBuffer<impl Fn(usize, usize) -> Option<f32>> {
        let image = RgbImage::new(width, height);
        let mut buf = DitherBuffer {
            image,
            noise_fn: |_, _| None,
            target: Vec::new(),
            bits_per_pixel: bpp,
        };
        buf.target = vec![0u8; buf.packed_size()];
        buf
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
        let mut b = buf(3, 2, 8);
        b.put_pixel(0, 0, 0xAA);
        b.put_pixel(2, 1, 0xBB);
        assert_eq!(b.target, vec![0xAA, 0, 0, 0, 0, 0xBB]);
    }

    #[test]
    fn pack_4bit_msb_first_and_rounds_rows() {
        let mut b = buf(3, 1, 4);
        b.put_pixel(0, 0, 0x0);
        b.put_pixel(1, 0, 0xA);
        b.put_pixel(2, 0, 0x5);
        // Row has 3 pixels at 4 bpp => 2 bytes (second byte's low nibble is padding).
        assert_eq!(b.target, vec![0x0A, 0x50]);
    }

    #[test]
    fn pack_2bit_packs_four_per_byte() {
        let mut b = buf(5, 1, 2);
        for (x, v) in [0u8, 1, 2, 3, 1].iter().enumerate() {
            b.put_pixel(x, 0, *v);
        }
        // 00 01 10 11 | 01 00 00 00  =>  0b00011011, 0b01000000
        assert_eq!(b.target, vec![0b00_01_10_11, 0b01_00_00_00]);
    }

    #[test]
    fn pack_1bit_msb_first() {
        let mut b = buf(9, 1, 1);
        for (x, v) in [1u8, 0, 1, 1, 0, 0, 1, 0, 1].iter().enumerate() {
            b.put_pixel(x, 0, *v);
        }
        // 10110010 | 10000000
        assert_eq!(b.target, vec![0b1011_0010, 0b1000_0000]);
    }

    #[test]
    fn put_pixel_overwrites_on_repeat() {
        let mut b = buf(2, 1, 4);
        b.put_pixel(0, 0, 0xF);
        b.put_pixel(0, 0, 0x3);
        assert_eq!(b.target, vec![0x30]);
    }
}
