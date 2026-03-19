use serde::{Deserialize, Serialize};

/// コンパイルパスをまたいで安定な識別子
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StableId(pub u64);

#[cfg(test)]
mod tests {
    use super::StableId;

    #[test]
    fn stable_ids_compare_by_value() {
        assert_eq!(StableId(42), StableId(42));
        assert_ne!(StableId(42), StableId(7));
    }
}
