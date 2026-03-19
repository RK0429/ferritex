use serde::{Deserialize, Serialize};

/// ソースファイル内の位置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceLocation {
    pub file_id: u32,
    pub line: u32,
    pub column: u32,
}

/// ソースファイル内の範囲
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    pub start: SourceLocation,
    pub end: SourceLocation,
}

#[cfg(test)]
mod tests {
    use super::{SourceLocation, SourceSpan};

    #[test]
    fn spans_with_the_same_bounds_are_equal() {
        let start = SourceLocation {
            file_id: 1,
            line: 10,
            column: 4,
        };
        let end = SourceLocation {
            file_id: 1,
            line: 10,
            column: 12,
        };

        let left = SourceSpan { start, end };
        let right = SourceSpan { start, end };

        assert_eq!(left, right);
    }
}
