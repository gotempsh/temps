use clap::Args;
use colored::Colorize;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::upgrade;

const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Diagnose the temps installation and check system health
#[derive(Args)]
pub struct DoctorCommand {
    /// Database connection URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: Option<String>,

    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
}

/// Result of a single diagnostic check
#[derive(Debug, Clone)]
enum CheckResult {
    Pass(String),
    Warn(String),
    Fail(String),
    Info(String),
}

/// Tracks overall diagnostic state
struct DiagnosticReport {
    checks: Vec<(&'static str, CheckResult)>,
    pass_count: u32,
    warn_count: u32,
    fail_count: u32,
}

impl DiagnosticReport {
    fn new() -> Self {
        Self {
            checks: Vec::new(),
            pass_count: 0,
            warn_count: 0,
            fail_count: 0,
        }
    }

    fn add(&mut self, label: &'static str, result: CheckResult) {
        match &result {
            CheckResult::Pass(_) => self.pass_count += 1,
            CheckResult::Warn(_) => self.warn_count += 1,
            CheckResult::Fail(_) => self.fail_count += 1,
            CheckResult::Info(_) => {}
        }
        self.checks.push((label, result));
    }

    fn print(&self) {
        for (label, result) in &self.checks {
            match result {
                CheckResult::Pass(msg) => {
                    println!(
                        "  {} {} {}",
                        "PASS".bright_green().bold(),
                        format!("{}:", label).bright_white(),
                        msg
                    );
                }
                CheckResult::Warn(msg) => {
                    println!(
                        "  {} {} {}",
                        "WARN".bright_yellow().bold(),
                        format!("{}:", label).bright_white(),
                        msg.bright_yellow()
                    );
                }
                CheckResult::Fail(msg) => {
                    println!(
                        "  {} {} {}",
                        "FAIL".bright_red().bold(),
                        format!("{}:", label).bright_white(),
                        msg.bright_red()
                    );
                }
                CheckResult::Info(msg) => {
                    println!(
                        "  {} {} {}",
                        "INFO".bright_cyan().bold(),
                        format!("{}:", label).bright_white(),
                        msg
                    );
                }
            }
        }
    }

    fn print_summary(&self) {
        println!();
        let summary = format!(
            "  {} passed, {} warnings, {} failed",
            self.pass_count, self.warn_count, self.fail_count
        );
        if self.fail_count > 0 {
            println!("{}", summary.bright_red().bold());
        } else if self.warn_count > 0 {
            println!("{}", summary.bright_yellow().bold());
        } else {
            println!("{}", summary.bright_green().bold());
        }
    }
}

impl DoctorCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.run())
    }

    async fn run(self) -> anyhow::Result<()> {
        println!();
        println!(
            "{}",
            "  Temps Doctor - System Health Check".bright_white().bold()
        );
        println!("{}", "  ===================================".bright_cyan());
        println!();

        let mut report = DiagnosticReport::new();

        // -- Version & Update --
        println!("{}", "  Version".bright_yellow().bold());
        self.check_version(&mut report).await;
        report.print();
        report.checks.clear();

        // -- Data Directory --
        let data_dir = self.resolve_data_dir();
        println!();
        println!("{}", "  Data Directory".bright_yellow().bold());
        self.check_data_dir(&data_dir, &mut report);
        report.print();
        report.checks.clear();

        // -- Docker --
        println!();
        println!("{}", "  Docker".bright_yellow().bold());
        self.check_docker(&mut report).await;
        report.print();
        report.checks.clear();

        // -- Database --
        println!();
        println!("{}", "  Database".bright_yellow().bold());
        let db = self.check_database(&mut report).await;
        report.print();
        report.checks.clear();

        // -- Application Settings (requires DB) --
        if let Some(ref db) = db {
            println!();
            println!("{}", "  Application".bright_yellow().bold());
            self.check_app_settings(db, &mut report).await;
            self.check_git_providers(db, &mut report).await;
            report.print();
            report.checks.clear();
        }

        // -- External Connectivity --
        println!();
        println!("{}", "  Connectivity".bright_yellow().bold());
        self.check_external_connectivity(&mut report).await;
        report.print();

        report.print_summary();
        println!();

        Ok(())
    }

    fn resolve_data_dir(&self) -> PathBuf {
        if let Some(ref d) = self.data_dir {
            d.clone()
        } else if let Ok(d) = std::env::var("TEMPS_DATA_DIR") {
            PathBuf::from(d)
        } else {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".temps")
        }
    }

    // ── Version checks ──────────────────────────────────────────────

    async fn check_version(&self, report: &mut DiagnosticReport) {
        let current = upgrade::current_version_tag();
        report.add("Current version", CheckResult::Info(current.clone()));

        // Doctor wants visibility into "is anything newer out there?"
        // regardless of channel — `Beta` includes both stable and beta
        // releases, so it gives the absolute latest tag. The label below
        // surfaces `(prerelease)` if the newest happens to be a beta.
        match upgrade::fetch_latest_release_in_channel(upgrade::UpgradeChannel::Beta).await {
            Ok(release) => {
                let latest = &release.tag_name;
                if latest == &current {
                    report.add("Update", CheckResult::Pass("Up to date".to_string()));
                } else {
                    let label = if release.prerelease {
                        format!("{} available (prerelease)", latest)
                    } else {
                        format!("{} available", latest)
                    };
                    report.add(
                        "Update",
                        CheckResult::Warn(format!("{} - run `temps upgrade` to update", label)),
                    );
                }
            }
            Err(e) => {
                report.add(
                    "Update check",
                    CheckResult::Warn(format!("Could not check for updates: {}", e)),
                );
            }
        }
    }

    // ── Data directory checks ───────────────────────────────────────

    fn check_data_dir(&self, data_dir: &Path, report: &mut DiagnosticReport) {
        report.add("Path", CheckResult::Info(data_dir.display().to_string()));

        if !data_dir.exists() {
            report.add(
                "Directory",
                CheckResult::Fail(
                    "Data directory does not exist. Run `temps setup` first.".to_string(),
                ),
            );
            return;
        }

        // Check writable
        let test_file = data_dir.join(".doctor-test");
        match std::fs::write(&test_file, "test") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                report.add("Writable", CheckResult::Pass("Yes".to_string()));
            }
            Err(e) => {
                report.add(
                    "Writable",
                    CheckResult::Fail(format!("Not writable: {}", e)),
                );
            }
        }

        // Check encryption_key
        let enc_key_path = data_dir.join("encryption_key");
        if enc_key_path.exists() {
            match std::fs::read_to_string(&enc_key_path) {
                Ok(content) => {
                    let trimmed = content.trim();
                    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                        report.add(
                            "Encryption key",
                            CheckResult::Pass("Valid (32 bytes)".to_string()),
                        );
                    } else {
                        report.add(
                            "Encryption key",
                            CheckResult::Fail(format!(
                                "Invalid format (expected 64 hex chars, got {})",
                                trimmed.len()
                            )),
                        );
                    }
                }
                Err(e) => {
                    report.add(
                        "Encryption key",
                        CheckResult::Fail(format!("Cannot read: {}", e)),
                    );
                }
            }
        } else {
            report.add(
                "Encryption key",
                CheckResult::Fail("Missing. Run `temps setup` first.".to_string()),
            );
        }

        // Check auth_secret
        let auth_secret_path = data_dir.join("auth_secret");
        if auth_secret_path.exists() {
            match std::fs::read_to_string(&auth_secret_path) {
                Ok(content) => {
                    if content.trim().is_empty() {
                        report.add(
                            "Auth secret",
                            CheckResult::Fail("File is empty".to_string()),
                        );
                    } else {
                        report.add("Auth secret", CheckResult::Pass("Present".to_string()));
                    }
                }
                Err(e) => {
                    report.add(
                        "Auth secret",
                        CheckResult::Fail(format!("Cannot read: {}", e)),
                    );
                }
            }
        } else {
            report.add(
                "Auth secret",
                CheckResult::Fail("Missing. Run `temps setup` first.".to_string()),
            );
        }

        // Check GeoLite2 database
        let geo_paths = [
            data_dir.join("GeoLite2-City.mmdb"),
            PathBuf::from("/usr/share/GeoIP/GeoLite2-City.mmdb"),
        ];
        let geo_found = geo_paths.iter().find(|p| p.exists());
        if let Some(path) = geo_found {
            report.add(
                "GeoLite2 database",
                CheckResult::Pass(path.display().to_string()),
            );
        } else {
            report.add(
                "GeoLite2 database",
                CheckResult::Warn("Not found. Geolocation features will be disabled.".to_string()),
            );
        }

        // Check logs directory
        let logs_dir = data_dir.join("logs");
        if logs_dir.exists() {
            report.add("Logs directory", CheckResult::Pass("Present".to_string()));
        } else {
            report.add(
                "Logs directory",
                CheckResult::Warn("Not found. Will be created on first deployment.".to_string()),
            );
        }
    }

    // ── Docker checks ───────────────────────────────────────────────

    async fn check_docker(&self, report: &mut DiagnosticReport) {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                report.add(
                    "Daemon",
                    CheckResult::Fail(format!("Cannot connect: {}", e)),
                );
                return;
            }
        };

        // Version
        match timeout(CHECK_TIMEOUT, docker.version()).await {
            Ok(Ok(version)) => {
                let ver_str = version.version.unwrap_or_else(|| "unknown".to_string());
                let api_ver = version.api_version.unwrap_or_else(|| "unknown".to_string());
                report.add(
                    "Daemon",
                    CheckResult::Pass(format!("v{} (API {})", ver_str, api_ver)),
                );
            }
            Ok(Err(e)) => {
                report.add("Daemon", CheckResult::Fail(format!("Error: {}", e)));
                return;
            }
            Err(_) => {
                report.add(
                    "Daemon",
                    CheckResult::Fail("Connection timed out".to_string()),
                );
                return;
            }
        }

        // Server info (OS, architecture, containers running)
        match timeout(CHECK_TIMEOUT, docker.info()).await {
            Ok(Ok(info)) => {
                let os = info
                    .operating_system
                    .unwrap_or_else(|| "unknown".to_string());
                let arch = info.architecture.unwrap_or_else(|| "unknown".to_string());
                let containers = info.containers_running.unwrap_or(0);
                report.add(
                    "Host",
                    CheckResult::Info(format!(
                        "{} ({}) - {} containers running",
                        os, arch, containers
                    )),
                );
            }
            Ok(Err(e)) => {
                report.add(
                    "Host info",
                    CheckResult::Warn(format!("Could not fetch: {}", e)),
                );
            }
            Err(_) => {
                report.add("Host info", CheckResult::Warn("Timed out".to_string()));
            }
        }

        // BuildKit support
        if let Ok(Ok(info)) = timeout(CHECK_TIMEOUT, docker.info()).await {
            // Check for buildx/buildkit via server version >= 18.09
            let has_buildkit = info
                .server_version
                .as_ref()
                .map(|v| {
                    let parts: Vec<&str> = v.split('.').collect();
                    if let Some(major) = parts.first().and_then(|s| s.parse::<u32>().ok()) {
                        major >= 18
                    } else {
                        false
                    }
                })
                .unwrap_or(false);

            if has_buildkit {
                report.add("BuildKit", CheckResult::Pass("Supported".to_string()));
            } else {
                report.add(
                    "BuildKit",
                    CheckResult::Warn(
                        "Docker version may not support BuildKit. Builds may be slower."
                            .to_string(),
                    ),
                );
            }
        }

        // Docker network
        let network_name = temps_core::NETWORK_NAME.as_str();
        match timeout(
            CHECK_TIMEOUT,
            docker.list_networks(None::<bollard::query_parameters::ListNetworksOptions>),
        )
        .await
        {
            Ok(Ok(networks)) => {
                let found = networks
                    .iter()
                    .find(|n| n.name.as_deref() == Some(network_name));
                if let Some(_network) = found {
                    report.add(
                        "Network",
                        CheckResult::Pass(format!("'{}' exists", network_name)),
                    );
                } else {
                    report.add(
                        "Network",
                        CheckResult::Warn(format!(
                            "'{}' not found. It will be created on first deployment.",
                            network_name
                        )),
                    );
                }
            }
            Ok(Err(e)) => {
                report.add(
                    "Network",
                    CheckResult::Warn(format!("Could not list networks: {}", e)),
                );
            }
            Err(_) => {
                report.add("Network", CheckResult::Warn("Check timed out".to_string()));
            }
        }
    }

    // ── Database checks ─────────────────────────────────────────────

    async fn check_database(
        &self,
        report: &mut DiagnosticReport,
    ) -> Option<sea_orm::DatabaseConnection> {
        let database_url = match &self.database_url {
            Some(url) => url.clone(),
            None => {
                report.add(
                    "Connection",
                    CheckResult::Warn(
                        "No --database-url provided. Skipping database checks.".to_string(),
                    ),
                );
                return None;
            }
        };

        // Mask password in display URL
        let display_url = mask_database_url(&database_url);
        report.add("URL", CheckResult::Info(display_url));

        // Parse host:port
        let (host, port) = match parse_pg_url(&database_url) {
            Ok(hp) => hp,
            Err(e) => {
                report.add(
                    "URL parse",
                    CheckResult::Fail(format!("Invalid URL: {}", e)),
                );
                return None;
            }
        };

        // TCP connectivity
        let addr = format!("{}:{}", host, port);
        match timeout(CHECK_TIMEOUT, TcpStream::connect(&addr)).await {
            Ok(Ok(_)) => {
                report.add(
                    "TCP connectivity",
                    CheckResult::Pass(format!("{} reachable", addr)),
                );
            }
            Ok(Err(e)) => {
                report.add(
                    "TCP connectivity",
                    CheckResult::Fail(format!("{}: {}", addr, e)),
                );
                return None;
            }
            Err(_) => {
                report.add(
                    "TCP connectivity",
                    CheckResult::Fail(format!("{}: timed out", addr)),
                );
                return None;
            }
        }

        // SeaORM connection (without running migrations)
        let mut opt = sea_orm::ConnectOptions::new(&database_url);
        opt.max_connections(2)
            .min_connections(1)
            .connect_timeout(CHECK_TIMEOUT)
            .sqlx_logging(false);

        let db = match timeout(CHECK_TIMEOUT, sea_orm::Database::connect(opt)).await {
            Ok(Ok(db)) => db,
            Ok(Err(e)) => {
                report.add(
                    "Connection",
                    CheckResult::Fail(format!("Cannot connect: {}", e)),
                );
                return None;
            }
            Err(_) => {
                report.add(
                    "Connection",
                    CheckResult::Fail("Connection timed out".to_string()),
                );
                return None;
            }
        };

        report.add("Connection", CheckResult::Pass("Connected".to_string()));

        // PostgreSQL version
        match db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT version()".to_string(),
            ))
            .await
        {
            Ok(Some(row)) => {
                use sea_orm::TryGetable;
                if let Ok(version) = String::try_get_by(&row, "version") {
                    // Extract just the version part (e.g., "PostgreSQL 16.2 on ...")
                    let short = version.split(" on ").next().unwrap_or(&version).to_string();
                    report.add("Server", CheckResult::Info(short));
                }
            }
            Ok(None) => {}
            Err(e) => {
                report.add(
                    "Server version",
                    CheckResult::Warn(format!("Could not query: {}", e)),
                );
            }
        }

        // TimescaleDB extension
        match db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT extversion FROM pg_extension WHERE extname = 'timescaledb'".to_string(),
            ))
            .await
        {
            Ok(Some(row)) => {
                use sea_orm::TryGetable;
                if let Ok(ver) = String::try_get_by(&row, "extversion") {
                    report.add("TimescaleDB", CheckResult::Pass(format!("v{}", ver)));
                }
            }
            Ok(None) => {
                report.add(
                    "TimescaleDB",
                    CheckResult::Warn(
                        "Extension not installed. Analytics features may not work.".to_string(),
                    ),
                );
            }
            Err(e) => {
                report.add(
                    "TimescaleDB",
                    CheckResult::Warn(format!("Could not check: {}", e)),
                );
            }
        }

        // Migration status — report applied count AND, crucially, whether this
        // binary has migrations the DB hasn't applied yet. Pending migrations
        // are the key pre-upgrade signal: run `temps migrate` before restarting
        // the server so a slow migration can't crash-loop the API.
        {
            use temps_migrations::{Migrator, MigratorTrait};

            let applied: Option<i64> = db
                .query_one(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "SELECT COUNT(*) as count FROM seaql_migrations".to_string(),
                ))
                .await
                .ok()
                .flatten()
                .and_then(|row| {
                    use sea_orm::TryGetable;
                    i64::try_get_by(&row, "count").ok()
                });

            match Migrator::get_pending_migrations(&db).await {
                Ok(pending) => {
                    let pending_n = pending.len();
                    let applied_str = applied
                        .map(|c| format!("{c} applied"))
                        .unwrap_or_else(|| "applied count unknown".to_string());
                    if pending_n == 0 {
                        report.add(
                            "Migrations",
                            CheckResult::Pass(format!("{applied_str}, up to date")),
                        );
                    } else {
                        report.add(
                            "Migrations",
                            CheckResult::Warn(format!(
                                "{applied_str}, {pending_n} PENDING — run `temps migrate` \
                                 before restarting the server"
                            )),
                        );
                    }
                }
                Err(_) => {
                    report.add(
                        "Migrations",
                        CheckResult::Warn(
                            "Migration table not found. Run `temps migrate` to initialize the schema."
                                .to_string(),
                        ),
                    );
                }
            }
        }

        Some(db)
    }

    // ── App settings checks ─────────────────────────────────────────

    async fn check_app_settings(
        &self,
        db: &sea_orm::DatabaseConnection,
        report: &mut DiagnosticReport,
    ) {
        // Query settings row
        match db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT data FROM settings WHERE id = 1".to_string(),
            ))
            .await
        {
            Ok(Some(row)) => {
                use sea_orm::TryGetable;
                match serde_json::Value::try_get_by(&row, "data") {
                    Ok(data) => {
                        let settings = temps_core::AppSettings::from_json(data);

                        // External URL
                        if let Some(ref url) = settings.external_url {
                            report.add("External URL", CheckResult::Info(url.clone()));
                        } else {
                            report.add(
                                "External URL",
                                CheckResult::Warn("Not configured".to_string()),
                            );
                        }

                        // Preview domain
                        if settings.preview_domain == "localho.st" {
                            report.add(
                                "Preview domain",
                                CheckResult::Warn(format!(
                                    "'{}' (default - configure a real domain for production)",
                                    settings.preview_domain
                                )),
                            );
                        } else {
                            report.add(
                                "Preview domain",
                                CheckResult::Pass(settings.preview_domain.clone()),
                            );
                        }

                        // Let's Encrypt
                        if let Some(ref email) = settings.letsencrypt.email {
                            report.add(
                                "Let's Encrypt",
                                CheckResult::Pass(format!(
                                    "{} ({})",
                                    email, settings.letsencrypt.environment
                                )),
                            );
                        } else {
                            report.add(
                                "Let's Encrypt",
                                CheckResult::Warn(
                                    "No email configured. TLS certificates cannot be issued."
                                        .to_string(),
                                ),
                            );
                        }

                        // DNS provider
                        if settings.dns_provider.provider == "manual" {
                            report.add(
                                "DNS provider",
                                CheckResult::Warn(
                                    "Manual (wildcard certificates require an automated DNS provider)"
                                        .to_string(),
                                ),
                            );
                        } else {
                            report.add(
                                "DNS provider",
                                CheckResult::Pass(settings.dns_provider.provider.clone()),
                            );
                        }
                    }
                    Err(e) => {
                        report.add(
                            "Settings",
                            CheckResult::Warn(format!("Could not parse: {:?}", e)),
                        );
                    }
                }
            }
            Ok(None) => {
                report.add(
                    "Settings",
                    CheckResult::Warn(
                        "No settings found. Run `temps setup` to configure.".to_string(),
                    ),
                );
            }
            Err(_) => {
                report.add(
                    "Settings",
                    CheckResult::Warn(
                        "Settings table not found. Run `temps serve` first.".to_string(),
                    ),
                );
            }
        }
    }

    // ── Git provider checks ─────────────────────────────────────────

    async fn check_git_providers(
        &self,
        db: &sea_orm::DatabaseConnection,
        report: &mut DiagnosticReport,
    ) {
        match db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT provider_type, is_default FROM git_providers WHERE is_active = true"
                    .to_string(),
            ))
            .await
        {
            Ok(rows) if !rows.is_empty() => {
                use sea_orm::TryGetable;
                let mut providers = Vec::new();
                for row in &rows {
                    let ptype = String::try_get_by(row, "provider_type")
                        .unwrap_or_else(|_| "unknown".to_string());
                    let is_default = bool::try_get_by(row, "is_default").unwrap_or(false);
                    if is_default {
                        providers.push(format!("{} (default)", ptype));
                    } else {
                        providers.push(ptype);
                    }
                }
                report.add("Git providers", CheckResult::Pass(providers.join(", ")));
            }
            Ok(_) => {
                report.add(
                    "Git providers",
                    CheckResult::Warn(
                        "None configured. Add a provider to deploy from Git.".to_string(),
                    ),
                );
            }
            Err(_) => {
                report.add(
                    "Git providers",
                    CheckResult::Warn("Could not query (table may not exist yet)".to_string()),
                );
            }
        }
    }

    // ── Connectivity checks ─────────────────────────────────────────

    async fn check_external_connectivity(&self, report: &mut DiagnosticReport) {
        // GitHub API (needed for git provider webhooks, upgrade checks)
        check_url_reachable("GitHub API", "https://api.github.com", report).await;

        // Docker Hub (needed for pulling base images)
        check_url_reachable("Docker Hub", "https://registry-1.docker.io/v2/", report).await;

        // Let's Encrypt (needed for TLS certificates)
        check_url_reachable(
            "Let's Encrypt",
            "https://acme-v02.api.letsencrypt.org/directory",
            report,
        )
        .await;
    }
}

/// Check if a URL is reachable via HTTPS.
async fn check_url_reachable(label: &'static str, url: &str, report: &mut DiagnosticReport) {
    let client = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match client
        .get(url)
        .header("User-Agent", "temps-doctor")
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.is_redirection() || status == 401 {
                report.add(label, CheckResult::Pass("Reachable".to_string()));
            } else {
                report.add(
                    label,
                    CheckResult::Warn(format!("Returned HTTP {}", status)),
                );
            }
        }
        Err(e) => {
            report.add(label, CheckResult::Fail(format!("Unreachable: {}", e)));
        }
    }
}

/// Parse host and port from a PostgreSQL URL.
fn parse_pg_url(url: &str) -> Result<(String, u16), String> {
    let without_scheme = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .ok_or("URL must start with postgres:// or postgresql://")?;

    let host_part = if let Some(at_pos) = without_scheme.rfind('@') {
        &without_scheme[at_pos + 1..]
    } else {
        without_scheme
    };

    let host_port = host_part.split('/').next().unwrap_or(host_part);
    let host_port = host_port.split('?').next().unwrap_or(host_port);

    if host_port.starts_with('[') {
        // IPv6
        if let Some(bracket_end) = host_port.find(']') {
            let host = &host_port[1..bracket_end];
            let port_part = &host_port[bracket_end + 1..];
            let port = if let Some(stripped) = port_part.strip_prefix(':') {
                stripped.parse::<u16>().unwrap_or(5432)
            } else {
                5432
            };
            Ok((host.to_string(), port))
        } else {
            Err("Invalid IPv6 address".to_string())
        }
    } else if let Some(colon_pos) = host_port.rfind(':') {
        let host = &host_port[..colon_pos];
        let port = host_port[colon_pos + 1..].parse::<u16>().unwrap_or(5432);
        Ok((host.to_string(), port))
    } else {
        Ok((host_port.to_string(), 5432))
    }
}

/// Mask password in a database URL for safe display.
fn mask_database_url(url: &str) -> String {
    // Pattern: postgres://user:password@host:port/db
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        if let Some(at_pos) = after_scheme.rfind('@') {
            let credentials = &after_scheme[..at_pos];
            let rest = &after_scheme[at_pos..];
            if let Some(colon_pos) = credentials.find(':') {
                let user = &credentials[..colon_pos];
                return format!("{}://{}:***{}", &url[..scheme_end], user, rest);
            }
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_database_url_with_password() {
        let url = "postgres://myuser:secret123@localhost:5432/mydb";
        assert_eq!(
            mask_database_url(url),
            "postgres://myuser:***@localhost:5432/mydb"
        );
    }

    #[test]
    fn test_mask_database_url_with_special_chars() {
        let url = "postgresql://admin:p%40ss%23word@db.example.com:5433/temps";
        assert_eq!(
            mask_database_url(url),
            "postgresql://admin:***@db.example.com:5433/temps"
        );
    }

    #[test]
    fn test_mask_database_url_no_password() {
        let url = "postgres://localhost:5432/mydb";
        assert_eq!(mask_database_url(url), "postgres://localhost:5432/mydb");
    }

    #[test]
    fn test_mask_database_url_no_credentials() {
        let url = "postgres://localhost/mydb";
        assert_eq!(mask_database_url(url), "postgres://localhost/mydb");
    }

    #[test]
    fn test_parse_pg_url_basic() {
        let (host, port) = parse_pg_url("postgres://user:pass@localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_pg_url_default_port() {
        let (host, port) = parse_pg_url("postgres://user:pass@myhost/db").unwrap();
        assert_eq!(host, "myhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_pg_url_custom_port() {
        let (host, port) = parse_pg_url("postgresql://u:p@db.example.com:5433/mydb").unwrap();
        assert_eq!(host, "db.example.com");
        assert_eq!(port, 5433);
    }

    #[test]
    fn test_parse_pg_url_with_query_params() {
        let (host, port) =
            parse_pg_url("postgres://u:p@localhost:5432/db?sslmode=require").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_pg_url_ipv6() {
        let (host, port) = parse_pg_url("postgres://u:p@[::1]:5432/db").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_pg_url_invalid_scheme() {
        let result = parse_pg_url("mysql://u:p@localhost:3306/db");
        assert!(result.is_err());
    }

    #[test]
    fn test_diagnostic_report_counts() {
        let mut report = DiagnosticReport::new();
        report.add("a", CheckResult::Pass("ok".into()));
        report.add("b", CheckResult::Pass("ok".into()));
        report.add("c", CheckResult::Warn("meh".into()));
        report.add("d", CheckResult::Fail("bad".into()));
        report.add("e", CheckResult::Info("fyi".into()));

        assert_eq!(report.pass_count, 2);
        assert_eq!(report.warn_count, 1);
        assert_eq!(report.fail_count, 1);
    }
}
