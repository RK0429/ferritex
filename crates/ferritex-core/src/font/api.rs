use crate::kernel::api::{DimensionValue, StableId};
use crate::typesetting::api::CharWidthProvider;

pub use super::opentype::OpenTypeFont;
pub use super::tfm::TfmMetrics;

const SCALED_POINTS_PER_POINT: i64 = 65_536;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoadedFont {
    pub id: StableId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontMetrics {
    pub ascent: DimensionValue,
    pub descent: DimensionValue,
}

pub struct OpenTypeWidthProvider<'a> {
    pub font: &'a OpenTypeFont,
    pub fallback_width: DimensionValue,
}

impl CharWidthProvider for OpenTypeWidthProvider<'_> {
    fn char_width(&self, codepoint: char) -> DimensionValue {
        self.font
            .glyph_id(u32::from(codepoint))
            .and_then(|glyph_id| self.font.advance_width(glyph_id))
            .map(|advance_width| {
                DimensionValue(
                    i64::from(advance_width) * SCALED_POINTS_PER_POINT
                        / i64::from(self.font.units_per_em()),
                )
            })
            .unwrap_or(self.fallback_width)
    }

    fn space_width(&self) -> DimensionValue {
        self.char_width(' ')
    }
}
