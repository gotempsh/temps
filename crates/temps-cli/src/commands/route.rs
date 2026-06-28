//! Reverse-proxy route management commands (direct database access).
//!
//! A *custom route* maps an external hostname to an arbitrary upstream
//! `host:port` that Temps does not itself deploy or manage — the building block
//! for fronting a side-car service (a self-hosted Sentry, an object store, a
//! legacy app) with the same `:443` that Temps already owns. The backend table
//! and matching logic already exist (`custom_routes` + [`LbService`]); this
//! command makes them a first-class, scriptable surface instead of requiring
//! hand-written `INSERT`s.
//!
//! TLS for the hostname is managed separately via `temps domain` (`add` for an
//! ACME-issued cert, or `import` for a custom/self-signed one). A route with
//! `--type tls` performs SNI passthrough and needs no Temps-held cert; a route
//! with the default `--type http` terminates TLS at the proxy and therefore
//! needs a `domains` cert for its hostname to be served over HTTPS.

use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;
use temps_database::establish_connection;
use temps_entities::custom_routes::RouteType;
use temps_proxy::service::lb_service::LbService;

/// Reverse-proxy route management commands
#[derive(Args)]
pub struct RouteCommand {
    #[command(subcommand)]
    pub command: RouteSubcommand,
}

#[derive(Subcommand)]
pub enum RouteSubcommand {
    /// Create a route mapping a hostname to an upstream host:port
    Add(AddRouteCommand),
    /// List all configured routes
    #[command(alias = "ls")]
    List(ListRoutesCommand),
    /// Show details for a single route
    Show(ShowRouteCommand),
    /// Delete a route
    #[command(alias = "rm")]
    Delete(DeleteRouteCommand),
}

/// How the proxy matches and forwards traffic for a route.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RouteTypeArg {
    /// Terminate TLS at the proxy and match on the HTTP Host header (Layer 7).
    /// Requires a `domains` certificate for the hostname to serve HTTPS.
    Http,
    /// Match on the TLS SNI hostname and pass the connection through without
    /// terminating TLS (Layer 4). The upstream presents its own certificate.
    Tls,
}

impl From<RouteTypeArg> for RouteType {
    fn from(arg: RouteTypeArg) -> Self {
        match arg {
            RouteTypeArg::Http => RouteType::Http,
            RouteTypeArg::Tls => RouteType::Tls,
        }
    }
}

/// Create a new route
#[derive(Args)]
pub struct AddRouteCommand {
    /// Hostname to route (e.g. "sentry.example.com" or "*.example.com")
    #[arg(long, short = 'd')]
    pub domain: String,

    /// Upstream target as host:port (e.g. "127.0.0.1:9300")
    #[arg(long, short = 'u')]
    pub upstream: String,

    /// How the proxy handles this route
    #[arg(long, short = 't', value_enum, default_value = "http")]
    pub route_type: RouteTypeArg,

    /// Database URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,
}

/// List all routes
#[derive(Args)]
pub struct ListRoutesCommand {
    /// Database URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// Output as JSON
    #[arg(long, default_value = "false")]
    pub json: bool,
}

/// Show a single route
#[derive(Args)]
pub struct ShowRouteCommand {
    /// Hostname to show
    #[arg(long, short = 'd')]
    pub domain: String,

    /// Database URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// Output as JSON
    #[arg(long, default_value = "false")]
    pub json: bool,
}

/// Delete a route
#[derive(Args)]
pub struct DeleteRouteCommand {
    /// Hostname to delete
    #[arg(long, short = 'd')]
    pub domain: String,

    /// Database URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// Skip confirmation
    #[arg(long, short = 'y', default_value = "false")]
    pub yes: bool,
}

/// Split an `host:port` upstream string into its parts.
///
/// IPv6 literals must be bracketed (`[::1]:9300`). The port must be a valid
/// `u16` in `1..=65535`. Returns a descriptive error rather than panicking on
/// any malformed input.
pub fn parse_upstream(upstream: &str) -> anyhow::Result<(String, i32)> {
    let (host, port_str) = upstream.rsplit_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Upstream '{}' must be in host:port form (e.g. 127.0.0.1:9300)",
            upstream
        )
    })?;

    let host = host.trim();
    // Allow, but strip, brackets around IPv6 literals.
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    if host.is_empty() {
        return Err(anyhow::anyhow!("Upstream '{}' has an empty host", upstream));
    }

    let port: u16 = port_str
        .trim()
        .parse()
        .with_context(|| format!("Upstream '{}' has an invalid port '{}'", upstream, port_str))?;

    if port == 0 {
        return Err(anyhow::anyhow!(
            "Upstream '{}' port must be 1-65535",
            upstream
        ));
    }

    Ok((host.to_string(), port as i32))
}

impl RouteCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            match self.command {
                RouteSubcommand::Add(cmd) => execute_add(cmd).await,
                RouteSubcommand::List(cmd) => execute_list(cmd).await,
                RouteSubcommand::Show(cmd) => execute_show(cmd).await,
                RouteSubcommand::Delete(cmd) => execute_delete(cmd).await,
            }
        })
    }
}

async fn execute_add(cmd: AddRouteCommand) -> anyhow::Result<()> {
    let (host, port) = parse_upstream(&cmd.upstream)?;
    let route_type: RouteType = cmd.route_type.into();

    let db = establish_connection(&cmd.database_url).await?;
    let service = LbService::new(db);

    let route = service
        .create_route(cmd.domain.clone(), host, port, Some(route_type.clone()))
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    println!(
        "  {} Route created: {} {} {}:{} ({})",
        "✓".bright_green(),
        route.domain.bright_cyan(),
        "→".bright_blue(),
        route.host.bright_cyan(),
        route.port.to_string().bright_cyan(),
        route_type.to_string().bright_white()
    );

    if matches!(route_type, RouteType::Http) {
        println!(
            "  {} HTTP routes terminate TLS at the proxy. Ensure a certificate exists for {}:",
            "ℹ".bright_blue(),
            cmd.domain.bright_cyan()
        );
        println!(
            "      {}  (ACME)   or   {}  (custom/self-signed)",
            format!("temps domain add -d {} -c dns-01", cmd.domain).bright_white(),
            format!(
                "temps domain import -d {} -c cert.pem -k key.pem",
                cmd.domain
            )
            .bright_white()
        );
    }

    Ok(())
}

async fn execute_list(cmd: ListRoutesCommand) -> anyhow::Result<()> {
    let db = establish_connection(&cmd.database_url).await?;
    let service = LbService::new(db);
    let routes = service.list_routes().await?;

    if cmd.json {
        println!("{}", serde_json::to_string_pretty(&routes)?);
        return Ok(());
    }

    println!();
    println!(
        "  {:<40} {:<30} {:<6} {:<8}",
        "DOMAIN".bright_white().bold(),
        "UPSTREAM".bright_white().bold(),
        "TYPE".bright_white().bold(),
        "ENABLED".bright_white().bold()
    );
    println!("  {}", "─".repeat(86));

    if routes.is_empty() {
        println!("  {} No routes configured.", "ℹ".bright_blue());
        println!();
        return Ok(());
    }

    for route in &routes {
        let enabled = if route.enabled {
            "yes".bright_green()
        } else {
            "no".bright_red()
        };
        println!(
            "  {:<40} {:<30} {:<6} {:<8}",
            route.domain.bright_cyan(),
            format!("{}:{}", route.host, route.port),
            route.route_type.to_string(),
            enabled
        );
    }
    println!();
    Ok(())
}

async fn execute_show(cmd: ShowRouteCommand) -> anyhow::Result<()> {
    let db = establish_connection(&cmd.database_url).await?;
    let service = LbService::new(db);
    let route = service
        .get_route(&cmd.domain)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if cmd.json {
        println!("{}", serde_json::to_string_pretty(&route)?);
        return Ok(());
    }

    println!();
    println!(
        "  {} {}",
        "Domain:".bright_white(),
        route.domain.bright_cyan()
    );
    println!(
        "  {} {}:{}",
        "Upstream:".bright_white(),
        route.host.bright_cyan(),
        route.port.to_string().bright_cyan()
    );
    println!(
        "  {} {}",
        "Type:".bright_white(),
        route.route_type.to_string().bright_cyan()
    );
    println!(
        "  {} {}",
        "Enabled:".bright_white(),
        if route.enabled {
            "yes".bright_green()
        } else {
            "no".bright_red()
        }
    );
    println!();
    Ok(())
}

async fn execute_delete(cmd: DeleteRouteCommand) -> anyhow::Result<()> {
    if !cmd.yes {
        println!(
            "{} Are you sure you want to delete the route for '{}'? Use --yes to confirm.",
            "⚠".bright_yellow(),
            cmd.domain.bright_cyan()
        );
        return Ok(());
    }

    let db = establish_connection(&cmd.database_url).await?;
    let service = LbService::new(db);

    // Confirm the route exists so we can give an accurate message (delete_many
    // succeeds with zero rows affected otherwise).
    service
        .get_route(&cmd.domain)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    service.delete_route(&cmd.domain).await?;

    println!(
        "  {} Route for '{}' deleted.",
        "✓".bright_green(),
        cmd.domain.bright_cyan()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_upstream_ipv4() {
        let (host, port) = parse_upstream("127.0.0.1:9300").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 9300);
    }

    #[test]
    fn parse_upstream_hostname() {
        let (host, port) = parse_upstream("rustfs:9000").unwrap();
        assert_eq!(host, "rustfs");
        assert_eq!(port, 9000);
    }

    #[test]
    fn parse_upstream_ipv6_bracketed() {
        let (host, port) = parse_upstream("[::1]:9300").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 9300);
    }

    #[test]
    fn parse_upstream_rejects_missing_port() {
        assert!(parse_upstream("127.0.0.1").is_err());
    }

    #[test]
    fn parse_upstream_rejects_empty_host() {
        assert!(parse_upstream(":9300").is_err());
    }

    #[test]
    fn parse_upstream_rejects_zero_port() {
        assert!(parse_upstream("127.0.0.1:0").is_err());
    }

    #[test]
    fn parse_upstream_rejects_overflow_port() {
        assert!(parse_upstream("127.0.0.1:70000").is_err());
    }

    #[test]
    fn parse_upstream_rejects_non_numeric_port() {
        assert!(parse_upstream("127.0.0.1:abc").is_err());
    }

    #[test]
    fn route_type_arg_maps_to_entity() {
        assert_eq!(RouteType::from(RouteTypeArg::Http), RouteType::Http);
        assert_eq!(RouteType::from(RouteTypeArg::Tls), RouteType::Tls);
    }
}
