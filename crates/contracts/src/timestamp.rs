use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

/// UTC timestamp that serializes as an RFC 3339 string ending in `Z`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UtcTimestamp(OffsetDateTime);

impl UtcTimestamp {
    /// Creates a timestamp normalized to UTC.
    #[must_use]
    pub fn new(value: OffsetDateTime) -> Self {
        Self(value.to_offset(UtcOffset::UTC))
    }

    /// Returns the normalized timestamp.
    #[must_use]
    pub const fn into_inner(self) -> OffsetDateTime {
        self.0
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.0.format(&Rfc3339).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&value)
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TimestampVisitor;

        impl de::Visitor<'_> for TimestampVisitor {
            type Value = UtcTimestamp;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an RFC 3339 timestamp with a UTC offset")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                OffsetDateTime::parse(value, &Rfc3339)
                    .map(UtcTimestamp::new)
                    .map_err(E::custom)
            }
        }

        deserializer.deserialize_str(TimestampVisitor)
    }
}
