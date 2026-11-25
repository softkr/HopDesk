//! Icons for the assistant overlay: the launcher's Lucide SVGs, rasterized once per size into
//! coverage masks that the canvas tints with any color.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use resvg::{tiny_skia, usvg};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Glyph {
    Sparkles,
    ArrowUp,
    Square,
    Check,
    X,
    Hand,
    Pointer,
    Zap,
}

impl Glyph {
    fn svg(self) -> &'static str {
        match self {
            Self::Sparkles => include_str!("../../../assets/icons/lucide/sparkles.svg"),
            Self::ArrowUp => include_str!("../../../assets/icons/lucide/arrow-up.svg"),
            Self::Square => include_str!("../../../assets/icons/lucide/square.svg"),
            Self::Check => include_str!("../../../assets/icons/lucide/check.svg"),
            Self::X => include_str!("../../../assets/icons/lucide/x.svg"),
            Self::Hand => include_str!("../../../assets/icons/lucide/hand.svg"),
            Self::Pointer => include_str!("../../../assets/icons/lucide/mouse-pointer-2.svg"),
            Self::Zap => include_str!("../../../assets/icons/lucide/zap.svg"),
        }
    }
}

/// How much of each pixel of a square icon is covered, row by row.
pub(super) struct Mask {
    pub size: usize,
    pub coverage: Vec<u8>,
}

type Cache = Mutex<HashMap<(Glyph, usize), Option<Arc<Mask>>>>;

static MASKS: LazyLock<Cache> = LazyLock::new(Cache::default);

/// The icon at `size` pixels square; `None` if it cannot be drawn.
pub(super) fn mask(glyph: Glyph, size: usize) -> Option<Arc<Mask>> {
    MASKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry((glyph, size))
        .or_insert_with(|| rasterize(glyph, size).map(Arc::new))
        .clone()
}

fn rasterize(glyph: Glyph, size: usize) -> Option<Mask> {
    let side = u32::try_from(size).ok().filter(|side| *side > 0)?;
    let tree = usvg::Tree::from_str(glyph.svg(), &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(side, side)?;
    #[expect(clippy::cast_precision_loss, reason = "icon sizes are small")]
    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // The strokes are white, so a pixel's alpha is how much of it the icon covers.
    Some(Mask {
        size,
        coverage: pixmap.pixels().iter().map(|pixel| pixel.alpha()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_draws_something_inside_its_square() {
        for glyph in [
            Glyph::Sparkles,
            Glyph::ArrowUp,
            Glyph::Square,
            Glyph::Check,
            Glyph::X,
            Glyph::Hand,
            Glyph::Pointer,
            Glyph::Zap,
        ] {
            let mask = mask(glyph, 24).unwrap_or_else(|| panic!("{glyph:?} draws"));
            assert_eq!(mask.coverage.len(), 24 * 24);
            assert!(
                mask.coverage.iter().any(|c| *c > 200),
                "{glyph:?} has solid strokes"
            );
            assert!(mask.coverage[0] == 0, "{glyph:?} leaves its corner empty");
        }
    }

    #[test]
    fn a_zero_size_is_refused() {
        assert!(mask(Glyph::Check, 0).is_none());
    }
}
