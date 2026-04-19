use image::{DynamicImage, RgbImage, imageops};

use crate::config::BackgroundMethod;

/// Reconcile `img` with the exact target size: error if it's larger on either
/// axis; otherwise pad according to `method` (no-op if already exact).
pub fn apply(
    img: DynamicImage,
    width: u32,
    height: u32,
    method: &BackgroundMethod,
) -> anyhow::Result<DynamicImage> {
    let (iw, ih) = (img.width(), img.height());
    anyhow::ensure!(
        iw <= width && ih <= height,
        "returned image {iw}×{ih} is larger than requested {width}×{height}",
    );
    if iw == width && ih == height {
        return Ok(img);
    }
    Ok(match method {
        BackgroundMethod::Solid(colour) => pad(&img, width, height, *colour),
        BackgroundMethod::Blur => blur(&img, width, height),
    })
}

fn pad(img: &DynamicImage, width: u32, height: u32, colour: image::Rgb<u8>) -> DynamicImage {
    let fg = img.to_rgb8();
    let mut bg = RgbImage::from_pixel(width, height, colour);
    imageops::overlay(&mut bg, &fg, offset_x(&fg, width), offset_y(&fg, height));
    DynamicImage::ImageRgb8(bg)
}

fn blur(img: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    // Cover-scale to target dims; the Gaussian blur hides the cheap resampling.
    let cover = img.resize_to_fill(width, height, imageops::FilterType::Triangle);
    let mut bg = imageops::blur(&cover.to_rgb8(), 24.0);

    let fg = img.to_rgb8();
    imageops::overlay(&mut bg, &fg, offset_x(&fg, width), offset_y(&fg, height));
    DynamicImage::ImageRgb8(bg)
}

fn offset_x(fg: &RgbImage, width: u32) -> i64 {
    width.saturating_sub(fg.width()) as i64 / 2
}

fn offset_y(fg: &RgbImage, height: u32) -> i64 {
    height.saturating_sub(fg.height()) as i64 / 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn exact_size_passes_through() {
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(200, 200, Rgb([10, 20, 30])));
        let out = apply(src, 200, 200, &BackgroundMethod::Solid(Rgb([0, 0, 0]))).unwrap();
        assert_eq!((out.width(), out.height()), (200, 200));
        assert_eq!(out.to_rgb8().get_pixel(0, 0), &Rgb([10, 20, 30]));
    }

    #[test]
    fn oversized_errors() {
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(300, 200, Rgb([0, 0, 0])));
        let err = apply(src, 200, 200, &BackgroundMethod::Solid(Rgb([0, 0, 0]))).unwrap_err();
        assert!(err.to_string().contains("larger than requested"));
    }

    #[test]
    fn solid_centres_smaller_image() {
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 80, Rgb([128, 0, 0])));
        let out = apply(src, 200, 200, &BackgroundMethod::Solid(Rgb([0, 255, 0])))
            .unwrap()
            .to_rgb8();
        assert_eq!((out.width(), out.height()), (200, 200));
        assert_eq!(out.get_pixel(100, 100), &Rgb([128, 0, 0]));
        assert_eq!(out.get_pixel(0, 0), &Rgb([0, 255, 0]));
    }
}
