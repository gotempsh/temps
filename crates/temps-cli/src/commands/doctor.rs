// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use clap::{Args, Subcommand};
use colored::Colorize;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::upgrade;
use crate::database_url_source::{
    resolve_database_url, DatabaseUrlNotFound, DatabaseUrlOrigin, ResolvedDatabaseUrl, UrlDiscovery,
};
use crate::server_secrets::SecretsEnvironment;
use crate::service_definition::{self, ServiceDefinition, ServiceLocations, ServiceProbe};

mod firecracker;
mod mesh;
pub use mesh::MeshDoctorArgs;

const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// What to do when the daemon is unreachable; `temps serve` refuses to start
/// the console in that state (the `full` profile).
const DOCKER_REMEDIATION: &str = "Start the Docker daemon, or point DOCKER_HOST (or `docker \
     context use`) at the socket it listens on; `temps serve --profile control-plane` runs \
     without a local daemon";

/// Diagnose the temps installation and check system health
#[derive(Args)]
pub struct DoctorCommand {
    /// Database connection URL. Prefer TEMPS_DATABASE_URL (or TEMPS_DATABASE_URL_FILE / the systemd credential temps_database_url): the flag form puts the password in the process command line
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: Option<String>,

    /// Data directory for storing configuration and runtime files
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub scope: Option<DoctorScope>,
}

#[derive(Subcommand)]
pub enum DoctorScope {
    /// Check this host's end of the WireGuard mesh (a node or the control
    /// plane), with the fix for each failing check
    Mesh(MeshDoctorArgs),
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
    checks: Vec<(String, CheckResult)>,
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

    fn add(&mut self, label: impl Into<String>, result: CheckResult) {
        match &result {
            CheckResult::Pass(_) => self.pass_count += 1,
            CheckResult::Warn(_) => self.warn_count += 1,
            CheckResult::Fail(_) => self.fail_count += 1,
            CheckResult::Info(_) => {}
        }
        self.checks.push((label.into(), result));
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
        // The same sources `temps serve` reads, then the installed service
        // definition, so a shell finds the server's database and data
        // directory without repeating its flags.
        let database =
            resolve_database_url(self.database_url.as_deref(), UrlDiscovery::InstalledService);
        let probes = service_definition::probe(&ServiceLocations::installed());
        let service = database
            .as_ref()
            .ok()
            .and_then(ResolvedDatabaseUrl::service)
            .cloned()
            .or_else(|| {
                probes.iter().find_map(|probe| match probe {
                    ServiceProbe::Found(definition) => Some((**definition).clone()),
                    ServiceProbe::Absent { .. } | ServiceProbe::Ignored { .. } => None,
                })
            });
        let DoctorDataDir {
            dir: data_dir,
            source: data_dir_source,
            unverified: data_dir_unverified,
        } = self.resolve_data_dir(service.as_ref(), &probes);

        if let Some(DoctorScope::Mesh(args)) = &self.scope {
            let database_url = database
                .as_ref()
                .map(ResolvedDatabaseUrl::url)
                .map_err(ToString::to_string);
            return mesh::run(args, database_url, &data_dir).await;
        }
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

        // -- Installed service --
        println!();
        println!("{}", "  Installed service".bright_yellow().bold());
        check_installed_service(service.as_ref(), &probes, &mut report);
        report.print();
        report.checks.clear();

        // -- Data Directory --
        println!();
        println!("{}", "  Data Directory".bright_yellow().bold());
        self.check_data_dir(
            &data_dir,
            &data_dir_source,
            data_dir_unverified,
            service.as_ref(),
            SecretsEnvironment::choose(service.as_ref()),
            &mut report,
        );
        report.print();
        report.checks.clear();

        // -- Docker --
        println!();
        println!("{}", "  Docker".bright_yellow().bold());
        self.check_docker(service.as_ref(), &mut report).await;
        report.print();
        report.checks.clear();

        // -- Database --
        println!();
        println!("{}", "  Database".bright_yellow().bold());
        let db = self.check_database(&database, &mut report).await;
        report.print();
        report.checks.clear();

        // -- Application Settings (requires DB) --
        println!();
        println!("{}", "  Initial admin".bright_yellow().bold());
        let serving = database
            .as_ref()
            .ok()
            .and_then(ResolvedDatabaseUrl::serving_service);
        self.check_initial_admin(&db, serving, &mut report).await;
        report.print();
        report.checks.clear();

        if let DatabaseState::Connected(ref db) = db {
            println!();
            println!("{}", "  Application".bright_yellow().bold());
            self.check_app_settings(db, &mut report).await;
            self.check_geo_database_freshness(db, &mut report).await;
            self.check_git_providers(db, &mut report).await;
            report.print();
            report.checks.clear();

            println!();
            println!("{}", "  Multi-node networking".bright_yellow().bold());
            self.check_multi_node_networking(db, &mut report).await;
            report.print();
            report.checks.clear();

            println!();
            println!("{}", "  Internal DNS".bright_yellow().bold());
            self.check_dns_name_conflicts(db, &mut report).await;
            report.print();
            report.checks.clear();

            println!();
            println!("{}", "  WireGuard mesh".bright_yellow().bold());
            let checks = mesh::control_plane_checks(db, &data_dir).await;
            mesh::add_to_report(&checks, &mut report);
            report.print();
            report.checks.clear();
        }

        // -- Firecracker (only once it was set up here) --
        if let Some(recorded) = firecracker::recorded_setup(&data_dir) {
            println!();
            println!("{}", "  Firecracker microVMs".bright_yellow().bold());
            let facts = firecracker::gather(&data_dir, recorded);
            for (label, result) in firecracker::assess(&facts) {
                report.add(label, result);
            }
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

    /// The data directory `temps serve` uses, and where that answer came
    /// from: `--data-dir` / `TEMPS_DATA_DIR` here, else the installed
    /// service's, else the default layout.
    fn resolve_data_dir(
        &self,
        service: Option<&ServiceDefinition>,
        probes: &[ServiceProbe],
    ) -> DoctorDataDir {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        resolve_doctor_data_dir(self.data_dir.as_deref(), service, probes, &home)
    }

    // ── Version checks ──────────────────────────────────────────────

    async fn check_version(&self, report: &mut DiagnosticReport) {
        let current = upgrade::current_version_tag();
        report.add("Current version", CheckResult::Info(current.clone()));
        // The exact string `temps --version` prints, so the two are visibly
        // the same build (tag, commit, build time).
        report.add(
            "Build",
            CheckResult::Info(env!("TEMPS_VERSION").to_string()),
        );

        // Compare within the channel this build came from -- a nightly
        // against nightlies, a beta against betas (and stable), stable
        // against stable -- the same inference the server's update notifier
        // uses when no channel is pinned in Settings.
        let channel = upgrade::UpgradeChannel::for_installed_version(&current);
        match upgrade::fetch_latest_release_in_channel(channel).await {
            Ok(release) => {
                report.add(
                    "Update",
                    update_check_result(&current, channel, &release.tag_name),
                );
            }
            Err(e) => {
                report.add(
                    "Update check",
                    CheckResult::Warn(format!(
                        "Could not check the {} channel for updates: {}",
                        channel.as_str(),
                        e
                    )),
                );
            }
        }
    }

    // ── Data directory checks ───────────────────────────────────────

    fn check_data_dir(
        &self,
        data_dir: &Path,
        source: &str,
        unverified: bool,
        service: Option<&ServiceDefinition>,
        secrets_env: SecretsEnvironment<'_>,
        report: &mut DiagnosticReport,
    ) {
        report.add(
            "Path",
            CheckResult::Info(format!("{} ({source})", data_dir.display())),
        );
        // A guess, while an installed server exists: a pass below says
        // nothing about the server's own files.
        if unverified {
            report.add(
                "Server's data directory",
                CheckResult::Warn(format!(
                    "Unknown, so the checks below are for {}, which may not be the one the \
                     installed server uses. Pass --data-dir with the server's data directory",
                    data_dir.display()
                )),
            );
        }

        if let Some(result) = data_dir_absence(data_dir, unverified, service.is_some()) {
            report.add("Directory", result);
            return;
        }

        match probe_writable(data_dir) {
            Ok(()) => report.add("Writable", CheckResult::Pass("Yes".to_string())),
            Err(e) => report.add("Writable", CheckResult::Fail(format!("Not writable: {e}"))),
        }

        // The secrets as `temps serve` resolves them (ServerConfig::new):
        // injected through its environment, else the data directory's files,
        // which it generates when they are missing. Its environment is this
        // shell's when that injects any of them, else the installed service's.
        match &secrets_env {
            SecretsEnvironment::Service(service) => report.add(
                "Secrets source",
                CheckResult::Info(format!(
                    "Checked with the environment of {}",
                    service.path.display()
                )),
            ),
            SecretsEnvironment::Unknown { service, reason } => {
                let result = environment_unknown_result(&service.path, reason);
                report.add("Encryption key", result.clone());
                report.add("Auth secret", result);
            }
            SecretsEnvironment::Shell(_) => report.add(
                "Secrets source",
                CheckResult::Info(format!("Checked as {}", secrets_env.describe())),
            ),
            SecretsEnvironment::Empty => {}
        }
        // serve refuses to start with it; read as unset, the checks below
        // would pass on what it never uses.
        if let Some(variable) = secrets_env.not_unicode() {
            report.add(
                "Secrets",
                CheckResult::Fail(format!(
                    "This shell sets {variable} to a value that is not valid Unicode, which \
                     `temps serve` refuses to start with. Fix or unset it in this shell"
                )),
            );
        } else if !matches!(secrets_env, SecretsEnvironment::Unknown { .. }) {
            self.check_secrets(data_dir, &secrets_env, report);
        }

        report.add(
            "GeoLite2 database",
            geolite_result(
                data_dir,
                service.and_then(|service| service.working_directory.as_deref()),
            ),
        );
        report.add("Logs directory", logs_dir_result(data_dir));
    }

    fn check_secrets(
        &self,
        data_dir: &Path,
        secrets_env: &SecretsEnvironment<'_>,
        report: &mut DiagnosticReport,
    ) {
        let inspection =
            temps_config::inspect_installation_secrets_with(data_dir, |name| secrets_env.get(name));
        let initialized = matches!(
            inspection.auth_secret,
            Ok(temps_config::InspectedSecret::Found {
                source: temps_config::SecretSource::DataDir { .. },
                ..
            })
        );
        report.add(
            "Encryption key",
            encryption_key_result(&inspection.encryption_key, initialized),
        );
        report.add("Auth secret", auth_secret_result(&inspection.auth_secret));
    }

    // ── Docker checks ───────────────────────────────────────────────

    async fn check_docker(
        &self,
        service: Option<&ServiceDefinition>,
        report: &mut DiagnosticReport,
    ) {
        // Same endpoint resolution as `temps serve`: DOCKER_HOST (including
        // tcp:// and ssh:// endpoints), else the active Docker context adopted
        // at startup, else the default socket. A doctor that probed a
        // different endpoint than the server would report the wrong answer.
        let endpoint = crate::docker_context::docker_endpoint();
        report.add("Endpoint", CheckResult::Info(endpoint.describe()));
        if let Some(note) = service_docker_host_mismatch(service, &endpoint.host) {
            report.add("Service endpoint", CheckResult::Warn(note));
        }

        let docker = match bollard::Docker::connect_with_defaults() {
            Ok(d) => d,
            Err(e) => {
                report.add(
                    "Daemon",
                    CheckResult::Fail(format!(
                        "Cannot connect to {}: {}. {}",
                        endpoint.host, e, DOCKER_REMEDIATION
                    )),
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
                report.add(
                    "Daemon",
                    CheckResult::Fail(format!(
                        "Not reachable at {}: {}. {}",
                        endpoint.host, e, DOCKER_REMEDIATION
                    )),
                );
                return;
            }
            Err(_) => {
                report.add(
                    "Daemon",
                    CheckResult::Fail(format!(
                        "No answer from {} within {}s. {}",
                        endpoint.host,
                        CHECK_TIMEOUT.as_secs(),
                        DOCKER_REMEDIATION
                    )),
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

        // Docker network: the name the server uses, which its own
        // environment may override.
        let network_name = docker_network_name(service);
        let network_name = network_name.as_str();
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
        database: &Result<ResolvedDatabaseUrl, DatabaseUrlNotFound>,
        report: &mut DiagnosticReport,
    ) -> DatabaseState {
        let resolved = match database {
            Ok(resolved) => resolved,
            Err(not_found) => {
                report.add(
                    "Connection",
                    CheckResult::Warn(format!("Skipping database checks. {not_found}")),
                );
                return DatabaseState::NoUrl;
            }
        };
        // Where the URL came from, never the URL: it carries the password.
        report.add("Source", CheckResult::Info(resolved.origin().describe()));
        if let Some(note) = service_url_mismatch(resolved) {
            report.add("Installed service", CheckResult::Warn(note));
        }
        let database_url = resolved.url();

        // Parse host:port
        let (host, port) = match parse_pg_url(database_url) {
            Ok(hp) => hp,
            Err(e) => {
                report.add(
                    "URL parse",
                    CheckResult::Fail(format!("Invalid URL: {}", e)),
                );
                return DatabaseState::Unreachable;
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
                return DatabaseState::Unreachable;
            }
            Err(_) => {
                report.add(
                    "TCP connectivity",
                    CheckResult::Fail(format!("{}: timed out", addr)),
                );
                return DatabaseState::Unreachable;
            }
        }

        // SeaORM connection (without running migrations)
        let mut opt = temps_database::connect_options(database_url);
        opt.max_connections(2)
            .min_connections(1)
            .connect_timeout(CHECK_TIMEOUT);

        let db = match timeout(CHECK_TIMEOUT, sea_orm::Database::connect(opt)).await {
            Ok(Ok(db)) => db,
            Ok(Err(e)) => {
                report.add(
                    "Connection",
                    CheckResult::Fail(format!("Cannot connect: {}", e)),
                );
                return DatabaseState::Unreachable;
            }
            Err(_) => {
                report.add(
                    "Connection",
                    CheckResult::Fail("Connection timed out".to_string()),
                );
                return DatabaseState::Unreachable;
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
        // the server so a slow migration can't crash-loop the API. Uses the
        // read-only schema guard: unlike Sea-ORM's migrator it never creates
        // the ledger table, and it understands a database migrated by a newer
        // release (e.g. after a binary-only rollback).
        let schema_status = temps_schema::check_schema_compatibility(&db)
            .await
            .map_err(|error| error.to_string());
        report.add("Migrations", migration_check_result(schema_status.as_ref()));

        // A migration that changes hypertable constraints locks every chunk in
        // one transaction; warn while the lock table can still be resized,
        // before a binary with such a migration is installed.
        let lock_capacity = temps_schema::read_lock_capacity(db.get_postgres_connection_pool())
            .await
            .map_err(|error| error.to_string());
        report.add(
            "Lock capacity",
            lock_capacity_result(lock_capacity.as_ref()),
        );

        // Only this binary's schema refuses project deletes while the keys
        // remain; with its migrations still pending, the running server is an
        // older one that still cascades.
        if schema_status
            .as_ref()
            .is_ok_and(|status| status.pending().is_empty())
        {
            let remaining =
                temps_schema::remaining_telemetry_foreign_keys(db.get_postgres_connection_pool())
                    .await
                    .map_err(|error| error.to_string());
            report.add(
                "Telemetry foreign keys",
                telemetry_foreign_keys_result(remaining.as_ref()),
            );
        }

        DatabaseState::Connected(db)
    }

    // ── Initial admin bootstrap ─────────────────────────────────────

    /// `temps serve` creates the first admin from TEMPS_ADMIN_EMAIL and
    /// TEMPS_ADMIN_PASSWORD_FILE when no users exist. Doctor checks persisted
    /// bootstrap state without rereading these one-shot input secrets;
    /// `serving` is the installed service whose database this is, whose
    /// environment says whether its server can create the admin.
    async fn check_initial_admin(
        &self,
        db: &DatabaseState,
        serving: Option<&ServiceDefinition>,
        report: &mut DiagnosticReport,
    ) {
        let users = match db {
            DatabaseState::NoUrl => UserCount::NoDatabaseUrl,
            DatabaseState::Unreachable => UserCount::Unreachable,
            DatabaseState::Connected(db) => {
                use sea_orm::TryGetable;
                match db
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        "SELECT COUNT(*) AS count FROM users WHERE deleted_at IS NULL AND id <> 0"
                            .to_string(),
                    ))
                    .await
                {
                    Ok(Some(row)) => match i64::try_get_by(&row, "count") {
                        Ok(count) => UserCount::Count(count),
                        Err(error) => UserCount::QueryFailed(format!("{error:?}")),
                    },
                    Ok(None) => UserCount::QueryFailed("the count returned no row".to_string()),
                    Err(error) => UserCount::QueryFailed(error.to_string()),
                }
            }
        };
        report.add("Initial admin", initial_admin_result(&users, serving));
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
                        let settings = temps_config::AppSettings::from_json(data);

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

    /// Report how current the geolocation database is.
    ///
    /// The data-directory section above only answers "is the file there?" --
    /// which is what an operator would check, and is exactly why a database
    /// that has never been refreshed since install looks healthy while it
    /// geolocates reassigned IPs to the city they used to be in.
    async fn check_geo_database_freshness(
        &self,
        db: &sea_orm::DatabaseConnection,
        report: &mut DiagnosticReport,
    ) {
        // Read the settings row directly rather than building a
        // `ConfigService`: `temps doctor` is a read-only diagnostic holding a
        // borrowed connection, and it only needs the geo section. The license
        // key is never decrypted here -- whether one is *stored* is all an
        // operator needs to see, and is all this reports.
        let geo = match read_geo_settings(db).await {
            Ok(geo) => geo,
            Err(e) => {
                report.add(
                    "GeoLite2 freshness",
                    CheckResult::Warn(format!("Could not read the geolocation settings: {}", e)),
                );
                return;
            }
        };

        report.add(
            "GeoLite2 source",
            CheckResult::Info(format!(
                "{} (MaxMind license key configured: {})",
                geo.source.clone().unwrap_or_else(|| {
                    "unknown (never downloaded by this instance)".to_string()
                }),
                geo.license_key_configured()
            )),
        );

        let stale_after = i64::from(geo.effective_stale_lookup_days());
        match geo.age_days(chrono::Utc::now()) {
            Some(age) if age > stale_after => report.add(
                "GeoLite2 freshness",
                CheckResult::Warn(format!(
                    "Database is {} (older than the {}-day threshold). \
                     Add a MaxMind license key under Settings -> Metrics Monitoring so \
                     refreshes fetch the latest build.",
                    describe_age(age),
                    stale_after
                )),
            ),
            Some(age) => report.add(
                "GeoLite2 freshness",
                CheckResult::Pass(format!("Database is {}", describe_age(age))),
            ),
            None if geo.license_key_configured() => report.add(
                "GeoLite2 freshness",
                CheckResult::Warn(format!(
                    "No refresh has been recorded yet; the scheduled job runs every {} hours",
                    geo.effective_refresh_interval_hours()
                )),
            ),
            None => report.add(
                "GeoLite2 freshness",
                CheckResult::Warn(
                    "No refresh has been recorded yet, and no MaxMind license key is \
                     configured -- the scheduled job downloads nothing without one, so \
                     lookups will keep using the database currently on disk. Add a key \
                     under Settings -> Metrics Monitoring -> Geolocation database."
                        .to_string(),
                ),
            ),
        }

        match (geo.last_check_status.as_deref(), geo.last_check_at) {
            (Some(temps_config::GEO_CHECK_STATUS_ERROR), checked_at) => {
                let when = checked_at
                    .map(iso8601)
                    .unwrap_or_else(|| "an unknown time".to_string());
                report.add(
                    "GeoLite2 last check",
                    CheckResult::Warn(format!(
                        "Failed at {}: {}",
                        when,
                        geo.last_error
                            .clone()
                            .unwrap_or_else(|| "no reason recorded".to_string())
                    )),
                );
            }
            (Some(temps_config::GEO_CHECK_STATUS_SKIPPED_NO_LICENSE_KEY), checked_at) => {
                let when = checked_at
                    .map(iso8601)
                    .unwrap_or_else(|| "an unknown time".to_string());
                report.add(
                    "GeoLite2 last check",
                    CheckResult::Warn(format!(
                        "Skipped at {}: no MaxMind license key is configured, so the scheduled \
                         refresh does not download anything. Add a key (free MaxMind account) \
                         under Settings -> Metrics Monitoring -> Geolocation database to keep \
                         the database current.",
                        when
                    )),
                );
            }
            (Some(_), Some(checked_at)) => report.add(
                "GeoLite2 last check",
                CheckResult::Pass(format!("Succeeded at {}", iso8601(checked_at))),
            ),
            (Some(_), None) | (None, _) => report.add(
                "GeoLite2 last check",
                CheckResult::Info("Never run on this instance".to_string()),
            ),
        }
    }

    async fn check_multi_node_networking(
        &self,
        db: &sea_orm::DatabaseConnection,
        report: &mut DiagnosticReport,
    ) {
        let query = "SELECT \
            (SELECT COUNT(*) FROM nodes WHERE status = 'active') AS active_workers, \
            nc.control_plane_compute_cidr, \
            nc.control_plane_underlay_address, \
            nc.control_plane_overlay_ready, \
            (SELECT COUNT(*) FROM external_services es \
             WHERE es.node_id IS NULL AND es.topology = 'standalone' \
               AND NOT EXISTS (SELECT 1 FROM service_endpoints se \
                 WHERE se.owner_kind = 'service_role' AND se.owner_id = es.id)) \
              AS missing_service_endpoints \
            FROM network_config nc WHERE nc.id = 1";
        let row = match db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                query.to_string(),
            ))
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                report.add(
                    "Configuration",
                    CheckResult::Warn("network_config is not initialized".to_string()),
                );
                return;
            }
            Err(error) => {
                report.add(
                    "Configuration",
                    CheckResult::Warn(format!(
                        "Could not inspect control-plane overlay state: {error}. Run `temps migrate` first."
                    )),
                );
                return;
            }
        };

        use sea_orm::TryGetable;
        let active_workers = i64::try_get_by(&row, "active_workers").unwrap_or(0);
        let cidr = Option::<String>::try_get_by(&row, "control_plane_compute_cidr")
            .ok()
            .flatten();
        let underlay = Option::<String>::try_get_by(&row, "control_plane_underlay_address")
            .ok()
            .flatten();
        let ready = bool::try_get_by(&row, "control_plane_overlay_ready").unwrap_or(false);
        let missing = i64::try_get_by(&row, "missing_service_endpoints").unwrap_or(0);

        report.add(
            "Workers",
            CheckResult::Info(format!("{active_workers} active")),
        );
        if active_workers == 0 {
            report.add(
                "Control-plane overlay",
                CheckResult::Info("Not required until a worker is active".to_string()),
            );
            return;
        }

        match (cidr, underlay, ready) {
            (Some(cidr), Some(underlay), true) => report.add(
                "Control-plane overlay",
                CheckResult::Pass(format!("{cidr} via {underlay}")),
            ),
            (Some(cidr), Some(underlay), false) => report.add(
                "Control-plane overlay",
                CheckResult::Fail(format!(
                    "Reserved as {cidr} via {underlay}, but setup did not complete. Run `sudo temps network setup-multi-node`."
                )),
            ),
            _ => report.add(
                "Control-plane overlay",
                CheckResult::Fail(
                    "Not configured. Run `sudo temps network setup-multi-node` on the control plane."
                        .to_string(),
                ),
            ),
        }

        // The cluster's overlay name (ADR 059); `temps0` before the column
        // existed.
        let overlay_name = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT docker_network_name FROM network_config WHERE id = 1".to_string(),
            ))
            .await
            .ok()
            .flatten()
            .and_then(|row| String::try_get_by(&row, "docker_network_name").ok())
            .unwrap_or_else(|| temps_network::DEFAULT_OVERLAY_NETWORK_NAME.to_string());
        let overlay_exists = match bollard::Docker::connect_with_defaults() {
            Ok(docker) => tokio::time::timeout(
                std::time::Duration::from_secs(5),
                docker.list_networks(None::<bollard::query_parameters::ListNetworksOptions>),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .map(|networks| {
                networks
                    .iter()
                    .any(|network| network.name.as_deref() == Some(overlay_name.as_str()))
            })
            .unwrap_or(false),
            Err(_) => false,
        };
        report.add(
            "Docker overlay",
            if overlay_exists {
                CheckResult::Pass(format!("{overlay_name} exists"))
            } else {
                CheckResult::Fail(format!(
                    "{overlay_name} is missing. Run `sudo temps network setup-multi-node`."
                ))
            },
        );
        report.add(
            "Managed-service DNS",
            if missing == 0 {
                CheckResult::Pass("All local standalone services are published".to_string())
            } else {
                CheckResult::Fail(format!(
                    "{missing} local service(s) have no internal endpoint. Run `sudo temps network setup-multi-node`."
                ))
            },
        );
    }

    /// Internal DNS names two owners publish (ADR-055 §10). The holder's
    /// records are served and the other owner's publishes are refused, so a
    /// conflict never breaks resolution by itself, but the refused owner
    /// stops updating its records until one side is renamed or removed.
    async fn check_dns_name_conflicts(
        &self,
        db: &sea_orm::DatabaseConnection,
        report: &mut DiagnosticReport,
    ) {
        let query = "SELECT fqdn, owner_kind, owner_id, holder_kind, holder_id \
            FROM dns_name_conflicts ORDER BY first_seen_at, id LIMIT 21";
        let rows = match db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                query.to_string(),
            ))
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                report.add(
                    "Name conflicts",
                    CheckResult::Warn(format!(
                        "Could not read dns_name_conflicts: {error}. Run `temps migrate` first."
                    )),
                );
                return;
            }
        };
        use sea_orm::TryGetable;
        let conflicts: Vec<DnsNameConflict> = rows
            .iter()
            .filter_map(|row| {
                Some(DnsNameConflict {
                    fqdn: String::try_get_by(row, "fqdn").ok()?,
                    owner_kind: String::try_get_by(row, "owner_kind").ok()?,
                    owner_id: i64::try_get_by(row, "owner_id").ok()?,
                    holder_kind: String::try_get_by(row, "holder_kind").ok()?,
                    holder_id: i64::try_get_by(row, "holder_id").ok()?,
                })
            })
            .collect();
        report.add("Name conflicts", dns_name_conflicts_result(&conflicts, 20));
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

/// What the database section left for the checks that need a connection.
enum DatabaseState {
    /// No source supplied a URL.
    NoUrl,
    /// A URL was found, but the database could not be reached.
    Unreachable,
    Connected(sea_orm::DatabaseConnection),
}

/// The outcome of counting users for the initial-admin check.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UserCount {
    NoDatabaseUrl,
    Unreachable,
    QueryFailed(String),
    Count(i64),
}

/// Assess persisted bootstrap state without reading first-boot inputs.
fn initial_admin_result(users: &UserCount, serving: Option<&ServiceDefinition>) -> CheckResult {
    match users {
        UserCount::Count(count) if *count > 0 => CheckResult::Pass(format!(
            "{count} user(s) exist; initial user bootstrap has completed"
        )),
        UserCount::Count(_) => CheckResult::Warn(no_admin_message(serving)),
        UserCount::NoDatabaseUrl => CheckResult::Warn(
            "Not checked: no database URL was found (see Database above)".to_string(),
        ),
        UserCount::Unreachable => CheckResult::Warn(
            "Not checked: the database could not be reached (see Database above)".to_string(),
        ),
        UserCount::QueryFailed(error) => CheckResult::Warn(format!(
            "Could not count users: {error}. If the schema is not migrated yet, run `temps migrate`"
        )),
    }
}

const ADMIN_EMAIL_ENV: &str = "TEMPS_ADMIN_EMAIL";
const ADMIN_PASSWORD_FILE_ENV: &str = "TEMPS_ADMIN_PASSWORD_FILE";

/// What creates the first admin when there is none. `temps serve` does it
/// as its console starts: from TEMPS_ADMIN_EMAIL and
/// TEMPS_ADMIN_PASSWORD_FILE, else by asking on its terminal. A service has
/// no terminal, so without both its console does not start; the installed
/// service's environment says which case the server is in.
fn no_admin_message(serving: Option<&ServiceDefinition>) -> String {
    let general = format!(
        "No users exist yet. `temps serve` creates the first admin when its console starts: \
         in a terminal it asks for the admin's email; run as a service it needs \
         {ADMIN_EMAIL_ENV} and {ADMIN_PASSWORD_FILE_ENV} (a file holding the password) in its \
         environment, and its console does not start without them"
    );
    let Some(service) = serving else {
        return general;
    };
    if let Some(gap) = service.incomplete_environment() {
        return format!(
            "{general}. Whether the installed service {} sets them is unknown: {gap}",
            service.path.display()
        );
    }
    let path = service.path.display();
    match (
        service.env(ADMIN_EMAIL_ENV).is_some(),
        service.env(ADMIN_PASSWORD_FILE_ENV).is_some(),
    ) {
        (true, true) => format!(
            "No users exist yet. The installed service {path} sets {ADMIN_EMAIL_ENV} and \
             {ADMIN_PASSWORD_FILE_ENV}, so `temps serve` creates the first admin when its \
             console starts. If the server is running, its console has not started: see the \
             server's log"
        ),
        (false, false) => format!(
            "No users exist yet, and the installed service {path} sets neither \
             {ADMIN_EMAIL_ENV} nor {ADMIN_PASSWORD_FILE_ENV}. With no terminal to ask for the \
             admin's email, `temps serve` does not start its console: set both in the \
             service's environment ({ADMIN_PASSWORD_FILE_ENV} is a file holding the password), \
             or stop the service and run `temps serve` once in a terminal"
        ),
        (email, _) => {
            let (set, missing) = if email {
                (ADMIN_EMAIL_ENV, ADMIN_PASSWORD_FILE_ENV)
            } else {
                (ADMIN_PASSWORD_FILE_ENV, ADMIN_EMAIL_ENV)
            };
            format!(
                "No users exist yet, and the installed service {path} sets {set} but not \
                 {missing}. `temps serve` needs both to create the first admin, and does not \
                 start its console without them: set {missing} in the service's environment"
            )
        }
    }
}

/// A directory's runtime secrets: the auth secret counts double, because
/// `temps serve` always writes both, while the installer copies only
/// `encryption_key` up to `~/.temps` beside the real `~/.temps/data`.
fn runtime_files_score(dir: &Path) -> u8 {
    u8::from(dir.join(temps_config::AUTH_SECRET_FILE).is_file()) * 2
        + u8::from(dir.join(temps_config::ENCRYPTION_KEY_FILE).is_file())
}

/// Resolve the default installation layout without overriding an explicit
/// `--data-dir` or `TEMPS_DATA_DIR`. Current installs use `~/.temps`, while
/// the installer (and older deployment scripts) run the server on
/// `~/.temps/data`; the directory holding more of the server's runtime
/// files wins, the root on a tie.
pub(super) fn default_data_dir(home: &Path) -> PathBuf {
    let root = home.join(".temps");
    let nested = root.join("data");
    if runtime_files_score(&nested) > runtime_files_score(&root) {
        nested
    } else {
        root
    }
}

/// The data directory doctor checks.
#[derive(Debug)]
struct DoctorDataDir {
    dir: PathBuf,
    /// Where the answer came from.
    source: String,
    /// The default layout guessed while an installed service exists whose
    /// data directory could not be found: it may not be the server's.
    unverified: bool,
}

/// Doctor's data directory and a description of where it came from:
/// explicit, else the installed service's (what the server really uses),
/// else the default layout.
fn resolve_doctor_data_dir(
    explicit: Option<&Path>,
    service: Option<&ServiceDefinition>,
    probes: &[ServiceProbe],
    home: &Path,
) -> DoctorDataDir {
    if let Some(dir) = explicit {
        return DoctorDataDir {
            dir: dir.to_path_buf(),
            source: "from --data-dir / TEMPS_DATA_DIR".to_string(),
            unverified: false,
        };
    }
    let unverified = service.is_some() || ignored_probes(probes).next().is_some();
    let why_default = match service {
        Some(service) => match &service.data_dir {
            Ok((dir, location)) => {
                return DoctorDataDir {
                    dir: dir.clone(),
                    source: format!("from {} in {}", location.describe(), service.path.display()),
                    unverified: false,
                };
            }
            Err(reason) => format!(
                "the data directory of {} is unknown ({reason}); pass --data-dir",
                service.path.display()
            ),
        },
        None => match ignored_probes(probes).next() {
            Some((path, _)) => format!(
                "the installed service {} could not be used (see Installed service); pass \
                 --data-dir",
                path.display()
            ),
            None => "no --data-dir, TEMPS_DATA_DIR or installed service".to_string(),
        },
    };
    DoctorDataDir {
        dir: default_data_dir(home),
        source: format!("default layout: {why_default}"),
        unverified,
    }
}

fn ignored_probes(probes: &[ServiceProbe]) -> impl Iterator<Item = (&Path, &str)> {
    probes.iter().filter_map(|probe| match probe {
        ServiceProbe::Ignored { path, reason } => Some((path.as_path(), reason.as_str())),
        ServiceProbe::Absent { .. } | ServiceProbe::Found(_) => None,
    })
}

/// The installed service doctor read, and anything it references that could
/// not be used.
fn check_installed_service(
    service: Option<&ServiceDefinition>,
    probes: &[ServiceProbe],
    report: &mut DiagnosticReport,
) {
    // A definition that exists but is not used (another user can rewrite it,
    // this user cannot read it) is the likeliest reason nothing below
    // matches the server.
    let mut ignored = false;
    for (path, reason) in ignored_probes(probes) {
        ignored = true;
        report.add(
            "Ignored",
            CheckResult::Warn(format!("{} {reason}", path.display())),
        );
    }
    let Some(service) = service else {
        if ignored {
            return;
        }
        let checked = service_definition::ServiceLocations::installed()
            .paths()
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        report.add(
            "Definition",
            CheckResult::Info(if checked.is_empty() {
                "No installed service location is known on this platform".to_string()
            } else {
                format!("None found (checked {})", checked.join(", "))
            }),
        );
        return;
    };
    report.add(
        "Definition",
        CheckResult::Info(service.path.display().to_string()),
    );
    for problem in &service.problems {
        report.add("Definition", CheckResult::Warn(problem.clone()));
    }
    for gap in &service.environment_gaps {
        if !service.problems.contains(gap) {
            report.add("Definition", CheckResult::Warn(gap.clone()));
        }
    }
}

fn describe_secret_source(source: &temps_config::SecretSource) -> String {
    match source {
        temps_config::SecretSource::Environment { variable } => {
            format!("injected with {variable}")
        }
        temps_config::SecretSource::InjectedFile { variable, path } => {
            format!("{}, from {variable}", path.display())
        }
        temps_config::SecretSource::DataDir { path } => path.display().to_string(),
    }
}

/// A read this user is not allowed to make: `temps serve` may well be (it
/// often runs as root), so it is unverified rather than failed.
fn unreadable_by_this_user(error: &temps_config::ConfigServiceError) -> Option<&Path> {
    match error {
        temps_config::ConfigServiceError::LocalSecretRead { path, source }
        | temps_config::ConfigServiceError::SecretFileRead { path, source, .. }
            if source.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            Some(path)
        }
        _ => None,
    }
}

/// The service's environment could not be read completely, so what it
/// injects (and so which secrets the server uses) is unknown.
fn environment_unknown_result(definition: &Path, reason: &str) -> CheckResult {
    CheckResult::Warn(format!(
        "Could not verify: the environment {} gives the server could not be read completely \
         ({reason}). Run `sudo temps doctor`, or run it as the user `temps serve` runs as",
        definition.display()
    ))
}

fn unverifiable(path: &Path) -> CheckResult {
    CheckResult::Warn(format!(
        "Could not verify: {} cannot be read by this user. Run `sudo temps doctor`, or run it \
         as the user `temps serve` runs as",
        path.display()
    ))
}

/// FAIL only where `temps serve` would refuse to start: a missing file is
/// generated at startup, and one this user cannot read may be readable by
/// the server.
fn auth_secret_result(
    inspected: &Result<temps_config::InspectedSecret, temps_config::ConfigServiceError>,
) -> CheckResult {
    match inspected {
        Ok(temps_config::InspectedSecret::Found { source, value }) => {
            match temps_core::CookieCrypto::new(value) {
                Ok(_) => CheckResult::Pass(format!("Present ({})", describe_secret_source(source))),
                Err(error) => CheckResult::Fail(format!(
                    "{} holds an auth secret `temps serve` rejects: {error}",
                    describe_secret_source(source)
                )),
            }
        }
        Ok(temps_config::InspectedSecret::NotCreatedYet { path }) => CheckResult::Warn(format!(
            "{} does not exist yet. `temps serve` generates it when it starts, so this is \
             expected before the first start; if the server is already running, it uses a \
             different data directory (pass --data-dir)",
            path.display()
        )),
        Err(error) => match unreadable_by_this_user(error) {
            Some(path) => unverifiable(path),
            None => CheckResult::Fail(format!("`temps serve` would refuse to start: {error}")),
        },
    }
}

/// Like [`auth_secret_result`], except that a missing key on an installation
/// that already has its auth secret is a failure: the key serve would
/// generate cannot decrypt the credentials stored with the old one.
fn encryption_key_result(
    inspected: &Result<temps_config::InspectedSecret, temps_config::ConfigServiceError>,
    initialized: bool,
) -> CheckResult {
    match inspected {
        Ok(temps_config::InspectedSecret::Found { source, value }) => {
            match temps_core::EncryptionService::new(value) {
                Ok(_) => CheckResult::Pass(format!(
                    "Valid, 32 bytes ({})",
                    describe_secret_source(source)
                )),
                Err(error) => CheckResult::Fail(format!(
                    "{} holds an encryption key `temps serve` rejects: {error}",
                    describe_secret_source(source)
                )),
            }
        }
        Ok(temps_config::InspectedSecret::NotCreatedYet { path }) if initialized => {
            CheckResult::Fail(format!(
                "{} is missing, but this data directory already has an auth secret: \
                 `temps serve` would generate a new key that cannot decrypt the credentials \
                 stored with the old one. Restore the original encryption_key",
                path.display()
            ))
        }
        Ok(temps_config::InspectedSecret::NotCreatedYet { path }) => CheckResult::Warn(format!(
            "{} does not exist yet. `temps serve` generates it when it starts, so this is \
             expected before the first start; if the server is already running, it uses a \
             different data directory (pass --data-dir)",
            path.display()
        )),
        Err(error) => match unreadable_by_this_user(error) {
            Some(path) => unverifiable(path),
            None => CheckResult::Fail(format!("`temps serve` would refuse to start: {error}")),
        },
    }
}

/// Where `temps serve` opens GeoLite2 (`resolve_mmdb_path`): its working
/// directory, then the data directory. Nowhere else, so a copy elsewhere
/// (such as /usr/share/GeoIP) is not one the server uses.
fn geolite_result(data_dir: &Path, working_directory: Option<&Path>) -> CheckResult {
    let candidates: Vec<PathBuf> = working_directory
        .map(|dir| dir.join(temps_geo::CITY_DB_FILENAME))
        .into_iter()
        .chain(std::iter::once(data_dir.join(temps_geo::CITY_DB_FILENAME)))
        .collect();
    if let Some(found) = candidates.iter().find(|path| path.is_file()) {
        return CheckResult::Pass(found.display().to_string());
    }
    let mut looked_in = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ");
    // `resolve_mmdb_path` tries the server's working directory first. This
    // process's own is not the server's, so it is not a stand-in.
    if working_directory.is_none() {
        looked_in.push_str(
            " (the server's working directory is unknown, and a copy there would be used first)",
        );
    }
    let system_copy = Path::new("/usr/share/GeoIP").join(temps_geo::CITY_DB_FILENAME);
    if system_copy.is_file() {
        return CheckResult::Warn(format!(
            "Not in {looked_in}. A copy exists at {}, but `temps serve` does not read that \
             location: copy it to {}",
            system_copy.display(),
            data_dir.join(temps_geo::CITY_DB_FILENAME).display()
        ));
    }
    if working_directory.is_none() {
        return CheckResult::Warn(format!(
            "Not found in {looked_in}. If it is not there either, `temps serve` tries to \
             download it at startup, and geolocation is disabled until it has it"
        ));
    }
    CheckResult::Warn(format!(
        "Not found in {looked_in}. `temps serve` tries to download it at startup; until it is \
         there, geolocation is disabled"
    ))
}

/// `temps serve` creates `<data dir>/logs` every time it starts and refuses
/// to start when it cannot.
/// Whether this user can create a file in `data_dir`. The probe is created
/// exclusively under a random name and removed on drop, so it never opens an
/// existing file: with sudo, a symlink planted in a directory the service
/// account can write would otherwise have root truncate its target.
fn probe_writable(data_dir: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut probe = tempfile::Builder::new()
        .prefix(".doctor-test-")
        .tempfile_in(data_dir)?;
    probe.write_all(b"test")?;
    probe.flush()
}

/// Why `data_dir` cannot be checked, when it cannot: missing, or not
/// visible to this user (`Path::exists` says "missing" for both). With an
/// installed server a missing directory is not a setup step: `temps serve`
/// creates it on its first start, and a guessed one says nothing.
fn data_dir_absence(data_dir: &Path, unverified: bool, has_service: bool) -> Option<CheckResult> {
    let error = std::fs::metadata(data_dir).err()?;
    let dir = data_dir.display();
    if error.kind() != std::io::ErrorKind::NotFound {
        return Some(CheckResult::Warn(format!(
            "{dir} cannot be inspected by this user ({error}), so whether it exists and what it \
             holds is unknown. Run doctor with sudo or as the service's user"
        )));
    }
    Some(if unverified {
        CheckResult::Warn(format!(
            "{dir} does not exist, but it is only this user's default: the installed server's \
             data directory is unknown (see above). Run doctor with sudo or as the service's \
             user, or pass --data-dir"
        ))
    } else if has_service {
        CheckResult::Warn(format!(
            "{dir} does not exist yet. `temps serve` creates it when it first starts with this \
             data directory, so the installed server has not started yet, or it uses a \
             different one (pass --data-dir)"
        ))
    } else {
        CheckResult::Fail("Data directory does not exist. Run `temps setup` first.".to_string())
    })
}

fn logs_dir_result(data_dir: &Path) -> CheckResult {
    let logs = data_dir.join(temps_config::PIPELINE_LOGS_DIR_NAME);
    if logs.is_dir() {
        return CheckResult::Pass(logs.display().to_string());
    }
    if logs.exists() {
        return CheckResult::Fail(format!(
            "{} is not a directory, so `temps serve` cannot create its logs directory and \
             refuses to start",
            logs.display()
        ));
    }
    CheckResult::Warn(format!(
        "{} does not exist. `temps serve` creates it each time its console starts, after \
         checking Docker, so the server has not started with this data directory yet, its \
         console failed to start (see Docker above, and the server's log), or it runs with a \
         different data directory (pass --data-dir)",
        logs.display()
    ))
}

/// The Docker network name: this shell's TEMPS_NETWORK_NAME when set, else
/// the installed service's, else the default — what the server resolves.
fn docker_network_name(service: Option<&ServiceDefinition>) -> String {
    if std::env::var_os("TEMPS_NETWORK_NAME").is_some() {
        return temps_core::NETWORK_NAME.clone();
    }
    service
        .and_then(|service| service.env("TEMPS_NETWORK_NAME"))
        .map(str::to_string)
        .unwrap_or_else(|| temps_core::NETWORK_NAME.clone())
}

/// The installed service talks to a different Docker daemon than the one
/// these checks probe.
fn service_docker_host_mismatch(
    service: Option<&ServiceDefinition>,
    probed: &str,
) -> Option<String> {
    let service = service?;
    let host = service.env("DOCKER_HOST")?;
    (host != probed).then(|| {
        format!(
            "{} sets DOCKER_HOST to a different endpoint than the one checked here; run with \
             the same DOCKER_HOST to check the daemon the server uses",
            service.path.display()
        )
    })
}

/// The URL doctor was given is not the one the installed server uses, so the
/// database checks describe some other database.
fn service_url_mismatch(resolved: &ResolvedDatabaseUrl) -> Option<String> {
    if matches!(
        resolved.origin(),
        DatabaseUrlOrigin::InstalledService { .. }
    ) {
        return None;
    }
    let service = resolved.service()?;
    let service_url = service.url()?;
    (!crate::database_url_source::same_database(service_url, resolved.url())).then(|| {
        format!(
            "{} connects to a different database URL than {}; the checks below are for the \
             one given here",
            service.path.display(),
            resolved.origin().describe()
        )
    })
}

/// Other candidate data directories (`~/.temps` and the nested
/// `~/.temps/data`) that already hold `marker` — a path relative to a data
/// directory, such as `microsandbox/bin/msb` — excluding `data_dir` itself.
///
/// `temps microsandbox setup` / `temps firecracker setup` run from a shell
/// have no `TEMPS_DATA_DIR`, so they can install into a directory the server
/// never reads. A hit here is the signature of that mistake.
pub(super) fn other_data_dirs_with(data_dir: &Path, home: &Path, marker: &Path) -> Vec<PathBuf> {
    let root = home.join(".temps");
    [root.clone(), root.join("data")]
        .into_iter()
        .filter(|candidate| candidate != data_dir)
        .filter(|candidate| candidate.join(marker).is_file())
        .collect()
}

/// Read the geolocation section of the singleton settings row.
///
/// Returns the defaults when the row does not exist yet, which is a valid
/// state on a fresh install and must read as "never refreshed" rather than as
/// a failure.
async fn read_geo_settings(
    db: &sea_orm::DatabaseConnection,
) -> anyhow::Result<temps_config::GeoSettings> {
    use sea_orm::TryGetable;

    let Some(row) = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT data FROM settings WHERE id = 1".to_string(),
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to query the settings row: {}", e))?
    else {
        return Ok(temps_config::GeoSettings::default());
    };

    let data = serde_json::Value::try_get_by(&row, "data")
        .map_err(|e| anyhow::anyhow!("Failed to read the settings 'data' column: {:?}", e))?;
    Ok(temps_config::AppSettings::from_json(data).geo)
}

fn iso8601(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Render an age in days the way an operator reads it ("3 days old").
fn describe_age(days: i64) -> String {
    match days {
        d if d <= 0 => "less than a day old".to_string(),
        1 => "1 day old".to_string(),
        d if d < 60 => format!("{} days old", d),
        d if d < 365 => format!("{} days old (about {} months)", d, d / 30),
        d => format!("{} days old (about {} years)", d, d / 365),
    }
}

/// Check if a URL is reachable via HTTPS.
/// One row of `dns_name_conflicts`, as `temps doctor` reports it.
struct DnsNameConflict {
    fqdn: String,
    owner_kind: String,
    owner_id: i64,
    holder_kind: String,
    holder_id: i64,
}

/// `conflicts` holds at most `shown + 1` rows; the extra one only says
/// there are more.
fn dns_name_conflicts_result(conflicts: &[DnsNameConflict], shown: usize) -> CheckResult {
    if conflicts.is_empty() {
        return CheckResult::Pass("Every internal name has a single owner".to_string());
    }
    let mut lines: Vec<String> = conflicts
        .iter()
        .take(shown)
        .map(|c| {
            format!(
                "'{}' is held by {} {}; {} {} cannot publish it",
                c.fqdn, c.holder_kind, c.holder_id, c.owner_kind, c.owner_id
            )
        })
        .collect();
    if conflicts.len() > shown {
        lines.push("... and more: see GET /api/cluster/dns/status".to_string());
    }
    CheckResult::Warn(format!(
        "{}. Rename one owner of each name or remove the other; the refused owner's \
         records stop updating until then.",
        lines.join("; ")
    ))
}

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

/// Turn "newest release on the channel" into a doctor line. Only a strictly
/// newer release is reported as an update, and the suggested command names
/// the channel so it installs what was suggested (plain `temps upgrade`
/// tracks stable).
/// Doctor's "Migrations" line for a classified schema.
fn migration_check_result(status: Result<&temps_schema::SchemaStatus, &String>) -> CheckResult {
    use temps_schema::SchemaStatus;
    let pending_hint = |count: usize| {
        format!("{count} PENDING — run `temps migrate` before restarting the server")
    };
    match status {
        Err(error) => CheckResult::Warn(format!("Could not read the migration ledger: {error}")),
        Ok(SchemaStatus::Fresh) => CheckResult::Warn(
            "No migrations applied yet. Run `temps migrate` to initialize the schema.".to_string(),
        ),
        Ok(SchemaStatus::UpToDate { applied }) => {
            CheckResult::Pass(format!("{applied} applied, up to date"))
        }
        Ok(SchemaStatus::PendingUpgrade { applied, pending }) => CheckResult::Warn(format!(
            "{applied} applied, {}",
            pending_hint(pending.len())
        )),
        Ok(status @ SchemaStatus::NewerThanBinary { pending, .. }) => {
            let warning = status.newer_schema_warning().unwrap_or_default();
            if pending.is_empty() {
                CheckResult::Warn(warning)
            } else {
                CheckResult::Warn(format!(
                    "{warning} This binary also defines {} migration(s) the database lacks, \
                     which its next start applies: {}",
                    pending.len(),
                    pending.join(", ")
                ))
            }
        }
    }
}

/// The "Lock capacity" check: warn when the lock table cannot hold the locks a
/// migration changing every hypertable's constraints at once would take, with
/// the `max_locks_per_transaction` to set. Informational without TimescaleDB.
fn lock_capacity_result(capacity: Result<&temps_schema::LockCapacity, &String>) -> CheckResult {
    match capacity {
        Err(error) => CheckResult::Warn(format!(
            "Could not read max_locks_per_transaction or the TimescaleDB chunk count: {error}"
        )),
        Ok(capacity) if !capacity.timescaledb => CheckResult::Info(format!(
            "TimescaleDB is not installed, so there are no chunks to lock; the lock table holds \
             about {} locks",
            capacity.lock_slots()
        )),
        Ok(capacity) if capacity.at_risk() => CheckResult::Warn(format!(
            "{}. A migration that changes the constraints of every hypertable at once would fail \
             with \"out of shared memory\" (SQLSTATE 53200). {} Do this before installing a new \
             release.",
            capacity.describe(),
            capacity.remedy()
        )),
        Ok(capacity) => CheckResult::Pass(capacity.describe()),
    }
}

/// The "Telemetry foreign keys" check: warn, naming them, while foreign keys
/// the post-migration step has not dropped yet still make project deletes
/// refuse.
fn telemetry_foreign_keys_result(
    remaining: Result<&Vec<temps_schema::TelemetryForeignKey>, &String>,
) -> CheckResult {
    match remaining {
        Err(error) => CheckResult::Warn(format!(
            "Could not list the telemetry foreign keys: {error}"
        )),
        Ok(remaining) if remaining.is_empty() => {
            CheckResult::Pass("telemetry tables are out of the project delete cascade".to_string())
        }
        Ok(remaining) => CheckResult::Warn(format!(
            "{} foreign key(s) still tie telemetry tables to the project delete cascade ({}), so \
             project deletes are refused. Post-migration maintenance drops them after `temps \
             serve` starts; run `temps migrate` to do it now and see what stops it",
            remaining.len(),
            remaining
                .iter()
                .map(|foreign_key| format!("{}.{}", foreign_key.table, foreign_key.constraint))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn update_check_result(
    current: &str,
    channel: upgrade::UpgradeChannel,
    latest: &str,
) -> CheckResult {
    let channel_name = channel.as_str();
    match upgrade::update_verdict(current, latest) {
        upgrade::UpdateVerdict::UpToDate => {
            CheckResult::Pass(format!("Up to date on the {channel_name} channel"))
        }
        upgrade::UpdateVerdict::Available => CheckResult::Warn(format!(
            "{latest} available on the {channel_name} channel - run \
             `temps upgrade --channel {channel_name}` to update"
        )),
        upgrade::UpdateVerdict::AheadOfChannel => CheckResult::Pass(format!(
            "Up to date on the {channel_name} channel ({current} is newer than the newest \
             published {channel_name} release, {latest})"
        )),
        upgrade::UpdateVerdict::Incomparable => CheckResult::Info(format!(
            "Newest {channel_name} release is {latest}; this build ({current}) is not a \
             release tag, so it cannot be compared"
        )),
    }
}

#[cfg(test)]
mod tests;
