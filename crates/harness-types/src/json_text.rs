use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::value::RawValue;
use thiserror::Error;

/// One complete JSON value encoded as text.
///
/// The original text is retained byte-for-byte, including insignificant
/// whitespace. Validation happens at construction/deserialization time.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JsonText(String);

#[derive(Debug, Error)]
#[error("value is not one complete JSON value: {0}")]
pub struct JsonTextError(#[from] serde_json::Error);

impl JsonText {
    pub fn new(value: String) -> Result<Self, JsonTextError> {
        let _: &RawValue = serde_json::from_str(&value)?;
        Ok(Self(value))
    }

    pub fn from_static(value: &'static str) -> Result<Self, JsonTextError> {
        Self::new(value.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for JsonText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for JsonText {
    type Error = JsonTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for JsonText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // argumentsJson is itself a JSON string field in the wire/domain schema.
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for JsonText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_json_text_as_a_string() {
        let value = JsonText::new(r#"  { "path": "README.md" }  "#.to_owned()).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        let decoded: JsonText = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.as_str(), value.as_str());
    }

    #[test]
    fn rejects_multiple_json_values() {
        assert!(JsonText::new("{} {}".to_owned()).is_err());
    }
}
