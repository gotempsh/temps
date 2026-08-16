//! `temps firecracker` — provision the Firecracker microVM sandbox backend.
//!
//! ADR-029 §8: one-command enablement of the Firecracker backend. `setup`
//! is idempotent — every stage checks current state before mutating and
//! re-running after a partial failure resumes where it left off. Stages:
//!
//!   1. Preflight     — KVM/virt/arch/tooling checks, actionable failures
//!   2. Binaries      — pinned `firecracker` + `jailer` release, sha256-verified
//!   3. Guest kernel  — pinned `vmlinux`, digest-verified
//!   4. Network       — `temps-fc-br0` bridge + NAT (root; `--network-only`)
//!   5. Jailer uids   — unprivileged uid/gid range, collision-checked
//!   6. Smoke test    — boot a real microVM end-to-end, expect a marker
//!
//! Only a successful smoke test marks the backend enabled in `state.json`;
//! the sandbox provider's `is_available` probes that file plus the live
//! host state, so a half-provisioned host never advertises Firecracker.

use clap::{Args, Subcommand};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::upgrade::{download_asset, download_asset_text, verify_checksum};

/// Supported Firecracker release. Pinned deliberately (snapshot format and
/// API are version-coupled); bump alongside the sandbox provider, never
/// resolve "latest" at setup time.
pub const FIRECRACKER_VERSION: &str = "v1.16.1";

const FC_RELEASE_BASE: &str =
    "https://github.com/firecracker-microvm/firecracker/releases/download";

/// Pinned guest kernel. Served from the Firecracker CI artifact bucket until
/// Temps CI publishes its own `vmlinux` (ADR-029 §4); the digest below is
/// what makes the pin trustworthy regardless of the source.
const KERNEL_VERSION: &str = "6.1.141";
const KERNEL_BASE: &str = "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.13";
const KERNEL_SHA256_X86_64: &str =
    "b36a4a1b10f33b9cfdcde3d1a787d9c090556a3edb211cd06d1f3f9a6c7e8724";
const KERNEL_SHA256_AARCH64: &str =
    "69aa3308219ec1a070bc9a8e7f80c3b34056fed8ae05efb44e55f73b31adde44";

const BRIDGE_NAME: &str = "temps-fc-br0";
const TAP_NAME_PREFIX: &str = "temps-fc-tap";
const SMOKE_MARKER: &str = "TEMPS_FC_SMOKE_OK";
const SMOKE_IMAGE: &str = "busybox:stable";
const SMOKE_TIMEOUT: Duration = Duration::from_secs(60);

/// Manage the Firecracker microVM sandbox backend
#[derive(Args)]
pub struct FirecrackerCommand {
    #[command(subcommand)]
    pub command: FirecrackerSubcommand,
}

#[derive(Subcommand)]
pub enum FirecrackerSubcommand {
    /// Provision the Firecracker backend (binaries, kernel, network, smoke test)
    Setup(FirecrackerSetupCommand),
}

#[derive(Args)]
pub struct FirecrackerSetupCommand {
    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Run all probes without mutating the host, then exit non-zero on failures
    #[arg(long)]
    pub check: bool,

    /// Only run the network stage (bridge + NAT). Requires root; printed by
    /// a non-root `setup` run so the privileged part can be done separately.
    #[arg(long)]
    pub network_only: bool,

    /// Remove bridge, NAT rules, binaries, kernel, and cached state
    #[arg(long)]
    pub uninstall: bool,

    /// Skip the smoke-test VM boot (not recommended; backend stays disabled)
    #[arg(long)]
    pub skip_smoke: bool,

    /// Gateway address + subnet for the sandbox bridge, CIDR notation
    #[arg(long, default_value = "192.168.222.1/24")]
    pub subnet: String,

    /// First uid/gid of the jailer's unprivileged identity range
    #[arg(long, default_value_t = 52000)]
    pub uid_base: u32,

    /// Number of uids/gids reserved for jailed VMs
    #[arg(long, default_value_t = 1000)]
    pub uid_count: u32,

    /// Number of persistent TAP devices to pre-create (one concurrent
    /// networked VM each). Created root-owned-but-user-attachable so the
    /// unprivileged server can wire VMs without privileges.
    #[arg(long, default_value_t = 32)]
    pub tap_count: u32,

    /// User allowed to attach the TAP devices (defaults to the user who
    /// invoked sudo, or $USER)
    #[arg(long)]
    pub tap_user: Option<String>,

    /// Path to the static musl `temps-vm-agent` guest binary to install.
    /// Defaults to a `temps-vm-agent` file next to the running executable.
    #[arg(long)]
    pub vm_agent_bin: Option<PathBuf>,
}

/// Provisioned state, written only after stages succeed. The sandbox
/// provider's `is_available` reads this and re-verifies the host.
#[derive(Debug, Serialize, Deserialize, Default)]
struct FirecrackerState {
    firecracker_version: String,
    kernel_path: String,
    kernel_version: String,
    bridge: String,
    subnet: String,
    uid_base: u32,
    uid_count: u32,
    #[serde(default)]
    tap_count: u32,
    network_ready: bool,
    smoke_ok: bool,
}

struct Dirs {
    root: PathBuf,
    bin: PathBuf,
    kernel: PathBuf,
    smoke: PathBuf,
    state_file: PathBuf,
}

impl Dirs {
    fn new(data_dir: &Path) -> Self {
        let root = data_dir.join("firecracker");
        Self {
            bin: root.join("bin"),
            kernel: root.join("kernel"),
            smoke: root.join("smoke"),
            state_file: root.join("state.json"),
            root,
        }
    }

    fn firecracker_bin(&self) -> PathBuf {
        self.bin.join("firecracker")
    }

    fn jailer_bin(&self) -> PathBuf {
        self.bin.join("jailer")
    }

    fn kernel_file(&self) -> PathBuf {
        self.kernel.join(format!("vmlinux-{}", KERNEL_VERSION))
    }
}

// ── Output helpers (same visual language as `temps doctor`) ─────────────

fn pass(label: &str, msg: impl AsRef<str>) {
    println!(
        "  {} {} {}",
        "PASS".bright_green().bold(),
        format!("{}:", label).bright_white(),
        msg.as_ref()
    );
}

fn warn(label: &str, msg: impl AsRef<str>) {
    println!(
        "  {} {} {}",
        "WARN".bright_yellow().bold(),
        format!("{}:", label).bright_white(),
        msg.as_ref().bright_yellow()
    );
}

fn fail(label: &str, msg: impl AsRef<str>) {
    println!(
        "  {} {} {}",
        "FAIL".bright_red().bold(),
        format!("{}:", label).bright_white(),
        msg.as_ref().bright_red()
    );
}

fn info(label: &str, msg: impl AsRef<str>) {
    println!(
        "  {} {} {}",
        "INFO".bright_cyan().bold(),
        format!("{}:", label).bright_white(),
        msg.as_ref()
    );
}

fn section(title: &str) {
    println!();
    println!("{}", format!("  {}", title).bright_yellow().bold());
}

impl FirecrackerCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        match self.command {
            FirecrackerSubcommand::Setup(cmd) => cmd.execute(),
        }
    }
}

impl FirecrackerSetupCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.run())
    }

    async fn run(self) -> anyhow::Result<()> {
        let data_dir = resolve_data_dir(&self.data_dir);
        let dirs = Dirs::new(&data_dir);

        if self.uninstall {
            return self.uninstall(&dirs);
        }

        println!();
        let title = if self.check {
            "  Temps Firecracker - Readiness Check"
        } else {
            "  Temps Firecracker - Backend Setup"
        };
        println!("{}", title.bright_white().bold());
        println!("{}", "  ===================================".bright_cyan());
        info("Data directory", dirs.root.display().to_string());

        if self.network_only {
            let failures = self.stage_network(&dirs, false)?;
            if failures == 0 {
                println!();
                println!("{}", "  Network stage complete.".bright_green().bold());
                // Record network readiness so a prior non-root full setup
                // flips to fully provisioned without re-running everything.
                if let Some(mut state) = read_state(&dirs) {
                    state.network_ready = true;
                    state.tap_count = self.tap_count;
                    write_state(&dirs, &state)?;
                }
                return Ok(());
            }
            anyhow::bail!("network stage failed");
        }

        let mut failures = 0u32;

        section("Preflight");
        failures += self.stage_preflight().await?;

        if self.check {
            section("Provisioned state");
            failures += self.stage_check_provisioned(&dirs);
            println!();
            if failures > 0 {
                println!(
                    "{}",
                    format!("  {} check(s) failed.", failures)
                        .bright_red()
                        .bold()
                );
                std::process::exit(1);
            }
            println!("{}", "  All checks passed.".bright_green().bold());
            return Ok(());
        }

        if failures > 0 {
            println!();
            anyhow::bail!(
                "preflight failed ({} check(s)) — fix the failures above and re-run \
                 `temps firecracker setup`",
                failures
            );
        }

        std::fs::create_dir_all(&dirs.bin)?;
        std::fs::create_dir_all(&dirs.kernel)?;

        section("Firecracker binaries");
        self.stage_binaries(&dirs).await?;
        self.stage_vm_agent(&dirs)?;

        section("Guest kernel");
        self.stage_kernel(&dirs).await?;

        section("Network");
        let network_failures = self.stage_network(&dirs, true)?;
        let network_ready = network_failures == 0;

        section("Jailer identities");
        self.stage_jailer_uids()?;

        let smoke_ok = if self.skip_smoke {
            section("Smoke test");
            warn(
                "Skipped",
                "--skip-smoke set; backend will NOT be marked enabled",
            );
            false
        } else {
            section("Smoke test");
            self.stage_smoke(&dirs).await?
        };

        let state = FirecrackerState {
            firecracker_version: FIRECRACKER_VERSION.to_string(),
            kernel_path: dirs.kernel_file().display().to_string(),
            kernel_version: KERNEL_VERSION.to_string(),
            bridge: BRIDGE_NAME.to_string(),
            subnet: self.subnet.clone(),
            uid_base: self.uid_base,
            uid_count: self.uid_count,
            tap_count: if network_ready { self.tap_count } else { 0 },
            network_ready,
            smoke_ok,
        };
        write_state(&dirs, &state)?;

        println!();
        if smoke_ok && network_ready {
            println!(
                "{}",
                "  Firecracker backend is provisioned and enabled."
                    .bright_green()
                    .bold()
            );
            println!("  Select it in Settings → Sandbox, or per sandbox with \"backend\": \"firecracker\".");
        } else if smoke_ok {
            println!(
                "{}",
                "  Firecracker works, but the network stage is pending."
                    .bright_yellow()
                    .bold()
            );
            println!(
                "  Run {} to finish (VMs boot but have no egress until then).",
                "sudo temps firecracker setup --network-only".bright_white()
            );
        } else {
            anyhow::bail!("smoke test did not pass; backend left disabled");
        }
        Ok(())
    }

    // ── Stage 1: preflight ──────────────────────────────────────────

    async fn stage_preflight(&self) -> anyhow::Result<u32> {
        let mut failures = 0u32;

        // Platform
        if std::env::consts::OS != "linux" {
            fail(
                "Platform",
                format!(
                    "Firecracker requires Linux (detected {})",
                    std::env::consts::OS
                ),
            );
            return Ok(1);
        }
        match fc_arch() {
            Ok(arch) => pass("Architecture", arch),
            Err(e) => {
                fail("Architecture", e.to_string());
                return Ok(1);
            }
        }

        // Host kernel version (KVM API we rely on is stable since 4.14)
        match std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            Ok(rel) => {
                let rel = rel.trim().to_string();
                let ok = parse_kernel_version(&rel)
                    .map(|(maj, min)| (maj, min) >= (4, 14))
                    .unwrap_or(false);
                if ok {
                    pass("Host kernel", &rel);
                } else {
                    fail("Host kernel", format!("{} (need >= 4.14)", rel));
                    failures += 1;
                }
            }
            Err(e) => {
                warn("Host kernel", format!("Could not read version: {}", e));
            }
        }

        // CPU virtualization extensions
        if std::env::consts::ARCH == "x86_64" {
            let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
            let has_virt = cpuinfo
                .lines()
                .filter(|l| l.starts_with("flags"))
                .any(|l| l.contains(" vmx") || l.contains(" svm"));
            if has_virt {
                pass("CPU virtualization", "vmx/svm present");
            } else {
                fail(
                    "CPU virtualization",
                    "No vmx/svm CPU flags. On a cloud VM, this host has no nested \
                     virtualization — use a bare-metal instance (e.g. AWS *.metal, \
                     Hetzner dedicated) or enable nested virt on the hypervisor.",
                );
                failures += 1;
            }
        }

        // /dev/kvm
        failures += check_kvm_access();

        // Required host tooling for rootfs builds (ADR-029 §4)
        for tool in ["mkfs.ext4", "ip"] {
            match find_in_path(tool) {
                Some(p) => pass(tool, p.display().to_string()),
                None => {
                    fail(
                        tool,
                        format!(
                            "Not found. Install it (Debian/Ubuntu: `apt install {}`).",
                            if tool == "mkfs.ext4" {
                                "e2fsprogs"
                            } else {
                                "iproute2"
                            }
                        ),
                    );
                    failures += 1;
                }
            }
        }

        // Docker daemon — the image toolchain in v1 (pull/build/export)
        match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => match tokio::time::timeout(Duration::from_secs(5), docker.ping()).await {
                Ok(Ok(_)) => pass("Docker", "Daemon reachable (image toolchain)"),
                _ => {
                    fail(
                        "Docker",
                        "Daemon not reachable. Firecracker sandboxes derive their \
                             root filesystems from Docker images; Docker must be running.",
                    );
                    failures += 1;
                }
            },
            Err(e) => {
                fail("Docker", format!("Cannot connect: {}", e));
                failures += 1;
            }
        }

        Ok(failures)
    }

    // ── `--check`: report provisioned state without mutating ────────

    fn stage_check_provisioned(&self, dirs: &Dirs) -> u32 {
        let mut failures = 0u32;

        // Binaries
        if dirs.firecracker_bin().exists() && dirs.jailer_bin().exists() {
            match installed_version(dirs) {
                Some(v) if v == FIRECRACKER_VERSION => {
                    pass("Binaries", format!("firecracker + jailer {}", v))
                }
                Some(v) => {
                    warn(
                        "Binaries",
                        format!(
                            "Installed {} but this build pins {} — re-run `temps firecracker setup`",
                            v, FIRECRACKER_VERSION
                        ),
                    );
                }
                None => warn("Binaries", "Present but version unknown"),
            }
        } else {
            fail("Binaries", "Not installed — run `temps firecracker setup`");
            failures += 1;
        }

        // Kernel
        if dirs.kernel_file().exists() {
            pass("Guest kernel", dirs.kernel_file().display().to_string());
        } else {
            fail(
                "Guest kernel",
                "Not downloaded — run `temps firecracker setup`",
            );
            failures += 1;
        }

        // Bridge
        if bridge_exists() {
            pass("Bridge", format!("{} present", BRIDGE_NAME));
        } else {
            fail(
                "Bridge",
                format!(
                    "{} missing — run `sudo temps firecracker setup --network-only`",
                    BRIDGE_NAME
                ),
            );
            failures += 1;
        }

        // State / smoke
        match read_state(dirs) {
            Some(s) if s.smoke_ok => pass("Smoke test", "Passed on last setup"),
            Some(_) => {
                fail("Smoke test", "Never passed — run `temps firecracker setup`");
                failures += 1;
            }
            None => {
                fail("State", "No state.json — run `temps firecracker setup`");
                failures += 1;
            }
        }

        failures
    }

    // ── Stage 2: pinned binaries ────────────────────────────────────

    async fn stage_binaries(&self, dirs: &Dirs) -> anyhow::Result<()> {
        if installed_version(dirs).as_deref() == Some(FIRECRACKER_VERSION)
            && dirs.firecracker_bin().exists()
            && dirs.jailer_bin().exists()
        {
            pass(
                "Binaries",
                format!("{} already installed", FIRECRACKER_VERSION),
            );
            return Ok(());
        }

        let arch = fc_arch()?;
        let tarball_name = format!("firecracker-{}-{}.tgz", FIRECRACKER_VERSION, arch);
        let base = format!("{}/{}", FC_RELEASE_BASE, FIRECRACKER_VERSION);

        println!("  Downloading {}...", tarball_name);
        let tarball = download_asset(&format!("{}/{}", base, tarball_name)).await?;
        let checksum =
            download_asset_text(&format!("{}/{}.sha256.txt", base, tarball_name)).await?;
        verify_checksum(&tarball, &checksum)?;
        pass("Checksum", "sha256 verified");

        // The release tarball layout is `release-{tag}-{arch}/<tool>-{tag}-{arch}`.
        let fc_entry = format!("firecracker-{}-{}", FIRECRACKER_VERSION, arch);
        let jailer_entry = format!("jailer-{}-{}", FIRECRACKER_VERSION, arch);
        let mut found_fc = false;
        let mut found_jailer = false;

        let decoder = flate2::read::GzDecoder::new(&tarball[..]);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let dest = if name == fc_entry {
                found_fc = true;
                dirs.firecracker_bin()
            } else if name == jailer_entry {
                found_jailer = true;
                dirs.jailer_bin()
            } else {
                continue;
            };
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            std::fs::write(&dest, &buf)?;
            set_executable(&dest)?;
        }
        if !found_fc || !found_jailer {
            anyhow::bail!(
                "release tarball did not contain expected binaries ({}, {})",
                fc_entry,
                jailer_entry
            );
        }
        std::fs::write(dirs.bin.join("VERSION"), FIRECRACKER_VERSION)?;

        // Sanity: the binary must run on this host.
        let out = std::process::Command::new(dirs.firecracker_bin())
            .arg("--version")
            .output()?;
        let version_line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        pass("Installed", version_line);
        Ok(())
    }

    /// Install the static musl guest agent — the only transport into a VM,
    /// injected into every rootfs by the provider. Sourced from `--vm-agent-bin`
    /// or a `temps-vm-agent` file next to the running executable (release
    /// tarballs ship it there; CI publishes it alongside the temps binary).
    fn stage_vm_agent(&self, dirs: &Dirs) -> anyhow::Result<()> {
        let source = self.vm_agent_bin.clone().or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.join("temps-vm-agent")))
                .filter(|p| p.exists())
        });
        let dest = dirs.bin.join("temps-vm-agent");
        match source {
            Some(src) if src.exists() => {
                std::fs::copy(&src, &dest)?;
                set_executable(&dest)?;
                pass("Guest agent", format!("Installed from {}", src.display()));
            }
            Some(src) => {
                anyhow::bail!("--vm-agent-bin {} does not exist", src.display());
            }
            None if dest.exists() => {
                pass("Guest agent", "Already installed");
            }
            None => {
                warn(
                    "Guest agent",
                    "temps-vm-agent binary not found (looked next to this executable). \
                     Firecracker sandboxes stay unavailable until it is installed — \
                     re-run with --vm-agent-bin <path>.",
                );
            }
        }
        Ok(())
    }

    // ── Stage 3: guest kernel ───────────────────────────────────────

    async fn stage_kernel(&self, dirs: &Dirs) -> anyhow::Result<()> {
        let arch = fc_arch()?;
        let expected_sha = match arch {
            "x86_64" => KERNEL_SHA256_X86_64,
            "aarch64" => KERNEL_SHA256_AARCH64,
            _ => unreachable!("fc_arch validated"),
        };
        let dest = dirs.kernel_file();

        if dest.exists() {
            let data = std::fs::read(&dest)?;
            if sha256_hex(&data) == expected_sha {
                pass(
                    "Guest kernel",
                    format!("vmlinux-{} present", KERNEL_VERSION),
                );
                return Ok(());
            }
            warn(
                "Guest kernel",
                "Digest mismatch on existing file, re-downloading",
            );
        }

        let url = format!("{}/{}/vmlinux-{}", KERNEL_BASE, arch, KERNEL_VERSION);
        println!("  Downloading vmlinux-{} ({})...", KERNEL_VERSION, arch);
        let data = download_asset(&url).await?;
        let got = sha256_hex(&data);
        if got != expected_sha {
            anyhow::bail!(
                "guest kernel digest mismatch\n  expected: {}\n  got:      {}",
                expected_sha,
                got
            );
        }
        std::fs::write(&dest, &data)?;
        pass(
            "Guest kernel",
            format!("vmlinux-{} downloaded, sha256 verified", KERNEL_VERSION),
        );
        Ok(())
    }

    // ── Stage 4: network (root) ─────────────────────────────────────

    /// Returns the number of failures. With `allow_defer`, a non-root run
    /// reports the pending sudo step instead of failing the whole setup —
    /// VMs can boot without egress, and the smoke test needs no network.
    fn stage_network(&self, _dirs: &Dirs, allow_defer: bool) -> anyhow::Result<u32> {
        let (gateway, subnet) = parse_subnet(&self.subnet)?;

        if !is_root() {
            if bridge_exists() {
                pass("Bridge", format!("{} already present", BRIDGE_NAME));
                return Ok(0);
            }
            if allow_defer {
                warn(
                    "Deferred",
                    "Network stage needs root. Run: sudo temps firecracker setup --network-only",
                );
                return Ok(1);
            }
            fail("Network", "This stage must run as root (sudo)");
            return Ok(1);
        }

        // Bridge (idempotent: `ip link add` fails if present, so check first)
        if !bridge_exists() {
            run_cmd("ip", &["link", "add", BRIDGE_NAME, "type", "bridge"])?;
        }
        run_cmd("ip", &["addr", "replace", &self.subnet, "dev", BRIDGE_NAME])?;
        run_cmd("ip", &["link", "set", BRIDGE_NAME, "up"])?;
        pass("Bridge", format!("{} up, gateway {}", BRIDGE_NAME, gateway));

        // IP forwarding, now and across reboots
        std::fs::write("/proc/sys/net/ipv4/ip_forward", "1")?;
        std::fs::write(
            "/etc/sysctl.d/99-temps-firecracker.conf",
            "net.ipv4.ip_forward = 1\n",
        )?;
        pass("IP forwarding", "Enabled (persisted via sysctl.d)");

        // NAT + forwarding. iptables (nft-backed on modern hosts) rather than
        // a separate nft table: Docker sets FORWARD policy DROP through
        // iptables, so our accept rules must live in the same ruleset to
        // interleave correctly. `-C || -A` keeps re-runs idempotent.
        let masq = [
            "-s".to_string(),
            subnet.clone(),
            "!".to_string(),
            "-d".to_string(),
            subnet.clone(),
            "-j".to_string(),
            "MASQUERADE".to_string(),
        ];
        iptables_ensure("nat", "POSTROUTING", &masq)?;
        iptables_ensure(
            "filter",
            "FORWARD",
            &[
                "-i".to_string(),
                BRIDGE_NAME.to_string(),
                "-j".to_string(),
                "ACCEPT".to_string(),
            ],
        )?;
        iptables_ensure(
            "filter",
            "FORWARD",
            &[
                "-o".to_string(),
                BRIDGE_NAME.to_string(),
                "-m".to_string(),
                "conntrack".to_string(),
                "--ctstate".to_string(),
                "RELATED,ESTABLISHED".to_string(),
                "-j".to_string(),
                "ACCEPT".to_string(),
            ],
        )?;
        pass("NAT", format!("Masquerade for {} installed", subnet));

        // Guest isolation (defense-in-depth; the full egress-scoping story is
        // ADR-013's credential proxy). Inserted at the TOP of FORWARD so they
        // beat the broad `-i bridge ACCEPT` above. Internet egress is
        // unaffected — these only drop the dangerous internal paths:
        //  - cloud instance metadata (169.254.169.254) — credential theft
        //  - the rest of link-local
        for dst in ["169.254.169.254/32", "169.254.0.0/16"] {
            iptables_insert(
                "filter",
                "FORWARD",
                &[
                    "-i".to_string(),
                    BRIDGE_NAME.to_string(),
                    "-d".to_string(),
                    dst.to_string(),
                    "-j".to_string(),
                    "DROP".to_string(),
                ],
            )?;
        }
        // Guest -> host: the guest only needs the host as a router (FORWARD),
        // never as a service endpoint, so drop traffic destined TO the host
        // from the bridge. Established replies are allowed first.
        iptables_insert(
            "filter",
            "INPUT",
            &[
                "-i".to_string(),
                BRIDGE_NAME.to_string(),
                "-j".to_string(),
                "DROP".to_string(),
            ],
        )?;
        iptables_insert(
            "filter",
            "INPUT",
            &[
                "-i".to_string(),
                BRIDGE_NAME.to_string(),
                "-m".to_string(),
                "conntrack".to_string(),
                "--ctstate".to_string(),
                "RELATED,ESTABLISHED".to_string(),
                "-j".to_string(),
                "ACCEPT".to_string(),
            ],
        )?;
        pass("Guest isolation", "Metadata + guest→host blocked");

        // Persistent TAP pool. Owned by the operating user so the (non-root)
        // server process — and Firecracker under it — can attach VMs to the
        // bridge without privileges. One TAP per concurrent networked VM.
        let tap_user = self.resolve_tap_user();
        let mut created = 0u32;
        for i in 0..self.tap_count {
            let tap = format!("{}{}", TAP_NAME_PREFIX, i);
            if !Path::new("/sys/class/net").join(&tap).exists() {
                run_cmd(
                    "ip",
                    &[
                        "tuntap", "add", "dev", &tap, "mode", "tap", "user", &tap_user,
                    ],
                )?;
                created += 1;
            }
            run_cmd("ip", &["link", "set", &tap, "master", BRIDGE_NAME])?;
            // Isolate ports from each other so one sandbox can't reach another
            // over the shared bridge (they can still reach the host bridge IP
            // for routing). `bridge` may be absent on minimal hosts — the
            // metadata/host DROP rules above are the load-bearing controls, so
            // treat isolation as best-effort.
            let _ = run_cmd("bridge", &["link", "set", "dev", &tap, "isolated", "on"]);
            run_cmd("ip", &["link", "set", &tap, "up"])?;
        }
        pass(
            "TAP pool",
            format!(
                "{} devices ready for user '{}' ({} newly created)",
                self.tap_count, tap_user, created
            ),
        );
        Ok(0)
    }

    fn resolve_tap_user(&self) -> String {
        self.tap_user.clone().unwrap_or_else(|| {
            std::env::var("SUDO_USER")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_else(|_| "root".to_string())
        })
    }

    // ── Stage 5: jailer uid range ───────────────────────────────────

    fn stage_jailer_uids(&self) -> anyhow::Result<()> {
        let range = self.uid_base..(self.uid_base + self.uid_count);
        let mut collisions = Vec::new();
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            for line in passwd.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if let (Some(name), Some(uid)) = (
                    fields.first(),
                    fields.get(2).and_then(|u| u.parse::<u32>().ok()),
                ) {
                    if range.contains(&uid) {
                        collisions.push(format!("{} (uid {})", name, uid));
                    }
                }
            }
        }
        if collisions.is_empty() {
            pass(
                "Uid range",
                format!(
                    "{}-{} free for jailed VMs",
                    self.uid_base,
                    self.uid_base + self.uid_count - 1
                ),
            );
            Ok(())
        } else {
            fail(
                "Uid range",
                format!(
                    "Collides with existing users: {} — pick another with --uid-base",
                    collisions.join(", ")
                ),
            );
            anyhow::bail!("jailer uid range collision")
        }
    }

    // ── Stage 6: smoke test ─────────────────────────────────────────

    /// Boot a real microVM end-to-end: busybox rootfs derived from a Docker
    /// image (exactly the ADR-029 §4 pipeline in miniature), pinned kernel,
    /// an init that prints a marker and powers off. Success = marker seen
    /// on the serial console and a clean Firecracker exit.
    async fn stage_smoke(&self, dirs: &Dirs) -> anyhow::Result<bool> {
        let _ = std::fs::remove_dir_all(&dirs.smoke);
        std::fs::create_dir_all(&dirs.smoke)?;

        println!("  Building smoke rootfs from {}...", SMOKE_IMAGE);
        let rootfs_img = match build_smoke_rootfs(&dirs.smoke).await {
            Ok(p) => p,
            Err(e) => {
                fail("Rootfs build", e.to_string());
                return Ok(false);
            }
        };

        let config = serde_json::json!({
            "boot-source": {
                "kernel_image_path": dirs.kernel_file(),
                "boot_args": format!(
                    "console=ttyS0 reboot=k panic=1 pci=off init=/temps-smoke-init"
                ),
            },
            "drives": [{
                "drive_id": "rootfs",
                "path_on_host": rootfs_img,
                "is_root_device": true,
                "is_read_only": false,
            }],
            "machine-config": { "vcpu_count": 1, "mem_size_mib": 128 },
        });
        let config_path = dirs.smoke.join("vm.json");
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;

        println!("  Booting smoke VM (1 vCPU, 128 MiB)...");
        let started = Instant::now();
        let child = tokio::process::Command::new(dirs.firecracker_bin())
            .arg("--no-api")
            .arg("--config-file")
            .arg(&config_path)
            .current_dir(&dirs.smoke)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let output = match tokio::time::timeout(SMOKE_TIMEOUT, child.wait_with_output()).await {
            Ok(out) => out?,
            Err(_) => {
                fail(
                    "Smoke test",
                    format!(
                        "VM did not exit within {}s (artifacts kept in {})",
                        SMOKE_TIMEOUT.as_secs(),
                        dirs.smoke.display()
                    ),
                );
                return Ok(false);
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stdout.contains(SMOKE_MARKER) {
            pass(
                "Smoke test",
                format!(
                    "microVM booted, ran init, and shut down in {:.2}s",
                    started.elapsed().as_secs_f64()
                ),
            );
            let _ = std::fs::remove_dir_all(&dirs.smoke);
            Ok(true)
        } else {
            let tail: String = stdout
                .lines()
                .chain(stderr.lines())
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n    ");
            fail(
                "Smoke test",
                format!(
                    "Marker not seen on serial console (exit: {:?}). Last output:\n    {}\n  Artifacts kept in {}",
                    output.status.code(),
                    tail,
                    dirs.smoke.display()
                ),
            );
            Ok(false)
        }
    }

    // ── Uninstall ───────────────────────────────────────────────────

    fn uninstall(&self, dirs: &Dirs) -> anyhow::Result<()> {
        println!();
        println!(
            "{}",
            "  Temps Firecracker - Uninstall".bright_white().bold()
        );
        println!("{}", "  ===================================".bright_cyan());

        if is_root() {
            let (_, subnet) = parse_subnet(&self.subnet)?;
            let masq = [
                "-s".to_string(),
                subnet.clone(),
                "!".to_string(),
                "-d".to_string(),
                subnet,
                "-j".to_string(),
                "MASQUERADE".to_string(),
            ];
            iptables_remove("nat", "POSTROUTING", &masq);
            iptables_remove(
                "filter",
                "FORWARD",
                &[
                    "-i".to_string(),
                    BRIDGE_NAME.to_string(),
                    "-j".to_string(),
                    "ACCEPT".to_string(),
                ],
            );
            iptables_remove(
                "filter",
                "FORWARD",
                &[
                    "-o".to_string(),
                    BRIDGE_NAME.to_string(),
                    "-m".to_string(),
                    "conntrack".to_string(),
                    "--ctstate".to_string(),
                    "RELATED,ESTABLISHED".to_string(),
                    "-j".to_string(),
                    "ACCEPT".to_string(),
                ],
            );
            // Guest-isolation rules added by the network stage.
            for dst in ["169.254.169.254/32", "169.254.0.0/16"] {
                iptables_remove(
                    "filter",
                    "FORWARD",
                    &[
                        "-i".to_string(),
                        BRIDGE_NAME.to_string(),
                        "-d".to_string(),
                        dst.to_string(),
                        "-j".to_string(),
                        "DROP".to_string(),
                    ],
                );
            }
            iptables_remove(
                "filter",
                "INPUT",
                &[
                    "-i".to_string(),
                    BRIDGE_NAME.to_string(),
                    "-j".to_string(),
                    "DROP".to_string(),
                ],
            );
            iptables_remove(
                "filter",
                "INPUT",
                &[
                    "-i".to_string(),
                    BRIDGE_NAME.to_string(),
                    "-m".to_string(),
                    "conntrack".to_string(),
                    "--ctstate".to_string(),
                    "RELATED,ESTABLISHED".to_string(),
                    "-j".to_string(),
                    "ACCEPT".to_string(),
                ],
            );
            let mut taps_removed = 0u32;
            // Sweep generously past the configured count — a prior setup may
            // have created more taps than the current flag value.
            for i in 0..self.tap_count.max(256) {
                let tap = format!("{}{}", TAP_NAME_PREFIX, i);
                if Path::new("/sys/class/net").join(&tap).exists() {
                    let _ = run_cmd("ip", &["link", "del", &tap]);
                    taps_removed += 1;
                }
            }
            if bridge_exists() {
                let _ = run_cmd("ip", &["link", "del", BRIDGE_NAME]);
            }
            let _ = std::fs::remove_file("/etc/sysctl.d/99-temps-firecracker.conf");
            pass(
                "Network",
                format!(
                    "Bridge, {} TAPs, NAT rules, and sysctl removed",
                    taps_removed
                ),
            );
        } else if bridge_exists() {
            warn(
                "Network",
                "Bridge/NAT left in place (needs root). Run: sudo temps firecracker setup --uninstall",
            );
        }

        if dirs.root.exists() {
            std::fs::remove_dir_all(&dirs.root)?;
            pass("Files", format!("Removed {}", dirs.root.display()));
        } else {
            info("Files", "Nothing to remove");
        }
        println!();
        Ok(())
    }
}

// ── Smoke rootfs build (Docker image → ext4, the ADR-029 §4 pipeline) ──

async fn build_smoke_rootfs(smoke_dir: &Path) -> anyhow::Result<PathBuf> {
    use futures::StreamExt;

    let docker = bollard::Docker::connect_with_local_defaults()?;

    // Pull the image (no-op if present)
    let mut pull = docker.create_image(
        Some(bollard::query_parameters::CreateImageOptions {
            from_image: Some(SMOKE_IMAGE.to_string()),
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(item) = pull.next().await {
        item.map_err(|e| anyhow::anyhow!("pulling {}: {}", SMOKE_IMAGE, e))?;
    }

    // Materialize the image filesystem: create (not run) a container, export it.
    let container = docker
        .create_container(
            None::<bollard::query_parameters::CreateContainerOptions>,
            bollard::models::ContainerCreateBody {
                image: Some(SMOKE_IMAGE.to_string()),
                cmd: Some(vec!["true".to_string()]),
                ..Default::default()
            },
        )
        .await?;

    let mut export = docker.export_container(&container.id);
    let mut tar_bytes = Vec::new();
    while let Some(chunk) = export.next().await {
        tar_bytes.extend_from_slice(&chunk?);
    }
    docker
        .remove_container(
            &container.id,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await?;

    // Extract as an unprivileged user: regular files, dirs, and symlinks
    // only. Device nodes can't be created without root and aren't needed —
    // the smoke init mounts devtmpfs.
    let rootfs_dir = smoke_dir.join("rootfs");
    std::fs::create_dir_all(&rootfs_dir)?;
    let mut archive = tar::Archive::new(&tar_bytes[..]);
    for entry in archive.entries()? {
        let mut entry = entry?;
        match entry.header().entry_type() {
            tar::EntryType::Regular
            | tar::EntryType::Directory
            | tar::EntryType::Symlink
            | tar::EntryType::Link => {
                // `unpack_in` sanitizes path traversal.
                let _ = entry.unpack_in(&rootfs_dir)?;
            }
            _ => {}
        }
    }

    // Init: prove userspace ran, then power off. Plain `echo` covers kernels
    // that automount devtmpfs; the explicit mount + /dev/console write covers
    // ones that don't. `reboot -f` from PID 1 exits the VMM cleanly.
    let init_path = rootfs_dir.join("temps-smoke-init");
    std::fs::write(
        &init_path,
        format!(
            "#!/bin/sh\n\
             echo {marker}\n\
             /bin/busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null\n\
             echo {marker} > /dev/console 2>/dev/null\n\
             /bin/busybox reboot -f\n",
            marker = SMOKE_MARKER
        ),
    )?;
    set_executable(&init_path)?;

    // 64 MiB ext4, populated without root or loop devices via `mkfs.ext4 -d`.
    let img = smoke_dir.join("rootfs.ext4");
    let out = std::process::Command::new("mkfs.ext4")
        .args(["-q", "-F", "-b", "4096", "-d"])
        .arg(&rootfs_dir)
        .arg(&img)
        .arg("16384")
        .output()?;
    if !out.status.success() {
        anyhow::bail!("mkfs.ext4 failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(img)
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn resolve_data_dir(flag: &Option<PathBuf>) -> PathBuf {
    if let Some(d) = flag {
        d.clone()
    } else if let Ok(d) = std::env::var("TEMPS_DATA_DIR") {
        PathBuf::from(d)
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".temps")
    }
}

fn fc_arch() -> anyhow::Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => anyhow::bail!(
            "Firecracker supports x86_64 and aarch64 (detected {})",
            other
        ),
    }
}

fn parse_kernel_version(rel: &str) -> Option<(u32, u32)> {
    let mut parts = rel.split(['.', '-']);
    let maj = parts.next()?.parse().ok()?;
    let min = parts.next()?.parse().ok()?;
    Some((maj, min))
}

/// KVM availability + access. Returns the number of failures and prints
/// remediation for the exact cause (missing module/nested virt vs. perms).
fn check_kvm_access() -> u32 {
    let kvm = Path::new("/dev/kvm");
    if !kvm.exists() {
        fail(
            "/dev/kvm",
            "Missing. Load the module (`sudo modprobe kvm_intel` or `kvm_amd`). \
             If this host is itself a VM, its hypervisor must expose nested \
             virtualization.",
        );
        return 1;
    }
    match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
        Ok(_) => {
            pass("/dev/kvm", "Accessible");
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let user = std::env::var("USER").unwrap_or_else(|_| "<user>".to_string());
            // A stale session is the common trap: the user already ran
            // usermod but hasn't re-logged in, so the group isn't effective
            // yet. ACLs are NOT a durable alternative — udev re-applies
            // static device permissions and silently drops them.
            if user_in_kvm_group(&user) {
                fail(
                    "/dev/kvm",
                    format!(
                        "Permission denied, but '{}' IS in the kvm group — this login \
                         session predates the membership. Log out and back in, or run \
                         commands under `sg kvm -c '...'` for this session.",
                        user
                    ),
                );
            } else {
                fail(
                    "/dev/kvm",
                    format!(
                        "Permission denied. Add yourself to the kvm group and re-login: \
                         `sudo usermod -aG kvm {}`",
                        user
                    ),
                );
            }
            1
        }
        Err(e) => {
            fail("/dev/kvm", format!("Cannot open: {}", e));
            1
        }
    }
}

/// True when `/etc/group` lists the user as a member of `kvm` (i.e. the
/// membership exists but may not be effective in the current session).
fn user_in_kvm_group(user: &str) -> bool {
    let Ok(groups) = std::fs::read_to_string("/etc/group") else {
        return false;
    };
    groups.lines().any(|line| {
        let mut fields = line.split(':');
        fields.next() == Some("kvm")
            && fields
                .nth(2)
                .is_some_and(|members| members.split(',').any(|m| m == user))
    })
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    // sbin dirs are often absent from non-root PATHs but the tools live there
    let candidates = path.split(':').map(PathBuf::from).chain(
        ["/usr/sbin", "/sbin", "/usr/local/sbin"]
            .iter()
            .map(PathBuf::from),
    );
    for dir in candidates {
        let p = dir.join(bin);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn installed_version(dirs: &Dirs) -> Option<String> {
    std::fs::read_to_string(dirs.bin.join("VERSION"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn bridge_exists() -> bool {
    Path::new("/sys/class/net").join(BRIDGE_NAME).exists()
}

fn is_root() -> bool {
    // Safety: geteuid has no failure modes.
    unsafe { libc::geteuid() == 0 }
}

fn set_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Split "192.168.222.1/24" into the gateway IP and the subnet in network
/// notation ("192.168.222.0/24").
fn parse_subnet(cidr: &str) -> anyhow::Result<(String, String)> {
    let net: ipnet::Ipv4Net = cidr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --subnet '{}': {}", cidr, e))?;
    let gateway = net.addr().to_string();
    let subnet = format!("{}/{}", net.network(), net.prefix_len());
    Ok((gateway, subnet))
}

fn run_cmd(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let resolved = find_in_path(bin).unwrap_or_else(|| PathBuf::from(bin));
    let out = std::process::Command::new(&resolved).args(args).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "`{} {}` failed: {}",
            bin,
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn iptables_rule_args<'a>(
    table: &'a str,
    action: &'a str,
    chain: &'a str,
    rule: &'a [String],
) -> Vec<&'a str> {
    let mut args = vec!["-t", table, action, chain];
    args.extend(rule.iter().map(|s| s.as_str()));
    args
}

/// Append an iptables rule unless an identical one exists (`-C` probe).
fn iptables_ensure(table: &str, chain: &str, rule: &[String]) -> anyhow::Result<()> {
    let iptables =
        find_in_path("iptables").ok_or_else(|| anyhow::anyhow!("iptables not found on PATH"))?;
    let check = std::process::Command::new(&iptables)
        .args(iptables_rule_args(table, "-C", chain, rule))
        .output()?;
    if check.status.success() {
        return Ok(());
    }
    let add = std::process::Command::new(&iptables)
        .args(iptables_rule_args(table, "-A", chain, rule))
        .output()?;
    if !add.status.success() {
        anyhow::bail!(
            "iptables -t {} -A {} failed: {}",
            table,
            chain,
            String::from_utf8_lossy(&add.stderr).trim()
        );
    }
    Ok(())
}

/// Insert a rule at the TOP of a chain unless already present. Used for the
/// guest-isolation DROP rules, which must precede the broad forward-ACCEPT.
fn iptables_insert(table: &str, chain: &str, rule: &[String]) -> anyhow::Result<()> {
    let iptables =
        find_in_path("iptables").ok_or_else(|| anyhow::anyhow!("iptables not found on PATH"))?;
    let check = std::process::Command::new(&iptables)
        .args(iptables_rule_args(table, "-C", chain, rule))
        .output()?;
    if check.status.success() {
        return Ok(());
    }
    let add = std::process::Command::new(&iptables)
        .args(iptables_rule_args(table, "-I", chain, rule))
        .output()?;
    if !add.status.success() {
        anyhow::bail!(
            "iptables -t {} -I {} failed: {}",
            table,
            chain,
            String::from_utf8_lossy(&add.stderr).trim()
        );
    }
    Ok(())
}

/// Delete an iptables rule if present; silent no-op otherwise.
fn iptables_remove(table: &str, chain: &str, rule: &[String]) {
    let Some(iptables) = find_in_path("iptables") else {
        return;
    };
    let check = std::process::Command::new(&iptables)
        .args(iptables_rule_args(table, "-C", chain, rule))
        .output();
    if matches!(check, Ok(ref o) if o.status.success()) {
        let _ = std::process::Command::new(&iptables)
            .args(iptables_rule_args(table, "-D", chain, rule))
            .output();
    }
}

fn read_state(dirs: &Dirs) -> Option<FirecrackerState> {
    let data = std::fs::read(&dirs.state_file).ok()?;
    serde_json::from_slice(&data).ok()
}

fn write_state(dirs: &Dirs, state: &FirecrackerState) -> anyhow::Result<()> {
    std::fs::create_dir_all(&dirs.root)?;
    std::fs::write(&dirs.state_file, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_subnet_gateway_and_network() {
        let (gw, net) = parse_subnet("192.168.222.1/24").unwrap();
        assert_eq!(gw, "192.168.222.1");
        assert_eq!(net, "192.168.222.0/24");
    }

    #[test]
    fn test_parse_subnet_rejects_garbage() {
        assert!(parse_subnet("not-a-subnet").is_err());
        assert!(parse_subnet("10.0.0.1").is_err());
    }

    #[test]
    fn test_parse_kernel_version() {
        assert_eq!(parse_kernel_version("6.8.0-100-generic"), Some((6, 8)));
        assert_eq!(parse_kernel_version("4.14.1"), Some((4, 14)));
        assert_eq!(parse_kernel_version("garbage"), None);
    }

    #[test]
    fn test_iptables_rule_args_shape() {
        let rule = vec!["-i".to_string(), "br0".to_string()];
        assert_eq!(
            iptables_rule_args("filter", "-C", "FORWARD", &rule),
            vec!["-t", "filter", "-C", "FORWARD", "-i", "br0"]
        );
    }
}
