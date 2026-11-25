//! Screenshots of the remote screen for the model: scaled down to fit its image limits and
//! encoded as PNG (docs/ai-computer-use-plan.md §5.4).
//!
//! The pixels are the ones the session already received to show the screen: nothing is
//! captured on the remote side.

use anyhow::{Context as _, bail};
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, ImageEncoder as _, Rgba, RgbaImage};

use super::action::Region;

/// Longest screenshot side sent to the model. Claude takes up to 2576 px but recommends desktop
/// screenshots around 1280x800: fewer tokens per step, and clicks land as accurately.
pub const MAX_LONG_EDGE: u32 = 1280;

/// The remote screen as RGBA pixels, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A PNG for the model and its size in pixels.
#[derive(Debug, Clone)]
pub struct Shot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The size an area of `size` is sent at: shrunk to [`MAX_LONG_EDGE`], never enlarged.
pub fn fit((width, height): (u32, u32)) -> (u32, u32) {
    let long = width.max(height);
    if long <= MAX_LONG_EDGE {
        return (width, height);
    }
    let shrink = |side: u32| {
        let side = u64::from(side) * u64::from(MAX_LONG_EDGE);
        u32::try_from(side.div_ceil(u64::from(long))).map_or(MAX_LONG_EDGE, |side| side.max(1))
    };
    (shrink(width), shrink(height))
}

/// The whole screen.
pub fn screenshot(pixels: &Pixels) -> anyhow::Result<Shot> {
    let whole = Region {
        left: 0,
        top: 0,
        right: pixels.width,
        bottom: pixels.height,
    };
    capture(pixels, whole)
}

/// An area of the screen at up to full resolution, sized like a screenshot.
pub fn capture(pixels: &Pixels, region: Region) -> anyhow::Result<Shot> {
    let expected = u64::from(pixels.width) * u64::from(pixels.height) * 4;
    if u64::try_from(pixels.rgba.len()).ok() != Some(expected) || expected == 0 {
        bail!("no screen to capture yet");
    }
    if region.left >= region.right || region.top >= region.bottom || region.right > pixels.width || region.bottom > pixels.height {
        bail!("region {region:?} is not on the {}x{} screen", pixels.width, pixels.height);
    }
    let (row, line) = (pixels.width as usize * 4, region.width() as usize * 4);
    let mut area = Vec::with_capacity(line * region.height() as usize);
    for y in region.top as usize..region.bottom as usize {
        let start = y * row + region.left as usize * 4;
        area.extend_from_slice(&pixels.rgba[start..start + line]);
    }
    let area = RgbaImage::from_raw(region.width(), region.height(), area).context("copy the screen area")?;
    let (width, height) = fit((region.width(), region.height()));
    let area = if (width, height) == (area.width(), area.height()) {
        area
    } else {
        // Catmull-Rom keeps small text legible at a fraction of Lanczos' cost.
        imageops::resize(&area, width, height, FilterType::CatmullRom)
    };
    // The screen is opaque: RGB makes a smaller PNG.
    let rgb: Vec<u8> = area.pixels().flat_map(|Rgba([r, g, b, _])| [*r, *g, *b]).collect();
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, PngFilter::Adaptive)
        .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
        .context("encode screenshot")?;
    Ok(Shot { png, width, height })
}

#[cfg(test)]
mod tests {
    use image::{GenericImageView as _, ImageFormat};

    use super::*;

    /// A screen whose pixel at (x, y) is `[x, y, 7, 255]` (sizes up to 256).
    fn screen(width: u32, height: u32) -> Pixels {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend([u8::try_from(x % 256).unwrap(), u8::try_from(y % 256).unwrap(), 7, 255]);
            }
        }
        Pixels { width, height, rgba }
    }

    fn decode(shot: &Shot) -> image::DynamicImage {
        image::load_from_memory_with_format(&shot.png, ImageFormat::Png).expect("a PNG")
    }

    #[test]
    fn large_screens_shrink_to_the_long_edge_limit() {
        assert_eq!(fit((2560, 1600)), (1280, 800));
        assert_eq!(fit((3840, 2160)), (1280, 720));
        assert_eq!(fit((1600, 2560)), (800, 1280));
        assert_eq!(fit((1024, 768)), (1024, 768));
        assert_eq!(fit((100_000, 1)), (1280, 1));
    }

    #[test]
    fn small_screens_are_sent_as_they_are() {
        let pixels = screen(4, 3);
        let shot = screenshot(&pixels).unwrap();
        assert_eq!((shot.width, shot.height), (4, 3));
        let image = decode(&shot);
        assert_eq!(image.dimensions(), (4, 3));
        assert_eq!(image.get_pixel(3, 2).0, [3, 2, 7, 255]);
    }

    #[test]
    fn a_zoom_shows_the_area_at_full_resolution() {
        let pixels = screen(200, 100);
        let region = Region {
            left: 10,
            top: 20,
            right: 30,
            bottom: 25,
        };
        let shot = capture(&pixels, region).unwrap();
        assert_eq!((shot.width, shot.height), (20, 5));
        assert_eq!(decode(&shot).get_pixel(0, 0).0, [10, 20, 7, 255]);
    }

    #[test]
    fn a_large_screen_is_scaled_down() {
        let pixels = Pixels {
            width: 2560,
            height: 1600,
            rgba: vec![128; 2560 * 1600 * 4],
        };
        let shot = screenshot(&pixels).unwrap();
        assert_eq!((shot.width, shot.height), (1280, 800));
        assert_eq!(decode(&shot).dimensions(), (1280, 800));
    }

    #[test]
    fn no_screen_or_a_region_off_the_screen_is_an_error() {
        let empty = Pixels {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        assert!(screenshot(&empty).is_err());
        let pixels = screen(4, 3);
        let off = Region {
            left: 2,
            top: 0,
            right: 5,
            bottom: 1,
        };
        assert!(capture(&pixels, off).is_err());
    }
}
