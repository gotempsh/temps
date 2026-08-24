//! Plugin binary download, checksum-verification, and installation.
//!
//! # Security note — SSRF / RCE guard
//!
//! `fetch_manifest` only accepts a fixed caller-supplied trusted string. It
//! **must never** accept an arbitrary URL from an untrusted HTTP caller.
//! Letting an unauthenticated or insufficiently-privileged caller supply the
//! manifest URL would trivially enable SSRF (reaching internal services) or
//! RCE (installing an attacker-controlled binary). The HTTP handler is
//! responsible for supplying the fixed, compile-time constant URL.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info};
use utoipa::ToSchema;

/// Hard ceiling on a downloaded plugin archive.
///
/// The download is buffered before the checksum can be computed, so without a
/// ceiling a hostile or compromised asset host can exhaust memory on the
/// control plane — the reference deployment is a 3 vCPU / 4 GB box, and this
/// path is reachable from an authenticated HTTP endpoint.
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// Hard ceiling on the *decompressed* plugin binary.
///
/// The SHA-256 in the manifest covers the compressed archive, so it provides
/// no protection against a decompression bomb: a small, correctly-checksummed
/// tarball can expand without limit. This bound is what actually stops that.
const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;

/// Hard ceiling on a registry manifest document.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Everything that can go wrong installing a plugin.
///
/// Typed rather than `anyhow` so the HTTP layer can map each case to a status
/// code by matching the variant. The previous version recovered the error kind
/// by substring-matching the rendered message (`detail.contains("Checksum
/// mismatch")`), which silently degraded to a generic 500 the moment a message
/// was reworded.
#[derive(Debug, Error)]
pub enum InstallError {
    #[error("Unsupported platform: {os} {arch}. Plugin install is available for: macOS (x86_64, aarch64), Linux (x86_64, aarch64)")]
    UnsupportedPlatform { os: String, arch: String },

    #[error("Plugin '{plugin}' v{version} has no release for platform '{platform}'. Available: {available}")]
    NoAssetForPlatform {
        plugin: String,
        version: String,
        platform: String,
        available: String,
    },

    #[error("Refusing to download plugin '{plugin}' asset over a non-HTTPS URL: {url}")]
    InsecureAssetUrl { plugin: String, url: String },

    #[error("{what} from {url} exceeds the {limit}-byte limit")]
    TooLarge {
        what: &'static str,
        url: String,
        limit: u64,
    },

    #[error("Extracted binary '{entry}' from plugin '{plugin}' exceeds the {limit}-byte limit")]
    ExtractedTooLarge {
        plugin: String,
        entry: String,
        limit: u64,
    },

    #[error("Download from {url} returned HTTP {status}")]
    DownloadStatus { url: String, status: u16 },

    #[error("Failed to download from {url}: {reason}")]
    Download { url: String, reason: String },

    #[error("Failed to parse manifest JSON from {url}: {reason}")]
    ManifestParse { url: String, reason: String },

    #[error("Checksum verification failed for plugin '{plugin}' v{version} (platform '{platform}'): {reason}")]
    ChecksumMismatch {
        plugin: String,
        version: String,
        platform: String,
        reason: String,
    },

    #[error("Entry '{entry}' not found in plugin '{plugin}' tarball")]
    EntryNotFound { plugin: String, entry: String },

    #[error("Failed to read plugin '{plugin}' tarball: {reason}")]
    Tarball { plugin: String, reason: String },

    #[error("Failed to write plugin '{plugin}' binary to {path}: {reason}")]
    Write {
        plugin: String,
        path: String,
        reason: String,
    },
}

/// Per-platform download descriptor embedded in a manifest.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct PlatformAsset {
    /// Direct download URL for the `.tar.gz` archive.
    pub url: String,
    /// Expected SHA-256 hex digest of the archive bytes.
    pub sha256: String,
}

/// Registry manifest for a single external plugin.
///
/// JSON shape:
/// ```json
/// {
///   "name": "vibetemps",
///   "version": "1.2.3",
///   "platforms": {
///     "linux-amd64":  { "url": "...", "sha256": "..." },
///     "linux-arm64":  { "url": "...", "sha256": "..." },
///     "darwin-amd64": { "url": "...", "sha256": "..." },
///     "darwin-arm64": { "url": "...", "sha256": "..." }
///   }
/// }
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct PluginRegistryManifest {
    /// Plugin name (e.g. `"vibetemps"`).
    pub name: String,
    /// Semver version string (e.g. `"1.2.3"`).
    pub version: String,
    /// Map from platform key (`"linux-amd64"`, etc.) to download descriptor.
    pub platforms: HashMap<String, PlatformAsset>,
}

/// Installer for external plugin binaries.
///
/// Stateless — all methods are `&self` or free functions; the struct is a
/// namespace for the install flow.
pub struct PluginInstaller;

impl PluginInstaller {
    pub fn new() -> Self {
        Self
    }

    /// Fetch and parse the registry manifest from a trusted, fixed URL.
    ///
    /// # SSRF guard
    ///
    /// The `manifest_url` parameter **must** be a compile-time constant
    /// supplied by the HTTP handler, not a value taken from an incoming
    /// request body. Validating the URL here (instead of in the handler)
    /// would be too late — the caller is responsible for this invariant.
    pub async fn fetch_manifest(
        manifest_url: &str,
    ) -> Result<PluginRegistryManifest, InstallError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| InstallError::Download {
                url: manifest_url.to_string(),
                reason: format!("Failed to build HTTP client: {e}"),
            })?;

        let response = client
            .get(manifest_url)
            .header("User-Agent", "temps-plugin-installer")
            .send()
            .await
            .map_err(|e| InstallError::Download {
                url: manifest_url.to_string(),
                reason: e.to_string(),
            })?;

        if !response.status().is_success() {
            return Err(InstallError::DownloadStatus {
                url: manifest_url.to_string(),
                status: response.status().as_u16(),
            });
        }

        let body = read_body_capped(response, manifest_url, "Manifest", MAX_MANIFEST_BYTES).await?;

        serde_json::from_slice::<PluginRegistryManifest>(&body).map_err(|e| {
            InstallError::ManifestParse {
                url: manifest_url.to_string(),
                reason: e.to_string(),
            }
        })
    }

    /// Download, verify, extract, and atomically install a plugin binary.
    ///
    /// Steps performed in order:
    /// 1. Resolve the current OS/arch to a platform key.
    /// 2. Look up the platform asset in `manifest.platforms`.
    /// 3. Download the tarball.
    /// 4. Verify the SHA-256 checksum — aborts **before** touching disk on mismatch.
    /// 5. Extract the plugin binary from the tarball (entry named `binary_name`).
    /// 6. Write to `plugins_dir/<binary_name>` via temp-file → atomic rename.
    ///
    /// Returns the path of the installed binary on success.
    ///
    /// Note: this method is a pure filesystem operation. Calling
    /// `ExternalPluginManager::reload_plugin` after this returns is the
    /// caller's responsibility.
    pub async fn install(
        &self,
        binary_name: &str,
        manifest: &PluginRegistryManifest,
        plugins_dir: &Path,
    ) -> Result<PathBuf, InstallError> {
        // 1. Resolve platform key
        let platform_key = platform_target()?;

        // 2. Look up platform asset
        let asset = manifest.platforms.get(&platform_key).ok_or_else(|| {
            let mut available: Vec<&str> = manifest.platforms.keys().map(String::as_str).collect();
            available.sort_unstable();
            InstallError::NoAssetForPlatform {
                plugin: manifest.name.clone(),
                version: manifest.version.clone(),
                platform: platform_key.clone(),
                available: available.join(", "),
            }
        })?;

        // The manifest is the trust root, but it is still remote data: a
        // plaintext asset URL would let a network attacker swap the archive,
        // and the digest that would catch it travels over the same channel.
        if !asset.url.starts_with("https://") {
            return Err(InstallError::InsecureAssetUrl {
                plugin: manifest.name.clone(),
                url: asset.url.clone(),
            });
        }

        info!(
            plugin = %manifest.name,
            version = %manifest.version,
            platform = %platform_key,
            url = %asset.url,
            "Downloading plugin binary",
        );

        // 3. Download tarball (bounded — see MAX_ARCHIVE_BYTES)
        let bytes = download_asset(&asset.url, MAX_ARCHIVE_BYTES).await?;

        debug!(plugin = %manifest.name, bytes = bytes.len(), "Download complete, verifying checksum");

        // 4. Verify checksum BEFORE writing anything to disk.
        //    The checksum_text format expected by verify_checksum is
        //    "<hex>  <filename>" (sha256sum style). We pass the binary_name
        //    as the filename component — it is ignored by the parser, only the
        //    hash token matters.
        let checksum_text = format!("{}  {}", asset.sha256, binary_name);
        temps_core::checksum::verify_checksum(&bytes, &checksum_text).map_err(|e| {
            InstallError::ChecksumMismatch {
                plugin: manifest.name.clone(),
                version: manifest.version.clone(),
                platform: platform_key.clone(),
                reason: e.to_string(),
            }
        })?;

        debug!(plugin = %manifest.name, "Checksum verified, extracting binary from tarball");

        // 5. Extract binary from tarball (bounded — see MAX_EXTRACTED_BYTES)
        let binary_bytes = extract_binary_from_tarball(&bytes, binary_name, MAX_EXTRACTED_BYTES)
            .map_err(|e| match e {
                ExtractError::NotFound => InstallError::EntryNotFound {
                    plugin: manifest.name.clone(),
                    entry: binary_name.to_string(),
                },
                ExtractError::TooLarge => InstallError::ExtractedTooLarge {
                    plugin: manifest.name.clone(),
                    entry: binary_name.to_string(),
                    limit: MAX_EXTRACTED_BYTES,
                },
                ExtractError::Io(io) => InstallError::Tarball {
                    plugin: manifest.name.clone(),
                    reason: io.to_string(),
                },
            })?;

        // Ensure the plugins directory exists
        tokio::fs::create_dir_all(plugins_dir)
            .await
            .map_err(|e| InstallError::Write {
                plugin: manifest.name.clone(),
                path: plugins_dir.display().to_string(),
                reason: format!("Failed to create plugins directory: {e}"),
            })?;

        let dest_path = plugins_dir.join(binary_name);

        // 6. Atomic temp-file → rename install
        write_binary_atomically(&dest_path, &binary_bytes).map_err(|e| InstallError::Write {
            plugin: manifest.name.clone(),
            path: dest_path.display().to_string(),
            reason: e.to_string(),
        })?;

        info!(
            plugin = %manifest.name,
            version = %manifest.version,
            path = %dest_path.display(),
            "Plugin binary installed successfully",
        );

        Ok(dest_path)
    }
}

impl Default for PluginInstaller {
    fn default() -> Self {
        Self::new()
    }
}

/// Map the current OS/architecture to a platform key used in plugin manifests.
///
/// Returns one of: `"linux-amd64"`, `"linux-arm64"`, `"darwin-amd64"`,
/// `"darwin-arm64"`. Returns an error on any unsupported platform.
pub fn platform_target() -> Result<String, InstallError> {
    use std::env::consts::{ARCH, OS};
    let key = match (OS, ARCH) {
        ("macos", "x86_64") => "darwin-amd64",
        ("macos", "aarch64") => "darwin-arm64",
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        _ => {
            return Err(InstallError::UnsupportedPlatform {
                os: OS.to_string(),
                arch: ARCH.to_string(),
            });
        }
    };
    Ok(key.to_string())
}

/// Download a URL and return the raw bytes, refusing anything over `limit`.
async fn download_asset(url: &str, limit: u64) -> Result<Vec<u8>, InstallError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| InstallError::Download {
            url: url.to_string(),
            reason: format!("Failed to build HTTP client: {e}"),
        })?;

    let response = client
        .get(url)
        .header("User-Agent", "temps-plugin-installer")
        .send()
        .await
        .map_err(|e| InstallError::Download {
            url: url.to_string(),
            reason: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(InstallError::DownloadStatus {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }

    read_body_capped(response, url, "Download", limit).await
}

/// Read a response body into memory, aborting as soon as `limit` is exceeded.
///
/// Streams chunk-by-chunk rather than calling `Response::bytes()`, which
/// buffers the whole body regardless of size. The advertised `Content-Length`
/// is rejected up front when it is already over the limit, so an honest
/// oversized body costs one round trip instead of a full transfer — but the
/// running total is still enforced, because that header is attacker-controlled
/// and may be absent (chunked encoding) or a lie.
pub(crate) async fn read_body_capped(
    response: reqwest::Response,
    url: &str,
    what: &'static str,
    limit: u64,
) -> Result<Vec<u8>, InstallError> {
    let too_large = || InstallError::TooLarge {
        what,
        url: url.to_string(),
        limit,
    };

    if response.content_length().is_some_and(|len| len > limit) {
        return Err(too_large());
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| InstallError::Download {
            url: url.to_string(),
            reason: e.to_string(),
        })?;
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Extract a single named entry from a gzip-compressed tar archive.
///
/// `entry_name` is matched against the archive entry's file name (the last
/// path component), not the full path inside the archive.
fn extract_binary_from_tarball(
    tarball_bytes: &[u8],
    entry_name: &str,
    limit: u64,
) -> Result<Vec<u8>, ExtractError> {
    let decoder = GzDecoder::new(tarball_bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries().map_err(ExtractError::Io)? {
        let entry = entry.map_err(ExtractError::Io)?;
        let path = entry.path().map_err(ExtractError::Io)?;

        if path.file_name().map(|n| n == entry_name) != Some(true) {
            continue;
        }

        // `take(limit + 1)` rather than `read_to_end`: the manifest's SHA-256
        // covers the *compressed* archive, so a validly-checksummed tarball
        // can still expand without bound. Reading one byte past the limit is
        // what distinguishes "exactly at the limit" from "over it".
        let mut buf = Vec::new();
        entry
            .take(limit + 1)
            .read_to_end(&mut buf)
            .map_err(ExtractError::Io)?;
        if buf.len() as u64 > limit {
            return Err(ExtractError::TooLarge);
        }
        return Ok(buf);
    }

    Err(ExtractError::NotFound)
}

/// Internal failure modes of tarball extraction, mapped to `InstallError` by
/// the caller (which knows the plugin name and entry for the message).
#[derive(Debug, Error)]
enum ExtractError {
    #[error("{0}")]
    Io(std::io::Error),
    #[error("entry not found")]
    NotFound,
    #[error("entry exceeds the size limit")]
    TooLarge,
}

/// Write `binary_bytes` to `dest` atomically by writing to a sibling temp
/// file first and then renaming. Sets the executable bit before the rename.
fn write_binary_atomically(dest: &Path, binary_bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let parent = dest.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "destination path {} has no parent directory",
                dest.display()
            ),
        )
    })?;

    // Use a uniquely-named temp file in the same directory so the rename is
    // guaranteed to be on the same filesystem (required for atomicity).
    let mut tmp_file = tempfile::NamedTempFile::new_in(parent)?;

    tmp_file.write_all(binary_bytes)?;

    // Set executable bit before rename
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tmp_file.as_file().metadata()?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        tmp_file.as_file().set_permissions(perms)?;
    }

    // Atomic rename — replaces dest if it already exists
    tmp_file.persist(dest).map_err(|e| e.error)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_target_returns_supported_key() {
        // Running on the CI host — whatever platform this is must be one we support.
        let result = platform_target();
        assert!(result.is_ok(), "platform_target() failed: {:?}", result);
        let key = result.unwrap();
        assert!(
            ["linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64"].contains(&key.as_str()),
            "Unexpected platform key: {key}"
        );
    }

    #[test]
    fn manifest_deserializes_correctly() {
        let json = r#"{
            "name": "vibetemps",
            "version": "1.2.3",
            "platforms": {
                "linux-amd64": { "url": "https://example.com/v.tar.gz", "sha256": "abc123" },
                "darwin-arm64": { "url": "https://example.com/d.tar.gz", "sha256": "def456" }
            }
        }"#;
        let manifest: PluginRegistryManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "vibetemps");
        assert_eq!(manifest.version, "1.2.3");
        assert!(manifest.platforms.contains_key("linux-amd64"));
        assert_eq!(manifest.platforms["darwin-arm64"].sha256, "def456");
    }

    #[test]
    fn manifest_serializes_round_trip() {
        let manifest = PluginRegistryManifest {
            name: "myplugin".to_string(),
            version: "0.1.0".to_string(),
            platforms: {
                let mut m = HashMap::new();
                m.insert(
                    "linux-amd64".to_string(),
                    PlatformAsset {
                        url: "https://example.com/p.tar.gz".to_string(),
                        sha256: "deadbeef".to_string(),
                    },
                );
                m
            },
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginRegistryManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, manifest.name);
        assert_eq!(back.version, manifest.version);
        assert_eq!(back.platforms["linux-amd64"].sha256, "deadbeef");
    }

    #[test]
    fn extract_binary_not_found_returns_error() {
        // Build a minimal valid gzip+tar with a single entry named "other"
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut gz_buf = Vec::new();
        {
            let enc = GzEncoder::new(&mut gz_buf, Compression::default());
            let mut tar = tar::Builder::new(enc);
            let data = b"binary data here";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "other-binary", data.as_ref())
                .unwrap();
            tar.finish().unwrap();
        }

        let result = extract_binary_from_tarball(&gz_buf, "mybin", MAX_EXTRACTED_BYTES);
        assert!(matches!(result, Err(ExtractError::NotFound)));
    }

    /// A gzip bomb passes the manifest's SHA-256 — that digest covers the
    /// compressed bytes — so the extraction bound is the only thing standing
    /// between a compromised registry and an OOM.
    #[test]
    fn extract_binary_rejects_oversized_entry() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let limit = 1024u64;
        let payload = vec![0u8; (limit as usize) + 1];
        let mut gz_buf = Vec::new();
        {
            let enc = GzEncoder::new(&mut gz_buf, Compression::default());
            let mut tar = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "mybin", payload.as_slice())
                .unwrap();
            tar.finish().unwrap();
        }

        // Highly compressible, so the archive itself is far under the limit —
        // the expansion is what has to be caught.
        assert!((gz_buf.len() as u64) < limit, "archive must be small");
        let result = extract_binary_from_tarball(&gz_buf, "mybin", limit);
        assert!(matches!(result, Err(ExtractError::TooLarge)));
    }

    #[test]
    fn extract_binary_accepts_entry_exactly_at_limit() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let limit = 1024u64;
        let payload = vec![7u8; limit as usize];
        let mut gz_buf = Vec::new();
        {
            let enc = GzEncoder::new(&mut gz_buf, Compression::default());
            let mut tar = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "mybin", payload.as_slice())
                .unwrap();
            tar.finish().unwrap();
        }

        let out = extract_binary_from_tarball(&gz_buf, "mybin", limit).unwrap();
        assert_eq!(
            out.len(),
            limit as usize,
            "the limit itself must be allowed"
        );
    }

    #[test]
    fn extract_binary_found_returns_bytes() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let expected = b"the real binary";
        let mut gz_buf = Vec::new();
        {
            let enc = GzEncoder::new(&mut gz_buf, Compression::default());
            let mut tar = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(expected.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "mybin", expected.as_ref())
                .unwrap();
            tar.finish().unwrap();
        }

        let result = extract_binary_from_tarball(&gz_buf, "mybin", MAX_EXTRACTED_BYTES).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn write_binary_atomically_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("my-plugin");
        write_binary_atomically(&dest, b"plugin-binary-bytes").unwrap();
        assert!(dest.exists());
        let contents = std::fs::read(&dest).unwrap();
        assert_eq!(contents, b"plugin-binary-bytes");
    }

    #[test]
    #[cfg(unix)]
    fn write_binary_atomically_sets_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("executable-plugin");
        write_binary_atomically(&dest, b"data").unwrap();
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "Executable bit should be set: mode={mode:#o}"
        );
    }

    #[tokio::test]
    async fn install_fails_on_unsupported_platform_in_manifest() {
        let manifest = PluginRegistryManifest {
            name: "testplugin".to_string(),
            version: "0.0.1".to_string(),
            platforms: HashMap::new(), // empty — no platform matches
        };
        let tmp = tempfile::tempdir().unwrap();
        let installer = PluginInstaller::new();
        let result = installer
            .install("testplugin-bin", &manifest, tmp.path())
            .await;
        assert!(matches!(
            result,
            Err(InstallError::NoAssetForPlatform { ref plugin, .. }) if plugin == "testplugin"
        ));
    }

    /// The digest that would catch a swapped archive travels over the same
    /// connection as the archive, so plaintext transport defeats it.
    #[tokio::test]
    async fn install_rejects_plaintext_asset_url() {
        let platform = platform_target().unwrap();
        let manifest = PluginRegistryManifest {
            name: "testplugin".to_string(),
            version: "0.0.1".to_string(),
            platforms: HashMap::from([(
                platform,
                PlatformAsset {
                    url: "http://example.com/p.tar.gz".to_string(),
                    sha256: "deadbeef".to_string(),
                },
            )]),
        };
        let tmp = tempfile::tempdir().unwrap();
        let result = PluginInstaller::new()
            .install("testplugin-bin", &manifest, tmp.path())
            .await;
        assert!(
            matches!(result, Err(InstallError::InsecureAssetUrl { .. })),
            "a plaintext asset URL must be refused before any download"
        );
    }
}
