use std::sync::{Arc, LazyLock};

use epd_dither::decompose::Decomposer;
use epd_dither::decompose::gray::{OffsetBlendGrayDecomposer, PureSpreadGrayDecomposer};
use epd_dither::decompose::naive::{NaiveDecomposer, NaiveDecomposerStrategy};
use epd_dither::decompose::octahedron::{OctahedronDecomposer, OctahedronDecomposerAxisStrategy};
use epd_dither::dither::DecomposingDitherStrategy;
use epd_dither::dither::diffuse::{ImageReader, ImageSize, ImageWriter, diffuse_dither};
use image::{ImageBuffer, ImageFormat, Luma, Rgb};
use nalgebra::geometry::Point3;
use rand::distr::StandardUniform;
use rand::prelude::*;
use tiny_skia::{Pixmap, PremultipliedColorU8};

use crate::config::{DitherConfig, NoiseSource, Strategy};
use crate::palette_image::{PaletteImage, VerifiedPalette};

/// Bundles a borrowed `Pixmap` source, a per-pixel noise function, and a
/// `PaletteImage` write target into the single value
/// `epd_dither::dither::diffuse::diffuse_dither` wants. The canvas is opaque
/// by construction (background painted opaque, overlays composite onto it),
/// so premultiplied storage equals straight RGB and we read channel bytes
/// directly.
struct PixmapDitherInOut<'a, F> {
    pixels: &'a [PremultipliedColorU8],
    width: usize,
    height: usize,
    noise_fn: F,
    writer: PaletteImage,
}

impl<F> ImageSize for PixmapDitherInOut<'_, F> {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
}

impl<F: Fn(usize, usize) -> Option<f32>> ImageReader<(Rgb<u8>, Option<f32>)>
    for PixmapDitherInOut<'_, F>
{
    fn get_pixel(&self, x: usize, y: usize) -> (Rgb<u8>, Option<f32>) {
        let p = self.pixels[y * self.width + x];
        (Rgb([p.red(), p.green(), p.blue()]), (self.noise_fn)(x, y))
    }
}

impl<F> ImageWriter<usize> for PixmapDitherInOut<'_, F> {
    fn put_pixel(&mut self, x: usize, y: usize, pixel: usize) {
        self.writer.put_pixel(x, y, pixel);
    }
}

/// 256x256 high-quality blue-noise texture from the upstream `epd-dither`
/// repo (`HDR_L_0.png`). Embedded into the binary, decoded once on first
/// use, and tiled by simple `% width / % height` over the image.
static BLUE_NOISE: LazyLock<ImageBuffer<Luma<f32>, Vec<f32>>> = LazyLock::new(|| {
    let bytes = include_bytes!("../assets/HDR_L_0.png");
    image::load_from_memory_with_format(bytes, ImageFormat::Png)
        .expect("embedded HDR_L_0.png must decode")
        .to_luma32f()
});

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

type BoxedPixelStrategy = Box<
    dyn epd_dither::dither::diffuse::PixelStrategy<
            Source = (Rgb<u8>, Option<f32>),
            Target = usize,
            QuantizationError = epd_dither::dither::decomposing::DecomposedQuantizationError,
        > + Sync
        + Send,
>;

/// A dither pipeline pre-configured for one screen. Built once at boot from
/// a `DitherConfig`; palette / strategy / level validation runs inside
/// `prepare` so misconfiguration surfaces at startup. Run-time errors are
/// still propagated as `Result` rather than panicking — boot validation
/// catches them early but isn't a substitute for runtime error handling.
///
/// Cheap to clone — it's an `Arc` under the hood.
#[derive(Clone)]
pub struct PreparedDitherMethod {
    inner: Arc<dyn Fn(Pixmap) -> anyhow::Result<PaletteImage> + Send + Sync>,
}

impl PreparedDitherMethod {
    pub fn prepare(config: &DitherConfig) -> anyhow::Result<Self> {
        let dither_palette = config.dither_palette.colors();
        let output_palette = VerifiedPalette::new(config.output_palette.colors())?;

        let pixel_strategy = build_pixel_strategy(&config.strategy, &dither_palette)?;

        let noise = build_noise_fn(config.noise.clone());
        let matrix = config.diffuse.to_boxed_matrix();

        let inner = Arc::new(move |img: Pixmap| -> anyhow::Result<PaletteImage> {
            let (width, height) = (img.width(), img.height());
            let writer = PaletteImage::new(width, height, output_palette.clone());
            let mut inout = PixmapDitherInOut {
                pixels: img.pixels(),
                width: width as usize,
                height: height as usize,
                noise_fn: &noise,
                writer,
            };
            diffuse_dither(pixel_strategy.as_ref(), matrix.as_ref(), &mut inout, true);
            Ok(inout.writer)
        });

        Ok(Self { inner })
    }

    /// Run the prepared pipeline on an image. Errors here are runtime issues
    /// (allocation, internal invariants); configuration errors were filtered
    /// out at `prepare` time, but the result is `Result` so request handlers
    /// can turn unexpected failures into 500s rather than panicking.
    pub fn run(&self, img: Pixmap) -> anyhow::Result<PaletteImage> {
        (self.inner)(img)
    }
}

fn build_pixel_strategy(
    strategy: &Strategy,
    dither_palette: &[Rgb<u8>],
) -> anyhow::Result<BoxedPixelStrategy> {
    Ok(match *strategy {
        Strategy::OctahedronClosest => {
            octahedron_with(dither_palette, OctahedronDecomposerAxisStrategy::Closest)?
        }
        Strategy::OctahedronFurthest => {
            octahedron_with(dither_palette, OctahedronDecomposerAxisStrategy::Furthest)?
        }
        Strategy::OctahedronAxis(axis) => {
            octahedron_with(dither_palette, OctahedronDecomposerAxisStrategy::Axis(axis))?
        }
        Strategy::OctahedronAverage => {
            octahedron_with(dither_palette, OctahedronDecomposerAxisStrategy::Average)?
        }
        Strategy::NaiveMix => naive_with(dither_palette, NaiveDecomposerStrategy::FavorMix)?,
        Strategy::NaiveDominant => {
            naive_with(dither_palette, NaiveDecomposerStrategy::FavorDominant)?
        }
        Strategy::NaiveTetraBlend(p) => {
            naive_with(dither_palette, NaiveDecomposerStrategy::TetraBlend(p))?
        }
        Strategy::GrayPureSpread(spread) => grayscale_with(dither_palette, |levels| {
            Ok(PureSpreadGrayDecomposer::new(levels)
                .ok_or_else(|| anyhow::anyhow!("PureSpreadGrayDecomposer build failed"))?
                .with_spread_ratio(spread))
        })?,
        Strategy::GrayOffsetBlend(distance) => grayscale_with(dither_palette, |levels| {
            Ok(OffsetBlendGrayDecomposer::new(levels)
                .ok_or_else(|| anyhow::anyhow!("OffsetBlendGrayDecomposer build failed"))?
                .with_distance(distance))
        })?,
    })
}

fn octahedron_with(
    palette: &[Rgb<u8>],
    axis: OctahedronDecomposerAxisStrategy,
) -> anyhow::Result<BoxedPixelStrategy> {
    let palette_points: Vec<_> = palette.iter().copied().map(color_to_point).collect();
    Ok(Box::new(DecomposingDitherStrategy::new(
        OctahedronDecomposer::new(palette_points.as_slice())
            .ok_or_else(|| anyhow::anyhow!("OctahedronDecomposer build failed"))?
            .with_strategy(axis),
        color_to_point,
    )))
}

fn naive_with(
    palette: &[Rgb<u8>],
    strategy: NaiveDecomposerStrategy,
) -> anyhow::Result<BoxedPixelStrategy> {
    let palette_points: Vec<_> = palette.iter().copied().map(color_to_point).collect();
    Ok(Box::new(DecomposingDitherStrategy::new(
        NaiveDecomposer::new(palette_points.as_slice())
            .ok_or_else(|| anyhow::anyhow!("NaiveDecomposer build failed"))?
            .with_strategy(strategy),
        color_to_point,
    )))
}

fn grayscale_with<
    D: Decomposer<f32, Input = f32> + Send + Sync + 'static,
    F: Fn(Vec<f32>) -> anyhow::Result<D>,
>(
    palette: &[Rgb<u8>],
    strategy: F,
) -> anyhow::Result<BoxedPixelStrategy> {
    let levels = grayscale_levels(palette)?;
    Ok(Box::new(DecomposingDitherStrategy::new(
        strategy(levels)?,
        rgb_to_brightness,
    )))
}

type BoxedNoiseFn = Box<dyn Fn(usize, usize) -> Option<f32> + Send + Sync>;

fn build_noise_fn(noise: NoiseSource) -> BoxedNoiseFn {
    match &noise {
        NoiseSource::None => Box::new(|_, _| None),
        NoiseSource::Bayer(Some(n)) => {
            let n = *n;
            Box::new(move |x, y| Some(epd_dither::noise::bayer(x, y, n)))
        }
        NoiseSource::Bayer(None) => Box::new(|x, y| Some(epd_dither::noise::bayer_inf(x, y))),
        NoiseSource::InterleavedGradient => Box::new(|x, y| {
            Some(epd_dither::noise::interleaved_gradient_noise(
                x as f32, y as f32,
            ))
        }),
        NoiseSource::White => Box::new(|_, _| Some(rand::rng().sample(StandardUniform))),
        NoiseSource::Blue => Box::new(|x, y| {
            let v = BLUE_NOISE
                .get_pixel(
                    x as u32 % BLUE_NOISE.width(),
                    y as u32 % BLUE_NOISE.height(),
                )
                .0[0];
            // HDR_L_0.png is 16-bit grayscale, so `to_luma32f` lands on
            // one of 65536 evenly-spaced levels; add white noise scaled
            // to one quantization step to fill the gap and avoid tied
            // thresholds across smooth gradients.
            const STEP: f32 = 1.0 / 65535.0;
            let w: f32 = rand::rng().sample(StandardUniform);
            Some(v + w * STEP)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn prepared_gray_pipeline_produces_indexed_png() {
        use crate::config::{DiffuseMethod, NoiseSource, Palette, Strategy};
        use tiny_skia::Color;

        let mut img = Pixmap::new(8, 4).expect("valid size");
        img.fill(Color::from_rgba8(128, 128, 128, 255));
        let cfg = DitherConfig {
            noise: NoiseSource::None,
            strategy: Strategy::GrayPureSpread(0.25),
            diffuse: DiffuseMethod::FloydSteinberg,
            dither_palette: Palette::Grayscale4,
            output_palette: Palette::Grayscale4,
        };
        let prepared = PreparedDitherMethod::prepare(&cfg).expect("prepare should succeed");
        let png_bytes = prepared
            .run(img)
            .expect("dither should succeed")
            .to_png()
            .expect("encoding should succeed");
        // PNG magic.
        assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
