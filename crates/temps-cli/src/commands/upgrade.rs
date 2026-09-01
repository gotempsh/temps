// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use clap::{Args, ValueEnum};
use serde::Deserialize;
use std::env::consts::{ARCH, OS};
use std::fs;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::{NamedTempFile, TempPath};
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
/// tags don't (`v1.2.0`). Nightly tags are a distinct kind of prerelease,
/// identified by a `-nightly.` segment (`v1.2.0-nightly.20260727.abc1234`),
/// minted automatically by CI — see `is_nightly_tag`.
///
/// Channel selection is **CLI-only** — there is no env-var fallback. The
/// default is `Stable` and the user must explicitly pass `--channel beta`
/// or `--channel nightly` to opt into prereleases. This is by design: an
/// env var on a long-lived shell or CI runner could silently switch a host
/// onto beta/nightly without an audit trail, which we want to prevent.
/// Pinning a specific `--version` ignores the channel entirely.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum UpgradeChannel {
    /// Track stable releases only (default). Tag must NOT contain `-`.
    Stable,
    /// Track beta releases: any prerelease tag EXCEPT nightly builds.
    /// `Beta` selects the newest of stable + beta, so a beta host receives
    /// stable releases too — they're considered an upgrade from the latest
    /// beta on the same line. Nightly builds are excluded so an operator who
    /// opts into a deliberate `-beta.N` cut never silently lands on an
    /// automated nightly instead.
    Beta,
    /// Track nightly builds only: tags containing `-nightly.`, cut
    /// automatically once a day from `main` when it has new commits. The
    /// least stable channel — intended for testing unreleased work, not
    /// production hosts.
    Nightly,
}

/// Is this tag a nightly build (minted by the `Nightly Release` CI workflow)?
/// Nightly tags look like `v0.1.0-nightly.20260727.abc1234` — the
/// `-nightly.` marker distinguishes them from a deliberate `-beta.N` cut.
fn is_nightly_tag(tag: &str) -> bool {
    tag.contains("-nightly.")
}

impl UpgradeChannel {
    /// Parse a channel configured in settings. Unknown values return `None` so
    /// the caller falls back to inferring from the running version rather than
    /// silently tracking the wrong channel.
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "beta" => Some(Self::Beta),
            "nightly" => Some(Self::Nightly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Nightly => "nightly",
        }
    }

    /// Does this release belong to this channel?
    /// - Stable: only non-prerelease tags. `v1.2.0` matches; `v1.2.0-beta.4` does not.
    /// - Beta: any non-draft prerelease-or-stable tag EXCEPT nightly builds.
    /// - Nightly: only tags with a `-nightly.` marker.
    fn includes(self, release: &GitHubRelease) -> bool {
        if release.draft {
            return false;
        }
        let nightly = is_nightly_tag(&release.tag_name);
        match self {
            Self::Stable => !release.prerelease,
            Self::Beta => !nightly,
            Self::Nightly => nightly,
        }
    }

    /// Infer the channel an *installed* binary is on from its version tag.
    /// A `-nightly.` tag means the host opted into nightly; any other
    /// prerelease tag (`v1.2.0-beta.4` — anything with a `-` after the core
    /// version) means the host opted into beta at install/upgrade time; a
    /// plain tag (`v1.2.0`) means stable. Used by the startup update
    /// notifier, which has no `--channel` flag to consult.
    pub fn for_installed_version(tag: &str) -> Self {
        let core = tag.trim().trim_start_matches('v');
        if is_nightly_tag(tag) {
            Self::Nightly
        } else if core.contains('-') {
            Self::Beta
        } else {
            Self::Stable
        }
    }
}

/// Self-upgrade temps to the latest version
#[derive(Args)]
pub struct UpgradeCommand {
    /// Release channel to track. Default: `stable`. Pass `--channel beta`
    /// to opt into prereleases, or `--channel nightly` to track automated
    /// nightly builds cut from `main`. Pinning a `--version` ignores the
    /// channel.
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
    ///
    /// Also readable from `TEMPS_EE_LICENSE_PATH` -- the same env var an
    /// EE binary's own startup gate reads, so one value covers both "the
    /// license this running binary starts with" and "the license to
    /// install for this upgrade" when they're the same file, which they
    /// almost always are.
    #[arg(long, env = "TEMPS_EE_LICENSE_PATH")]
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
    pub(crate) name: String,
    pub(crate) browser_download_url: String,
    pub(crate) size: u64,
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

        let parent = binary_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine parent directory of binary"))?
            .to_path_buf();

        // The whole upgrade streams through disk. The tarball is ~110 MB and
        // the binary inside it ~270 MB; buffering either one meant a peak well
        // past half a gigabyte, which on a 1 GB host with the server running
        // pushes the box into swap thrashing.
        //
        // The tarball is staged to disk rather than piped straight into the
        // untar because the checksum has to be verified BEFORE any of those
        // bytes are unpacked. A direct download -> gunzip -> tar -> binary
        // pipeline would save the 110 MB of scratch disk but would write
        // unverified content over the live executable.
        let mut download_file = create_upgrade_temp_file(&parent, ".temps-upgrade-dl.")?;
        let download_path = download_file.path().to_path_buf();

        // Download the tarball
        println!("  Downloading {}...", tarball_name);
        let computed = download_asset_to_file(
            &asset.browser_download_url,
            download_file.as_file_mut(),
            &download_path,
        )
        .await?;

        // Also download the checksum
        let checksum_name = format!("{}.sha256", tarball_name);
        let checksum_asset = release.assets.iter().find(|a| a.name == checksum_name);

        if let Some(checksum_asset) = checksum_asset {
            debug!("Verifying checksum...");
            let checksum_text = download_asset_text(&checksum_asset.browser_download_url).await?;
            verify_computed_checksum(&computed, &checksum_text)?;
            println!("  Checksum verified.");
        } else {
            debug!("No checksum asset found, skipping verification");
        }

        // Extract the binary from the tarball, straight into the staging file
        // that the atomic rename below consumes.
        println!("  Extracting binary...");
        let mut staged_file = create_upgrade_temp_file(&parent, ".temps-upgrade-bin.")?;
        let staged_path = staged_file.path().to_path_buf();
        extract_binary_from_tarball_file(
            download_file.as_file_mut(),
            &download_path,
            staged_file.as_file_mut(),
            &staged_path,
        )?;

        // Close the writable staging handle before the file can ever be
        // executed. This is also required by the in-process updater's
        // preflight: Unix rejects an executable that is still open for write.
        let staged_path = seal_staged_binary(staged_file)?;

        // Replace the binary atomically
        println!("  Replacing binary at {}...", binary_path.display());
        finalize_staged_binary(&binary_path, staged_path)?;

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

        let parent = binary_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine parent directory of binary"))?
            .to_path_buf();

        // Same streaming shape as the OSS path, and for the same reason: see
        // the comment in `run_oss`.
        let mut download_file = create_upgrade_temp_file(&parent, ".temps-upgrade-dl.")?;
        let download_path = download_file.path().to_path_buf();

        println!("  Downloading {}...", asset);
        let computed = download_ee_asset_to_file(
            &api,
            &version,
            &asset,
            &license_jwt,
            download_file.as_file_mut(),
            &download_path,
        )
        .await?;
        verify_computed_checksum(&computed, &expected)?;
        println!("  Checksum verified.");

        println!("  Extracting binary...");
        let mut staged_file = create_upgrade_temp_file(&parent, ".temps-upgrade-bin.")?;
        let staged_path = staged_file.path().to_path_buf();
        extract_binary_from_tarball_file(
            download_file.as_file_mut(),
            &download_path,
            staged_file.as_file_mut(),
            &staged_path,
        )?;

        let staged_path = seal_staged_binary(staged_file)?;

        println!("  Replacing binary at {}...", binary_path.display());
        finalize_staged_binary(&binary_path, staged_path)?;

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

/// How long a single background update check may take before it's abandoned.
const UPDATE_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Delay before the first startup check so it never competes with startup
/// work (DB migrations, plugin init, proxy bind) for I/O or log attention.
const UPDATE_CHECK_STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// Default re-check interval. Two hours stays comfortably below GitHub's
/// unauthenticated API limit while surfacing releases promptly.
const DEFAULT_UPDATE_CHECK_INTERVAL_HOURS: u64 = 2;
const DEFAULT_UPDATE_CHECK_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(DEFAULT_UPDATE_CHECK_INTERVAL_HOURS * 60 * 60);
const UPDATE_CHECK_INTERVAL_ENV: &str = "TEMPS_UPDATE_CHECK_INTERVAL_HOURS";
const MIN_UPDATE_CHECK_INTERVAL_HOURS: u64 = 1;
const MAX_UPDATE_CHECK_INTERVAL_HOURS: u64 = 168;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum UpdateCheckIntervalError {
    #[error("value '{value}' is not a whole number of hours")]
    InvalidNumber { value: String },
    #[error("{hours} hours is outside the supported range 1..=168")]
    OutOfRange { hours: u64 },
    #[error("value is not valid UTF-8")]
    NotUnicode,
}

fn parse_update_check_interval_hours(value: &str) -> Result<u64, UpdateCheckIntervalError> {
    let hours =
        value
            .trim()
            .parse::<u64>()
            .map_err(|_| UpdateCheckIntervalError::InvalidNumber {
                value: value.to_string(),
            })?;
    if !(MIN_UPDATE_CHECK_INTERVAL_HOURS..=MAX_UPDATE_CHECK_INTERVAL_HOURS).contains(&hours) {
        return Err(UpdateCheckIntervalError::OutOfRange { hours });
    }
    Ok(hours)
}

pub fn configured_update_check_interval() -> std::time::Duration {
    let configured_hours = match std::env::var(UPDATE_CHECK_INTERVAL_ENV) {
        Ok(value) => match parse_update_check_interval_hours(&value) {
            Ok(hours) => hours,
            Err(error) => {
                tracing::warn!(
                    variable = UPDATE_CHECK_INTERVAL_ENV,
                    value = %value,
                    error = %error,
                    default_hours = DEFAULT_UPDATE_CHECK_INTERVAL_HOURS,
                    "Invalid update-check interval; using the default"
                );
                return DEFAULT_UPDATE_CHECK_INTERVAL;
            }
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_UPDATE_CHECK_INTERVAL_HOURS,
        Err(std::env::VarError::NotUnicode(_)) => {
            let error = UpdateCheckIntervalError::NotUnicode;
            tracing::warn!(
                variable = UPDATE_CHECK_INTERVAL_ENV,
                error = %error,
                default_hours = DEFAULT_UPDATE_CHECK_INTERVAL_HOURS,
                "Invalid update-check interval; using the default"
            );
            return DEFAULT_UPDATE_CHECK_INTERVAL;
        }
    };
    let interval = std::time::Duration::from_secs(configured_hours * 60 * 60);
    tracing::info!(
        interval_hours = configured_hours,
        variable = UPDATE_CHECK_INTERVAL_ENV,
        "Release update-check cadence configured"
    );
    interval
}

/// A newer release the background notifier found for this install's channel.
pub struct UpdateNotice {
    pub current_version: String,
    pub latest_version: String,
    pub channel: UpgradeChannel,
    /// GitHub release page (release notes) for the newer version.
    pub release_url: String,
}

/// Prerelease identifier with semver §11 ordering: numeric identifiers
/// compare numerically and rank below alphanumeric ones (`Num` before
/// `Alpha` in the enum gives that via derived `Ord`).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PreIdent {
    Num(u64),
    Alpha(String),
}

/// Sort key for a `vMAJOR.MINOR.PATCH[-pre]` tag, ordered per semver:
/// core version first, then "release beats prerelease" (the `bool`), then
/// prerelease identifiers element-wise (a longer identifier list wins a
/// shared prefix, which is exactly `Vec`'s derived ordering).
type VersionSortKey = ((u64, u64, u64), bool, Vec<PreIdent>);

/// Parse a tag into its ordering key. `None` when the tag isn't
/// version-shaped — callers must treat that as "not comparable", never as
/// older or newer.
fn version_sort_key(tag: &str) -> Option<VersionSortKey> {
    let v = tag.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (v, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let (is_release, idents) = match pre {
        None => (true, Vec::new()),
        Some(p) => (
            false,
            p.split('.')
                .map(|ident| {
                    ident
                        .parse::<u64>()
                        .map(PreIdent::Num)
                        .unwrap_or_else(|_| PreIdent::Alpha(ident.to_string()))
                })
                .collect(),
        ),
    };
    Some(((major, minor, patch), is_release, idents))
}

/// Is `candidate` strictly newer than `current`? Conservative by design:
/// if either tag doesn't parse as a version, the answer is `false` — the
/// notifier would rather stay silent than nag a dev build or a fork with
/// exotic tags. This is deliberately stricter than `temps upgrade`, which
/// treats any tag difference as upgradeable (including downgrades the
/// operator explicitly pins with `--version`).
pub(crate) fn is_newer_version(candidate: &str, current: &str) -> bool {
    match (version_sort_key(candidate), version_sort_key(current)) {
        (Some(candidate_key), Some(current_key)) => candidate_key > current_key,
        _ => false,
    }
}

/// One background update check: fetch the newest release on this install's
/// channel and return a notice if it is strictly newer than the running
/// binary. Every failure path (network, GitHub quota, unparsable tags)
/// collapses to `None` with a debug log — the notifier is advisory and must
/// never surface errors to an operator who didn't ask for a check.
pub async fn check_for_newer_release(configured_channel: Option<&str>) -> Option<UpdateNotice> {
    let current_version = current_version_tag();
    // An explicit channel from settings wins; otherwise fall back to the tag
    // the running binary carries, which is what the CLI has always done.
    let channel = configured_channel
        .and_then(UpgradeChannel::from_setting)
        .unwrap_or_else(|| UpgradeChannel::for_installed_version(&current_version));

    let release = match tokio::time::timeout(
        UPDATE_CHECK_TIMEOUT,
        fetch_latest_release_in_channel(channel),
    )
    .await
    {
        Ok(Ok(release)) => release,
        Ok(Err(e)) => {
            debug!(
                "Update check on '{}' channel failed: {}",
                channel.as_str(),
                e
            );
            return None;
        }
        Err(_) => {
            debug!(
                "Update check on '{}' channel timed out after {:?}",
                channel.as_str(),
                UPDATE_CHECK_TIMEOUT
            );
            return None;
        }
    };

    if is_newer_version(&release.tag_name, &current_version) {
        Some(UpdateNotice {
            current_version,
            latest_version: release.tag_name,
            channel,
            release_url: release.html_url,
        })
    } else {
        debug!(
            "temps {} is up to date on '{}' channel (latest published: {})",
            current_version,
            channel.as_str(),
            release.tag_name
        );
        None
    }
}

/// Background task for `temps serve`: check for a newer release shortly
/// after startup, then re-check at the configured interval. Each hit is published into the
/// shared `UpdateStatusSlot`, which `GET /settings/update-status` serves so
/// the web console can render an upgrade banner; a single WARN line covers
/// headless/log-only operators. Never returns; the caller detaches it on a
/// long-lived runtime.
pub async fn update_notifier_loop(
    slot: Arc<temps_core::UpdateStatusSlot>,
    interval: std::time::Duration,
    config_service: Arc<temps_config::ConfigService>,
) {
    tokio::time::sleep(UPDATE_CHECK_STARTUP_DELAY).await;
    loop {
        // Re-read every pass so switching channel in the console takes effect
        // on the next check instead of requiring a restart.
        let configured_channel = config_service
            .get_settings()
            .await
            .ok()
            .and_then(|s| s.self_update().channel);
        if let Some(notice) = check_for_newer_release(configured_channel.as_deref()).await {
            tracing::warn!(
                current_version = %notice.current_version,
                latest_version = %notice.latest_version,
                channel = notice.channel.as_str(),
                "A new temps release is available: {} -> {} ({} channel). \
                 See {} or run `temps upgrade`.",
                notice.current_version,
                notice.latest_version,
                notice.channel.as_str(),
                temps_core::UPGRADE_DOCS_URL
            );
            slot.set(temps_core::AvailableUpdate {
                current_version: notice.current_version,
                latest_version: notice.latest_version,
                channel: notice.channel.as_str().to_string(),
                release_url: notice.release_url,
                checked_at: chrono::Utc::now(),
            });
        }
        tokio::time::sleep(interval).await;
    }
}

/// Determine the platform target string matching release asset names.
pub(crate) fn platform_target() -> anyhow::Result<String> {
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
        UpgradeChannel::Nightly => anyhow::anyhow!(
            "No nightly releases found. The nightly build only cuts a new tag when \
             `main` has commits since the last one — check the 'Nightly Release' \
             workflow run history, or try `--channel beta`."
        ),
    })
}

/// Pure picker — split out so tests can drive it without an HTTP mock.
fn pick_release_for_channel(
    releases: Vec<GitHubRelease>,
    channel: UpgradeChannel,
) -> Option<GitHubRelease> {
    releases.into_iter().find(|r| channel.includes(r))
}

/// Normalize a caller-supplied version into a release tag, rejecting anything
/// that is not a plain semver-shaped tag.
///
/// **This is a security boundary, not cosmetics.** The tag is interpolated into
/// a GitHub API path, and the `url` crate resolves `..` segments when parsing —
/// so an unvalidated tag like `v/../../../../../owner/repo/releases/latest`
/// walks out of `gotempsh/temps` and resolves to *another repository's* release.
/// Everything downstream then behaves normally: it downloads that release's
/// `temps-<target>.tar.gz`, checks it against that release's own `.sha256`
/// (which of course matches), executes it for the version preflight, and
/// installs it over the running binary. In other words, a caller who can reach
/// `temps upgrade --version` or `POST /settings/update` could install an
/// arbitrary binary. Keep this strict.
pub(crate) fn normalize_release_tag(version: &str) -> anyhow::Result<String> {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("Version must not be empty"));
    }

    let core = trimmed.strip_prefix('v').unwrap_or(trimmed);
    // Split off the prerelease/build suffix; the numeric core is validated
    // separately so `1.2.3` and `1.2.3-beta.4` are both accepted but
    // `1.2.3/../x` is not.
    let (numeric, suffix) = match core.split_once(['-', '+']) {
        Some((numeric, suffix)) => (numeric, Some(suffix)),
        None => (core, None),
    };

    let mut parts = numeric.split('.');
    let mut components = 0;
    for _ in 0..3 {
        let component = parts.next().ok_or_else(|| {
            anyhow::anyhow!("Version '{version}' must look like 'v1.2.3' or 'v1.2.3-beta.4'")
        })?;
        if component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()) {
            return Err(anyhow::anyhow!(
                "Version '{version}' has a non-numeric component '{component}'"
            ));
        }
        components += 1;
    }
    if components != 3 || parts.next().is_some() {
        return Err(anyhow::anyhow!(
            "Version '{version}' must have exactly three numeric components"
        ));
    }

    if let Some(suffix) = suffix {
        // Deliberately narrow: alphanumerics, dot and dash only. No slashes, no
        // percent-encoding, nothing that can add or escape a path segment.
        if suffix.is_empty()
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(anyhow::anyhow!(
                "Version '{version}' has an unsupported prerelease suffix"
            ));
        }
        if suffix.split('.').any(|segment| segment.is_empty()) {
            return Err(anyhow::anyhow!(
                "Version '{version}' has an empty prerelease segment"
            ));
        }
    }

    Ok(format!("v{core}"))
}

/// Fetch a specific release by tag from GitHub.
pub(crate) async fn fetch_specific_release(version: &str) -> anyhow::Result<GitHubRelease> {
    let tag = normalize_release_tag(version)?;

    // Built by pushing a validated segment rather than string interpolation, so
    // even a future validation slip cannot alter the path structure.
    let mut url = reqwest::Url::parse("https://api.github.com/repos/gotempsh/temps/releases/tags/")
        .map_err(|e| anyhow::anyhow!("Failed to build the release URL: {e}"))?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Failed to build the release URL"))?
        .pop_if_empty()
        .push(&tag);
    let url = url.to_string();

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
pub(crate) async fn download_asset(url: &str) -> anyhow::Result<Vec<u8>> {
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
pub(crate) async fn download_asset_text(url: &str) -> anyhow::Result<String> {
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

/// Stream a release asset to an already-open file, hashing it in the same pass.
///
/// Returns the lowercase hex SHA256 of everything written, so the caller can
/// verify the download without reading the file back. The whole point is that
/// the artifact never exists in memory: the release tarball is ~110 MB and the
/// binary inside it ~270 MB, which does not fit alongside a running server on
/// a 1 GB host.
pub(crate) async fn download_asset_to_file(
    url: &str,
    dest: &mut fs::File,
    dest_path: &Path,
) -> anyhow::Result<String> {
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

    stream_response_to_file(response, dest, dest_path).await
}

/// Shared body of the streaming downloads (OSS release and EE proxy).
///
/// Peak memory here is one HTTP chunk (tens of KB), not the asset size.
async fn stream_response_to_file(
    mut response: reqwest::Response,
    file: &mut fs::File,
    dest_path: &Path,
) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};

    file.seek(std::io::SeekFrom::Start(0)).map_err(|e| {
        anyhow::anyhow!(
            "Failed to seek download file {}: {}",
            dest_path.display(),
            e
        )
    })?;
    file.set_len(0).map_err(|e| {
        anyhow::anyhow!(
            "Failed to truncate download file {}: {}",
            dest_path.display(),
            e
        )
    })?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read download response: {}", e))?
    {
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(|e| {
            anyhow::anyhow!(
                "Failed to write download to {} (after {} bytes): {}",
                dest_path.display(),
                written,
                e
            )
        })?;
        written += chunk.len() as u64;
    }

    file.flush().map_err(|e| {
        anyhow::anyhow!("Failed to flush download to {}: {}", dest_path.display(), e)
    })?;

    debug!("Downloaded {} bytes to {}", written, dest_path.display());
    Ok(hex::encode(hasher.finalize()))
}

/// Parse the expected hash out of a `.sha256` file body.
///
/// Format: `"<hash>  <filename>"` or `"<hash> <filename>"`.
fn parse_expected_checksum(checksum_text: &str) -> anyhow::Result<String> {
    Ok(checksum_text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid checksum file format"))?
        .to_lowercase())
}

/// Compare an already-computed SHA256 against a `.sha256` file body.
///
/// Split out from [`verify_checksum`] so the streaming download can verify the
/// digest it accumulated on the way to disk, instead of re-reading the file (or
/// keeping it in memory) just to hash it a second time.
pub(crate) fn verify_computed_checksum(computed: &str, checksum_text: &str) -> anyhow::Result<()> {
    let expected = parse_expected_checksum(checksum_text)?;
    let computed = computed.to_lowercase();

    if computed != expected {
        return Err(anyhow::anyhow!(
            "Checksum mismatch!\n  Expected: {}\n  Got:      {}",
            expected,
            computed
        ));
    }

    Ok(())
}

/// Verify SHA256 checksum of downloaded data.
pub(crate) fn verify_checksum(data: &[u8], checksum_text: &str) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(data);
    let computed = hex::encode(hasher.finalize());

    verify_computed_checksum(&computed, checksum_text)
}

/// Extract the `temps` binary from a gzipped tarball.
///
/// No production caller remains — both `temps upgrade` and the in-process
/// self-updater stream through disk via [`extract_binary_from_tarball_file`].
/// Kept `#[cfg(test)]` as the byte-identical baseline the streaming path is
/// checked against.
#[cfg(test)]
pub(crate) fn extract_binary_from_tarball(tarball_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
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

/// Extract the `temps` binary from an open gzipped tarball, straight into an
/// already-open staging file.
///
/// The streaming counterpart of [`extract_binary_from_tarball`]: the gunzip and
/// untar run over the file, and the entry is copied out with `std::io::copy`,
/// so memory stays at one copy buffer instead of holding the ~270 MB
/// uncompressed binary (which `read_to_end` would additionally double while
/// growing its `Vec`).
pub(crate) fn extract_binary_from_tarball_file(
    tarball: &mut fs::File,
    tarball_path: &Path,
    dest: &mut fs::File,
    dest_path: &Path,
) -> anyhow::Result<u64> {
    use flate2::read::GzDecoder;

    tarball.seek(std::io::SeekFrom::Start(0)).map_err(|e| {
        anyhow::anyhow!(
            "Failed to seek downloaded tarball {}: {}",
            tarball_path.display(),
            e
        )
    })?;
    let decoder = GzDecoder::new(std::io::BufReader::new(tarball));
    let mut archive = tar::Archive::new(decoder);

    let entries = archive
        .entries()
        .map_err(|e| anyhow::anyhow!("Failed to read tarball {}: {}", tarball_path.display(), e))?;

    for entry in entries {
        let mut entry = entry.map_err(|e| {
            anyhow::anyhow!(
                "Failed to read entry in tarball {}: {}",
                tarball_path.display(),
                e
            )
        })?;
        let is_binary = entry
            .path()
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read entry path in tarball {}: {}",
                    tarball_path.display(),
                    e
                )
            })?
            .file_name()
            .map(|n| n == "temps")
            .unwrap_or(false);

        if !is_binary {
            continue;
        }

        // An entry can carry the right name and still not be a binary: an
        // empty file, a symlink or a hard link named `temps` would copy zero
        // bytes and pass a size check of 0 == 0, landing on the executable the
        // whole system runs.
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() {
            return Err(anyhow::anyhow!(
                "Entry 'temps' in tarball {} is not a regular file ({:?})",
                tarball_path.display(),
                entry_type
            ));
        }

        // `Entry::size`, not `Header::size`: the former is the number of bytes
        // the entry reader yields, honouring a PAX size override, while the
        // latter reports the logical size and disagrees for PAX and sparse
        // entries, which would read as a truncation that never happened.
        let declared = entry.size();

        dest.seek(std::io::SeekFrom::Start(0)).map_err(|e| {
            anyhow::anyhow!(
                "Failed to seek staged binary {}: {}",
                dest_path.display(),
                e
            )
        })?;
        dest.set_len(0).map_err(|e| {
            anyhow::anyhow!(
                "Failed to truncate staged binary {}: {}",
                dest_path.display(),
                e
            )
        })?;
        let written = std::io::copy(&mut entry, dest).map_err(|e| {
            anyhow::anyhow!("Failed to extract binary to {}: {}", dest_path.display(), e)
        })?;

        // Validate before the rename, not after. The verified checksum covers
        // the tarball, so a short read here (full disk, truncated gzip stream)
        // would otherwise put a valid-looking but incomplete file over the live
        // executable. Comparing against the tar header's declared size is free
        // and catches exactly that.
        if written != declared {
            return Err(anyhow::anyhow!(
                "Extracted binary is truncated: expected {} bytes, wrote {} to {}",
                declared,
                written,
                dest_path.display()
            ));
        }
        if written == 0 {
            return Err(anyhow::anyhow!(
                "Entry 'temps' in tarball {} is empty",
                tarball_path.display()
            ));
        }

        debug!("Extracted {} bytes to {}", written, dest_path.display());
        return Ok(written);
    }

    Err(anyhow::anyhow!(
        "Binary 'temps' not found in the downloaded tarball"
    ))
}

/// Securely create an upgrade scratch file beside the target binary.
///
/// This preserves constant-memory/same-filesystem staging while `tempfile`
/// supplies an unpredictable name and atomic create-new semantics.
pub(crate) fn create_upgrade_temp_file(
    parent: &Path,
    prefix: &str,
) -> anyhow::Result<NamedTempFile> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(parent)
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to create upgrade temporary file in {}: {}",
                parent.display(),
                e
            )
        })
}

/// Finish writing a staged executable, make it durable, and close its writable
/// handle.
///
/// Closing the handle is part of the correctness contract: the in-process
/// updater executes this path for its preflight, and Unix can reject (or kill)
/// an executable while any process still has it open for writing.
pub(crate) fn seal_staged_binary(staged_file: NamedTempFile) -> anyhow::Result<TempPath> {
    let staged_path = staged_file.path().to_path_buf();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        staged_file.as_file().set_permissions(perms).map_err(|e| {
            anyhow::anyhow!(
                "Failed to set executable permissions on staged binary {}: {}",
                staged_path.display(),
                e
            )
        })?;
    }

    staged_file.as_file().sync_all().map_err(|e| {
        anyhow::anyhow!(
            "Failed to sync staged binary {} before closing its write handle: {}",
            staged_path.display(),
            e
        )
    })?;

    Ok(staged_file.into_temp_path())
}

/// Atomically persist a sealed staged binary over the target.
pub(crate) fn finalize_staged_binary(
    binary_path: &Path,
    staged_path: TempPath,
) -> anyhow::Result<()> {
    let staged_path_for_error = staged_path.to_path_buf();

    // The destination normally exists, so overwrite-capable `persist` is the
    // correct atomic operation; `persist_noclobber` would reject an upgrade.
    staged_path.persist(binary_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to atomically replace binary {} using staged file {}: {}",
            binary_path.display(),
            staged_path_for_error.display(),
            e.error
        )
    })?;

    sync_binary_parent(binary_path)?;
    Ok(())
}

#[cfg(unix)]
fn sync_binary_parent(binary_path: &Path) -> anyhow::Result<()> {
    let parent = binary_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Cannot determine parent directory of replaced binary {}",
            binary_path.display()
        )
    })?;
    let directory = fs::File::open(parent).map_err(|e| {
        anyhow::anyhow!(
            "Binary {} was replaced, but failed to open parent directory {} for sync: {}",
            binary_path.display(),
            parent.display(),
            e
        )
    })?;
    directory.sync_all().map_err(|e| {
        anyhow::anyhow!(
            "Binary {} was replaced, but failed to sync parent directory {}: {}",
            binary_path.display(),
            parent.display(),
            e
        )
    })
}

#[cfg(not(unix))]
fn sync_binary_parent(_binary_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Check we have write permission to the binary path.
pub(crate) fn check_write_permission(binary_path: &PathBuf) -> anyhow::Result<()> {
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

/// Replace the binary using an atomic persist strategy:
/// 1. Securely create and write a random temp file next to the target
/// 2. Set executable permissions
/// 3. Sync it, persist it over the target, and sync the parent directory
///
/// No production caller remains — both `temps upgrade` and the in-process
/// self-updater stage into a file via [`create_upgrade_temp_file`] and commit
/// with [`finalize_staged_binary`] directly. Kept `#[cfg(test)]` to exercise
/// the write-then-finalize sequence from an in-memory buffer.
#[cfg(test)]
pub(crate) fn replace_binary(binary_path: &Path, new_binary: &[u8]) -> anyhow::Result<()> {
    let parent = binary_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine parent directory"))?;

    let mut staged_file = create_upgrade_temp_file(parent, ".temps-upgrade-bin.")?;
    let staged_path = staged_file.path().to_path_buf();
    staged_file
        .as_file_mut()
        .write_all(new_binary)
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to write staged binary {} for {}: {}",
                staged_path.display(),
                binary_path.display(),
                e
            )
        })?;

    let staged_path = seal_staged_binary(staged_file)?;
    finalize_staged_binary(binary_path, staged_path)
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

/// Stream an EE binary tarball through the license-gated proxy into `dest`,
/// returning its lowercase hex SHA256.
///
/// Streaming counterpart of the OSS `download_asset_to_file`; the EE tarball is
/// the same size and would blow the same memory budget.
async fn download_ee_asset_to_file(
    api: &str,
    version: &str,
    asset: &str,
    license_jwt: &str,
    dest: &mut fs::File,
    dest_path: &Path,
) -> anyhow::Result<String> {
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
    stream_response_to_file(resp, dest, dest_path).await
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
    fn test_normalize_release_tag_accepts_real_tags() {
        for (input, expected) in [
            ("v1.2.3", "v1.2.3"),
            ("1.2.3", "v1.2.3"),
            ("v0.1.0-beta.55", "v0.1.0-beta.55"),
            (
                "v0.1.0-nightly.20260806.c64e8f98",
                "v0.1.0-nightly.20260806.c64e8f98",
            ),
            ("  v1.0.0  ", "v1.0.0"),
        ] {
            assert_eq!(
                normalize_release_tag(input).expect(input),
                expected,
                "should accept {input}"
            );
        }
    }

    #[test]
    fn test_normalize_release_tag_blocks_path_traversal() {
        // The exploit this validation exists for: the `url` crate resolves
        // `..` segments, so an unvalidated tag escapes gotempsh/temps and
        // reaches an arbitrary repository's release — whose asset would then
        // be downloaded, checksum-matched against ITS OWN published hash,
        // executed by the preflight and installed over the running binary.
        for input in [
            "v/../../../../../rust-lang/rust/releases/latest",
            "v1.2.3/../../../../../owner/repo/releases/latest",
            "../../owner/repo/releases/latest",
            "v1.2.3/..",
            "v1.2.3/extra",
            "v1.2.3%2f..%2fowner",
        ] {
            assert!(
                normalize_release_tag(input).is_err(),
                "must reject traversal: {input}"
            );
        }
    }

    #[test]
    fn test_normalize_release_tag_blocks_malformed_tags() {
        for input in [
            "",
            "   ",
            "v",
            "v1.2",
            "v1.2.3.4",
            "v1.2.x",
            "v1.2.3-beta 4",
            "v1.2.3-",
            "v1.2.3-beta..4",
            "v1.2.3?foo=bar",
            "v1.2.3#frag",
            "v1.2.3@evil.com",
            "http://evil.com/v1.2.3",
        ] {
            assert!(
                normalize_release_tag(input).is_err(),
                "must reject malformed tag: {input:?}"
            );
        }
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

    /// Build a gzipped tarball containing a single `temps` entry.
    fn tarball_with_binary(content: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, "temps", content).unwrap();
            builder.finish().unwrap();
        }
        encoder.finish().unwrap()
    }

    fn upgrade_scratch_paths(parent: &Path) -> Vec<PathBuf> {
        let mut paths = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".temps-upgrade-"))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    async fn spawn_raw_http_server(
        response_parts: Vec<Vec<u8>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            for part in response_parts {
                socket.write_all(&part).await.unwrap();
                socket.flush().await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        (format!("http://{address}/asset"), server)
    }

    #[test]
    fn test_verify_computed_checksum_matches_case_insensitively() {
        let computed = "AABBCC";
        let checksum_text = "aabbcc  temps-linux-amd64.tar.gz";
        assert!(verify_computed_checksum(computed, checksum_text).is_ok());
    }

    #[test]
    fn test_verify_computed_checksum_reports_both_hashes_on_mismatch() {
        let err = verify_computed_checksum("aaaa", "bbbb  temps.tar.gz")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Checksum mismatch"));
        assert!(err.contains("aaaa"));
        assert!(err.contains("bbbb"));
    }

    #[tokio::test]
    async fn test_download_asset_to_file_multiple_chunks_writes_all_bytes_and_sha() {
        use sha2::{Digest, Sha256};

        let header = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        let (url, server) = spawn_raw_http_server(vec![
            [header.as_slice(), b"5\r\nhello\r\n"].concat(),
            b"1\r\n \r\n".to_vec(),
            b"5\r\nworld\r\n0\r\n\r\n".to_vec(),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut download = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let download_path = download.path().to_path_buf();

        let computed = download_asset_to_file(&url, download.as_file_mut(), &download_path)
            .await
            .unwrap();

        server.await.unwrap();
        let expected_bytes = b"hello world";
        assert_eq!(std::fs::read(&download_path).unwrap(), expected_bytes);
        assert_eq!(
            computed,
            hex::encode(Sha256::digest(expected_bytes)),
            "the digest must cover every streamed response chunk"
        );
    }

    #[tokio::test]
    async fn test_download_asset_to_file_truncated_response_errors_and_temp_cleans_on_drop() {
        let header = b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n";
        let (url, server) =
            spawn_raw_http_server(vec![[header.as_slice(), b"short"].concat()]).await;
        let dir = tempfile::tempdir().unwrap();
        let download_path;

        {
            let mut download = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
            download_path = download.path().to_path_buf();

            let error = download_asset_to_file(&url, download.as_file_mut(), &download_path)
                .await
                .unwrap_err()
                .to_string();

            assert!(
                error.contains("Failed to read download response"),
                "error was: {error}"
            );
            assert!(download_path.exists());
        }

        server.await.unwrap();
        assert!(!download_path.exists());
        assert!(upgrade_scratch_paths(dir.path()).is_empty());
    }

    #[test]
    fn test_checksum_mismatch_preserves_target_and_cleans_download_temp() {
        use sha2::{Digest, Sha256};

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("temps");
        let original = b"existing-production-binary";
        std::fs::write(&target, original).unwrap();

        let verification_error = {
            let mut download = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
            download
                .as_file_mut()
                .write_all(b"downloaded-but-untrusted")
                .unwrap();
            let computed = hex::encode(Sha256::digest(b"downloaded-but-untrusted"));

            verify_computed_checksum(
                &computed,
                "0000000000000000000000000000000000000000000000000000000000000000  temps.tar.gz",
            )
        };

        assert!(verification_error.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert!(upgrade_scratch_paths(dir.path()).is_empty());
    }

    #[test]
    fn test_extract_binary_from_tarball_file_streams_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut tarball = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let tarball_path = tarball.path().to_path_buf();
        tarball
            .as_file_mut()
            .write_all(&tarball_with_binary(b"fake-binary-content"))
            .unwrap();
        let mut dest = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let dest_path = dest.path().to_path_buf();

        let written = extract_binary_from_tarball_file(
            tarball.as_file_mut(),
            &tarball_path,
            dest.as_file_mut(),
            &dest_path,
        )
        .unwrap();
        assert_eq!(written, b"fake-binary-content".len() as u64);
        assert_eq!(std::fs::read(&dest_path).unwrap(), b"fake-binary-content");
    }

    /// The streaming path must produce exactly what the in-memory path does,
    /// byte for byte, for the same tarball.
    #[test]
    fn test_extract_binary_from_tarball_file_matches_in_memory_path() {
        let dir = tempfile::tempdir().unwrap();
        // Big enough that `read_to_end` would have grown its Vec more than once.
        let content: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        let tarball_bytes = tarball_with_binary(&content);
        let mut tarball = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let tarball_path = tarball.path().to_path_buf();
        tarball.as_file_mut().write_all(&tarball_bytes).unwrap();
        let mut dest = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let dest_path = dest.path().to_path_buf();

        extract_binary_from_tarball_file(
            tarball.as_file_mut(),
            &tarball_path,
            dest.as_file_mut(),
            &dest_path,
        )
        .unwrap();

        let streamed = std::fs::read(&dest_path).unwrap();
        let in_memory = extract_binary_from_tarball(&tarball_bytes).unwrap();
        assert_eq!(streamed, in_memory);
        assert_eq!(streamed, content);
    }

    #[test]
    fn test_extract_binary_from_tarball_file_rejects_symlink_named_temps() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let dir = tempfile::tempdir().unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o777);
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_cksum();
            builder
                .append_link(&mut header, "temps", "/etc/passwd")
                .unwrap();
            builder.finish().unwrap();
        }
        let mut tarball = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let tarball_path = tarball.path().to_path_buf();
        tarball
            .as_file_mut()
            .write_all(&encoder.finish().unwrap())
            .unwrap();
        let mut dest = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let dest_path = dest.path().to_path_buf();

        let err = extract_binary_from_tarball_file(
            tarball.as_file_mut(),
            &tarball_path,
            dest.as_file_mut(),
            &dest_path,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a regular file"), "error was: {err}");
        drop(dest);
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_extract_binary_from_tarball_file_rejects_empty_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut tarball = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let tarball_path = tarball.path().to_path_buf();
        tarball
            .as_file_mut()
            .write_all(&tarball_with_binary(b""))
            .unwrap();
        let mut dest = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let dest_path = dest.path().to_path_buf();

        let err = extract_binary_from_tarball_file(
            tarball.as_file_mut(),
            &tarball_path,
            dest.as_file_mut(),
            &dest_path,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("is empty"), "error was: {err}");
        drop(dest);
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_replace_binary_removes_staged_file_when_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        // A directory cannot be replaced by a file rename, so finalize fails
        // after the staged file is written and synced.
        let target = dir.path().join("temps");
        std::fs::create_dir(&target).unwrap();

        assert!(replace_binary(&target, b"new-binary").is_err());
        assert!(upgrade_scratch_paths(dir.path()).is_empty());
    }

    #[test]
    fn test_replace_binary_valid_bytes_replaces_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("temps");
        std::fs::write(&target, b"old-binary").unwrap();

        replace_binary(&target, b"new-binary").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new-binary");
        assert!(upgrade_scratch_paths(dir.path()).is_empty());
    }

    #[test]
    fn test_extract_binary_from_tarball_file_not_found() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let dir = tempfile::tempdir().unwrap();
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
        let mut tarball = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let tarball_path = tarball.path().to_path_buf();
        tarball
            .as_file_mut()
            .write_all(&encoder.finish().unwrap())
            .unwrap();
        let mut dest = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let dest_path = dest.path().to_path_buf();

        let err = extract_binary_from_tarball_file(
            tarball.as_file_mut(),
            &tarball_path,
            dest.as_file_mut(),
            &dest_path,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not found"));
        drop(dest);
        assert!(!dest_path.exists());
    }

    #[test]
    fn test_extract_binary_from_tarball_file_corrupt_gzip_cleans_staged_temp() {
        let dir = tempfile::tempdir().unwrap();
        let mut tarball = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let tarball_path = tarball.path().to_path_buf();
        tarball
            .as_file_mut()
            .write_all(b"not-a-complete-gzip-stream")
            .unwrap();
        let mut dest = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let dest_path = dest.path().to_path_buf();

        let err = extract_binary_from_tarball_file(
            tarball.as_file_mut(),
            &tarball_path,
            dest.as_file_mut(),
            &dest_path,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(&tarball_path.display().to_string()));
        drop(dest);
        drop(tarball);
        assert!(!dest_path.exists());
        assert!(upgrade_scratch_paths(dir.path()).is_empty());
    }

    #[test]
    fn test_create_upgrade_temp_file_uses_random_distinct_prefixed_names() {
        let dir = tempfile::tempdir().unwrap();
        let first = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let second = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();

        assert_ne!(first.path(), second.path());
        for file in [&first, &second] {
            let name = file.path().file_name().unwrap().to_string_lossy();
            assert!(name.starts_with(".temps-upgrade-bin."), "name was: {name}");
            assert!(file.path().exists());
        }
    }

    #[test]
    fn test_create_upgrade_temp_file_returns_atomically_created_open_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
        let path = file.path().to_path_buf();

        file.as_file_mut().write_all(b"owned").unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"owned");
    }

    #[test]
    fn test_create_upgrade_temp_file_removes_file_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = {
            let file = create_upgrade_temp_file(dir.path(), ".temps-upgrade-dl.").unwrap();
            let path = file.path().to_path_buf();
            assert!(path.exists());
            path
        };

        assert!(!path.exists());
    }

    #[test]
    fn test_finalize_staged_binary_renames_and_marks_executable() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("temps");
        std::fs::write(&target, b"old").unwrap();
        let mut staged = create_upgrade_temp_file(dir.path(), ".temps-upgrade-bin.").unwrap();
        let staged_path = staged.path().to_path_buf();
        staged.as_file_mut().write_all(b"new").unwrap();

        let staged = seal_staged_binary(staged).unwrap();
        finalize_staged_binary(&target, staged).unwrap();

        assert!(!staged_path.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"new");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
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

    // ── Startup update notifier ──────────────────────────────────────────
    //
    // The notifier decides (a) which channel an installed binary tracks and
    // (b) whether a published tag is strictly newer. Both rules are pinned
    // here because a regression means either nagging stable hosts about
    // betas or silently never telling anyone about releases.

    #[test]
    fn test_installed_channel_stable_for_plain_tag() {
        assert_eq!(
            UpgradeChannel::for_installed_version("v1.2.0"),
            UpgradeChannel::Stable
        );
    }

    #[test]
    fn test_installed_channel_beta_for_prerelease_tag() {
        assert_eq!(
            UpgradeChannel::for_installed_version("v1.2.0-beta.4"),
            UpgradeChannel::Beta
        );
        assert_eq!(
            UpgradeChannel::for_installed_version("v1.2.0-rc.1"),
            UpgradeChannel::Beta
        );
    }

    #[test]
    fn test_installed_channel_nightly_for_nightly_tag() {
        assert_eq!(
            UpgradeChannel::for_installed_version("v1.2.0-nightly.20260727.abc1234"),
            UpgradeChannel::Nightly
        );
    }

    #[test]
    fn test_is_newer_version_core_ordering() {
        assert!(is_newer_version("v0.2.0", "v0.1.9"));
        assert!(is_newer_version("v1.0.0", "v0.9.9"));
        assert!(is_newer_version("v0.1.10", "v0.1.9"));
        assert!(!is_newer_version("v0.1.9", "v0.1.9"));
        // Never flag a downgrade: a host running a newer (e.g. unreleased)
        // version than the latest published tag must stay silent.
        assert!(!is_newer_version("v0.1.8", "v0.1.9"));
    }

    #[test]
    fn test_is_newer_version_prerelease_ordering() {
        // Release beats its own prereleases…
        assert!(is_newer_version("v1.0.0", "v1.0.0-beta.2"));
        assert!(!is_newer_version("v1.0.0-beta.2", "v1.0.0"));
        // …prereleases order among themselves numerically…
        assert!(is_newer_version("v1.0.0-beta.10", "v1.0.0-beta.2"));
        // …and a prerelease of the NEXT version beats the current release.
        assert!(is_newer_version("v1.1.0-beta.1", "v1.0.0"));
    }

    #[test]
    fn test_is_newer_version_unparsable_tags_stay_silent() {
        // Fork/dev tags that aren't vX.Y.Z-shaped must never trigger the
        // banner in either position.
        assert!(!is_newer_version("nightly", "v1.0.0"));
        assert!(!is_newer_version("v2.0.0", "local-dev"));
        assert!(!is_newer_version("v1.2", "v1.1.0"));
        assert!(!is_newer_version("v1.2.3.4", "v1.1.0"));
    }

    #[test]
    fn test_update_check_interval_defaults_to_two_hours() {
        assert_eq!(
            DEFAULT_UPDATE_CHECK_INTERVAL,
            std::time::Duration::from_secs(2 * 60 * 60)
        );
    }

    #[test]
    fn test_update_check_interval_parser_accepts_bounded_whole_hours() {
        assert_eq!(parse_update_check_interval_hours("1"), Ok(1));
        assert_eq!(parse_update_check_interval_hours(" 2 "), Ok(2));
        assert_eq!(parse_update_check_interval_hours("168"), Ok(168));
    }

    #[test]
    fn test_update_check_interval_parser_rejects_invalid_values() {
        assert!(matches!(
            parse_update_check_interval_hours("0"),
            Err(UpdateCheckIntervalError::OutOfRange { hours: 0 })
        ));
        assert!(matches!(
            parse_update_check_interval_hours("169"),
            Err(UpdateCheckIntervalError::OutOfRange { hours: 169 })
        ));
        assert!(matches!(
            parse_update_check_interval_hours("1.5"),
            Err(UpdateCheckIntervalError::InvalidNumber { .. })
        ));
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
    fn channel_beta_excludes_nightly_builds() {
        // Beta must never silently resolve to an automated nightly — an
        // operator who deliberately opts into beta expects a `-beta.N` cut,
        // not whatever CI cut overnight. Nightly is a separate, opt-in
        // channel.
        let nightly = release("v1.3.0-nightly.20260727.abc1234", true, false);
        assert!(!UpgradeChannel::Beta.includes(&nightly));
    }

    #[test]
    fn channel_nightly_includes_only_nightly_tags() {
        let stable = release("v1.2.0", false, false);
        let beta = release("v1.3.0-beta.1", true, false);
        let nightly = release("v1.3.0-nightly.20260727.abc1234", true, false);
        let draft_nightly = release("v1.4.0-nightly.20260728.def5678", true, true);

        assert!(!UpgradeChannel::Nightly.includes(&stable));
        assert!(!UpgradeChannel::Nightly.includes(&beta));
        assert!(UpgradeChannel::Nightly.includes(&nightly));
        assert!(!UpgradeChannel::Nightly.includes(&draft_nightly));
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
