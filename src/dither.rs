use anyhow::anyhow;
use epd_dither::dither::DynDitherer;
use epd_dither::dither::image_traits::{ImageCombinedRW, ImageReader, ImageSize};
use epd_dither::image::palette_image::{PaletteImage, VerifiedPalette};
use epd_dither::registry::decompose_ditherer;
use image::Rgb;
use tiny_skia::Pixmap;

use crate::config::DitherConfig;

/// Reader-only newtype over `tiny_skia::Pixmap` exposing the pixel buffer
/// to `epd_dither`'s `ImageReader<Rgb<u8>>`. The canvas is opaque by
/// construction (background painted opaque, overlays composite onto it),
/// so premultiplied storage equals straight RGB and we read channel
/// bytes directly. Owns its `Pixmap` so the type is `'static`,
/// satisfying `DynDitherer<T>`'s `T: 'static` bound.
struct PixmapReader(Pixmap);

impl ImageSize for PixmapReader {
    fn width(&self) -> usize {
        self.0.width() as usize
    }
    fn height(&self) -> usize {
        self.0.height() as usize
    }
}

impl ImageReader<Rgb<u8>> for PixmapReader {
    fn get_pixel(&self, x: usize, y: usize) -> Rgb<u8> {
        let p = self.0.pixels()[y * self.width() + x];
        Rgb([p.red(), p.green(), p.blue()])
    }
}

/// The combined source-and-sink type the boxed ditherer operates on:
/// reads pixels out of a `PixmapReader`, writes palette indices into a
/// `PaletteImage`. Pinned as the `T` parameter on
/// `Box<dyn DynDitherer<T>>` so the trait object knows exactly which
/// reader/writer pair it'll see.
type DitherInOut = ImageCombinedRW<PixmapReader, PaletteImage>;

/// A dither pipeline pre-configured for one screen. Built once at boot from
/// a `DitherConfig`; palette / strategy / level validation runs inside
/// `prepare` so misconfiguration surfaces at startup. Run-time errors are
/// still propagated as `Result` rather than panicking — boot validation
/// catches them early but isn't a substitute for runtime error handling.
pub struct PreparedDitherMethod {
    output_palette: VerifiedPalette,
    ditherer: Box<dyn DynDitherer<DitherInOut> + Send + Sync>,
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

        let ditherer = decompose_ditherer::<Rgb<u8>, [u8; 3], DitherInOut>(
            config.strategy,
            config.noise.clone(),
            config.dither_palette.as_rgb_slice(),
            config.diffuse.to_matrix(),
        )
        .map_err(|e| anyhow!("ditherer construction: {e}"))?;

        Ok(Self {
            output_palette,
            ditherer,
        })
    }

    /// Run the prepared pipeline on an image. Errors here are runtime issues
    /// (allocation, internal invariants); configuration errors were filtered
    /// out at `prepare` time, but the result is `Result` so request handlers
    /// can turn unexpected failures into 500s rather than panicking.
    pub fn run(&self, pixmap: Pixmap) -> anyhow::Result<PaletteImage> {
        let writer =
            PaletteImage::new(pixmap.width(), pixmap.height(), self.output_palette.clone());
        let reader = PixmapReader(pixmap);
        // Reader and writer are built from the same source `Pixmap`'s
        // dimensions, so `ImageCombinedRW::new` (which returns `None` on
        // mismatch) cannot fail here.
        let mut inout = ImageCombinedRW::new(reader, writer)
            .ok_or_else(|| anyhow!("reader/writer dimension mismatch"))?;
        self.ditherer.dyn_dither_into(&mut inout);
        Ok(inout.writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn prepared_color_pipeline_produces_indexed_png() {
        use crate::config::{DiffuseMethod, NoiseSource, Palette, Strategy};
        use epd_dither::decompose::octahedron::OctahedronDecomposerAxisStrategy;
        use tiny_skia::Color;

        let mut img = Pixmap::new(8, 4).expect("valid size");
        img.fill(Color::from_rgba8(200, 100, 50, 255));
        let cfg = DitherConfig {
            noise: NoiseSource::Bayer(Some(2)),
            strategy: Strategy::Octahedron(OctahedronDecomposerAxisStrategy::Closest),
            diffuse: DiffuseMethod::FloydSteinberg,
            dither_palette: Palette::Spectra6,
            output_palette: Palette::Spectra6,
        };
        let prepared = PreparedDitherMethod::prepare(&cfg).expect("prepare should succeed");
        let png_bytes = prepared
            .run(img)
            .expect("dither should succeed")
            .to_png()
            .expect("encoding should succeed");
        assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
