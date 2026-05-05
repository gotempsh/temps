use clap::{Args, ValueEnum};
use serde::Deserialize;
use std::env::consts::{ARCH, OS};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tracing::{debug, info};

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/gotempsh/temps/releases";

/// Release channel the upgrader subscribes to. The picker filters all
/// available GitHub releases through this channel before selecting the
/// newest, so a host on `Stable` never auto-upgrades onto a beta tag.
/// Pre-release tags carry a `-` (`v1.2.0-beta.4`, `v1.2.0-rc.1`); stable
/// tags don't (`v1.2.0`).
///
/// Channel selection is **CLI-only** — there is no env-var fallback. The
/// default is `Stable` and the user must explicitly pass `--channel beta`
/// to opt into prereleases. This is by design: an env var on a long-lived
/// shell or CI runner could silently switch a host onto beta without an
/// audit trail, which we want to prevent. Pinning a specific `--version`
/// ignores the channel entirely.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum UpgradeChannel {
    /// Track stable releases only (default). Tag must NOT contain `-`.
    Stable,
    /// Track beta releases. Includes any prerelease tag (anything with `-`).
    /// `Beta` selects the newest of stable + beta, so a beta host receives
    /// stable releases too — they're considered an upgrade from the latest
    /// beta on the same line.
    Beta,
}

impl UpgradeChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }

    /// Does this release belong to this channel?
    /// - Stable: only non-prerelease tags. `v1.2.0` matches; `v1.2.0-beta.4` does not.
    /// - Beta: any non-draft tag. Both stable AND beta releases qualify, so
    ///   a beta host always sees the freshest available version regardless of
    ///   whether it was promoted to stable or not.
    fn includes(self, release: &GitHubRelease) -> bool {
        if release.draft {
            return false;
        }
        match self {
            Self::Stable => !release.prerelease,
            Self::Beta => true,
        }
    }
}

/// Self-upgrade temps to the latest version
#[derive(Args)]
pub struct UpgradeCommand {
    /// Release channel to track. Default: `stable`. Pass `--channel beta`
    /// to opt into prereleases. Pinning a `--version` ignores the channel.
    #[arg(long, value_enum)]
    pub channel: Option<UpgradeChannel>,

    /// Target version to upgrade to (e.g. "v1.2.0"). Defaults to latest.
    #[arg(long)]
    pub version: Option<String>,

    /// Path to the temps binary to replace. Defaults to the currently running binary.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// Skip confirmation prompt
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Only check for updates, don't install
    #[arg(long)]
    pub check: bool,

    /// DEPRECATED: alias for `--channel stable`. Kept for backward compat
    /// with existing scripts; will be removed in a future release. New
    /// callers should use `--channel stable` (or just omit the flag — it's
    /// the default).
    #[arg(long, hide = true)]
    pub stable: bool,
}

#[derive(Clone, Deserialize, Debug)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<GitHubAsset>,
    pub html_url: String,
}

#[derive(Clone, Deserialize, Debug)]
pub struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

impl UpgradeCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.run())
    }

    /// Resolve the effective channel. CLI-only by design — no env-var
    /// fallback so a host can never auto-switch onto beta without an
    /// explicit `--channel` invocation.
    /// Precedence:
    ///   1. `--channel <X>` flag wins
    ///   2. legacy `--stable` alias selects Stable
    ///   3. default: Stable
    fn resolved_channel(&self) -> UpgradeChannel {
        if let Some(c) = self.channel {
            return c;
        }
        if self.stable {
            return UpgradeChannel::Stable;
        }
        UpgradeChannel::Stable
    }

    async fn run(self) -> anyhow::Result<()> {
        // Determine the binary path to upgrade
        let binary_path = match &self.path {
            Some(p) => p.clone(),
            None => std::env::current_exe()
                .map_err(|e| anyhow::anyhow!("Failed to determine current binary path: {}", e))?,
        };

        // Resolve symlinks to get the actual binary path
        let binary_path = fs::canonicalize(&binary_path).unwrap_or(binary_path);

        info!("Binary path: {}", binary_path.display());

        // Get current version (the git tag portion only)
        let current_version = current_version_tag();
        info!("Current version: {}", current_version);

        // Determine platform target
        let target = platform_target()?;
        debug!("Detected platform target: {}", target);

        // Resolve channel before any network call so log output reflects
        // the actual subscription. Pinning a `--version` ignores channel.
        let channel = self.resolved_channel();

        // Fetch release info
        let release = if let Some(ref version) = self.version {
            info!("Fetching release {}...", version);
            fetch_specific_release(version).await?
        } else {
            info!(
                "Checking for latest release on '{}' channel...",
                channel.as_str()
            );
            fetch_latest_release_in_channel(channel).await?
        };

        let latest_version = &release.tag_name;
        info!("Latest version: {}", latest_version);

        // Compare versions
        if latest_version == &current_version && self.version.is_none() {
            println!("Already up to date ({})", current_version);
            return Ok(());
        }

        if latest_version == &current_version {
            println!("Already on version {}", current_version);
            return Ok(());
        }

        // Find the matching asset
        let tarball_name = format!("temps-{}.tar.gz", target);
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == tarball_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No release asset found for platform '{}'. Available assets: {}",
                    target,
                    release
                        .assets
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;

        let size_mb = asset.size as f64 / 1_048_576.0;

        // Display upgrade plan. Echo the channel so the operator can
        // confirm at a glance whether this run is subscribed to stable or
        // beta — easier than scraping logs after the fact.
        let prerelease_label = if release.prerelease {
            " (prerelease)"
        } else {
            ""
        };
        println!();
        println!("  Upgrade available:");
        println!(
            "    {} -> {}{}",
            current_version, latest_version, prerelease_label
        );
        println!("    Channel:  {}", channel.as_str());
        println!("    Platform: {}", target);
        println!("    Binary:   {}", binary_path.display());
        println!("    Size:     {:.1} MB", size_mb);
        println!("    Release:  {}", release.html_url);
        println!();

        if self.check {
            println!("Run `temps upgrade` to install this update.");
            return Ok(());
        }

        // Confirm unless --yes
        if !self.yes {
            print!("  Proceed with upgrade? [y/N] ");
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let input = input.trim().to_lowercase();

            if input != "y" && input != "yes" {
                println!("  Upgrade cancelled.");
                return Ok(());
            }
        }

        // Check write permissions before downloading
        check_write_permission(&binary_path)?;

        // Download the tarball
        println!("  Downloading {}...", tarball_name);
        let tarball_bytes = download_asset(&asset.browser_download_url).await?;

        // Also download the checksum
        let checksum_name = format!("{}.sha256", tarball_name);
        let checksum_asset = release.assets.iter().find(|a| a.name == checksum_name);

        if let Some(checksum_asset) = checksum_asset {
            debug!("Verifying checksum...");
            let checksum_text = download_asset_text(&checksum_asset.browser_download_url).await?;
            verify_checksum(&tarball_bytes, &checksum_text)?;
            println!("  Checksum verified.");
        } else {
            debug!("No checksum asset found, skipping verification");
        }

        // Extract the binary from the tarball
        println!("  Extracting binary...");
        let new_binary = extract_binary_from_tarball(&tarball_bytes)?;

        // Replace the binary atomically
        println!("  Replacing binary at {}...", binary_path.display());
        replace_binary(&binary_path, &new_binary)?;

        println!();
        println!(
            "  Successfully upgraded temps {} -> {}",
            current_version, latest_version
        );
        println!("  Run `temps --version` to verify.");

        Ok(())
    }
}

/// Extract the clean version tag from the compiled TEMPS_VERSION string.
/// TEMPS_VERSION format: "v1.0.0 (abc1234) built 2025-01-25 12:34:56 UTC"
/// or: "v1.0.0-abc1234 built 2025-01-25 12:34:56 UTC"
pub fn current_version_tag() -> String {
    let full_version = env!("TEMPS_VERSION");

    // If it contains a space, take everything before the first space
    // Then strip any "-commitsha" suffix (non-tag builds)
    let version = full_version
        .split_whitespace()
        .next()
        .unwrap_or(full_version);

    // For "v1.0.0-abc1234" (not on a tag), strip the commit hash suffix
    // A tag looks like "v1.0.0" or "v1.0.0-beta.1", a non-tag looks like "v1.0.0-abc1234"
    // We identify commit hashes as short hex strings after the last dash
    if let Some(last_dash_pos) = version.rfind('-') {
        let suffix = &version[last_dash_pos + 1..];
        // If suffix looks like a commit hash (all hex, 7-12 chars), strip it
        if suffix.len() >= 7 && suffix.len() <= 12 && suffix.chars().all(|c| c.is_ascii_hexdigit())
        {
            return version[..last_dash_pos].to_string();
        }
    }

    version.to_string()
}

/// Determine the platform target string matching release asset names.
fn platform_target() -> anyhow::Result<String> {
    let target = match (OS, ARCH) {
        ("macos", "x86_64") => "darwin-amd64",
        ("macos", "aarch64") => "darwin-arm64",
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        _ => {
            return Err(anyhow::anyhow!(
                "Unsupported platform: {} {}. Self-upgrade is available for: \
                 macOS (x86_64, aarch64), Linux (x86_64, aarch64)",
                OS,
                ARCH
            ));
        }
    };
    Ok(target.to_string())
}

/// Fetch the latest release on a given channel from GitHub.
///
/// Pulls the first page of releases (per_page=20, GitHub's default ordering
/// is most-recent-first) and returns the first one that belongs to the
/// requested channel. 20 is enough to find the newest stable even on a
/// project that ships many betas between stables.
///
/// Note: this returns the channel's *newest* release, which may be older
/// than the absolute newest tag — that's the point. A `Stable` host on a
/// project actively shipping `vX.Y.Z-beta.N` should ignore those betas.
pub async fn fetch_latest_release_in_channel(
    channel: UpgradeChannel,
) -> anyhow::Result<GitHubRelease> {
    let client = reqwest::Client::new();
    let url = format!("{}?per_page=20", GITHUB_RELEASES_API);
    let response = client
        .get(&url)
        .header("User-Agent", "temps-self-upgrade")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch releases: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "GitHub API returned {} when fetching releases: {}",
            status,
            body
        ));
    }

    let releases: Vec<GitHubRelease> = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse releases response: {}", e))?;

    pick_release_for_channel(releases, channel).ok_or_else(|| match channel {
        UpgradeChannel::Stable => anyhow::anyhow!(
            "No stable releases found. Try `--channel beta` to include prereleases."
        ),
        UpgradeChannel::Beta => anyhow::anyhow!("No releases found."),
    })
}

/// Pure picker — split out so tests can drive it without an HTTP mock.
fn pick_release_for_channel(
    releases: Vec<GitHubRelease>,
    channel: UpgradeChannel,
) -> Option<GitHubRelease> {
    releases.into_iter().find(|r| channel.includes(r))
}

/// Fetch a specific release by tag from GitHub.
async fn fetch_specific_release(version: &str) -> anyhow::Result<GitHubRelease> {
    // Ensure the version starts with 'v'
    let tag = if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{}", version)
    };

    let url = format!(
        "https://api.github.com/repos/gotempsh/temps/releases/tags/{}",
        tag
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "temps-self-upgrade")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch release {}: {}", tag, e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow::anyhow!("Release '{}' not found", tag));
        }
        return Err(anyhow::anyhow!(
            "GitHub API returned {} when fetching release {}: {}",
            status,
            tag,
            body
        ));
    }

    response
        .json::<GitHubRelease>()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse release response: {}", e))
}

/// Download a release asset as bytes.
async fn download_asset(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "temps-self-upgrade")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to download asset: {}", e))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to download asset: HTTP {}",
            response.status()
        ));
    }

    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| anyhow::anyhow!("Failed to read download response: {}", e))
}

/// Download a release asset as text (for checksums).
async fn download_asset_text(url: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "temps-self-upgrade")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to download checksum: {}", e))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to download checksum: HTTP {}",
            response.status()
        ));
    }

    response
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read checksum response: {}", e))
}

/// Verify SHA256 checksum of downloaded data.
fn verify_checksum(data: &[u8], checksum_text: &str) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(data);
    let computed = format!("{:x}", hasher.finalize());

    // Checksum file format: "<hash>  <filename>" or "<hash> <filename>"
    let expected = checksum_text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid checksum file format"))?
        .to_lowercase();

    if computed != expected {
        return Err(anyhow::anyhow!(
            "Checksum mismatch!\n  Expected: {}\n  Got:      {}",
            expected,
            computed
        ));
    }

    Ok(())
}

/// Extract the `temps` binary from a gzipped tarball.
fn extract_binary_from_tarball(tarball_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let decoder = GzDecoder::new(tarball_bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        if path.file_name().map(|n| n == "temps").unwrap_or(false) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }

    Err(anyhow::anyhow!(
        "Binary 'temps' not found in the downloaded tarball"
    ))
}

/// Check we have write permission to the binary path.
fn check_write_permission(binary_path: &PathBuf) -> anyhow::Result<()> {
    // Check the parent directory is writable (for atomic rename)
    let parent = binary_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine parent directory of binary"))?;

    let md = fs::metadata(parent)
        .map_err(|e| anyhow::anyhow!("Cannot access directory {}: {}", parent.display(), e))?;

    if md.permissions().readonly() {
        return Err(anyhow::anyhow!(
            "No write permission to {}. You may need to run with sudo.",
            parent.display()
        ));
    }

    // Also check the file itself (if it exists)
    if binary_path.exists() {
        let file_md = fs::metadata(binary_path).map_err(|e| {
            anyhow::anyhow!("Cannot access binary at {}: {}", binary_path.display(), e)
        })?;

        if file_md.permissions().readonly() {
            return Err(anyhow::anyhow!(
                "Binary at {} is read-only. You may need to run with sudo.",
                binary_path.display()
            ));
        }
    }

    Ok(())
}

/// Replace the binary using an atomic rename strategy:
/// 1. Write new binary to a temp file next to the target
/// 2. Set executable permissions
/// 3. Rename temp file over the target (atomic on the same filesystem)
fn replace_binary(binary_path: &PathBuf, new_binary: &[u8]) -> anyhow::Result<()> {
    let parent = binary_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine parent directory"))?;

    let tmp_path = parent.join(".temps-upgrade-tmp");

    // Write the new binary to temp file
    fs::write(&tmp_path, new_binary)
        .map_err(|e| anyhow::anyhow!("Failed to write temporary file: {}", e))?;

    // Set executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&tmp_path, perms)
            .map_err(|e| anyhow::anyhow!("Failed to set executable permissions: {}", e))?;
    }

    // Atomic rename
    fs::rename(&tmp_path, binary_path).map_err(|e| {
        // Clean up temp file on failure
        let _ = fs::remove_file(&tmp_path);
        anyhow::anyhow!("Failed to replace binary: {}", e)
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_version_tag_exact_tag() {
        // The function uses env!() so we test the parsing logic directly
        // For a tagged build "v1.0.0 (abc1234) built ...", it should return "v1.0.0"
        let version = parse_version_tag("v1.0.0 (abc1234) built 2025-01-25 12:34:56 UTC");
        assert_eq!(version, "v1.0.0");
    }

    #[test]
    fn test_current_version_tag_non_tag_build() {
        // For "v0.1.0-abc1234 built ...", it should strip the commit hash
        let version = parse_version_tag("v0.1.0-abc1234 built 2025-01-25 12:34:56 UTC");
        assert_eq!(version, "v0.1.0");
    }

    #[test]
    fn test_current_version_tag_prerelease() {
        // For "v1.0.0-beta.1 (abc1234) built ...", the suffix is NOT a commit hash
        let version = parse_version_tag("v1.0.0-beta.1 (abc1234) built 2025-01-25 12:34:56 UTC");
        assert_eq!(version, "v1.0.0-beta.1");
    }

    #[test]
    fn test_current_version_tag_simple() {
        let version = parse_version_tag("v2.3.4");
        assert_eq!(version, "v2.3.4");
    }

    #[test]
    fn test_platform_target() {
        // Just verify it doesn't panic on the current platform
        let result = platform_target();
        assert!(
            result.is_ok(),
            "platform_target() should succeed on supported platforms"
        );
        let target = result.unwrap();
        assert!(
            ["darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64"]
                .contains(&target.as_str()),
            "Unexpected target: {}",
            target
        );
    }

    #[test]
    fn test_verify_checksum_valid() {
        use sha2::{Digest, Sha256};

        let data = b"hello world";
        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash = format!("{:x}", hasher.finalize());

        let checksum_text = format!("{}  temps-darwin-arm64.tar.gz", hash);
        let result = verify_checksum(data, &checksum_text);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_checksum_invalid() {
        let data = b"hello world";
        let checksum_text =
            "0000000000000000000000000000000000000000000000000000000000000000  temps.tar.gz";
        let result = verify_checksum(data, checksum_text);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Checksum mismatch"));
    }

    #[test]
    fn test_verify_checksum_bad_format() {
        let data = b"hello world";
        let checksum_text = "";
        let result = verify_checksum(data, checksum_text);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_binary_from_tarball() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        // Create a tarball with a "temps" binary
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let binary_content = b"fake-binary-content";
            let mut header = tar::Header::new_gnu();
            header.set_size(binary_content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "temps", &binary_content[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let tarball = encoder.finish().unwrap();

        let result = extract_binary_from_tarball(&tarball);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"fake-binary-content");
    }

    #[test]
    fn test_extract_binary_from_tarball_not_found() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        // Create a tarball without a "temps" binary
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let content = b"not-temps";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "other-file", &content[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let tarball = encoder.finish().unwrap();

        let result = extract_binary_from_tarball(&tarball);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    /// Helper to parse version tag from a TEMPS_VERSION-like string,
    /// replicating the logic from `current_version_tag()` but on arbitrary input.
    fn parse_version_tag(full_version: &str) -> String {
        let version = full_version
            .split_whitespace()
            .next()
            .unwrap_or(full_version);

        if let Some(last_dash_pos) = version.rfind('-') {
            let suffix = &version[last_dash_pos + 1..];
            if suffix.len() >= 7
                && suffix.len() <= 12
                && suffix.chars().all(|c| c.is_ascii_hexdigit())
            {
                return version[..last_dash_pos].to_string();
            }
        }

        version.to_string()
    }

    // ── Channel logic ─────────────────────────────────────────────────────
    //
    // The release picker is the contract that determines what `temps
    // upgrade` actually does. Each test below pins one rule of that
    // contract so a future refactor can't silently change behavior.

    fn release(tag: &str, prerelease: bool, draft: bool) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_string(),
            prerelease,
            draft,
            assets: vec![],
            html_url: String::new(),
        }
    }

    #[test]
    fn channel_includes_only_non_prerelease_for_stable() {
        // Stable must reject any prerelease tag, even if it's newer.
        // This is the property that protects stable hosts from auto-
        // upgrading onto a beta line.
        let stable = release("v1.2.0", false, false);
        let beta = release("v1.3.0-beta.1", true, false);
        let draft = release("v1.4.0", false, true);

        assert!(UpgradeChannel::Stable.includes(&stable));
        assert!(!UpgradeChannel::Stable.includes(&beta));
        assert!(!UpgradeChannel::Stable.includes(&draft));
    }

    #[test]
    fn channel_includes_both_kinds_for_beta() {
        // Beta sees both stable and beta releases — a beta host should
        // upgrade to a fresh stable when one ships, not stay stuck on
        // the latest beta. Drafts are never visible.
        let stable = release("v1.2.0", false, false);
        let beta = release("v1.3.0-beta.1", true, false);
        let draft = release("v1.4.0-beta.2", true, true);

        assert!(UpgradeChannel::Beta.includes(&stable));
        assert!(UpgradeChannel::Beta.includes(&beta));
        assert!(!UpgradeChannel::Beta.includes(&draft));
    }

    #[test]
    fn picker_returns_first_matching_in_response_order() {
        // GitHub returns releases newest-first. Picker takes the first
        // match, which is the newest release on that channel. We trust
        // GitHub's ordering here — re-sorting by semver locally would
        // also have to handle prerelease ordering correctly, and we'd
        // rather lean on GitHub than reimplement it.
        let releases = vec![
            release("v1.3.0-beta.2", true, false), // newest, beta
            release("v1.3.0-beta.1", true, false),
            release("v1.2.0", false, false), // newest stable
            release("v1.1.0", false, false),
        ];

        let picked_stable = pick_release_for_channel(releases.clone(), UpgradeChannel::Stable);
        assert_eq!(
            picked_stable.expect("stable should match v1.2.0").tag_name,
            "v1.2.0"
        );

        let picked_beta = pick_release_for_channel(releases, UpgradeChannel::Beta);
        assert_eq!(
            picked_beta
                .expect("beta should match v1.3.0-beta.2")
                .tag_name,
            "v1.3.0-beta.2"
        );
    }

    #[test]
    fn picker_skips_drafts() {
        // A draft should never be selected even if it's the newest entry,
        // because users can't actually download a draft release's assets.
        let releases = vec![
            release("v2.0.0", false, true), // draft, ignored
            release("v1.9.0", false, false),
        ];
        let picked = pick_release_for_channel(releases, UpgradeChannel::Stable);
        assert_eq!(
            picked.expect("should fall through to v1.9.0").tag_name,
            "v1.9.0"
        );
    }

    #[test]
    fn picker_returns_none_when_no_release_in_channel() {
        // If every available release is a prerelease, a Stable picker
        // returns None. The caller is responsible for surfacing a
        // helpful error pointing the user at `--channel beta`.
        let releases = vec![
            release("v1.0.0-beta.1", true, false),
            release("v1.0.0-beta.2", true, false),
        ];
        let picked = pick_release_for_channel(releases, UpgradeChannel::Stable);
        assert!(picked.is_none());
    }

    #[test]
    fn resolved_channel_defaults_to_stable() {
        // CLI-only design: with no flags set, the user always lands on
        // stable. No env var or implicit state can change this. This is
        // the contract operators rely on — running `temps upgrade` on a
        // fresh shell never lands them on a beta build.
        let cmd = UpgradeCommand {
            channel: None,
            version: None,
            path: None,
            yes: false,
            check: false,
            stable: false,
        };
        assert_eq!(cmd.resolved_channel(), UpgradeChannel::Stable);
    }

    #[test]
    fn resolved_channel_legacy_stable_flag_selects_stable() {
        // The legacy `--stable` flag is now a no-op (Stable is already
        // default), but we accept it for backward compat with existing
        // CI scripts. Verify it doesn't somehow yield Beta.
        let cmd = UpgradeCommand {
            channel: None,
            version: None,
            path: None,
            yes: false,
            check: false,
            stable: true,
        };
        assert_eq!(cmd.resolved_channel(), UpgradeChannel::Stable);
    }

    #[test]
    fn resolved_channel_explicit_flag_wins_over_legacy() {
        // If a user passes both `--channel beta` and the legacy
        // `--stable`, the explicit channel flag wins. Documented
        // precedence: --channel > --stable > default.
        let cmd = UpgradeCommand {
            channel: Some(UpgradeChannel::Beta),
            version: None,
            path: None,
            yes: false,
            check: false,
            stable: true,
        };
        assert_eq!(cmd.resolved_channel(), UpgradeChannel::Beta);
    }

    #[test]
    fn resolved_channel_explicit_beta_selects_beta() {
        // Sanity: --channel beta does what it says.
        let cmd = UpgradeCommand {
            channel: Some(UpgradeChannel::Beta),
            version: None,
            path: None,
            yes: false,
            check: false,
            stable: false,
        };
        assert_eq!(cmd.resolved_channel(), UpgradeChannel::Beta);
    }
}
