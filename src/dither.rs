use epd_dither::decompose::naive::{NaiveDecomposer, NaiveDecomposerStrategy};
use epd_dither::decompose::octahedron::{OctahedronDecomposer, OctahedronDecomposerAxisStrategy};
use image::{Rgb, RgbImage};
use nalgebra::{DVector, geometry::Point3};
use rand::distr::StandardUniform;
use rand::prelude::*;

use crate::config::{DitherConfig, NoiseSource, Strategy};

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
    /// Palette-indexed output, length `width * height`, pre-allocated.
    target: Vec<u8>,
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
        let w = self.image.width() as usize;
        let h = self.image.height() as usize;
        // Defensive: ensures correctness even if caller forgot to pre-size.
        // `Vec::resize` is a no-op when `len` already matches — cheap on the hot path.
        self.target.resize(w * h, 0);
        self.target[y * w + x] = pixel;
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

    let pixel_count = (img.width() * img.height()) as usize;
    let mut buf = DitherBuffer {
        image: img,
        noise_fn,
        target: vec![0u8; pixel_count],
    };

    epd_dither::dither::diffuse::diffuse_dither(
        DecomposingDitherStrategy { decompose_fn: decompose },
        config.diffuse.to_boxed_matrix(),
        &mut buf,
        true,
    );

    // Encode as indexed PNG
    let mut png_bytes: Vec<u8> = Vec::new();
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(&mut png_bytes),
        buf.image.width(),
        buf.image.height(),
    );
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
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
