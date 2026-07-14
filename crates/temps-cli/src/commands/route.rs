//! Authenticated reverse-proxy route management commands.
//!
//! A custom route maps an external hostname to an operator-selected upstream.
//! Mutations go through the Temps API so normal authorization, validation, and
//! audit logging are never bypassed by the CLI.

use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;
use reqwest::Response;
use serde::{Deserialize, Serialize};

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
    /// Show details for one exact route
    Show(ShowRouteCommand),
    /// Delete one exact route
    #[command(alias = "rm")]
    Delete(DeleteRouteCommand),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RouteTypeArg {
    /// Terminate TLS at the proxy and route by HTTP Host.
    Http,
    /// Route by TLS SNI and let the upstream terminate TLS.
    Tls,
}

impl RouteTypeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Tls => "tls",
        }
    }
}

#[derive(Args)]
pub struct AddRouteCommand {
    /// Hostname to route (for example, sentry.example.com or *.example.com)
    #[arg(long, short = 'd')]
    pub domain: String,
    /// Upstream target as host:port; IPv6 literals must be bracketed
    #[arg(long, short = 'u')]
    pub upstream: String,
    /// How the proxy handles this route
    #[arg(long, short = 't', value_enum, default_value = "http")]
    pub route_type: RouteTypeArg,
    /// Deliberately override a domain already managed by Temps
    #[arg(long)]
    pub force_override: bool,
    /// Deliberately expose a private or loopback upstream through this route
    #[arg(long)]
    pub allow_private_upstream: bool,
    /// Temps API URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// Temps API token
    #[arg(long, env = "TEMPS_API_TOKEN", hide_env_values = true)]
    pub api_token: String,
}

#[derive(Args)]
pub struct ListRoutesCommand {
    /// Temps API URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// Temps API token
    #[arg(long, env = "TEMPS_API_TOKEN", hide_env_values = true)]
    pub api_token: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ShowRouteCommand {
    /// Exact route hostname to show
    #[arg(long, short = 'd')]
    pub domain: String,
    /// Temps API URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// Temps API token
    #[arg(long, env = "TEMPS_API_TOKEN", hide_env_values = true)]
    pub api_token: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct DeleteRouteCommand {
    /// Exact route hostname to delete
    #[arg(long, short = 'd')]
    pub domain: String,
    /// Temps API URL
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// Temps API token
    #[arg(long, env = "TEMPS_API_TOKEN", hide_env_values = true)]
    pub api_token: String,
    /// Skip confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Serialize)]
struct CreateRoutePayload {
    domain: String,
    host: String,
    port: i32,
    route_type: String,
    force_override: bool,
    allow_private_upstream: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct RouteResponse {
    id: i32,
    domain: String,
    host: String,
    port: i32,
    enabled: bool,
    route_type: String,
    force_override: bool,
    created_at: i64,
    updated_at: i64,
}

pub fn parse_upstream(upstream: &str) -> anyhow::Result<(String, i32)> {
    let upstream = upstream.trim();
    let (host, port_text) = if let Some(rest) = upstream.strip_prefix('[') {
        let closing = rest.find(']').ok_or_else(|| {
            anyhow::anyhow!("Upstream '{upstream}' has an unterminated IPv6 literal")
        })?;
        let (address, suffix) = rest.split_at(closing);
        let port_text = suffix.strip_prefix("]:").ok_or_else(|| {
            anyhow::anyhow!("Upstream '{upstream}' must place a port after the IPv6 literal")
        })?;
        address
            .parse::<std::net::Ipv6Addr>()
            .with_context(|| format!("Upstream '{upstream}' has an invalid IPv6 literal"))?;
        (format!("[{address}]"), port_text)
    } else {
        let (host, port_text) = upstream
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("Upstream '{upstream}' must be in host:port form"))?;
        if host.contains(':') {
            return Err(anyhow::anyhow!(
                "Upstream '{upstream}' contains an unbracketed IPv6 literal"
            ));
        }
        (host.trim().to_string(), port_text)
    };

    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(anyhow::anyhow!(
            "Upstream '{upstream}' has an empty or invalid host"
        ));
    }
    let port = port_text
        .trim()
        .parse::<u16>()
        .with_context(|| format!("Upstream '{upstream}' has an invalid port '{port_text}'"))?;
    if port == 0 {
        return Err(anyhow::anyhow!(
            "Upstream '{upstream}' port must be 1-65535"
        ));
    }
    Ok((host, i32::from(port)))
}

impl RouteCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        tokio::runtime::Runtime::new()?.block_on(async {
            match self.command {
                RouteSubcommand::Add(command) => execute_add(command).await,
                RouteSubcommand::List(command) => execute_list(command).await,
                RouteSubcommand::Show(command) => execute_show(command).await,
                RouteSubcommand::Delete(command) => execute_delete(command).await,
            }
        })
    }
}

fn route_url(api_url: &str, suffix: &str) -> String {
    format!("{}/api/lb/routes{suffix}", api_url.trim_end_matches('/'))
}

async fn checked(response: Response, operation: &str) -> anyhow::Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| format!("failed to read error response: {error}"));
    Err(anyhow::anyhow!(
        "Failed to {operation}: server returned {status}: {body}"
    ))
}

async fn execute_add(command: AddRouteCommand) -> anyhow::Result<()> {
    let domain = temps_proxy::service::lb_service::normalize_route_domain(&command.domain)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let (host, port) = parse_upstream(&command.upstream)?;
    if command.allow_private_upstream {
        println!(
            "  {} This route may expose a private service to public traffic.",
            "⚠".bright_yellow()
        );
    }
    let payload = CreateRoutePayload {
        domain,
        host,
        port,
        route_type: command.route_type.as_str().to_string(),
        force_override: command.force_override,
        allow_private_upstream: command.allow_private_upstream,
    };
    let response = reqwest::Client::new()
        .post(route_url(&command.api_url, ""))
        .bearer_auth(&command.api_token)
        .json(&payload)
        .send()
        .await
        .context("Failed to contact the Temps route API")?;
    let route: RouteResponse = checked(response, "create route").await?.json().await?;

    println!(
        "  {} Route created: {} {} {}:{} ({})",
        "✓".bright_green(),
        route.domain.bright_cyan(),
        "→".bright_blue(),
        route.host.bright_cyan(),
        route.port.to_string().bright_cyan(),
        route.route_type.bright_white()
    );
    if route.route_type == "http" {
        println!(
            "  {} HTTP routes need a certificate for {} to serve HTTPS.",
            "ℹ".bright_blue(),
            route.domain.bright_cyan()
        );
    }
    Ok(())
}

async fn execute_list(command: ListRoutesCommand) -> anyhow::Result<()> {
    let response = reqwest::Client::new()
        .get(route_url(&command.api_url, ""))
        .bearer_auth(&command.api_token)
        .send()
        .await
        .context("Failed to contact the Temps route API")?;
    let routes: Vec<RouteResponse> = checked(response, "list routes").await?.json().await?;
    if command.json {
        println!("{}", serde_json::to_string_pretty(&routes)?);
        return Ok(());
    }
    println!(
        "  {:<40} {:<30} {:<6} ENABLED",
        "DOMAIN", "UPSTREAM", "TYPE"
    );
    for route in routes {
        println!(
            "  {:<40} {:<30} {:<6} {}",
            route.domain,
            format!("{}:{}", route.host, route.port),
            route.route_type,
            if route.enabled { "yes" } else { "no" }
        );
    }
    Ok(())
}

async fn execute_show(command: ShowRouteCommand) -> anyhow::Result<()> {
    let domain = temps_proxy::service::lb_service::normalize_route_domain(&command.domain)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let suffix = format!("/{}", urlencoding::encode(&domain));
    let response = reqwest::Client::new()
        .get(route_url(&command.api_url, &suffix))
        .bearer_auth(&command.api_token)
        .send()
        .await
        .context("Failed to contact the Temps route API")?;
    let route: RouteResponse = checked(response, "get route").await?.json().await?;
    if command.json {
        println!("{}", serde_json::to_string_pretty(&route)?);
    } else {
        println!("  Domain:   {}", route.domain.bright_cyan());
        println!("  Upstream: {}:{}", route.host.bright_cyan(), route.port);
        println!("  Type:     {}", route.route_type.bright_cyan());
        println!("  Enabled:  {}", if route.enabled { "yes" } else { "no" });
    }
    Ok(())
}

async fn execute_delete(command: DeleteRouteCommand) -> anyhow::Result<()> {
    let domain = temps_proxy::service::lb_service::normalize_route_domain(&command.domain)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if !command.yes {
        println!(
            "{} Route '{}' was not deleted. Re-run with --yes to confirm.",
            "⚠".bright_yellow(),
            domain.bright_cyan()
        );
        return Ok(());
    }
    let suffix = format!("/{}", urlencoding::encode(&domain));
    let response = reqwest::Client::new()
        .delete(route_url(&command.api_url, &suffix))
        .bearer_auth(&command.api_token)
        .send()
        .await
        .context("Failed to contact the Temps route API")?;
    checked(response, "delete route").await?;
    println!("  {} Route for '{}' deleted.", "✓".bright_green(), domain);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_upstreams() {
        assert_eq!(
            parse_upstream("127.0.0.1:9300").unwrap(),
            ("127.0.0.1".into(), 9300)
        );
        assert_eq!(
            parse_upstream("rustfs:9000").unwrap(),
            ("rustfs".into(), 9000)
        );
        assert_eq!(
            parse_upstream("[::1]:9300").unwrap(),
            ("[::1]".into(), 9300)
        );
    }

    #[test]
    fn rejects_ambiguous_or_invalid_upstreams() {
        for upstream in [
            "127.0.0.1",
            ":9300",
            "127.0.0.1:0",
            "127.0.0.1:70000",
            "127.0.0.1:abc",
            "::1:9300",
            "[::1:9300",
            "[not-ip]:9300",
        ] {
            assert!(parse_upstream(upstream).is_err(), "accepted {upstream}");
        }
    }

    #[test]
    fn route_url_normalizes_one_trailing_slash() {
        assert_eq!(
            route_url("http://localhost:3000/", ""),
            "http://localhost:3000/api/lb/routes"
        );
    }
}
