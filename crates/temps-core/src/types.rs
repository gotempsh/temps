//! Custom types for common data structures and validation

use chrono::{DateTime as ChronoDateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::Deref;
use utoipa::ToSchema;

/// Database DateTime type used across all Temps crates
///
/// This is the canonical datetime type for:
/// - Database TIMESTAMPTZ columns
/// - Time bucketing results from TimescaleDB
///
/// # Example
/// ```rust
/// use temps_core::DBDateTime;
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// pub struct Response {
///     pub created_at: DBDateTime,
/// }
/// ```
pub type DBDateTime = ChronoDateTime<Utc>;

/// Standard UTC DateTime type used across all Temps crates
///
/// This is the canonical datetime type for:
/// - API responses (serializes as ISO 8601 with 'Z' suffix: `2025-10-12T12:15:47.609192Z`)
/// - Database TIMESTAMPTZ columns
/// - Time bucketing results from TimescaleDB
///
/// # Example
/// ```rust
/// use temps_core::UtcDateTime;
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// pub struct Response {
///     #[schema(value_type = String, format = DateTime)]
///     pub created_at: UtcDateTime,
/// }
/// ```
///
/// # OpenAPI Schema
/// When using with utoipa, add the schema attribute:
/// ```rust
/// #[schema(value_type = String, format = DateTime)]
/// pub field: UtcDateTime,
/// ```
pub type UtcDateTime = ChronoDateTime<Utc>;

/// Wrapper type for DateTime<Utc> that automatically parses ISO 8601 format
/// Accepts multiple formats:
/// - `2024-01-15T14:30:00` (naive datetime, assumes UTC)
/// - `2024-01-15T14:30:00Z` (UTC)
/// - `2024-01-15T14:30:00+00:00` (with timezone offset)
///
/// All formats are converted to DateTime<Utc>. Serializes with 'Z' suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ToSchema)]
#[schema(value_type = String, example = "2024-01-15T14:30:00Z")]
pub struct DateTime(pub ChronoDateTime<Utc>);

impl<'de> Deserialize<'de> for DateTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;

        // Try parsing as RFC3339 (with timezone) first
        if let Ok(dt) = s.parse::<ChronoDateTime<Utc>>() {
            return Ok(DateTime(dt));
        }

        // Try parsing as naive datetime (YYYY-MM-DDTHH:MM:SS) and assume UTC
        if let Ok(naive_dt) = NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S") {
            let dt = ChronoDateTime::<Utc>::from_naive_utc_and_offset(naive_dt, Utc);
            return Ok(DateTime(dt));
        }

        Err(serde::de::Error::custom(
            "Invalid datetime format. Use ISO 8601: YYYY-MM-DDTHH:MM:SSZ",
        ))
    }
}

impl Serialize for DateTime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Serialize as RFC3339 with 'Z' suffix
        let formatted = self.0.to_rfc3339();
        serializer.serialize_str(&formatted)
    }
}

// Allow using DateTime like DateTime<Utc>
impl Deref for DateTime {
    type Target = ChronoDateTime<Utc>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// Conversions
impl From<ChronoDateTime<Utc>> for DateTime {
    fn from(dt: ChronoDateTime<Utc>) -> Self {
        DateTime(dt)
    }
}

impl From<DateTime> for ChronoDateTime<Utc> {
    fn from(dt: DateTime) -> Self {
        dt.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};
    use serde_json;

    #[test]
    fn test_datetime_deserialize_valid() {
        let json = r#""2024-01-15T14:30:00""#;
        let dt: DateTime = serde_json::from_str(json).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn test_datetime_deserialize_invalid() {
        let json = r#""invalid-date""#;
        let result: Result<DateTime, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_datetime_serialize() {
        let naive =
            NaiveDateTime::parse_from_str("2024-01-15T14:30:00", "%Y-%m-%dT%H:%M:%S").unwrap();
        let dt = DateTime(ChronoDateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        let json = serde_json::to_string(&dt).unwrap();
        assert_eq!(json, r#""2024-01-15T14:30:00+00:00""#);
    }

    #[test]
    fn test_datetime_deref() {
        let naive =
            NaiveDateTime::parse_from_str("2024-01-15T14:30:00", "%Y-%m-%dT%H:%M:%S").unwrap();
        let dt = DateTime(ChronoDateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        assert_eq!(dt.year(), 2024);
        assert_eq!(*dt, dt.0);
    }

    #[test]
    fn test_datetime_in_struct() {
        #[derive(Deserialize, Serialize)]
        struct Query {
            start_time: Option<DateTime>,
            end_time: Option<DateTime>,
        }

        // Can deserialize from naive format
        let json = r#"{"start_time":"2024-01-15T14:30:00","end_time":"2024-01-15T18:30:00"}"#;
        let query: Query = serde_json::from_str(json).unwrap();
        assert!(query.start_time.is_some());
        assert!(query.end_time.is_some());

        // Serializes to RFC3339 format with timezone
        let serialized = serde_json::to_string(&query).unwrap();
        assert_eq!(
            serialized,
            r#"{"start_time":"2024-01-15T14:30:00+00:00","end_time":"2024-01-15T18:30:00+00:00"}"#
        );
    }

    #[test]
    fn test_datetime_deserialize_rfc3339_utc() {
        let json = r#""2024-01-15T14:30:00Z""#;
        let dt: DateTime = serde_json::from_str(json).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn test_datetime_deserialize_rfc3339_offset() {
        let json = r#""2024-01-15T14:30:00+00:00""#;
        let dt: DateTime = serde_json::from_str(json).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn test_datetime_deserialize_rfc3339_timezone_conversion() {
        // 2024-01-15 16:30:00 in +02:00 timezone should be 14:30:00 UTC
        let json = r#""2024-01-15T16:30:00+02:00""#;
        let dt: DateTime = serde_json::from_str(json).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 14); // Converted to UTC
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn test_datetime_multiple_formats() {
        // All these should parse successfully
        let formats = vec![
            r#""2024-01-15T14:30:00""#,       // Naive
            r#""2024-01-15T14:30:00Z""#,      // UTC
            r#""2024-01-15T14:30:00+00:00""#, // UTC with offset
        ];

        for format in formats {
            let result: Result<DateTime, _> = serde_json::from_str(format);
            assert!(result.is_ok(), "Failed to parse: {}", format);
        }
    }
}
