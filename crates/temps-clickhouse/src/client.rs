// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The one place a `clickhouse::Client` for the instance's ClickHouse server
//! is built.
//!
//! # Why the mode is explicit
//!
//! The `clickhouse` crate (default feature `lz4`) sends `compress=1` on every
//! request and decodes exactly one codec: LZ4, recognised by method byte
//! `0x82` right after the 16-byte checksum. It never says *which* codec it
//! wants, so it quietly depends on the server's `network_compression_method`
//! default being LZ4. ClickHouse 26.9 changed that default to ZSTD (`0x90`),
//! after which every response that carries rows fails with
//! `decompression error: incorrect magic number`. Error responses are plain
//! text and still read fine, which is why the symptom looks like a missing
//! table rather than a transport problem.
//!
//! So the choice is made here, once, from the stored setting:
//!
//! | mode   | client                     | query setting                     |
//! |--------|----------------------------|-----------------------------------|
//! | `lz4`  | `Compression::Lz4`         | `network_compression_method=lz4`  |
//! | `none` | `Compression::None`        | — (no `compress=1` is sent)       |

use clickhouse::{Client, Compression};
use temps_core::ClickHouseCompression;

/// The ClickHouse setting that picks the codec for compressed responses.
pub const NETWORK_COMPRESSION_METHOD_SETTING: &str = "network_compression_method";

/// What a [`ClickHouseCompression`] mode puts on every request, as plain
/// data. [`build_client`] applies exactly this; the type exists so the
/// mapping can be tested and logged without a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientWireSettings {
    /// Whether the request carries `compress=1`, asking for block-compressed
    /// responses.
    pub compress: bool,
    /// Value of the `network_compression_method` query setting pinned on the
    /// request, or `None` when the server default is not consulted at all.
    pub network_compression_method: Option<&'static str>,
}

impl ClientWireSettings {
    /// The wire settings for `mode`.
    pub fn for_mode(mode: ClickHouseCompression) -> Self {
        match mode {
            ClickHouseCompression::Lz4 => Self {
                compress: true,
                network_compression_method: Some("lz4"),
            },
            ClickHouseCompression::None => Self {
                compress: false,
                network_compression_method: None,
            },
        }
    }
}

/// Apply `mode` to a client. Prefer [`build_client`]; this exists for the few
/// call sites that must start from a client they already configured.
pub fn apply_compression(client: Client, mode: ClickHouseCompression) -> Client {
    let wire = ClientWireSettings::for_mode(mode);
    let client = client.with_compression(if wire.compress {
        Compression::Lz4
    } else {
        Compression::None
    });
    match wire.network_compression_method {
        Some(method) => client.with_setting(NETWORK_COMPRESSION_METHOD_SETTING, method),
        None => client,
    }
}

/// Build a client for a ClickHouse server.
///
/// Construction does no I/O. The compression mode is applied here and nowhere
/// else; a `clickhouse::Client::default()` anywhere else in the workspace is
/// the bug this function exists to prevent.
pub fn build_client(
    url: &str,
    database: &str,
    user: &str,
    password: &str,
    compression: ClickHouseCompression,
) -> Client {
    apply_compression(
        Client::default()
            .with_url(url)
            .with_database(database)
            .with_user(user)
            .with_password(password),
        compression,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client's `Debug` output is the only public view of what it will
    /// send: its compression and its per-query settings.
    fn debug_of(compression: ClickHouseCompression) -> String {
        format!(
            "{:?}",
            build_client(
                "http://clickhouse.internal:8123",
                "db",
                "u",
                "p",
                compression
            )
        )
    }

    #[test]
    fn lz4_mode_requests_compressed_responses_and_pins_the_codec() {
        let wire = ClientWireSettings::for_mode(ClickHouseCompression::Lz4);
        assert!(wire.compress);
        assert_eq!(wire.network_compression_method, Some("lz4"));

        let debug = debug_of(ClickHouseCompression::Lz4);
        assert!(debug.contains("compression: Lz4"), "{debug}");
        assert!(
            debug.contains("\"network_compression_method\": \"lz4\""),
            "{debug}"
        );
    }

    #[test]
    fn none_mode_sends_neither_compress_nor_a_codec_setting() {
        let wire = ClientWireSettings::for_mode(ClickHouseCompression::None);
        assert!(!wire.compress);
        assert_eq!(wire.network_compression_method, None);

        let debug = debug_of(ClickHouseCompression::None);
        assert!(debug.contains("compression: None"), "{debug}");
        assert!(!debug.contains("network_compression_method"), "{debug}");
    }

    #[test]
    fn default_mode_is_lz4() {
        assert_eq!(ClickHouseCompression::default(), ClickHouseCompression::Lz4);
        assert_eq!(
            debug_of(ClickHouseCompression::default()),
            debug_of(ClickHouseCompression::Lz4)
        );
    }

    #[test]
    fn config_client_honours_the_configured_mode() {
        let cfg = crate::ClickHouseConfig::new("http://clickhouse.internal:8123", "db", "u", "p");
        let default_debug = format!("{:?}", cfg.client());
        assert!(
            default_debug.contains("compression: Lz4"),
            "{default_debug}"
        );

        let none_debug = format!(
            "{:?}",
            cfg.with_compression(ClickHouseCompression::None).client()
        );
        assert!(none_debug.contains("compression: None"), "{none_debug}");
    }

    #[test]
    fn client_debug_does_not_leak_credentials() {
        let debug = format!(
            "{:?}",
            build_client(
                "http://u:hunter2@clickhouse.internal:8123",
                "db",
                "u",
                "hunter2",
                ClickHouseCompression::Lz4
            )
        );
        assert!(!debug.contains("hunter2"), "{debug}");
    }

    // ── Real server whose default codec is ZSTD ──────────────────────────
    //
    // ClickHouse 26.9 made ZSTD the default `network_compression_method`, so a
    // stock 26.9 server reproduces the field failure with no configuration.
    // Docker is detected at runtime and the test skips when it is unavailable
    // (never `#[ignore]`).

    use crate::diagnostics::CompressionMethod;
    use crate::{apply_migrations, preflight, ClickHouseConfig, Migration};

    const IMAGE: &str = "clickhouse/clickhouse-server";
    /// First release whose default `network_compression_method` is ZSTD.
    const IMAGE_TAG: &str = "26.9";

    /// Start a stock ClickHouse 26.9 server (default codec ZSTD). `None` (with a
    /// message on stderr) when Docker or the image is unavailable.
    ///
    /// Data directories are tmpfs: the test needs no persistence and a
    /// developer's Docker disk should not pay for it.
    async fn start_zstd_default_server() -> Option<(
        testcontainers::ContainerAsync<testcontainers::GenericImage>,
        ClickHouseConfig,
    )> {
        use testcontainers::{
            core::{wait::HttpWaitStrategy, ContainerPort, Mount, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };

        let image = GenericImage::new(IMAGE, IMAGE_TAG)
            .with_exposed_port(ContainerPort::Tcp(8123))
            // The image never logs readiness to stdout; wait on /ping.
            .with_wait_for(WaitFor::http(
                HttpWaitStrategy::new("/ping")
                    .with_port(ContainerPort::Tcp(8123))
                    .with_expected_status_code(200u16),
            ))
            .with_env_var("CLICKHOUSE_DB", "temps_test")
            // Not CLICKHOUSE_USER=default, and a non-empty password: either
            // makes the image's user-init reject the built-in `default` user.
            .with_env_var("CLICKHOUSE_PASSWORD", "test")
            .with_mount(Mount::tmpfs_mount("/var/lib/clickhouse"))
            .with_mount(Mount::tmpfs_mount("/var/log/clickhouse-server"));

        let container = match image.start().await {
            Ok(container) => container,
            Err(error) => {
                eprintln!("Skipping ClickHouse compression test: cannot start container ({error})");
                return None;
            }
        };
        let port = match container.get_host_port_ipv4(8123).await {
            Ok(port) => port,
            Err(error) => {
                eprintln!("Skipping ClickHouse compression test: cannot get host port ({error})");
                return None;
            }
        };

        // The host the daemon publishes ports on: 127.0.0.1 normally, the
        // bridge gateway when the tests themselves run inside a container.
        let host = match container.get_host().await {
            Ok(host) => host.to_string(),
            Err(error) => {
                eprintln!("Skipping ClickHouse compression test: cannot get host ({error})");
                return None;
            }
        };
        let config = ClickHouseConfig::new(
            format!("http://{host}:{port}"),
            "temps_test",
            "default",
            "test",
        );
        // /ping answers before the HTTP query path settles; use the `none`
        // client, which has no codec to disagree about, to wait for it.
        let waiter = config
            .clone()
            .with_compression(ClickHouseCompression::None)
            .client();
        let mut last_error = String::new();
        for _ in 0..50 {
            match waiter.query("SELECT 1").execute().await {
                Ok(()) => {
                    last_error.clear();
                    break;
                }
                Err(error) => {
                    last_error = error.to_string();
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        if !last_error.is_empty() {
            eprintln!("Skipping ClickHouse compression test: never became ready ({last_error})");
            return None;
        }
        Some((container, config))
    }

    const MIGRATIONS: &[Migration] = &[
        Migration {
            name: "0001_things",
            sql: "CREATE TABLE IF NOT EXISTS things (id UInt64, label String) \
                  ENGINE = MergeTree ORDER BY id",
        },
        Migration {
            name: "0002_things_rollup",
            sql: "CREATE TABLE IF NOT EXISTS things_rollup (label String, n UInt64) \
                  ENGINE = SummingMergeTree ORDER BY label",
        },
    ];

    #[derive(clickhouse::Row, serde::Deserialize)]
    struct NumberRow {
        number: u64,
    }

    #[tokio::test]
    async fn both_modes_decode_against_a_26_9_server_whose_default_codec_is_zstd() {
        let Some((_container, base)) = start_zstd_default_server().await else {
            return;
        };

        // Premise: this server really does reproduce the bug. A client that
        // leaves the codec to the server default (what every client in the
        // workspace used to do) cannot decode a row-returning query.
        let unpinned = clickhouse::Client::default()
            .with_url(&base.url)
            .with_database(&base.database)
            .with_user(&base.user)
            .with_password(&base.password);
        let broken = unpinned
            .query("SELECT number FROM system.numbers LIMIT 3")
            .fetch_all::<NumberRow>()
            .await;
        match broken {
            Err(clickhouse::error::Error::Decompression(reason)) => {
                assert!(reason.to_string().contains("magic number"), "{reason}")
            }
            other => panic!(
                "premise failed: an unpinned client should hit a decompression error against a \
                 ZSTD-default server, got {:?}",
                other.map(|rows| rows.len())
            ),
        }

        // The server really does answer ZSTD when asked the way the old
        // client did, and LZ4 when the codec is pinned the way we now do.
        assert_eq!(
            raw_method(&base, false).await,
            Some(CompressionMethod::Zstd)
        );
        assert_eq!(raw_method(&base, true).await, Some(CompressionMethod::Lz4));

        for mode in ClickHouseCompression::ALL {
            let config = base.clone().with_compression(mode);
            let client = config.client();

            // (a)/(b): a row-returning query decodes.
            let rows = client
                .query("SELECT number FROM system.numbers LIMIT 5")
                .fetch_all::<NumberRow>()
                .await
                .unwrap_or_else(|e| panic!("{mode} mode failed to decode rows: {e}"));
            assert_eq!(
                rows.iter().map(|r| r.number).collect::<Vec<_>>(),
                vec![0, 1, 2, 3, 4],
                "{mode} mode"
            );

            // The startup probe agrees.
            let version = preflight(&config)
                .await
                .unwrap_or_else(|e| panic!("{mode} mode preflight failed: {e}"));
            assert!(version.major > 0, "{mode} mode");

            // (c): migrations apply, in their own database per mode, and a
            // second pass is a no-op.
            let database = format!("modes_{mode}");
            let migrating = ClickHouseConfig {
                database: database.clone(),
                ..config
            };
            let migrating_client = migrating.client();
            let tracking = "_temps_ch_test_migrations";
            let first = apply_migrations(&migrating_client, &database, tracking, MIGRATIONS)
                .await
                .unwrap_or_else(|e| panic!("{mode} mode migrations failed: {e}"));
            assert_eq!(
                first.applied,
                vec!["0001_things".to_string(), "0002_things_rollup".to_string()],
                "{mode} mode"
            );
            let second = apply_migrations(&migrating_client, &database, tracking, MIGRATIONS)
                .await
                .unwrap_or_else(|e| panic!("{mode} mode second migration pass failed: {e}"));
            assert!(second.applied.is_empty(), "{mode} mode");
            assert_eq!(second.skipped, 2, "{mode} mode");
        }
    }

    /// The method byte of a raw `compress=1` response, with or without the
    /// codec pinned to LZ4.
    async fn raw_method(config: &ClickHouseConfig, pin_lz4: bool) -> Option<CompressionMethod> {
        let mut query = vec![("database", "temps_test"), ("compress", "1")];
        if pin_lz4 {
            query.push((NETWORK_COMPRESSION_METHOD_SETTING, "lz4"));
        }
        let body = reqwest::Client::new()
            .post(&config.url)
            .query(&query)
            .basic_auth(&config.user, Some(&config.password))
            .body("SELECT 1")
            .send()
            .await
            .ok()?
            .bytes()
            .await
            .ok()?;
        CompressionMethod::from_body(&body)
    }
}
