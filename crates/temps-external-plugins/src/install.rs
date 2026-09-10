// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded direct-binary installation and authenticated activation records.

use std::io::{Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::catalog::{
    validate_url, PlatformRelease, RegistryConfig, RegistryEnvelope, RegistryPlugin,
    VerifiedRegistry,
};
use crate::trust::{KeysetEnvelope, KeysetUse, VerifiedKeyset};

pub const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const BINARY_TIMEOUT: Duration = Duration::from_secs(120);
const ACTIVE_FILE: &str = "active.json";
const RECEIPT_FILE: &str = "receipt.json";
const BINARY_FILE: &str = "plugin";
const REGISTRY_STATE_FILE: &str = "registry-state.json";

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("Plugin registry name '{name}' is unsafe; expected 1-64 lowercase ASCII letters, digits, or internal hyphens")]
    UnsafePluginName { name: String },
    #[error("Plugin '{plugin}' has unsafe release version '{version}'")]
    UnsafeVersion { plugin: String, version: String },
    #[error("Unsupported platform: {os} {arch} ({target_env})")]
    UnsupportedPlatform {
        os: String,
        arch: String,
        target_env: String,
    },
    #[error("Plugin '{plugin}' v{version} has no binary for platform '{platform}'")]
    NoRelease {
        plugin: String,
        version: String,
        platform: String,
    },
    #[error("Plugin '{plugin}' v{version} has invalid SHA-256 '{digest}': expected exactly 64 hexadecimal characters")]
    InvalidDigest {
        plugin: String,
        version: String,
        digest: String,
    },
    #[error("Refusing unsafe binary URL for plugin '{plugin}': {url}")]
    UnsafeArtifactUrl { plugin: String, url: String },
    #[error("Failed to create plugin download client for {url}: {reason}")]
    Client { url: String, reason: String },
    #[error("Failed to download plugin '{plugin}' from {url}: {reason}")]
    Download {
        plugin: String,
        url: String,
        reason: String,
    },
    #[error("Plugin binary download from {url} returned HTTP {status}")]
    DownloadStatus { url: String, status: u16 },
    #[error("Plugin binary download from {url} exceeded the {limit}-byte limit")]
    TooLarge { url: String, limit: u64 },
    #[error("SHA-256 mismatch for plugin '{plugin}' v{version}: expected {expected}, downloaded {actual}")]
    DigestMismatch {
        plugin: String,
        version: String,
        expected: String,
        actual: String,
    },
    #[error("Failed to write plugin '{plugin}' installation path {path}: {reason}")]
    Io {
        plugin: String,
        path: String,
        reason: String,
    },
    #[error("Installed plugin '{plugin}' has no active release record at {path}")]
    MissingActiveRecord { plugin: String, path: String },
    #[error("Installed plugin '{plugin}' has an invalid trusted receipt at {path}: {reason}")]
    InvalidReceipt {
        plugin: String,
        path: String,
        reason: String,
    },
    #[error("Refusing signed plugin registry revision {received}; this instance has already accepted revision {highest}")]
    RegistryRollback { received: u64, highest: u64 },
    #[error("Refusing plugin registry revision {revision}: it differs from the already accepted catalogue with that revision")]
    RegistryRevisionConflict { revision: u64 },
    #[error("Refusing signed plugin keyset generation {received}; this instance has already accepted generation {highest}")]
    KeysetRollback { received: u64, highest: u64 },
    #[error("Refusing plugin keyset generation {generation}: it differs from the already accepted document with that generation")]
    KeysetGenerationConflict { generation: u64 },
}

#[derive(Debug, Clone)]
pub struct InstallCandidate {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub sha256: String,
    pub binary_path: PathBuf,
    plugin_root: PathBuf,
    install_directory: String,
}

#[derive(Debug, Clone)]
pub struct ActiveInstallation {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub binary_path: PathBuf,
}

/// A regular executable opened without following a leaf symlink and hashed
/// through this exact descriptor. The manager executes this descriptor rather
/// than resolving the path again, closing the verify-to-exec race.
pub(crate) struct VerifiedExecutable {
    file: std::fs::File,
    pub display_path: PathBuf,
}

impl VerifiedExecutable {
    #[cfg(unix)]
    pub(crate) fn command_path(&self, plugin: &str) -> Result<PathBuf, InstallError> {
        use std::os::fd::AsRawFd as _;

        let descriptor = self.file.as_raw_fd();
        // SAFETY: fcntl reads and updates flags on a live descriptor owned by
        // this value. Clearing CLOEXEC is required so /proc/self/fd or /dev/fd
        // still identifies the verified inode in the child at exec time.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if flags < 0
            || unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
        {
            return Err(io_error(
                plugin,
                &self.display_path,
                std::io::Error::last_os_error(),
            ));
        }
        #[cfg(target_os = "linux")]
        let path = PathBuf::from(format!("/proc/self/fd/{descriptor}"));
        // macOS does not expose fexecve and rejects executing /dev/fd paths;
        // its fallback reopens the owner-read-only path after the descriptor
        // walk and hash. Linux executes the exact verified descriptor.
        #[cfg(not(target_os = "linux"))]
        let path = self.display_path.clone();
        Ok(path)
    }

    #[cfg(not(unix))]
    pub(crate) fn command_path(&self, _plugin: &str) -> Result<PathBuf, InstallError> {
        Ok(self.display_path.clone())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct InstallReceipt {
    keyset: KeysetEnvelope,
    keyset_generation: u64,
    envelope: RegistryEnvelope,
    plugin_name: String,
    version: String,
    platform: String,
    sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ActiveRecord {
    version: String,
    directory: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RegistryState {
    highest_revision: u64,
    #[serde(default)]
    catalog_payload_sha256: String,
    highest_keyset_generation: u64,
    keyset: KeysetEnvelope,
}

#[derive(Clone)]
pub struct PluginInstaller {
    registry: RegistryConfig,
    client: reqwest::Client,
}

impl PluginInstaller {
    pub fn new(registry: RegistryConfig) -> Result<Self, InstallError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(BINARY_TIMEOUT)
            .build()
            .map_err(|error| InstallError::Client {
                url: registry.url.clone(),
                reason: error.to_string(),
            })?;
        Ok(Self { registry, client })
    }

    pub async fn prepare(
        &self,
        plugins_dir: &Path,
        registry: &VerifiedRegistry,
        plugin: &RegistryPlugin,
    ) -> Result<InstallCandidate, InstallError> {
        validate_plugin_name(&plugin.name)?;
        validate_version(&plugin.name, &plugin.version)?;
        let platform = platform_target()?;
        let release = plugin
            .platforms
            .get(&platform)
            .ok_or_else(|| InstallError::NoRelease {
                plugin: plugin.name.clone(),
                version: plugin.version.clone(),
                platform: platform.clone(),
            })?;
        let sha256 = normalize_digest(&plugin.name, &plugin.version, &release.sha256)?;
        validate_url(&release.url, &self.registry, false).map_err(|_| {
            InstallError::UnsafeArtifactUrl {
                plugin: plugin.name.clone(),
                url: release.url.clone(),
            }
        })?;

        ensure_directory(&plugin.name, plugins_dir).await?;
        let staging_root = plugins_dir.join(".staging");
        ensure_directory(&plugin.name, &staging_root).await?;
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let stage = staging_root.join(format!("{}-{unique}", plugin.name));
        create_unique_directory(&plugin.name, &stage).await?;
        let staged_binary = stage.join(BINARY_FILE);
        let plugin_root = plugins_dir.join(&plugin.name);
        let directory = format!("{}-{unique}", plugin.version);
        let version_dir = plugin_root.join(&directory);
        let mut moved_to_version_dir = false;
        let prepared = async {
            self.download_binary(plugin, release, &sha256, &staged_binary)
                .await?;
            let receipt = InstallReceipt {
                keyset: registry.keyset.envelope.clone(),
                keyset_generation: registry.keyset.document.generation,
                envelope: registry.envelope.clone(),
                plugin_name: plugin.name.clone(),
                version: plugin.version.clone(),
                platform: platform.clone(),
                sha256: sha256.clone(),
            };
            write_json_synced(&plugin.name, &stage.join(RECEIPT_FILE), &receipt).await?;
            sync_directory(&plugin.name, &stage).await?;
            ensure_directory(&plugin.name, &plugin_root).await?;
            // Install directories are immutable and unique. This means even a
            // same-version reinstall cannot modify the directory referenced by
            // the current active record before its candidate passes startup.
            tokio::fs::rename(&stage, &version_dir)
                .await
                .map_err(|error| io_error(&plugin.name, &version_dir, error))?;
            moved_to_version_dir = true;
            sync_directory(&plugin.name, &plugin_root).await?;

            Ok(InstallCandidate {
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                platform,
                sha256,
                binary_path: version_dir.join(BINARY_FILE),
                plugin_root,
                install_directory: directory,
            })
        }
        .await;

        if let Err(primary) = prepared {
            let cleanup_path = if moved_to_version_dir {
                &version_dir
            } else {
                &stage
            };
            if let Err(cleanup_error) = tokio::fs::remove_dir_all(cleanup_path).await {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        plugin = %plugin.name,
                        path = %cleanup_path.display(),
                        error = %cleanup_error,
                        "Failed to remove incomplete plugin installation"
                    );
                }
            }
            return Err(primary);
        }

        prepared
    }

    /// Persist the signed global registry revision before any artifact from it
    /// is downloaded or executed. Equal revisions are safe for multiple
    /// installs; a lower revision is a replay/downgrade attempt.
    pub async fn accept_registry_revision(
        &self,
        plugins_dir: &Path,
        registry: &VerifiedRegistry,
    ) -> Result<(), InstallError> {
        ensure_directory("registry", plugins_dir).await?;
        let state_path = plugins_dir.join(REGISTRY_STATE_FILE);
        // A newer root-authorized keyset is security state in its own right.
        // Persist it even when the accompanying catalogue is later refused as
        // a revision rollback, so an emergency revocation cannot be discarded
        // by pairing it with a stale catalogue response.
        self.refresh_keyset(plugins_dir, &registry.keyset).await?;
        let previous = read_registry_state(&state_path).await?;
        let catalog_payload = base64::engine::general_purpose::STANDARD
            .decode(&registry.envelope.payload)
            .map_err(|error| {
                invalid_receipt(
                    "registry",
                    &state_path,
                    format!("verified catalogue payload is not valid base64: {error}"),
                )
            })?;
        let catalog_payload_sha256 = hex::encode(Sha256::digest(&catalog_payload));
        if let Some(previous) = &previous {
            if registry.document.revision < previous.highest_revision {
                return Err(InstallError::RegistryRollback {
                    received: registry.document.revision,
                    highest: previous.highest_revision,
                });
            }
            if registry.document.revision == previous.highest_revision
                && !previous.catalog_payload_sha256.is_empty()
            {
                if catalog_payload_sha256 != previous.catalog_payload_sha256 {
                    return Err(InstallError::RegistryRevisionConflict {
                        revision: registry.document.revision,
                    });
                }
                return Ok(());
            }
            // An equal revision with an empty digest continues once to migrate
            // the pre-binding state format using this authenticated payload.
        }
        write_json_atomically(
            "registry",
            &state_path,
            &RegistryState {
                highest_revision: registry.document.revision,
                catalog_payload_sha256,
                highest_keyset_generation: registry.keyset.document.generation,
                keyset: registry.keyset.envelope.clone(),
            },
        )
        .await?;
        sync_directory("registry", plugins_dir).await
    }

    /// Persist a newer root-authorized keyset while retaining the last accepted
    /// catalogue revision for rollback protection. Callers must serialize this
    /// read/compare/write operation with catalogue revision acceptance.
    pub async fn refresh_keyset(
        &self,
        plugins_dir: &Path,
        keyset: &VerifiedKeyset,
    ) -> Result<(), InstallError> {
        let state_path = plugins_dir.join(REGISTRY_STATE_FILE);
        let Some(previous) = read_registry_state(&state_path).await? else {
            return Ok(());
        };
        if keyset.document.generation < previous.highest_keyset_generation {
            return Err(InstallError::KeysetRollback {
                received: keyset.document.generation,
                highest: previous.highest_keyset_generation,
            });
        }
        if keyset.document.generation == previous.highest_keyset_generation {
            if keyset.envelope.payload != previous.keyset.payload {
                return Err(InstallError::KeysetGenerationConflict {
                    generation: keyset.document.generation,
                });
            }
            return Ok(());
        }
        write_json_atomically(
            "registry",
            &state_path,
            &RegistryState {
                highest_revision: previous.highest_revision,
                catalog_payload_sha256: previous.catalog_payload_sha256,
                highest_keyset_generation: keyset.document.generation,
                keyset: keyset.envelope.clone(),
            },
        )
        .await?;
        sync_directory("registry", plugins_dir).await
    }

    pub async fn activate(&self, candidate: &InstallCandidate) -> Result<(), InstallError> {
        write_json_atomically(
            &candidate.name,
            &candidate.plugin_root.join(ACTIVE_FILE),
            &ActiveRecord {
                version: candidate.version.clone(),
                directory: candidate.install_directory.clone(),
            },
        )
        .await?;
        // The rename above is the commit point. A directory fsync failure
        // means durability is uncertain, not that activation was rolled back;
        // reporting an error here would make the caller kill the candidate
        // even though active.json already points at it.
        if let Err(error) = sync_directory(&candidate.name, &candidate.plugin_root).await {
            tracing::warn!(
                plugin = %candidate.name,
                path = %candidate.plugin_root.display(),
                error = %error,
                "Plugin activation committed but directory fsync failed"
            );
        }
        Ok(())
    }

    /// Remove a prepared release that never reached the activation commit
    /// point. The exact immutable directory is carried by the candidate, so
    /// cleanup never derives a recursive target from caller input.
    pub async fn discard(&self, candidate: &InstallCandidate) -> Result<(), InstallError> {
        let directory = candidate.plugin_root.join(&candidate.install_directory);
        tokio::fs::remove_dir_all(&directory)
            .await
            .map_err(|error| io_error(&candidate.name, &directory, error))?;
        sync_directory(&candidate.name, &candidate.plugin_root).await
    }

    async fn download_binary(
        &self,
        plugin: &RegistryPlugin,
        release: &PlatformRelease,
        expected: &str,
        destination: &Path,
    ) -> Result<(), InstallError> {
        let response = self
            .client
            .get(&release.url)
            .header("User-Agent", "temps-plugin-installer")
            .send()
            .await
            .map_err(|error| InstallError::Download {
                plugin: plugin.name.clone(),
                url: release.url.clone(),
                reason: error.to_string(),
            })?;
        if response.status().is_redirection() || !response.status().is_success() {
            return Err(InstallError::DownloadStatus {
                url: release.url.clone(),
                status: response.status().as_u16(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BINARY_BYTES)
        {
            return Err(InstallError::TooLarge {
                url: release.url.clone(),
                limit: MAX_BINARY_BYTES,
            });
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await
            .map_err(|error| io_error(&plugin.name, destination, error))?;
        let mut total = 0u64;
        let mut hasher = Sha256::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| InstallError::Download {
                plugin: plugin.name.clone(),
                url: release.url.clone(),
                reason: error.to_string(),
            })?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_BINARY_BYTES {
                return Err(InstallError::TooLarge {
                    url: release.url.clone(),
                    limit: MAX_BINARY_BYTES,
                });
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|error| io_error(&plugin.name, destination, error))?;
        }
        file.flush()
            .await
            .map_err(|error| io_error(&plugin.name, destination, error))?;
        file.sync_all()
            .await
            .map_err(|error| io_error(&plugin.name, destination, error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o500))
                .await
                .map_err(|error| io_error(&plugin.name, destination, error))?;
            file.sync_all()
                .await
                .map_err(|error| io_error(&plugin.name, destination, error))?;
        }
        drop(file);
        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            return Err(InstallError::DigestMismatch {
                plugin: plugin.name.clone(),
                version: plugin.version.clone(),
                expected: expected.to_string(),
                actual,
            });
        }
        Ok(())
    }
}

async fn read_registry_state(state_path: &Path) -> Result<Option<RegistryState>, InstallError> {
    match tokio::fs::symlink_metadata(state_path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(InstallError::Io {
                plugin: "registry".to_string(),
                path: state_path.display().to_string(),
                reason: "registry state must be a regular file".to_string(),
            })
        }
        Ok(_) => {
            let bytes = read_regular_file_capped("registry", state_path, 128 * 1024).await?;
            serde_json::from_slice::<RegistryState>(&bytes)
                .map(Some)
                .map_err(|error| InstallError::Io {
                    plugin: "registry".to_string(),
                    path: state_path.display().to_string(),
                    reason: format!("invalid registry state: {error}"),
                })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("registry", state_path, error)),
    }
}

pub async fn discover_active(
    plugins_dir: &Path,
    registry: &RegistryConfig,
) -> Vec<Result<ActiveInstallation, InstallError>> {
    let mut results = Vec::new();
    let state_path = plugins_dir.join(REGISTRY_STATE_FILE);
    let state_bytes = match read_regular_file_capped("registry", &state_path, 128 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => {
            if has_plugin_directories(plugins_dir).await {
                results.push(Err(error));
            }
            return results;
        }
    };
    let state: RegistryState = match serde_json::from_slice(&state_bytes) {
        Ok(state) => state,
        Err(error) => {
            results.push(Err(invalid_receipt(
                "registry",
                &state_path,
                format!("invalid registry state: {error}"),
            )));
            return results;
        }
    };
    let accepted_keyset = match VerifiedKeyset::verify(
        state.keyset,
        &registry.root_trust,
        chrono::Utc::now(),
        KeysetUse::HistoricalReceipt,
    ) {
        Ok(keyset) if keyset.document.generation == state.highest_keyset_generation => keyset,
        Ok(keyset) => {
            results.push(Err(invalid_receipt(
                "registry",
                &state_path,
                format!(
                    "keyset generation {} does not match persisted generation {}",
                    keyset.document.generation, state.highest_keyset_generation
                ),
            )));
            return results;
        }
        Err(error) => {
            results.push(Err(invalid_receipt(
                "registry",
                &state_path,
                error.to_string(),
            )));
            return results;
        }
    };
    let mut entries = match tokio::fs::read_dir(plugins_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return results,
        Err(error) => {
            results.push(Err(io_error("registry", plugins_dir, error)));
            return results;
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = match entry.file_name().to_str() {
            Some(name) if !name.starts_with('.') => name.to_string(),
            _ => continue,
        };
        let path = entry.path();
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) => {
                results.push(Err(io_error(&name, &path, error)));
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        results.push(verify_active(&name, &path, registry, &accepted_keyset).await);
    }
    results
}

async fn has_plugin_directories(plugins_dir: &Path) -> bool {
    let Ok(mut entries) = tokio::fs::read_dir(plugins_dir).await else {
        return false;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if tokio::fs::symlink_metadata(entry.path())
            .await
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            return true;
        }
    }
    false
}

async fn verify_active(
    name: &str,
    plugin_root: &Path,
    registry: &RegistryConfig,
    accepted_keyset: &VerifiedKeyset,
) -> Result<ActiveInstallation, InstallError> {
    validate_plugin_name(name)?;
    let active_path = plugin_root.join(ACTIVE_FILE);
    let active_bytes = read_regular_file_capped(name, &active_path, 16 * 1024)
        .await
        .map_err(|_| InstallError::MissingActiveRecord {
            plugin: name.to_string(),
            path: active_path.display().to_string(),
        })?;
    let active: ActiveRecord =
        serde_json::from_slice(&active_bytes).map_err(|error| InstallError::InvalidReceipt {
            plugin: name.to_string(),
            path: active_path.display().to_string(),
            reason: error.to_string(),
        })?;
    validate_version(name, &active.version)?;
    validate_version(name, &active.directory)?;
    let version_dir = plugin_root.join(&active.directory);
    let receipt_path = version_dir.join(RECEIPT_FILE);
    let receipt_bytes = read_regular_file_capped(name, &receipt_path, 2 * 1024 * 1024)
        .await
        .map_err(|error| invalid_receipt(name, &receipt_path, error.to_string()))?;
    let receipt: InstallReceipt = serde_json::from_slice(&receipt_bytes)
        .map_err(|error| invalid_receipt(name, &receipt_path, error.to_string()))?;
    let receipt_keyset = VerifiedKeyset::verify(
        receipt.keyset,
        &registry.root_trust,
        chrono::Utc::now(),
        KeysetUse::HistoricalReceipt,
    )
    .map_err(|error| invalid_receipt(name, &receipt_path, error.to_string()))?;
    if receipt_keyset.document.generation != receipt.keyset_generation {
        return Err(invalid_receipt(
            name,
            &receipt_path,
            "receipt keyset generation does not match its signed payload",
        ));
    }
    if receipt.keyset_generation > accepted_keyset.document.generation {
        return Err(invalid_receipt(
            name,
            &receipt_path,
            "receipt keyset is newer than the accepted registry state",
        ));
    }
    let current_platform = platform_target()?;
    if receipt.platform != current_platform {
        return Err(invalid_receipt(
            name,
            &receipt_path,
            format!(
                "receipt targets platform '{}', but this host is '{}'",
                receipt.platform, current_platform
            ),
        ));
    }
    let verified = crate::catalog::verify_envelope(
        receipt.envelope,
        accepted_keyset.clone(),
        &registry.url,
        KeysetUse::HistoricalReceipt,
    )
    .map_err(|error| invalid_receipt(name, &receipt_path, error.to_string()))?;
    let plugin = verified
        .document
        .plugins
        .iter()
        .find(|plugin| plugin.name == name && plugin.version == active.version)
        .ok_or_else(|| invalid_receipt(name, &receipt_path, "signed plugin/version missing"))?;
    let release = plugin
        .platforms
        .get(&receipt.platform)
        .ok_or_else(|| invalid_receipt(name, &receipt_path, "signed platform release missing"))?;
    let signed_digest = normalize_digest(name, &active.version, &release.sha256)?;
    if receipt.plugin_name != name
        || receipt.version != active.version
        || receipt.sha256 != signed_digest
    {
        return Err(invalid_receipt(
            name,
            &receipt_path,
            "receipt fields do not match the signed release",
        ));
    }
    let binary_path = version_dir.join(BINARY_FILE);
    let actual = hash_regular_file_capped(name, &binary_path, MAX_BINARY_BYTES)
        .await
        .map_err(|error| invalid_receipt(name, &receipt_path, error.to_string()))?;
    if actual != signed_digest {
        return Err(InstallError::DigestMismatch {
            plugin: name.to_string(),
            version: active.version,
            expected: signed_digest,
            actual,
        });
    }
    Ok(ActiveInstallation {
        name: name.to_string(),
        version: receipt.version,
        sha256: signed_digest,
        binary_path,
    })
}

pub(crate) async fn open_verified_executable(
    plugin: &str,
    plugins_dir: &Path,
    path: &Path,
    expected_sha256: &str,
) -> Result<VerifiedExecutable, InstallError> {
    let mut file = open_executable_beneath(plugin, plugins_dir, path)?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| io_error(plugin, path, error))?;
    if metadata.len() > MAX_BINARY_BYTES {
        return Err(InstallError::TooLarge {
            url: path.display().to_string(),
            limit: MAX_BINARY_BYTES,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(InstallError::Io {
                plugin: plugin.to_string(),
                path: path.display().to_string(),
                reason: "verified executable must have exactly one hard link".to_string(),
            });
        }
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o500 {
            return Err(InstallError::Io {
                plugin: plugin.to_string(),
                path: path.display().to_string(),
                reason: "verified executable must be owned by the Temps user with mode 0500"
                    .to_string(),
            });
        }
    }
    let actual = hash_open_file_capped(plugin, path, &mut file, MAX_BINARY_BYTES).await?;
    if actual != expected_sha256 {
        return Err(InstallError::DigestMismatch {
            plugin: plugin.to_string(),
            version: "active".to_string(),
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    let mut file = file.into_std().await;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_error(plugin, path, error))?;
    Ok(VerifiedExecutable {
        file,
        display_path: path.to_path_buf(),
    })
}

#[cfg(unix)]
fn open_executable_beneath(
    plugin: &str,
    plugins_dir: &Path,
    path: &Path,
) -> Result<tokio::fs::File, InstallError> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;

    let relative = path
        .strip_prefix(plugins_dir)
        .map_err(|_| InstallError::Io {
            plugin: plugin.to_string(),
            path: path.display().to_string(),
            reason: format!(
                "executable must be beneath plugin root {}",
                plugins_dir.display()
            ),
        })?;
    let components: Vec<_> = relative.components().collect();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(InstallError::Io {
            plugin: plugin.to_string(),
            path: path.display().to_string(),
            reason: "executable path contains an unsafe component".to_string(),
        });
    }

    let root = std::ffi::CString::new(plugins_dir.as_os_str().as_bytes()).map_err(|_| {
        InstallError::Io {
            plugin: plugin.to_string(),
            path: plugins_dir.display().to_string(),
            reason: "plugin root contains a NUL byte".to_string(),
        }
    })?;
    // SAFETY: root is a valid C string and the returned descriptor is moved
    // exactly once into OwnedFd.
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io_error(
            plugin,
            plugins_dir,
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: root_fd was returned by open and is uniquely owned here.
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };

    for component in &components[..components.len() - 1] {
        let std::path::Component::Normal(component) = component else {
            unreachable!("components were validated above")
        };
        let component =
            std::ffi::CString::new(component.as_bytes()).map_err(|_| InstallError::Io {
                plugin: plugin.to_string(),
                path: path.display().to_string(),
                reason: "executable path contains a NUL byte".to_string(),
            })?;
        // SAFETY: both descriptors and the C string are live for the call.
        let next = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 {
            return Err(io_error(plugin, path, std::io::Error::last_os_error()));
        }
        // SAFETY: next was returned by openat and ownership moves once.
        directory = unsafe { OwnedFd::from_raw_fd(next) };
    }

    let std::path::Component::Normal(file_name) = components[components.len() - 1] else {
        unreachable!("components were validated above")
    };
    let file_name = std::ffi::CString::new(file_name.as_bytes()).map_err(|_| InstallError::Io {
        plugin: plugin.to_string(),
        path: path.display().to_string(),
        reason: "executable filename contains a NUL byte".to_string(),
    })?;
    // SAFETY: directory and file_name are valid and live for this call.
    let file = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            file_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if file < 0 {
        return Err(io_error(plugin, path, std::io::Error::last_os_error()));
    }
    // SAFETY: file was returned by openat and ownership moves into File once.
    let file = unsafe { std::fs::File::from_raw_fd(file) };
    Ok(tokio::fs::File::from_std(file))
}

#[cfg(not(unix))]
fn open_executable_beneath(
    plugin: &str,
    plugins_dir: &Path,
    path: &Path,
) -> Result<tokio::fs::File, InstallError> {
    if !path.starts_with(plugins_dir) {
        return Err(InstallError::Io {
            plugin: plugin.to_string(),
            path: path.display().to_string(),
            reason: format!(
                "executable must be beneath plugin root {}",
                plugins_dir.display()
            ),
        });
    }
    open_regular_file(plugin, path)
}

pub fn platform_target_for(os: &str, arch: &str, target_env: &str) -> Result<String, InstallError> {
    let target = match (os, arch, target_env) {
        ("macos", "x86_64", _) => "darwin-amd64",
        ("macos", "aarch64", _) => "darwin-arm64",
        ("linux", "x86_64", "gnu") => "linux-amd64-gnu",
        ("linux", "x86_64", "musl") => "linux-amd64-musl",
        ("linux", "aarch64", "gnu") => "linux-arm64-gnu",
        ("linux", "aarch64", "musl") => "linux-arm64-musl",
        _ => {
            return Err(InstallError::UnsupportedPlatform {
                os: os.to_string(),
                arch: arch.to_string(),
                target_env: target_env.to_string(),
            })
        }
    };
    Ok(target.to_string())
}

pub fn platform_target() -> Result<String, InstallError> {
    let target_env = if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "gnu") {
        "gnu"
    } else {
        "unknown"
    };
    platform_target_for(std::env::consts::OS, std::env::consts::ARCH, target_env)
}

pub fn validate_plugin_name(name: &str) -> Result<(), InstallError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(InstallError::UnsafePluginName {
            name: name.to_string(),
        })
    }
}

pub(crate) fn validate_version(plugin: &str, version: &str) -> Result<(), InstallError> {
    let valid = !version.is_empty()
        && version.len() <= 64
        && !version.contains(['/', '\\'])
        && version != "."
        && version != ".."
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    if valid {
        Ok(())
    } else {
        Err(InstallError::UnsafeVersion {
            plugin: plugin.to_string(),
            version: version.to_string(),
        })
    }
}

pub(crate) fn normalize_digest(
    plugin: &str,
    version: &str,
    digest: &str,
) -> Result<String, InstallError> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(InstallError::InvalidDigest {
            plugin: plugin.to_string(),
            version: version.to_string(),
            digest: digest.to_string(),
        });
    }
    Ok(digest.to_ascii_lowercase())
}

async fn ensure_directory(plugin: &str, path: &Path) -> Result<(), InstallError> {
    let result = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(InstallError::Io {
                plugin: plugin.to_string(),
                path: path.display().to_string(),
                reason: "path must be a real directory, not a symlink or file".to_string(),
            })
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(path)
                .await
                .map_err(|error| io_error(plugin, path, error))?;
            let metadata = tokio::fs::symlink_metadata(path)
                .await
                .map_err(|error| io_error(plugin, path, error))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(InstallError::Io {
                    plugin: plugin.to_string(),
                    path: path.display().to_string(),
                    reason: "created path is not a real directory".to_string(),
                });
            }
            Ok(())
        }
        Err(error) => Err(io_error(plugin, path, error)),
    };
    result?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| io_error(plugin, path, error))?;
    }
    Ok(())
}

async fn create_unique_directory(plugin: &str, path: &Path) -> Result<(), InstallError> {
    tokio::fs::create_dir(path)
        .await
        .map_err(|error| io_error(plugin, path, error))
}

async fn write_json_synced<T: Serialize>(
    plugin: &str,
    path: &Path,
    value: &T,
) -> Result<(), InstallError> {
    let bytes = serde_json::to_vec(value).map_err(|error| InstallError::Io {
        plugin: plugin.to_string(),
        path: path.display().to_string(),
        reason: format!("failed to encode JSON: {error}"),
    })?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| io_error(plugin, path, error))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| io_error(plugin, path, error))?;
    file.sync_all()
        .await
        .map_err(|error| io_error(plugin, path, error))
}

async fn write_json_atomically<T: Serialize>(
    plugin: &str,
    destination: &Path,
    value: &T,
) -> Result<(), InstallError> {
    let parent = destination.parent().ok_or_else(|| InstallError::Io {
        plugin: plugin.to_string(),
        path: destination.display().to_string(),
        reason: "destination has no parent".to_string(),
    })?;
    ensure_directory(plugin, parent).await?;
    let temporary = parent.join(format!(".active-{}.tmp", uuid::Uuid::new_v4()));
    write_json_synced(plugin, &temporary, value).await?;
    tokio::fs::rename(&temporary, destination)
        .await
        .map_err(|error| io_error(plugin, destination, error))
}

async fn sync_directory(plugin: &str, path: &Path) -> Result<(), InstallError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| io_error(plugin, path, error))?;
    file.sync_all()
        .await
        .map_err(|error| io_error(plugin, path, error))
}

async fn read_regular_file_capped(
    plugin: &str,
    path: &Path,
    limit: u64,
) -> Result<Vec<u8>, InstallError> {
    let mut file = open_regular_file(plugin, path)?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| io_error(plugin, path, error))?;
    if metadata.len() > limit {
        return Err(InstallError::TooLarge {
            url: path.display().to_string(),
            limit,
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .await
        .map_err(|error| io_error(plugin, path, error))?;
    if bytes.len() as u64 > limit {
        return Err(InstallError::TooLarge {
            url: path.display().to_string(),
            limit,
        });
    }
    Ok(bytes)
}

async fn hash_regular_file_capped(
    plugin: &str,
    path: &Path,
    limit: u64,
) -> Result<String, InstallError> {
    let mut file = open_regular_file(plugin, path)?;
    hash_open_file_capped(plugin, path, &mut file, limit).await
}

async fn hash_open_file_capped(
    plugin: &str,
    path: &Path,
    file: &mut tokio::fs::File,
    limit: u64,
) -> Result<String, InstallError> {
    let metadata = file
        .metadata()
        .await
        .map_err(|error| io_error(plugin, path, error))?;
    if metadata.len() > limit {
        return Err(InstallError::TooLarge {
            url: path.display().to_string(),
            limit,
        });
    }
    let mut total = 0u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| io_error(plugin, path, error))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > limit {
            return Err(InstallError::TooLarge {
                url: path.display().to_string(),
                limit,
            });
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn open_regular_file(plugin: &str, path: &Path) -> Result<tokio::fs::File, InstallError> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    };
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new().read(true).open(path);

    let file = file.map_err(|error| io_error(plugin, path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| io_error(plugin, path, error))?;
    if !metadata.is_file() {
        return Err(InstallError::Io {
            plugin: plugin.to_string(),
            path: path.display().to_string(),
            reason: "path must be a regular file, not a symlink".to_string(),
        });
    }
    Ok(tokio::fs::File::from_std(file))
}

fn io_error(plugin: &str, path: &Path, error: std::io::Error) -> InstallError {
    InstallError::Io {
        plugin: plugin.to_string(),
        path: path.display().to_string(),
        reason: error.to_string(),
    }
}

fn invalid_receipt(plugin: &str, path: &Path, reason: impl Into<String>) -> InstallError {
    InstallError::InvalidReceipt {
        plugin: plugin.to_string(),
        path: path.display().to_string(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::collections::BTreeMap;

    async fn serve_once(status: &str, headers: &[(&str, String)], body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let status = status.to_string();
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await.expect("read request");
            let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
            for (name, value) in headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            if !response.to_ascii_lowercase().contains("content-length:") {
                response.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            response.push_str("\r\n");
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response headers");
            stream.write_all(&body).await.expect("write response body");
        });
        format!("http://{address}/plugin")
    }

    fn signed_registry(plugin: RegistryPlugin) -> (VerifiedRegistry, SigningKey) {
        signed_registry_revision(plugin, 1)
    }

    fn signed_registry_revision(
        plugin: RegistryPlugin,
        revision: u64,
    ) -> (VerifiedRegistry, SigningKey) {
        let signing = SigningKey::from_bytes(&[42; 32]);
        let document = crate::catalog::RegistryDocument {
            schema_version: 1,
            revision,
            issued_at: chrono::Utc::now() - chrono::Duration::minutes(1),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            plugins: vec![plugin],
        };
        let payload = serde_json::to_vec(&document).expect("serialize registry");
        let envelope = RegistryEnvelope {
            key_id: "test-key".to_string(),
            payload: base64::engine::general_purpose::STANDARD.encode(&payload),
            signature: base64::engine::general_purpose::STANDARD.encode(
                signing
                    .sign(&crate::trust::signature_message(
                        crate::trust::CATALOG_SIGNATURE_DOMAIN,
                        &payload,
                    ))
                    .to_bytes(),
            ),
        };
        let keyset = crate::trust::VerifiedKeyset::test_fixture(
            "test-key",
            signing.verifying_key().to_bytes(),
        )
        .1;
        (
            VerifiedRegistry {
                keyset,
                envelope,
                document,
            },
            signing,
        )
    }

    fn plugin(url: String, bytes: &[u8], version: &str) -> RegistryPlugin {
        RegistryPlugin {
            name: "test-plugin".to_string(),
            title: "Test plugin".to_string(),
            summary: "test".to_string(),
            description: "test".to_string(),
            author: "Temps Contributors".to_string(),
            category: "test".to_string(),
            keywords: vec!["test".to_string()],
            logo_url: None,
            repository: None,
            docs_url: None,
            version: version.to_string(),
            platforms: BTreeMap::from([(
                platform_target().expect("supported test platform"),
                PlatformRelease {
                    url,
                    sha256: hex::encode(Sha256::digest(bytes)),
                },
            )]),
        }
    }

    fn installer(url: &str, signing: &SigningKey) -> (PluginInstaller, RegistryConfig) {
        let config = RegistryConfig::local(
            url.to_string(),
            "test-key",
            signing.verifying_key().to_bytes(),
        );
        (
            PluginInstaller::new(config.clone()).expect("test installer"),
            config,
        )
    }

    fn rotated_keyset(
        old_key: &SigningKey,
        old_status: crate::trust::CatalogKeyStatus,
        new_key: &SigningKey,
        generation: u64,
    ) -> VerifiedKeyset {
        crate::trust::VerifiedKeyset::test_fixture_with_keys(
            vec![
                (
                    "test-key".to_string(),
                    old_key.verifying_key().to_bytes(),
                    old_status,
                ),
                (
                    "next-key".to_string(),
                    new_key.verifying_key().to_bytes(),
                    crate::trust::CatalogKeyStatus::Active,
                ),
            ],
            generation,
        )
        .1
    }

    #[test]
    fn platform_selection_is_explicit() {
        assert_eq!(
            platform_target_for("linux", "x86_64", "gnu").expect("linux gnu"),
            "linux-amd64-gnu"
        );
        assert_eq!(
            platform_target_for("linux", "aarch64", "musl").expect("linux musl"),
            "linux-arm64-musl"
        );
        assert_eq!(
            platform_target_for("macos", "aarch64", "unknown").expect("mac"),
            "darwin-arm64"
        );
        assert!(matches!(
            platform_target_for("windows", "x86_64", "gnu"),
            Err(InstallError::UnsupportedPlatform { .. })
        ));
        assert!(matches!(
            platform_target_for("linux", "x86_64", "unknown"),
            Err(InstallError::UnsupportedPlatform { .. })
        ));
    }

    #[test]
    fn unsafe_names_cannot_escape_plugin_root() {
        for name in ["../escape", "a/b", "A", "-leading", "trailing-", ""] {
            assert!(validate_plugin_name(name).is_err(), "accepted {name:?}");
        }
        assert!(validate_plugin_name("analytics-2").is_ok());
    }

    #[test]
    fn digest_accepts_uppercase_and_normalizes() {
        let upper = "AB".repeat(32);
        assert_eq!(
            normalize_digest("p", "1", &upper).expect("digest"),
            "ab".repeat(32)
        );
        assert!(normalize_digest("p", "1", "abc").is_err());
    }

    #[tokio::test]
    async fn registry_revision_is_persisted_and_cannot_roll_back() {
        let signing = SigningKey::from_bytes(&[42; 32]);
        let (installer, _) = installer("http://127.0.0.1/plugin", &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        let release = plugin("http://127.0.0.1/plugin".to_string(), b"fixture", "1.0.0");
        let registry_7 = signed_registry_revision(release.clone(), 7).0;
        let registry_6 = signed_registry_revision(release, 6).0;
        installer
            .accept_registry_revision(temp.path(), &registry_7)
            .await
            .expect("accept revision");
        installer
            .accept_registry_revision(temp.path(), &registry_7)
            .await
            .expect("equal revision remains valid");
        assert!(matches!(
            installer
                .accept_registry_revision(temp.path(), &registry_6)
                .await,
            Err(InstallError::RegistryRollback {
                received: 6,
                highest: 7
            })
        ));
    }

    #[tokio::test]
    async fn equal_registry_revision_with_different_payload_is_rejected() {
        let signing = SigningKey::from_bytes(&[42; 32]);
        let (installer, _) = installer("http://127.0.0.1/plugin", &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        let first = signed_registry_revision(
            plugin("http://127.0.0.1/plugin".to_string(), b"one", "1.0.0"),
            7,
        )
        .0;
        let conflicting = signed_registry_revision(
            plugin("http://127.0.0.1/plugin".to_string(), b"two", "2.0.0"),
            7,
        )
        .0;
        installer
            .accept_registry_revision(temp.path(), &first)
            .await
            .expect("accept first revision payload");

        assert!(matches!(
            installer
                .accept_registry_revision(temp.path(), &conflicting)
                .await,
            Err(InstallError::RegistryRevisionConflict { revision: 7 })
        ));
    }

    #[tokio::test]
    async fn keyset_refresh_rejects_rollback_and_same_generation_conflict() {
        let old_key = SigningKey::from_bytes(&[42; 32]);
        let new_key = SigningKey::from_bytes(&[43; 32]);
        let (installer, _) = installer("http://127.0.0.1/plugin", &old_key);
        let temp = tempfile::tempdir().expect("tempdir");
        let release = plugin("http://127.0.0.1/plugin".to_string(), b"fixture", "1.0.0");
        let registry = signed_registry_revision(release, 7).0;
        installer
            .accept_registry_revision(temp.path(), &registry)
            .await
            .expect("accept initial keyset");
        let generation_2 = rotated_keyset(
            &old_key,
            crate::trust::CatalogKeyStatus::VerifyOnly,
            &new_key,
            2,
        );
        installer
            .refresh_keyset(temp.path(), &generation_2)
            .await
            .expect("accept planned rotation");

        assert!(matches!(
            installer
                .refresh_keyset(temp.path(), &registry.keyset)
                .await,
            Err(InstallError::KeysetRollback {
                received: 1,
                highest: 2
            })
        ));

        let conflicting_generation_2 = rotated_keyset(
            &old_key,
            crate::trust::CatalogKeyStatus::Revoked,
            &new_key,
            2,
        );
        assert!(matches!(
            installer
                .refresh_keyset(temp.path(), &conflicting_generation_2)
                .await,
            Err(InstallError::KeysetGenerationConflict { generation: 2 })
        ));
    }

    #[tokio::test]
    async fn newer_keyset_is_kept_when_catalogue_revision_rolls_back() {
        let old_key = SigningKey::from_bytes(&[42; 32]);
        let new_key = SigningKey::from_bytes(&[43; 32]);
        let (installer, _) = installer("http://127.0.0.1/plugin", &old_key);
        let temp = tempfile::tempdir().expect("tempdir");
        let release = plugin("http://127.0.0.1/plugin".to_string(), b"fixture", "1.0.0");
        let registry_7 = signed_registry_revision(release.clone(), 7).0;
        installer
            .accept_registry_revision(temp.path(), &registry_7)
            .await
            .expect("accept initial registry");
        let mut registry_6 = signed_registry_revision(release, 6).0;
        registry_6.keyset = rotated_keyset(
            &old_key,
            crate::trust::CatalogKeyStatus::Revoked,
            &new_key,
            2,
        );

        assert!(matches!(
            installer
                .accept_registry_revision(temp.path(), &registry_6)
                .await,
            Err(InstallError::RegistryRollback {
                received: 6,
                highest: 7
            })
        ));
        let state = read_registry_state(&temp.path().join(REGISTRY_STATE_FILE))
            .await
            .expect("read registry state")
            .expect("registry state exists");
        assert_eq!(state.highest_revision, 7);
        assert_eq!(state.highest_keyset_generation, 2);
    }

    #[tokio::test]
    async fn rotation_preserves_receipt_but_revocation_blocks_it() {
        let bytes = b"standalone executable bytes";
        let url = serve_once("200 OK", &[], bytes.to_vec()).await;
        let (registry, old_key) = signed_registry(plugin(url.clone(), bytes, "1.2.3"));
        let new_key = SigningKey::from_bytes(&[43; 32]);
        let (installer, config) = installer(&url, &old_key);
        let temp = tempfile::tempdir().expect("tempdir");
        installer
            .accept_registry_revision(temp.path(), &registry)
            .await
            .expect("accept initial registry state");
        let candidate = installer
            .prepare(temp.path(), &registry, &registry.document.plugins[0])
            .await
            .expect("prepare binary");
        installer.activate(&candidate).await.expect("activate");

        let verify_only = rotated_keyset(
            &old_key,
            crate::trust::CatalogKeyStatus::VerifyOnly,
            &new_key,
            2,
        );
        installer
            .refresh_keyset(temp.path(), &verify_only)
            .await
            .expect("accept verify-only rotation");
        assert!(matches!(
            &discover_active(temp.path(), &config).await[..],
            [Ok(found)] if found.version == "1.2.3"
        ));

        let revoked = rotated_keyset(
            &old_key,
            crate::trust::CatalogKeyStatus::Revoked,
            &new_key,
            3,
        );
        installer
            .refresh_keyset(temp.path(), &revoked)
            .await
            .expect("accept emergency revocation");
        assert!(matches!(
            &discover_active(temp.path(), &config).await[..],
            [Err(InstallError::InvalidReceipt { reason, .. })]
                if reason.contains("key status is Revoked")
        ));
    }

    #[tokio::test]
    async fn successful_direct_binary_install_and_discovery() {
        let bytes = b"standalone executable bytes";
        let url = serve_once("200 OK", &[], bytes.to_vec()).await;
        let (registry, signing) = signed_registry(plugin(url.clone(), bytes, "1.2.3"));
        let (installer, config) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_dir = temp.path().join("plugins");
        installer
            .accept_registry_revision(&plugins_dir, &registry)
            .await
            .expect("accept registry state");
        let candidate = installer
            .prepare(&plugins_dir, &registry, &registry.document.plugins[0])
            .await
            .expect("prepare binary");
        assert_eq!(
            std::fs::read(&candidate.binary_path).expect("binary"),
            bytes
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&candidate.binary_path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o500);
        }
        installer.activate(&candidate).await.expect("activate");
        let active = discover_active(&plugins_dir, &config).await;
        assert!(matches!(&active[..], [Ok(found)] if found.version == "1.2.3"));
    }

    #[tokio::test]
    async fn test_prepare_receipt_write_failure_removes_staging_directory() {
        // Arrange: place a receipt in the newly-created staging directory
        // while the installer is waiting for the artifact response. This
        // forces the post-download create_new receipt write to fail.
        let bytes = b"standalone executable bytes".to_vec();
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins_dir = temp.path().join("plugins");
        let staging_root = plugins_dir.join(".staging");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let url = format!("http://{}/plugin", listener.local_addr().expect("address"));
        let server_staging_root = staging_root.clone();
        let response_body = bytes.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await.expect("read request");
            let stage = std::fs::read_dir(&server_staging_root)
                .expect("staging root")
                .next()
                .expect("staging child")
                .expect("staging entry")
                .path();
            std::fs::write(stage.join(RECEIPT_FILE), b"occupied").expect("occupy receipt path");
            let response = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                response_body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response headers");
            stream
                .write_all(&response_body)
                .await
                .expect("write response body");
        });
        let (registry, signing) = signed_registry(plugin(url.clone(), &bytes, "1.2.3"));
        let (installer, _) = installer(&url, &signing);

        // Act
        let result = installer
            .prepare(&plugins_dir, &registry, &registry.document.plugins[0])
            .await;
        server.await.expect("test server task");

        // Assert
        assert!(matches!(result, Err(InstallError::Io { .. })));
        let remaining = std::fs::read_dir(&staging_root)
            .expect("staging root remains")
            .count();
        assert_eq!(remaining, 0, "failed preparation must remove its stage");
    }

    #[tokio::test]
    async fn test_discover_active_tampered_receipt_is_rejected() {
        // Arrange
        let bytes = b"standalone executable bytes";
        let url = serve_once("200 OK", &[], bytes.to_vec()).await;
        let (registry, signing) = signed_registry(plugin(url.clone(), bytes, "1.2.3"));
        let (installer, config) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        installer
            .accept_registry_revision(temp.path(), &registry)
            .await
            .expect("accept registry state");
        let candidate = installer
            .prepare(temp.path(), &registry, &registry.document.plugins[0])
            .await
            .expect("prepare binary");
        installer.activate(&candidate).await.expect("activate");
        let receipt_path = candidate
            .plugin_root
            .join(&candidate.install_directory)
            .join(RECEIPT_FILE);
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).expect("read original receipt"))
                .expect("parse receipt");
        receipt["sha256"] = serde_json::Value::String("00".repeat(32));
        std::fs::write(
            &receipt_path,
            serde_json::to_vec(&receipt).expect("serialize tampered receipt"),
        )
        .expect("tamper receipt");

        // Act
        let discovered = discover_active(temp.path(), &config).await;

        // Assert
        assert!(matches!(
            &discovered[..],
            [Err(InstallError::InvalidReceipt { reason, .. })]
                if reason.contains("do not match the signed release")
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_discover_active_tampered_binary_returns_digest_mismatch() {
        use std::os::unix::fs::PermissionsExt as _;

        // Arrange
        let bytes = b"standalone executable bytes";
        let url = serve_once("200 OK", &[], bytes.to_vec()).await;
        let (registry, signing) = signed_registry(plugin(url.clone(), bytes, "1.2.3"));
        let (installer, config) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        installer
            .accept_registry_revision(temp.path(), &registry)
            .await
            .expect("accept registry state");
        let candidate = installer
            .prepare(temp.path(), &registry, &registry.document.plugins[0])
            .await
            .expect("prepare binary");
        installer.activate(&candidate).await.expect("activate");
        std::fs::set_permissions(
            &candidate.binary_path,
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("make installed binary writable for tampering");
        std::fs::write(&candidate.binary_path, b"tampered executable")
            .expect("tamper installed binary");

        // Act
        let discovered = discover_active(temp.path(), &config).await;

        // Assert
        assert!(matches!(
            &discovered[..],
            [Err(InstallError::DigestMismatch { plugin, version, .. })]
                if plugin == "test-plugin" && version == "1.2.3"
        ));
    }

    #[tokio::test]
    async fn sha_mismatch_does_not_create_install() {
        let bytes = b"tampered";
        let url = serve_once("200 OK", &[], bytes.to_vec()).await;
        let release = plugin(url.clone(), b"expected", "2.0.0");
        let (registry, signing) = signed_registry(release);
        let (installer, _) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        let result = installer
            .prepare(temp.path(), &registry, &registry.document.plugins[0])
            .await;
        assert!(matches!(result, Err(InstallError::DigestMismatch { .. })));
    }

    #[tokio::test]
    async fn advertised_oversized_binary_is_rejected_before_streaming() {
        let url = serve_once(
            "200 OK",
            &[("Content-Length", (MAX_BINARY_BYTES + 1).to_string())],
            Vec::new(),
        )
        .await;
        let (registry, signing) = signed_registry(plugin(url.clone(), b"", "2.0.0"));
        let (installer, _) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            installer
                .prepare(temp.path(), &registry, &registry.document.plugins[0])
                .await,
            Err(InstallError::TooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn redirect_is_not_followed() {
        let url = serve_once(
            "302 Found",
            &[("Location", "http://127.0.0.1:9/attacker".to_string())],
            Vec::new(),
        )
        .await;
        let (registry, signing) = signed_registry(plugin(url.clone(), b"", "2.0.0"));
        let (installer, _) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            installer
                .prepare(temp.path(), &registry, &registry.document.plugins[0])
                .await,
            Err(InstallError::DownloadStatus { status: 302, .. })
        ));
    }

    #[tokio::test]
    async fn same_version_failed_prepare_preserves_active_release() {
        let v1 = b"healthy version";
        let url = serve_once("200 OK", &[], v1.to_vec()).await;
        let (registry, signing) = signed_registry(plugin(url.clone(), v1, "1.0.0"));
        let (installer, config) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        installer
            .accept_registry_revision(temp.path(), &registry)
            .await
            .expect("accept registry state");
        let candidate = installer
            .prepare(temp.path(), &registry, &registry.document.plugins[0])
            .await
            .expect("prepare healthy release");
        installer
            .activate(&candidate)
            .await
            .expect("activate healthy release");
        let active_before =
            std::fs::read(temp.path().join("test-plugin/active.json")).expect("active record");

        let bad_url = serve_once("200 OK", &[], b"corrupt".to_vec()).await;
        let (bad_registry, _) = signed_registry(plugin(bad_url, v1, "1.0.0"));
        assert!(matches!(
            installer
                .prepare(
                    temp.path(),
                    &bad_registry,
                    &bad_registry.document.plugins[0]
                )
                .await,
            Err(InstallError::DigestMismatch { .. })
        ));
        assert_eq!(
            std::fs::read(temp.path().join("test-plugin/active.json")).expect("active record"),
            active_before
        );
        let active = discover_active(temp.path(), &config).await;
        assert!(
            matches!(&active[..], [Ok(found)] if std::fs::read(&found.binary_path).expect("binary") == v1)
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn staging_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let bytes = b"binary";
        let url = serve_once("200 OK", &[], bytes.to_vec()).await;
        let (registry, signing) = signed_registry(plugin(url.clone(), bytes, "1.0.0"));
        let (installer, _) = installer(&url, &signing);
        let temp = tempfile::tempdir().expect("tempdir");
        let plugins = temp.path().join("plugins");
        std::fs::create_dir(&plugins).expect("plugins directory");
        symlink(temp.path(), plugins.join(".staging")).expect("staging symlink");
        assert!(matches!(
            installer
                .prepare(&plugins, &registry, &registry.document.plugins[0])
                .await,
            Err(InstallError::Io { .. })
        ));
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn verified_descriptor_is_executed_even_if_path_is_replaced() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let plugins = temp.path().join("plugins");
        let version = plugins.join("safe/1.0.0-test");
        std::fs::create_dir_all(&version).expect("plugin directories");
        let binary = version.join(BINARY_FILE);
        let trusted = b"#!/bin/sh\nprintf trusted";
        std::fs::write(&binary, trusted).expect("trusted binary");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o500))
            .expect("trusted permissions");
        let digest = hex::encode(Sha256::digest(trusted));
        let executable = open_verified_executable("safe", &plugins, &binary, &digest)
            .await
            .expect("verified executable");
        let command_path = executable.command_path("safe").expect("descriptor path");

        std::fs::rename(&binary, version.join("replaced")).expect("replace trusted pathname");
        std::fs::write(&binary, b"#!/bin/sh\nprintf attacker").expect("replacement binary");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o500))
            .expect("replacement permissions");

        let output = std::process::Command::new(command_path)
            .output()
            .expect("execute verified descriptor");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"trusted");
        drop(executable);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn executable_hardlinks_are_rejected() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let plugins = temp.path().join("plugins");
        let version = plugins.join("safe/1.0.0-test");
        std::fs::create_dir_all(&version).expect("plugin directories");
        let binary = version.join(BINARY_FILE);
        let bytes = b"binary";
        std::fs::write(&binary, bytes).expect("binary");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o500))
            .expect("binary permissions");
        std::fs::hard_link(&binary, version.join("second-link")).expect("hardlink");
        let digest = hex::encode(Sha256::digest(bytes));

        assert!(matches!(
            open_verified_executable("safe", &plugins, &binary, &digest).await,
            Err(InstallError::Io { .. })
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn executable_ancestor_symlinks_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let plugins = temp.path().join("plugins");
        let real = plugins.join("real");
        std::fs::create_dir_all(&real).expect("real directory");
        let binary = real.join(BINARY_FILE);
        let bytes = b"binary";
        std::fs::write(&binary, bytes).expect("binary");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o500))
            .expect("binary permissions");
        symlink(&real, plugins.join("alias")).expect("ancestor symlink");
        let digest = hex::encode(Sha256::digest(bytes));

        assert!(matches!(
            open_verified_executable("safe", &plugins, &plugins.join("alias/plugin"), &digest)
                .await,
            Err(InstallError::Io { .. })
        ));
    }
}
