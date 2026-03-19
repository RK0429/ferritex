use crate::kernel::api::{DimensionValue, StableId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoadedFont {
    pub id: StableId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontMetrics {
    pub ascent: DimensionValue,
    pub descent: DimensionValue,
}
