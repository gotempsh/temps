// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared S3-compatible client construction.
//!
//! One small helper reused by both new S3 backends (`S3FileStore` here, and
//! `temps_deployer::static_deployer::S3StaticDeployer`) so their client setup
//! — credentials, region, path-style, and timeouts — can never drift apart.
//! Style follows the existing `temps-log-aggregator` and
//! `temps-cloud::backup_mirror` S3 client constructors: `aws_sdk_s3::Client`
//! built directly from a `Config`, no `aws-config` credential-chain
//! resolution (credentials are always operator-supplied, never picked up
//! from the environment/IMDS).

use crate::s3_config::S3StorageConfig;
use aws_sdk_s3::config::{retry::RetryConfig, timeout::TimeoutConfig, Credentials, Region};
use aws_sdk_s3::{Client as S3Client, Config};

/// Build an S3-compatible client from a resolved [`S3StorageConfig`].
///
/// Sets an explicit connect timeout and a bounded operation timeout (headers
/// phase only — see `DEFAULT_S3_TIMEOUT_SECS` doc comment) so a stalled or
/// unreachable endpoint fails fast with a typed error instead of hanging a
/// caller on the request hot path. Streaming reads of a GetObject body are
/// bounded separately by `s3_store::IdleTimeoutReader`, since a valid large
/// object must not be killed by `operation_timeout` while its body is still
/// being read.
pub fn build_s3_client(config: &S3StorageConfig) -> S3Client {
    let credentials = Credentials::new(
        config.access_key_id.clone(),
        config.secret_access_key.clone(),
        None,
        None,
        "temps-static-storage",
    );

    let mut builder = Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(Region::new(config.region.clone()))
        .credentials_provider(credentials)
        .force_path_style(config.force_path_style)
        .retry_config(RetryConfig::standard().with_max_attempts(3))
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(config.timeout)
                .operation_timeout(config.timeout)
                .build(),
        );

    if let Some(endpoint) = &config.endpoint {
        builder = builder.endpoint_url(endpoint.clone());
    }

    S3Client::from_conf(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample_config() -> S3StorageConfig {
        S3StorageConfig {
            bucket: "temps-static".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://127.0.0.1:9000".to_string()),
            access_key_id: "key".to_string(),
            secret_access_key: "secret".to_string(),
            force_path_style: true,
            timeout: Duration::from_secs(10),
            prefix: None,
        }
    }

    #[test]
    fn builds_a_client_without_panicking_for_a_minio_style_endpoint() {
        // Construction alone must never make a network call — this only
        // verifies the config builder doesn't panic on a well-formed input.
        let _client = build_s3_client(&sample_config());
    }

    #[test]
    fn builds_a_client_without_an_endpoint_override() {
        let mut config = sample_config();
        config.endpoint = None;
        let _client = build_s3_client(&config);
    }
}
