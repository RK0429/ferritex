use crate::kernel::api::DimensionValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsBox {
    pub width: DimensionValue,
    pub height: DimensionValue,
}
