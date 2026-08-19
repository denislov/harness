use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

pub const MAX_JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("{kind} value {value} exceeds the cross-language maximum {MAX_JS_SAFE_INTEGER}")]
pub struct CounterError {
    kind: &'static str,
    value: u64,
}

impl CounterError {
    pub const fn kind(&self) -> &'static str {
        self.kind
    }

    pub const fn value(&self) -> u64 {
        self.value
    }
}

macro_rules! define_counter {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            pub const ZERO: Self = Self(0);
            pub const FIRST: Self = Self(1);

            pub fn new(value: u64) -> Result<Self, CounterError> {
                if value > MAX_JS_SAFE_INTEGER {
                    return Err(CounterError { kind: $kind, value });
                }
                Ok(Self(value))
            }

            pub const fn get(self) -> u64 {
                self.0
            }

            pub fn checked_next(self) -> Result<Self, CounterError> {
                let value = self.0.checked_add(1).unwrap_or(u64::MAX);
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl TryFrom<u64> for $name {
            type Error = CounterError;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_u64(self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = u64::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

define_counter!(EventSeq, "EventSeq");
define_counter!(TurnNo, "TurnNo");
define_counter!(StepNo, "StepNo");
define_counter!(StreamSeq, "StreamSeq");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_values_that_are_not_js_safe_integers() {
        assert!(EventSeq::new(MAX_JS_SAFE_INTEGER).is_ok());
        assert!(EventSeq::new(MAX_JS_SAFE_INTEGER + 1).is_err());
    }
}
