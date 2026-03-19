use std::ops::{Add, Neg, Sub};

use serde::{Deserialize, Serialize};

/// TeX の寸法値 (scaled points: 1sp = 1/65536 pt)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DimensionValue(pub i64);

impl DimensionValue {
    pub const fn zero() -> Self {
        Self(0)
    }
}

impl Add for DimensionValue {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for DimensionValue {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Neg for DimensionValue {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::DimensionValue;

    #[test]
    fn supports_basic_arithmetic() {
        let left = DimensionValue(120);
        let right = DimensionValue(20);

        assert_eq!(left + right, DimensionValue(140));
        assert_eq!(left - right, DimensionValue(100));
        assert_eq!(-right, DimensionValue(-20));
        assert_eq!(DimensionValue::zero(), DimensionValue(0));
    }
}
