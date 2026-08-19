use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind} must be a non-empty opaque identifier")]
pub struct IdentifierError {
    kind: &'static str,
}

impl IdentifierError {
    pub const fn kind(&self) -> &'static str {
        self.kind
    }
}

macro_rules! define_identifier {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                if value.is_empty() {
                    return Err(IdentifierError { kind: $kind });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentifierError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdentifierError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

define_identifier!(SessionId, "SessionId");
define_identifier!(AgentInstanceId, "AgentInstanceId");
define_identifier!(EventId, "EventId");
define_identifier!(MessageId, "MessageId");
define_identifier!(RequestId, "RequestId");
define_identifier!(ToolCallId, "ToolCallId");
define_identifier!(InvocationId, "InvocationId");
define_identifier!(IdempotencyKey, "IdempotencyKey");
define_identifier!(ProviderId, "ProviderId");
define_identifier!(BlobId, "BlobId");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_opaque_strings_on_the_wire() {
        let id = SessionId::new("ses_example").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""ses_example""#);
        assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), id);
    }

    #[test]
    fn empty_id_is_rejected() {
        assert!(SessionId::new("").is_err());
    }
}
