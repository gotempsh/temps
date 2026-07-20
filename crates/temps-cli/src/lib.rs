//! Temps CLI — library entrypoint.
//!
//! Exposes the same dispatch as the OSS `temps` binary so an out-of-tree
//! binary (notably `temps-ee/apps/temps-cli`) can call into it while
//! injecting additional plugins for `temps serve`. This is the seam
//! described in ADR 0001 §"Extension points exposed by OSS".

pub mod commands;

use clap::{Parser, Subcommand};
use commands::{
    AgentCommand, ApiKeyCommand, BackfillCommand, BackupCommand, BuildCommand, DeployCommand,
    DoctorCommand, DomainCommand, EdgeCommand, FirecrackerCommand, JoinCommand, MigrateCommand,
    NetworkCommand, NodeCommand, ProxyCommand, ResetPasswordCommand, SandboxCommand, ServeCommand,
    ServicesCommand, SetupCommand, UpgradeCommand,
};
use tracing_subscriber::{layer::SubscriberExt, Layer};

#[derive(Parser)]
#[command(
    author,
    version = env!("TEMPS_VERSION"),
    about,
    long_about = None
)]
pub struct Cli {
    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", env = "TEMPS_LOG_LEVEL", global = true)]
    pub log_level: String,

    /// Log format: compact, full
    #[arg(
        long,
        default_value = "compact",
        env = "TEMPS_LOG_FORMAT",
        global = true
    )]
    pub log_format: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the HTTP API server
    Serve(ServeCommand),
    /// Start only the proxy server
    Proxy(ProxyCommand),
    /// Initial setup: create admin user, configure DNS/Git providers, and domain
    #[command(alias = "init")]
    Setup(Box<SetupCommand>),
    /// Apply pending database migrations (recommended before upgrading the server)
    Migrate(MigrateCommand),
    /// Reset admin user password
    ResetAdminPassword(ResetPasswordCommand),
    /// Create an API key with a specified role
    #[command(alias = "create-api-key")]
    ApiKey(ApiKeyCommand),
    /// Backup management commands
    Backup(BackupCommand),
    /// One-shot data migration utilities (e.g. TimescaleDB → ClickHouse)
    Backfill(BackfillCommand),
    /// Manage platform services (KV, Blob)
    Services(ServicesCommand),
    /// Domain and certificate management
    Domain(DomainCommand),
    /// Build a Docker image locally
    Build(BuildCommand),
    /// Deploy pre-built images or static files to environments
    Deploy(DeployCommand),
    /// Self-upgrade temps to the latest version
    #[command(alias = "self-update")]
    Upgrade(UpgradeCommand),
    /// Diagnose the temps installation and check system health
    Doctor(DoctorCommand),
    /// Join this machine to a Temps cluster as a worker node
    Join(JoinCommand),
    /// Run the worker node agent server
    Agent(AgentCommand),
    /// Manage cluster worker nodes (list, show, drain, remove)
    Node(NodeCommand),
    /// Inspect the multi-host overlay (status, peers, diag)
    Network(NetworkCommand),
    /// Start an edge CDN proxy node (caches static assets, proxies dynamic requests to origin)
    Edge(EdgeCommand),
    /// Manage standalone sandboxes via the Vercel-compatible `/v1/sandbox/*` API
    Sandbox(SandboxCommand),
    /// Provision and manage the Firecracker microVM sandbox backend
    Firecracker(FirecrackerCommand),
}

/// Install the global tracing subscriber. Safe to call once per process.
pub fn install_tracing(log_level: &str, log_format: &str) {
    install_tracing_extra(log_level, log_format, "");
}

/// Like [`install_tracing`], with extra filter directives appended — for
/// embedding binaries whose own crate targets aren't in the default list
/// (e.g. `vibetemps_api={level}`). `extra` is comma-separated directives,
/// empty for none.
pub fn install_tracing_extra(log_level: &str, log_format: &str, extra: &str) {
    let filter = if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .expect("Invalid RUST_LOG environment variable")
    } else {
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!("{extra},")
        };
        tracing_subscriber::EnvFilter::new(format!(
            "{extra}\
             temps_cli={level},\
             temps_deployments={level},\
             temps_deployer={level},\
             temps_core={level},\
             temps_git={level},\
             temps_projects={level},\
             temps_environments={level},\
             temps_domains={level},\
             temps_proxy={level},\
             temps_queue={level},\
             temps_logs={level},\
             temps_auth={level},\
             temps_providers={level},\
             temps_audit={level},\
             temps_backup={level},\
             temps_config={level},\
             temps_analytics={level},\
             temps_notifications={level},\
             temps_infra={level},\
             temps_geo={level},\
             temps_entities={level},\
             temps_database={level},\
             temps_migrations={level},\
             temps_presets={level},\
             temps_status_page={level},\
             temps_monitoring={level},\
             temps_metrics={level},\
             temps_routes={level},\
             temps_error_tracking={level},\
             temps_sentry_ingester={level},\
             temps_analytics_performance={level},\
             temps_analytics_session_replay={level},\
             temps_analytics_events={level},\
             temps_analytics_funnels={level},\
             temps_webhooks={level},\
             temps_external_plugins={level},\
             temps_plugin_sdk={level},\
             temps_blob={level},\
             temps_dns={level},\
             temps_email={level},\
             temps_embeddings={level},\
             temps_import={level},\
             temps_import_docker={level},\
             temps_import_types={level},\
             temps_kv={level},\
             temps_log_aggregator={level},\
             temps_otel={level},\
             temps_query={level},\
             temps_query_postgres={level},\
             temps_query_redis={level},\
             temps_query_s3={level},\
             temps_query_mongodb={level},\
             temps_screenshots={level},\
             temps_static_files={level},\
             temps_vulnerability_scanner={level},\
             temps_agent={level},\
             temps_edge={level},\
             temps_wireguard={level},\
             temps_ai={level},\
             temps_ai_gateway={level},\
             temps_ai_chat={level},\
             temps_ai_api_tools={level},\
             temps_agents={level},\
             pingora=warn,\
             sqlx=warn,\
             sea_orm=warn,\
             sea_orm_migration=warn,\
             h2=warn,\
             tower=warn,\
             hyper=warn,\
             reqwest=warn,\
             rustls=warn,\
             tungstenite=warn",
            level = log_level
        ))
    };

    let fmt_layer = match log_format {
        "full" => tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .boxed(),
        _ => tracing_subscriber::fmt::layer()
            .compact()
            .with_target(false)
            .with_thread_ids(false)
            .with_thread_names(false)
            .boxed(),
    };

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(ErrorMetricsLayer);
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");
}

/// Tracing layer that counts ERROR-level events for the anonymous
/// `error_summary` telemetry event (see `temps_core::error_metrics`).
///
/// Records ONLY the event's target — the module path, a compile-time
/// identifier of our own code. The message and all fields are ignored, so no
/// user data (IDs, paths, resource names embedded in error messages) can
/// reach telemetry. Counting is in-memory and bounded; nothing leaves the
/// process unless the serve command's telemetry flusher is running and the
/// operator hasn't opted out.
struct ErrorMetricsLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorMetricsLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() == tracing::Level::ERROR {
            temps_core::error_metrics::record_log_error(event.metadata().target());
        }
    }
}

// Re-exported so embedding binaries build their UI bundle with the exact
// `include_dir` version this crate compares `Dir` types against.
pub use include_dir;

/// Replace the embedded console SPA with the embedding binary's own UI
/// bundle. Call once, before [`dispatch`]. The bundle is served at the
/// document root with the same SPA-fallback semantics as the OSS console;
/// the `/api` surface is unaffected. Returns `Err` if already set.
pub fn set_embedded_ui(dir: &'static include_dir::Dir<'static>) -> Result<(), &'static str> {
    commands::serve::console::set_embedded_ui(dir)
}

/// Serve the ORIGINAL temps console on its own dedicated listener (extra
/// port) when the root bundle has been replaced via [`set_embedded_ui`].
/// The console SPA assumes it owns its origin, so a path prefix cannot work;
/// a separate listener gives it a clean origin. Same `/api`, same auth and
/// admin gate. Bind to loopback (e.g. `127.0.0.1:8082`) unless you
/// deliberately want it exposed. Call once, before [`dispatch`].
pub fn set_platform_console_addr(addr: impl Into<String>) -> Result<(), &'static str> {
    commands::serve::console::set_platform_console_addr(addr.into())
}

/// Dispatch the parsed CLI. `extra_plugins` is forwarded to `temps serve`
/// only; every other subcommand ignores it (they have no plugin lifecycle).
pub fn dispatch(
    cli: Cli,
    extra_plugins: Vec<Box<dyn temps_core::plugin::TempsPlugin>>,
) -> anyhow::Result<()> {
    // Commands are now synchronous to be compatible with pingora
    match cli.command {
        Commands::Serve(serve_cmd) => serve_cmd.execute_with_extra_plugins(extra_plugins),
        Commands::Proxy(proxy_cmd) => proxy_cmd.execute(),
        Commands::Setup(setup_cmd) => setup_cmd.execute(),
        Commands::Migrate(migrate_cmd) => migrate_cmd.execute(),
        Commands::ResetAdminPassword(reset_cmd) => reset_cmd.execute(),
        Commands::ApiKey(api_key_cmd) => api_key_cmd.execute(),
        Commands::Backup(backup_cmd) => backup_cmd.execute(),
        Commands::Backfill(backfill_cmd) => backfill_cmd.execute(),
        Commands::Services(services_cmd) => services_cmd.execute(),
        Commands::Domain(domain_cmd) => domain_cmd.execute(),
        Commands::Build(build_cmd) => build_cmd.execute(),
        Commands::Deploy(deploy_cmd) => deploy_cmd.execute(),
        Commands::Upgrade(upgrade_cmd) => upgrade_cmd.execute(),
        Commands::Doctor(doctor_cmd) => doctor_cmd.execute(),
        Commands::Join(join_cmd) => join_cmd.execute(),
        Commands::Agent(agent_cmd) => agent_cmd.execute(),
        Commands::Node(node_cmd) => node_cmd.execute(),
        Commands::Network(network_cmd) => network_cmd.execute(),
        Commands::Edge(edge_cmd) => edge_cmd.execute(),
        Commands::Sandbox(sandbox_cmd) => sandbox_cmd.execute(),
        Commands::Firecracker(firecracker_cmd) => firecracker_cmd.execute(),
    }
}

/// Install the process-wide rustls crypto provider exactly once.
///
/// Several dependencies (e.g. `temps-domains`) construct rustls clients and
/// expect a default `CryptoProvider` to be present. `install_default`
/// returns `Err` if one is already installed, which is the normal outcome on
/// the second and later calls — so the error is intentionally ignored,
/// giving the same idempotent behaviour the old library helper provided.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Scrub sensitive flag values from the process argv so they don't appear in
/// `pgrep`, `ps`, or `/proc/self/cmdline`.
///
/// Clap has already parsed the arguments by the time this runs, so we can
/// safely overwrite the raw argv strings with `x` characters in-place.
/// The length of each argument is preserved — we never reallocate.
///
/// Best-effort: if the platform doesn't support argv scrubbing, the process
/// continues normally. The flag is still accepted; it may remain visible in
/// the process table, which is no worse than before this change.
fn scrub_sensitive_argv() {
    const SENSITIVE_FLAGS: &[&str] = &["--database-url"];

    // macOS: use the stable _NSGetArgc/_NSGetArgv APIs from libSystem.
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn _NSGetArgc() -> *mut libc::c_int;
            fn _NSGetArgv() -> *mut *mut *mut libc::c_char;
        }
        unsafe {
            let argc_ptr = _NSGetArgc();
            let argv_ptr = _NSGetArgv();
            if !argc_ptr.is_null() && !argv_ptr.is_null() {
                scrub_argv_raw(*argc_ptr as usize, *argv_ptr, SENSITIVE_FLAGS);
            }
        }
    }

    // Linux: recover argv base from environ. The kernel stack layout is:
    //   argc | argv[0..argc] | NULL | envp[0..] | NULL
    // environ points to envp[0], so argv[0] is at environ[-(argc+1)].
    // We determine argc by counting NUL-terminated args in /proc/self/cmdline,
    // then validate by checking that argv[0] matches the cmdline prefix.
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CStr;

        let cmdline = match std::fs::read("/proc/self/cmdline") {
            Ok(b) if !b.is_empty() => b,
            _ => return,
        };
        let argc = cmdline.iter().filter(|&&b| b == 0).count();
        if argc == 0 {
            return;
        }

        unsafe {
            extern "C" {
                static environ: *const *const libc::c_char;
            }
            if environ.is_null() {
                return;
            }
            // environ[-1] should be the NULL terminator of argv.
            let argv_null = (environ as *mut *mut libc::c_char).sub(1);
            if !(*argv_null).is_null() {
                return;
            }
            // argv[0] is argc slots before the NULL.
            let argv = argv_null.sub(argc);
            // Sanity-check: argv[0] must match the first cmdline token.
            let first = *argv;
            if first.is_null() {
                return;
            }
            let first_bytes = CStr::from_ptr(first).to_bytes();
            let cmdline_first = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
            if first_bytes != cmdline_first {
                return;
            }
            scrub_argv_raw(argc, argv, SENSITIVE_FLAGS);
        }
    }
}

/// Overwrite sensitive flag values in a raw C argv array in-place.
///
/// # Safety
/// `argv` must point to a valid array of `argc` writable C strings
/// (i.e. the actual process argv, not a copy).
#[cfg(any(target_os = "linux", target_os = "macos"))]
unsafe fn scrub_argv_raw(argc: usize, argv: *mut *mut libc::c_char, sensitive_flags: &[&str]) {
    use std::ffi::CStr;

    let mut i = 0usize;
    while i < argc {
        let ptr = *argv.add(i);
        if ptr.is_null() {
            i += 1;
            continue;
        }
        let arg = CStr::from_ptr(ptr).to_string_lossy();

        // `--flag=value` form: overwrite only the value part after `=`.
        for flag in sensitive_flags {
            let prefix = format!("{}=", flag);
            if arg.starts_with(prefix.as_str()) {
                let value_offset = prefix.len();
                let total_len = libc::strlen(ptr);
                if value_offset < total_len {
                    std::ptr::write_bytes(ptr.add(value_offset), b'x', total_len - value_offset);
                }
            }
        }

        // `--flag value` form: the next argv slot holds the value.
        for flag in sensitive_flags {
            if arg.as_ref() == *flag && i + 1 < argc {
                let next = *argv.add(i + 1);
                if !next.is_null() {
                    let len = libc::strlen(next);
                    if len > 0 {
                        std::ptr::write_bytes(next, b'x', len);
                    }
                }
            }
        }

        i += 1;
    }
}

/// Convenience entrypoint that parses, installs tracing, and dispatches.
/// Used by both the OSS `temps` binary (`extra_plugins = vec![]`) and any
/// EE-bundled binary that wraps the same CLI surface.
pub fn run(extra_plugins: Vec<Box<dyn temps_core::plugin::TempsPlugin>>) -> anyhow::Result<()> {
    install_crypto_provider();
    let cli = Cli::parse();
    // Scrub sensitive flag values from argv *after* clap has parsed them so
    // they no longer appear in `pgrep -af` or /proc/self/cmdline.
    scrub_sensitive_argv();
    install_tracing(&cli.log_level, &cli.log_format);
    dispatch(cli, extra_plugins)
}

#[cfg(test)]
mod error_metrics_layer_tests {
    use super::*;
    use temps_core::error_metrics::{self, CATEGORY_LOG_ERROR};

    /// The layer must count ERROR events by target and ignore every other
    /// level. Targets are unique to this test so parallel tests can't
    /// interfere via the global counter store.
    #[test]
    fn counts_only_error_level_events_by_target() {
        let subscriber = tracing_subscriber::registry().with(ErrorMetricsLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(
                target: "layer_test_error_target",
                "message with user data {} that must never be recorded",
                "/home/alice/secret"
            );
            tracing::error!(target: "layer_test_error_target", "second error");
            tracing::warn!(target: "layer_test_warn_target", "not counted");
            tracing::info!(target: "layer_test_info_target", "not counted");
        });

        let counters = error_metrics::global();
        assert_eq!(
            counters.count_for(CATEGORY_LOG_ERROR, "layer_test_error_target"),
            2
        );
        assert_eq!(
            counters.count_for(CATEGORY_LOG_ERROR, "layer_test_warn_target"),
            0
        );
        assert_eq!(
            counters.count_for(CATEGORY_LOG_ERROR, "layer_test_info_target"),
            0
        );
    }
}
