// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A sandbox linked to one assistant chat, and the tools that run inside it.
//!
//! Some steps need a shell: cloning and committing a repository, unpacking an
//! archive, starting a server to see whether it comes up. The assistant cannot
//! reach the generic sandbox API for that, because those operations accept any
//! sandbox id, including a user's persistent workspace. Instead the user links
//! exactly one sandbox to the chat, and the tools in this module act on that
//! sandbox only.
//!
//! The link lives in `ai_conversations.metadata` under [`SANDBOX_LINK_KEY`].
//! It is re-read and re-checked on every tool call, so unlinking, a destroyed
//! sandbox or a revoked permission takes effect on the next call, and the
//! model can never name a sandbox of its own choosing. Nothing here holds
//! platform or Git credentials; the sandbox is an ordinary user sandbox.
//!
//! The user can also link the sandbox to services, one at a time or all of a
//! project's (ADR-051). The links belong to the sandbox (`sandbox_services`),
//! the way a project's belong to the project. Each command then runs with
//! credentials that reach a namespace of the sandbox's own on each service,
//! resolved on the server for that command: they are never written to a file
//! in the sandbox, never shown to the model, and removed from the output if a
//! command prints them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, Statement,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use temps_ai::ChatTool;
use temps_auth::context::AuthContext;
use temps_auth::permissions::Permission;
use temps_core::{AuditContext, AuditLogger};
use temps_entities::ai_conversations;
use temps_sandbox::error::SandboxError;
use temps_sandbox::services::{CreateSandboxRequest, ExecOptions, LinkedService};
use temps_sandbox::SandboxService;
use thiserror::Error;
use tracing::{debug, info, warn};
use utoipa::ToSchema;

use crate::providers::assistant::ASSISTANT_CONTEXT_TYPE;

/// Key under `ai_conversations.metadata` that holds the link.
pub const SANDBOX_LINK_KEY: &str = "sandbox_link";
/// Key holding the last port the assistant confirmed serving a page in the
/// linked sandbox. The chat's Preview tab shows it.
pub const SANDBOX_PREVIEW_KEY: &str = "sandbox_preview_port";
/// Key recording that the user has already been offered a sandbox in this chat.
pub const SANDBOX_OFFERED_KEY: &str = "sandbox_offered_at";
/// How long the credentials resolved for a chat's commands are reused, so a
/// run of commands does not provision the database role again for each one.
const CREDENTIAL_CACHE_SECS: u64 = 300;

pub const SANDBOX_EXEC_TOOL: &str = "sandbox_exec";
pub const SANDBOX_READ_TOOL: &str = "sandbox_read_file";
pub const SANDBOX_WRITE_TOOL: &str = "sandbox_write_file";
pub const SANDBOX_PREVIEW_TOOL: &str = "sandbox_preview";
pub const SANDBOX_OFFER_TOOL: &str = "temps_offer_sandbox";
pub const SANDBOX_CREATE_TOOL: &str = "temps_create_sandbox";
pub use crate::sandbox_deploy::SANDBOX_DEPLOY_TOOL;

/// Most bytes of command output or file content handed back to the model.
const MAX_TOOL_OUTPUT_BYTES: usize = 128 * 1024;
/// Most lines one `sandbox_read_file` call returns unless the model asks for
/// fewer or more, so a large file is read in pieces instead of filling the
/// context in one go.
const DEFAULT_READ_LINES: usize = 500;
/// Most lines the model may ask for in one read.
const MAX_READ_LINES: usize = 2000;
/// Most bytes of file content one read returns, whatever the line count.
const MAX_READ_BYTES: usize = 48 * 1024;
/// Most bytes of a file `sandbox_read_file` ever takes out of the sandbox
/// (about twenty windows of [`MAX_READ_BYTES`]). A file up to this size is
/// read whole and paged with `offset`; a larger one is never buffered whole:
/// only its first this-many bytes are read, its windows come from that part,
/// and the result says so and points at `sandbox_exec` for the rest.
const MAX_READ_FILE_BYTES: usize = 1024 * 1024;
/// How long reading the start of an oversized file may take.
const READ_HEAD_TIMEOUT_SECS: u64 = 30;
/// Largest file the model may write in one call.
const MAX_WRITE_BYTES: usize = 1024 * 1024;
const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 120;
const MAX_EXEC_TIMEOUT_SECS: u64 = 600;
/// Idle lifetime of a sandbox created from a chat.
const CREATED_SANDBOX_TIMEOUT_SECS: u64 = 3600;
/// How long a request waits for a new sandbox to come up before it reports
/// that it is still starting. The sandbox keeps starting and shows up in the
/// user's sandbox list; only this request stops waiting.
const CREATE_WAIT_SECS: u64 = 90;

/// How long a tool call waits for a sleeping sandbox to wake before it tells
/// the model and the user it did not.
const RESUME_WAIT_SECS: u64 = 120;
/// Prefix of the persistent per-user application workspace sandboxes.
const APPLICATION_WORKSPACE_PREFIX: &str = "ai-application:";
/// Context type of chats scoped to one project.
pub const PROJECT_CONTEXT_TYPE: &str = "project";

/// Whether a chat of this kind can have a sandbox linked to it: the Temps
/// assistant, and chats about one project (whose sandbox is seeded from the
/// project's repository).
pub fn context_supports_sandbox(context_type: &str) -> bool {
    context_type == ASSISTANT_CONTEXT_TYPE || context_type == PROJECT_CONTEXT_TYPE
}

/// The project a new sandbox for this chat is created from: only project chats
/// carry one, and the sandbox then starts as a clone of its repository.
pub fn project_for_new_sandbox(conversation: &ai_conversations::Model) -> Option<i32> {
    if conversation.context_type == PROJECT_CONTEXT_TYPE {
        conversation.project_id
    } else {
        None
    }
}

#[derive(Error, Debug)]
pub enum SandboxLinkError {
    #[error(
        "Conversation {conversation_id} cannot use a sandbox: this server has no sandbox backend"
    )]
    Unavailable { conversation_id: String },

    #[error("Sandbox {sandbox_id} was not found among the sandboxes of user {user_id}")]
    SandboxNotFound { sandbox_id: String, user_id: i32 },

    #[error("Sandbox {sandbox_id} cannot be linked to conversation {conversation_id}: {reason}")]
    NotLinkable {
        sandbox_id: String,
        conversation_id: String,
        reason: String,
    },

    #[error("Conversation {conversation_id} has no linked sandbox")]
    NotLinked { conversation_id: String },

    #[error("Invalid sandbox link request for conversation {conversation_id}: {message}")]
    Validation {
        conversation_id: String,
        message: String,
    },

    #[error("Sandbox operation failed for conversation {conversation_id}: {reason}")]
    Sandbox {
        conversation_id: String,
        reason: String,
    },

    #[error("Database error while updating the sandbox link of conversation {conversation_id}: {source}")]
    Database {
        conversation_id: String,
        #[source]
        source: DbErr,
    },
}

/// The sandbox a chat is linked to, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SandboxLink {
    /// Public id of the sandbox (`sbx_…`).
    pub sandbox_id: String,
    /// Display name of the sandbox when it was linked.
    pub name: String,
    /// ISO 8601 time the link was made.
    pub linked_at: DateTime<Utc>,
}

/// Read the link out of a conversation's metadata, if there is a well-formed one.
pub fn link_from_metadata(metadata: Option<&Value>) -> Option<SandboxLink> {
    let link = metadata?.get(SANDBOX_LINK_KEY)?;
    serde_json::from_value(link.clone()).ok()
}

/// Whether a variable holds something that must not be shown.
fn is_secret_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    // A key prefix names where a sandbox may write; it is not a credential.
    if name.ends_with("KEY_PREFIX") {
        return false;
    }
    ["PASSWORD", "SECRET", "TOKEN", "KEY", "URL"]
        .iter()
        .any(|marker| name.contains(marker))
}

/// Replace the secret values among `variables` that appear in `text`.
///
/// This keeps a password out of the transcript when a command prints its
/// environment. It is hygiene, not the boundary: the credential only reaches a
/// database of the sandbox's own.
pub fn redact_secrets(text: &str, variables: &HashMap<String, String>) -> String {
    let mut secrets: Vec<&str> = variables
        .iter()
        .filter(|(name, value)| is_secret_variable(name) && value.len() >= 6)
        .map(|(_, value)| value.as_str())
        .collect();
    // Longest first, so a URL that contains the password is replaced whole.
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    let mut redacted = text.to_string();
    for secret in secrets {
        redacted = redacted.replace(secret, "[redacted]");
    }
    redacted
}

/// A service a sandbox is linked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConnectedService {
    pub service_id: i32,
    /// The project the link was made through, if it was made through one.
    pub project_id: Option<i32>,
}

impl From<&LinkedService> for ConnectedService {
    fn from(linked: &LinkedService) -> Self {
        Self {
            service_id: linked.service_id,
            project_id: linked.project_id,
        }
    }
}

/// The projects a sandbox's links were made through, sorted and without
/// duplicates.
pub fn connected_project_ids(linked: &[LinkedService]) -> Vec<i32> {
    let mut projects: Vec<i32> = linked.iter().filter_map(|link| link.project_id).collect();
    projects.sort_unstable();
    projects.dedup();
    projects
}

/// The projects among a sandbox's links that `auth` can no longer reach, so
/// the links made through them must go before the sandbox is used again.
///
/// A link is checked when it is made, but access can be taken away later: a
/// member removed from a project would otherwise keep the credentials and the
/// network route to its services for as long as the sandbox lives. Mirrors
/// `project_access_guard!`: a credential confined to another project loses the
/// link, instance administrators and deployment tokens are not subject to team
/// membership, and with no team layer installed there is no membership to
/// lose. A failed check is an `Err`, so nothing runs on an answer that could
/// not be had.
pub(crate) async fn revoked_link_projects(
    auth: &AuthContext,
    linked: &[LinkedService],
    checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<Vec<i32>, String> {
    let projects = connected_project_ids(linked);
    let mut revoked: Vec<i32> = projects
        .iter()
        .copied()
        .filter(|project_id| !auth.is_scoped_to_project(*project_id))
        .collect();
    if projects.is_empty()
        || auth.is_deployment_token()
        || auth.is_admin()
        || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin)
    {
        return Ok(revoked);
    }
    let Some(checker) = checker else {
        return Ok(revoked);
    };
    let Some(user_id) = auth.user_id_opt() else {
        // No user to check membership for: nothing reached through a project
        // stays linked.
        return Ok(projects);
    };
    let access = checker
        .user_can_access_projects(user_id, &projects)
        .await
        .map_err(|error| {
            format!(
                "access to projects {projects:?} could not be checked for user {user_id}: {error}"
            )
        })?;
    for project_id in projects {
        let allowed = access.get(&project_id).copied().unwrap_or(false);
        if !allowed && !revoked.contains(&project_id) {
            revoked.push(project_id);
        }
    }
    revoked.sort_unstable();
    Ok(revoked)
}

/// What linking the sandbox to services gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SandboxConnections {
    /// Projects the sandbox is now linked to services through.
    pub project_ids: Vec<i32>,
    /// Every service the sandbox is now linked to.
    pub services: Vec<ConnectedService>,
    /// Names (never values) of the variables commands now run with.
    pub variable_names: Vec<String>,
    /// Services the sandbox can now reach.
    pub connected_service_ids: Vec<i32>,
    /// Linked services left out because their type cannot give a sandbox
    /// credentials of its own yet.
    pub skipped_service_ids: Vec<i32>,
}

struct CachedCredentials {
    resolved: Instant,
    variables: Arc<HashMap<String, String>>,
}

/// The sandbox a chat stopped using when its link went from `previous` to
/// `current`: none when it was re-linked to the same one.
fn sandbox_left_behind<'a>(previous: &'a str, current: Option<&str>) -> Option<&'a str> {
    (current != Some(previous)).then_some(previous)
}

/// The tool result refusing `temps_create_sandbox` in a chat whose permission
/// mode asks before changes, or `None` when the mode allows it. The tool is
/// only offered in such chats, but a model can emit the name anyway, so the
/// same rule is enforced when it is called.
fn create_refused_by_permission_mode(permission_mode: &str) -> Option<String> {
    if crate::service::permission_mode_auto_approves_provider_tools(permission_mode) {
        return None;
    }
    Some(format!(
        "`{SANDBOX_CREATE_TOOL}` is not available: this chat asks before making changes, so \
         the user decides whether to create a sandbox. Offer one with `{SANDBOX_OFFER_TOOL}` \
         when a shell step is really needed."
    ))
}

/// The key a sandbox's credentials are cached under.
fn credential_cache_key(sandbox_id: &str) -> String {
    sandbox_id.to_string()
}

/// Drop every cached credential of `sandbox_id`: the entry under its own key
/// and any keyed `"{sandbox_id}:…"`.
fn forget_cached_credentials(cache: &mut HashMap<String, CachedCredentials>, sandbox_id: &str) {
    let key = credential_cache_key(sandbox_id);
    let prefix = format!("{sandbox_id}:");
    cache.retain(|cached, _| *cached != key && !cached.starts_with(&prefix));
}

/// The port the assistant last confirmed serving a page, if one is recorded.
pub fn preview_port_from_metadata(metadata: Option<&Value>) -> Option<u16> {
    let port = metadata?.get(SANDBOX_PREVIEW_KEY)?.as_u64()?;
    u16::try_from(port).ok().filter(|port| *port >= 1)
}

pub(crate) fn offered_in_metadata(metadata: Option<&Value>) -> bool {
    metadata
        .and_then(|value| value.get(SANDBOX_OFFERED_KEY))
        .is_some_and(|value| !value.is_null())
}

/// Why a sandbox may not be linked, if it may not.
///
/// Application workspaces are the persistent per-user environments managed by
/// the application API and agent-run sandboxes belong to their run; neither is
/// something a chat should shell into.
pub fn linkability_problem(
    name: &str,
    agent_run_id: Option<i32>,
    status: &str,
) -> Option<&'static str> {
    if name.starts_with(APPLICATION_WORKSPACE_PREFIX) {
        return Some("it is an application workspace, which is managed from its application");
    }
    if agent_run_id.is_some() {
        return Some("it belongs to an agent run, which manages its lifecycle");
    }
    if status == "destroyed" {
        return Some("it has been destroyed");
    }
    None
}

/// Whether a linked sandbox can take a command right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxReadiness {
    /// Running: run the tool as is.
    Ready,
    /// Stopped ("sleeping"): wake it first, and say so.
    Asleep,
}

/// Classify a sandbox row's status. Only an explicitly stopped sandbox needs
/// waking; `destroyed` is rejected earlier by [`linkability_problem`].
pub fn readiness(status: &str) -> SandboxReadiness {
    if status == "stopped" {
        SandboxReadiness::Asleep
    } else {
        SandboxReadiness::Ready
    }
}

/// Put a notice for the user and the model in front of a tool result.
fn with_notice(notice: Option<String>, result: String) -> String {
    match notice {
        Some(notice) => format!("{notice}\n\n{result}"),
        None => result,
    }
}

/// Services that publish a sandbox's port through someone else's network.
/// Temps serves previews itself, so a command that fetches or starts one is
/// refused. This is a guard on the common tools, not a security boundary.
const EXTERNAL_EXPOSURE_TOOLS: &[&str] = &[
    "cloudflared",
    "trycloudflare",
    "ngrok",
    "localtunnel",
    "loca.lt",
    "untun",
    "serveo",
    "localhost.run",
    "pinggy",
    "tunnelto",
    "bore.pub",
];

fn is_word_boundary(c: Option<char>) -> bool {
    c.map(|c| !c.is_ascii_alphanumeric()).unwrap_or(true)
}

/// The third-party tunnel service a shell command uses, if any.
pub fn third_party_exposure(command: &str) -> Option<&'static str> {
    let lowered = command.to_ascii_lowercase();
    for name in EXTERNAL_EXPOSURE_TOOLS {
        let mut from = 0;
        while let Some(found) = lowered[from..].find(name) {
            let start = from + found;
            let end = start + name.len();
            let before = lowered[..start].chars().next_back();
            let after = lowered[end..].chars().next();
            if is_word_boundary(before) && is_word_boundary(after) {
                return Some(name);
            }
            from = end;
        }
    }
    if lowered.contains("tailscale funnel") {
        return Some("tailscale funnel");
    }
    None
}

/// Where a port is listening inside the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenScope {
    /// Nothing listens on the port.
    Nothing,
    /// Only on the loopback address: the preview cannot reach it.
    LoopbackOnly,
    /// On a non-loopback address: the preview can reach it.
    Reachable,
}

/// Read a port's listener out of the text of `/proc/net/tcp` and
/// `/proc/net/tcp6` (concatenated).
pub fn listen_scope(proc_net_tcp: &str, port: u16) -> ListenScope {
    let wanted = format!("{port:04X}");
    let mut scope = ListenScope::Nothing;
    for line in proc_net_tcp.lines() {
        let columns: Vec<&str> = line.split_whitespace().collect();
        // Column 3 is the socket state; 0A is LISTEN.
        if columns.len() < 4 || columns[3] != "0A" {
            continue;
        }
        let Some((address, listening_port)) = columns[1].rsplit_once(':') else {
            continue;
        };
        if !listening_port.eq_ignore_ascii_case(&wanted) {
            continue;
        }
        // /proc prints each 32-bit word little-endian: 127.0.0.1 is
        // `0100007F`, ::1 is `00000000000000000000000001000000`, and a
        // v4-mapped loopback ends the same way as the v4 one.
        let loopback = address.eq_ignore_ascii_case("00000000000000000000000001000000")
            || address.to_ascii_uppercase().ends_with("0100007F")
            || (address.len() == 8 && address.to_ascii_uppercase().ends_with("7F"));
        if loopback {
            if scope == ListenScope::Nothing {
                scope = ListenScope::LoopbackOnly;
            }
        } else {
            scope = ListenScope::Reachable;
        }
    }
    scope
}

/// Keep the end of `text` (errors and results come last), at most `max` bytes.
fn cap_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[earlier output truncated: showing the last {} of {} bytes]\n{}",
        text.len() - start,
        text.len(),
        &text[start..]
    )
}

/// The window of `text` a read returns: the whole file when it fits, otherwise
/// the requested lines with a note saying which lines these are and how to
/// ask for the next ones.
fn read_window(
    path: &str,
    text: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    max_bytes: usize,
) -> String {
    read_window_of(path, text, offset, limit, max_bytes, None)
}

/// The part of a file that was read, when it is only its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileHead {
    /// Size of the whole file.
    file_bytes: u64,
}

/// A path quoted for the shell commands the read result suggests.
fn quoted_path(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// The window of an oversized file's first [`MAX_READ_FILE_BYTES`] bytes
/// (`head`, as the sandbox handed them back). The last line, which the cut
/// may have split, is dropped unless it is the only one; a character the cut
/// split is dropped with it.
fn read_head_window(
    path: &str,
    head: &str,
    file_bytes: u64,
    offset: Option<usize>,
    limit: Option<usize>,
    max_bytes: usize,
) -> String {
    let complete = match head.rfind('\n') {
        Some(end) => &head[..=end],
        None => head.trim_end_matches('\u{FFFD}'),
    };
    read_window_of(
        path,
        complete,
        offset,
        limit,
        max_bytes,
        Some(FileHead { file_bytes }),
    )
}

fn read_window_of(
    path: &str,
    text: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    max_bytes: usize,
    head: Option<FileHead>,
) -> String {
    let total = text.split_inclusive('\n').count();
    let start = offset.unwrap_or(1).max(1);
    let wanted = limit.unwrap_or(DEFAULT_READ_LINES).clamp(1, MAX_READ_LINES);
    if let Some(FileHead { file_bytes }) = head {
        if start > total {
            return format!(
                "{path} is {file_bytes} bytes, larger than the {MAX_READ_FILE_BYTES} bytes \
                 `{SANDBOX_READ_TOOL}` reads, and offset {start} is past the {total} lines in \
                 that first part. Read further with `{SANDBOX_EXEC_TOOL}`, for example \
                 `sed -n '{start},{}p' {}`.",
                start + wanted - 1,
                quoted_path(path)
            );
        }
    }
    if total == 0 {
        return String::new();
    }
    if start > total {
        return format!(
            "{path} has {total} lines; offset {start} is past the end. Use an offset from 1 to {total}."
        );
    }
    let mut body = String::new();
    let mut last = start - 1;
    for line in text.split_inclusive('\n').skip(start - 1).take(wanted) {
        if body.len() + line.len() > max_bytes {
            if body.is_empty() {
                // One line larger than the budget: hand back its start.
                let mut end = max_bytes.min(line.len());
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                body.push_str(&line[..end]);
                body.push_str("\n[line cut: it is longer than the read limit]\n");
                last += 1;
            }
            break;
        }
        body.push_str(line);
        last += 1;
    }
    if let Some(FileHead { file_bytes }) = head {
        let mut out = format!(
            "[{path} is {file_bytes} bytes, larger than the {MAX_READ_FILE_BYTES} bytes \
             `{SANDBOX_READ_TOOL}` reads; showing lines {start}-{last} of the {total} complete \
             lines in its first {MAX_READ_FILE_BYTES} bytes."
        );
        if last < total {
            out.push_str(&format!(
                " Call `{SANDBOX_READ_TOOL}` again with offset={} to continue within that part;",
                last + 1
            ));
        }
        out.push_str(&format!(
            " read past it with `{SANDBOX_EXEC_TOOL}`, for example `sed -n '{},{}p' {}`.]\n",
            total + 1,
            total + DEFAULT_READ_LINES,
            quoted_path(path)
        ));
        out.push_str(&body);
        return out;
    }
    if start == 1 && last == total && !body.contains("[line cut:") {
        return body;
    }
    let mut out = format!("[{path}: lines {start}-{last} of {total}.");
    if last < total {
        out.push_str(&format!(
            " Call `{SANDBOX_READ_TOOL}` again with offset={} to continue.",
            last + 1
        ));
    }
    out.push_str("]\n");
    out.push_str(&body);
    out
}

/// What the head-reading script printed: the file's size on the first line,
/// then its first [`MAX_READ_FILE_BYTES`] bytes. `None` when the output is not
/// that shape, or the file turned out not to be over the limit after all.
fn parse_head_output(stdout: &str) -> Option<(u64, &str)> {
    let (size, head) = stdout.split_once('\n')?;
    let size: u64 = size.trim().parse().ok()?;
    if size <= MAX_READ_FILE_BYTES as u64 {
        return None;
    }
    // A shimmed `head` could print more than asked; never hand on more.
    let mut end = head.len().min(MAX_READ_FILE_BYTES);
    while !head.is_char_boundary(end) {
        end -= 1;
    }
    Some((size, &head[..end]))
}

/// The script that prints a regular file's size and its first
/// [`MAX_READ_FILE_BYTES`] bytes, or exits non-zero when the path is not a
/// regular file over that size.
fn read_head_command(path: &str) -> String {
    format!(
        "f={}; [ -f \"$f\" ] || exit 3; size=$(wc -c < \"$f\") || exit 4; \
         [ \"$size\" -gt {MAX_READ_FILE_BYTES} ] || exit 5; printf '%s\\n' \"$size\"; \
         head -c {MAX_READ_FILE_BYTES} \"$f\"",
        quoted_path(path)
    )
}

/// Resolve a model-supplied path against the sandbox work dir.
/// A file to copy into a chat's sandbox.
pub struct SandboxFile<'a> {
    /// The attachment's opaque id; keeps two files with one name apart.
    pub id: &'a str,
    pub name: &'a str,
    pub bytes: &'a [u8],
}

/// Where an attachment lands in a chat's sandbox: the same layout a workspace
/// chat uses, under the sandbox's working directory. Name and id are already
/// sanitized by the upload path (no separators), so they cannot climb out.
pub fn attachment_sandbox_path(
    work_dir: &str,
    conversation_public_id: &str,
    attachment_id: &str,
    name: &str,
) -> String {
    resolve_path(
        work_dir,
        &format!(".temps/chat-attachments/{conversation_public_id}/{attachment_id}/{name}"),
    )
}

pub(crate) fn resolve_path(work_dir: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{}", work_dir.trim_end_matches('/'), path)
    }
}

#[derive(Debug, Deserialize)]
struct ExecArgs {
    command: String,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
    /// First line to return, counting from 1.
    #[serde(default)]
    offset: Option<usize>,
    /// Most lines to return.
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    executable: bool,
}

#[derive(Debug, Deserialize)]
struct PreviewArgs {
    port: Option<u16>,
}

/// Separates the listener table from the HTTP status in the probe's output.
const PROBE_SEPARATOR: &str = "---temps-probe---";

/// The tool result for a preview check: the address, whether it will work,
/// and what to do when it will not.
fn preview_report(url: &str, port: u16, scope: ListenScope, status: Option<u16>) -> String {
    let (ready, advice) = match (scope, status) {
        (ListenScope::Nothing, _) => (
            false,
            format!(
                "Nothing is listening on port {port} in the sandbox. Start the server in the \
                 background (`nohup … &`) bound to 0.0.0.0, wait for it to finish starting, and \
                 call this again."
            ),
        ),
        (ListenScope::LoopbackOnly, _) => (
            false,
            format!(
                "The server on port {port} listens only on localhost, so the preview cannot \
                 reach it. Restart it bound to 0.0.0.0 (for example `-H 0.0.0.0` for Next.js or \
                 `--host 0.0.0.0` for Vite)."
            ),
        ),
        (ListenScope::Reachable, None) => (
            false,
            format!(
                "Port {port} is open but the server did not answer an HTTP request. It may \
                 still be starting: check its log, then call this again."
            ),
        ),
        (ListenScope::Reachable, Some(code)) => (
            true,
            format!(
                "The server answers on port {port} (HTTP {code}). Give the user this URL; it is \
                 served by Temps, so no tunnel or outside service is needed."
            ),
        ),
    };
    json!({ "url": url, "ready": ready, "advice": advice }).to_string()
}

#[derive(Debug, Deserialize)]
struct OfferArgs {
    reason: String,
}

/// Whether `name` is one of the tools this module serves.
pub fn is_sandbox_tool(name: &str) -> bool {
    matches!(
        name,
        SANDBOX_EXEC_TOOL
            | SANDBOX_READ_TOOL
            | SANDBOX_WRITE_TOOL
            | SANDBOX_PREVIEW_TOOL
            | SANDBOX_OFFER_TOOL
            | SANDBOX_CREATE_TOOL
            | SANDBOX_DEPLOY_TOOL
    )
}

/// The tools a chat gets for its sandbox situation.
///
/// Linked: the three scoped tools, if the caller may execute in sandboxes.
/// Unlinked, and the caller has approved changes without asking (`may_create`):
/// the same three plus `temps_create_sandbox`, which makes a brand-new sandbox
/// for this chat. They are offered together so the model can create one and use
/// it in the same turn; each call re-checks the link.
/// Unlinked otherwise: only the offer tool, once, and the user decides.
/// Either way the model never picks an existing sandbox.
pub fn tools_for(
    linked: bool,
    offered: bool,
    backend_available: bool,
    may_exec: bool,
    may_create: bool,
    may_deploy: bool,
) -> Vec<ChatTool> {
    if !backend_available || !may_exec {
        return Vec::new();
    }
    if linked || may_create {
        let mut tools = vec![
            ChatTool {
                name: SANDBOX_EXEC_TOOL.to_string(),
                description: "Run a shell command in the sandbox the user linked to this chat. \
                    Use it for Git work (clone, branch, commit), unpacking files, installing \
                    dependencies and starting or testing a server. The command runs with `sh -c` \
                    in the sandbox work directory unless `cwd` is set. Returns the exit code, \
                    stdout and stderr. A non-zero exit code is a result, not a tool failure: read \
                    it and decide what to do. The sandbox has no Git or platform credentials, so \
                    private clones and pushes go through the Temps repository operations instead."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to run." },
                        "cwd": { "type": "string", "description": "Absolute working directory. Defaults to the sandbox work directory." },
                        "timeout_secs": { "type": "integer", "description": "Give up after this many seconds (1 to 600, default 120). Start long-lived servers in the background (`nohup … &`) instead of waiting on them." }
                    },
                    "required": ["command"]
                }),
            },
            ChatTool {
                name: SANDBOX_READ_TOOL.to_string(),
                description: "Read a text file from the linked sandbox. Relative paths resolve \
                    against the sandbox work directory. A long file comes back in pieces of \
                    about 500 lines, with a note naming the lines shown and the `offset` for \
                    the next piece: read only the part you need, and prefer `sandbox_exec` \
                    with `grep -n` or `sed -n '120,200p'` to find and fetch a region of a big \
                    file."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path." },
                        "offset": { "type": "integer", "description": "First line to return, counting from 1. Defaults to 1." },
                        "limit": { "type": "integer", "description": "Most lines to return (default 500, at most 2000)." }
                    },
                    "required": ["path"]
                }),
            },
            ChatTool {
                name: SANDBOX_WRITE_TOOL.to_string(),
                description: "Write a text file in the linked sandbox, replacing any existing \
                    file. Relative paths resolve against the sandbox work directory. At most \
                    1 MiB per call. Keep a single project flat at the top of the work \
                    directory (`package.json`, `index.html`, `src/…`), not inside a folder named \
                    after it; give each project its own folder only when the sandbox holds more \
                    than one."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path." },
                        "content": { "type": "string", "description": "Full file content." },
                        "executable": { "type": "boolean", "description": "Mark the file executable." }
                    },
                    "required": ["path", "content"]
                }),
            },
            ChatTool {
                name: SANDBOX_PREVIEW_TOOL.to_string(),
                description: "Get the address the user opens in their browser to see a server \
                    running in the sandbox, and check that the server is listening. Start the \
                    server first, in the background and bound to 0.0.0.0. Call this every time you \
                    tell the user a page is running: it opens the page in the chat's Preview tab. \
                    Always give the user the URL this returns: it is served by Temps, so no \
                    tunnel or outside service is ever needed."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "port": { "type": "integer", "description": "Port the server listens on inside the sandbox (1 to 65535)." }
                    },
                    "required": ["port"]
                }),
            },
        ];
        if may_deploy {
            tools.push(crate::sandbox_deploy::deploy_tool());
        }
        if !linked {
            tools.push(create_tool());
        }
        return tools;
    }
    if offered {
        return Vec::new();
    }
    vec![ChatTool {
        name: SANDBOX_OFFER_TOOL.to_string(),
        description: "Offer the user a sandbox for this chat. Call it only when the next step \
            needs a shell and cannot be done with the platform tools: several Git operations \
            on a working copy, unpacking an uploaded archive, or running and testing a server. \
            Never call it for plain platform reads and changes. The user decides: they can \
            create a sandbox, link one of their own, or decline. It has no side effects and \
            you cannot link anything yourself. After calling it, tell the user in one sentence \
            what you want to do with the sandbox and stop; the shell tools appear on your next \
            turn once they link one. It can be called once per chat."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string", "description": "One sentence on what you would do in the sandbox, e.g. 'unpack the uploaded archive and commit it to the new repository'." }
            },
            "required": ["reason"]
        }),
    }]
}

fn create_tool() -> ChatTool {
    ChatTool {
        name: SANDBOX_CREATE_TOOL.to_string(),
        description: "Create a new sandbox for this chat and link it, when none is linked. \
            Call it right away, without asking, when the user wants you to build or change an \
            app or site and the work needs a shell: writing code, installing dependencies, \
            running it and testing it. In a project chat the sandbox starts as a clone of the \
            project's repository; otherwise it is empty. It creates a new sandbox only; you \
            cannot pick or link an existing one. Do not call it for plain platform reads and \
            changes. When it returns, `sandbox_exec`, `sandbox_read_file` and \
            `sandbox_write_file` work in this same turn."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string", "description": "One sentence on what you will build or run there." }
            },
            "required": ["reason"]
        }),
    }
}

/// A linked sandbox that is up and may be used for a tool call.
pub(crate) struct ReadySandbox {
    pub(crate) link: SandboxLink,
    pub(crate) work_dir: String,
    /// Set when the sandbox had to be woken; goes in front of the tool result.
    pub(crate) notice: Option<String>,
}

/// The chats of one workspace share its sandbox (ADR-053): the link and the
/// confirmed preview port are written to every assistant chat of the
/// workspace at once, so whichever chat links, relinks or unlinks, the others
/// follow. Other metadata stays per chat.
fn is_workspace_shared_key(key: &str) -> bool {
    matches!(key, SANDBOX_LINK_KEY | SANDBOX_PREVIEW_KEY)
}

/// The rows a metadata write touches: the chat itself, plus (for a shared
/// key) the other assistant chats of the same owner in the same workspace.
/// Workspace membership is read in the statement, so a chat that just joined
/// or left is scoped correctly without reloading it first.
///
/// An autofix incident chat (ADR-054) is in its workspace but keeps its own
/// sandbox: it neither shares its link nor receives the workspace's.
pub(crate) fn metadata_scope(conversation_param: &str, shared: bool) -> String {
    if !shared {
        return format!("id = {conversation_param}");
    }
    let incident_key = crate::autofix::INCIDENT_METADATA_KEY;
    format!(
        "(id = {conversation_param} OR id IN ( \
            SELECT sibling.id FROM ai_conversations sibling \
            JOIN ai_conversations chat ON chat.id = {conversation_param} \
            WHERE chat.application_id IS NOT NULL \
              AND chat.context_type = '{ASSISTANT_CONTEXT_TYPE}' \
              AND sibling.context_type = '{ASSISTANT_CONTEXT_TYPE}' \
              AND sibling.application_id = chat.application_id \
              AND sibling.created_by = chat.created_by \
              AND chat.metadata -> '{incident_key}' IS NULL \
              AND sibling.metadata -> '{incident_key}' IS NULL))"
    )
}

/// Links a sandbox to a chat and runs the chat's scoped sandbox tools.
#[derive(Clone)]
pub struct SandboxLinkService {
    pub(crate) db: Arc<DatabaseConnection>,
    pub(crate) sandboxes: Arc<SandboxService>,
    pub(crate) audit: Option<Arc<dyn AuditLogger>>,
    /// Deploys an archive of the sandbox's files; absent on a server without
    /// the deployments plugin.
    pub(crate) source_drop: Option<Arc<dyn temps_core::SourceDropDeployer>>,
    /// The team layer, when one is installed: every tool call re-checks that
    /// the user can still reach each project the sandbox is linked through.
    project_access: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// Credentials resolved for recent commands, by sandbox and project set.
    /// Held in memory only.
    credentials: Arc<Mutex<HashMap<String, CachedCredentials>>>,
}

impl SandboxLinkService {
    pub fn new(db: Arc<DatabaseConnection>, sandboxes: Arc<SandboxService>) -> Self {
        Self {
            db,
            sandboxes,
            audit: None,
            source_drop: None,
            project_access: None,
            credentials: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Let the assistant deploy the sandbox's files as a project's source.
    pub fn with_source_drop(mut self, deployer: Arc<dyn temps_core::SourceDropDeployer>) -> Self {
        self.source_drop = Some(deployer);
        self
    }

    /// Check project membership again on every tool call, with the team layer's
    /// current answer. Without one, there is no membership to check.
    pub fn with_project_access(
        mut self,
        checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    ) -> Self {
        self.project_access = checker;
        self
    }

    /// Record sandboxes the assistant creates itself in the audit log.
    pub fn with_audit(mut self, audit: Arc<dyn AuditLogger>) -> Self {
        self.audit = Some(audit);
        self
    }

    fn db_err(conversation: &ai_conversations::Model, source: DbErr) -> SandboxLinkError {
        SandboxLinkError::Database {
            conversation_id: conversation.public_id.clone(),
            source,
        }
    }

    /// Merge one key into the conversation metadata in a single statement, so
    /// it cannot overwrite concurrent writers of other metadata keys.
    async fn merge_metadata_key(
        &self,
        conversation: &ai_conversations::Model,
        key: &str,
        value: &Value,
    ) -> Result<(), SandboxLinkError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE ai_conversations \
                     SET metadata = (CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata ELSE '{{}}'::jsonb END) \
                         || jsonb_build_object($1::text, $2::jsonb) \
                     WHERE {}",
                    metadata_scope("$3", is_workspace_shared_key(key))
                ),
                [key.into(), value.clone().into(), conversation.id.into()],
            ))
            .await
            .map_err(|source| Self::db_err(conversation, source))?;
        Ok(())
    }

    /// [`Self::merge_metadata_key`] for the link, applied only where the
    /// stored link still names `expected` (`None`: no link). Returns whether
    /// it was written.
    async fn merge_link_if_unchanged(
        &self,
        conversation: &ai_conversations::Model,
        value: &Value,
        expected: Option<&str>,
    ) -> Result<bool, SandboxLinkError> {
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE ai_conversations \
                     SET metadata = (CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata ELSE '{{}}'::jsonb END) \
                         || jsonb_build_object($1::text, $2::jsonb) \
                     WHERE {} \
                       AND (CASE WHEN jsonb_typeof(metadata) = 'object' \
                                 THEN metadata -> $1::text ->> 'sandbox_id' END) \
                           IS NOT DISTINCT FROM $4",
                    metadata_scope("$3", is_workspace_shared_key(SANDBOX_LINK_KEY))
                ),
                [
                    SANDBOX_LINK_KEY.into(),
                    value.clone().into(),
                    conversation.id.into(),
                    expected.map(str::to_string).into(),
                ],
            ))
            .await
            .map_err(|source| Self::db_err(conversation, source))?;
        Ok(result.rows_affected() > 0)
    }

    async fn remove_metadata_key(
        &self,
        conversation: &ai_conversations::Model,
        key: &str,
    ) -> Result<(), SandboxLinkError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE ai_conversations \
                     SET metadata = metadata - $1::text \
                     WHERE {} AND jsonb_typeof(metadata) = 'object'",
                    metadata_scope("$2", is_workspace_shared_key(key))
                ),
                [key.into(), conversation.id.into()],
            ))
            .await
            .map_err(|source| Self::db_err(conversation, source))?;
        Ok(())
    }

    /// Link a sandbox the user already owns.
    pub async fn link_existing(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        sandbox_id: &str,
    ) -> Result<SandboxLink, SandboxLinkError> {
        let row = self
            .sandboxes
            .find_by_public_id(sandbox_id, user_id)
            .await
            .map_err(|_| SandboxLinkError::SandboxNotFound {
                sandbox_id: sandbox_id.to_string(),
                user_id,
            })?;
        if let Some(reason) = linkability_problem(&row.name, row.agent_run_id, &row.status) {
            return Err(SandboxLinkError::NotLinkable {
                sandbox_id: sandbox_id.to_string(),
                conversation_id: conversation.public_id.clone(),
                reason: reason.to_string(),
            });
        }
        self.store_link(conversation, row.public_id, row.name).await
    }

    /// Create a fresh sandbox for the chat and link it.
    ///
    /// The create runs in a detached task. It inserts the sandbox row before
    /// the container exists, and a slow first image pull can outlast the wait
    /// below (or the chat turn can be cancelled): dropping the future at that
    /// point would leave a row with no container, which can never be resumed.
    /// The task finishes the create and links the sandbox regardless, and the
    /// caller only waits `CREATE_WAIT_SECS` for the answer.
    ///
    /// `assistant_reason` is `Some` when the model asked for the sandbox; the
    /// task then audits it, so a slow create is audited too.
    pub async fn create_and_link(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        assistant_reason: Option<String>,
    ) -> Result<SandboxLink, SandboxLinkError> {
        let project_id = project_for_new_sandbox(conversation);
        let prefix = if project_id.is_some() {
            "project-chat"
        } else {
            "assistant-chat"
        };
        let name: String = format!("{prefix}-{}", conversation.public_id)
            .chars()
            .take(48)
            .collect();
        let request = CreateSandboxRequest {
            name: Some(name),
            timeout_secs: Some(CREATED_SANDBOX_TIMEOUT_SECS),
            lifecycle: Some("workspace".to_string()),
            project_id,
            ..Default::default()
        };
        let service = self.clone();
        let target = conversation.clone();
        // The link the chat has now. A slow create must not replace a sandbox
        // the user links by hand while it runs.
        let linked_before =
            link_from_metadata(conversation.metadata.as_ref()).map(|l| l.sandbox_id);
        let task = tokio::spawn(async move {
            let row = match service.sandboxes.create_sandbox(user_id, request).await {
                Ok(row) => row,
                Err(error) => {
                    // The chat may have stopped waiting already; this log is
                    // then the only record of why no sandbox was linked.
                    warn!(
                        conversation_id = %target.public_id,
                        user_id,
                        error = %error,
                        "creating a sandbox for the chat failed; nothing was linked"
                    );
                    return Err(SandboxLinkError::Sandbox {
                        conversation_id: target.public_id.clone(),
                        reason: format!("could not create a sandbox: {error}"),
                    });
                }
            };
            let Some(link) = service
                .store_link_if_unchanged(
                    &target,
                    row.public_id.clone(),
                    row.name,
                    linked_before.as_deref(),
                )
                .await?
            else {
                info!(
                    conversation_id = %target.public_id,
                    sandbox_id = %row.public_id,
                    "the chat was linked to another sandbox while this one was created; leaving it unlinked"
                );
                return Err(SandboxLinkError::Sandbox {
                    conversation_id: target.public_id.clone(),
                    reason: format!(
                        "sandbox {} was created, but the chat was linked to another sandbox in \
                         the meantime, so it was left unlinked",
                        row.public_id
                    ),
                });
            };
            if let Some(reason) = assistant_reason {
                service
                    .audit_created(&target, user_id, &link.sandbox_id, &reason)
                    .await;
            }
            Ok::<SandboxLink, SandboxLinkError>(link)
        });
        match tokio::time::timeout(Duration::from_secs(CREATE_WAIT_SECS), task).await {
            Ok(Ok(result)) => result,
            Ok(Err(join_error)) => Err(SandboxLinkError::Sandbox {
                conversation_id: conversation.public_id.clone(),
                reason: format!("the sandbox creation stopped unexpectedly: {join_error}"),
            }),
            Err(_) => Err(SandboxLinkError::Sandbox {
                conversation_id: conversation.public_id.clone(),
                reason: format!(
                    "the sandbox was still starting after {CREATE_WAIT_SECS} seconds (the first \
                     one pulls its image). It keeps starting in the background and is linked to \
                     this chat automatically when it is ready: tell the user, then try the \
                     sandbox tools again in a minute"
                ),
            }),
        }
    }

    async fn store_link(
        &self,
        conversation: &ai_conversations::Model,
        sandbox_id: String,
        name: String,
    ) -> Result<SandboxLink, SandboxLinkError> {
        let sandbox = sandbox_id.clone();
        self.write_link(conversation, sandbox_id, name, None)
            .await?
            .ok_or_else(|| SandboxLinkError::Validation {
                conversation_id: conversation.public_id.clone(),
                message: format!("the link to sandbox {sandbox} was not stored"),
            })
    }

    /// Store the link only while the chat is still linked to `expected`
    /// (`None`: not linked). `Ok(None)` when the link changed in between.
    async fn store_link_if_unchanged(
        &self,
        conversation: &ai_conversations::Model,
        sandbox_id: String,
        name: String,
        expected: Option<&str>,
    ) -> Result<Option<SandboxLink>, SandboxLinkError> {
        self.write_link(conversation, sandbox_id, name, Some(expected))
            .await
    }

    async fn write_link(
        &self,
        conversation: &ai_conversations::Model,
        sandbox_id: String,
        name: String,
        expected: Option<Option<&str>>,
    ) -> Result<Option<SandboxLink>, SandboxLinkError> {
        let link = SandboxLink {
            sandbox_id,
            name,
            linked_at: Utc::now(),
        };
        let value = serde_json::to_value(&link).map_err(|error| SandboxLinkError::Validation {
            conversation_id: conversation.public_id.clone(),
            message: format!("could not encode the link: {error}"),
        })?;
        match expected {
            None => {
                self.merge_metadata_key(conversation, SANDBOX_LINK_KEY, &value)
                    .await?
            }
            Some(expected) => {
                if !self
                    .merge_link_if_unchanged(conversation, &value, expected)
                    .await?
                {
                    return Ok(None);
                }
            }
        }
        // A port confirmed on the previous sandbox says nothing about this one,
        // and neither do the services that sandbox was connected to.
        self.remove_metadata_key(conversation, SANDBOX_PREVIEW_KEY)
            .await?;
        // `conversation` still holds the previous link: the row now holds the
        // new one.
        self.release_connections(conversation, Some(&link.sandbox_id))
            .await;
        info!(
            conversation_id = %conversation.public_id,
            sandbox_id = %link.sandbox_id,
            "sandbox linked to chat"
        );
        Ok(Some(link))
    }

    /// Remove the link. The sandbox itself is left running and untouched.
    pub async fn unlink(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<(), SandboxLinkError> {
        self.remove_metadata_key(conversation, SANDBOX_LINK_KEY)
            .await?;
        self.remove_metadata_key(conversation, SANDBOX_PREVIEW_KEY)
            .await?;
        // Released only once the row no longer links it, so the check for
        // other chats using the sandbox does not count this one.
        self.release_connections(conversation, None).await;
        info!(
            conversation_id = %conversation.public_id,
            "sandbox unlinked from assistant chat"
        );
        Ok(())
    }

    /// Cut the sandbox the conversation was linked to (`conversation` as it
    /// was loaded, before the link changed) off from the services it was
    /// linked to, once no chat uses it: not when the chat was re-linked to the
    /// same sandbox (`current`), and not while another chat, such as a
    /// workspace sibling, still links it. Best effort: the chat's link has
    /// changed either way, and a sandbox nobody is working in should not keep
    /// reaching databases.
    async fn release_connections(
        &self,
        conversation: &ai_conversations::Model,
        current: Option<&str>,
    ) {
        let Some(link) = link_from_metadata(conversation.metadata.as_ref()) else {
            return;
        };
        let Some(previous) = sandbox_left_behind(&link.sandbox_id, current) else {
            return;
        };
        match self.sandbox_in_use(previous).await {
            Ok(false) => {}
            Ok(true) => {
                info!(
                    conversation_id = %conversation.public_id,
                    sandbox_id = previous,
                    "sandbox is still linked to another chat; keeping its service links"
                );
                return;
            }
            Err(error) => {
                warn!(
                    conversation_id = %conversation.public_id,
                    sandbox_id = previous,
                    "could not check whether another chat uses the sandbox, keeping its \
                     service links: {error}"
                );
                return;
            }
        }
        self.release_sandbox(
            conversation.created_by,
            &link.sandbox_id,
            &conversation.public_id,
        )
        .await;
    }

    /// Cut a sandbox off from every service it is linked to, for a sandbox no
    /// chat uses any more. Best effort, like [`Self::unlink`]: failures are
    /// logged with the chat that let the sandbox go.
    pub async fn release_sandbox(&self, user_id: i32, sandbox_id: &str, conversation_id: &str) {
        self.forget_credentials(sandbox_id);
        let linked = match self.sandboxes.linked_services(user_id, sandbox_id).await {
            Ok(linked) => linked,
            Err(error) => {
                warn!(
                    conversation_id,
                    sandbox_id, "could not read the sandbox's service links: {error}"
                );
                return;
            }
        };
        if linked.is_empty() {
            return;
        }
        let service_ids: Vec<i32> = linked.iter().map(|link| link.service_id).collect();
        match self
            .sandboxes
            .unlink_services(user_id, sandbox_id, &service_ids)
            .await
        {
            Ok(_) => info!(
                conversation_id,
                sandbox_id,
                services = service_ids.len(),
                "disconnected a sandbox no chat uses from its services"
            ),
            Err(error) => warn!(
                conversation_id,
                sandbox_id, "could not disconnect the sandbox from its services: {error}"
            ),
        }
    }

    /// Whether any chat's link names `sandbox_id`. One query, like the
    /// workspace check for sandboxes no chat uses.
    async fn sandbox_in_use(&self, sandbox_id: &str) -> Result<bool, DbErr> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT 1 AS used FROM ai_conversations \
                     WHERE jsonb_typeof(metadata) = 'object' \
                       AND metadata -> '{SANDBOX_LINK_KEY}' ->> 'sandbox_id' = $1 \
                     LIMIT 1"
                ),
                [sandbox_id.into()],
            ))
            .await?;
        Ok(row.is_some())
    }

    fn forget_credentials(&self, sandbox_id: &str) {
        forget_cached_credentials(
            &mut self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            sandbox_id,
        );
    }

    /// Link the chat's sandbox to the services of `project_id` that can give a
    /// sandbox credentials of its own.
    ///
    /// The sandbox gets a namespace of its own on each (a database, a bucket),
    /// named for the project, and commands then run with credentials for it.
    /// Nothing is linked unless at least one service could be.
    pub async fn connect_project(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        project_id: i32,
    ) -> Result<SandboxConnections, SandboxLinkError> {
        let link = self.linked_sandbox(conversation)?;
        self.wake_if_asleep(conversation, &link, user_id).await?;
        let service_ids = self
            .sandboxes
            .linkable_project_services(project_id)
            .await
            .map_err(|error| Self::connection_error(conversation, error))?;
        if service_ids.is_empty() {
            return Err(SandboxLinkError::Validation {
                conversation_id: conversation.public_id.clone(),
                message: format!(
                    "project {project_id} has no linked service that can give a sandbox \
                     credentials of its own yet (PostgreSQL, MariaDB, MongoDB, Redis and S3 can). Link a database or a \
                     bucket to the project first"
                ),
            });
        }
        let links: Vec<LinkedService> = service_ids
            .into_iter()
            .map(|service_id| LinkedService {
                service_id,
                project_id: Some(project_id),
            })
            .collect();
        let runtime = self
            .sandboxes
            .link_services(user_id, &link.sandbox_id, &links)
            .await
            .map_err(|error| Self::connection_error(conversation, error))?;
        self.forget_credentials(&link.sandbox_id);
        info!(
            conversation_id = %conversation.public_id,
            sandbox_id = %link.sandbox_id,
            project_id,
            services = ?runtime.connected_service_ids,
            "linked the chat's sandbox to a project's services"
        );
        self.connections_of(user_id, &link, &runtime).await
    }

    /// Link the chat's sandbox to one service. `project_id` is the project the
    /// service is used through, when there is one: it names the sandbox's
    /// database or bucket and must be a project the service is linked to.
    pub async fn connect_service(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        service_id: i32,
        project_id: Option<i32>,
    ) -> Result<SandboxConnections, SandboxLinkError> {
        let link = self.linked_sandbox(conversation)?;
        self.wake_if_asleep(conversation, &link, user_id).await?;
        let supported = self
            .sandboxes
            .service_supports_sandbox(service_id)
            .await
            .map_err(|error| Self::connection_error(conversation, error))?;
        if !supported {
            return Err(SandboxLinkError::Validation {
                conversation_id: conversation.public_id.clone(),
                message: format!(
                    "service {service_id} cannot give a sandbox credentials of its own: its type \
                     is not supported yet (PostgreSQL, MariaDB, MongoDB, Redis and S3 are), or it does not run on this \
                     server"
                ),
            });
        }
        let runtime = self
            .sandboxes
            .link_services(
                user_id,
                &link.sandbox_id,
                &[LinkedService {
                    service_id,
                    project_id,
                }],
            )
            .await
            .map_err(|error| Self::connection_error(conversation, error))?;
        self.forget_credentials(&link.sandbox_id);
        info!(
            conversation_id = %conversation.public_id,
            sandbox_id = %link.sandbox_id,
            service_id,
            "linked the chat's sandbox to a service"
        );
        self.connections_of(user_id, &link, &runtime).await
    }

    /// Unlink the chat's sandbox from the services it was linked to through
    /// `project_id`.
    pub async fn disconnect_project(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        project_id: i32,
    ) -> Result<SandboxConnections, SandboxLinkError> {
        let link = self.linked_sandbox(conversation)?;
        let (removed, runtime) = self
            .sandboxes
            .unlink_project(user_id, &link.sandbox_id, project_id)
            .await
            .map_err(|error| Self::connection_error(conversation, error))?;
        self.forget_credentials(&link.sandbox_id);
        info!(
            conversation_id = %conversation.public_id,
            sandbox_id = %link.sandbox_id,
            project_id,
            services = ?removed,
            "unlinked the chat's sandbox from a project's services"
        );
        self.connections_of(user_id, &link, &runtime).await
    }

    /// Unlink the chat's sandbox from one service.
    pub async fn disconnect_service(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        service_id: i32,
    ) -> Result<SandboxConnections, SandboxLinkError> {
        let link = self.linked_sandbox(conversation)?;
        let runtime = self
            .sandboxes
            .unlink_services(user_id, &link.sandbox_id, &[service_id])
            .await
            .map_err(|error| Self::connection_error(conversation, error))?;
        self.forget_credentials(&link.sandbox_id);
        info!(
            conversation_id = %conversation.public_id,
            sandbox_id = %link.sandbox_id,
            service_id,
            "unlinked the chat's sandbox from a service"
        );
        self.connections_of(user_id, &link, &runtime).await
    }

    fn linked_sandbox(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<SandboxLink, SandboxLinkError> {
        link_from_metadata(conversation.metadata.as_ref()).ok_or_else(|| {
            SandboxLinkError::NotLinked {
                conversation_id: conversation.public_id.clone(),
            }
        })
    }

    /// Start the chat's sandbox if it is asleep, so the console can have it up
    /// before anything asks for it. `Ok(true)` when it was woken, `Ok(false)`
    /// when it was already running.
    pub async fn wake_linked(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
    ) -> Result<bool, SandboxLinkError> {
        let link = self.linked_sandbox(conversation)?;
        self.wake_if_asleep(conversation, &link, user_id).await
    }

    /// Wake the sandbox before changing what it is connected to: a sleeping
    /// container has no network to attach to. Says whether it had to.
    async fn wake_if_asleep(
        &self,
        conversation: &ai_conversations::Model,
        link: &SandboxLink,
        user_id: i32,
    ) -> Result<bool, SandboxLinkError> {
        let row = self
            .sandboxes
            .find_by_public_id(&link.sandbox_id, user_id)
            .await
            .map_err(|_| SandboxLinkError::SandboxNotFound {
                sandbox_id: link.sandbox_id.clone(),
                user_id,
            })?;
        if readiness(&row.status) == SandboxReadiness::Asleep {
            self.wake(conversation.id, link, user_id)
                .await
                .map_err(|reason| SandboxLinkError::Sandbox {
                    conversation_id: conversation.public_id.clone(),
                    reason,
                })?;
            return Ok(true);
        }
        Ok(false)
    }

    /// What the sandbox is now linked to, read fresh from its links.
    async fn connections_of(
        &self,
        user_id: i32,
        link: &SandboxLink,
        runtime: &temps_sandbox::services::ScopedRuntime,
    ) -> Result<SandboxConnections, SandboxLinkError> {
        let linked = self
            .sandboxes
            .linked_services(user_id, &link.sandbox_id)
            .await
            .map_err(|error| SandboxLinkError::Sandbox {
                conversation_id: link.sandbox_id.clone(),
                reason: error.to_string(),
            })?;
        let mut variable_names = runtime.variable_names();
        variable_names.sort();
        Ok(SandboxConnections {
            project_ids: connected_project_ids(&linked),
            services: linked.iter().map(ConnectedService::from).collect(),
            variable_names,
            connected_service_ids: runtime.connected_service_ids.clone(),
            skipped_service_ids: runtime.skipped_service_ids.clone(),
        })
    }

    fn connection_error(
        conversation: &ai_conversations::Model,
        error: SandboxError,
    ) -> SandboxLinkError {
        match error {
            SandboxError::RuntimeVariableConflict { .. } => SandboxLinkError::Validation {
                conversation_id: conversation.public_id.clone(),
                message: format!(
                    "{error}. Two linked services each set the same variables, and an app reads \
                     one set (for example one PostgreSQL database): link one at a time"
                ),
            },
            SandboxError::ServiceNotFound { .. } => SandboxLinkError::Validation {
                conversation_id: conversation.public_id.clone(),
                message: error.to_string(),
            },
            other => SandboxLinkError::Sandbox {
                conversation_id: conversation.public_id.clone(),
                reason: other.to_string(),
            },
        }
    }

    /// Unlink the sandbox from every project the user can no longer reach,
    /// before anything runs in it: that drops the project's services from the
    /// sandbox's network and its commands' credentials. `Ok(Some(_))` is a
    /// notice saying what was disconnected; `Err` is the complete tool result
    /// when this could not be made sure of.
    async fn drop_revoked_links(
        &self,
        conversation_id: i64,
        link: &SandboxLink,
        auth: &AuthContext,
    ) -> Result<Option<String>, String> {
        let user_id = auth.user_id();
        let nothing_ran = |reason: String| {
            warn!(
                conversation_id,
                sandbox_id = %link.sandbox_id,
                "could not confirm the sandbox's project access: {reason}"
            );
            format!(
                "Nothing ran: access to the projects this sandbox is connected to could not be \
                 confirmed ({reason}). Tell the user and try again shortly."
            )
        };
        let linked = self
            .sandboxes
            .linked_services(user_id, &link.sandbox_id)
            .await
            .map_err(|error| nothing_ran(error.to_string()))?;
        let revoked = revoked_link_projects(auth, &linked, self.project_access.as_ref())
            .await
            .map_err(nothing_ran)?;
        if revoked.is_empty() {
            return Ok(None);
        }
        let mut result = Ok(());
        for &project_id in &revoked {
            if let Err(error) = self
                .sandboxes
                .unlink_project(user_id, &link.sandbox_id, project_id)
                .await
            {
                result = Err(format!("unlinking project {project_id} failed: {error}"));
                break;
            }
            self.audit_revoked(conversation_id, user_id, &link.sandbox_id, project_id)
                .await;
        }
        // Whatever was unlinked, the cached credentials may name it.
        self.forget_credentials(&link.sandbox_id);
        result.map_err(nothing_ran)?;
        info!(
            conversation_id,
            sandbox_id = %link.sandbox_id,
            user_id,
            projects = ?revoked,
            "disconnected the sandbox from projects the user can no longer access"
        );
        Ok(Some(format!(
            "The sandbox was disconnected from project(s) {revoked:?}: you no longer have access \
             to them, so their services and credentials are gone from it."
        )))
    }

    async fn audit_revoked(
        &self,
        conversation_id: i64,
        user_id: i32,
        sandbox_id: &str,
        project_id: i32,
    ) {
        let Some(audit) = self.audit.as_ref() else {
            return;
        };
        let public_id = match ai_conversations::Entity::find_by_id(conversation_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(conversation)) => conversation.public_id,
            Ok(None) | Err(_) => conversation_id.to_string(),
        };
        let entry = crate::audit::AiSandboxServicesDisconnectedAudit {
            context: AuditContext {
                user_id,
                ip_address: None,
                user_agent: "temps-ai-assistant".to_string(),
            },
            project_id: Some(project_id),
            service_id: None,
            conversation_id: public_id,
            sandbox_id: sandbox_id.to_string(),
        };
        if let Err(error) = audit.create_audit_log(&entry).await {
            warn!(
                conversation_id,
                sandbox_id,
                project_id,
                "failed to audit the sandbox disconnected from a revoked project: {error}"
            );
        }
    }

    /// The variables a command in the chat's sandbox runs with: empty when it
    /// is linked to no service. `Err` is the complete tool result for a command
    /// that must not run without them.
    async fn exec_environment(
        &self,
        conversation_id: i64,
        link: &SandboxLink,
        user_id: i32,
    ) -> Result<Arc<HashMap<String, String>>, String> {
        let key = credential_cache_key(&link.sandbox_id);
        let fresh = Duration::from_secs(CREDENTIAL_CACHE_SECS);
        if let Some(cached) = self
            .credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .filter(|cached| cached.resolved.elapsed() < fresh)
        {
            return Ok(cached.variables.clone());
        }
        let runtime = self
            .sandboxes
            .scoped_runtime_variables(user_id, &link.sandbox_id)
            .await
            .map_err(|error| {
                warn!(
                    conversation_id,
                    sandbox_id = %link.sandbox_id,
                    "could not prepare the sandbox's database credentials: {error}"
                );
                format!(
                    "Nothing ran: the database credentials for this sandbox could not be \
                     prepared ({error}). Tell the user; they can disconnect the sandbox from \
                     the project's services in the chat's Made here tab."
                )
            })?;
        let variables = Arc::new(runtime.variables);
        self.credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                key,
                CachedCredentials {
                    resolved: Instant::now(),
                    variables: variables.clone(),
                },
            );
        Ok(variables)
    }

    /// Record that the user has been offered a sandbox. Returns `false` when
    /// they already had been.
    pub async fn mark_offered(&self, conversation_id: i64) -> Result<bool, SandboxLinkError> {
        let Some(conversation) = ai_conversations::Entity::find_by_id(conversation_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| SandboxLinkError::Database {
                conversation_id: conversation_id.to_string(),
                source,
            })?
        else {
            return Ok(false);
        };
        if offered_in_metadata(conversation.metadata.as_ref()) {
            return Ok(false);
        }
        self.merge_metadata_key(
            &conversation,
            SANDBOX_OFFERED_KEY,
            &json!(Utc::now().to_rfc3339()),
        )
        .await?;
        Ok(true)
    }

    /// The link of an owned conversation, read fresh from the database.
    async fn current_link(
        &self,
        conversation_id: i64,
        user_id: i32,
    ) -> Result<Option<SandboxLink>, DbErr> {
        let row = ai_conversations::Entity::find_by_id(conversation_id)
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?;
        Ok(row.and_then(|row| link_from_metadata(row.metadata.as_ref())))
    }

    /// Run one sandbox tool call from the model and return the text it reads
    /// back. Failures are sentences written to be acted on, never panics.
    pub async fn dispatch(
        &self,
        tool: &str,
        arguments: &str,
        conversation_id: i64,
        auth: &AuthContext,
    ) -> String {
        let user_id = auth.user_id();
        if tool == SANDBOX_OFFER_TOOL {
            return self.dispatch_offer(arguments, conversation_id).await;
        }
        if tool == SANDBOX_CREATE_TOOL {
            return self.dispatch_create(arguments, conversation_id, auth).await;
        }
        if !auth.has_permission(&Permission::SandboxesExec) {
            return format!(
                "`{tool}` is not available: running commands in a sandbox needs the {} permission.",
                Permission::SandboxesExec
            );
        }
        let ReadySandbox {
            link,
            work_dir,
            notice,
        } = match self.ready_sandbox(tool, conversation_id, user_id).await {
            Ok(ready) => ready,
            Err(message) => return message,
        };
        let notice = match self.drop_revoked_links(conversation_id, &link, auth).await {
            Ok(None) => notice,
            Ok(Some(revoked)) => Some(match notice {
                Some(earlier) => format!("{earlier}\n{revoked}"),
                None => revoked,
            }),
            Err(message) => return message,
        };
        let result = match tool {
            SANDBOX_EXEC_TOOL => {
                match self.exec_environment(conversation_id, &link, user_id).await {
                    Ok(environment) => {
                        self.run_exec(
                            arguments,
                            conversation_id,
                            &link,
                            &work_dir,
                            user_id,
                            &environment,
                        )
                        .await
                    }
                    Err(message) => message,
                }
            }
            SANDBOX_READ_TOOL => {
                // A file can hold what a command wrote from the environment
                // (`.env`), so the read is redacted like command output.
                match self.exec_environment(conversation_id, &link, user_id).await {
                    Ok(environment) => {
                        self.run_read(
                            arguments,
                            conversation_id,
                            &link,
                            &work_dir,
                            user_id,
                            &environment,
                        )
                        .await
                    }
                    Err(_) => "Nothing was read: the credentials of the services this \
                               sandbox is linked to could not be prepared, so a file holding \
                               them could not be redacted. Tell the user; they can disconnect \
                               the sandbox from the project's services in the chat's Made here \
                               tab."
                        .to_string(),
                }
            }
            SANDBOX_WRITE_TOOL => {
                self.run_write(arguments, conversation_id, &link, &work_dir, user_id)
                    .await
            }
            SANDBOX_PREVIEW_TOOL => {
                self.run_preview(arguments, conversation_id, &link, user_id)
                    .await
            }
            other => format!("Unknown sandbox tool `{other}`."),
        };
        with_notice(notice, result)
    }

    /// The chat's linked sandbox, checked and awake, or the complete tool
    /// result that says why it is not. Ownership and linkability are checked
    /// again on every call: the sandbox may have been destroyed or handed to a
    /// run since linking.
    pub(crate) async fn ready_sandbox(
        &self,
        tool: &str,
        conversation_id: i64,
        user_id: i32,
    ) -> Result<ReadySandbox, String> {
        let link = match self.current_link(conversation_id, user_id).await {
            Ok(Some(link)) => link,
            Ok(None) => {
                return Err(format!(
                    "`{tool}` is not available: no sandbox is linked to this chat. Create one \
                     with `{SANDBOX_CREATE_TOOL}` if you have it, otherwise offer one with \
                     `{SANDBOX_OFFER_TOOL}`, when a shell step is really needed."
                ))
            }
            Err(error) => {
                warn!(conversation_id, "could not read the sandbox link: {error}");
                return Err(format!(
                    "`{tool}` failed: could not read this chat's sandbox link: {error}"
                ));
            }
        };
        let row = match self
            .sandboxes
            .find_by_public_id(&link.sandbox_id, user_id)
            .await
        {
            Ok(row) => row,
            Err(_) => {
                return Err(format!(
                    "The linked sandbox {} no longer exists. Ask the user to link another one.",
                    link.sandbox_id
                ))
            }
        };
        if let Some(reason) = linkability_problem(&row.name, row.agent_run_id, &row.status) {
            return Err(format!(
                "The linked sandbox {} can no longer be used: {reason}.",
                link.sandbox_id
            ));
        }
        let notice = if readiness(&row.status) == SandboxReadiness::Asleep {
            Some(self.wake(conversation_id, &link, user_id).await?)
        } else {
            None
        };
        Ok(ReadySandbox {
            link,
            work_dir: row.work_dir,
            notice,
        })
    }

    /// Wake a sleeping linked sandbox. `Ok` is the notice that goes in front
    /// of the tool result, which is how the user sees in the chat that it
    /// happened; `Err` is the complete tool result for a sandbox that stayed
    /// down.
    async fn wake(
        &self,
        conversation_id: i64,
        link: &SandboxLink,
        user_id: i32,
    ) -> Result<String, String> {
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(RESUME_WAIT_SECS),
            self.sandboxes.resume_sandbox(&link.sandbox_id, user_id),
        )
        .await;
        // Another caller (the console, or the turn's own warm-up) may have
        // started it first and made this resume lose the status check. The
        // sandbox is up, which is all this caller wanted.
        let outcome = match outcome {
            Ok(Err(error)) if !matches!(error, SandboxError::NotFound { .. }) => {
                match self
                    .sandboxes
                    .find_by_public_id(&link.sandbox_id, user_id)
                    .await
                {
                    Ok(row) if row.status == "running" => Ok(Ok(row)),
                    _ => Ok(Err(error)),
                }
            }
            other => other,
        };
        match outcome {
            Ok(Ok(_)) => {
                let seconds = started.elapsed().as_secs();
                info!(
                    conversation_id,
                    sandbox_id = %link.sandbox_id,
                    seconds,
                    "resumed the chat's sleeping sandbox"
                );
                self.reconnect_after_wake(conversation_id, link, user_id)
                    .await;
                Ok(format!(
                    "Sandbox {} was asleep and has been resumed (took {seconds}s). Its files \
                     are intact. Mention in one short sentence that you woke it.",
                    link.name
                ))
            }
            Ok(Err(SandboxError::NotFound { .. })) => {
                Err(self.container_gone(conversation_id, link).await)
            }
            Ok(Err(error)) => {
                warn!(
                    conversation_id,
                    sandbox_id = %link.sandbox_id,
                    "could not resume the chat's sandbox: {error}"
                );
                Err(format!(
                    "The linked sandbox {} is asleep and could not be resumed: {error}. Tell the \
                     user, and that they can press Resume on the Sandboxes page. Do not move the \
                     work to Git providers or other places because of this.",
                    link.name
                ))
            }
            Err(_) => {
                warn!(
                    conversation_id,
                    sandbox_id = %link.sandbox_id,
                    "the chat's sandbox did not resume within {RESUME_WAIT_SECS}s"
                );
                Err(format!(
                    "The linked sandbox {} is asleep and did not resume within {RESUME_WAIT_SECS} \
                     seconds, so the container runtime on this server may be unhealthy. Tell the \
                     user, and that they can press Resume on the Sandboxes page and then ask \
                     again. Do not move the work to Git providers or other places because of \
                     this.",
                    link.name
                ))
            }
        }
    }

    /// Put a woken sandbox back on the data network of the services it was
    /// connected to. Best effort: a failure surfaces when a command needs them.
    async fn reconnect_after_wake(&self, conversation_id: i64, link: &SandboxLink, user_id: i32) {
        let linked = match self
            .sandboxes
            .linked_services(user_id, &link.sandbox_id)
            .await
        {
            Ok(linked) if !linked.is_empty() => linked,
            Ok(_) => return,
            Err(error) => {
                warn!(
                    conversation_id,
                    "could not read the sandbox's service links: {error}"
                );
                return;
            }
        };
        if let Err(error) = self
            .sandboxes
            .link_services(user_id, &link.sandbox_id, &linked)
            .await
        {
            warn!(
                conversation_id,
                sandbox_id = %link.sandbox_id,
                "could not reconnect the woken sandbox to its services: {error}"
            );
        }
    }

    /// The sandbox row exists but its container does not, so it can never run
    /// again. Unlink it, so the chat can get a working one, and say so.
    async fn container_gone(&self, conversation_id: i64, link: &SandboxLink) -> String {
        match ai_conversations::Entity::find_by_id(conversation_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(conversation)) => {
                if let Err(error) = self.unlink(&conversation).await {
                    warn!(
                        conversation_id,
                        "could not unlink a vanished sandbox: {error}"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => warn!(
                conversation_id,
                "could not load the conversation to unlink a vanished sandbox: {error}"
            ),
        }
        warn!(
            conversation_id,
            sandbox_id = %link.sandbox_id,
            "the chat's sandbox has no container any more; unlinked it"
        );
        format!(
            "The linked sandbox {} ({}) has no container on this server any more, so it cannot be \
             resumed or used. It has been unlinked from this chat. Tell the user in one sentence \
             and ask them to send their request again: a new sandbox is created for it. Do not \
             move the work to Git providers or other places because of this.",
            link.name, link.sandbox_id
        )
    }

    /// A sandbox tool failed. A sandbox that is gone is handled like one that
    /// cannot be woken; anything else is reported as it happened.
    pub(crate) async fn tool_failure(
        &self,
        what: &str,
        error: SandboxError,
        conversation_id: i64,
        link: &SandboxLink,
    ) -> String {
        if matches!(error, SandboxError::NotFound { .. }) {
            return self.container_gone(conversation_id, link).await;
        }
        format!("{what}: {error}")
    }

    /// Create a sandbox for the chat on the model's request. The sandbox is
    /// always new and always owned by the caller, so the model never chooses
    /// which sandbox it gets.
    async fn dispatch_create(
        &self,
        arguments: &str,
        conversation_id: i64,
        auth: &AuthContext,
    ) -> String {
        let user_id = auth.user_id();
        let reason = serde_json::from_str::<OfferArgs>(arguments)
            .map(|args| args.reason)
            .unwrap_or_default();
        for permission in [Permission::SandboxesExec, Permission::SandboxesWrite] {
            if !auth.has_permission(&permission) {
                return format!(
                    "`{SANDBOX_CREATE_TOOL}` is not available: creating a sandbox needs the \
                     {permission} permission. Tell the user, and carry on without a shell."
                );
            }
        }
        let conversation = match ai_conversations::Entity::find_by_id(conversation_id)
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(conversation)) => conversation,
            Ok(None) => {
                return format!(
                    "`{SANDBOX_CREATE_TOOL}` failed: conversation {conversation_id} was not found."
                )
            }
            Err(error) => {
                warn!(conversation_id, "could not load the conversation: {error}");
                return format!(
                    "`{SANDBOX_CREATE_TOOL}` failed: could not load this chat: {error}"
                );
            }
        };
        if !context_supports_sandbox(&conversation.context_type) {
            return format!("`{SANDBOX_CREATE_TOOL}` is not available in this kind of chat.");
        }
        if let Some(refusal) = create_refused_by_permission_mode(&conversation.ai_permission_mode) {
            info!(
                conversation_id,
                permission_mode = %conversation.ai_permission_mode,
                "refused a sandbox creation the chat's permission mode does not allow"
            );
            return refusal;
        }
        if project_for_new_sandbox(&conversation).is_some()
            && !auth.has_permission(&Permission::GitRepositoriesRead)
        {
            return format!(
                "`{SANDBOX_CREATE_TOOL}` is not available: cloning the project's repository \
                 needs the {} permission.",
                Permission::GitRepositoriesRead
            );
        }
        if let Some(link) = link_from_metadata(conversation.metadata.as_ref()) {
            return format!(
                "A sandbox is already linked to this chat: {} ({}). Use the sandbox tools on it.",
                link.name, link.sandbox_id
            );
        }
        match self
            .create_and_link(&conversation, user_id, Some(reason))
            .await
        {
            Ok(link) => {
                let seeded = if project_for_new_sandbox(&conversation).is_some() {
                    "It starts as a clone of the project's repository."
                } else {
                    "It is empty."
                };
                format!(
                    "Created sandbox {} ({}) and linked it to this chat. {seeded} Use \
                     `{SANDBOX_EXEC_TOOL}`, `{SANDBOX_READ_TOOL}` and `{SANDBOX_WRITE_TOOL}` on it \
                     now. Tell the user in one sentence that you created it.",
                    link.name, link.sandbox_id
                )
            }
            Err(error) => {
                warn!(
                    conversation_id,
                    "could not create a sandbox for the chat: {error}"
                );
                format!(
                    "`{SANDBOX_CREATE_TOOL}` failed: {error}. Tell the user why and carry on \
                     without a shell; they can link a sandbox themselves from the chat's \
                     Sandbox button."
                )
            }
        }
    }

    async fn audit_created(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        sandbox_id: &str,
        reason: &str,
    ) {
        let Some(audit) = self.audit.as_ref() else {
            return;
        };
        let entry = crate::audit::AiSandboxLinkedAudit {
            context: AuditContext {
                user_id,
                ip_address: None,
                user_agent: "temps-ai-assistant".to_string(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            sandbox_id: sandbox_id.to_string(),
            created: true,
        };
        if let Err(error) = audit.create_audit_log(&entry).await {
            warn!(
                conversation_id = %conversation.public_id,
                sandbox_id,
                reason,
                "failed to audit the sandbox the assistant created: {error}"
            );
        }
    }

    async fn dispatch_offer(&self, arguments: &str, conversation_id: i64) -> String {
        let args: OfferArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => {
                return format!(
                "`{SANDBOX_OFFER_TOOL}` needs {{\"reason\": \"…\"}}: {error}. Nothing was offered."
            )
            }
        };
        match self.mark_offered(conversation_id).await {
            Ok(true) => format!(
                "The user was shown an offer to create or link a sandbox so you can: {}. Tell \
                 them in one sentence what you want to do there and stop. Do not run shell \
                 steps until the sandbox tools appear.",
                args.reason.trim()
            ),
            Ok(false) => "A sandbox was already offered in this chat. Do not offer again; \
                continue with the platform tools or tell the user what you could do with a \
                sandbox if they link one."
                .to_string(),
            Err(error) => format!("`{SANDBOX_OFFER_TOOL}` failed: {error}. Nothing was offered."),
        }
    }

    async fn run_exec(
        &self,
        arguments: &str,
        conversation_id: i64,
        link: &SandboxLink,
        work_dir: &str,
        user_id: i32,
        environment: &HashMap<String, String>,
    ) -> String {
        let args: ExecArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => {
                return format!(
                    "`{SANDBOX_EXEC_TOOL}` needs {{\"command\": \"…\"}}: {error}. Nothing ran."
                )
            }
        };
        if args.command.trim().is_empty() {
            return format!("`{SANDBOX_EXEC_TOOL}` needs a non-empty command. Nothing ran.");
        }
        if let Some(service) = third_party_exposure(&args.command) {
            return format!(
                "Nothing ran: `{service}` would publish the sandbox through a third party, and \
                 Temps does not use one. Use `{SANDBOX_PREVIEW_TOOL}` with the server's port to \
                 get the address the user opens (it is served by Temps; the `show-a-running-app` \
                 playbook has the steps), or deploy the code as a Temps project to publish it. If the user asked for that service by name, tell \
                 them this sandbox does not run it."
            );
        }
        let timeout = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS)
                .clamp(1, MAX_EXEC_TIMEOUT_SECS),
        );
        let options = ExecOptions {
            cmd: vec!["sh".to_string(), "-c".to_string(), args.command],
            env: environment.clone(),
            cwd: Some(
                args.cwd
                    .map(|cwd| resolve_path(work_dir, &cwd))
                    .unwrap_or_else(|| work_dir.to_string()),
            ),
        };
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            timeout,
            self.sandboxes.exec(&link.sandbox_id, user_id, options),
        )
        .await;
        match outcome {
            Err(_) => format!(
                "The command did not finish within {} seconds and was abandoned; it may still \
                 be running in the sandbox. Start long-lived servers in the background with \
                 `nohup … &` and check them with a second command.",
                timeout.as_secs()
            ),
            Ok(Err(error)) => {
                self.tool_failure("The command could not run", error, conversation_id, link)
                    .await
            }
            Ok(Ok(result)) => {
                info!(
                    sandbox_id = %link.sandbox_id,
                    exit_code = result.exit_code,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "assistant chat ran a command in its linked sandbox"
                );
                // The address a server started in the sandbox answers on. The
                // model must quote this one: the gateway behind it only
                // accepts the Temps proxy, so any other host or port it
                // invents is refused.
                let preview_url = self
                    .sandboxes
                    .preview_parts()
                    .await
                    .host_template(&link.sandbox_id);
                json!({
                    "exit_code": result.exit_code,
                    "stdout": cap_tail(
                        &redact_secrets(&result.stdout, environment),
                        MAX_TOOL_OUTPUT_BYTES / 2,
                    ),
                    "stderr": cap_tail(
                        &redact_secrets(&result.stderr, environment),
                        MAX_TOOL_OUTPUT_BYTES / 2,
                    ),
                    "preview_url_template": preview_url,
                    "preview_note": format!(
                        "To show the user a page, call `{SANDBOX_PREVIEW_TOOL}` with its port: it \
                         checks the server and opens the page in the chat's Preview tab. Do not \
                         fill in the template yourself."
                    ),
                })
                .to_string()
            }
        }
    }

    async fn run_preview(
        &self,
        arguments: &str,
        conversation_id: i64,
        link: &SandboxLink,
        user_id: i32,
    ) -> String {
        let args: PreviewArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => {
                return format!("`{SANDBOX_PREVIEW_TOOL}` needs {{\"port\": 3000}}: {error}.")
            }
        };
        let Some(port) = args.port.filter(|port| *port >= 1) else {
            return format!("`{SANDBOX_PREVIEW_TOOL}` needs a port between 1 and 65535.");
        };
        // One command answers both questions: where the port listens, and
        // whether the server answers on it.
        let probe = format!(
            "cat /proc/net/tcp /proc/net/tcp6 2>/dev/null; echo '{PROBE_SEPARATOR}'; \
             curl -s -m 5 -o /dev/null -w '%{{http_code}}' http://127.0.0.1:{port}/ 2>/dev/null \
             || curl -s -m 5 -o /dev/null -w '%{{http_code}}' 'http://[::1]:{port}/' 2>/dev/null; \
             true"
        );
        let options = ExecOptions {
            cmd: vec!["sh".to_string(), "-c".to_string(), probe],
            env: Default::default(),
            cwd: None,
        };
        let output = match tokio::time::timeout(
            Duration::from_secs(20),
            self.sandboxes.exec(&link.sandbox_id, user_id, options),
        )
        .await
        {
            Err(_) => {
                return "Could not check the server: the sandbox did not answer within 20 seconds."
                    .to_string()
            }
            Ok(Err(error)) => {
                return self
                    .tool_failure("Could not check the server", error, conversation_id, link)
                    .await
            }
            Ok(Ok(result)) => result.stdout,
        };
        let (table, status) = output
            .split_once(PROBE_SEPARATOR)
            .unwrap_or((output.as_str(), ""));
        let status = status.trim().parse::<u16>().ok().filter(|code| *code != 0);
        let scope = listen_scope(table, port);
        let url = self
            .sandboxes
            .preview_parts()
            .await
            .url_for(&link.sandbox_id, port);
        if scope == ListenScope::Reachable && status.is_some() {
            self.remember_preview(conversation_id, port).await;
        }
        preview_report(&url, port, scope, status)
    }

    /// Record the port the assistant just confirmed, for the Preview tab. The
    /// tool result does not depend on it, so a failure is logged and dropped.
    async fn remember_preview(&self, conversation_id: i64, port: u16) {
        let conversation = match ai_conversations::Entity::find_by_id(conversation_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(conversation)) => conversation,
            Ok(None) => return,
            Err(error) => {
                warn!(conversation_id, port, error = %error, "could not load the chat to record its preview port");
                return;
            }
        };
        if let Err(error) = self
            .merge_metadata_key(&conversation, SANDBOX_PREVIEW_KEY, &json!(port))
            .await
        {
            warn!(conversation_id, port, error = %error, "could not record the chat's preview port");
        }
    }

    async fn run_read(
        &self,
        arguments: &str,
        conversation_id: i64,
        link: &SandboxLink,
        work_dir: &str,
        user_id: i32,
        environment: &HashMap<String, String>,
    ) -> String {
        let args: ReadArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => {
                return format!("`{SANDBOX_READ_TOOL}` needs {{\"path\": \"…\"}}: {error}.")
            }
        };
        let path = resolve_path(work_dir, &args.path);
        // Never buffer more than MAX_READ_FILE_BYTES of the file: the read
        // stops early (and fails) on a bigger one.
        match self
            .sandboxes
            .fs_read_bounded(&link.sandbox_id, user_id, &path, MAX_READ_FILE_BYTES as u64)
            .await
        {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => read_window(
                    &path,
                    &redact_secrets(&text, environment),
                    args.offset,
                    args.limit,
                    MAX_READ_BYTES,
                ),
                Err(error) => format!(
                    "{path} is not UTF-8 text ({} bytes). Inspect it with `{SANDBOX_EXEC_TOOL}` \
                     (for example `file` or `head -c 200 | xxd`).",
                    error.as_bytes().len()
                ),
            },
            // Most likely a file over the limit: show its start instead.
            Err(error @ SandboxError::Validation { .. }) => {
                match self.read_head(link, user_id, &path).await {
                    Some((file_bytes, head)) => {
                        if head.contains(['\0', '\u{FFFD}']) {
                            format!(
                                "{path} is {file_bytes} bytes and does not look like UTF-8 text. \
                                 Inspect it with `{SANDBOX_EXEC_TOOL}` (for example `file` or \
                                 `head -c 200 | xxd`)."
                            )
                        } else {
                            read_head_window(
                                &path,
                                &redact_secrets(&head, environment),
                                file_bytes,
                                args.offset,
                                args.limit,
                                MAX_READ_BYTES,
                            )
                        }
                    }
                    None => {
                        self.tool_failure(
                            &format!("Could not read {path}"),
                            error,
                            conversation_id,
                            link,
                        )
                        .await
                    }
                }
            }
            Err(error) => {
                self.tool_failure(
                    &format!("Could not read {path}"),
                    error,
                    conversation_id,
                    link,
                )
                .await
            }
        }
    }

    /// The size and first [`MAX_READ_FILE_BYTES`] bytes of a regular file
    /// larger than that, read with `head -c` so the rest is never copied out
    /// of the sandbox. `None` when the path is not such a file or the read
    /// failed; the caller then reports its original error.
    async fn read_head(
        &self,
        link: &SandboxLink,
        user_id: i32,
        path: &str,
    ) -> Option<(u64, String)> {
        let options = ExecOptions {
            cmd: vec!["sh".to_string(), "-c".to_string(), read_head_command(path)],
            env: HashMap::new(),
            cwd: None,
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(READ_HEAD_TIMEOUT_SECS),
            self.sandboxes.exec(&link.sandbox_id, user_id, options),
        )
        .await;
        match outcome {
            Ok(Ok(result)) if result.exit_code == 0 => {
                parse_head_output(&result.stdout).map(|(size, head)| (size, head.to_string()))
            }
            Ok(Ok(result)) => {
                debug!(
                    sandbox_id = %link.sandbox_id,
                    path,
                    exit_code = result.exit_code,
                    "not reading the start of a file: it is not a regular file over the read limit"
                );
                None
            }
            Ok(Err(error)) => {
                warn!(sandbox_id = %link.sandbox_id, path, "could not read the start of a large file: {error}");
                None
            }
            Err(_) => {
                warn!(
                    sandbox_id = %link.sandbox_id,
                    path,
                    "reading the start of a large file did not finish within {READ_HEAD_TIMEOUT_SECS}s"
                );
                None
            }
        }
    }

    async fn run_write(
        &self,
        arguments: &str,
        conversation_id: i64,
        link: &SandboxLink,
        work_dir: &str,
        user_id: i32,
    ) -> String {
        let args: WriteArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => {
                return format!(
                    "`{SANDBOX_WRITE_TOOL}` needs {{\"path\": \"…\", \"content\": \"…\"}}: {error}. Nothing was written."
                )
            }
        };
        if args.content.len() > MAX_WRITE_BYTES {
            return format!(
                "The content is {} bytes; `{SANDBOX_WRITE_TOOL}` accepts at most {MAX_WRITE_BYTES}. \
                 Split the file or generate it with `{SANDBOX_EXEC_TOOL}`. Nothing was written.",
                args.content.len()
            );
        }
        let path = resolve_path(work_dir, &args.path);
        let mode = if args.executable { 0o755 } else { 0o644 };
        match self
            .sandboxes
            .fs_write(
                &link.sandbox_id,
                user_id,
                &path,
                args.content.as_bytes(),
                mode,
            )
            .await
        {
            Ok(()) => format!("Wrote {} bytes to {path}.", args.content.len()),
            Err(error) => {
                self.tool_failure(
                    &format!("Could not write {path}"),
                    error,
                    conversation_id,
                    link,
                )
                .await
            }
        }
    }
}

impl SandboxLinkService {
    /// Copy attached files into the chat's linked sandbox so shell tools can
    /// reach them. Returns the sandbox path of each file, or `None` for a file
    /// that was not placed: no sandbox is linked, it could not be woken, or the
    /// write failed. Callers must only tell the model about the paths returned.
    pub async fn place_attachments(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
        files: &[SandboxFile<'_>],
    ) -> Vec<Option<String>> {
        if files.is_empty() {
            return Vec::new();
        }
        let ReadySandbox { link, work_dir, .. } = match self
            .ready_sandbox("attachments", conversation.id, user_id)
            .await
        {
            Ok(ready) => ready,
            Err(reason) => {
                debug!(
                    conversation_id = conversation.id,
                    "attachments stay out of the sandbox: {reason}"
                );
                return vec![None; files.len()];
            }
        };
        let mut placed = Vec::with_capacity(files.len());
        for file in files {
            let path =
                attachment_sandbox_path(&work_dir, &conversation.public_id, file.id, file.name);
            match self
                .sandboxes
                .fs_write(&link.sandbox_id, user_id, &path, file.bytes, 0o644)
                .await
            {
                Ok(()) => placed.push(Some(path)),
                Err(error) => {
                    warn!(
                        conversation_id = conversation.id,
                        sandbox_id = %link.sandbox_id,
                        attachment_id = file.id,
                        "could not copy attachment to {path}: {error}"
                    );
                    placed.push(None);
                }
            }
        }
        placed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(variables: &[(&str, &str)]) -> CachedCredentials {
        CachedCredentials {
            resolved: Instant::now(),
            variables: Arc::new(
                variables
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
            ),
        }
    }

    #[test]
    fn forgetting_a_sandbox_drops_the_credentials_cached_for_it() {
        let mut cache = HashMap::new();
        cache.insert(
            credential_cache_key("sbx_one"),
            cached(&[("POSTGRES_PASSWORD", "hunter2-secret")]),
        );
        cache.insert("sbx_one:extra".to_string(), cached(&[]));
        cache.insert(credential_cache_key("sbx_one2"), cached(&[]));
        cache.insert(credential_cache_key("sbx_two"), cached(&[]));

        forget_cached_credentials(&mut cache, "sbx_one");

        let mut left: Vec<&String> = cache.keys().collect();
        left.sort();
        assert_eq!(left, ["sbx_one2", "sbx_two"]);
    }

    #[test]
    fn a_read_redacts_the_linked_services_credentials() {
        let environment: HashMap<String, String> = [
            (
                "DATABASE_URL".to_string(),
                "postgres://sbx:pa55word-xyz@db:5432/app".to_string(),
            ),
            ("POSTGRES_PASSWORD".to_string(), "pa55word-xyz".to_string()),
        ]
        .into_iter()
        .collect();
        let file = "DATABASE_URL=postgres://sbx:pa55word-xyz@db:5432/app\nPORT=3000\n";
        let shown = read_window(
            ".env",
            &redact_secrets(file, &environment),
            None,
            None,
            MAX_READ_BYTES,
        );
        assert!(!shown.contains("pa55word-xyz"), "{shown}");
        assert!(shown.contains("DATABASE_URL=[redacted]"), "{shown}");
        assert!(shown.contains("PORT=3000"), "{shown}");
    }

    #[test]
    fn relinking_the_same_sandbox_leaves_nothing_behind() {
        assert_eq!(sandbox_left_behind("sbx_a", Some("sbx_a")), None);
        assert_eq!(sandbox_left_behind("sbx_a", Some("sbx_b")), Some("sbx_a"));
        assert_eq!(sandbox_left_behind("sbx_a", None), Some("sbx_a"));
    }

    #[test]
    fn creating_a_sandbox_is_refused_unless_the_mode_approves_changes() {
        for mode in ["auto", "full-access"] {
            assert_eq!(create_refused_by_permission_mode(mode), None, "{mode}");
        }
        for mode in ["default", "ask", "plan", ""] {
            let refusal = create_refused_by_permission_mode(mode)
                .unwrap_or_else(|| panic!("{mode} should refuse"));
            assert!(refusal.contains(SANDBOX_OFFER_TOOL), "{refusal}");
        }
    }
    use chrono::TimeZone;

    /// A team layer whose members can reach `allowed` only, or that fails.
    struct Membership {
        allowed: Vec<i32>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for Membership {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            if self.fail {
                return Err(Box::new(std::io::Error::other("team store unavailable")));
            }
            Ok(self.allowed.contains(&project_id))
        }
    }

    fn membership(allowed: &[i32]) -> Arc<dyn temps_core::ProjectAccessChecker> {
        Arc::new(Membership {
            allowed: allowed.to_vec(),
            fail: false,
        })
    }

    fn member_session(role: temps_auth::permissions::Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            temps_entities::users::Model {
                id: 11,
                name: "Team Member".to_string(),
                email: "member@example.com".to_string(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: now,
                updated_at: now,
            },
            role,
        )
    }

    /// Links through projects 3 and 5 (twice), and one made to a service
    /// directly.
    fn links() -> Vec<LinkedService> {
        [(1, Some(3)), (2, Some(5)), (4, Some(5)), (6, None)]
            .into_iter()
            .map(|(service_id, project_id)| LinkedService {
                service_id,
                project_id,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_member_removed_from_a_project_loses_the_links_made_through_it() {
        let auth = member_session(temps_auth::permissions::Role::User);
        let checker = membership(&[3]);
        let revoked = revoked_link_projects(&auth, &links(), Some(&checker))
            .await
            .expect("the check completes");
        assert_eq!(revoked, vec![5]);
    }

    #[tokio::test]
    async fn a_member_of_every_linked_project_keeps_them_all() {
        let auth = member_session(temps_auth::permissions::Role::User);
        let checker = membership(&[3, 5]);
        let revoked = revoked_link_projects(&auth, &links(), Some(&checker))
            .await
            .expect("the check completes");
        assert!(revoked.is_empty(), "{revoked:?}");
    }

    #[tokio::test]
    async fn a_failed_membership_check_runs_nothing() {
        let auth = member_session(temps_auth::permissions::Role::User);
        let checker: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(Membership {
            allowed: vec![3, 5],
            fail: true,
        });
        let message = revoked_link_projects(&auth, &links(), Some(&checker))
            .await
            .unwrap_err();
        assert!(message.contains("[3, 5]"), "{message}");
        assert!(message.contains("user 11"), "{message}");
    }

    #[tokio::test]
    async fn instance_admins_are_not_subject_to_team_membership() {
        let auth = member_session(temps_auth::permissions::Role::Admin);
        let checker = membership(&[]);
        let revoked = revoked_link_projects(&auth, &links(), Some(&checker))
            .await
            .expect("the check completes");
        assert!(revoked.is_empty(), "{revoked:?}");
    }

    #[tokio::test]
    async fn without_a_team_layer_there_is_no_membership_to_lose() {
        let auth = member_session(temps_auth::permissions::Role::User);
        let revoked = revoked_link_projects(&auth, &links(), None)
            .await
            .expect("the check completes");
        assert!(revoked.is_empty(), "{revoked:?}");
    }

    #[tokio::test]
    async fn links_made_to_a_service_directly_are_not_project_access() {
        let auth = member_session(temps_auth::permissions::Role::User);
        let checker = membership(&[]);
        let direct = vec![LinkedService {
            service_id: 6,
            project_id: None,
        }];
        let revoked = revoked_link_projects(&auth, &direct, Some(&checker))
            .await
            .expect("nothing to check");
        assert!(revoked.is_empty(), "{revoked:?}");
    }

    fn numbered(lines: usize) -> String {
        (1..=lines).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn a_small_file_is_read_whole_with_no_note() {
        let text = numbered(3);
        assert_eq!(read_window("/w/a.txt", &text, None, None, 4096), text);
    }

    #[test]
    fn a_long_file_is_read_in_pieces_that_say_where_to_continue() {
        let text = numbered(1200);
        let first = read_window("/w/a.txt", &text, None, None, 1 << 20);
        assert!(first.starts_with("[/w/a.txt: lines 1-500 of 1200."));
        assert!(first.contains("offset=501"));
        assert!(first.contains("line 500\n"));
        assert!(!first.contains("line 501\n"));

        let last = read_window("/w/a.txt", &text, Some(1101), Some(500), 1 << 20);
        assert!(last.starts_with("[/w/a.txt: lines 1101-1200 of 1200.]"));
        assert!(!last.contains("offset="));
        assert!(last.ends_with("line 1200\n"));
    }

    #[test]
    fn a_requested_window_is_honoured_and_clamped() {
        let text = numbered(50);
        let window = read_window("/w/a.txt", &text, Some(10), Some(3), 4096);
        assert!(window.starts_with("[/w/a.txt: lines 10-12 of 50. Call"));
        assert!(window.contains("offset=13"));
        assert!(window.ends_with("line 10\nline 11\nline 12\n"));
        // offset 0 and limit 0 are read as 1.
        let clamped = read_window("/w/a.txt", &text, Some(0), Some(0), 4096);
        assert!(clamped.starts_with("[/w/a.txt: lines 1-1 of 50."));
    }

    #[test]
    fn an_offset_past_the_end_says_how_long_the_file_is() {
        let message = read_window("/w/a.txt", &numbered(5), Some(9), None, 4096);
        assert!(message.contains("has 5 lines"));
        assert!(message.contains("offset 9"));
    }

    #[test]
    fn the_byte_budget_stops_a_piece_early_and_points_at_the_next_line() {
        let text = numbered(100);
        let piece = read_window("/w/a.txt", &text, None, Some(100), 40);
        let shown = piece.lines().filter(|l| l.starts_with("line ")).count();
        assert!(shown > 0 && shown < 100);
        assert!(piece.contains(&format!("offset={}", shown + 1)));
    }

    #[test]
    fn one_huge_line_is_cut_instead_of_filling_the_context() {
        let text = format!("{}\nnext\n", "x".repeat(10_000));
        let piece = read_window("/w/min.js", &text, None, None, 100);
        assert!(piece.contains("[line cut"));
        assert!(piece.contains("lines 1-1 of 2"));
        assert!(piece.contains("offset=2"));
        assert!(piece.len() < 400);
    }

    #[test]
    fn the_start_of_an_oversized_file_is_paged_and_says_it_is_only_the_start() {
        let head = numbered(30);
        let first = read_head_window("/w/big.log", &head, 5_000_000, None, Some(10), 4096);
        assert!(first.starts_with("[/w/big.log is 5000000 bytes, larger than the"));
        assert!(first.contains("showing lines 1-10 of the 30 complete lines"));
        assert!(first.contains("offset=11"));
        assert!(first.contains("sed -n '31,530p' '/w/big.log'"));
        assert!(first.ends_with("line 10\n"));

        // The last window of the part still points past it.
        let last = read_head_window("/w/big.log", &head, 5_000_000, Some(25), None, 4096);
        assert!(last.contains("showing lines 25-30 of the 30"));
        assert!(!last.contains("offset="));
        assert!(last.contains("sed -n '31,530p'"));

        // An offset beyond the part is an instruction, not an error.
        let past = read_head_window("/w/big.log", &head, 5_000_000, Some(40), Some(5), 4096);
        assert!(past.contains("offset 40 is past the 30 lines"));
        assert!(past.contains("sed -n '40,44p' '/w/big.log'"));
    }

    #[test]
    fn a_line_the_cut_split_is_left_out() {
        let head = format!("{}partial li", numbered(3));
        let window = read_head_window("/w/big.log", &head, 9_000_000, None, None, 4096);
        assert!(window.contains("of the 3 complete lines"));
        assert!(window.ends_with("line 3\n"));
        assert!(!window.contains("partial"));
        // With no newline at all the single line is kept (and cut by budget).
        let one_line = format!("{}\u{FFFD}", "x".repeat(10_000));
        let window = read_head_window("/w/min.js", &one_line, 9_000_000, None, None, 100);
        assert!(window.contains("[line cut"));
        assert!(!window.contains('\u{FFFD}'));
    }

    #[test]
    fn the_head_output_is_parsed_and_never_longer_than_the_read_limit() {
        assert_eq!(
            parse_head_output("2000000\nhello\n"),
            Some((2_000_000, "hello\n"))
        );
        // Not over the limit after all, or not the expected shape.
        assert_eq!(parse_head_output("10\nhello"), None);
        assert_eq!(parse_head_output("garbage"), None);
        assert_eq!(parse_head_output("x\nhello"), None);
        // A shim that prints more than asked is cut to the limit.
        let flood = format!("3000000\n{}", "é".repeat(MAX_READ_FILE_BYTES));
        let (_, head) = parse_head_output(&flood).unwrap();
        assert!(head.len() <= MAX_READ_FILE_BYTES);
    }

    #[test]
    fn the_head_command_quotes_the_path_and_reads_only_the_limit() {
        let command = read_head_command("/w/it's here");
        assert!(command.starts_with("f='/w/it'\\''s here';"));
        assert!(command.contains(&format!("head -c {MAX_READ_FILE_BYTES} \"$f\"")));
        assert!(command.contains(&format!("-gt {MAX_READ_FILE_BYTES} ]")));
    }

    #[test]
    fn the_read_bound_is_a_small_multiple_of_the_window() {
        const { assert!(MAX_READ_FILE_BYTES >= MAX_READ_BYTES) };
        const { assert!(MAX_READ_FILE_BYTES <= 32 * MAX_READ_BYTES) };
    }

    #[test]
    fn an_empty_file_reads_as_empty() {
        assert_eq!(read_window("/w/a.txt", "", None, None, 4096), "");
    }

    #[test]
    fn attachments_land_under_the_sandbox_work_dir_in_the_workspace_layout() {
        assert_eq!(
            attachment_sandbox_path("/home/temps/workspace/", "conv_1", "att_2", "shot.png"),
            "/home/temps/workspace/.temps/chat-attachments/conv_1/att_2/shot.png"
        );
    }

    #[test]
    fn only_a_stopped_sandbox_needs_waking() {
        assert_eq!(readiness("stopped"), SandboxReadiness::Asleep);
        assert_eq!(readiness("running"), SandboxReadiness::Ready);
    }

    #[test]
    fn a_notice_goes_in_front_of_the_tool_result() {
        let result = with_notice(
            Some("Resumed.".to_string()),
            "{\"exit_code\":0}".to_string(),
        );
        assert_eq!(result, "Resumed.\n\n{\"exit_code\":0}");
        assert_eq!(with_notice(None, "ok".to_string()), "ok");
    }

    #[test]
    fn every_credential_the_engines_return_is_redacted_and_the_key_prefix_is_not() {
        for name in [
            "REDIS_URL",
            "REDIS_PASSWORD",
            "MYSQL_URL",
            "MYSQL_PASSWORD",
            "MARIADB_PASSWORD",
            "DATABASE_URL",
            "MONGODB_URL",
            "MONGODB_PASSWORD",
            "AWS_SESSION_TOKEN",
            "S3_SECRET_KEY",
        ] {
            assert!(is_secret_variable(name), "{name}");
        }
        assert!(!is_secret_variable("REDIS_KEY_PREFIX"));
        assert!(!is_secret_variable("REDIS_USER"));
        let variables = HashMap::from([
            ("REDIS_KEY_PREFIX".to_string(), "sbx:shop:".to_string()),
            ("REDIS_PASSWORD".to_string(), "hunter2hunter2".to_string()),
        ]);
        let shown = redact_secrets("sbx:shop:k hunter2hunter2", &variables);
        assert_eq!(shown, "sbx:shop:k [redacted]");
    }

    #[test]
    fn linking_to_a_service_that_does_not_exist_is_a_validation_error() {
        let chat = conversation("assistant", None);
        let error = SandboxLinkService::connection_error(
            &chat,
            SandboxError::ServiceNotFound { service_id: 999 },
        );
        match error {
            SandboxLinkError::Validation { message, .. } => assert!(message.contains("999")),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn connected_projects_are_the_projects_the_links_were_made_through() {
        let linked = [
            LinkedService {
                service_id: 1,
                project_id: Some(7),
            },
            LinkedService {
                service_id: 2,
                project_id: Some(3),
            },
            LinkedService {
                service_id: 3,
                project_id: Some(7),
            },
            LinkedService {
                service_id: 4,
                project_id: None,
            },
        ];
        assert_eq!(connected_project_ids(&linked), vec![3, 7]);
        assert!(connected_project_ids(&[]).is_empty());
    }

    fn database_variables() -> HashMap<String, String> {
        HashMap::from([
            (
                "POSTGRES_PASSWORD".to_string(),
                "p4ssw0rd-derived".to_string(),
            ),
            (
                "POSTGRES_URL".to_string(),
                "postgresql://sbx_app:p4ssw0rd-derived@postgres-main:5432/app_sandbox".to_string(),
            ),
            ("POSTGRES_USER".to_string(), "sbx_app".to_string()),
            ("POSTGRES_HOST".to_string(), "postgres-main".to_string()),
            ("POSTGRES_PORT".to_string(), "5432".to_string()),
            ("POSTGRES_DB".to_string(), "app_sandbox".to_string()),
        ])
    }

    #[test]
    fn a_command_that_prints_its_environment_does_not_show_the_password() {
        let variables = database_variables();
        let printed = "POSTGRES_PASSWORD=p4ssw0rd-derived\nPOSTGRES_URL=postgresql://sbx_app:p4ssw0rd-derived@postgres-main:5432/app_sandbox\n";
        let shown = redact_secrets(printed, &variables);
        assert!(!shown.contains("p4ssw0rd-derived"), "{shown}");
        assert_eq!(shown.matches("[redacted]").count(), 2);
    }

    #[test]
    fn redaction_leaves_the_non_secret_names_readable() {
        let variables = database_variables();
        let shown = redact_secrets("host postgres-main db app_sandbox port 5432", &variables);
        assert_eq!(shown, "host postgres-main db app_sandbox port 5432");
    }

    #[test]
    fn short_values_are_not_redacted_so_ordinary_text_survives() {
        let variables = HashMap::from([("APP_TOKEN".to_string(), "abc".to_string())]);
        assert_eq!(redact_secrets("abc abc", &variables), "abc abc");
    }

    #[test]
    fn variable_names_are_classified_by_what_they_hold() {
        for secret in [
            "POSTGRES_PASSWORD",
            "POSTGRES_URL",
            "api_key",
            "AUTH_TOKEN",
            "CLIENT_SECRET",
        ] {
            assert!(is_secret_variable(secret), "{secret}");
        }
        for plain in [
            "POSTGRES_HOST",
            "POSTGRES_PORT",
            "POSTGRES_DB",
            "POSTGRES_USER",
        ] {
            assert!(!is_secret_variable(plain), "{plain}");
        }
    }

    #[test]
    fn third_party_tunnels_are_recognised() {
        for command in [
            "./cloudflared tunnel --url http://localhost:3100",
            "curl -L -o cf https://github.com/cloudflare/cloudflared/releases/latest/download/x",
            "npx -y untun@latest tunnel http://localhost:3000",
            "ngrok http 3000",
            "NGROK_AUTHTOKEN=x ngrok http 3000",
            "npx localtunnel --port 3000",
            "ssh -R 80:localhost:3000 serveo.net",
            "ssh -R 80:localhost:3000 nokey@localhost.run",
            "tailscale funnel 3000",
        ] {
            assert!(third_party_exposure(command).is_some(), "{command}");
        }
    }

    #[test]
    fn ordinary_commands_are_not_mistaken_for_tunnels() {
        for command in [
            "npm run dev -- -H 0.0.0.0 -p 3000",
            "curl -s localhost:3000",
            "git clone https://github.com/example/app && cd app && npm install",
            "echo uptuning && ls",
            "grep -r 'ngrokish' .",
        ] {
            assert_eq!(third_party_exposure(command), None, "{command}");
        }
    }

    const PROC_TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11 1 0000000000000000 100 0 0 10 0\n\
   1: 00000000:1388 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12 1 0000000000000000 100 0 0 10 0\n\
   2: 0300A8C0:2710 0100A8C0:C350 01 00000000:00000000 00:00000000 00000000  1000        0 13 1 0000000000000000 100 0 0 10 0\n\
   3: 00000000000000000000000000000000:0C1C 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 14 1 0000000000000000 100 0 0 10 0\n\
   4: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 15 1 0000000000000000 100 0 0 10 0\n";

    #[test]
    fn listen_scope_tells_loopback_from_reachable() {
        // 3000 (0x0BB8) listens on 127.0.0.1 only.
        assert_eq!(listen_scope(PROC_TCP, 3000), ListenScope::LoopbackOnly);
        // 5000 (0x1388) on every IPv4 address.
        assert_eq!(listen_scope(PROC_TCP, 5000), ListenScope::Reachable);
        // 3100 (0x0C1C) on every IPv6 address.
        assert_eq!(listen_scope(PROC_TCP, 3100), ListenScope::Reachable);
        // 8080 (0x1F90) on ::1 only.
        assert_eq!(listen_scope(PROC_TCP, 8080), ListenScope::LoopbackOnly);
        // 10000 (0x2710) is an established connection, not a listener.
        assert_eq!(listen_scope(PROC_TCP, 10000), ListenScope::Nothing);
        assert_eq!(listen_scope(PROC_TCP, 9999), ListenScope::Nothing);
        assert_eq!(listen_scope("", 3000), ListenScope::Nothing);
    }

    #[test]
    fn the_recorded_preview_port_must_be_a_real_port() {
        let port =
            |value: Value| preview_port_from_metadata(Some(&json!({ SANDBOX_PREVIEW_KEY: value })));
        assert_eq!(port(json!(3000)), Some(3000));
        assert_eq!(port(json!(0)), None);
        assert_eq!(port(json!(70000)), None);
        assert_eq!(port(json!("3000")), None);
        assert_eq!(preview_port_from_metadata(None), None);
        assert_eq!(preview_port_from_metadata(Some(&json!({}))), None);
    }

    #[test]
    fn preview_report_explains_every_failure_and_only_blesses_a_working_server() {
        let url = "http://ws-abc-3000.localho.st:8080";
        let ok: Value = serde_json::from_str(&preview_report(
            url,
            3000,
            ListenScope::Reachable,
            Some(200),
        ))
        .unwrap();
        assert_eq!(ok["ready"], true);
        assert_eq!(ok["url"], url);
        for (scope, status, needle) in [
            (ListenScope::Nothing, None, "Nothing is listening"),
            (ListenScope::LoopbackOnly, Some(200), "only on localhost"),
            (ListenScope::Reachable, None, "did not answer"),
        ] {
            let report: Value =
                serde_json::from_str(&preview_report(url, 3000, scope, status)).unwrap();
            assert_eq!(report["ready"], false);
            assert!(report["advice"].as_str().unwrap().contains(needle));
        }
    }

    fn link() -> SandboxLink {
        SandboxLink {
            sandbox_id: "sbx_abc123".to_string(),
            name: "assistant-chat-x".to_string(),
            linked_at: Utc.with_ymd_and_hms(2026, 10, 17, 9, 30, 0).unwrap(),
        }
    }

    fn conversation(context_type: &str, project_id: Option<i32>) -> ai_conversations::Model {
        let now = Utc::now();
        ai_conversations::Model {
            id: 1,
            public_id: "conv_test".to_string(),
            project_id,
            application_id: None,
            context_type: context_type.to_string(),
            context_id: "ctx".to_string(),
            title: None,
            status: "active".to_string(),
            kind: "ai".to_string(),
            created_by: 7,
            metadata: None,
            created_at: now,
            last_activity_at: now,
            ai_provider: "anthropic".to_string(),
            ai_model: "model".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "auto".to_string(),
            cli_session_id: None,
            cli_session_fingerprint: None,
            turn_status: "idle".to_string(),
            active_turn_id: None,
            last_turn_id: None,
            turn_started_at: None,
            next_step_suggestions: None,
        }
    }

    #[test]
    fn only_assistant_and_project_chats_support_sandboxes() {
        assert!(context_supports_sandbox("assistant"));
        assert!(context_supports_sandbox("project"));
        for other in ["deployment", "alert", "application", "global", ""] {
            assert!(!context_supports_sandbox(other), "{other}");
        }
    }

    #[test]
    fn a_new_sandbox_is_seeded_from_the_project_of_a_project_chat_only() {
        assert_eq!(
            project_for_new_sandbox(&conversation("project", Some(42))),
            Some(42)
        );
        assert_eq!(
            project_for_new_sandbox(&conversation("project", None)),
            None
        );
        assert_eq!(
            project_for_new_sandbox(&conversation("assistant", Some(42))),
            None
        );
    }

    #[test]
    fn link_round_trips_through_conversation_metadata() {
        let metadata = json!({
            "context_usage": { "tokens": 10 },
            SANDBOX_LINK_KEY: serde_json::to_value(link()).unwrap(),
        });
        assert_eq!(link_from_metadata(Some(&metadata)), Some(link()));
    }

    #[test]
    fn missing_or_malformed_link_reads_as_unlinked() {
        assert_eq!(link_from_metadata(None), None);
        assert_eq!(link_from_metadata(Some(&json!({}))), None);
        assert_eq!(
            link_from_metadata(Some(&json!({ SANDBOX_LINK_KEY: "sbx_abc123" }))),
            None
        );
        assert_eq!(
            link_from_metadata(Some(&json!({ SANDBOX_LINK_KEY: { "sandbox_id": 4 } }))),
            None
        );
    }

    #[test]
    fn offered_flag_is_read_from_metadata() {
        assert!(!offered_in_metadata(None));
        assert!(!offered_in_metadata(Some(&json!({}))));
        assert!(!offered_in_metadata(Some(
            &json!({ SANDBOX_OFFERED_KEY: null })
        )));
        assert!(offered_in_metadata(Some(
            &json!({ SANDBOX_OFFERED_KEY: "2026-10-17T09:30:00Z" })
        )));
    }

    #[test]
    fn application_workspaces_agent_runs_and_destroyed_sandboxes_are_not_linkable() {
        assert!(linkability_problem("ai-application:global-user-7", None, "running").is_some());
        assert!(linkability_problem("scratch", Some(12), "running").is_some());
        assert!(linkability_problem("scratch", None, "destroyed").is_some());
        assert!(linkability_problem("scratch", None, "running").is_none());
        assert!(linkability_problem("scratch", None, "stopped").is_none());
    }

    #[test]
    fn long_output_keeps_its_tail_on_a_char_boundary() {
        let text = format!("{}end", "é".repeat(100));
        let capped = cap_tail(&text, 11);
        assert!(capped.ends_with("end"));
        assert!(capped.starts_with("[earlier output truncated"));
        assert_eq!(cap_tail("short", 100), "short");
    }

    #[test]
    fn relative_paths_resolve_against_the_work_dir() {
        assert_eq!(
            resolve_path("/workspace", "src/a.rs"),
            "/workspace/src/a.rs"
        );
        assert_eq!(resolve_path("/workspace/", "a"), "/workspace/a");
        assert_eq!(resolve_path("/workspace", "/etc/hosts"), "/etc/hosts");
    }

    fn names(tools: Vec<ChatTool>) -> Vec<String> {
        tools.into_iter().map(|tool| tool.name).collect()
    }

    #[test]
    fn a_linked_chat_gets_the_scoped_tools_and_no_offer() {
        for may_create in [false, true] {
            assert_eq!(
                names(tools_for(true, false, true, true, may_create, false)),
                [
                    SANDBOX_EXEC_TOOL,
                    SANDBOX_READ_TOOL,
                    SANDBOX_WRITE_TOOL,
                    SANDBOX_PREVIEW_TOOL
                ]
            );
        }
    }

    #[test]
    fn deploying_the_sandbox_is_offered_only_to_a_caller_who_may_deploy() {
        let with = names(tools_for(true, false, true, true, false, true));
        assert!(with.contains(&SANDBOX_DEPLOY_TOOL.to_string()));
        let without = names(tools_for(true, false, true, true, false, false));
        assert!(!without.contains(&SANDBOX_DEPLOY_TOOL.to_string()));
        // Usable in the same turn a sandbox is created.
        let creating = names(tools_for(false, false, true, true, true, true));
        assert!(creating.contains(&SANDBOX_DEPLOY_TOOL.to_string()));
        // Never without a sandbox to read from, nor to a caller who cannot exec.
        assert!(tools_for(true, false, true, false, false, true).is_empty());
        assert!(!names(tools_for(false, false, true, true, false, true))
            .contains(&SANDBOX_DEPLOY_TOOL.to_string()));
    }

    #[test]
    fn an_unlinked_chat_gets_only_the_offer_until_it_was_made() {
        assert_eq!(
            names(tools_for(false, false, true, true, false, false)),
            [SANDBOX_OFFER_TOOL]
        );
        assert!(tools_for(false, true, true, true, false, false).is_empty());
    }

    #[test]
    fn an_unlinked_chat_that_may_create_gets_create_and_the_shell_tools_together() {
        // Together, so the sandbox it creates is usable in the same turn.
        for offered in [false, true] {
            assert_eq!(
                names(tools_for(false, offered, true, true, true, false)),
                [
                    SANDBOX_EXEC_TOOL,
                    SANDBOX_READ_TOOL,
                    SANDBOX_WRITE_TOOL,
                    SANDBOX_PREVIEW_TOOL,
                    SANDBOX_CREATE_TOOL
                ]
            );
        }
        assert!(is_sandbox_tool(SANDBOX_CREATE_TOOL));
    }

    #[test]
    fn no_backend_or_no_exec_permission_offers_nothing() {
        for may_create in [false, true] {
            assert!(tools_for(true, false, false, true, may_create, false).is_empty());
            assert!(tools_for(false, false, false, true, may_create, false).is_empty());
            assert!(tools_for(true, false, true, false, may_create, false).is_empty());
            assert!(tools_for(false, false, true, false, may_create, false).is_empty());
        }
    }

    #[test]
    fn error_messages_name_the_conversation_and_sandbox() {
        let error = SandboxLinkError::NotLinkable {
            sandbox_id: "sbx_1".into(),
            conversation_id: "conv_1".into(),
            reason: "it has been destroyed".into(),
        };
        let message = error.to_string();
        assert!(message.contains("sbx_1") && message.contains("conv_1"));
        assert!(SandboxLinkError::NotLinked {
            conversation_id: "conv_2".into()
        }
        .to_string()
        .contains("conv_2"));
    }
}
