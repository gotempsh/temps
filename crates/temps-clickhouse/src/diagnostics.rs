// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Telling "the server answered in a codec we cannot decode" apart from every
//! other way a ClickHouse call fails, and the startup probe that does so.
//!
//! The distinction matters because the remedies differ completely. A refused
//! connection heals by waiting; a decode failure never does — it persists until
//! the server's `network_compression_method` or Temps' compression setting
//! changes. Retry loops and the health monitor use [`FailureKind`] to say so in
//! the log instead of looking like a flaky network.

use std::time::Duration;

use clickhouse::error::Error as ChError;
use temps_core::ClickHouseCompression;
use tracing::debug;

use crate::{server_version, ClickHouseConfig, ClickHouseError, ServerVersion};

/// How long the startup probe waits for `SELECT version()`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The remedies name `PUT /settings` rather than a console control: the
/// ClickHouse section of the settings page is a separate follow-up, and a
/// remedy must never point at a control that is not there yet.
///
/// What to do about a decode failure when the codec the server used is not
/// known (a failure that reached us only as a string, or a raw probe that
/// could not observe one). Names both fixes; Temps never switches modes on its
/// own. See [`remedy_for`] for the text matched to what was actually seen.
pub const COMPRESSION_REMEDY: &str = "Retrying cannot fix this. Either set the ClickHouse \
server's `network_compression_method` to `lz4` (for example in a `users.d` profile), or set \
Temps' ClickHouse compression to `none` (PUT /settings with \
{\"clickhouse\":{\"compression\":\"none\"}}) and restart Temps.";

const REMEDY_ALTERED_LZ4: &str = "The server answered with LZ4 as requested, so its codec is \
not the problem: the response was altered or cut short on the way (a proxy or load balancer \
in front of ClickHouse re-encoding bodies, or a dropped connection). If it persists, set \
Temps' ClickHouse compression to `none` (PUT /settings with \
{\"clickhouse\":{\"compression\":\"none\"}}) and restart Temps; the failure is re-checked periodically, so a transient one clears by itself.";

const REMEDY_NOT_A_BLOCK: &str = "The response was not a ClickHouse compressed block (no \
recognisable compression header). Check that the configured URL is ClickHouse's HTTP port and \
that nothing in front of it rewrites response bodies; setting Temps' ClickHouse compression to \
`none` (PUT /settings with {\"clickhouse\":{\"compression\":\"none\"}}) removes the block \
framing altogether. Restart Temps to apply it.";

const REMEDY_UNDECODABLE_UNCOMPRESSED: &str = "Temps asked for uncompressed responses and still \
received a body it could not decode, so something between Temps and ClickHouse is compressing or \
rewriting it. Check the configured URL and any proxy or load balancer in front of ClickHouse.";

/// The advice that matches a decode failure, from the configured `mode` and
/// the compression method actually seen on the wire.
///
/// In `lz4` mode every request already pins `network_compression_method=lz4`,
/// so "change the server default" is only the right advice when the server
/// answered with a *different* codec anyway; an LZ4 answer that still fails
/// points at the path between Temps and the server instead.
pub fn remedy_for(
    mode: ClickHouseCompression,
    observed: Option<CompressionMethod>,
) -> &'static str {
    match (mode, observed) {
        (ClickHouseCompression::None, _) => REMEDY_UNDECODABLE_UNCOMPRESSED,
        (ClickHouseCompression::Lz4, Some(CompressionMethod::Lz4)) => REMEDY_ALTERED_LZ4,
        // A well-formed block in some other codec (including NONE, `0x02`):
        // the server answered differently from what the request pinned.
        (ClickHouseCompression::Lz4, Some(CompressionMethod::Zstd))
        | (ClickHouseCompression::Lz4, Some(CompressionMethod::None))
        | (ClickHouseCompression::Lz4, Some(CompressionMethod::Other(_))) => COMPRESSION_REMEDY,
        // No block header at all: nothing ClickHouse-shaped came back.
        (ClickHouseCompression::Lz4, None) => REMEDY_NOT_A_BLOCK,
    }
}

/// The codec a ClickHouse compressed block says it was written with: the byte
/// right after the 16-byte checksum of every block on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMethod {
    /// `0x02` — the block is stored uncompressed.
    None,
    /// `0x82` — LZ4. The only codec the `clickhouse` client decodes.
    Lz4,
    /// `0x90` — ZSTD, the server default from ClickHouse 26.9.
    Zstd,
    /// Any other method byte (delta, T64, a multi-codec chain, …).
    Other(u8),
}

impl CompressionMethod {
    /// Interpret a compression method byte.
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0x02 => CompressionMethod::None,
            0x82 => CompressionMethod::Lz4,
            0x90 => CompressionMethod::Zstd,
            other => CompressionMethod::Other(other),
        }
    }

    /// The method byte of the first block of `body` (the 17th byte), if the
    /// body is long enough to hold one.
    pub fn from_body(body: &[u8]) -> Option<Self> {
        body.get(CHECKSUM_LEN).copied().map(Self::from_byte)
    }
}

/// Size of the checksum that precedes every compressed block.
const CHECKSUM_LEN: usize = 16;

impl std::fmt::Display for CompressionMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionMethod::None => f.write_str("none (0x02)"),
            CompressionMethod::Lz4 => f.write_str("lz4 (0x82)"),
            CompressionMethod::Zstd => f.write_str("zstd (0x90)"),
            CompressionMethod::Other(byte) => write!(f, "unrecognised (0x{byte:02x})"),
        }
    }
}

/// Why a ClickHouse call failed, coarsely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// The server answered with a body the client could not decode. Waiting
    /// does not help; configuration has to change.
    Decode,
    /// No usable transport: connection refused/reset, DNS, timeout.
    Unreachable,
    /// The server answered and rejected the request (bad credentials, unknown
    /// table, …).
    Server,
    /// Anything else.
    Other,
}

impl FailureKind {
    /// True when no amount of waiting will make the next attempt succeed.
    pub fn persists_until_config_changes(self) -> bool {
        matches!(self, FailureKind::Decode)
    }

    /// A stable lowercase name for logs and the settings API.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::Decode => "decode",
            FailureKind::Unreachable => "unreachable",
            FailureKind::Server => "server",
            FailureKind::Other => "other",
        }
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Classify a `clickhouse` client error.
pub fn classify_error(error: &ChError) -> FailureKind {
    match error {
        ChError::Decompression(_) => FailureKind::Decode,
        ChError::Network(_) | ChError::TimedOut => FailureKind::Unreachable,
        ChError::BadResponse(message) => classify_message(message),
        other => classify_message(&other.to_string()),
    }
}

/// Classify an error that has already been flattened to a string, which is
/// what most per-crate error types do with a ClickHouse failure.
pub fn classify_message(message: &str) -> FailureKind {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("decompression error") {
        FailureKind::Decode
    } else if lowered.contains("network error") || lowered.contains("timeout expired") {
        FailureKind::Unreachable
    } else if lowered.contains("code: ") || lowered.contains("bad response") {
        FailureKind::Server
    } else {
        FailureKind::Other
    }
}

/// `host[:port]` of a URL for log lines. The userinfo, path and query are
/// dropped: any of them may carry credentials.
pub fn host_for_logs(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => match (parsed.host_str(), parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_string(),
            (None, _) => "<no host>".to_string(),
        },
        Err(_) => "<invalid url>".to_string(),
    }
}

/// Run `SELECT version()` against the configured server. On a decode failure
/// the returned error names the mode, the host and the compression method
/// actually seen on the wire; it never silently falls back to the other mode.
pub async fn preflight(config: &ClickHouseConfig) -> Result<ServerVersion, ClickHouseError> {
    let client = config.client();
    match tokio::time::timeout(PROBE_TIMEOUT, server_version(&client)).await {
        Err(_) => Err(ClickHouseError::ProbeTimeout {
            host: config.host_for_logs(),
            database: config.database.clone(),
            timeout_secs: PROBE_TIMEOUT.as_secs(),
        }),
        Ok(Ok(version)) => Ok(version),
        Ok(Err(ClickHouseError::Query(source)))
            if classify_error(&source) == FailureKind::Decode =>
        {
            let observed = observe_compression_method(config).await;
            Err(ClickHouseError::ResponseDecode {
                host: config.host_for_logs(),
                database: config.database.clone(),
                mode: config.compression,
                observed,
                source,
            })
        }
        Ok(Err(other)) => Err(other),
    }
}

/// Issue one raw `SELECT 1` the way the client would (same compression
/// settings) and read the method byte off the response body, bypassing the
/// client's decoder. Best effort: any failure yields `None`.
async fn observe_compression_method(config: &ClickHouseConfig) -> Option<CompressionMethod> {
    // Without `compress=1` a server never frames its response in blocks, so
    // there is no method byte to observe.
    if config.compression != ClickHouseCompression::Lz4 {
        return None;
    }

    let mut url = match url::Url::parse(&config.url) {
        Ok(url) => url,
        Err(error) => {
            debug!(%error, "cannot observe compression method: invalid ClickHouse URL");
            return None;
        }
    };
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        pairs
            .append_pair("database", &config.database)
            .append_pair("compress", "1");
        // Mirror the setting the real client pins, so the byte seen here is
        // the byte the client would see.
        if let Some(method) =
            crate::ClientWireSettings::for_mode(config.compression).network_compression_method
        {
            pairs.append_pair(crate::NETWORK_COMPRESSION_METHOD_SETTING, method);
        }
    }

    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(http) => http,
        Err(error) => {
            debug!(%error, "cannot observe compression method: HTTP client build failed");
            return None;
        }
    };
    let response = match http
        .post(url)
        .basic_auth(&config.user, Some(&config.password))
        .body("SELECT 1")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            debug!(
                status = %response.status(),
                "cannot observe compression method: server rejected the raw probe"
            );
            return None;
        }
        Err(error) => {
            debug!(%error, "cannot observe compression method: raw probe failed");
            return None;
        }
    };
    let body = response.bytes().await.ok()?;
    CompressionMethod::from_body(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_bytes_map_to_their_codecs() {
        assert_eq!(CompressionMethod::from_byte(0x82), CompressionMethod::Lz4);
        assert_eq!(CompressionMethod::from_byte(0x90), CompressionMethod::Zstd);
        assert_eq!(CompressionMethod::from_byte(0x02), CompressionMethod::None);
        assert_eq!(
            CompressionMethod::from_byte(0x91),
            CompressionMethod::Other(0x91)
        );
    }

    #[test]
    fn method_byte_is_read_after_the_checksum() {
        let mut body = vec![0xAA; CHECKSUM_LEN];
        body.push(0x90);
        body.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            CompressionMethod::from_body(&body),
            Some(CompressionMethod::Zstd)
        );
        // Too short to hold a block header: a plain-text error body, say.
        assert_eq!(CompressionMethod::from_body(b"Code: 60"), None);
        assert_eq!(CompressionMethod::from_body(&[]), None);
    }

    #[test]
    fn display_names_the_codec_and_its_byte() {
        assert_eq!(CompressionMethod::Zstd.to_string(), "zstd (0x90)");
        assert_eq!(
            CompressionMethod::Other(0x91).to_string(),
            "unrecognised (0x91)"
        );
    }

    #[test]
    fn decompression_errors_are_decode_failures_that_waiting_cannot_fix() {
        let error = ChError::Decompression("incorrect magic number".into());
        let kind = classify_error(&error);
        assert_eq!(kind, FailureKind::Decode);
        assert!(kind.persists_until_config_changes());
    }

    #[test]
    fn transport_and_server_errors_are_not_decode_failures() {
        assert_eq!(classify_error(&ChError::TimedOut), FailureKind::Unreachable);
        assert_eq!(
            classify_error(&ChError::BadResponse(
                "Code: 60. DB::Exception: Unknown table".into()
            )),
            FailureKind::Server
        );
        assert!(!FailureKind::Unreachable.persists_until_config_changes());
        assert!(!FailureKind::Server.persists_until_config_changes());
    }

    #[test]
    fn flattened_error_strings_are_classified_too() {
        // What the per-crate error types keep of a decode failure.
        assert_eq!(
            classify_message(
                "failed to read migration tracking table: decompression error: incorrect magic number"
            ),
            FailureKind::Decode
        );
        assert_eq!(
            classify_message("ClickHouse query failed: network error: connection refused"),
            FailureKind::Unreachable
        );
        assert_eq!(
            classify_message("migration 0001_events failed: bad response: Code: 60"),
            FailureKind::Server
        );
        assert_eq!(classify_message("something else"), FailureKind::Other);
    }

    #[test]
    fn host_for_logs_drops_credentials_path_and_query() {
        assert_eq!(
            host_for_logs("http://user:hunter2@clickhouse.internal:8123/db?password=x"),
            "clickhouse.internal:8123"
        );
        assert_eq!(
            host_for_logs("https://clickhouse.internal"),
            "clickhouse.internal"
        );
        assert_eq!(host_for_logs("not a url"), "<invalid url>");
    }

    #[test]
    fn remedy_matches_what_was_seen_on_the_wire() {
        use ClickHouseCompression::{Lz4, None as Uncompressed};

        // A different codec despite the pin: the server default is the cause.
        // That includes a well-formed NONE (`0x02`) block.
        for observed in [
            CompressionMethod::Zstd,
            CompressionMethod::None,
            CompressionMethod::Other(0x91),
        ] {
            let remedy = remedy_for(Lz4, Some(observed));
            assert!(remedy.contains("network_compression_method"), "{remedy}");
        }
        // LZ4 as asked, yet undecodable: the server default cannot be the cause.
        let altered = remedy_for(Lz4, Some(CompressionMethod::Lz4));
        assert!(
            !altered.contains("`network_compression_method` to"),
            "{altered}"
        );
        assert!(altered.contains("altered or cut short"), "{altered}");
        // Not a block at all: no header to read a method byte from.
        let remedy = remedy_for(Lz4, None);
        assert!(
            remedy.contains("not a ClickHouse compressed block"),
            "{remedy}"
        );
        // Uncompressed mode never involves a codec.
        let uncompressed = remedy_for(Uncompressed, None);
        assert!(
            uncompressed.contains("proxy or load balancer"),
            "{uncompressed}"
        );
    }

    #[test]
    fn decode_error_message_names_both_fixes_and_the_observed_byte() {
        let error = ClickHouseError::ResponseDecode {
            host: "clickhouse.internal:8123".into(),
            database: "temps".into(),
            mode: ClickHouseCompression::Lz4,
            observed: Some(CompressionMethod::Zstd),
            source: ChError::Decompression("incorrect magic number".into()),
        };
        let message = error.to_string();
        assert!(message.contains("clickhouse.internal:8123"), "{message}");
        assert!(message.contains("database 'temps'"), "{message}");
        assert!(message.contains("'lz4' compression mode"), "{message}");
        assert!(message.contains("zstd (0x90)"), "{message}");
        assert!(message.contains("network_compression_method"), "{message}");
        assert!(message.contains("`none`"), "{message}");
    }
}
