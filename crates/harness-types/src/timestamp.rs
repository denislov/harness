use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

#[derive(Debug, Error)]
pub enum TimestampError {
    #[error("timestamp is not valid RFC 3339: {0}")]
    InvalidRfc3339(#[from] time::error::Parse),

    #[error("timestamp must use a UTC offset")]
    NotUtc,
}

impl Timestamp {
    pub fn now_utc() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    pub fn parse(value: &str) -> Result<Self, TimestampError> {
        let parsed = OffsetDateTime::parse(value, &Rfc3339)?;
        if parsed.offset() != UtcOffset::UTC {
            return Err(TimestampError::NotUtc);
        }
        Ok(Self(parsed))
    }

    pub const fn as_datetime(self) -> OffsetDateTime {
        self.0
    }

    pub fn to_rfc3339(self) -> Result<String, time::error::Format> {
        self.0.format(&Rfc3339)
    }
}

impl Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.0.format(&Rfc3339).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&value)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_utc_rfc3339() {
        let ts = Timestamp::parse("2026-08-19T13:00:00Z").unwrap();
        assert_eq!(
            serde_json::to_string(&ts).unwrap(),
            r#""2026-08-19T13:00:00Z""#
        );
    }

    #[test]
    fn rejects_non_utc_offset() {
        assert!(Timestamp::parse("2026-08-19T14:00:00+01:00").is_err());
    }
}
