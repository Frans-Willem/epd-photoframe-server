use std::sync::{Arc, LazyLock};

use anyhow::anyhow;
use epd_dither::Decomposer;
use epd_dither::decompose::gray::{OffsetBlendGrayDecomposer, PureSpreadGrayDecomposer};
use epd_dither::decompose::naive::NaiveDecomposer;
use epd_dither::decompose::octahedron::OctahedronDecomposer;
use epd_dither::dither::decomposing::DecomposedQuantizationError;
use epd_dither::dither::diffuse::PixelStrategy;
use epd_dither::dither::diffusion_matrix::{
    ATKINSON, FLOYD_STEINBERG, JARVIS_JUDICE_AND_NINKE, NO_DIFFUSE, RefDiffusionMatrix, SIERRA,
};
use epd_dither::dither::image_traits::{ImageReader, ImageSize, ImageWriter};
use epd_dither::dither::{BundledDitherer, DecomposingDitherStrategy, DynDitherer};
use epd_dither::palette_image::{PaletteImage, VerifiedPalette};
use image::{ImageBuffer, ImageFormat, Luma, Rgb};
use nalgebra::geometry::Point3;
use rand::distr::StandardUniform;
use rand::prelude::*;
use tiny_skia::Pixmap;

use crate::config::{DiffuseMethod, DitherConfig, NoiseSource, Strategy};

/// Bundles an owned `Pixmap` source and a `PaletteImage` write target into
/// the single value `BundledDitherer::dither_into` wants. The canvas is
/// opaque by construction (background painted opaque, overlays composite
/// onto it), so premultiplied storage equals straight RGB and we read
/// channel bytes directly. Owns its `Pixmap` so the type is `'static`,
/// satisfying the factory's / `DynDitherer<T>`'s `T: 'static` bound.
struct PixmapDitherInOut {
    pixmap: Pixmap,
    width: usize,
    height: usize,
    writer: PaletteImage,
}

impl ImageSize for PixmapDitherInOut {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
}

impl ImageReader<Rgb<u8>> for PixmapDitherInOut {
    fn get_pixel(&self, x: usize, y: usize) -> Rgb<u8> {
        let p = self.pixmap.pixels()[y * self.width + x];
        Rgb([p.red(), p.green(), p.blue()])
    }
}

impl ImageWriter<usize> for PixmapDitherInOut {
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
/// sorted ascending — the gray decomposers' `new` would reject them
/// anyway, but we want a clear message naming the offending entry.
fn grayscale_levels(palette: &[[u8; 3]]) -> anyhow::Result<Vec<f32>> {
    let levels: Vec<f32> = palette
        .iter()
        .enumerate()
        .map(|(i, &[r, g, b])| {
            if !(r == g && g == b) {
                anyhow::bail!(
                    "grayscale strategy requires r == g == b for every dither_palette entry; \
                     entry {i} = [{r}, {g}, {b}] is not achromatic"
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

/// Boxed noise function with the signature `DecomposingDitherStrategy::with_noise`
/// wants. `Box<dyn Fn>` impls `Fn` itself, so this slots in as the strategy's
/// generic `N` parameter.
type BoxedNoiseFn = Box<dyn Fn(usize, usize) -> f32 + Send + Sync>;

/// Erased ditherer: the four strategies × five matrices × two noise variants
/// produce many different `BundledDitherer<S, M>` types, but they all
/// implement `DynDitherer<PixmapDitherInOut>`, so we collapse them behind one
/// trait object.
type BoxedDitherer = Box<dyn DynDitherer<PixmapDitherInOut> + Send + Sync>;

fn matrix_for(method: DiffuseMethod) -> RefDiffusionMatrix {
    match method {
        DiffuseMethod::None => NO_DIFFUSE,
        DiffuseMethod::FloydSteinberg => FLOYD_STEINBERG,
        DiffuseMethod::JarvisJudiceAndNinke => JARVIS_JUDICE_AND_NINKE,
        DiffuseMethod::Atkinson => ATKINSON,
        DiffuseMethod::Sierra => SIERRA,
    }
}

/// Wrap a constructed `DecomposingDitherStrategy` in a `BundledDitherer` and
/// erase to `BoxedDitherer`. Two arms because `with_noise` changes the
/// strategy's `N` type parameter.
fn finish<D, F>(
    decomposer: D,
    convert: F,
    noise: Option<BoxedNoiseFn>,
    matrix: RefDiffusionMatrix,
) -> BoxedDitherer
where
    D: Decomposer<f32> + Send + Sync + 'static,
    F: Fn(Rgb<u8>) -> D::Input + Send + Sync + 'static,
    DecomposingDitherStrategy<D, F, fn(usize, usize) -> f32, Rgb<u8>>: PixelStrategy<
            Source = Rgb<u8>,
            Target = usize,
            QuantizationError = DecomposedQuantizationError,
        >,
    DecomposingDitherStrategy<D, F, BoxedNoiseFn, Rgb<u8>>: PixelStrategy<
            Source = Rgb<u8>,
            Target = usize,
            QuantizationError = DecomposedQuantizationError,
        >,
{
    let strategy = DecomposingDitherStrategy::new(decomposer, convert);
    match noise {
        Some(n) => Box::new(BundledDitherer::new(strategy.with_noise(n), matrix)),
        None => Box::new(BundledDitherer::new(strategy, matrix)),
    }
}

fn build_ditherer(
    strategy: Strategy,
    noise: Option<BoxedNoiseFn>,
    palette: &[[u8; 3]],
    matrix: RefDiffusionMatrix,
) -> anyhow::Result<BoxedDitherer> {
    match strategy {
        Strategy::Octahedron(axis) => {
            let palette_points: Vec<Point3<f32>> = palette
                .iter()
                .map(|&[r, g, b]| Point3::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0))
                .collect();
            let decomposer = OctahedronDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow!("OctahedronDecomposer build failed"))?
                .with_strategy(axis);
            Ok(finish(decomposer, color_to_point, noise, matrix))
        }
        Strategy::Naive(s) => {
            let palette_points: Vec<Point3<f32>> = palette
                .iter()
                .map(|&[r, g, b]| Point3::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0))
                .collect();
            let decomposer = NaiveDecomposer::new(&palette_points)
                .ok_or_else(|| anyhow!("NaiveDecomposer build failed"))?
                .with_strategy(s);
            Ok(finish(decomposer, color_to_point, noise, matrix))
        }
        Strategy::GrayPureSpread(spread) => {
            let levels = grayscale_levels(palette)?;
            let decomposer = PureSpreadGrayDecomposer::new(levels)
                .ok_or_else(|| anyhow!("PureSpreadGrayDecomposer build failed"))?
                .with_spread_ratio(spread);
            Ok(finish(decomposer, rgb_to_brightness, noise, matrix))
        }
        Strategy::GrayOffsetBlend(distance) => {
            let levels = grayscale_levels(palette)?;
            let decomposer = OffsetBlendGrayDecomposer::new(levels)
                .ok_or_else(|| anyhow!("OffsetBlendGrayDecomposer build failed"))?
                .with_distance(distance);
            Ok(finish(decomposer, rgb_to_brightness, noise, matrix))
        }
    }
}

fn build_noise_fn(noise: NoiseSource) -> anyhow::Result<Option<BoxedNoiseFn>> {
    Ok(match noise {
        NoiseSource::None => None,
        NoiseSource::Bayer(Some(n)) => {
            Some(Box::new(move |x, y| epd_dither::noise::bayer(x, y, n)))
        }
        NoiseSource::Bayer(None) => Some(Box::new(epd_dither::noise::bayer_inf)),
        NoiseSource::InterleavedGradient => Some(Box::new(|x, y| {
            epd_dither::noise::interleaved_gradient_noise(x as f32, y as f32)
        })),
        NoiseSource::White => Some(Box::new(|_, _| rand::rng().sample(StandardUniform))),
        NoiseSource::Blue => Some(Box::new(|x, y| {
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
            v + w * STEP
        })),
        NoiseSource::File(_) => {
            return Err(anyhow!(
                "noise = `file:...` is not supported by this server; use bayer/ign/blue/white/none"
            ));
        }
    })
}

/// A dither pipeline pre-configured for one screen. Built once at boot from
/// a `DitherConfig`; palette / strategy / level validation runs inside
/// `prepare` so misconfiguration surfaces at startup. Run-time errors are
/// still propagated as `Result` rather than panicking — boot validation
/// catches them early but isn't a substitute for runtime error handling.
///
/// Cheap to clone — it's an `Arc` under the hood.
#[derive(Clone)]
pub struct PreparedDitherMethod {
    output_palette: VerifiedPalette,
    ditherer: Arc<dyn DynDitherer<PixmapDitherInOut> + Send + Sync>,
}

impl PreparedDitherMethod {
    pub fn prepare(config: &DitherConfig) -> anyhow::Result<Self> {
        let output_palette = VerifiedPalette::new(
            config
                .output_palette
                .as_rgb_slice()
                .iter()
                .copied()
                .map(Rgb)
                .collect(),
        )
        .map_err(|e| anyhow!("output palette: {e}"))?;

        let noise = build_noise_fn(config.noise.clone())?;
        let matrix = matrix_for(config.diffuse);
        let ditherer = build_ditherer(
            config.strategy,
            noise,
            config.dither_palette.as_rgb_slice(),
            matrix,
        )?;

        Ok(Self {
            output_palette,
            ditherer: Arc::from(ditherer),
        })
    }

    /// Run the prepared pipeline on an image. Errors here are runtime issues
    /// (allocation, internal invariants); configuration errors were filtered
    /// out at `prepare` time, but the result is `Result` so request handlers
    /// can turn unexpected failures into 500s rather than panicking.
    pub fn run(&self, pixmap: Pixmap) -> anyhow::Result<PaletteImage> {
        let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
        let writer =
            PaletteImage::new(pixmap.width(), pixmap.height(), self.output_palette.clone());
        let mut inout = PixmapDitherInOut {
            pixmap,
            width,
            height,
            writer,
        };
        self.ditherer.dyn_dither_into(&mut inout);
        Ok(inout.writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grayscale_levels_rejects_chromatic_entries() {
        let palette = [[0, 0, 0], [100, 50, 50], [255, 255, 255]];
        assert!(grayscale_levels(&palette).is_err());
    }

    #[test]
    fn grayscale_levels_rejects_unsorted_palette() {
        let palette = [[0, 0, 0], [255, 255, 255], [128, 128, 128]];
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
