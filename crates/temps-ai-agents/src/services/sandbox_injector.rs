// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared injector: writes skills + MCP server config + secret mounts into
//! a live sandbox. Used by both the agent executor and the workspace session
//! executor. The two systems keep separate sandbox registries (one per-run,
//! one per-session), so the injector accepts any impl of [`SandboxFs`] that
//! can read/write/exec against the target sandbox.
//!
//! Logging-free by design — callers log through their own channel (agents use
//! `run_service.append_log`, workspaces use `tracing`). Returns a summary of
//! what was written so the caller can surface it.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::AgentError;
use crate::services::definition_service::DefinitionService;
use crate::services::secret_service::{ResolvedSecret, SecretService, SecretType};

/// Coerce a user-configured file-secret `mount_path` into a path that is
/// guaranteed NOT to live inside `/workspace/`.
///
/// `/workspace` is bind-mounted from the cloned repo, so writing secret
/// material there leaks it into the PR branch (we saw `.mcp/gsc-creds.json`
/// show up in a real PR). Any attempt to mount under `/workspace/...` is
/// rewritten to `/home/temps/.temps/secrets/<...>` preserving the relative
/// subpath; absolute paths outside `/workspace` are kept as-is so admins
/// can still target `/run/secrets/...` tmpfs mounts explicitly.
pub(crate) fn sanitize_secret_mount_path(requested: &str, secret_name: &str) -> String {
    const SAFE_ROOT: &str = "/home/temps/.temps/secrets";
    let trimmed = requested.trim();

    if trimmed.is_empty() {
        return format!("{}/{}", SAFE_ROOT, secret_name);
    }

    if let Some(rest) = trimmed.strip_prefix("/workspace/") {
        return format!("{}/{}", SAFE_ROOT, rest);
    }
    if trimmed == "/workspace" {
        return SAFE_ROOT.to_string();
    }

    if trimmed.contains("..") {
        return format!("{}/{}", SAFE_ROOT, secret_name);
    }

    if trimmed.starts_with('/') {
        return trimmed.to_string();
    }

    format!("{}/{}", SAFE_ROOT, trimmed)
}

/// Convert a merged `mcpServers` map (Claude Code JSON format) into Codex CLI
/// TOML (`~/.codex/config.toml`) format.
///
/// Claude Code stores each MCP server as:
/// ```json
/// { "server-name": { "command": "npx", "args": ["-y", "pkg"], "env": { "K": "V" } } }
/// ```
///
/// Codex CLI expects TOML sections:
/// ```toml
/// [mcp_servers.server-name]
/// command = "npx"
/// args = ["-y", "pkg"]
///
/// [mcp_servers.server-name.env]
/// K = "V"
/// ```
pub fn mcp_to_codex_toml(servers: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut toml = String::new();
    for (name, config) in servers {
        toml.push_str(&format!("[mcp_servers.{}]\n", name));

        if let Some(cmd) = config.get("command").and_then(|v| v.as_str()) {
            toml.push_str(&format!("command = {:?}\n", cmd));
        }

        if let Some(args) = config.get("args").and_then(|v| v.as_array()) {
            let args_str: Vec<String> = args
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| format!("{:?}", s))
                .collect();
            toml.push_str(&format!("args = [{}]\n", args_str.join(", ")));
        }

        // URL-based MCP servers (streamable HTTP)
        if let Some(url) = config.get("url").and_then(|v| v.as_str()) {
            toml.push_str(&format!("url = {:?}\n", url));
        }

        if let Some(env) = config.get("env").and_then(|v| v.as_object()) {
            if !env.is_empty() {
                toml.push_str(&format!("\n[mcp_servers.{}.env]\n", name));
                for (k, v) in env {
                    if let Some(val) = v.as_str() {
                        toml.push_str(&format!("{} = {:?}\n", k, val));
                    }
                }
            }
        }

        toml.push('\n');
    }
    toml
}

/// Convert a merged `mcpServers` map (Claude Code JSON format) into OpenCode
/// JSON format (`~/.config/opencode/opencode.json` → `mcp` key).
///
/// OpenCode expects:
/// ```json
/// {
///   "mcp": {
///     "server-name": {
///       "type": "local",
///       "command": ["npx", "-y", "pkg"],
///       "environment": { "K": "V" },
///       "enabled": true
///     }
///   }
/// }
/// ```
pub fn mcp_to_opencode_json(
    servers: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    let mut mcp = serde_json::Map::new();
    for (name, config) in servers {
        let mut entry = serde_json::Map::new();

        // Check if this is a URL-based (remote) or command-based (local) server
        if let Some(url) = config.get("url").and_then(|v| v.as_str()) {
            entry.insert("type".to_string(), serde_json::json!("remote"));
            entry.insert("url".to_string(), serde_json::json!(url));

            // Map headers if present
            if let Some(headers) = config.get("headers").and_then(|v| v.as_object()) {
                entry.insert(
                    "headers".to_string(),
                    serde_json::Value::Object(headers.clone()),
                );
            }
        } else {
            entry.insert("type".to_string(), serde_json::json!("local"));

            // OpenCode uses a single "command" array: ["executable", "arg1", "arg2"]
            let mut cmd_array: Vec<serde_json::Value> = Vec::new();
            if let Some(cmd) = config.get("command").and_then(|v| v.as_str()) {
                cmd_array.push(serde_json::json!(cmd));
            }
            if let Some(args) = config.get("args").and_then(|v| v.as_array()) {
                for arg in args {
                    cmd_array.push(arg.clone());
                }
            }
            if !cmd_array.is_empty() {
                entry.insert("command".to_string(), serde_json::Value::Array(cmd_array));
            }

            // OpenCode uses "environment" instead of "env"
            if let Some(env) = config.get("env").and_then(|v| v.as_object()) {
                if !env.is_empty() {
                    entry.insert(
                        "environment".to_string(),
                        serde_json::Value::Object(env.clone()),
                    );
                }
            }
        }

        entry.insert("enabled".to_string(), serde_json::json!(true));
        mcp.insert(name.clone(), serde_json::Value::Object(entry));
    }
    serde_json::json!({ "mcp": mcp })
}

/// Write MCP server configs in the format expected by the given AI provider.
///
/// Always writes Claude Code format (`.claude/settings.json` + `~/.claude.json`)
/// since those are the canonical storage. Additionally writes provider-specific
/// configs so the active provider can discover MCP servers natively.
///
/// `provider`: one of `"claude_cli"`, `"codex_cli"`, `"opencode"`.
pub async fn write_mcp_configs(
    fs: &dyn SandboxFs,
    merged: &serde_json::Map<String, serde_json::Value>,
    secret_map: &HashMap<String, String>,
    provider: &str,
) -> Result<(), AgentError> {
    // ── Always write Claude Code format ──
    write_claude_mcp(fs, merged, secret_map).await?;

    // ── Provider-specific formats ──
    match provider {
        "codex_cli" => write_codex_mcp(fs, merged, secret_map).await?,
        "opencode" => write_opencode_mcp(fs, merged, secret_map).await?,
        _ => {} // claude_cli is already covered above
    }

    Ok(())
}

/// Write MCP config in Claude Code format.
///
/// Writes ONLY to `/home/temps/.claude.json` (the CLI's user-level config).
/// We deliberately do NOT write a project-level `.claude/settings.json`
/// anymore: `/workspace/.claude/settings.json` lives inside the bind-mounted
/// repo and would leak resolved secrets (MCP env blocks) into PR diffs.
/// The user-level config is equivalent for MCP discovery in Claude Code.
async fn write_claude_mcp(
    fs: &dyn SandboxFs,
    merged: &serde_json::Map<String, serde_json::Value>,
    secret_map: &HashMap<String, String>,
) -> Result<(), AgentError> {
    // Home-level ~/.claude.json (sandbox private volume, NOT the bind-mounted repo)
    let home_existing = fs
        .read_file("/home/temps/.claude.json")
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let mut home_config = home_existing;
    let mut home_mcp = home_config
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for (k, v) in merged {
        home_mcp.insert(k.clone(), v.clone());
    }
    home_config["mcpServers"] = serde_json::Value::Object(home_mcp);

    let mut home_str = serde_json::to_string_pretty(&home_config).unwrap_or_default();
    if !secret_map.is_empty() && home_str.contains("${TEMPS_SECRET:") {
        home_str = SecretService::resolve_placeholders(&home_str, secret_map);
    }
    fs.write_file("/home/temps/.claude.json", home_str.as_bytes(), 0o600)
        .await?;

    Ok(())
}

/// Write MCP config in Codex CLI format (~/.codex/config.toml)
async fn write_codex_mcp(
    fs: &dyn SandboxFs,
    merged: &serde_json::Map<String, serde_json::Value>,
    secret_map: &HashMap<String, String>,
) -> Result<(), AgentError> {
    let _ = fs
        .exec(vec![
            "mkdir".to_string(),
            "-p".to_string(),
            "/home/temps/.codex".to_string(),
        ])
        .await;

    // Read existing config.toml if present, so we don't clobber non-MCP settings.
    // Since TOML editing is complex and we only own the mcp_servers section,
    // we read the existing file and append/replace the mcp_servers block.
    let existing = fs
        .read_file("/home/temps/.codex/config.toml")
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();

    // Strip any existing [mcp_servers.*] sections from the file
    let non_mcp_lines = strip_toml_mcp_sections(&existing);

    let mcp_toml = mcp_to_codex_toml(merged);
    let mut config_str = format!("{}\n{}", non_mcp_lines.trim(), mcp_toml);

    if !secret_map.is_empty() && config_str.contains("${TEMPS_SECRET:") {
        config_str = SecretService::resolve_placeholders(&config_str, secret_map);
    }

    fs.write_file(
        "/home/temps/.codex/config.toml",
        config_str.as_bytes(),
        0o644,
    )
    .await?;

    Ok(())
}

/// Write MCP config in OpenCode format (~/.config/opencode/opencode.json)
async fn write_opencode_mcp(
    fs: &dyn SandboxFs,
    merged: &serde_json::Map<String, serde_json::Value>,
    secret_map: &HashMap<String, String>,
) -> Result<(), AgentError> {
    let _ = fs
        .exec(vec![
            "mkdir".to_string(),
            "-p".to_string(),
            "/home/temps/.config/opencode".to_string(),
        ])
        .await;

    // Read existing opencode.json if present
    let existing = fs
        .read_file("/home/temps/.config/opencode/opencode.json")
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let mut config = existing;
    let opencode_mcp = mcp_to_opencode_json(merged);

    // Merge the mcp key into existing config
    if let Some(mcp_obj) = opencode_mcp.get("mcp") {
        let mut existing_mcp = config
            .get("mcp")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if let Some(new_mcp) = mcp_obj.as_object() {
            for (k, v) in new_mcp {
                existing_mcp.insert(k.clone(), v.clone());
            }
        }
        config["mcp"] = serde_json::Value::Object(existing_mcp);
    }

    let mut config_str = serde_json::to_string_pretty(&config).unwrap_or_default();
    if !secret_map.is_empty() && config_str.contains("${TEMPS_SECRET:") {
        config_str = SecretService::resolve_placeholders(&config_str, secret_map);
    }

    fs.write_file(
        "/home/temps/.config/opencode/opencode.json",
        config_str.as_bytes(),
        0o644,
    )
    .await?;

    Ok(())
}

/// Strip all `[mcp_servers.*]` sections from an existing TOML string,
/// preserving everything else. This allows us to regenerate the MCP block
/// without clobbering other Codex settings.
fn strip_toml_mcp_sections(toml_content: &str) -> String {
    let mut result = String::new();
    let mut in_mcp_section = false;

    for line in toml_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[mcp_servers.") || trimmed == "[mcp_servers]" {
            in_mcp_section = true;
            continue;
        }
        // A new non-mcp section header ends the mcp section
        if in_mcp_section && trimmed.starts_with('[') && !trimmed.starts_with("[mcp_servers") {
            in_mcp_section = false;
        }
        if !in_mcp_section {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// Abstracts the per-sandbox filesystem operations the injector needs.
/// Implemented for both `SandboxRegistry` (agents) and
/// `WorkspaceSessionManager` (workspaces) via thin adapters.
#[async_trait]
pub trait SandboxFs: Send + Sync {
    async fn exec(&self, cmd: Vec<String>) -> Result<(), AgentError>;
    async fn write_file(&self, path: &str, contents: &[u8], mode: u32) -> Result<(), AgentError>;
    async fn read_file(&self, path: &str) -> Result<Vec<u8>, AgentError>;
    async fn write_directory(
        &self,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError>;
}

/// What the injector accomplished. Used by callers for logging / telemetry.
#[derive(Debug, Clone, Default)]
pub struct InjectSummary {
    pub mcp_count: usize,
    pub skill_count: usize,
    pub env_secret_count: usize,
    pub file_secret_count: usize,
    pub unresolved_mcp_slugs: Vec<String>,
    pub unresolved_skill_slugs: Vec<String>,
}

/// Inject skills, MCP servers, and secrets into a sandbox.
///
/// - `fs`: an adapter scoped to the target sandbox (see [`SandboxFs`]).
/// - `project_id`: used to resolve project-scoped skill/MCP definitions; global definitions are always included.
/// - `mcp_slugs` / `skill_slugs`: slug arrays to resolve and inject. Empty = skip that section.
/// - `secrets`: already-resolved secrets (e.g. from `SecretService::resolve_secrets()`).
///   Env-type secrets are written to `/home/temps/.temps/secrets.env`; file-type
///   secrets are written to `sanitize_secret_mount_path(mount_path)` (any
///   `/workspace/...` target is rewritten to `/home/temps/.temps/secrets/...`).
///   `${TEMPS_SECRET:<name>}` placeholders in the generated `.claude` JSONs are
///   resolved inline.
/// - `provider`: AI provider id (`"claude_cli"`, `"codex_cli"`, `"opencode"`). MCP configs
///   are written in the format the active provider expects (in addition to Claude Code format).
pub async fn inject(
    fs: &dyn SandboxFs,
    definition_service: Arc<DefinitionService>,
    project_id: i32,
    mcp_slugs: &[String],
    skill_slugs: &[String],
    secrets: &[ResolvedSecret],
    provider: &str,
) -> Result<InjectSummary, AgentError> {
    let mut summary = InjectSummary::default();

    let has_mcp = !mcp_slugs.is_empty();
    let has_skills = !skill_slugs.is_empty();
    let has_secrets = !secrets.is_empty();

    if !has_mcp && !has_skills && !has_secrets {
        return Ok(summary);
    }

    // ── Phase 1: secrets ────────────────────────────────────────────
    //
    // Nothing written here is allowed under `/workspace/`. That directory is
    // bind-mounted from the cloned repo, so any file written there lands in
    // the PR branch and leaks secret material. All secret files stay under
    // `/home/temps/` (the private named volume).
    if has_secrets {
        for s in secrets {
            match s.secret_type {
                SecretType::File => {
                    if let Some(mount_path) = &s.mount_path {
                        let safe_path = sanitize_secret_mount_path(mount_path, &s.name);
                        fs.write_file(&safe_path, s.value.as_bytes(), 0o600).await?;
                        summary.file_secret_count += 1;
                    }
                }
                SecretType::Env => {
                    summary.env_secret_count += 1;
                }
            }
        }

        let env_secrets: Vec<&ResolvedSecret> = secrets
            .iter()
            .filter(|s| s.secret_type == SecretType::Env)
            .collect();
        if !env_secrets.is_empty() {
            let mut env_content = String::new();
            for s in &env_secrets {
                let escaped = s.value.replace('\'', "'\\''");
                env_content.push_str(&format!("export {}='{}'\n", s.name, escaped));
            }
            fs.write_file(
                "/home/temps/.temps/secrets.env",
                env_content.as_bytes(),
                0o600,
            )
            .await?;
        }
    }

    if !has_mcp && !has_skills {
        return Ok(summary);
    }

    let secret_map: HashMap<String, String> = secrets
        .iter()
        .map(|s| (s.name.clone(), s.value.clone()))
        .collect();

    let _ = fs
        .exec(vec![
            "mkdir".to_string(),
            "-p".to_string(),
            "/home/temps/.claude".to_string(),
        ])
        .await;

    // ── Phase 2: MCP servers ────────────────────────────────────────
    if has_mcp {
        let mut merged = serde_json::Map::new();

        let mcp_defs = definition_service
            .get_all_available_mcps(project_id, mcp_slugs)
            .await?;
        for def in &mcp_defs {
            merged.insert(def.slug.clone(), def.config.clone());
        }
        summary.mcp_count = mcp_defs.len();

        let resolved: std::collections::HashSet<&str> =
            mcp_defs.iter().map(|d| d.slug.as_str()).collect();
        for slug in mcp_slugs {
            if !resolved.contains(slug.as_str()) {
                summary.unresolved_mcp_slugs.push(slug.clone());
            }
        }

        write_mcp_configs(fs, &merged, &secret_map, provider).await?;
    }

    // ── Phase 3: Skills ─────────────────────────────────────────────
    if has_skills {
        // Each AI CLI reads skills from a different directory. We always
        // write under the user's home (/home/temps/...) rather than into
        // /workspace, because /workspace is bind-mounted from the host repo
        // and any writes there show up as modified files in the user's git
        // working tree. Claude Code discovers user-level skills from
        // ~/.claude/skills the same way it discovers project-level ones.
        //
        // - claude:   `/home/temps/.claude/skills/<slug>/SKILL.md`
        // - codex:    `/home/temps/.codex/skills/<slug>/SKILL.md`
        // - opencode: no native skills system — falls back to claude's dir.
        let skills_base = match provider {
            "codex_cli" => "/home/temps/.codex/skills",
            _ => "/home/temps/.claude/skills",
        };
        let _ = fs
            .exec(vec![
                "mkdir".to_string(),
                "-p".to_string(),
                skills_base.to_string(),
            ])
            .await;

        let skill_defs = definition_service
            .get_all_available_skills(project_id, skill_slugs)
            .await?;

        for def in &skill_defs {
            if let Some(archive_data) = &def.archive {
                inject_skill_archive(fs, skills_base, &def.slug, archive_data).await?;
            } else {
                let dir_path = format!("{}/{}", skills_base, def.slug);
                let _ = fs
                    .exec(vec![
                        "mkdir".to_string(),
                        "-p".to_string(),
                        dir_path.clone(),
                    ])
                    .await;
                let path = format!("{}/SKILL.md", dir_path);
                fs.write_file(&path, def.content.as_bytes(), 0o644).await?;
            }
            summary.skill_count += 1;
        }

        let resolved: std::collections::HashSet<&str> =
            skill_defs.iter().map(|d| d.slug.as_str()).collect();
        for slug in skill_slugs {
            if !resolved.contains(slug.as_str()) {
                summary.unresolved_skill_slugs.push(slug.clone());
            }
        }
    }

    Ok(summary)
}

async fn inject_skill_archive(
    fs: &dyn SandboxFs,
    skills_base: &str,
    slug: &str,
    archive_data: &[u8],
) -> Result<(), AgentError> {
    const MAX_DECOMPRESSED_BYTES: u64 = 500 * 1024 * 1024;
    let tmp_dir = tempfile::tempdir().map_err(|e| AgentError::SandboxExecFailed {
        run_id: 0,
        sandbox_id: String::new(),
        reason: format!("Failed to create temp dir for skill '{}': {}", slug, e),
    })?;
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive_data));
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);

    let entries = archive
        .entries()
        .map_err(|e| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: String::new(),
            reason: format!("Failed to read archive entries for skill '{}': {}", slug, e),
        })?;

    let mut total_bytes: u64 = 0;
    for entry in entries {
        let mut entry = entry.map_err(|e| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: String::new(),
            reason: format!("Invalid archive entry for skill '{}': {}", slug, e),
        })?;
        let entry_size = entry.header().size().unwrap_or(0);
        total_bytes = total_bytes.saturating_add(entry_size);
        if total_bytes > MAX_DECOMPRESSED_BYTES {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: String::new(),
                reason: format!(
                    "Archive for skill '{}' exceeds 500MB decompressed limit",
                    slug
                ),
            });
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: String::new(),
                reason: format!(
                    "Archive for skill '{}' contains disallowed link entry",
                    slug
                ),
            });
        }
        let unpacked =
            entry
                .unpack_in(tmp_dir.path())
                .map_err(|e| AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: String::new(),
                    reason: format!(
                        "Failed to extract archive entry for skill '{}': {}",
                        slug, e
                    ),
                })?;
        if !unpacked {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: String::new(),
                reason: format!("Archive for skill '{}' contains path traversal", slug),
            });
        }
    }

    let target_path = format!("{}/{}", skills_base, slug);
    fs.write_directory(tmp_dir.path(), &target_path).await?;
    Ok(())
}

/// Convenience: extract slug arrays out of a JSONB value (array of strings).
pub fn parse_slug_array(val: Option<&serde_json::Value>) -> Vec<String> {
    val.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Adapter that binds a `SandboxRegistry` to a specific `run_id` so it can be
/// passed to `inject()` as a `&dyn SandboxFs`. Used by the agents executor.
pub struct RegistrySandboxFs {
    pub registry: Arc<crate::services::sandbox_registry::SandboxRegistry>,
    pub run_id: i32,
}

#[async_trait]
impl SandboxFs for RegistrySandboxFs {
    async fn exec(&self, cmd: Vec<String>) -> Result<(), AgentError> {
        self.registry
            .exec(self.run_id, cmd, HashMap::new(), None)
            .await
            .map(|_| ())
    }

    async fn write_file(&self, path: &str, contents: &[u8], mode: u32) -> Result<(), AgentError> {
        self.registry
            .write_file(self.run_id, path, contents, mode)
            .await
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, AgentError> {
        self.registry.read_file(self.run_id, path).await
    }

    async fn write_directory(
        &self,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        self.registry
            .write_directory(self.run_id, local_dir, target_path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_to_codex_toml_stdio_server() {
        let mut servers = serde_json::Map::new();
        servers.insert(
            "context7".to_string(),
            serde_json::json!({
                "command": "npx",
                "args": ["-y", "@upstash/context7-mcp"]
            }),
        );

        let toml = mcp_to_codex_toml(&servers);
        assert!(toml.contains("[mcp_servers.context7]"));
        assert!(toml.contains(r#"command = "npx""#));
        assert!(toml.contains(r#"args = ["-y", "@upstash/context7-mcp"]"#));
    }

    #[test]
    fn mcp_to_codex_toml_with_env() {
        let mut servers = serde_json::Map::new();
        servers.insert(
            "myserver".to_string(),
            serde_json::json!({
                "command": "node",
                "args": ["server.js"],
                "env": { "API_KEY": "secret123", "DEBUG": "true" }
            }),
        );

        let toml = mcp_to_codex_toml(&servers);
        assert!(toml.contains("[mcp_servers.myserver.env]"));
        assert!(toml.contains(r#"API_KEY = "secret123""#));
        assert!(toml.contains(r#"DEBUG = "true""#));
    }

    #[test]
    fn mcp_to_codex_toml_url_server() {
        let mut servers = serde_json::Map::new();
        servers.insert(
            "remote".to_string(),
            serde_json::json!({
                "url": "https://api.example.com/mcp"
            }),
        );

        let toml = mcp_to_codex_toml(&servers);
        assert!(toml.contains("[mcp_servers.remote]"));
        assert!(toml.contains(r#"url = "https://api.example.com/mcp""#));
    }

    #[test]
    fn mcp_to_opencode_json_local_server() {
        let mut servers = serde_json::Map::new();
        servers.insert(
            "context7".to_string(),
            serde_json::json!({
                "command": "npx",
                "args": ["-y", "@upstash/context7-mcp"]
            }),
        );

        let result = mcp_to_opencode_json(&servers);
        let mcp = result.get("mcp").unwrap();
        let server = mcp.get("context7").unwrap();

        assert_eq!(server.get("type").unwrap(), "local");
        assert_eq!(server.get("enabled").unwrap(), true);

        let cmd = server.get("command").unwrap().as_array().unwrap();
        assert_eq!(cmd[0], "npx");
        assert_eq!(cmd[1], "-y");
        assert_eq!(cmd[2], "@upstash/context7-mcp");
    }

    #[test]
    fn mcp_to_opencode_json_with_env() {
        let mut servers = serde_json::Map::new();
        servers.insert(
            "myserver".to_string(),
            serde_json::json!({
                "command": "node",
                "args": ["server.js"],
                "env": { "TOKEN": "abc" }
            }),
        );

        let result = mcp_to_opencode_json(&servers);
        let server = result["mcp"]["myserver"].as_object().unwrap();

        assert_eq!(server["type"], "local");
        let env = server["environment"].as_object().unwrap();
        assert_eq!(env["TOKEN"], "abc");
    }

    #[test]
    fn mcp_to_opencode_json_remote_server() {
        let mut servers = serde_json::Map::new();
        servers.insert(
            "remote".to_string(),
            serde_json::json!({
                "url": "https://api.example.com/mcp",
                "headers": { "Authorization": "Bearer tok" }
            }),
        );

        let result = mcp_to_opencode_json(&servers);
        let server = result["mcp"]["remote"].as_object().unwrap();

        assert_eq!(server["type"], "remote");
        assert_eq!(server["url"], "https://api.example.com/mcp");
        assert_eq!(server["headers"]["Authorization"], "Bearer tok");
    }

    #[test]
    fn strip_toml_mcp_sections_preserves_other_config() {
        let toml = r#"
model = "gpt-5-codex"
approval_policy = "auto-edit"

[mcp_servers.old_server]
command = "old"
args = ["--old"]

[mcp_servers.old_server.env]
KEY = "val"

[other_section]
key = "value"
"#;

        let result = strip_toml_mcp_sections(toml);
        assert!(!result.contains("mcp_servers"));
        assert!(!result.contains("old_server"));
        assert!(result.contains("model ="));
        assert!(result.contains("[other_section]"));
        assert!(result.contains(r#"key = "value""#));
    }

    #[test]
    fn mcp_to_codex_toml_empty_servers() {
        let servers = serde_json::Map::new();
        let toml = mcp_to_codex_toml(&servers);
        assert!(toml.is_empty());
    }

    #[test]
    fn mcp_to_opencode_json_empty_servers() {
        let servers = serde_json::Map::new();
        let result = mcp_to_opencode_json(&servers);
        let mcp = result["mcp"].as_object().unwrap();
        assert!(mcp.is_empty());
    }
}
