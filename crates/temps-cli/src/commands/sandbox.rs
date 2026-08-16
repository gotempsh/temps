//! `temps sandbox` subcommand — manage standalone sandboxes via the
//! `/v1/sandbox/*` HTTP API.
//!
//! This is a thin HTTP client — every command hits the running control
//! plane. It exists so operators can drive the Vercel-compatible sandbox
//! surface from a shell without needing curl + jq incantations, and so
//! the contract gets exercised end-to-end in release smoke tests.
//!
//! Shape contract: DTOs here mirror the server-side DTOs in
//! `temps-sandbox::handlers::sandboxes`. The server rejects unknown
//! fields (`deny_unknown_fields`), so keep these structs in sync.

use clap::{Args, Subcommand};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Standalone sandbox management (Vercel-compatible `/v1/sandbox/*`).
#[derive(Args)]
pub struct SandboxCommand {
    #[command(subcommand)]
    pub command: SandboxSubcommand,
}

#[derive(Subcommand)]
pub enum SandboxSubcommand {
    /// Create a new sandbox
    Create(SandboxCreateCommand),
    /// List sandboxes for the authenticated user
    #[command(alias = "ls")]
    List(SandboxListCommand),
    /// Show details for a single sandbox
    Show(SandboxShowCommand),
    /// Run a command inside a sandbox and wait for it to finish
    Exec(SandboxExecCommand),
    /// Stop and destroy a sandbox
    #[command(alias = "rm")]
    Stop(SandboxStopCommand),
}

// ── Common auth/url args ────────────────────────────────────────────────────

/// Shared flags every subcommand takes. Kept as a flattened struct so
/// each subcommand gets identical help text + env var handling.
#[derive(Args, Clone)]
pub struct ApiArgs {
    /// API base URL (e.g., "http://localhost:3001").
    #[arg(long, env = "TEMPS_API_URL")]
    pub api_url: String,
    /// API authentication token.
    #[arg(long, env = "TEMPS_API_TOKEN")]
    pub api_token: String,
}

// ── Subcommand arg structs ──────────────────────────────────────────────────

#[derive(Args)]
pub struct SandboxCreateCommand {
    #[command(flatten)]
    pub api: ApiArgs,
    /// Docker image override. Defaults to the platform default when absent.
    #[arg(long)]
    pub image: Option<String>,
    /// Human-readable name. Defaults to the server-assigned id.
    #[arg(long)]
    pub name: Option<String>,
    /// Idle timeout in seconds. Clamped server-side to [60, 86400].
    #[arg(long)]
    pub timeout_secs: Option<u64>,
    /// Extra env vars as KEY=VALUE. Repeatable.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Isolation backend: "docker" (default) or "firecracker" (ADR-029;
    /// requires a host provisioned via `temps firecracker setup`).
    #[arg(long)]
    pub backend: Option<String>,
    /// Output the server response as JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct SandboxListCommand {
    #[command(flatten)]
    pub api: ApiArgs,
    /// Page number (1-indexed).
    #[arg(long, default_value = "1")]
    pub page: u64,
    /// Items per page (max 100).
    #[arg(long, default_value = "20")]
    pub page_size: u64,
    /// Output the server response as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct SandboxShowCommand {
    #[command(flatten)]
    pub api: ApiArgs,
    /// Public sandbox id (e.g., sbx_abc123).
    pub id: String,
    /// Output the server response as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct SandboxExecCommand {
    #[command(flatten)]
    pub api: ApiArgs,
    /// Public sandbox id.
    pub id: String,
    /// Command + args. Use `--` before the command to pass flags through.
    #[arg(required = true, num_args = 1..)]
    pub cmd: Vec<String>,
    /// Working directory inside the sandbox.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Output the server response as JSON rather than streaming
    /// stdout/stderr to the terminal.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct SandboxStopCommand {
    #[command(flatten)]
    pub api: ApiArgs,
    /// Public sandbox id.
    pub id: String,
    /// Suppress the success message.
    #[arg(long)]
    pub quiet: bool,
}

// ── Wire DTOs (mirror server-side DTOs) ─────────────────────────────────────

#[derive(Debug, Serialize)]
struct CreateSandboxBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExecBody {
    cmd: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    env: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct SandboxEnvelope {
    sandbox: SandboxResponse,
}

/// `SandboxInner` on the server (`@vercel/sandbox`-shaped): `cwd` for the
/// work dir, epoch-ms `createdAt`, idle `timeout` in ms.
#[derive(Debug, Deserialize)]
struct SandboxResponse {
    id: String,
    name: String,
    status: String,
    #[allow(dead_code)]
    image: Option<String>,
    cwd: String,
    #[serde(rename = "createdAt")]
    created_at_ms: i64,
    timeout: i64,
}

impl SandboxResponse {
    fn created_at(&self) -> String {
        chrono::DateTime::from_timestamp_millis(self.created_at_ms)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default()
    }

    fn expires_at(&self) -> String {
        chrono::DateTime::from_timestamp_millis(self.created_at_ms + self.timeout)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
struct ListSandboxesResponse {
    sandboxes: Vec<SandboxResponse>,
    pagination: Pagination,
}

#[derive(Debug, Deserialize)]
struct Pagination {
    count: u64,
}

#[derive(Debug, Deserialize)]
struct ExecResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Deserialize)]
struct ProblemDetail {
    title: Option<String>,
    detail: Option<String>,
}

// ── HTTP helpers ────────────────────────────────────────────────────────────

/// Build the absolute URL for a `/v1/sandbox/*` path, tolerating trailing
/// slashes in `TEMPS_API_URL`.
fn sandbox_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    // Plural is the canonical route (`/v1/sandboxes`, pinned by
    // tests/vercel_compat.rs) — the singular form predates the rename and
    // matches nothing on current servers.
    format!("{}/v1/sandboxes{}", base, path)
}

fn make_client() -> reqwest::Client {
    // Strict TLS — CLI talks to the control plane over the public
    // internet. Skipping verification here would let a MitM steal the
    // user's session token. The server-side opt-in (AppSettings.insecure_tls)
    // does NOT apply to CLI binaries.
    reqwest::Client::builder()
        .build()
        .expect("Failed to build HTTP client")
}

/// Convert a non-2xx response into a readable `anyhow::Error`. Prefers the
/// RFC 7807 `title`/`detail` fields when the server returned ProblemDetails.
async fn api_error(response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if let Ok(problem) = serde_json::from_str::<ProblemDetail>(&body) {
        anyhow::anyhow!(
            "API error ({}): {} - {}",
            status,
            problem.title.unwrap_or_default(),
            problem.detail.unwrap_or_default()
        )
    } else {
        anyhow::anyhow!("API error ({}): {}", status, body)
    }
}

/// Parse `KEY=VALUE` pairs from `--env`. Rejects entries missing `=` so a
/// typo doesn't silently produce an empty value.
fn parse_env(pairs: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut out = HashMap::with_capacity(pairs.len());
    for p in pairs {
        let Some((k, v)) = p.split_once('=') else {
            anyhow::bail!("--env '{}' is missing '=' (expected KEY=VALUE)", p);
        };
        if k.is_empty() {
            anyhow::bail!("--env '{}' has empty key", p);
        }
        out.insert(k.to_string(), v.to_string());
    }
    Ok(out)
}

// ── Command dispatch ────────────────────────────────────────────────────────

impl SandboxCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            match self.command {
                SandboxSubcommand::Create(c) => execute_create(c).await,
                SandboxSubcommand::List(c) => execute_list(c).await,
                SandboxSubcommand::Show(c) => execute_show(c).await,
                SandboxSubcommand::Exec(c) => execute_exec(c).await,
                SandboxSubcommand::Stop(c) => execute_stop(c).await,
            }
        })
    }
}

async fn execute_create(cmd: SandboxCreateCommand) -> anyhow::Result<()> {
    let env = parse_env(&cmd.env)?;
    let body = CreateSandboxBody {
        image: cmd.image,
        name: cmd.name,
        timeout_secs: cmd.timeout_secs,
        env,
        backend: cmd.backend,
    };

    let client = make_client();
    let url = sandbox_url(&cmd.api.api_url, "");
    let response = client
        .post(&url)
        .bearer_auth(&cmd.api.api_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(api_error(response).await);
    }

    let sandbox = response.json::<SandboxEnvelope>().await?.sandbox;

    if cmd.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": sandbox.id,
                "name": sandbox.name,
                "status": sandbox.status,
                "created_at": sandbox.created_at(),
                "expires_at": sandbox.expires_at(),
            }))?
        );
        return Ok(());
    }

    println!();
    println!(
        "{} {}",
        "Sandbox created:".bright_green().bold(),
        sandbox.id.bright_cyan().bold()
    );
    println!("  {} {}", "Name:".bright_white().bold(), sandbox.name);
    println!("  {} {}", "Status:".bright_white().bold(), sandbox.status);
    println!(
        "  {} {}",
        "Expires:".bright_white().bold(),
        sandbox.expires_at()
    );
    println!();
    Ok(())
}

async fn execute_list(cmd: SandboxListCommand) -> anyhow::Result<()> {
    let client = make_client();
    let url = sandbox_url(&cmd.api.api_url, "");

    let response = client
        .get(&url)
        .bearer_auth(&cmd.api.api_token)
        .query(&[("page", cmd.page), ("page_size", cmd.page_size)])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(api_error(response).await);
    }

    let data: ListSandboxesResponse = response.json().await?;

    if cmd.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "total": data.pagination.count,
                "page": cmd.page,
                "page_size": cmd.page_size,
                "items": data.sandboxes.iter().map(|s| serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "status": s.status,
                    "created_at": s.created_at(),
                    "expires_at": s.expires_at(),
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }

    if data.sandboxes.is_empty() {
        println!("No sandboxes.");
        println!(
            "Run {} to create one.",
            "temps sandbox create".bright_cyan()
        );
        return Ok(());
    }

    println!();
    println!(
        "  {:<18} {:<20} {:<10} {:<24}",
        "ID".bright_white().bold(),
        "NAME".bright_white().bold(),
        "STATUS".bright_white().bold(),
        "EXPIRES".bright_white().bold(),
    );
    println!("  {}", "─".repeat(74).bright_black());
    for s in &data.sandboxes {
        let status_colored = match s.status.as_str() {
            "running" => s.status.bright_green(),
            "stopped" => s.status.bright_red(),
            _ => s.status.bright_yellow(),
        };
        println!(
            "  {:<18} {:<20} {:<10} {:<24}",
            s.id.bright_cyan(),
            truncate(&s.name, 20),
            status_colored,
            s.expires_at(),
        );
    }
    println!();
    println!(
        "  {} page {} of {} ({} total)",
        "→".bright_black(),
        cmd.page,
        (data.pagination.count.div_ceil(cmd.page_size.max(1))).max(1),
        data.pagination.count,
    );
    println!();
    Ok(())
}

async fn execute_show(cmd: SandboxShowCommand) -> anyhow::Result<()> {
    let client = make_client();
    let url = sandbox_url(&cmd.api.api_url, &format!("/{}", cmd.id));

    let response = client
        .get(&url)
        .bearer_auth(&cmd.api.api_token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(api_error(response).await);
    }

    let sandbox = response.json::<SandboxEnvelope>().await?.sandbox;

    if cmd.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": sandbox.id,
                "name": sandbox.name,
                "status": sandbox.status,
                "image": sandbox.image,
                "work_dir": sandbox.cwd,
                "created_at": sandbox.created_at(),
                "expires_at": sandbox.expires_at(),
            }))?
        );
        return Ok(());
    }

    println!();
    println!(
        "  {} {}",
        "ID:".bright_white().bold(),
        sandbox.id.bright_cyan()
    );
    println!("  {} {}", "Name:".bright_white().bold(), sandbox.name);
    println!("  {} {}", "Status:".bright_white().bold(), sandbox.status);
    println!(
        "  {} {}",
        "Image:".bright_white().bold(),
        sandbox.image.as_deref().unwrap_or("<default>")
    );
    println!("  {} {}", "Work dir:".bright_white().bold(), sandbox.cwd);
    println!(
        "  {} {}",
        "Created:".bright_white().bold(),
        sandbox.created_at()
    );
    println!(
        "  {} {}",
        "Expires:".bright_white().bold(),
        sandbox.expires_at()
    );
    println!();
    Ok(())
}

async fn execute_exec(cmd: SandboxExecCommand) -> anyhow::Result<()> {
    if cmd.cmd.is_empty() {
        anyhow::bail!("Command is empty — provide at least one argument");
    }

    let body = ExecBody {
        cmd: cmd.cmd.clone(),
        cwd: cmd.cwd,
        env: HashMap::new(),
    };

    let client = make_client();
    let url = sandbox_url(&cmd.api.api_url, &format!("/{}/exec", cmd.id));

    let response = client
        .post(&url)
        .bearer_auth(&cmd.api.api_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(api_error(response).await);
    }

    let result: ExecResponse = response.json().await?;

    if cmd.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "exit_code": result.exit_code,
                "stdout": result.stdout,
                "stderr": result.stderr,
            }))?
        );
    } else {
        if !result.stdout.is_empty() {
            print!("{}", result.stdout);
            if !result.stdout.ends_with('\n') {
                println!();
            }
        }
        if !result.stderr.is_empty() {
            eprint!("{}", result.stderr);
            if !result.stderr.ends_with('\n') {
                eprintln!();
            }
        }
    }

    // Propagate the remote exit code. A non-zero exit_code is NOT an error
    // on the HTTP layer (200 OK), but shells expect the CLI to fail so
    // `set -e` and `&&` chains work as operators expect.
    if result.exit_code != 0 {
        std::process::exit(result.exit_code);
    }
    Ok(())
}

async fn execute_stop(cmd: SandboxStopCommand) -> anyhow::Result<()> {
    let client = make_client();
    let url = sandbox_url(&cmd.api.api_url, &format!("/{}/stop", cmd.id));

    let response = client
        .post(&url)
        .bearer_auth(&cmd.api.api_token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to API: {}", e))?;

    if !response.status().is_success() {
        return Err(api_error(response).await);
    }

    if !cmd.quiet {
        println!(
            "{} {}",
            "Sandbox stopped:".bright_green().bold(),
            cmd.id.bright_cyan()
        );
    }
    Ok(())
}

// ── Utilities ───────────────────────────────────────────────────────────────

/// Truncate a string to `max` chars for display in a table cell, adding
/// an ellipsis if it was shortened.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_url_trims_trailing_slash() {
        assert_eq!(
            sandbox_url("http://localhost:3001/", ""),
            "http://localhost:3001/v1/sandboxes"
        );
        assert_eq!(
            sandbox_url("http://localhost:3001", "/sbx_a"),
            "http://localhost:3001/v1/sandboxes/sbx_a"
        );
    }

    #[test]
    fn sandbox_url_passes_subpath_untouched() {
        assert_eq!(
            sandbox_url("https://api.example/", "/sbx_a/exec"),
            "https://api.example/v1/sandboxes/sbx_a/exec"
        );
    }

    #[test]
    fn parse_env_parses_simple_pairs() {
        let got = parse_env(&["A=1".into(), "B=two".into()]).expect("parses");
        assert_eq!(got.get("A").map(String::as_str), Some("1"));
        assert_eq!(got.get("B").map(String::as_str), Some("two"));
    }

    #[test]
    fn parse_env_preserves_equals_in_value() {
        // `split_once` on '=' keeps extra '=' in the value — a value like
        // `token=abc=def` must survive intact.
        let got = parse_env(&["TOKEN=abc=def".into()]).expect("parses");
        assert_eq!(got.get("TOKEN").map(String::as_str), Some("abc=def"));
    }

    #[test]
    fn parse_env_rejects_missing_equals() {
        let err = parse_env(&["BROKEN".into()]).expect_err("should error");
        assert!(err.to_string().contains("missing '='"), "{}", err);
    }

    #[test]
    fn parse_env_rejects_empty_key() {
        let err = parse_env(&["=value".into()]).expect_err("should error");
        assert!(err.to_string().contains("empty key"), "{}", err);
    }

    #[test]
    fn truncate_returns_input_when_under_limit() {
        assert_eq!(truncate("abc", 5), "abc");
    }

    #[test]
    fn truncate_shortens_with_ellipsis() {
        // 4-char input truncated to max=3 -> 2 chars + ellipsis
        assert_eq!(truncate("abcd", 3), "ab…");
    }

    #[test]
    fn create_body_skips_empty_fields_in_json() {
        let body = CreateSandboxBody {
            image: None,
            name: None,
            timeout_secs: None,
            env: HashMap::new(),
            backend: None,
        };
        let j = serde_json::to_string(&body).expect("serializes");
        // All fields have `skip_serializing_if`, so nothing is emitted.
        // Server-side `CreateSandboxBody` is `Default`, so `{}` is valid.
        assert_eq!(j, "{}");
    }

    #[test]
    fn create_body_preserves_populated_fields() {
        let mut env = HashMap::new();
        env.insert("X".into(), "1".into());
        let body = CreateSandboxBody {
            image: Some("node:20".into()),
            name: Some("demo".into()),
            timeout_secs: Some(300),
            env,
            backend: None,
        };
        let j = serde_json::to_value(&body).expect("serializes");
        assert_eq!(j["image"], "node:20");
        assert_eq!(j["name"], "demo");
        assert_eq!(j["timeout_secs"], 300);
        assert_eq!(j["env"]["X"], "1");
    }

    #[test]
    fn exec_body_skips_empty_cwd_and_env() {
        let body = ExecBody {
            cmd: vec!["ls".into(), "/tmp".into()],
            cwd: None,
            env: HashMap::new(),
        };
        let j = serde_json::to_value(&body).expect("serializes");
        assert_eq!(j["cmd"][0], "ls");
        // `cwd` absent (skip_serializing_if) — server-side ExecBody accepts
        // missing cwd via `#[serde(default)]`.
        assert!(j.get("cwd").is_none());
        assert!(j.get("env").is_none());
    }
}
