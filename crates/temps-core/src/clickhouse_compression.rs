// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Wire compression used on the HTTP connection to the instance's ClickHouse
/// server.
///
/// The `clickhouse` client asks for compressed responses (`compress=1`) and
/// decodes exactly one codec, LZ4, but it never says which codec it wants, so
/// it silently depends on the server's `network_compression_method` default.
/// ClickHouse 26.9 changed that default to ZSTD, after which every response
/// that carries rows fails with `decompression error: incorrect magic
/// number`. Temps therefore makes the choice itself and never leaves it to the
/// server default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClickHouseCompression {
    /// Ask for LZ4-compressed responses and pin the server's
    /// `network_compression_method` to `lz4` on every query, so the server
    /// default can never disagree with what the client decodes. Default.
    #[default]
    Lz4,
    /// Ask for uncompressed responses. The escape hatch for a proxy or load
    /// balancer between Temps and ClickHouse that re-encodes response bodies
    /// and breaks the LZ4 framing.
    None,
}

impl ClickHouseCompression {
    /// Every mode, in the order the settings UI should offer them.
    pub const ALL: [ClickHouseCompression; 2] =
        [ClickHouseCompression::Lz4, ClickHouseCompression::None];

    /// The stable lowercase name used in the API, logs and error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            ClickHouseCompression::Lz4 => "lz4",
            ClickHouseCompression::None => "none",
        }
    }
}

impl std::fmt::Display for ClickHouseCompression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value that is not one of the [`ClickHouseCompression`] modes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("'{value}' is not a valid ClickHouse compression mode; expected one of: lz4, none")]
pub struct ParseClickHouseCompressionError {
    pub value: String,
}

impl std::str::FromStr for ClickHouseCompression {
    type Err = ParseClickHouseCompressionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lz4" => Ok(ClickHouseCompression::Lz4),
            "none" => Ok(ClickHouseCompression::None),
            _ => Err(ParseClickHouseCompressionError {
                value: s.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clickhouse_compression_parses_names_case_insensitively() {
        assert_eq!(
            "LZ4".parse::<ClickHouseCompression>(),
            Ok(ClickHouseCompression::Lz4)
        );
        assert_eq!(
            " none ".parse::<ClickHouseCompression>(),
            Ok(ClickHouseCompression::None)
        );
        let error = "zstd".parse::<ClickHouseCompression>().unwrap_err();
        assert_eq!(error.value, "zstd");
        assert!(error.to_string().contains("expected one of: lz4, none"));
    }

    #[test]
    fn clickhouse_compression_display_matches_wire_name() {
        for mode in ClickHouseCompression::ALL {
            assert_eq!(mode.to_string(), mode.as_str());
            assert_eq!(mode.as_str().parse::<ClickHouseCompression>(), Ok(mode));
        }
    }
}
