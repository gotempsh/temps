//! Pagination cursor for the merged event stream.
//!
//! The merge service returns rows ordered by `(ts DESC, kind, id)`. To page
//! deeper, the next request must skip past the last row we returned. We
//! encode (ts, kind, id) into an opaque base64 token rather than exposing
//! the columns directly so the wire format is stable while we evolve the
//! sort order.

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ObservabilityError;
use crate::types::EventKind;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Cursor {
    /// Last returned row's timestamp.
    pub ts: DateTime<Utc>,
    /// Last returned row's kind.
    pub kind: EventKind,
    /// Last returned row's per-kind id (string so it works for log chunk
    /// composites and integer PKs alike).
    pub id: String,
}

impl Cursor {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    }

    pub fn decode(token: &str) -> Result<Cursor, ObservabilityError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token.as_bytes())
            .map_err(|e| ObservabilityError::InvalidCursor {
                reason: format!("base64 decode failed: {}", e),
            })?;
        serde_json::from_slice(&bytes).map_err(|e| ObservabilityError::InvalidCursor {
            reason: format!("json decode failed: {}", e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Cursor {
        Cursor {
            ts: DateTime::parse_from_rfc3339("2026-05-01T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            kind: EventKind::Request,
            id: "42".into(),
        }
    }

    #[test]
    fn round_trip_through_base64() {
        let c = sample();
        let token = c.encode();
        let back = Cursor::decode(&token).expect("valid cursor");
        assert_eq!(c, back);
    }

    #[test]
    fn rejects_garbage_base64() {
        let err = Cursor::decode("not_base64!!").unwrap_err();
        assert!(matches!(err, ObservabilityError::InvalidCursor { .. }));
    }

    #[test]
    fn rejects_valid_base64_but_invalid_json() {
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"not json");
        let err = Cursor::decode(&token).unwrap_err();
        assert!(matches!(err, ObservabilityError::InvalidCursor { .. }));
    }
}
