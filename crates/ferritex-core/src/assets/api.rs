use crate::kernel::api::StableId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogicalAssetId(pub StableId);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssetHandle {
    pub id: LogicalAssetId,
}
