use clap::{Args, ValueEnum};
use serde::Deserialize;
use std::env::consts::{ARCH, OS};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tracing::{debug, info};

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/gotempsh/temps/releases";

/// Default base URL of the Temps Cloud license + EE binary proxy. The EE
/// binary lives in a private repo and is only reachable through this
/// license-gated proxy. Overridable via `--ee-api` for staging/local.
const DEFAULT_EE_API: &str = "https://temps.sh";

/// Which edition to upgrade/switch to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum UpgradeTier {
    /// The open-source binary from GitHub releases (default).
    Oss,
    /// The Enterprise Edition binary from the license-gated temps.sh proxy.
    /// Requires `--license-path`.
    Ee,
}

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

    /// Print split-topology (ADR-017) console-restart guidance after the
    /// upgrade. In split mode the proxy (`temps proxy`, a systemd-managed,
    /// always-on service binding :80/:443) is untouched by an upgrade — only
    /// the CONSOLE process you run (`temps serve --role=console`) needs a
    /// manual restart to pick up the new binary. This flag ONLY prints the
    /// steps; temps does NOT restart, manage, or health-check anything for
    /// you. Default output (without `--split`) is unchanged.
    #[arg(long)]
    pub split: bool,

    /// DEPRECATED: alias for `--channel stable`. Kept for backward compat
    /// with existing scripts; will be removed in a future release. New
    /// callers should use `--channel stable` (or just omit the flag — it's
    /// the default).
    #[arg(long, hide = true)]
    pub stable: bool,

    /// Edition to upgrade to. Default: `oss` (GitHub releases). Pass
    /// `--tier ee` to switch this install to the Enterprise Edition binary,
    /// which requires `--license-path`.
    #[arg(long, value_enum)]
    pub tier: Option<UpgradeTier>,

    /// Path to the EE license JWT. Required with `--tier ee`. The license
    /// is also copied to `<data-dir>/data/license.jwt` and, if a systemd
    /// unit exists, the unit's `TEMPS_EE_LICENSE_PATH` env is updated so
    /// the binary finds its license on every restart.
    #[arg(long)]
    pub license_path: Option<PathBuf>,

    /// Base URL of the Temps Cloud EE proxy (`--tier ee` only). Defaults to
    /// `https://temps.sh`. Override for staging/local testing.
    #[arg(long)]
    pub ee_api: Option<String>,

    /// Data dir whose `data/license.jwt` receives the license on `--tier ee`.
    /// Defaults to `$TEMPS_DATA_DIR` or `~/.temps`.
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
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

    /// Effective tier. CLI-only, defaults to OSS.
    fn resolved_tier(&self) -> UpgradeTier {
        self.tier.unwrap_or(UpgradeTier::Oss)
    }

    async fn run(self) -> anyhow::Result<()> {
        // EE is a different distribution path (private repo, license-gated
        // proxy, license install, systemd env), so it gets its own method.
        if self.resolved_tier() == UpgradeTier::Ee {
            return self.run_ee().await;
        }
        self.run_oss().await
    }

    async fn run_oss(self) -> anyhow::Result<()> {
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

        // Split topology (ADR-017): the upgrade swapped the on-disk binary but
        // did NOT restart anything. The proxy is systemd-managed and keeps
        // serving :80/:443 on the OLD binary until its own process recycles;
        // the operator-run console must be restarted by hand to load the new
        // binary. We only PRINT the steps — see `restart_guidance`. With
        // `--split` absent this returns an empty string and nothing extra
        // prints, so the default output is unchanged.
        let guidance = restart_guidance(self.split);
        if !guidance.is_empty() {
            print!("{guidance}");
        }

        Ok(())
    }

    /// Switch this install to the Enterprise Edition binary.
    ///
    /// Differs from the OSS path: the EE binary lives in a private repo and
    /// is fetched through the license-gated proxy on temps.sh (no GitHub
    /// token on the host). After the swap we install the license to the
    /// data dir and, if a systemd unit exists, point its
    /// `TEMPS_EE_LICENSE_PATH` env at it so restarts keep working.
    async fn run_ee(self) -> anyhow::Result<()> {
        // EE only ships linux-amd64 today. Fail early with a clear message
        // rather than after resolving a version that has no usable asset.
        let target = platform_target()?;
        if target != "linux-amd64" {
            return Err(anyhow::anyhow!(
                "Temps EE currently ships linux-amd64 only (detected '{}'). \
                 macOS / arm64 EE builds are on the roadmap.",
                target
            ));
        }

        // License is mandatory for EE.
        let license_path = self.license_path.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "--tier ee requires --license-path <path-to-license.jwt>. \
                 Download yours from {}/dashboard/license",
                self.ee_api_base()
            )
        })?;
        let license_jwt = fs::read_to_string(&license_path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read license at {}: {}",
                    license_path.display(),
                    e
                )
            })?
            .trim()
            .to_string();
        if license_jwt.is_empty() {
            return Err(anyhow::anyhow!(
                "License file at {} is empty",
                license_path.display()
            ));
        }
        // Shape pre-check (signature is verified by the EE binary at boot).
        let summary = parse_license_summary(&license_jwt)?;

        // Determine the binary path to replace.
        let binary_path = match &self.path {
            Some(p) => p.clone(),
            None => std::env::current_exe()
                .map_err(|e| anyhow::anyhow!("Failed to determine current binary path: {}", e))?,
        };
        let binary_path = fs::canonicalize(&binary_path).unwrap_or(binary_path);

        let api = self.ee_api_base();
        let current_version = current_version_tag();

        // Resolve version (pinned or latest published) from the proxy.
        let version = match &self.version {
            Some(v) if v.starts_with('v') => v.clone(),
            Some(v) => format!("v{}", v),
            None => fetch_latest_ee_version(&api).await?,
        };

        // EE asset name: temps-ee-<version-without-v>-linux-amd64.tar.gz
        let asset = format!(
            "temps-ee-{}-{}.tar.gz",
            version.trim_start_matches('v'),
            target
        );

        println!();
        println!("  Switch to Enterprise Edition:");
        println!("    {} -> {} (ee)", current_version, version);
        println!("    Tier:     {}", summary.tier);
        println!("    Expires:  {}", summary.expires_display());
        println!("    Platform: {}", target);
        println!("    Binary:   {}", binary_path.display());
        println!(
            "    Source:   {}/api/ee/download/{}/{}",
            api, version, asset
        );
        println!();

        if self.check {
            println!("  Run without --check to install.");
            return Ok(());
        }

        if !self.yes {
            print!("  Proceed with EE switch? [y/N] ");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let input = input.trim().to_lowercase();
            if input != "y" && input != "yes" {
                println!("  Cancelled.");
                return Ok(());
            }
        }

        check_write_permission(&binary_path)?;

        // Verify checksum first (cheap; fails fast on a bad license/network).
        println!("  Verifying checksum...");
        let expected = fetch_ee_checksum(&api, &version, &asset, &license_jwt).await?;

        println!("  Downloading {}...", asset);
        let tarball = download_ee_asset(&api, &version, &asset, &license_jwt).await?;
        verify_checksum(&tarball, &expected)?;
        println!("  Checksum verified.");

        println!("  Extracting binary...");
        let new_binary = extract_binary_from_tarball(&tarball)?;

        println!("  Replacing binary at {}...", binary_path.display());
        replace_binary(&binary_path, &new_binary)?;

        // Install the license into the data dir so the binary finds it.
        let data_dir = resolve_data_dir(&self.data_dir)?;
        let installed_license = install_license(&data_dir, &license_jwt)?;
        println!("  License installed at {}", installed_license.display());

        // Best-effort: point the systemd unit at the license so restarts
        // keep working without re-passing --license-path.
        match update_systemd_license_env(&installed_license) {
            Ok(true) => println!("  Updated systemd unit env (TEMPS_EE_LICENSE_PATH)."),
            Ok(false) => {} // no unit / not linux — silent
            Err(e) => println!("  Note: could not update systemd unit env: {e}"),
        }

        println!();
        println!("  Successfully switched to Temps EE {}", version);
        println!("  Restart the service to activate:");
        println!("    sudo systemctl restart temps   # or your service manager");
        println!("  The binary will refuse to start without a valid license.");

        Ok(())
    }

    /// Resolve the EE proxy base URL (flag > default), trailing slash trimmed.
    fn ee_api_base(&self) -> String {
        self.ee_api
            .clone()
            .unwrap_or_else(|| DEFAULT_EE_API.to_string())
            .trim_end_matches('/')
            .to_string()
    }
}

/// Build the post-upgrade console-restart guidance for split topology
/// (ADR-017 Phase 3). PURE: returns the exact text to print, performs no
/// I/O, and starts/stops/health-checks NOTHING.
///
/// - `split == false` → returns an empty string (default `temps upgrade`
///   output is unchanged; the caller prints nothing extra).
/// - `split == true`  → returns multi-line guidance explaining that the
///   systemd-managed PROXY keeps serving :80/:443 untouched, and that the
///   operator must MANUALLY restart the console process they run
///   (`temps serve --role=console`), then confirm readiness via
///   `curl -fsS http://<console-address>/readyz`.
fn restart_guidance(split: bool) -> String {
    if !split {
        return String::new();
    }

    let mut out = String::new();
    out.push('\n');
    out.push_str("  Split topology (ADR-017) — finish the upgrade manually:\n");
    out.push('\n');
    out.push_str("  The new binary is in place, but temps did NOT restart anything.\n");
    out.push_str("  • The PROXY (`temps proxy`) is your systemd-managed, always-on\n");
    out.push_str("    service that serves :80/:443. It keeps running and serving\n");
    out.push_str("    traffic untouched — you do not restart it for a console upgrade.\n");
    out.push_str("  • The CONSOLE (`temps serve --role=console`) is NOT managed by\n");
    out.push_str("    temps. It is whatever YOU run it as — a manual process, a custom\n");
    out.push_str("    systemd unit, a supervised job, etc. You must restart it yourself\n");
    out.push_str("    so it loads the new binary.\n");
    out.push('\n');
    out.push_str("  1. Restart however you run the console, for example:\n");
    out.push_str("       # if you run it as a manual/foreground process: stop it, then\n");
    out.push_str("       temps serve --role=console --console-address <host:port>\n");
    out.push_str("       # if you wrapped it in your own unit, restart that unit instead\n");
    out.push('\n');
    out.push_str("  2. Confirm the console is ready (expects 'ready' / HTTP 200):\n");
    out.push_str("       curl -fsS http://<console-address>/readyz\n");
    out.push('\n');
    out.push_str("  temps does NOT restart, manage, or health-check the console for you.\n");
    out
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
    let computed = hex::encode(hasher.finalize());

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

// ── EE proxy helpers ────────────────────────────────────────────────────────

/// Minimal decoded view of an EE license JWT for the upgrade pre-check and
/// the confirmation summary. Signature is NOT verified here — only the EE
/// binary (with its embedded pubkey) can do that. This catches typos and
/// already-expired licenses before a long download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseSummary {
    pub tier: String,
    /// `exp` claim (unix seconds), if present.
    pub exp: Option<i64>,
}

impl LicenseSummary {
    fn expires_display(&self) -> String {
        match self.exp {
            Some(e) => chrono::DateTime::<chrono::Utc>::from_timestamp(e, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| e.to_string()),
            None => "unknown".to_string(),
        }
    }
}

/// Decode a base64url (no-padding) string. JWT segments use this alphabet
/// (`-`/`_` instead of `+`/`/`, no `=` padding). Small self-contained
/// decoder so we don't pull in the `base64` crate just for this.
fn decode_base64url(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => Err(format!("invalid base64url character: {}", c as char)),
        }
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u8;
    for &b in bytes {
        acc = (acc << 6) | val(b)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Decode + shape-validate an EE license JWT. Returns its tier/exp summary.
/// Rejects malformed JWTs, non-premium/enterprise tiers, and expired
/// licenses. Pure (takes `now` for testability via the wrapper below).
fn parse_license_summary(jwt: &str) -> anyhow::Result<LicenseSummary> {
    parse_license_summary_at(jwt, chrono::Utc::now().timestamp())
}

fn parse_license_summary_at(jwt: &str, now: i64) -> anyhow::Result<LicenseSummary> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow::anyhow!(
            "License is not a valid JWT (expected 3 segments, got {})",
            parts.len()
        ));
    }
    let payload = decode_base64url(parts[1])
        .map_err(|e| anyhow::anyhow!("Failed to decode license payload: {e}"))?;
    let claims: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|e| anyhow::anyhow!("License payload is not valid JSON: {e}"))?;

    let tier = claims
        .get("tier")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("License has no 'tier' claim"))?
        .to_string();
    if tier != "premium" && tier != "enterprise" {
        return Err(anyhow::anyhow!(
            "License tier '{}' cannot run the EE binary (need premium or enterprise)",
            tier
        ));
    }

    let exp = claims.get("exp").and_then(|e| e.as_i64());
    if let Some(exp) = exp {
        if exp <= now {
            return Err(anyhow::anyhow!(
                "License expired at unix {} (now {})",
                exp,
                now
            ));
        }
    }

    Ok(LicenseSummary { tier, exp })
}

/// Resolve the latest published EE version tag from the proxy.
async fn fetch_latest_ee_version(api: &str) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct ReleasesResponse {
        releases: Vec<ReleaseEntry>,
    }
    #[derive(Deserialize)]
    struct ReleaseEntry {
        tag: String,
    }

    let url = format!("{}/api/ee/releases", api);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "temps-self-upgrade")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch EE releases from {}: {}", url, e))?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "EE releases endpoint returned {} ({})",
            resp.status(),
            url
        ));
    }
    let body: ReleasesResponse = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse EE releases response: {}", e))?;
    body.releases
        .into_iter()
        .next()
        .map(|r| r.tag)
        .ok_or_else(|| anyhow::anyhow!("No published EE releases found at {}", url))
}

/// Fetch the `.sha256` for an EE asset through the license-gated proxy.
async fn fetch_ee_checksum(
    api: &str,
    version: &str,
    asset: &str,
    license_jwt: &str,
) -> anyhow::Result<String> {
    let url = format!("{}/api/ee/download/{}/{}/sha256", api, version, asset);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "temps-self-upgrade")
        .header("Authorization", format!("Bearer {}", license_jwt))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch EE checksum: {}", e))?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "EE checksum request returned {} (is your license valid?)",
            resp.status()
        ));
    }
    resp.text()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read EE checksum: {}", e))
}

/// Download an EE binary tarball through the license-gated proxy.
async fn download_ee_asset(
    api: &str,
    version: &str,
    asset: &str,
    license_jwt: &str,
) -> anyhow::Result<Vec<u8>> {
    let url = format!("{}/api/ee/download/{}/{}", api, version, asset);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "temps-self-upgrade")
        .header("Authorization", format!("Bearer {}", license_jwt))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to download EE binary: {}", e))?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "EE download returned {} ({})",
            resp.status(),
            url
        ));
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| anyhow::anyhow!("Failed to read EE download: {}", e))
}

/// Resolve the data dir: explicit flag/env > `~/.temps`.
fn resolve_data_dir(explicit: &Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.clone());
    }
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory for data dir"))?;
    Ok(home.join(".temps"))
}

/// Install the license JWT at `<data_dir>/data/license.jwt` (mode 0600).
/// Returns the path written.
fn install_license(data_dir: &std::path::Path, license_jwt: &str) -> anyhow::Result<PathBuf> {
    let dir = data_dir.join("data");
    fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("Failed to create {}: {}", dir.display(), e))?;
    let path = dir.join("license.jwt");
    fs::write(&path, license_jwt)
        .map_err(|e| anyhow::anyhow!("Failed to write license to {}: {}", path.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// If a systemd unit exists at /etc/systemd/system/temps.service, ensure it
/// has `Environment=TEMPS_EE_LICENSE_PATH=<path>` in its [Service] section.
/// Returns Ok(true) if the unit was modified, Ok(false) if there's nothing
/// to do (non-linux, no unit, or already present). Best-effort.
fn update_systemd_license_env(license_path: &std::path::Path) -> anyhow::Result<bool> {
    if OS != "linux" {
        return Ok(false);
    }
    let unit = PathBuf::from("/etc/systemd/system/temps.service");
    if !unit.exists() {
        return Ok(false);
    }
    let contents =
        fs::read_to_string(&unit).map_err(|e| anyhow::anyhow!("read {}: {}", unit.display(), e))?;

    let env_line = format!(
        "Environment=TEMPS_EE_LICENSE_PATH={}",
        license_path.display()
    );
    if contents.contains("TEMPS_EE_LICENSE_PATH=") {
        // Already wired (possibly to a different path) — leave operator's
        // value alone rather than fighting them.
        return Ok(false);
    }

    // Insert our Environment line right after the [Service] header so it
    // lands in the right section regardless of unit layout.
    let mut out = String::with_capacity(contents.len() + env_line.len() + 1);
    let mut inserted = false;
    for line in contents.lines() {
        out.push_str(line);
        out.push('\n');
        if !inserted && line.trim() == "[Service]" {
            out.push_str(&env_line);
            out.push('\n');
            inserted = true;
        }
    }
    if !inserted {
        // No [Service] section? Don't guess — report nothing changed.
        return Ok(false);
    }
    fs::write(&unit, out).map_err(|e| anyhow::anyhow!("write {}: {}", unit.display(), e))?;
    // Reload so the next restart picks up the new env. Ignore failure
    // (operator can `daemon-reload` manually).
    let _ = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status();
    Ok(true)
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
    fn test_restart_guidance_default_is_empty() {
        // Without --split, the default upgrade output must be unchanged:
        // the helper contributes nothing.
        assert_eq!(restart_guidance(false), "");
    }

    #[test]
    fn test_restart_guidance_split_mentions_console_restart_and_readyz() {
        let g = restart_guidance(true);
        assert!(!g.is_empty());
        // Targets the CONSOLE the operator runs, not the proxy.
        assert!(g.contains("temps serve --role=console"));
        // Readiness confirmation via /readyz curl line.
        assert!(g.contains("/readyz"));
        assert!(g.contains("curl"));
        // Explicit that temps manages/restarts nothing.
        assert!(g.contains("does NOT restart"));
        // Reassures that the always-on proxy / :80/:443 is untouched.
        assert!(g.contains(":80/:443") || g.to_lowercase().contains("untouched"));
    }

    #[test]
    fn test_restart_guidance_split_does_not_invoke_systemctl() {
        // Guidance must not tell the operator (or imply temps will run)
        // systemctl — the console is unmanaged by design.
        let g = restart_guidance(true);
        assert!(!g.contains("systemctl"));
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
        let hash = hex::encode(hasher.finalize());

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
            split: false,
            stable: false,
            tier: None,
            license_path: None,
            ee_api: None,
            data_dir: None,
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
            split: false,
            stable: true,
            tier: None,
            license_path: None,
            ee_api: None,
            data_dir: None,
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
            split: false,
            stable: true,
            tier: None,
            license_path: None,
            ee_api: None,
            data_dir: None,
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
            split: false,
            stable: false,
            tier: None,
            license_path: None,
            ee_api: None,
            data_dir: None,
        };
        assert_eq!(cmd.resolved_channel(), UpgradeChannel::Beta);
    }

    // ── EE tier + license logic ──────────────────────────────────────────

    fn cmd_with_tier(tier: Option<UpgradeTier>) -> UpgradeCommand {
        UpgradeCommand {
            channel: None,
            version: None,
            path: None,
            yes: false,
            check: false,
            split: false,
            stable: false,
            tier,
            license_path: None,
            ee_api: None,
            data_dir: None,
        }
    }

    #[test]
    fn resolved_tier_defaults_to_oss() {
        // No --tier means OSS: existing scripts keep working unchanged.
        assert_eq!(cmd_with_tier(None).resolved_tier(), UpgradeTier::Oss);
    }

    #[test]
    fn resolved_tier_ee_when_flagged() {
        assert_eq!(
            cmd_with_tier(Some(UpgradeTier::Ee)).resolved_tier(),
            UpgradeTier::Ee
        );
    }

    #[test]
    fn ee_api_base_defaults_and_trims() {
        let mut cmd = cmd_with_tier(Some(UpgradeTier::Ee));
        assert_eq!(cmd.ee_api_base(), "https://temps.sh");
        cmd.ee_api = Some("http://localhost:4432/".to_string());
        assert_eq!(cmd.ee_api_base(), "http://localhost:4432");
    }

    #[test]
    fn decode_base64url_roundtrip() {
        // base64url of {"tier":"premium"} (no padding)
        let json = b"{\"tier\":\"premium\"}";
        // Build the encoding the same way a JWT would (URL_SAFE_NO_PAD).
        // Hand-encode via a known-good value instead of importing base64:
        // we just assert our decoder produces the original bytes from a
        // string we encode with the standard alphabet mapping.
        let encoded = encode_base64url_for_test(json);
        assert_eq!(decode_base64url(&encoded).unwrap(), json);
    }

    #[test]
    fn parse_license_summary_accepts_valid_premium() {
        let jwt = make_test_jwt(r#"{"tier":"premium","exp":9999999999}"#);
        let s = parse_license_summary_at(&jwt, 1_000_000_000).unwrap();
        assert_eq!(s.tier, "premium");
        assert_eq!(s.exp, Some(9999999999));
    }

    #[test]
    fn parse_license_summary_rejects_expired() {
        let jwt = make_test_jwt(r#"{"tier":"premium","exp":100}"#);
        let err = parse_license_summary_at(&jwt, 1_000_000_000).unwrap_err();
        assert!(err.to_string().contains("expired"), "got: {err}");
    }

    #[test]
    fn parse_license_summary_rejects_community_tier() {
        let jwt = make_test_jwt(r#"{"tier":"community","exp":9999999999}"#);
        let err = parse_license_summary_at(&jwt, 1_000_000_000).unwrap_err();
        assert!(
            err.to_string().contains("cannot run the EE binary"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_license_summary_rejects_malformed() {
        let err = parse_license_summary_at("not.a.jwt.extra", 0).unwrap_err();
        assert!(err.to_string().contains("3 segments"), "got: {err}");
    }

    // Test-only base64url encoder (no padding) so we can build JWTs to feed
    // the decoder + parser without adding the base64 crate as a dep.
    fn encode_base64url_for_test(input: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        let mut acc: u32 = 0;
        let mut bits = 0u8;
        for &b in input {
            acc = (acc << 8) | b as u32;
            bits += 8;
            while bits >= 6 {
                bits -= 6;
                out.push(ALPHABET[((acc >> bits) & 0x3f) as usize] as char);
            }
        }
        if bits > 0 {
            out.push(ALPHABET[((acc << (6 - bits)) & 0x3f) as usize] as char);
        }
        out
    }

    fn make_test_jwt(claims_json: &str) -> String {
        let header = encode_base64url_for_test(br#"{"alg":"EdDSA","typ":"JWT"}"#);
        let payload = encode_base64url_for_test(claims_json.as_bytes());
        // Signature segment is arbitrary — parse_license_summary never
        // verifies it (the EE binary does).
        format!("{header}.{payload}.{}", encode_base64url_for_test(b"sig"))
    }
}
