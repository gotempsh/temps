// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Latest published release of a coding-agent CLI, read from the npm
//! registry.
//!
//! The registry host is a constant and the package names are constants from
//! `temps_ai_agent_cli::harness_update`: nothing from a request reaches the
//! URL. The answer is bounded in time and size, redirects are never followed
//! (the hardened client), and only a well-formed version of the expected
//! package is accepted.

use std::time::Duration;

use serde::Deserialize;
use url::Url;

use super::error::CodingAgentError;
use crate::providers::ProviderHttpClient;

/// Public npm registry. Its `/<package>/latest` document is a few KiB.
pub(crate) const NPM_REGISTRY: &str = "https://registry.npmjs.org/";
pub(crate) const MAX_RELEASE_BYTES: usize = 1024 * 1024;
pub(crate) const REGISTRY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct LatestDocument {
    name: String,
    version: String,
}

/// `<registry>/<package>/latest`, with a scope's `/` encoded as the registry
/// documents it (`@scope%2fname`).
pub(crate) fn latest_url(registry: &Url, package: &str) -> Result<Url, CodingAgentError> {
    let path = format!("{}/latest", package.replace('/', "%2f"));
    registry
        .join(&path)
        .map_err(|error| CodingAgentError::Registry {
            package: package.to_string(),
            reason: format!("invalid registry URL: {error}"),
        })
}

/// The version in a `/latest` document, if it names `package` and carries a
/// `major.minor.patch` version.
pub(crate) fn parse_latest(package: &str, body: &[u8]) -> Result<String, CodingAgentError> {
    let parse_error = |reason: String| CodingAgentError::RegistryParse {
        package: package.to_string(),
        reason,
    };
    let document: LatestDocument =
        serde_json::from_slice(body).map_err(|error| parse_error(error.to_string()))?;
    if document.name != package {
        return Err(parse_error(format!(
            "the document describes '{}'",
            document.name
        )));
    }
    let version = temps_ai_agent_cli::harness_update::parse_version(&document.version)
        .filter(|version| version == &document.version && version.len() <= 64)
        .ok_or_else(|| parse_error(format!("'{}' is not a release version", document.version)))?;
    Ok(version)
}

/// Fetch the latest release of `package`.
pub(crate) async fn fetch_latest(
    client: &ProviderHttpClient,
    registry: &Url,
    package: &str,
) -> Result<String, CodingAgentError> {
    let url = latest_url(registry, package)?;
    let request = async {
        let mut response = client
            .get(url.as_str())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "temps-coding-agents")
            .send()
            .await
            .map_err(|error| CodingAgentError::Registry {
                package: package.to_string(),
                reason: error.without_url().to_string(),
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(CodingAgentError::RegistryStatus {
                package: package.to_string(),
                status: status.as_u16(),
            });
        }
        let too_large = || CodingAgentError::RegistryTooLarge {
            package: package.to_string(),
            limit_bytes: MAX_RELEASE_BYTES,
        };
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RELEASE_BYTES as u64)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) =
            response
                .chunk()
                .await
                .map_err(|error| CodingAgentError::Registry {
                    package: package.to_string(),
                    reason: error.without_url().to_string(),
                })?
        {
            if body.len() + chunk.len() > MAX_RELEASE_BYTES {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        parse_latest(package, &body)
    };
    tokio::time::timeout(REGISTRY_TIMEOUT, request)
        .await
        .map_err(|_| CodingAgentError::RegistryTimeout {
            package: package.to_string(),
            seconds: REGISTRY_TIMEOUT.as_secs(),
        })?
}
