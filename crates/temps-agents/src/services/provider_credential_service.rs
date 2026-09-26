// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Discovers credentials already used by AI harnesses on the Temps host.
//!
//! Discovery is deliberately limited to provider-owned environment variables
//! and fixed files below the server process user's home/config directories.
//! Callers receive credential bytes only in-process; HTTP responses must expose
//! [`LocalCredentialSummary`] rather than [`DiscoveredLocalCredential`].

use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::ai_cli::catalog::{CredentialFormat, ProviderCatalogEntry};

const MAX_LOCAL_CREDENTIAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCredentialSourceKind {
    Environment,
    HostAuthStore,
}

impl LocalCredentialSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::HostAuthStore => "host_auth_store",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "Temps server environment",
            Self::HostAuthStore => "Authenticated host CLI",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCredentialSummary {
    pub auth_type: String,
    pub source: LocalCredentialSourceKind,
}

pub struct DiscoveredLocalCredential {
    pub auth_type: String,
    pub source: LocalCredentialSourceKind,
    pub credential: String,
}

impl DiscoveredLocalCredential {
    pub fn summary(&self) -> LocalCredentialSummary {
        LocalCredentialSummary {
            auth_type: self.auth_type.clone(),
            source: self.source,
        }
    }
}

#[derive(Debug, Error)]
pub enum LocalCredentialError {
    #[error("Claude Code local-login import is disabled. Save a token from `claude setup-token` or an Anthropic API key manually.")]
    ClaudeLocalImportDisabled,
    #[error("provider '{provider_id}' is not supported for local credential discovery")]
    UnsupportedProvider { provider_id: String },
    #[error("could not inspect the local {provider_id} credential store: {reason}")]
    InspectFailed { provider_id: String, reason: String },
    #[error("the local {provider_id} credential store is larger than {max_bytes} bytes")]
    CredentialTooLarge { provider_id: String, max_bytes: u64 },
    #[error("the local {provider_id} credential store is not valid JSON: {reason}")]
    InvalidCredentialFile { provider_id: String, reason: String },
    #[error("the local {provider_id} credential store does not contain a usable credential")]
    CredentialMissing { provider_id: String },
}

/// Find an importable local credential without exposing where it lives.
///
/// Provider order is intentional: explicit process environment wins over a
/// host auth store because it is how the operator configured the Temps
/// service itself. Provider-specific ordering then follows the least
/// surprising locally configured credential for that CLI.
pub async fn discover_local_credential(
    provider: &ProviderCatalogEntry,
) -> Result<Option<DiscoveredLocalCredential>, LocalCredentialError> {
    match provider.id {
        "claude_cli" => Err(LocalCredentialError::ClaudeLocalImportDisabled),
        "codex_cli" => discover_codex_credential(provider).await,
        "opencode" => discover_opencode_credential(provider).await,
        provider_id => Err(LocalCredentialError::UnsupportedProvider {
            provider_id: provider_id.to_string(),
        }),
    }
}

/// Report whether an importable credential exists without returning the
/// credential to the caller. Claude is intentionally excluded: its host login
/// must not be copied into workspace credentials.
pub async fn discover_local_credential_summary(
    provider: &ProviderCatalogEntry,
) -> Result<Option<LocalCredentialSummary>, LocalCredentialError> {
    match provider.id {
        "claude_cli" => Ok(None),
        "codex_cli" => {
            if credential_from_flavor_environment(provider, "api_key").is_some() {
                return Ok(Some(LocalCredentialSummary {
                    auth_type: "api_key".to_string(),
                    source: LocalCredentialSourceKind::Environment,
                }));
            }
            let Some(path) = codex_credential_path() else {
                return Ok(None);
            };
            credential_file_exists(provider.id, &path)
                .await
                .map(|exists| {
                    exists.then(|| LocalCredentialSummary {
                        auth_type: "subscription".to_string(),
                        source: LocalCredentialSourceKind::HostAuthStore,
                    })
                })
        }
        "opencode" => {
            let Some(path) = opencode_credential_path() else {
                return Ok(None);
            };
            credential_file_exists(provider.id, &path)
                .await
                .map(|exists| {
                    exists.then(|| LocalCredentialSummary {
                        auth_type: "config_file".to_string(),
                        source: LocalCredentialSourceKind::HostAuthStore,
                    })
                })
        }
        provider_id => Err(LocalCredentialError::UnsupportedProvider {
            provider_id: provider_id.to_string(),
        }),
    }
}

async fn discover_codex_credential(
    provider: &ProviderCatalogEntry,
) -> Result<Option<DiscoveredLocalCredential>, LocalCredentialError> {
    if let Some(credential) = credential_from_flavor_environment(provider, "api_key") {
        return Ok(Some(credential));
    }
    let Some(path) = codex_credential_path() else {
        return Ok(None);
    };
    discover_config_file(provider, "subscription", &path).await
}

async fn discover_opencode_credential(
    provider: &ProviderCatalogEntry,
) -> Result<Option<DiscoveredLocalCredential>, LocalCredentialError> {
    let Some(path) = opencode_credential_path() else {
        return Ok(None);
    };
    discover_config_file(provider, "config_file", &path).await
}

fn credential_from_flavor_environment(
    provider: &ProviderCatalogEntry,
    auth_type: &str,
) -> Option<DiscoveredLocalCredential> {
    let flavor = provider.flavor(auth_type)?;
    if !matches!(flavor.format, CredentialFormat::ApiKey) {
        return None;
    }
    non_empty_env(flavor.env_var).map(|credential| DiscoveredLocalCredential {
        auth_type: auth_type.to_string(),
        source: LocalCredentialSourceKind::Environment,
        credential,
    })
}

async fn discover_config_file(
    provider: &ProviderCatalogEntry,
    auth_type: &str,
    path: &Path,
) -> Result<Option<DiscoveredLocalCredential>, LocalCredentialError> {
    let Some(body) = read_optional_credential_file(provider.id, path).await? else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(&body).map_err(|error| {
        LocalCredentialError::InvalidCredentialFile {
            provider_id: provider.id.to_string(),
            reason: error.to_string(),
        }
    })?;
    if !parsed.is_object() {
        return Err(LocalCredentialError::InvalidCredentialFile {
            provider_id: provider.id.to_string(),
            reason: "expected a JSON object".to_string(),
        });
    }
    Ok(Some(DiscoveredLocalCredential {
        auth_type: auth_type.to_string(),
        source: LocalCredentialSourceKind::HostAuthStore,
        credential: body,
    }))
}

async fn read_optional_credential_file(
    provider_id: &str,
    path: &Path,
) -> Result<Option<String>, LocalCredentialError> {
    let Some((file, _metadata)) = open_optional_credential_file(provider_id, path).await? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.take(MAX_LOCAL_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| LocalCredentialError::InspectFailed {
            provider_id: provider_id.to_string(),
            reason: error.to_string(),
        })?;
    if bytes.len() as u64 > MAX_LOCAL_CREDENTIAL_BYTES {
        return Err(LocalCredentialError::CredentialTooLarge {
            provider_id: provider_id.to_string(),
            max_bytes: MAX_LOCAL_CREDENTIAL_BYTES,
        });
    }
    let body =
        String::from_utf8(bytes).map_err(|error| LocalCredentialError::InvalidCredentialFile {
            provider_id: provider_id.to_string(),
            reason: format!("credential data is not UTF-8: {error}"),
        })?;
    if body.trim().is_empty() {
        return Err(LocalCredentialError::CredentialMissing {
            provider_id: provider_id.to_string(),
        });
    }
    Ok(Some(body))
}

async fn open_optional_credential_file(
    provider_id: &str,
    path: &Path,
) -> Result<Option<(tokio::fs::File, std::fs::Metadata)>, LocalCredentialError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);

    let file = match options.open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(LocalCredentialError::InspectFailed {
                provider_id: provider_id.to_string(),
                reason: error.to_string(),
            })
        }
    };
    let metadata = file
        .metadata()
        .await
        .map_err(|error| LocalCredentialError::InspectFailed {
            provider_id: provider_id.to_string(),
            reason: error.to_string(),
        })?;
    if !metadata.is_file() {
        return Err(LocalCredentialError::InspectFailed {
            provider_id: provider_id.to_string(),
            reason: "the configured credential path is not a regular file".to_string(),
        });
    }
    if metadata.len() > MAX_LOCAL_CREDENTIAL_BYTES {
        return Err(LocalCredentialError::CredentialTooLarge {
            provider_id: provider_id.to_string(),
            max_bytes: MAX_LOCAL_CREDENTIAL_BYTES,
        });
    }
    Ok(Some((file, metadata)))
}

async fn credential_file_exists(
    provider_id: &str,
    path: &Path,
) -> Result<bool, LocalCredentialError> {
    Ok(open_optional_credential_file(provider_id, path)
        .await?
        .is_some_and(|(_, metadata)| metadata.len() > 0))
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn codex_credential_path() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".codex")))
        .map(|directory| directory.join("auth.json"))
}

fn opencode_credential_path() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".local/share")))
        .map(|directory| directory.join("opencode/auth.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str) -> &'static ProviderCatalogEntry {
        crate::ai_cli::catalog::find_provider(id).expect("provider in catalog")
    }

    #[tokio::test]
    async fn config_file_discovery_rejects_non_json_without_leaking_body() {
        let directory = tempfile::tempdir().expect("temporary credential directory");
        let path = directory.path().join("auth.json");
        tokio::fs::write(&path, "not-json-secret")
            .await
            .expect("write credential fixture");

        let error = discover_config_file(provider("codex_cli"), "subscription", &path)
            .await
            .err()
            .expect("invalid credential file");
        let message = error.to_string();
        assert!(message.contains("not valid JSON"));
        assert!(!message.contains("not-json-secret"));
    }

    #[tokio::test]
    async fn claude_local_import_is_disabled_and_not_advertised() {
        assert!(discover_local_credential_summary(provider("claude_cli"))
            .await
            .expect("summary")
            .is_none());
        let error = discover_local_credential(provider("claude_cli"))
            .await
            .err()
            .expect("Claude import must be rejected");
        assert!(matches!(
            error,
            LocalCredentialError::ClaudeLocalImportDisabled
        ));
        assert!(error.to_string().contains("claude setup-token"));
    }

    #[tokio::test]
    async fn config_file_discovery_returns_secret_only_in_process() {
        let directory = tempfile::tempdir().expect("temporary credential directory");
        let path = directory.path().join("auth.json");
        tokio::fs::write(&path, r#"{"tokens":{"access_token":"test-secret"}}"#)
            .await
            .expect("write credential fixture");

        let discovered = discover_config_file(provider("codex_cli"), "subscription", &path)
            .await
            .expect("valid credential store")
            .expect("credential discovered");
        assert_eq!(discovered.auth_type, "subscription");
        assert_eq!(discovered.source, LocalCredentialSourceKind::HostAuthStore);
        assert!(discovered.credential.contains("test-secret"));
        assert_eq!(
            discovered.summary().source.label(),
            "Authenticated host CLI"
        );
    }

    #[tokio::test]
    async fn missing_config_file_is_not_an_error() {
        let directory = tempfile::tempdir().expect("temporary credential directory");
        let missing = directory.path().join("missing.json");
        assert!(
            discover_config_file(provider("opencode"), "config_file", &missing)
                .await
                .expect("missing files are ordinary discovery misses")
                .is_none()
        );
    }

    #[tokio::test]
    async fn oversized_config_file_is_rejected_before_reading() {
        let directory = tempfile::tempdir().expect("temporary credential directory");
        let path = directory.path().join("auth.json");
        let file = std::fs::File::create(&path).expect("create credential fixture");
        file.set_len(MAX_LOCAL_CREDENTIAL_BYTES + 1)
            .expect("grow credential fixture");

        assert!(matches!(
            discover_config_file(provider("opencode"), "config_file", &path).await,
            Err(LocalCredentialError::CredentialTooLarge { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn credential_discovery_rejects_symbolic_links() {
        let directory = tempfile::tempdir().expect("temporary credential directory");
        let target = directory.path().join("real-auth.json");
        let link = directory.path().join("auth.json");
        tokio::fs::write(&target, r#"{"token":"secret"}"#)
            .await
            .expect("write credential fixture");
        std::os::unix::fs::symlink(&target, &link).expect("create credential symlink");

        assert!(matches!(
            discover_config_file(provider("opencode"), "config_file", &link).await,
            Err(LocalCredentialError::InspectFailed { .. })
        ));
    }

    #[tokio::test]
    async fn summary_probe_checks_file_metadata_without_parsing_secrets() {
        let directory = tempfile::tempdir().expect("temporary credential directory");
        let path = directory.path().join("auth.json");
        tokio::fs::write(&path, "not-json-secret")
            .await
            .expect("write credential fixture");

        assert!(credential_file_exists("codex_cli", &path)
            .await
            .expect("metadata probe"));
        assert!(matches!(
            discover_config_file(provider("codex_cli"), "subscription", &path).await,
            Err(LocalCredentialError::InvalidCredentialFile { .. })
        ));
    }
}
