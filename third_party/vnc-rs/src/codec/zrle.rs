use crate::{PixelFormat, Rect, VncError, VncEvent};
use std::future::Future;
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::error;

use super::initialized_vec;

// [PATCH HopDesk] A rectangle is inflated in one go and parsed from memory: asking zlib for
// every pixel made a 6K screen take 0.4 s to decode.
struct Inflated<'a> {
    data: &'a [u8],
}

impl<'a> Inflated<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], VncError> {
        if len > self.data.len() {
            return Err(VncError::InvalidImageData);
        }
        let (head, rest) = self.data.split_at(len);
        self.data = rest;
        Ok(head)
    }

    fn read_u8(&mut self) -> Result<u8, VncError> {
        Ok(self.take(1)?[0])
    }
}

/// Inflates all of `input`, which must not produce more than `limit` bytes.
fn inflate(
    decompressor: &mut flate2::Decompress,
    mut input: &[u8],
    limit: usize,
) -> Result<Vec<u8>, VncError> {
    let mut output = Vec::with_capacity(limit.min(input.len().saturating_mul(4) + 1024));
    loop {
        let in_before = decompressor.total_in();
        let status = decompressor
            .decompress_vec(input, &mut output, flate2::FlushDecompress::None)
            .map_err(|_| VncError::InvalidImageData)?;
        input = &input[(decompressor.total_in() - in_before) as usize..];
        if matches!(status, flate2::Status::StreamEnd) {
            return Err(VncError::InvalidImageData);
        }
        if output.len() < output.capacity() {
            // Room is left, so zlib stopped for lack of input.
            if input.is_empty() {
                return Ok(output);
            }
            return Err(VncError::InvalidImageData);
        }
        if output.len() >= limit {
            return Err(VncError::InvalidImageData);
        }
        output.reserve((limit - output.len()).min(output.len().max(1024)));
    }
}

/// Appends one pixel; the usual 4-byte pixel is stored without a call to `memcpy`.
#[inline(always)]
fn push_pixel(pixels: &mut Vec<u8>, pixel: &[u8]) {
    match <[u8; 4]>::try_from(pixel) {
        Ok(pixel) => pixels.extend_from_slice(&pixel),
        Err(_) => pixels.extend_from_slice(pixel),
    }
}

fn read_run_length(reader: &mut Inflated, remaining: usize) -> Result<usize, VncError> {
    let mut run_length_part;
    let mut run_length = 1;
    loop {
        run_length_part = reader.read_u8()?;
        run_length += run_length_part as usize;
        if run_length > remaining {
            return Err(VncError::InvalidImageData);
        }
        if 255 != run_length_part {
            break;
        }
    }
    Ok(run_length)
}

#[inline(always)]
fn copy_true_color(
    reader: &mut Inflated,
    pixels: &mut Vec<u8>,
    pad: bool,
    compressed_bpp: usize,
    bpp: usize,
) -> Result<(), VncError> {
    let mut buf = [255; 4];
    buf[pad as usize..pad as usize + compressed_bpp].copy_from_slice(reader.take(compressed_bpp)?);
    push_pixel(pixels, &buf[..bpp]);
    Ok(())
}

#[inline(always)]
fn copy_indexed(
    palette: &[u8],
    pixels: &mut Vec<u8>,
    bpp: usize,
    index: u8,
) -> Result<(), VncError> {
    let start = index as usize * bpp;
    let pixel = palette
        .get(start..start + bpp)
        .ok_or(VncError::InvalidImageData)?;
    push_pixel(pixels, pixel);
    Ok(())
}

pub struct Decoder {
    decompressor: Option<flate2::Decompress>,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            decompressor: Some(flate2::Decompress::new(true)),
        }
    }

    pub async fn decode<S, F, Fut>(
        &mut self,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        crate::limits::dimensions(rect.width, rect.height)?;
        format.validate()?;
        let data_len = input.read_u32().await? as usize;
        if data_len > crate::limits::MAX_COMPRESSED {
            return Err(VncError::InvalidImageData);
        }
        let mut zlib_data = initialized_vec(data_len);
        input.read_exact(&mut zlib_data).await?;
        let mut decompressor = self.decompressor.take().ok_or(VncError::InvalidImageData)?;
        // No tile needs more than its pixels at 4 bytes plus a header and a 127-color palette.
        let tiles = (rect.width as usize).div_ceil(64) * (rect.height as usize).div_ceil(64);
        let limit = rect.width as usize * rect.height as usize * 4 + tiles * (1 + 127 * 4);
        let inflated = inflate(&mut decompressor, &zlib_data, limit)?;
        drop(zlib_data);
        let mut reader = Inflated { data: &inflated };

        let bpp = format.bits_per_pixel as usize / 8;
        let pixel_mask = if format.true_color_flag != 0 {
            ((format.red_max as u32) << format.red_shift)
                | ((format.green_max as u32) << format.green_shift)
                | ((format.blue_max as u32) << format.blue_shift)
        } else {
            0
        };

        let (compressed_bpp, alpha_at_first) =
            if format.bits_per_pixel == 32 && format.true_color_flag > 0 && format.depth <= 24 {
                if pixel_mask & 0x000000ff == 0 {
                    // rgb at the most significant bits
                    // if format.big_endian_flag is set
                    // then decompressed data is excepted to be [rgb.0, rgb.1, rgb.2, alpha]
                    // otherwise the decompressed data should be [alpha, rgb.0, rgb.1, rgb.2]
                    (3, format.big_endian_flag == 0)
                } else if pixel_mask & 0xff000000 == 0 {
                    // rgb at the least significant bits
                    // if format.big_endian_flag is set
                    // then decompressed data should be [alpha, rgb.0, rgb.1, rgb.2]
                    // otherwise the decompressed data should be [rgb.0, rgb.1, rgb.2, alpha]
                    (3, format.big_endian_flag > 0)
                } else {
                    (4, false)
                }
            } else {
                (bpp, false)
            };
        let mut palette = Vec::with_capacity(128 * bpp);

        let mut y = 0;
        while y < rect.height {
            let height = if y + 64 > rect.height {
                rect.height - y
            } else {
                64
            };
            let mut x = 0;
            while x < rect.width {
                let width = if x + 64 > rect.width {
                    rect.width - x
                } else {
                    64
                };
                let pixel_count = height as usize * width as usize;

                let control = reader.read_u8()?;
                let is_rle = control & 0x80 > 0;
                let palette_size = control & 0x7f;
                palette.clear();

                for _ in 0..palette_size {
                    copy_true_color(
                        &mut reader,
                        &mut palette,
                        alpha_at_first,
                        compressed_bpp,
                        bpp,
                    )?
                }

                let mut pixels = Vec::with_capacity(pixel_count * bpp);
                match (is_rle, palette_size) {
                    (false, 0) => {
                        // True Color pixels
                        let pad = alpha_at_first as usize;
                        let mut buf = [255; 4];
                        for pixel in reader
                            .take(pixel_count * compressed_bpp)?
                            .chunks_exact(compressed_bpp)
                        {
                            buf[pad..pad + compressed_bpp].copy_from_slice(pixel);
                            push_pixel(&mut pixels, &buf[..bpp]);
                        }
                    }
                    (false, 1) => {
                        // Color fill
                        for _ in 0..pixel_count {
                            copy_indexed(&palette, &mut pixels, bpp, 0)?
                        }
                    }
                    (false, 2..=16) => {
                        // Indexed pixels
                        let bits_per_index = match palette_size {
                            2 => 1,
                            3..=4 => 2,
                            5..=16 => 4,
                            _ => unreachable!(),
                        };
                        let mut encoded = reader.read_u8()?;
                        let mask = (1 << bits_per_index) - 1;

                        for y in 0..height {
                            let mut shift = 8 - bits_per_index;
                            for _ in 0..width {
                                if shift < 0 {
                                    shift = 8 - bits_per_index;
                                    encoded = reader.read_u8()?;
                                }
                                let idx = (encoded >> shift) & mask;

                                copy_indexed(&palette, &mut pixels, bpp, idx)?;
                                shift -= bits_per_index;
                            }
                            if shift < 8 - bits_per_index && y < height - 1 {
                                encoded = reader.read_u8()?;
                            }
                        }
                    }
                    (true, 0) => {
                        // True Color RLE
                        let mut count = 0;
                        let mut pixel = Vec::new();
                        while count < pixel_count {
                            pixel.clear();
                            copy_true_color(
                                &mut reader,
                                &mut pixel,
                                alpha_at_first,
                                compressed_bpp,
                                bpp,
                            )?;
                            let run_length = read_run_length(&mut reader, pixel_count - count)?;
                            for _ in 0..run_length {
                                push_pixel(&mut pixels, &pixel);
                            }
                            count += run_length;
                        }
                    }
                    (true, 2..=127) => {
                        // Indexed RLE
                        let mut count = 0;
                        while count < pixel_count {
                            let control = reader.read_u8()?;
                            let longer_than_one = control & 0x80 > 0;
                            let index = control & 0x7f;
                            let run_length = if longer_than_one {
                                read_run_length(&mut reader, pixel_count - count)?
                            } else {
                                1
                            };
                            for _ in 0..run_length {
                                copy_indexed(&palette, &mut pixels, bpp, index)?;
                            }
                            count += run_length;
                        }
                    }
                    (x, y) => {
                        error!("ZRLE subencoding error {:?}", (x, y));
                        return Err(VncError::InvalidImageData);
                    }
                }
                output_func(VncEvent::RawImage(
                    Rect {
                        x: rect.x + x,
                        y: rect.y + y,
                        width,
                        height,
                    },
                    pixels,
                ))
                .await?;
                x += width;
            }
            y += height;
        }

        if !reader.data.is_empty() {
            return Err(VncError::InvalidImageData);
        }
        self.decompressor = Some(decompressor);

        Ok(())
    }
}

#[cfg(test)]
mod tests;
