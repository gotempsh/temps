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
    DoctorCommand, DomainCommand, EdgeCommand, JoinCommand, NetworkCommand, NodeCommand,
    ProxyCommand, ResetPasswordCommand, SandboxCommand, ServeCommand, ServicesCommand,
    SetupCommand, UpgradeCommand,
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
}

/// Install the global tracing subscriber. Safe to call once per process.
pub fn install_tracing(log_level: &str, log_format: &str) {
    let filter = if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .expect("Invalid RUST_LOG environment variable")
    } else {
        tracing_subscriber::EnvFilter::new(format!(
            "temps_cli={level},\
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

    let subscriber = tracing_subscriber::registry().with(filter).with(fmt_layer);
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");
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

/// Convenience entrypoint that parses, installs tracing, and dispatches.
/// Used by both the OSS `temps` binary (`extra_plugins = vec![]`) and any
/// EE-bundled binary that wraps the same CLI surface.
pub fn run(extra_plugins: Vec<Box<dyn temps_core::plugin::TempsPlugin>>) -> anyhow::Result<()> {
    install_crypto_provider();
    let cli = Cli::parse();
    install_tracing(&cli.log_level, &cli.log_format);
    dispatch(cli, extra_plugins)
}
