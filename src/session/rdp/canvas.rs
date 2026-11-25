//! Drawing on a frame of `0x00RRGGBB` pixels: anti-aliased shapes and text in the OS Korean UI
//! font, for the assistant overlay.

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _, point};

/// An `0xRRGGBB` color.
pub(super) type Color = u32;

/// A frame to draw on, in physical pixels.
pub(super) struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }
}

impl Canvas<'_> {
    /// Mixes `color` into the pixel by `alpha` (0 to 1); pixels off the frame are skipped.
    pub fn blend(&mut self, x: i32, y: i32, color: Color, alpha: f32) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return };
        if x >= self.width || y >= self.height || alpha <= 0.0 {
            return;
        }
        let pixel = &mut self.pixels[y * self.width + x];
        let alpha = alpha.min(1.0);
        let mix = |shift: u32| {
            let below = f32::from(u8::try_from((*pixel >> shift) & 0xff).unwrap_or(0));
            let above = f32::from(u8::try_from((color >> shift) & 0xff).unwrap_or(0));
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 255")]
            let value = (below + (above - below) * alpha).round() as u32;
            value << shift
        };
        *pixel = mix(16) | mix(8) | mix(0);
    }

    /// A rounded rectangle; `alpha` applies to the whole shape.
    pub fn round_rect(&mut self, rect: Rect, radius: f32, color: Color, alpha: f32) {
        let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0);
        self.shape(rect, color, alpha, |px, py| {
            // Distance to the rectangle shrunk by the radius, minus the radius.
            let dx = (px - (rect.x + rect.w / 2.0)).abs() - (rect.w / 2.0 - radius);
            let dy = (py - (rect.y + rect.h / 2.0)).abs() - (rect.h / 2.0 - radius);
            let outside = dx.max(0.0).hypot(dy.max(0.0));
            outside + dx.max(dy).min(0.0) - radius
        });
    }

    pub fn circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color, alpha: f32) {
        let bounds = Rect {
            x: cx - radius,
            y: cy - radius,
            w: radius * 2.0,
            h: radius * 2.0,
        };
        self.shape(bounds, color, alpha, |px, py| (px - cx).hypot(py - cy) - radius);
    }

    /// A ring of `width` around a circle.
    pub fn ring(&mut self, cx: f32, cy: f32, radius: f32, width: f32, color: Color, alpha: f32) {
        let outer = radius + width / 2.0;
        let bounds = Rect {
            x: cx - outer,
            y: cy - outer,
            w: outer * 2.0,
            h: outer * 2.0,
        };
        self.shape(bounds, color, alpha, |px, py| ((px - cx).hypot(py - cy) - radius).abs() - width / 2.0);
    }

    /// Fills where `distance` (negative inside) is below zero, with a one-pixel soft edge.
    fn shape(&mut self, bounds: Rect, color: Color, alpha: f32, distance: impl Fn(f32, f32) -> f32) {
        #[expect(clippy::cast_possible_truncation, reason = "screen coordinates")]
        let (left, top, right, bottom) = (
            bounds.x.floor() as i32 - 1,
            bounds.y.floor() as i32 - 1,
            (bounds.x + bounds.w).ceil() as i32 + 1,
            (bounds.y + bounds.h).ceil() as i32 + 1,
        );
        for y in top.max(0)..bottom {
            for x in left.max(0)..right {
                #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
                let d = distance(x as f32 + 0.5, y as f32 + 0.5);
                let coverage = (0.5 - d).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend(x, y, color, coverage * alpha);
                }
            }
        }
    }
}

/// Text in the OS UI font (Hangul included).
pub(super) struct Text {
    font: FontVec,
}

impl Text {
    /// The OS Korean UI font, else a common Latin one; `None` when none is installed.
    pub fn load() -> Option<Self> {
        let mut candidates: Vec<(std::path::PathBuf, u32)> = Vec::new();
        if let Some(font) = crate::gui::platform::ui_font() {
            candidates.push(font.regular);
        }
        candidates.push(("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into(), 0));
        candidates.into_iter().find_map(|(path, index)| {
            let data = std::fs::read(&path).ok()?;
            let font = FontVec::try_from_vec_and_index(data, index).ok()?;
            Some(Self { font })
        })
    }

    pub fn width(&self, text: &str, size: f32) -> f32 {
        let font = self.font.as_scaled(PxScale::from(size));
        text.chars().map(|c| font.h_advance(font.glyph_id(c))).sum()
    }

    /// Height of a line at `size`.
    pub fn line_height(&self, size: f32) -> f32 {
        let font = self.font.as_scaled(PxScale::from(size));
        (font.ascent() - font.descent() + font.line_gap()) * 1.15
    }

    /// `text` in lines no wider than `width`: at spaces where possible, else between characters.
    pub fn wrap(&self, text: &str, size: f32, width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            for word in paragraph.split(' ') {
                let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
                if self.width(&candidate, size) <= width {
                    line = candidate;
                    continue;
                }
                if !line.is_empty() {
                    lines.push(core::mem::take(&mut line));
                }
                for c in word.chars() {
                    line.push(c);
                    if self.width(&line, size) > width && line.chars().count() > 1 {
                        line.pop();
                        lines.push(core::mem::replace(&mut line, c.to_string()));
                    }
                }
            }
            lines.push(line);
        }
        lines
    }

    /// Draws one line with its top at `y`; returns where it ends.
    pub fn draw(&self, canvas: &mut Canvas<'_>, x: f32, y: f32, text: &str, size: f32, color: Color) -> f32 {
        let scale = PxScale::from(size);
        let scaled = self.font.as_scaled(scale);
        let baseline = y + scaled.ascent();
        let mut caret = x;
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            let glyph = id.with_scale_and_position(scale, point(caret, baseline));
            caret += scaled.h_advance(id);
            let Some(outline) = self.font.outline_glyph(glyph) else { continue };
            let bounds = outline.px_bounds();
            #[expect(clippy::cast_possible_truncation, reason = "screen coordinates")]
            let (left, top) = (bounds.min.x as i32, bounds.min.y as i32);
            outline.draw(|gx, gy, coverage| {
                let (Ok(gx), Ok(gy)) = (i32::try_from(gx), i32::try_from(gy)) else { return };
                canvas.blend(left + gx, top + gy, color, coverage);
            });
        }
        caret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(pixels: &mut [u32], width: usize) -> Canvas<'_> {
        let height = pixels.len() / width;
        Canvas { pixels, width, height }
    }

    #[test]
    fn blending_mixes_each_channel() {
        let mut pixels = [0x00_00_00_00];
        canvas(&mut pixels, 1).blend(0, 0, 0xff_80_00, 0.5);
        assert_eq!(pixels[0], 0x80_40_00);
        canvas(&mut pixels, 1).blend(5, 0, 0xff_ff_ff, 1.0);
        assert_eq!(pixels[0], 0x80_40_00, "off the frame");
    }

    #[test]
    fn a_circle_fills_its_middle_and_spares_the_corners() {
        let mut pixels = [0u32; 100];
        canvas(&mut pixels, 10).circle(5.0, 5.0, 4.0, 0xff_ff_ff, 1.0);
        assert_eq!(pixels[5 * 10 + 5], 0xff_ff_ff);
        assert_eq!(pixels[0], 0);
    }

    #[test]
    fn long_text_wraps_at_spaces_then_between_characters() {
        let Some(text) = Text::load() else { return };
        let size = 16.0;
        let width = text.width("hello world", size) + 1.0;
        assert_eq!(text.wrap("hello world again", size, width), ["hello world", "again"]);
        let narrow = text.width("abc", size) + 1.0;
        assert_eq!(text.wrap("abcdef", size, narrow), ["abc", "def"]);
        assert_eq!(text.wrap("a\nb", size, 100.0), ["a", "b"]);
    }
}
