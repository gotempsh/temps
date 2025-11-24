//! Temps CLI - Single entrypoint for all services
//!
//! This application orchestrates all the library crates and provides
//! different execution modes: server, proxy, worker, or combined.

mod commands;

use clap::{Parser, Subcommand};
use commands::{BackupCommand, ProxyCommand, ResetPasswordCommand, ServeCommand};
use tracing_subscriber::{layer::SubscriberExt, Layer};

#[derive(Parser)]
#[command(
    author,
    version = env!("TEMPS_VERSION"),
    about,
    long_about = None
)]
struct Cli {
    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", env = "TEMPS_LOG_LEVEL", global = true)]
    log_level: String,

    /// Log format: compact, full
    #[arg(
        long,
        default_value = "compact",
        env = "TEMPS_LOG_FORMAT",
        global = true
    )]
    log_format: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP API server
    Serve(ServeCommand),
    /// Start only the proxy server
    Proxy(ProxyCommand),
    /// Reset admin user password
    ResetAdminPassword(ResetPasswordCommand),
    /// Backup management commands
    Backup(BackupCommand),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Use log level from base CLI
    let log_level = cli.log_level.clone();

    // Configure logging with custom filter for cleaner output
    // If RUST_LOG is set, use it directly; otherwise use our default filter
    let filter = if std::env::var("RUST_LOG").is_ok() {
        // RUST_LOG is set, use it as-is (user wants full control)
        tracing_subscriber::EnvFilter::try_from_default_env()
            .expect("Invalid RUST_LOG environment variable")
    } else {
        // Use our default filter with all temps crates at the specified level
        // and noisy dependencies at warn level
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
             temps_mcp={level},\
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
             pingora=warn,\
             sqlx=warn,\
             sea_orm=warn,\
             h2=warn,\
             tower=warn,\
             hyper=warn,\
             reqwest=warn,\
             rustls=warn,\
             tungstenite=warn",
            level = log_level
        ))
    };

    // Configure tracing with filter and custom format
    let fmt_layer = match cli.log_format.as_str() {
        "full" => tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .boxed(),
        _ => tracing_subscriber::fmt::layer() // "compact" or any other value
            .compact()
            .with_target(false)
            .with_thread_ids(false)
            .with_thread_names(false)
            .boxed(),
    };

    let subscriber = tracing_subscriber::registry().with(filter).with(fmt_layer);
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");

    // Commands are now synchronous to be compatible with pingora
    match cli.command {
        Commands::Serve(serve_cmd) => serve_cmd.execute(),
        Commands::Proxy(proxy_cmd) => proxy_cmd.execute(),
        Commands::ResetAdminPassword(reset_cmd) => reset_cmd.execute(),
        Commands::Backup(backup_cmd) => backup_cmd.execute(),
    }
}
