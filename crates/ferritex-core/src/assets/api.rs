use serde::{Deserialize, Serialize};

use crate::kernel::api::StableId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogicalAssetId(pub StableId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetHandle {
    pub id: LogicalAssetId,
}
