//! Drawing on a frame of `0x00RRGGBB` pixels: anti-aliased shapes and text in the OS Korean UI
//! font, for the assistant overlay.

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _, point};

use super::icons::{self, Glyph};

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
        self.shape(rect, color, alpha, |px, py| {
            rounded_distance(rect, radius, px, py)
        });
    }

    /// The outline of a rounded rectangle, `width` wide, inside its edge.
    pub fn round_rect_stroke(
        &mut self,
        rect: Rect,
        radius: f32,
        width: f32,
        color: Color,
        alpha: f32,
    ) {
        self.shape(rect, color, alpha, |px, py| {
            (rounded_distance(rect, radius, px, py) + width / 2.0).abs() - width / 2.0
        });
    }

    /// Frosted glass: what is behind the rounded rectangle, blurred and tinted by `strength`.
    pub fn frost(&mut self, rect: Rect, radius: f32, tint: Color, strength: f32) {
        /// Pixels per blur cell; the blur reaches about three cells.
        const CELL: usize = 8;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped screen coordinates"
        )]
        let (left, top, right, bottom) = (
            rect.x.floor().max(0.0) as usize,
            rect.y.floor().max(0.0) as usize,
            ((rect.x + rect.w).ceil().max(0.0) as usize).min(self.width),
            ((rect.y + rect.h).ceil().max(0.0) as usize).min(self.height),
        );
        if left >= right || top >= bottom {
            return;
        }
        let (gw, gh) = (
            (right - left).div_ceil(CELL) + 1,
            (bottom - top).div_ceil(CELL) + 1,
        );
        let channels = |pixel: u32| {
            let [_, r, g, b] = pixel.to_be_bytes();
            [f32::from(r), f32::from(g), f32::from(b)]
        };
        let mut grid = vec![[0.0f32; 3]; gw * gh];
        for gy in 0..gh {
            for gx in 0..gw {
                let mut sum = [0.0f32; 3];
                for oy in [0, CELL / 2, CELL] {
                    for ox in [0, CELL / 2, CELL] {
                        let x = (left + gx * CELL + ox)
                            .saturating_sub(CELL / 2)
                            .clamp(left, right - 1);
                        let y = (top + gy * CELL + oy)
                            .saturating_sub(CELL / 2)
                            .clamp(top, bottom - 1);
                        let c = channels(self.pixels[y * self.width + x]);
                        for i in 0..3 {
                            sum[i] += c[i] / 9.0;
                        }
                    }
                }
                grid[gy * gw + gx] = sum;
            }
        }
        for _ in 0..3 {
            let source = grid.clone();
            for gy in 0..gh {
                for gx in 0..gw {
                    let mut sum = [0.0f32; 3];
                    for ny in gy.saturating_sub(1)..=(gy + 1).min(gh - 1) {
                        for nx in gx.saturating_sub(1)..=(gx + 1).min(gw - 1) {
                            for i in 0..3 {
                                sum[i] += source[ny * gw + nx][i];
                            }
                        }
                    }
                    #[expect(clippy::cast_precision_loss, reason = "up to nine cells")]
                    let count = (((gy + 1).min(gh - 1) - gy.saturating_sub(1) + 1)
                        * ((gx + 1).min(gw - 1) - gx.saturating_sub(1) + 1))
                        as f32;
                    grid[gy * gw + gx] = sum.map(|v| v / count);
                }
            }
        }
        let tint = channels(tint);
        for y in top..bottom {
            for x in left..right {
                #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let coverage = (0.5 - rounded_distance(rect, radius, px, py)).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
                let (fx, fy) = (
                    (x - left) as f32 / CELL as f32,
                    (y - top) as f32 / CELL as f32,
                );
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "within the grid"
                )]
                let (x0, y0) = ((fx as usize).min(gw - 1), (fy as usize).min(gh - 1));
                let (x1, y1) = ((x0 + 1).min(gw - 1), (y0 + 1).min(gh - 1));
                #[expect(clippy::cast_precision_loss, reason = "within the grid")]
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                let mut color = 0u32;
                for i in 0..3 {
                    let upper = grid[y0 * gw + x0][i] * (1.0 - tx) + grid[y0 * gw + x1][i] * tx;
                    let lower = grid[y1 * gw + x0][i] * (1.0 - tx) + grid[y1 * gw + x1][i] * tx;
                    let blurred = upper * (1.0 - ty) + lower * ty;
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "0 to 255"
                    )]
                    let value = (blurred + (tint[i] - blurred) * strength)
                        .round()
                        .clamp(0.0, 255.0) as u32;
                    color = (color << 8) | value;
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_possible_wrap,
                    reason = "screen coordinates"
                )]
                self.blend(x as i32, y as i32, color, coverage);
            }
        }
    }

    /// A glossy sphere whose three colors swirl with `phase` (radians).
    pub fn orb(&mut self, cx: f32, cy: f32, radius: f32, colors: [Color; 3], phase: f32) {
        #[expect(clippy::cast_possible_truncation, reason = "screen coordinates")]
        let (left, top, right, bottom) = (
            (cx - radius).floor() as i32 - 1,
            (cy - radius).floor() as i32 - 1,
            (cx + radius).ceil() as i32 + 1,
            (cy + radius).ceil() as i32 + 1,
        );
        for y in top..bottom {
            for x in left..right {
                #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
                let (dx, dy) = (
                    (x as f32 + 0.5 - cx) / radius,
                    (y as f32 + 0.5 - cy) / radius,
                );
                let distance = dx.hypot(dy);
                let coverage = ((1.0 - distance) * radius + 0.5).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                let angle = dy.atan2(dx) + phase;
                let first = 0.5 + 0.5 * (angle + distance * 2.0).sin();
                let second = 0.5 + 0.5 * (angle - phase * 0.7 + dy * 1.5).cos();
                let mut color = mix(mix(colors[0], colors[1], first), colors[2], second * 0.6);
                // Light from the upper left, and a darker rim.
                let highlight = (1.0 - (dx + 0.35).hypot(dy + 0.4) / 0.9).max(0.0);
                color = mix(color, 0xff_ff_ff, highlight * highlight * 0.5);
                color = mix(color, 0x00_00_00, distance.powi(3) * 0.2);
                self.blend(x, y, color, coverage);
            }
        }
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

    /// An icon `size` pixels square centered on (`cx`, `cy`), tinted `color`.
    pub fn icon(&mut self, glyph: Glyph, cx: f32, cy: f32, size: f32, color: Color, alpha: f32) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small pixel size"
        )]
        let side = size.round().max(1.0) as usize;
        let Some(mask) = icons::mask(glyph, side) else {
            return;
        };
        #[expect(clippy::cast_possible_truncation, reason = "screen coordinates")]
        let (left, top) = (
            (cx - size / 2.0).round() as i32,
            (cy - size / 2.0).round() as i32,
        );
        for (index, coverage) in mask.coverage.iter().enumerate() {
            if *coverage == 0 {
                continue;
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_possible_wrap,
                reason = "inside the icon"
            )]
            let (x, y) = (
                left + (index % mask.size) as i32,
                top + (index / mask.size) as i32,
            );
            self.blend(x, y, color, f32::from(*coverage) / 255.0 * alpha);
        }
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

    /// A soft black shadow around `rect`: `layers` faint rounded rectangles, each `step` pixels
    /// wider than the last, as if drawn one over another, but in one pass over the pixels.
    pub fn soft_shadow(&mut self, rect: Rect, radius: f32, step: f32, layers: u8, alpha: f32) {
        let spread = f32::from(layers) * step;
        // Where every layer covers the pixel fully (well inside), the shade is the same.
        let covered = (1.0 - alpha).powi(i32::from(layers));
        #[expect(clippy::cast_possible_truncation, reason = "screen coordinates")]
        let (left, top, right, bottom) = (
            (rect.x - spread).floor() as i32 - 1,
            (rect.y - spread).floor() as i32 - 1,
            (rect.x + rect.w + spread).ceil() as i32 + 1,
            (rect.y + rect.h + spread).ceil() as i32 + 1,
        );
        for y in top.max(0)..bottom {
            for x in left.max(0)..right {
                #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // A rounded rectangle grown by `grow` (its radius too) lies `grow` further out, so
                // one distance serves every layer. What shows through all of them:
                let distance = rounded_distance(rect, radius, px, py);
                if distance >= spread + 0.5 {
                    continue;
                }
                let through = if distance <= step - 0.5 {
                    covered
                } else {
                    (1..=layers).fold(1.0, |through, layer| {
                        let coverage = (0.5 - (distance - f32::from(layer) * step)).clamp(0.0, 1.0);
                        through * (1.0 - alpha * coverage)
                    })
                };
                self.blend(x, y, 0, 1.0 - through);
            }
        }
    }

    /// A polygon through `points`, grown by `grow` pixels on every side (for an outline drawn
    /// under it).
    pub fn polygon(&mut self, points: &[(f32, f32)], grow: f32, color: Color, alpha: f32) {
        if points.len() < 3 {
            return;
        }
        let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in points {
            (left, top, right, bottom) = (left.min(*x), top.min(*y), right.max(*x), bottom.max(*y));
        }
        let bounds = Rect {
            x: left - grow,
            y: top - grow,
            w: right - left + 2.0 * grow,
            h: bottom - top + 2.0 * grow,
        };
        self.shape(bounds, color, alpha, |px, py| polygon_distance(points, px, py) - grow);
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

/// Distance from a point to a rounded rectangle's edge, negative inside.
fn rounded_distance(rect: Rect, radius: f32, px: f32, py: f32) -> f32 {
    let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0);
    // Distance to the rectangle shrunk by the radius, minus the radius.
    let dx = (px - (rect.x + rect.w / 2.0)).abs() - (rect.w / 2.0 - radius);
    let dy = (py - (rect.y + rect.h / 2.0)).abs() - (rect.h / 2.0 - radius);
    dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius
}

/// Distance from a point to a polygon's edge, negative inside (even-odd rule).
fn polygon_distance(points: &[(f32, f32)], px: f32, py: f32) -> f32 {
    let mut distance = f32::MAX;
    let mut inside = false;
    let mut previous = points[points.len() - 1];
    for &(bx, by) in points {
        let (ax, ay) = previous;
        let (ex, ey) = (bx - ax, by - ay);
        let (wx, wy) = (px - ax, py - ay);
        let t = ((wx * ex + wy * ey) / (ex * ex + ey * ey).max(f32::EPSILON)).clamp(0.0, 1.0);
        distance = distance.min((wx - ex * t).hypot(wy - ey * t));
        if (ay > py) != (by > py) && px < ax + (py - ay) * ex / ey {
            inside = !inside;
        }
        previous = (bx, by);
    }
    if inside { -distance } else { distance }
}

/// The color `t` of the way from `a` to `b`.
pub(super) fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let (from, to) = (
            f32::from(u8::try_from((a >> shift) & 0xff).unwrap_or(0)),
            f32::from(u8::try_from((b >> shift) & 0xff).unwrap_or(0)),
        );
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "0 to 255"
        )]
        let value = (from + (to - from) * t).round() as u32;
        value << shift
    };
    channel(16) | channel(8) | channel(0)
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
    fn frost_blurs_what_is_behind_and_tints_it() {
        // Left half black, right half white.
        let mut pixels: Vec<u32> = (0..64 * 64)
            .map(|i| if i % 64 < 32 { 0 } else { 0xff_ff_ff })
            .collect();
        let whole = Rect {
            x: 0.0,
            y: 0.0,
            w: 64.0,
            h: 64.0,
        };
        canvas(&mut pixels, 64).frost(whole, 0.0, 0xff_ff_ff, 0.0);
        let middle = pixels[32 * 64 + 32] & 0xff;
        assert!((60..200).contains(&middle), "the edge is smeared: {middle}");
        canvas(&mut pixels, 64).frost(whole, 0.0, 0xff_00_00, 1.0);
        assert_eq!(
            pixels[32 * 64 + 5],
            0xff_00_00,
            "a full tint hides what is behind"
        );
    }

    #[test]
    fn a_soft_shadow_looks_like_its_layers_drawn_one_by_one() {
        let rect = Rect { x: 20.0, y: 15.0, w: 50.0, h: 30.0 };
        let (radius, step, layers, alpha) = (8.0, 1.5, 10u8, 0.04);
        let mut layered = vec![0xc0_d0_e0u32; 100 * 70];
        {
            let mut layered = Canvas { pixels: &mut layered, width: 100, height: 70 };
            for layer in 1..=layers {
                let grow = f32::from(layer) * step;
                let shadow = Rect { x: rect.x - grow, y: rect.y - grow, w: rect.w + 2.0 * grow, h: rect.h + 2.0 * grow };
                layered.round_rect(shadow, radius + grow, 0, alpha);
            }
        }
        let mut single = vec![0xc0_d0_e0u32; 100 * 70];
        Canvas { pixels: &mut single, width: 100, height: 70 }.soft_shadow(rect, radius, step, layers, alpha);
        let channels = |pixel: u32| pixel.to_be_bytes().map(i16::from);
        for (index, (a, b)) in layered.iter().zip(&single).enumerate() {
            let (a, b) = (channels(*a), channels(*b));
            for i in 1..4 {
                assert!((a[i] - b[i]).abs() <= 2, "pixel {index}: {a:?} vs {b:?}");
            }
        }
        assert_ne!(single[35 * 100 + 45], 0xc0_d0_e0, "darker under the surface");
        assert_eq!(single[0], 0xc0_d0_e0, "untouched far from it");
    }

    #[test]
    fn colors_mix_by_channel() {
        assert_eq!(mix(0x00_00_00, 0xff_80_00, 0.5), 0x80_40_00);
        assert_eq!(mix(0x12_34_56, 0xff_ff_ff, 0.0), 0x12_34_56);
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
