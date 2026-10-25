// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Standalone sandbox API.
//!
//! All routes live under `/v1/sandboxes/*`. The surface intentionally
//! mirrors the `@vercel/sandbox` npm SDK so drop-in clients work without
//! code changes beyond base URL + auth (see `tests/vercel_compat.rs` for
//! the pinned contract).
//!
//! Shape contract:
//! - Every handler calls through `SandboxService` — no DB access here.
//! - Errors map through `From<SandboxError> for Problem` (RFC 7807).
//! - IDs in URLs are opaque `sbx_<hex>` strings; the i32 is internal.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post, put},
    Extension, Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use futures::stream::{self, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use temps_agents::sandbox::ExecStream;
use tokio_stream::wrappers::BroadcastStream;
use utoipa::ToSchema;

use temps_auth::{permissions::Permission, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::secret_string::SecretString;
use temps_entities::ingress_mode::SandboxPreviewAccess;

use crate::error::SandboxError;
use crate::handlers::SandboxAppState;

/// Reject the request unless the caller carries a sandbox-specific scope
/// *or* the legacy project-scoped permission.
///
/// Two-tier check lets existing tokens (issued before sandbox scopes
/// existed, which only have `projects:read` / `projects:write`) keep
/// working while new sandbox-only API tokens don't need broader project
/// access just to manage sandboxes. The fallback is the quiet-deprecation
/// path: operators can migrate tokens without breaking integrations.
/// Gate host-global operations (rootfs inventory / GC) on admin. These
/// expose or mutate storage across every tenant, so ordinary sandbox
/// scopes are not enough.
pub(crate) fn require_sandbox_admin(
    auth: &temps_auth::context::AuthContext,
) -> Result<(), Problem> {
    if auth.is_admin() {
        return Ok(());
    }
    Err(
        temps_core::error_builder::ErrorBuilder::new(axum::http::StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Insufficient Permissions")
            .detail("This operation requires an administrator role")
            .value("user_role", auth.effective_role.to_string())
            .build(),
    )
}

pub(crate) fn sandbox_permission_guard(
    auth: &temps_auth::context::AuthContext,
    primary: Permission,
    fallback: Permission,
) -> Result<(), Problem> {
    if auth.has_permission(&primary) || auth.has_permission(&fallback) {
        return Ok(());
    }
    Err(
        temps_core::error_builder::ErrorBuilder::new(axum::http::StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Insufficient Permissions")
            .detail(format!(
                "This operation requires the {} permission",
                primary
            ))
            .value("required_permission", primary.to_string())
            .value("accepted_fallback", fallback.to_string())
            .value("user_role", auth.effective_role.to_string())
            .build(),
    )
}
use crate::services::exec::{ExecOptions, ExecResult};
use crate::services::fs::StatInfo;
use crate::services::job_tracker::{JobState, JobStatus};
use crate::services::sandbox_service::{
    CreateSandboxRequest, SandboxLifecycle, SandboxSource, SandboxSummary,
};

// ── Error → Problem conversion ──────────────────────────────────────────────

impl From<SandboxError> for Problem {
    fn from(error: SandboxError) -> Self {
        match error {
            SandboxError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Sandbox Not Found")
                .with_detail(error.to_string()),
            SandboxError::VmNotRunning { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_type("https://temps.sh/probs/sandbox-vm-not-running")
                .with_title("Sandbox VM Not Running")
                .with_detail(error.to_string()),
            SandboxError::JobNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Job Not Found")
                .with_detail(error.to_string()),
            SandboxError::CreateFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Sandbox Creation Failed")
                    .with_detail(error.to_string())
            }
            SandboxError::ImageUnavailable {
                ref sandbox_id,
                ref image,
                ref reason,
            } => {
                // The registry's own error text can carry hostnames and request
                // detail, so it goes to the log; the response names the image.
                tracing::warn!(
                    sandbox_id = %sandbox_id,
                    image = %image,
                    reason = %reason,
                    "sandbox image could not be pulled"
                );
                let detail = format!(
                    "Sandbox image '{image}' could not be pulled for sandbox {sandbox_id}. \
                     Check that this host can reach the image registry and that the \
                     tag is published; an administrator can pre-load it with `docker pull {image}`."
                );
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_type("https://temps.sh/probs/sandbox-image-unavailable")
                    .with_title("Sandbox Image Unavailable")
                    .with_detail(detail)
            }
            SandboxError::NetworkCapacityExhausted { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_type("https://temps.sh/probs/sandbox-network-capacity-exhausted")
                    .with_title("Sandbox Network Capacity Exhausted")
                    .with_detail(error.to_string())
            }
            SandboxError::ExecFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Sandbox Exec Failed")
                    .with_detail(error.to_string())
            }
            SandboxError::FileOp { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Sandbox FS Operation Failed")
                .with_detail(error.to_string()),
            SandboxError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            SandboxError::InvalidState { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Invalid Sandbox State")
                .with_detail(error.to_string()),
            SandboxError::ManagedByAgentRun { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Sandbox Managed By Agent Run")
                .with_detail(error.to_string()),
            SandboxError::ProjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(error.to_string()),
            SandboxError::ProjectHasNoRepo { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Project Has No Repository")
                .with_detail(error.to_string()),
            SandboxError::Timeout { .. } => problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                .with_title("Sandbox Operation Timed Out")
                .with_detail(error.to_string()),
            SandboxError::Unavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Sandbox Subsystem Unavailable")
                    .with_detail(error.to_string())
            }
            SandboxError::NodeUnreachable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_type("https://temps.sh/probs/sandbox-node-unreachable")
                    .with_title("Sandbox Node Unreachable")
                    .with_detail(error.to_string())
            }
            SandboxError::NodeConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_type("https://temps.sh/probs/sandbox-node-conflict")
                .with_title("Sandbox Conflict On Worker Node")
                .with_detail(error.to_string()),
            SandboxError::UnsupportedOnWorkerNode { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-unsupported-on-worker-node")
                    .with_title("Not Available For Sandboxes On Worker Nodes")
                    .with_detail(error.to_string())
            }
            SandboxError::EgressPolicyUnsupported { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-egress-policy-unsupported")
                    .with_title("Egress Policy Not Supported By This Backend")
                    .with_detail(error.to_string())
            }
            SandboxError::NodeNotFound { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-node-not-found")
                    .with_title("Sandbox Node Not Found")
                    .with_detail(error.to_string())
            }
            SandboxError::NodeNotAllowed { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-node-not-allowed")
                    .with_title("Sandbox Node Not Allowed")
                    .with_detail(error.to_string())
            }
            SandboxError::NodeNotReady { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-node-offline")
                    .with_title("Sandbox Node Not Ready")
                    .with_detail(error.to_string())
            }
            SandboxError::NoPlacementNode => problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                .with_type("https://temps.sh/probs/sandbox-no-placement-node")
                .with_title("No Node Available For Sandboxes")
                .with_detail(error.to_string()),
            SandboxError::DedicatedNodesOnly { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-dedicated-node-only")
                    .with_title("Only Dedicated Nodes Available For Sandboxes")
                    .with_detail(error.to_string())
            }
            SandboxError::RuntimeEnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Sandbox Runtime Environment Not Found")
                    .with_detail(error.to_string())
            }
            SandboxError::RuntimeCredentialsFailed { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Sandbox Runtime Credentials Failed")
                    .with_detail(error.to_string())
            }
            SandboxError::ServiceNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Service Not Found")
                .with_detail(error.to_string()),
            SandboxError::RuntimeVariableConflict { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Ambiguous Sandbox Runtime Variables")
                    .with_detail(error.to_string())
            }
            SandboxError::PasswordHashFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Password Hashing Failed")
                    .with_detail(error.to_string())
            }
            SandboxError::PreviewGrantFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Preview Grant Minting Failed")
                    .with_detail(error.to_string())
            }
            SandboxError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
            SandboxError::Io(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
            SandboxError::NoReadyPlacementNode { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-no-placement-node")
                    .with_title("No Node Available For Sandboxes")
                    .with_detail(error.to_string())
            }
            SandboxError::NodeEvictionInProgress { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_type("https://temps.sh/probs/sandbox-node-eviction-in-progress")
                    .with_title("Sandbox Node Eviction In Progress")
                    .with_detail(error.to_string())
            }
            SandboxError::PlacementSettings { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Sandbox Placement Settings Unavailable")
                    .with_detail(error.to_string())
            }
            SandboxError::LimitSettings { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Sandbox Limit Settings Unavailable")
                    .with_detail(error.to_string())
            }
            SandboxError::UserSandboxLimitReached { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-limit-reached")
                    .with_title("Sandbox Limit Reached")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ── DTOs ────────────────────────────────────────────────────────────────────

/// Initial content to seed into the sandbox work dir. Mirrors the
/// `@vercel/sandbox` `source` option. `type` is one of:
/// - `git` — clone `url`; optionally check out `revision`
/// - `tarball` — download `url` (must be tar or tar.gz) and extract
///
/// For private git repos, pass credentials one of two ways:
/// 1. **Inline (SDK-compatible):** `username` + `password`. GitHub
///    tokens use `username: "x-access-token"`.
/// 2. **Stored connection (temps-native):** `git_connection_id`
///    references a row in the caller's git provider connections. Temps
///    resolves the token server-side and injects it safely.
///
/// `git_connection_id` is mutually exclusive with `username`/`password`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum SourceBody {
    Git {
        url: String,
        #[serde(default)]
        revision: Option<String>,
        #[serde(default)]
        depth: Option<u32>,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        git_connection_id: Option<i32>,
        /// Optional path relative to the sandbox work directory.
        #[serde(default)]
        destination: Option<String>,
        /// Remove `<destination>/.git` after cloning. Requires destination.
        #[serde(default)]
        strip_git_metadata: bool,
    },
    Tarball {
        url: String,
    },
}

/// Reject URLs that point at private/loopback/metadata IPs or use
/// non-HTTP schemes. Without this, an authenticated user can drive the
/// sandbox to fetch from internal Docker network services (control
/// plane API, neighbor sandboxes, cloud metadata), turning sandbox
/// seeding into an SSRF primitive.
///
/// Cheap synchronous gate — IP literal blocklist and scheme check.
/// Hostname-based SSRF (e.g. `http://internal.local/`) is caught by
/// `validate_seed_url_async` below, which also resolves DNS and rejects
/// any name whose resolution lands on a blocked IP.
fn validate_seed_url(url: &str, kind: &str) -> Result<(), SandboxError> {
    use temps_core::url_validation::{validate_external_url, UrlValidationError};
    validate_external_url(url).map_err(|e| {
        let detail = match e {
            UrlValidationError::InvalidScheme => "scheme must be http or https".to_string(),
            UrlValidationError::InvalidFormat(m) => format!("invalid url: {m}"),
            UrlValidationError::PrivateIp
            | UrlValidationError::LoopbackIp
            | UrlValidationError::LinkLocalIp
            | UrlValidationError::CloudMetadata
            | UrlValidationError::MulticastIp
            | UrlValidationError::BroadcastIp
            | UrlValidationError::DocumentationIp
            | UrlValidationError::ReservedIp
            | UrlValidationError::UnspecifiedIp
            | UrlValidationError::DomainResolvesToBlockedIp => {
                "host points to a private, loopback, or metadata address".to_string()
            }
            UrlValidationError::DnsResolutionFailed(m) => format!("dns: {m}"),
            // NotLocalOrPrivate is only returned by the inverse
            // validate_loopback_or_private_url/_async check, and
            // InvalidTrustedNetwork only by the settings parser -- never by
            // validate_external_url. Unreachable here, kept explicit so this
            // match stays exhaustive as the shared enum grows.
            UrlValidationError::NotLocalOrPrivate
            | UrlValidationError::InvalidTrustedNetwork { .. } => {
                "unexpected validation error".to_string()
            }
        };
        SandboxError::Validation {
            message: format!("{kind} source: {detail}"),
        }
    })?;
    Ok(())
}

/// Async SSRF gate: runs the synchronous `validate_seed_url` first, then
/// — when the URL has a hostname (not an IP literal) — resolves DNS and
/// rejects any name that points at a blocked IP. Catches `internal.local`,
/// `metadata.google.internal`, and similar names that bypass the IP-literal
/// check.
async fn validate_seed_url_async(url: &str, kind: &str) -> Result<(), SandboxError> {
    validate_seed_url(url, kind)?;

    // Re-parse to get the host. `validate_seed_url` already accepted it,
    // so this won't error in practice — but if it does, we surface the
    // same Validation flavor for consistency.
    let parsed = url::Url::parse(url).map_err(|e| SandboxError::Validation {
        message: format!("{kind} source: invalid url: {e}"),
    })?;
    if let Some(url::Host::Domain(domain)) = parsed.host() {
        temps_core::url_validation::validate_domain_async(domain)
            .await
            .map_err(|_| SandboxError::Validation {
                message: format!(
                    "{kind} source: host resolves to a private, loopback, or metadata address"
                ),
            })?;
    }
    Ok(())
}

impl SourceBody {
    fn uses_stored_git_connection(&self) -> bool {
        matches!(
            self,
            SourceBody::Git {
                git_connection_id: Some(_),
                ..
            }
        )
    }

    /// Validate the body before converting it into the service-layer
    /// `SandboxSource`. Called by handlers that accept a SourceBody.
    ///
    /// Synchronous-only checks (scheme, IP literal blocklist, embedded
    /// credentials, mutually exclusive auth fields). Production handlers
    /// should call [`Self::validate_async`] instead so DNS-based SSRF is
    /// also caught.
    pub fn validate(&self) -> Result<(), SandboxError> {
        match self {
            SourceBody::Git {
                url,
                depth,
                username,
                password,
                git_connection_id,
                destination,
                strip_git_metadata,
                ..
            } => {
                if url.trim().is_empty() {
                    return Err(SandboxError::Validation {
                        message: "git source: url must not be empty".into(),
                    });
                }
                if crate::services::sandbox_service::url_has_embedded_credentials(url) {
                    return Err(SandboxError::Validation {
                        message: "git source: url must not contain embedded credentials — use username/password or git_connection_id".into(),
                    });
                }
                validate_seed_url(url, "git")?;
                if depth.is_some_and(|value| value == 0 || value > 1_000) {
                    return Err(SandboxError::Validation {
                        message: "git source: depth must be between 1 and 1000".into(),
                    });
                }
                let inline = username.is_some() || password.is_some();
                if inline && git_connection_id.is_some() {
                    return Err(SandboxError::Validation {
                        message: "git source: username/password and git_connection_id are mutually exclusive".into(),
                    });
                }
                if username.is_some() != password.is_some() {
                    return Err(SandboxError::Validation {
                        message: "git source: username and password must be provided together"
                            .into(),
                    });
                }
                crate::services::sandbox_service::validate_source_destination(
                    destination.as_deref(),
                )?;
                if *strip_git_metadata && destination.is_none() {
                    return Err(SandboxError::Validation {
                        message: "git source: strip_git_metadata requires destination".into(),
                    });
                }
                Ok(())
            }
            SourceBody::Tarball { url } => {
                if url.trim().is_empty() {
                    return Err(SandboxError::Validation {
                        message: "tarball source: url must not be empty".into(),
                    });
                }
                validate_seed_url(url, "tarball")?;
                Ok(())
            }
        }
    }

    /// Full async validation including DNS resolution. Use from handlers
    /// to block SSRF via hostnames that resolve to internal/metadata IPs.
    pub async fn validate_async(&self) -> Result<(), SandboxError> {
        // Run the cheap sync checks first so we fail fast on obvious errors
        // and avoid an unnecessary DNS lookup.
        self.validate()?;
        match self {
            SourceBody::Git { url, .. } => validate_seed_url_async(url, "git").await,
            SourceBody::Tarball { url } => validate_seed_url_async(url, "tarball").await,
        }
    }
}

impl From<SourceBody> for SandboxSource {
    fn from(s: SourceBody) -> Self {
        match s {
            SourceBody::Git {
                url,
                revision,
                depth,
                username,
                password,
                git_connection_id,
                destination,
                strip_git_metadata,
            } => SandboxSource::Git {
                url,
                revision,
                depth,
                username,
                password,
                git_connection_id,
                destination,
                strip_git_metadata,
            },
            SourceBody::Tarball { url } => SandboxSource::Tarball { url },
        }
    }
}

/// Nested `resources: { memory, vcpus }` as sent by `@vercel/sandbox`.
/// `memory` is in MB, `vcpus` is fractional CPU count.
#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct ResourcesBody {
    #[serde(default)]
    #[schema(minimum = 6, maximum = 32768)]
    pub memory: Option<u64>,
    #[serde(default)]
    #[schema(minimum = 0.01, maximum = 16.0)]
    pub vcpus: Option<f64>,
}

#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct CreateSandboxBody {
    /// Permit installing/running Docker with normal guest-root privileges.
    /// Requires backend="microsandbox". Does not install Docker or expose the host socket.
    #[serde(default)]
    pub allow_docker: bool,
    /// Docker image override. `null` uses the platform default.
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Idle timeout in seconds (temps-native). Clamped to `[60, 86400]`.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Idle timeout as sent by `@vercel/sandbox` (milliseconds). Converted
    /// to seconds when `timeout_secs` is absent.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Extra env vars baked into the container on create.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// CPU cores, `[0.01, 16]` (lower if an administrator set a maximum).
    /// Omit for the operator's default.
    #[schema(minimum = 0.01, maximum = 16.0)]
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    /// Memory in MB, `[6, 32768]` (lower if an administrator set a maximum,
    /// and never more than the host has). Omit for the operator's default.
    #[schema(minimum = 6, maximum = 32768)]
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
    /// Max processes, `[1, 4096]` (lower if an administrator set a maximum);
    /// `0`/negative (unlimited) is rejected. Omit for the platform default
    /// (512).
    #[schema(minimum = 1, maximum = 4096)]
    #[serde(default)]
    pub pids_limit: Option<i64>,
    /// Root disk size in MB, `[1, 65536]` (Firecracker only; Docker ignores
    /// it). Omit for the platform default (1 GiB).
    #[schema(minimum = 1, maximum = 65536)]
    #[serde(default)]
    pub disk_size_mb: Option<u64>,
    /// `@vercel/sandbox`'s nested resources object. When present, its
    /// `memory` / `vcpus` populate `memory_limit_mb` / `cpu_limit` if those
    /// weren't sent directly.
    #[serde(default)]
    pub resources: Option<ResourcesBody>,
    /// Optional initial content to seed into the work dir. Clones a
    /// repo or extracts a tarball after the sandbox is created.
    #[serde(default)]
    pub source: Option<SourceBody>,
    /// Optional preview-URL password. When set, every preview URL served
    /// for this sandbox is gated behind a login form. 8–256 characters.
    /// Omit to leave preview URLs open (the sandbox ID remains the only
    /// gate). The plaintext is never returned; only the last-4 hint is
    /// surfaced in `SandboxResponse.preview_password_hint`.
    #[serde(default)]
    pub preview_password: Option<SecretString>,
    /// Ports the sandbox will listen on. Each port becomes a `routes[]`
    /// entry in the create/get response so `@vercel/sandbox`'s
    /// `sandbox.domain(port)` can resolve it client-side without an
    /// extra round-trip.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// Isolation backend: `"docker"` (default), `"firecracker"` (ADR-029,
    /// hardware-virtualized microVM — requires a host provisioned with
    /// `temps firecracker setup`) or `"microsandbox"` (ADR-050, experimental
    /// libkrun microVM — requires `temps microsandbox setup`). Omit for the
    /// platform default; existing clients are unaffected. Requesting an
    /// unavailable backend fails with 400 rather than silently downgrading
    /// isolation.
    #[serde(default)]
    pub backend: Option<String>,
    /// Lifecycle class (ADR-036, temps-native): `"ephemeral"` (default) or
    /// `"workspace"`.
    ///
    /// A workspace is a long-lived development environment: it is still
    /// suspended after `timeout_secs` of inactivity, but the next exec or
    /// filesystem call wakes it transparently instead of returning 409, and
    /// nothing ever destroys it automatically. Use it for "give me a place
    /// to work on this repo"; leave it unset for throwaway sandboxes.
    #[serde(default)]
    pub lifecycle: Option<String>,
    /// Create the sandbox against a temps project (temps-native). The
    /// project's connected repo and git credential seed the work dir when
    /// no explicit `source` is given, and the sandbox is attributed to the
    /// project so it can be listed alongside it.
    ///
    /// Requires access to the project and `git_repositories:read` when Temps
    /// derives the source from the project. The same team/scope rules that
    /// gate every other project-scoped endpoint apply.
    #[serde(default)]
    pub project_id: Option<i32>,

    /// Create the sandbox from a snapshot (ADR-037).
    ///
    /// Mutually exclusive with `image`: if both are set the request fails
    /// with 400. When set, the sandbox is created with the snapshotted
    /// filesystem rather than a base image, giving users a reproducible
    /// starting point.
    ///
    /// The snapshot must be in `ready` status and belong to the calling user.
    #[serde(default)]
    pub from_snapshot: Option<String>,

    /// Node to create the sandbox on (ADR-048): a node name, a node id, or
    /// `control-plane` / `0` for the control plane. Omit to let Temps place
    /// it (the control plane when allowed, otherwise the allowed worker with
    /// the fewest live sandboxes). The node must be allowed by the operator's
    /// sandbox placement settings and online; otherwise the request fails
    /// with 422 — it never silently falls back to another node.
    #[serde(default)]
    #[schema(example = "worker-1")]
    pub node: Option<String>,

    /// Which outside destinations the sandbox may reach (temps-native).
    /// Omit for `{"mode": "internet"}`: any public destination on ports 80
    /// and 443. `{"mode": "restricted", "rules": [...]}` allows only the
    /// listed hosts (`api.example.com`, or `*.example.com` for subdomains)
    /// and CIDR blocks, each on its `ports` (default 80 and 443). Private,
    /// loopback, link-local and metadata addresses are never reachable.
    /// A restricted policy needs the docker backend; elsewhere the create
    /// fails with 422. Change it later with `PUT /v1/sandboxes/{id}/egress-policy`.
    #[serde(default)]
    pub egress_policy: temps_agents::sandbox::SandboxEgressPolicy,

    // ── `@vercel/sandbox` fields accepted for compatibility and ignored.
    // We accept them so SDK calls don't 422 on `deny_unknown_fields`; we
    // don't act on them because temps has no equivalent concept today.
    #[serde(default, rename = "projectId")]
    pub _project_id: Option<String>,
    #[serde(default)]
    pub _runtime: Option<String>,
    #[serde(default, rename = "networkPolicy")]
    pub _network_policy: Option<serde_json::Value>,
}

impl From<CreateSandboxBody> for CreateSandboxRequest {
    fn from(b: CreateSandboxBody) -> Self {
        let resources = b.resources.unwrap_or_default();
        Self {
            image: b.image,
            name: b.name,
            timeout_secs: b.timeout_secs.or_else(|| b.timeout.map(|ms| ms / 1000)),
            env: b.env,
            cpu_limit: b.cpu_limit.or(resources.vcpus),
            memory_limit_mb: b.memory_limit_mb.or(resources.memory),
            pids_limit: b.pids_limit,
            disk_size_mb: b.disk_size_mb,
            allow_docker: b.allow_docker,
            source: b.source.map(SandboxSource::from),
            preview_password: b.preview_password.map(SecretString::into_exposed),
            ports: b.ports,
            backend: b.backend,
            lifecycle: b.lifecycle,
            project_id: b.project_id,
            // Snapshot artifact is resolved and injected by the handler;
            // the `From` impl starts with None and the handler sets it.
            // Every request built from an HTTP body is subject to the
            // operator's maximums and per-user cap; the service applies them
            // once it knows which node the sandbox lands on.
            enforce_operator_limits: true,
            from_snapshot_artifact: None,
            host_work_dir_override: None,
            node: b.node,
            egress_policy: b.egress_policy,
        }
    }
}

/// Ceilings for the resource limits a caller may request on create. The
/// provider treats a request value as authoritative (the operator's
/// `agent_sandbox` settings are only the defaults for omitted fields), so
/// without a ceiling here any `sandboxes:write` key could ask for no process
/// cap (`pids_limit <= 0` means "unlimited" to Docker) or for more memory than
/// the host has. Generous enough for real workloads, finite so the host's
/// pid table and memory stay protected from untrusted code.
pub const MAX_SANDBOX_CPU_CORES: f64 = 16.0;
/// Floors below which the request cannot be enforced. Docker takes CPU as
/// integer NanoCPUs, so a tiny positive value truncates to 0, which Docker
/// reads as "no CPU quota"; it also rejects memory limits under 6 MB.
pub const MIN_SANDBOX_CPU_CORES: f64 = 0.01;
pub const MIN_SANDBOX_MEMORY_MB: u64 = 6;
pub const MAX_SANDBOX_MEMORY_MB: u64 = 32 * 1024;
pub const MAX_SANDBOX_PIDS_LIMIT: i64 = 4096;
pub const MAX_SANDBOX_DISK_MB: u64 = 64 * 1024;

/// Reject caller-supplied resource limits that are non-positive, non-finite
/// or above the ceilings. `None` is always accepted: it selects the
/// operator's defaults.
fn validate_resource_limits(req: &CreateSandboxRequest) -> Result<(), SandboxError> {
    if let Some(cpu) = req.cpu_limit {
        if !cpu.is_finite() || !(MIN_SANDBOX_CPU_CORES..=MAX_SANDBOX_CPU_CORES).contains(&cpu) {
            return Err(SandboxError::Validation {
                message: format!(
                    "cpu_limit/vcpus must be between {MIN_SANDBOX_CPU_CORES} and {MAX_SANDBOX_CPU_CORES}, got {cpu}"
                ),
            });
        }
    }
    if let Some(memory) = req.memory_limit_mb {
        if !(MIN_SANDBOX_MEMORY_MB..=MAX_SANDBOX_MEMORY_MB).contains(&memory) {
            return Err(SandboxError::Validation {
                message: format!(
                    "memory_limit_mb/memory must be between {MIN_SANDBOX_MEMORY_MB} and {MAX_SANDBOX_MEMORY_MB} MB, got {memory}"
                ),
            });
        }
    }
    if let Some(pids) = req.pids_limit {
        if pids <= 0 || pids > MAX_SANDBOX_PIDS_LIMIT {
            return Err(SandboxError::Validation {
                message: format!(
                    "pids_limit must be between 1 and {MAX_SANDBOX_PIDS_LIMIT} (0 and negative values mean unlimited and are not accepted), got {pids}"
                ),
            });
        }
    }
    if let Some(disk) = req.disk_size_mb {
        if disk == 0 || disk > MAX_SANDBOX_DISK_MB {
            return Err(SandboxError::Validation {
                message: format!(
                    "disk_size_mb must be between 1 and {MAX_SANDBOX_DISK_MB} MB, got {disk}"
                ),
            });
        }
    }
    Ok(())
}

/// Map our internal status to the Vercel SDK status enum
/// (`pending|running|stopping|stopped|failed|aborted|snapshotting`).
fn map_status(s: &str) -> &'static str {
    match s {
        "running" => "running",
        "stopped" => "stopped",
        "destroyed" => "aborted",
        "failed" => "failed",
        _ => "pending",
    }
}

/// A single preview route, one per declared port. We don't know ports
/// upfront, so we surface an empty array by default — SDK clients use
/// their own port when calling `sandbox.domain(port)`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxRoute {
    pub url: String,
    pub subdomain: String,
    pub port: u32,
}

/// Inner `sandbox` object in `@vercel/sandbox` responses. Strict shape —
/// the SDK's zod validator rejects missing required fields.
#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxInner {
    /// Normal guest-root semantics enabled for Docker (microsandbox only).
    pub allow_docker: bool,
    pub id: String,
    pub memory: u64,
    pub vcpus: f64,
    pub region: String,
    pub runtime: String,
    /// Idle timeout in milliseconds (SDK convention).
    pub timeout: u64,
    pub status: String,
    /// Creation time as Unix epoch milliseconds.
    #[serde(rename = "requestedAt")]
    pub requested_at: i64,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    pub cwd: String,

    // temps-native extras — the SDK ignores fields it doesn't know about.
    pub name: String,
    pub image: Option<String>,
    /// Isolation backend: "docker" | "firecracker" | "microsandbox". `None` on legacy rows
    /// created before the backend was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Configured root disk size in MB (Firecracker). `None` when unknown or
    /// the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_size_mb: Option<u64>,
    pub preview_url_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_password_hint: Option<String>,
    /// Who may open the preview URLs: `temps_login` (a signed-in Temps user
    /// who may read this sandbox; the default), `password` (whoever knows the
    /// preview password) or `public` (anyone).
    pub preview_access: SandboxPreviewAccess,
    /// Client networks allowed to open the preview URLs, as normalised
    /// CIDRs. Checked before `preview_access`; empty = any address.
    pub preview_allowed_cidrs: Vec<String>,
    /// Agent run this sandbox executes (autofixer / workflow agent).
    /// `None` for sandboxes created via this API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<i32>,
    /// Lifecycle class (ADR-036): `"ephemeral"` or `"workspace"`. Always
    /// present — a client that doesn't know about workspaces sees
    /// `"ephemeral"` and behaves exactly as before.
    pub lifecycle: String,
    /// Project this sandbox was created from, when created from one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i32>,
    /// Repo the work dir was seeded from. Never carries credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_repo_url: Option<String>,
    /// Worker node hosting the sandbox (ADR-048). `null` = the control plane.
    pub node_id: Option<i32>,
    /// Name of the hosting node; `"control-plane"` for control-plane
    /// sandboxes.
    pub node_name: String,
    /// Which outside destinations the sandbox may reach (temps-native).
    pub egress_policy: temps_agents::sandbox::SandboxEgressPolicy,
}

/// `@vercel/sandbox` wraps every single-sandbox response as
/// `{ sandbox: {...}, routes: [...] }`. The SDK reads both.
#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxResponse {
    pub sandbox: SandboxInner,
    pub routes: Vec<SandboxRoute>,
}

impl SandboxResponse {
    /// Build a response from an entity row, attaching the preview URL
    /// template for the given public_id and materialising `routes[]` for
    /// each declared port. Keeping URL construction out of `From` impls
    /// is intentional — the template depends on platform settings which
    /// only the service layer has access to.
    pub fn with_template(
        s: SandboxSummary,
        template: String,
        parts: &crate::services::preview_urls::PreviewUrlParts,
    ) -> Self {
        let created_ms = s.created_at.timestamp_millis();
        let expires_ms = s.expires_at.timestamp_millis();
        let timeout_ms = (expires_ms - created_ms).max(0) as u64;
        let label = s
            .public_id
            .strip_prefix("sbx_")
            .unwrap_or(&s.public_id)
            .to_string();
        // Worker sandboxes are served too: the proxy relays their previews
        // through the worker's agent.
        let routes = s
            .ports
            .iter()
            .map(|port| SandboxRoute {
                url: parts.url_for(&s.public_id, *port),
                subdomain: format!("ws-{}-{}", label, port),
                port: *port as u32,
            })
            .collect();
        Self {
            sandbox: SandboxInner {
                id: s.public_id,
                memory: 0,
                vcpus: 0.0,
                region: "local".to_string(),
                runtime: "node24".to_string(),
                timeout: timeout_ms,
                status: map_status(&s.status).to_string(),
                requested_at: created_ms,
                created_at: created_ms,
                updated_at: created_ms,
                cwd: s.work_dir,
                name: s.name,
                image: s.image,
                backend: s.backend,
                allow_docker: s.allow_docker,
                disk_size_mb: s.disk_size_mb,
                preview_url_template: template,
                preview_password_hint: s.preview_password_hint,
                preview_access: s.preview_access,
                preview_allowed_cidrs: s.preview_allowed_cidrs,
                agent_run_id: s.agent_run_id,
                lifecycle: s.lifecycle,
                project_id: s.project_id,
                source_repo_url: s.source_repo_url,
                node_id: s.node_id,
                node_name: s.node_name,
                egress_policy: s.egress_policy,
            },
            routes,
        }
    }
}

impl From<SandboxSummary> for SandboxResponse {
    fn from(s: SandboxSummary) -> Self {
        let parts = crate::services::preview_urls::PreviewUrlParts {
            protocol: "https".to_string(),
            domain: "localho.st".to_string(),
            port: None,
        };
        Self::with_template(s, String::new(), &parts)
    }
}

impl From<temps_entities::sandboxes::Model> for SandboxResponse {
    fn from(m: temps_entities::sandboxes::Model) -> Self {
        SandboxResponse::from(SandboxSummary::from(&m))
    }
}

/// SDK pagination cursor. We use opaque page numbers internally but
/// expose `count`/`next`/`prev` the way `@vercel/sandbox` expects.
#[derive(Debug, Serialize, ToSchema)]
pub struct Pagination {
    pub count: u64,
    pub next: Option<u64>,
    pub prev: Option<u64>,
}

/// SDK list response: `{ sandboxes: [...], pagination: {...} }`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ListSandboxesResponse {
    pub sandboxes: Vec<SandboxInner>,
    pub pagination: Pagination,
}

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    /// SDK-compat: `limit` maps to `page_size` when the latter isn't set.
    pub limit: Option<u64>,
    /// Accepted and ignored — the SDK's opaque project slug. temps uses
    /// the numeric `project_id` filter below instead.
    pub project: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// Filter by lifecycle class (temps-native): `"ephemeral"` or
    /// `"workspace"`. Omit for both.
    pub lifecycle: Option<String>,
    /// Filter to sandboxes created from a specific temps project.
    pub project_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ExtendTimeoutBody {
    /// Extra seconds to add to the existing `expires_at` (temps-native).
    #[serde(default)]
    pub extra_secs: Option<u64>,
    /// `@vercel/sandbox`-compatible alternative — duration in milliseconds.
    /// Used when `extra_secs` is absent.
    #[serde(default)]
    pub duration: Option<u64>,
}

impl ExtendTimeoutBody {
    pub fn resolve_secs(&self) -> Option<u64> {
        self.extra_secs
            .or_else(|| self.duration.map(|ms| ms / 1000))
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecBody {
    pub cmd: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

impl From<ExecBody> for ExecOptions {
    fn from(b: ExecBody) -> Self {
        Self {
            cmd: b.cmd,
            env: b.env,
            cwd: b.cwd,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExecResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<ExecResult> for ExecResponse {
    fn from(r: ExecResult) -> Self {
        Self {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExecDetachedResponse {
    pub job_id: String,
}

/// Snapshot of a background job. `status` is one of "running" | "exited"
/// | "failed"; `exit_code` is populated only when `status == "exited"`.
#[derive(Debug, Serialize, ToSchema)]
pub struct JobStatusResponse {
    pub status: String,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

impl From<JobState> for JobStatusResponse {
    fn from(s: JobState) -> Self {
        let (status, exit_code, reason) = match s.status {
            JobStatus::Running => ("running".to_string(), None, None),
            JobStatus::Exited { exit_code } => ("exited".to_string(), Some(exit_code), None),
            JobStatus::Failed { reason } => ("failed".to_string(), None, Some(reason)),
        };
        Self {
            status,
            exit_code,
            reason,
            stdout: s.stdout,
            stderr: s.stderr,
        }
    }
}

/// Row in the jobs list. Omits stdout/stderr so a noisy dev server doesn't
/// bloat the list payload — callers drill into `GET /jobs/{id}` for the
/// full buffer.
#[derive(Debug, Serialize, ToSchema)]
pub struct JobSummaryResponse {
    pub id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub cmd: String,
    pub started_at: String,
}

impl From<crate::services::job_tracker::JobSummary> for JobSummaryResponse {
    fn from(s: crate::services::job_tracker::JobSummary) -> Self {
        let (status, exit_code, reason) = match s.status {
            JobStatus::Running => ("running".to_string(), None, None),
            JobStatus::Exited { exit_code } => ("exited".to_string(), Some(exit_code), None),
            JobStatus::Failed { reason } => ("failed".to_string(), None, Some(reason)),
        };
        Self {
            id: s.id,
            status,
            exit_code,
            reason,
            cmd: s.cmd,
            started_at: s.started_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListJobsResponse {
    pub items: Vec<JobSummaryResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFileBody {
    /// Absolute path inside the sandbox. Must start with `/`.
    pub path: String,
    /// File contents, base64-encoded. Required — lets callers ship binary
    /// data over JSON without charset games.
    pub contents_b64: String,
    /// Unix permission mask (e.g. 0o644). Defaults to 0o644 when absent.
    #[serde(default)]
    pub mode: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileQuery {
    pub path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReadFileResponse {
    pub path: String,
    /// File contents, base64-encoded. Symmetric with `WriteFileBody`.
    pub contents_b64: String,
    pub size: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MkdirBody {
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatQuery {
    pub path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StatResponse {
    pub path: String,
    pub exists: bool,
    pub is_dir: bool,
    pub is_file: bool,
    pub size: u64,
}

impl From<StatInfo> for StatResponse {
    fn from(s: StatInfo) -> Self {
        Self {
            path: s.path,
            exists: s.exists,
            is_dir: s.is_dir,
            is_file: s.is_file,
            size: s.size,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListDirQuery {
    pub path: String,
    /// 1-based page. Defaults to 1.
    pub page: Option<usize>,
    /// Entries per page. Defaults to 20, at most 100.
    pub page_size: Option<usize>,
}

/// What a directory entry is. Symlinks are reported, never followed.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryEntry {
    /// Name within the directory, without any path.
    pub name: String,
    pub kind: DirectoryEntryKind,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryListingResponse {
    pub path: String,
    /// `false` when the path does not exist (not an error).
    pub exists: bool,
    /// `false` when the path exists but is not a directory.
    pub is_dir: bool,
    /// Directories first, then everything else, each group in byte order of
    /// the name. One page of the directory.
    pub entries: Vec<DirectoryEntry>,
    /// 1-based page these entries are.
    pub page: usize,
    /// Entries per page that was applied (default 20, at most 100).
    pub page_size: usize,
    /// Entries in the whole directory, so `ceil(total / page_size)` is the
    /// number of pages. At most 10000: see `truncated`.
    pub total: usize,
    /// The directory has more entries than the listing reads (10000), so
    /// `total` and the pages cover only the first 10000 names the filesystem
    /// returned.
    pub truncated: bool,
    /// More entries exist after this page.
    pub has_more: bool,
}

impl From<crate::services::fs::DirListing> for DirectoryListingResponse {
    fn from(listing: crate::services::fs::DirListing) -> Self {
        use crate::services::fs::DirEntryKind;
        Self {
            path: listing.path,
            exists: listing.exists,
            is_dir: listing.is_dir,
            entries: listing
                .entries
                .into_iter()
                .map(|entry| DirectoryEntry {
                    name: entry.name,
                    kind: match entry.kind {
                        DirEntryKind::File => DirectoryEntryKind::File,
                        DirEntryKind::Directory => DirectoryEntryKind::Directory,
                        DirEntryKind::Symlink => DirectoryEntryKind::Symlink,
                        DirEntryKind::Other => DirectoryEntryKind::Other,
                    },
                })
                .collect(),
            page: listing.page,
            page_size: listing.page_size,
            total: listing.total,
            truncated: listing.truncated,
            has_more: listing.has_more,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainQuery {
    pub port: u16,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxDomainResponse {
    pub url: String,
}

/// Build a `SandboxResponse` with the preview URL template attached.
/// Shared by every handler that returns a single sandbox so the template
/// wiring lives in one place.
async fn build_response(
    state: &Arc<SandboxAppState>,
    row: temps_entities::sandboxes::Model,
) -> SandboxResponse {
    let parts = state.sandbox_service.preview_parts().await;
    let template = parts.host_template(&row.public_id);
    let mut summary = [SandboxSummary::from(&row)];
    state.sandbox_service.fill_node_names(&mut summary).await;
    let [summary] = summary;
    SandboxResponse::with_template(summary, template, &parts)
}

/// Batched variant of `build_response` for list endpoints that return
/// `SandboxSummary` already — computes preview parts once per page
/// instead of per-row.
async fn build_summary_responses(
    state: &Arc<SandboxAppState>,
    mut items: Vec<SandboxSummary>,
) -> Vec<SandboxResponse> {
    let parts = state.sandbox_service.preview_parts().await;
    state.sandbox_service.fill_node_names(&mut items).await;
    items
        .into_iter()
        .map(|s| {
            let template = parts.host_template(&s.public_id);
            SandboxResponse::with_template(s, template, &parts)
        })
        .collect()
}

// ── Handlers ────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes",
    request_body = CreateSandboxBody,
    responses(
        (status = 201, description = "Sandbox created", body = SandboxResponse),
        (status = 400, description = "Validation error, including a CPU, memory, process or disk limit outside the allowed range or above the administrator's maximum"),
        (status = 401, description = "Unauthorized"),
        (status = 422, description = "Requested node does not exist, is not allowed to run sandboxes, is not active, or no node is eligible; or the user already has the maximum number of sandboxes an administrator allows"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Json(body): Json<CreateSandboxBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    // An implicit project source may resolve a stored provider connection in
    // the service. Require repository-read permission before that lookup so a
    // revoked Git grant cannot be bypassed through sandbox creation.
    if (body.project_id.is_some() && body.source.is_none())
        || body
            .source
            .as_ref()
            .is_some_and(SourceBody::uses_stored_git_connection)
    {
        temps_auth::permission_guard!(auth, GitRepositoriesRead);
    }
    // Creating a sandbox *from* a project reads that project's repo URL and
    // git credential, so it needs the same access gate as any other
    // project-scoped endpoint — an API key scoped to project A must not be
    // able to clone project B's private repo into a sandbox it owns.
    if let Some(project_id) = body.project_id {
        project_scope_guard!(auth, project_id);
        project_access_guard!(auth, project_id, state.project_access_checker);
    }
    if let Some(src) = body.source.as_ref() {
        src.validate_async().await?;
    }

    // `from_snapshot` and `image` are mutually exclusive: snapshotting
    // overrides the base image, so passing both is ambiguous.
    if body.from_snapshot.is_some() && body.image.is_some() {
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Conflicting Fields")
                .detail("'from_snapshot' and 'image' are mutually exclusive — omit one")
                .build(),
        );
    }

    // If `from_snapshot` is set, resolve the artifact and inject it into
    // the request so the service can call `provider.create_from_snapshot`.
    let user_id = auth.user_id();
    let from_snapshot_id = body.from_snapshot.clone();
    let mut req: crate::services::sandbox_service::CreateSandboxRequest = body.into();
    validate_resource_limits(&req)?;
    // Held through provider restore so a concurrent delete cannot remove the
    // verified artifact after resolution but before it is consumed.
    let mut artifact_lifecycle_guard = None;

    if let Some(snap_id) = from_snapshot_id {
        let snapshot_svc = state.snapshot_service.as_ref().ok_or_else(|| {
            use crate::error::SandboxSnapshotError;
            use temps_core::problemdetails::Problem;
            Problem::from(SandboxSnapshotError::NotSupported {
                backend: req.backend.clone().unwrap_or_else(|| "unknown".to_string()),
            })
        })?;
        artifact_lifecycle_guard = Some(snapshot_svc.acquire_artifact_lifecycle().await);
        let artifact = snapshot_svc
            .resolve_standalone_for_restore(user_id, &snap_id, req.backend.as_deref())
            .await
            .map_err(|e| {
                use temps_core::problemdetails::Problem;
                Problem::from(e)
            })?;
        if req.backend.is_none() {
            req.backend = Some(artifact.backend.to_string());
        }
        req.from_snapshot_artifact = Some(artifact);
    }

    let row = state.sandbox_service.create_sandbox(user_id, req).await?;
    drop(artifact_lifecycle_guard);
    let resp = build_response(&state, row).await;
    Ok((StatusCode::CREATED, Json(resp)))
}

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes",
    params(("page" = Option<u64>, Query, description = "Page (1-indexed)"),
           ("page_size" = Option<u64>, Query, description = "Items per page (default 20, max 100)"),
           ("lifecycle" = Option<String>, Query, description = "Filter by lifecycle class: \"ephemeral\" or \"workspace\""),
           ("project_id" = Option<i32>, Query, description = "Filter to sandboxes created from a project")),
    responses((status = 200, description = "List sandboxes", body = ListSandboxesResponse),
              (status = 400, description = "Unknown lifecycle value")),
    security(("bearer_auth" = []))
)]
pub async fn list_sandboxes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Query(q): Query<PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let page = q.page.unwrap_or(1);
    let page_size = q.page_size.or(q.limit).unwrap_or(20);
    let lifecycle = q
        .lifecycle
        .as_deref()
        .map(SandboxLifecycle::parse)
        .transpose()?;
    let (items, total) = state
        .sandbox_service
        .list_for_user(
            auth.user_id(),
            Some(page),
            Some(page_size),
            lifecycle,
            q.project_id,
        )
        .await?;
    let envelopes = build_summary_responses(&state, items).await;
    let sandboxes: Vec<SandboxInner> = envelopes.into_iter().map(|e| e.sandbox).collect();
    let has_next = (page * page_size) < total;
    let resp = ListSandboxesResponse {
        sandboxes,
        pagination: Pagination {
            count: total,
            next: if has_next { Some(page + 1) } else { None },
            prev: if page > 1 { Some(page - 1) } else { None },
        },
    };
    Ok(Json(resp))
}

/// One entry in a sandbox's operations timeline.
#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxEvent {
    /// Machine-readable operation (`created`, `stopped`, `resumed`,
    /// `restarted`, `timeout_extended`, `resized`, `preview_password_set`,
    /// `preview_password_cleared`, `preview_share_link_created`, `source_seeded`,
    /// `destroyed`).
    pub event_type: String,
    /// Optional structured context (shape depends on `event_type`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    /// Unix epoch milliseconds.
    pub at: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxEventsResponse {
    pub events: Vec<SandboxEvent>,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Max events to return (default 100, clamped to 500).
    pub limit: Option<u64>,
}

/// The operations timeline for a sandbox (lifecycle events only — never
/// shell/exec activity), newest first.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/events",
    responses((status = 200, description = "Operations timeline", body = SandboxEventsResponse)),
    security(("bearer_auth" = []))
)]
pub async fn list_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let rows = state
        .sandbox_service
        .list_events(&id, auth.user_id(), q.limit.unwrap_or(100))
        .await?;
    let events = rows
        .into_iter()
        .map(|e| SandboxEvent {
            event_type: e.event_type,
            detail: e.detail,
            at: e.created_at.timestamp_millis(),
        })
        .collect();
    Ok(Json(SandboxEventsResponse { events }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResizeSandboxBody {
    /// New root disk size in MB. Grow-only; must exceed the current size.
    pub disk_size_mb: u64,
}

/// Grow a Firecracker sandbox's root disk. Offline resize — the VM reboots
/// (filesystem/data persist) rather than resizing fully live.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/resize",
    request_body = ResizeSandboxBody,
    responses(
        (status = 200, description = "Resized", body = SandboxResponse),
        (status = 400, description = "Invalid size or unsupported backend")
    ),
    security(("bearer_auth" = []))
)]
pub async fn resize_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<ResizeSandboxBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let model = state
        .sandbox_service
        .resize_sandbox(&id, auth.user_id(), body.disk_size_mb)
        .await?;
    let envelopes = build_summary_responses(&state, vec![SandboxSummary::from(&model)]).await;
    Ok(Json(envelopes.into_iter().next()))
}

#[derive(Debug, Clone, Serialize)]
struct SandboxEgressPolicyUpdatedAudit {
    context: temps_core::AuditContext,
    sandbox_id: String,
    egress_policy: temps_agents::sandbox::SandboxEgressPolicy,
}

impl temps_core::AuditOperation for SandboxEgressPolicyUpdatedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_EGRESS_POLICY_UPDATED".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize SandboxEgressPolicyUpdatedAudit for sandbox {}: {}",
                self.sandbox_id,
                e
            )
        })
    }
}

/// Replace which outside destinations the sandbox may reach.
///
/// `{"mode": "internet"}` allows any public destination on ports 80 and 443;
/// `{"mode": "restricted", "rules": [{"host": "api.example.com", "ports": [443]},
/// {"cidr": "1.1.1.0/24"}]}` allows only what a rule lists (`*.example.com`
/// matches subdomains; `ports` defaults to 80 and 443). Private, loopback,
/// link-local and metadata destinations are refused in every mode, and the
/// Temps model, MCP and git relays keep working whatever the policy. A
/// refused request gets a 403 from the sandbox's proxy naming the
/// destination and the policy.
///
/// Applied before it is stored: on a running Docker sandbox the egress proxy
/// is replaced, which interrupts outbound connections for a second or two
/// and closes open tunnels; a stopped sandbox gets it on its next start.
#[utoipa::path(
    tag = "Sandboxes",
    put,
    path = "/v1/sandboxes/{id}/egress-policy",
    params(("id" = String, Path, description = "Sandbox id (`sbx_...`)")),
    request_body = temps_agents::sandbox::SandboxEgressPolicy,
    responses(
        (status = 200, description = "Policy applied and stored", body = SandboxResponse),
        (status = 400, description = "Invalid policy (the detail names the rule and value)", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Sandbox not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "Sandbox is not running or stopped, or belongs to an agent run", body = temps_core::problemdetails::ProblemDetails),
        (status = 422, description = "The sandbox's backend cannot enforce a restricted policy", body = temps_core::problemdetails::ProblemDetails),
        (status = 503, description = "The sandbox's backend or node is unavailable", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_egress_policy(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(id): Path<String>,
    Json(policy): Json<temps_agents::sandbox::SandboxEgressPolicy>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let user_id = auth.user_id();
    let model = state
        .sandbox_service
        .set_egress_policy(&id, user_id, policy)
        .await?;

    if let Some(ref audit) = state.audit_service {
        let event = SandboxEgressPolicyUpdatedAudit {
            context: temps_core::AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            sandbox_id: id.clone(),
            egress_policy: model.egress_policy.0.clone(),
        };
        if let Err(e) = audit.create_audit_log(&event).await {
            tracing::error!(
                user_id = %user_id,
                sandbox_id = %id,
                "sandbox egress policy: failed to write audit log: {}",
                e
            );
        }
    }

    let envelopes = build_summary_responses(&state, vec![SandboxSummary::from(&model)]).await;
    Ok(Json(envelopes.into_iter().next()))
}

/// Inspect rootfs storage: the Firecracker digest-keyed cache (with which
/// sandboxes reference each entry) and per-VM disks. Empty on Docker-only
/// hosts. Admin/read scope — this exposes host storage layout.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/rootfs",
    responses((status = 200, description = "Rootfs storage report")),
    security(("bearer_auth" = []))
)]
pub async fn rootfs_report(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
) -> Result<impl IntoResponse, Problem> {
    // Host-global inventory (every tenant's VM names + image digests) — admin
    // only, so one sandbox user can't enumerate the whole host.
    require_sandbox_admin(&auth)?;
    let report = state.sandbox_service.rootfs_report().await?;
    Ok(Json(report))
}

/// Reclaim rootfs cache entries not backing any live sandbox. Idempotent;
/// safe to call any time (live VMs hold their own per-VM disks).
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/rootfs/gc",
    responses((status = 200, description = "Reclaimed cache entries")),
    security(("bearer_auth" = []))
)]
pub async fn rootfs_gc(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
) -> Result<impl IntoResponse, Problem> {
    // Host-wide reclaim — admin only.
    require_sandbox_admin(&auth)?;
    let report = state.sandbox_service.gc_rootfs().await?;
    Ok(Json(report))
}

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}",
    responses(
        (status = 200, description = "Sandbox details", body = SandboxResponse),
        (status = 404, description = "Not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let row = state
        .sandbox_service
        .find_by_public_id(&id, auth.user_id())
        .await?;
    Ok(Json(build_response(&state, row).await))
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/stop",
    responses(
        (status = 204, description = "Sandbox stopped and destroyed"),
        (status = 404, description = "Not found"),
        (status = 409, description = "Sandbox belongs to an active agent run — stop the run instead"),
        (status = 503, description = "The sandbox's worker node is unreachable; the sandbox was kept")
    ),
    security(("bearer_auth" = []))
)]
pub async fn stop_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    state
        .sandbox_service
        .destroy_sandbox(&id, auth.user_id())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/destroy",
    responses(
        (status = 204, description = "Sandbox destroyed (alias for `/stop` with an explicit verb)"),
        (status = 404, description = "Not found"),
        (status = 409, description = "Sandbox belongs to an active agent run — stop the run instead"),
        (status = 503, description = "The sandbox's worker node is unreachable; the sandbox was kept")
    ),
    security(("bearer_auth" = []))
)]
pub async fn destroy_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    state
        .sandbox_service
        .destroy_sandbox(&id, auth.user_id())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/extend-timeout",
    request_body = ExtendTimeoutBody,
    responses(
        (status = 200, description = "Timeout extended", body = SandboxResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn extend_timeout(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<ExtendTimeoutBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let row = state
        .sandbox_service
        .extend_timeout(
            &id,
            auth.user_id(),
            body.resolve_secs().ok_or_else(|| {
                Problem::from(SandboxError::Validation {
                    message: "either extra_secs or duration must be provided".to_string(),
                })
            })?,
        )
        .await?;
    Ok(Json(build_response(&state, row).await))
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/pause",
    responses(
        (status = 200, description = "Sandbox paused (container stopped, state preserved)", body = SandboxResponse),
        (status = 404, description = "Not found"),
        (status = 409, description = "Sandbox is in an incompatible state (e.g. already destroyed)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn pause_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let row = state
        .sandbox_service
        .pause_sandbox(&id, auth.user_id())
        .await?;
    Ok(Json(build_response(&state, row).await))
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/resume",
    responses(
        (status = 200, description = "Sandbox resumed; expires_at refreshed", body = SandboxResponse),
        (status = 404, description = "Not found"),
        (status = 409, description = "Sandbox is not in a resumable state")
    ),
    security(("bearer_auth" = []))
)]
pub async fn resume_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let row = state
        .sandbox_service
        .resume_sandbox(&id, auth.user_id())
        .await?;
    Ok(Json(build_response(&state, row).await))
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/restart",
    responses(
        (status = 200, description = "Sandbox container restarted in place", body = SandboxResponse),
        (status = 404, description = "Not found"),
        (status = 409, description = "Sandbox is stopped (use /resume) or already destroyed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn restart_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let row = state
        .sandbox_service
        .restart_sandbox(&id, auth.user_id())
        .await?;
    Ok(Json(build_response(&state, row).await))
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/source",
    request_body = SourceBody,
    responses(
        (status = 200, description = "Source content seeded into the sandbox work dir", body = SandboxResponse),
        (status = 400, description = "Validation error (embedded creds, conflicting fields, etc.)"),
        (status = 404, description = "Sandbox not found"),
        (status = 409, description = "Sandbox is not running"),
        (status = 500, description = "Source seed failed inside sandbox")
    ),
    security(("bearer_auth" = []))
)]
pub async fn source_sandbox(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<SourceBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    if body.uses_stored_git_connection() {
        temps_auth::permission_guard!(auth, GitRepositoriesRead);
    }
    body.validate_async().await?;
    let source: SandboxSource = body.into();
    let row = state
        .sandbox_service
        .clone_source(&id, auth.user_id(), &source)
        .await?;
    Ok(Json(build_response(&state, row).await))
}

// ── Exec ────────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/exec",
    request_body = ExecBody,
    responses(
        (status = 200, description = "Command finished (non-zero exit is NOT an error)", body = ExecResponse),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn exec(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<ExecBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesExec, Permission::ProjectsWrite)?;
    let result = state
        .sandbox_service
        .exec(&id, auth.user_id(), body.into())
        .await?;
    Ok(Json(ExecResponse::from(result)))
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/exec-detached",
    request_body = ExecBody,
    responses(
        (status = 202, description = "Command accepted; poll /jobs/{job_id}", body = ExecDetachedResponse),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn exec_detached(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<ExecBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesExec, Permission::ProjectsWrite)?;
    let job_id = state
        .sandbox_service
        .exec_detached(&id, auth.user_id(), body.into())
        .await?;
    Ok((StatusCode::ACCEPTED, Json(ExecDetachedResponse { job_id })))
}

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/jobs",
    responses(
        (status = 200, description = "Detached jobs for this sandbox", body = ListJobsResponse),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_jobs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let items = state
        .sandbox_service
        .list_jobs(&id, auth.user_id())
        .await?
        .into_iter()
        .map(JobSummaryResponse::from)
        .collect();
    Ok(Json(ListJobsResponse { items }))
}

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/jobs/{job_id}",
    responses(
        (status = 200, description = "Job status snapshot", body = JobStatusResponse),
        (status = 404, description = "Sandbox or job not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn job_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path((id, job_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let state_snapshot = state
        .sandbox_service
        .job_status(&id, auth.user_id(), &job_id)
        .await?;
    Ok(Json(JobStatusResponse::from(state_snapshot)))
}

/// SSE endpoint streaming each stdout/stderr line from a detached job
/// as it's produced. Mirrors the `Command.logs()` async iterator shape
/// on `@vercel/sandbox` — events carry `{ stream, data }`.
///
/// Late subscribers only see events produced after they connect. The
/// JobState snapshot (`GET /jobs/{job_id}`) covers the history.
///
/// A "done" sentinel event fires when the broadcast channel closes
/// (the exec task has exited and dropped the sender), signalling
/// callers they can stop reading.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/jobs/{job_id}/logs",
    responses(
        (status = 200, description = "SSE stream of log events"),
        (status = 404, description = "Sandbox or job not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn job_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path((id, job_id)): Path<(String, String)>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let rx = state
        .sandbox_service
        .subscribe_job_logs(&id, auth.user_id(), &job_id)
        .await?;

    // `BroadcastStream` maps `RecvError::Lagged` → an error yielded on the
    // stream; we convert that into a sentinel `lagged` event so the client
    // can reconcile by polling `job_status` for the missed range. `Closed`
    // terminates the stream — we cap it with a `done` event.
    let live = BroadcastStream::new(rx).map(|msg| match msg {
        Ok(ev) => {
            let stream_name = match ev.stream {
                ExecStream::Stdout => "stdout",
                ExecStream::Stderr => "stderr",
            };
            let payload = serde_json::json!({
                "stream": stream_name,
                "data": ev.line,
            });
            Ok::<_, Infallible>(Event::default().event("log").data(payload.to_string()))
        }
        Err(_lagged) => Ok::<_, Infallible>(
            Event::default()
                .event("lagged")
                .data("subscriber fell behind; reconcile via GET /jobs/{job_id}"),
        ),
    });

    let done = stream::once(async { Ok::<_, Infallible>(Event::default().event("done").data("")) });

    let stream = live.chain(done);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[derive(Debug, Deserialize, ToSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct KillJobBody {
    /// When true, sends SIGKILL immediately. Defaults to SIGTERM so the
    /// process gets a chance to flush (mirrors `Command.kill()` in
    /// `@vercel/sandbox`, which also accepts a signal override).
    #[serde(default)]
    pub force: bool,
}

/// Terminate a detached job. Aborts the server-side tracking task and
/// sends SIGTERM (or SIGKILL if `force=true`) to any matching processes
/// inside the sandbox container. Returns 204 on success; 404 if the
/// sandbox or job is unknown.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/jobs/{job_id}/kill",
    request_body = KillJobBody,
    responses(
        (status = 204, description = "Job killed"),
        (status = 404, description = "Sandbox or job not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn kill_job(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path((id, job_id)): Path<(String, String)>,
    body: Option<Json<KillJobBody>>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesExec, Permission::ProjectsWrite)?;
    let Json(body) = body.unwrap_or(Json(KillJobBody::default()));
    state
        .sandbox_service
        .kill_job(&id, auth.user_id(), &job_id, body.force)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── SDK-compatible `/cmd` surface (`@vercel/sandbox` drop-in) ──────────────
//
// The Vercel SDK speaks a different command shape than our native `/exec`
// and `/jobs` endpoints: it sends `{command, args, cwd, env, sudo, wait}`
// and expects `{ command: { id, name, args, cwd, sandboxId, exitCode,
// startedAt } }` back. When `wait=true`, it consumes an
// `application/x-ndjson` stream where the first line carries the running
// command envelope and the second line carries the finished envelope.
//
// This is a thin adapter on top of `exec_detached` + `job_status`. The
// SDK's `cmdId` IS our internal `job_id` — no mapping table needed.

#[derive(Debug, Deserialize, ToSchema)]
pub struct CmdBody {
    /// Binary name (argv[0]) — e.g. `"ls"`, `"node"`. The SDK sends this
    /// separately from `args`.
    pub command: String,
    /// Arguments to pass to the binary. Defaults to empty.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory override.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Extra env vars.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// When true, the SDK runs the command privileged. We ignore it today
    /// — the underlying provider always runs as the sandbox's own user.
    #[serde(default)]
    pub sudo: bool,
    /// When true, the response is an `application/x-ndjson` stream where
    /// the first line is the running-command envelope and the second line
    /// is the finished-command envelope with `exitCode`.
    #[serde(default)]
    pub wait: bool,
}

impl CmdBody {
    fn into_exec_options(self) -> crate::services::exec::ExecOptions {
        let mut cmd = Vec::with_capacity(1 + self.args.len());
        cmd.push(self.command);
        cmd.extend(self.args);
        crate::services::exec::ExecOptions {
            cmd,
            env: self.env,
            cwd: self.cwd,
        }
    }
}

/// Inner `command` object — matches the SDK's zod validator exactly.
/// `exitCode` is `null` until the command terminates; `startedAt` is Unix
/// epoch milliseconds.
#[derive(Debug, Serialize, ToSchema)]
pub struct CmdInner {
    pub id: String,
    pub name: String,
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(rename = "sandboxId")]
    pub sandbox_id: String,
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i32>,
    #[serde(rename = "startedAt")]
    pub started_at: i64,
}

/// `@vercel/sandbox` envelope: `{ command: {...} }`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CmdResponse {
    pub command: CmdInner,
}

impl CmdInner {
    fn from_summary(sandbox_id: String, s: crate::services::job_tracker::JobSummary) -> Self {
        let (name, args) = split_cmd_display(&s.cmd);
        let exit_code = match s.status {
            JobStatus::Exited { exit_code } => Some(exit_code),
            _ => None,
        };
        Self {
            id: s.id,
            name,
            args,
            cwd: String::new(),
            sandbox_id,
            exit_code,
            started_at: s.started_at.timestamp_millis(),
        }
    }
}

/// `cmd_display` is `argv.join(" ")`. The SDK wants `name` + `args`
/// separately, so we split on the first space. Args that contained
/// spaces in the original input are collapsed — acceptable because the
/// SDK doesn't round-trip args through `name`.
fn split_cmd_display(s: &str) -> (String, Vec<String>) {
    let mut it = s.splitn(2, ' ');
    let name = it.next().unwrap_or("").to_string();
    let args = match it.next() {
        Some(rest) if !rest.is_empty() => rest.split(' ').map(String::from).collect(),
        _ => Vec::new(),
    };
    (name, args)
}

async fn fetch_cmd_summary(
    state: &Arc<SandboxAppState>,
    sandbox_id: &str,
    user_id: i32,
    cmd_id: &str,
) -> Result<crate::services::job_tracker::JobSummary, SandboxError> {
    let summaries = state.sandbox_service.list_jobs(sandbox_id, user_id).await?;
    summaries
        .into_iter()
        .find(|s| s.id == cmd_id)
        .ok_or_else(|| SandboxError::JobNotFound {
            sandbox_id: sandbox_id.to_string(),
            job_id: cmd_id.to_string(),
        })
}

/// Run a command inside the sandbox (`@vercel/sandbox`-compatible).
///
/// `wait=false` (default) returns `{ command: {..., exitCode: null} }`
/// immediately once the background task is spawned.
///
/// `wait=true` streams `application/x-ndjson`: the first line is the
/// running envelope, the second is the finished envelope with `exitCode`.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/cmd",
    request_body = CmdBody,
    responses(
        (status = 200, description = "Command started (wait=false) or finished (wait=true)", body = CmdResponse),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn cmd(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<CmdBody>,
) -> Result<axum::response::Response, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesExec, Permission::ProjectsWrite)?;

    let wait = body.wait;
    let options = body.into_exec_options();
    let user_id = auth.user_id();

    let cmd_id = state
        .sandbox_service
        .exec_detached(&id, user_id, options)
        .await?;
    let started = fetch_cmd_summary(&state, &id, user_id, &cmd_id).await?;

    if !wait {
        let env = CmdResponse {
            command: CmdInner::from_summary(id.clone(), started),
        };
        return Ok(Json(env).into_response());
    }

    // `wait=true` — stream ndjson. First line is the running envelope
    // (which we already have), second line is the finished envelope
    // produced by polling `job_status` until the job leaves `Running`.
    // Polling is acceptable here because job state transitions exactly
    // once; we aren't racing producers.
    let first = serde_json::to_vec(&CmdResponse {
        command: CmdInner::from_summary(id.clone(), started.clone()),
    })
    .map_err(|e| {
        Problem::from(SandboxError::Validation {
            message: e.to_string(),
        })
    })?;

    let sandbox_id = id.clone();
    let cmd_id_for_poll = cmd_id.clone();
    let state_for_poll = state.clone();
    let finished_stream = async_stream::stream! {
        // Emit the start envelope immediately so clients unblock on
        // `for await (const line of stream)`.
        let mut start = first;
        start.push(b'\n');
        yield Ok::<_, std::io::Error>(axum::body::Bytes::from(start));

        // Poll until the job is no longer Running. Exponential backoff
        // (50ms → 500ms) keeps short-lived commands snappy without
        // hammering the tracker for long-lived ones.
        let mut delay_ms = 50u64;
        loop {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            delay_ms = std::cmp::min(delay_ms.saturating_mul(2), 500);

            let summaries = match state_for_poll
                .sandbox_service
                .list_jobs(&sandbox_id, user_id)
                .await
            {
                Ok(s) => s,
                Err(_) => break,
            };
            let Some(summary) = summaries.into_iter().find(|s| s.id == cmd_id_for_poll)
            else {
                break;
            };
            if matches!(summary.status, JobStatus::Running) {
                continue;
            }
            let env = CmdResponse {
                command: CmdInner::from_summary(sandbox_id.clone(), summary),
            };
            if let Ok(mut bytes) = serde_json::to_vec(&env) {
                bytes.push(b'\n');
                yield Ok(axum::body::Bytes::from(bytes));
            }
            break;
        }
    };

    let body = axum::body::Body::from_stream(finished_stream);
    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-ndjson")
        .body(body)
        .map_err(|e| {
            Problem::from(SandboxError::Validation {
                message: e.to_string(),
            })
        })?;
    Ok(response)
}

/// Fetch a command's current state (`@vercel/sandbox`-compatible).
///
/// When `?wait=true`, blocks until the command terminates, then returns
/// the finished envelope. Otherwise returns the current snapshot with
/// `exitCode` potentially null.
#[derive(Debug, Deserialize)]
pub struct GetCmdQuery {
    #[serde(default)]
    pub wait: Option<String>,
}

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/cmd/{cmd_id}",
    responses(
        (status = 200, description = "Command snapshot", body = CmdResponse),
        (status = 404, description = "Sandbox or command not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_cmd(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path((id, cmd_id)): Path<(String, String)>,
    Query(q): Query<GetCmdQuery>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;

    let wait = matches!(q.wait.as_deref(), Some("true") | Some("1"));
    let user_id = auth.user_id();

    let mut delay_ms = 50u64;
    loop {
        let summary = fetch_cmd_summary(&state, &id, user_id, &cmd_id).await?;
        if !wait || !matches!(summary.status, JobStatus::Running) {
            return Ok(Json(CmdResponse {
                command: CmdInner::from_summary(id, summary),
            }));
        }
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        delay_ms = std::cmp::min(delay_ms.saturating_mul(2), 500);
    }
}

/// Stream a command's stdout/stderr as `application/x-ndjson`
/// (`@vercel/sandbox`-compatible). Each line is either
/// `{stream:"stdout"|"stderr", data:"..."}` or
/// `{stream:"error", data:{code, message}}`.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/cmd/{cmd_id}/logs",
    responses(
        (status = 200, description = "NDJSON stream of log events"),
        (status = 404, description = "Sandbox or command not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn cmd_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path((id, cmd_id)): Path<(String, String)>,
) -> Result<axum::response::Response, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;

    let rx = state
        .sandbox_service
        .subscribe_job_logs(&id, auth.user_id(), &cmd_id)
        .await?;

    let stream = BroadcastStream::new(rx).filter_map(|msg| async move {
        match msg {
            Ok(ev) => {
                let stream_name = match ev.stream {
                    ExecStream::Stdout => "stdout",
                    ExecStream::Stderr => "stderr",
                };
                let line = serde_json::json!({
                    "stream": stream_name,
                    "data": ev.line,
                });
                let mut bytes = line.to_string().into_bytes();
                bytes.push(b'\n');
                Some(Ok::<_, std::io::Error>(axum::body::Bytes::from(bytes)))
            }
            Err(_lagged) => {
                let line = serde_json::json!({
                    "stream": "error",
                    "data": {
                        "code": "lagged",
                        "message": "subscriber fell behind; reconcile via GET /cmd/{cmd_id}",
                    },
                });
                let mut bytes = line.to_string().into_bytes();
                bytes.push(b'\n');
                Some(Ok::<_, std::io::Error>(axum::body::Bytes::from(bytes)))
            }
        }
    });

    let body = axum::body::Body::from_stream(stream);
    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-ndjson")
        .body(body)
        .map_err(|e| {
            Problem::from(SandboxError::Validation {
                message: e.to_string(),
            })
        })?;
    Ok(response)
}

/// SDK-shaped kill body. The SDK sends `{signal: AbortSignal}` but only
/// uses the signal for HTTP request abortion client-side; there's no
/// signal name on the wire.
#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct CmdKillBody {
    /// Optional: when true, SIGKILL instead of SIGTERM.
    #[serde(default)]
    pub force: bool,
}

/// Kill a running command (`@vercel/sandbox`-compatible). The SDK
/// calls `POST /v1/sandboxes/{id}/{cmdId}/kill` — note the path has the
/// command ID directly under the sandbox, NOT under `/jobs/` or `/cmd/`.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/{cmd_id}/kill",
    responses(
        (status = 200, description = "Command killed; returns final snapshot", body = CmdResponse),
        (status = 404, description = "Sandbox or command not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn cmd_kill(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path((id, cmd_id)): Path<(String, String)>,
    body: Option<Json<CmdKillBody>>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesExec, Permission::ProjectsWrite)?;
    let force = body.map(|Json(b)| b.force).unwrap_or(false);
    let user_id = auth.user_id();

    // Snapshot before killing — `kill_job` removes the job from the
    // tracker, so we'd lose the summary otherwise.
    let summary = fetch_cmd_summary(&state, &id, user_id, &cmd_id).await?;

    state
        .sandbox_service
        .kill_job(&id, user_id, &cmd_id, force)
        .await?;

    Ok(Json(CmdResponse {
        command: CmdInner::from_summary(id, summary),
    }))
}

// ── Filesystem ──────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/fs/read",
    params(("path" = String, Query, description = "Absolute file path inside the sandbox")),
    responses(
        (status = 200, description = "File contents (base64)", body = ReadFileResponse),
        (status = 400, description = "Validation error"),
        (status = 404, description = "Sandbox or file not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn read_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Query(q): Query<ReadFileQuery>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let bytes = state
        .sandbox_service
        .fs_read(&id, auth.user_id(), &q.path)
        .await?;
    let size = bytes.len() as u64;
    Ok(Json(ReadFileResponse {
        path: q.path,
        contents_b64: B64.encode(&bytes),
        size,
    }))
}

/// Write a file into the sandbox. Accepts two body shapes — the SDK
/// picks one based on `Content-Type`:
///
/// - **`application/json`** (temps-native): `{path, contents_b64, mode}`
///   — one file, base64-encoded.
/// - **`application/gzip`** (`@vercel/sandbox`): a gzipped tarball of
///   one-or-more entries, with the target extract dir carried in the
///   `x-cwd` header. The SDK's `writeFile` and `writeFiles` both post
///   here; they differ only in how many entries the tarball contains.
///
/// Why merge them on one route: the SDK is hardcoded to
/// `POST /fs/write`, so splitting tar uploads onto a separate path would
/// force us to break SDK compat. Instead we dispatch on Content-Type,
/// preserve JSON for native callers, and add tar for SDK callers.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/fs/write",
    request_body = WriteFileBody,
    responses(
        (status = 204, description = "File(s) written"),
        (status = 400, description = "Validation error or invalid base64"),
        (status = 404, description = "Sandbox not found"),
        (status = 415, description = "Unsupported Content-Type (expected application/json or application/gzip)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn write_file(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // SDK path: gzipped tar. The `x-cwd` header tells us where to extract.
    if content_type.starts_with("application/gzip")
        || content_type.starts_with("application/x-gzip")
        || content_type.starts_with("application/x-tar+gzip")
    {
        let cwd = headers
            .get("x-cwd")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let entries = extract_tar_gz(&body).map_err(|e| {
            Problem::from(SandboxError::Validation {
                message: format!("failed to decode gzipped tar body: {}", e),
            })
        })?;
        let user_id = auth.user_id();
        for entry in entries {
            let abs_path = resolve_extract_path(&cwd, &entry.name);
            state
                .sandbox_service
                .fs_write(&id, user_id, &abs_path, &entry.content, entry.mode)
                .await?;
        }
        return Ok(StatusCode::OK);
    }

    // Native JSON path: unchanged behaviour.
    if content_type.starts_with("application/json") || content_type.is_empty() {
        let body_parsed: WriteFileBody = serde_json::from_slice(&body).map_err(|e| {
            Problem::from(SandboxError::Validation {
                message: format!("invalid JSON body: {}", e),
            })
        })?;
        let contents = B64
            .decode(body_parsed.contents_b64.as_bytes())
            .map_err(|e| {
                Problem::from(SandboxError::Validation {
                    message: format!("contents_b64 is not valid base64: {}", e),
                })
            })?;
        let mode = body_parsed.mode.unwrap_or(0o644);
        state
            .sandbox_service
            .fs_write(&id, auth.user_id(), &body_parsed.path, &contents, mode)
            .await?;
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(Problem::from(SandboxError::Validation {
        message: format!(
            "unsupported Content-Type '{}'; expected 'application/json' or 'application/gzip'",
            content_type
        ),
    }))
}

/// One extracted tar entry. We only carry what `fs_write` needs.
#[derive(Debug)]
struct TarEntry {
    name: String,
    content: Vec<u8>,
    mode: u32,
}

/// Extract a gzipped tar stream into in-memory entries. Bounded by the
/// request's size limit — callers control that upstream. We skip
/// directory entries (the sandbox's `write_file` auto-creates parents).
///
/// Entry names are validated to reject any `..` ParentDir component or
/// embedded NUL byte. Without this, a crafted tarball could escape the
/// caller-supplied `x-cwd` via `../../etc/...` and overwrite host paths
/// inside the sandbox container — `validate_absolute` is the second
/// gate, but tar names should never reach the FS layer with traversal
/// segments to begin with.
fn extract_tar_gz(body: &[u8]) -> Result<Vec<TarEntry>, String> {
    use std::io::Read;
    // Fix #27: decompression-bomb limits.
    // Sandbox uploads are tighter than deployment bundles (200 MiB total / 50 MiB per entry)
    // because sandboxes are interactive containers with tighter resource envelopes.
    const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024; //  200 MiB aggregate
    const MAX_ENTRY_BYTES: u64 = 50 * 1024 * 1024; //   50 MiB per entry
    let gz = flate2::read::GzDecoder::new(body);
    let mut archive = tar::Archive::new(gz);
    let mut out = Vec::new();
    let mut total_bytes: u64 = 0;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let header = entry.header();
        let entry_type = header.entry_type();
        if entry_type.is_dir() {
            continue;
        }
        // Fix #19: skip symlinks and hardlinks — they can reference paths outside
        // the container's working directory and be followed by the file-serving layer.
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            tracing::warn!(
                entry_type = ?entry_type,
                "Skipping symlink/hardlink entry in sandbox tar upload (security)"
            );
            continue;
        }
        if !entry_type.is_file() {
            tracing::warn!(
                entry_type = ?entry_type,
                "Skipping non-regular tar entry in sandbox upload"
            );
            continue;
        }
        let name = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        if name.is_empty() {
            return Err("tar entry has empty path".into());
        }
        if name.contains('\0') {
            return Err(format!("tar entry path contains NUL byte: {:?}", name));
        }
        if std::path::Path::new(&name)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!(
                "tar entry path contains '..' traversal segment: {:?}",
                name
            ));
        }
        // Fix #27: per-entry size check on declared header size (fires before reading content).
        let entry_size = header.size().unwrap_or(0);
        if entry_size > MAX_ENTRY_BYTES {
            return Err(format!(
                "tar entry '{}' declared size {} exceeds per-entry limit of {} bytes",
                name, entry_size, MAX_ENTRY_BYTES
            ));
        }
        total_bytes = total_bytes.saturating_add(entry_size);
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "tar archive total decompressed size exceeds limit of {} bytes",
                MAX_TOTAL_BYTES
            ));
        }
        let mode = header.mode().map_err(|e| e.to_string())? & 0o7777;
        let mut content = Vec::with_capacity(entry_size as usize);
        entry.read_to_end(&mut content).map_err(|e| e.to_string())?;
        out.push(TarEntry {
            name,
            content,
            mode: if mode == 0 { 0o644 } else { mode },
        });
    }
    Ok(out)
}

/// Combine the SDK's `x-cwd` extract directory with a tarball entry
/// path. When the entry is already absolute, it wins (the SDK encodes
/// full paths in `writeFile` single-file uploads); otherwise we join
/// under the cwd. When cwd is empty, treat entries as absolute.
fn resolve_extract_path(cwd: &str, entry_name: &str) -> String {
    if entry_name.starts_with('/') {
        return entry_name.to_string();
    }
    if cwd.is_empty() {
        return if entry_name.starts_with('/') {
            entry_name.to_string()
        } else {
            format!("/{}", entry_name)
        };
    }
    let cwd = cwd.trim_end_matches('/');
    let entry = entry_name.trim_start_matches("./");
    format!("{}/{}", cwd, entry)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFilesBody {
    /// List of files to write. Each entry must include an absolute
    /// `path` and base64-encoded `contents_b64`. Empty list is a no-op.
    pub files: Vec<WriteFileBody>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WriteFilesResponse {
    /// Number of files successfully written before the first failure
    /// (if any). On full success this equals `files.len()`.
    pub written: usize,
}

/// Batch-write multiple files in a single request. Mirrors
/// `@vercel/sandbox` `writeFiles()`. Semantics are fail-fast: if any
/// file errors, previously-written entries are left in place and the
/// error describes which file broke.
#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/fs/write-batch",
    request_body = WriteFilesBody,
    responses(
        (status = 200, description = "All files written", body = WriteFilesResponse),
        (status = 400, description = "Validation error or invalid base64"),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn write_files(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<WriteFilesBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let mut entries = Vec::with_capacity(body.files.len());
    for f in body.files {
        let contents = B64.decode(f.contents_b64.as_bytes()).map_err(|e| {
            Problem::from(SandboxError::Validation {
                message: format!("contents_b64 for '{}' is not valid base64: {}", f.path, e),
            })
        })?;
        entries.push(crate::services::fs::BatchWriteEntry {
            path: f.path,
            contents,
            mode: f.mode,
        });
    }
    let written = state
        .sandbox_service
        .fs_write_batch(&id, auth.user_id(), entries)
        .await?;
    Ok(Json(WriteFilesResponse { written }))
}

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/fs/stat",
    params(("path" = String, Query, description = "Absolute path inside the sandbox")),
    responses(
        (status = 200, description = "Stat info (exists=false when missing — not an error)", body = StatResponse),
        (status = 400, description = "Validation error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn stat_path(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Query(q): Query<StatQuery>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let info = state
        .sandbox_service
        .fs_stat(&id, auth.user_id(), &q.path)
        .await?;
    Ok(Json(StatResponse::from(info)))
}

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/fs/list",
    params(
        ("path" = String, Query, description = "Absolute path of a directory inside the sandbox"),
        ("page" = Option<usize>, Query, description = "1-based page (default 1)"),
        ("page_size" = Option<usize>, Query, description = "Entries per page (default 20, max 100)")
    ),
    responses(
        (status = 200, description = "One page of directory entries (exists=false when missing, is_dir=false for a file — not errors)", body = DirectoryListingResponse),
        (status = 400, description = "Validation error, or the directory is not readable by the sandbox user"),
        (status = 404, description = "Sandbox not found"),
        (status = 409, description = "Sandbox is not running (stopped or paused), so its filesystem cannot be listed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_dir(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Query(q): Query<ListDirQuery>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let listing = state
        .sandbox_service
        .fs_list(&id, auth.user_id(), &q.path, q.page, q.page_size)
        .await?;
    Ok(Json(DirectoryListingResponse::from(listing)))
}

#[utoipa::path(
    tag = "Sandboxes",
    post,
    path = "/v1/sandboxes/{id}/fs/mkdir",
    request_body = MkdirBody,
    responses(
        (status = 204, description = "Directory created (or already existed)"),
        (status = 400, description = "Validation error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn mkdir(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<MkdirBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    state
        .sandbox_service
        .fs_mkdir(&id, auth.user_id(), &body.path)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Preview URL (`domain(port)`) ────────────────────────────────────────────

#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/domain",
    params(("port" = u16, Query, description = "Port inside the sandbox (1..=65535)")),
    responses(
        (status = 200, description = "Preview URL for the port", body = SandboxDomainResponse),
        (status = 400, description = "Invalid port"),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Query(q): Query<DomainQuery>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::ProjectsRead)?;
    let url = state
        .sandbox_service
        .domain(&id, auth.user_id(), q.port)
        .await?;
    Ok(Json(SandboxDomainResponse { url }))
}

// ── Preview password ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetPreviewPasswordBody {
    /// Plaintext password to protect the sandbox's preview URLs. Hashed
    /// server-side with argon2id — we never persist or echo this back.
    /// Must be between 8 and 256 characters.
    pub password: SecretString,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SetPreviewPasswordResponse {
    /// Last 4 chars of the password we just stored. Surface in the UI so
    /// users can confirm which password is live without re-entering it.
    pub preview_password_hint: String,
}

#[utoipa::path(
    tag = "Sandboxes",
    put,
    path = "/v1/sandboxes/{id}/preview-password",
    request_body = SetPreviewPasswordBody,
    responses(
        (status = 200, description = "Preview password set or rotated", body = SetPreviewPasswordResponse),
        (status = 400, description = "Password too short or too long"),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_preview_password(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(id): Path<String>,
    Json(body): Json<SetPreviewPasswordBody>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let user_id = auth.user_id();
    let hint = state
        .sandbox_service
        .set_preview_password(&id, user_id, body.password.expose())
        .await?;
    let event = super::preview_access::SandboxPreviewPasswordAudit::set(
        super::preview_access::audit_context(user_id, &metadata),
        &id,
    );
    super::preview_access::write_audit(&state, &event, &id).await;
    Ok(Json(SetPreviewPasswordResponse {
        preview_password_hint: hint,
    }))
}

#[utoipa::path(
    tag = "Sandboxes",
    delete,
    path = "/v1/sandboxes/{id}/preview-password",
    responses(
        (status = 204, description = "Preview password removed; a sandbox in `password` access returns to `temps_login`"),
        (status = 404, description = "Sandbox not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn clear_preview_password(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;
    let user_id = auth.user_id();
    let access = state
        .sandbox_service
        .clear_preview_password(&id, user_id)
        .await?;
    let event = super::preview_access::SandboxPreviewPasswordAudit::cleared(
        super::preview_access::audit_context(user_id, &metadata),
        &id,
        access,
    );
    super::preview_access::write_audit(&state, &event, &id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ── Routing ─────────────────────────────────────────────────────────────────

// ── Shareable preview link ──────────────────────────────────────────────────

/// Request body for minting a preview share link.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PreviewShareLinkBody {
    /// Port inside the sandbox the preview serves on.
    pub port: u16,
    /// Path the recipient lands on. Must be same-origin (start with a single
    /// `/`); anything else is replaced with `/` so a share link can never be
    /// turned into an open redirect.
    #[serde(default)]
    pub path: Option<String>,
    /// How long the link stays usable, in seconds. Clamped to 24 hours.
    /// Defaults to one hour — long enough to send to a reviewer, short enough
    /// that a link pasted in a ticket does not stay live indefinitely.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PreviewShareLinkResponse {
    /// The full link. Its fragment contains the grant and must be treated as a
    /// credential; URL fragments are not sent to servers or in Referer headers.
    pub url: String,
    /// Unix seconds after which the link stops working.
    pub expires_at: u64,
}

/// Live project runtime variables issued to a sandbox. Values are deliberately
/// absent from Debug output and must not be cached or persisted by Temps.
#[derive(Serialize, ToSchema)]
pub struct SandboxRuntimeEnvironmentResponse {
    pub variables: HashMap<String, String>,
}

impl std::fmt::Debug for SandboxRuntimeEnvironmentResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut variable_names = self.variables.keys().collect::<Vec<_>>();
        variable_names.sort();
        formatter
            .debug_struct("SandboxRuntimeEnvironmentResponse")
            .field("variable_names", &variable_names)
            .finish()
    }
}

/// Issue the same scoped service variables used by a deployment runtime for
/// the project attached to this sandbox.
#[utoipa::path(
    tag = "Sandboxes",
    operation_id = "sandbox_issue_runtime_environment",
    post,
    path = "/v1/sandboxes/{id}/runtime-environment",
    responses(
        (status = 200, description = "Runtime variables issued", body = SandboxRuntimeEnvironmentResponse),
        (status = 400, description = "Sandbox has no attached project"),
        (status = 403, description = "Plaintext secret access is not permitted"),
        (status = 404, description = "Sandbox or project environment not found"),
        (status = 409, description = "Linked services expose ambiguous variables"),
        (status = 503, description = "Runtime credential provider unavailable")
    ),
    security(("bearer_auth" = []))
)]
pub async fn issue_runtime_environment(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    sandbox_permission_guard(&auth, Permission::SandboxesExec, Permission::ProjectsWrite)?;
    if !auth.has_permission(&Permission::SecretsRead) {
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/insufficient-permissions")
                .title("Insufficient Permissions")
                .detail("Issuing sandbox runtime variables requires the secrets:read permission")
                .value("required_permission", Permission::SecretsRead.to_string())
                .value("user_role", auth.effective_role.to_string())
                .build(),
        );
    }
    let sandbox = state
        .sandbox_service
        .find_by_public_id(&id, auth.user_id())
        .await?;
    if let Some(project_id) = sandbox.project_id {
        project_scope_guard!(auth, project_id);
        project_access_guard!(auth, project_id, state.project_access_checker);
    }
    let variables = state
        .sandbox_service
        .runtime_environment(auth.user_id(), &id)
        .await?;
    Ok((
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(SandboxRuntimeEnvironmentResponse { variables }),
    ))
}

/// Mint a shareable link to a sandbox preview.
///
/// `GET /domain` returns the bare preview URL, which is useless to anyone who
/// does not already hold the sandbox's preview password — so sharing a
/// protected preview meant sharing that password, which is the same secret for
/// every recipient and can only be withdrawn by rotating it for all of them.
///
/// This returns the same URL carrying a short-lived, sandbox-scoped grant. The
/// recipient's browser exchanges it for the ordinary preview cookie and lands
/// on `path`. The grant never reaches the sandbox, so preview application code
/// cannot read it and re-share it.
///
/// Anyone holding the returned URL can view the preview until it expires;
/// there is no per-link revocation short of rotating the preview password.
#[utoipa::path(
    tag = "Sandboxes",
    operation_id = "sandbox_create_preview_link",
    post,
    path = "/v1/sandboxes/{id}/preview-link",
    request_body = PreviewShareLinkBody,
    responses(
        (status = 200, description = "Shareable preview link", body = PreviewShareLinkResponse),
        (status = 400, description = "Invalid port"),
        (status = 404, description = "Sandbox not found"),
        (status = 409, description = "Sandbox has no preview password"),
        (status = 500, description = "Preview grant minting failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_preview_link(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(id): Path<String>,
    Json(body): Json<PreviewShareLinkBody>,
) -> Result<impl IntoResponse, Problem> {
    // Write, not read: this mints a credential that grants someone else access,
    // which is a stronger act than reading the sandbox yourself. Matches the
    // gate on `preview-password`.
    sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)?;

    let ttl = std::time::Duration::from_secs(body.ttl_seconds.unwrap_or(60 * 60));
    let (url, expires_at) = state
        .sandbox_service
        .preview_share_link(
            &id,
            auth.user_id(),
            body.port,
            body.path.as_deref().unwrap_or("/"),
            ttl,
        )
        .await?;

    Ok(Json(PreviewShareLinkResponse { url, expires_at }))
}

pub fn routes() -> Router<Arc<SandboxAppState>> {
    Router::new()
        .route("/v1/sandboxes", post(create_sandbox).get(list_sandboxes))
        // Rootfs management. Registered before `/{id}` — a static segment so
        // it never collides with the id capture.
        .route("/v1/sandboxes/rootfs", get(rootfs_report))
        // Sandbox placement (ADR-048) — static segment, same reason.
        .route(
            "/v1/sandboxes/placement",
            get(super::placement::get_sandbox_placement)
                .put(super::placement::update_sandbox_placement),
        )
        .route(
            "/v1/sandboxes/placement/nodes/{node}",
            get(super::placement::list_node_sandboxes),
        )
        .route(
            "/v1/sandboxes/placement/nodes/{node}/evict",
            post(super::placement::evict_node_sandboxes),
        )
        .route("/v1/sandboxes/rootfs/gc", post(rootfs_gc))
        .route("/v1/sandboxes/{id}", get(get_sandbox))
        .route(
            "/v1/sandboxes/{id}/terminal",
            get(crate::handlers::terminal::terminal),
        )
        .route("/v1/sandboxes/{id}/stop", post(stop_sandbox))
        .route("/v1/sandboxes/{id}/destroy", post(destroy_sandbox))
        .route("/v1/sandboxes/{id}/events", get(list_events))
        .route("/v1/sandboxes/{id}/resize", post(resize_sandbox))
        .route("/v1/sandboxes/{id}/egress-policy", put(set_egress_policy))
        .route("/v1/sandboxes/{id}/pause", post(pause_sandbox))
        .route("/v1/sandboxes/{id}/resume", post(resume_sandbox))
        .route("/v1/sandboxes/{id}/restart", post(restart_sandbox))
        .route("/v1/sandboxes/{id}/source", post(source_sandbox))
        .route("/v1/sandboxes/{id}/extend-timeout", post(extend_timeout))
        .route("/v1/sandboxes/{id}/exec", post(exec))
        .route(
            "/v1/sandboxes/{id}/runtime-environment",
            post(issue_runtime_environment),
        )
        .route("/v1/sandboxes/{id}/exec-detached", post(exec_detached))
        .route("/v1/sandboxes/{id}/jobs", get(list_jobs))
        .route("/v1/sandboxes/{id}/jobs/{job_id}", get(job_status))
        .route("/v1/sandboxes/{id}/jobs/{job_id}/logs", get(job_logs))
        .route("/v1/sandboxes/{id}/jobs/{job_id}/kill", post(kill_job))
        // `@vercel/sandbox`-compatible command surface. Wires through the
        // same `exec_detached` machinery as `/exec-detached` but speaks
        // the SDK's shape (`{command, args, cwd, env, sudo, wait}` /
        // `{command: {...}}`) and uses the SDK's path layout (`/cmd`,
        // `/cmd/{cmdId}`, `/cmd/{cmdId}/logs`, `/{cmdId}/kill`).
        .route("/v1/sandboxes/{id}/cmd", post(cmd))
        .route("/v1/sandboxes/{id}/cmd/{cmd_id}", get(get_cmd))
        .route("/v1/sandboxes/{id}/cmd/{cmd_id}/logs", get(cmd_logs))
        .route("/v1/sandboxes/{id}/{cmd_id}/kill", post(cmd_kill))
        .route("/v1/sandboxes/{id}/fs/read", get(read_file))
        // `/fs/write` accepts JSON (single file, base64) or `application/gzip`
        // tar uploads from the Vercel SDK. Cap at 64 MiB so a buggy client
        // (or a malicious one) can't drive control-plane memory exhaustion
        // — `read_to_end` buffers the whole tar before forwarding to Docker.
        .route(
            "/v1/sandboxes/{id}/fs/write",
            post(write_file).layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/v1/sandboxes/{id}/fs/write-batch",
            post(write_files).layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/v1/sandboxes/{id}/fs/stat", get(stat_path))
        .route("/v1/sandboxes/{id}/fs/list", get(list_dir))
        .route("/v1/sandboxes/{id}/fs/mkdir", post(mkdir))
        .route("/v1/sandboxes/{id}/domain", get(domain))
        .route(
            "/v1/sandboxes/{id}/preview-password",
            put(set_preview_password).delete(clear_preview_password),
        )
        .route(
            "/v1/sandboxes/{id}/preview-access",
            put(super::preview_access::update_preview_access),
        )
        .route("/v1/sandboxes/{id}/preview-link", post(create_preview_link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    fn test_user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 1,
            name: "Platform Admin".to_string(),
            email: "platform-admin@example.com".to_string(),
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
        }
    }

    /// Placement writes, the node sandbox listing and node eviction expose
    /// or destroy other users' sandboxes (ADR-048): admins only.
    #[test]
    fn only_admins_pass_the_sandbox_admin_guard() {
        let admin = AuthContext::new_session(test_user(), Role::Admin);
        assert!(require_sandbox_admin(&admin).is_ok());
        for role in [
            Role::User,
            Role::Reader,
            Role::PlatformAdmin,
            Role::ApiReader,
        ] {
            let auth = AuthContext::new_session(test_user(), role.clone());
            let problem = require_sandbox_admin(&auth).expect_err("non-admin must be refused");
            assert_eq!(problem.status_code, StatusCode::FORBIDDEN, "{role:?}");
        }
    }

    #[test]
    fn placement_errors_map_to_actionable_statuses() {
        let cases = [
            (
                SandboxError::NodeNotFound { node: "w9".into() },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                SandboxError::NodeNotAllowed { node: "w1".into() },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                SandboxError::NodeNotReady {
                    node: "w1".into(),
                    status: "offline".into(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                SandboxError::NoPlacementNode,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                SandboxError::DedicatedNodesOnly {
                    nodes: "node 7 (gpu-1) is dedicated".into(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                SandboxError::Unavailable {
                    reason: "worker node 'w2' is unavailable".into(),
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                SandboxError::NodeUnreachable {
                    sandbox_id: "sbx_1".into(),
                    node: "w2".into(),
                    reason: "it did not answer within 30s".into(),
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                SandboxError::ImageUnavailable {
                    sandbox_id: "sbx_1".into(),
                    image: "registry.example.test/team/img:1.0.0".into(),
                    reason: "manifest unknown".into(),
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (error, status) in cases {
            let label = error.to_string();
            assert_eq!(Problem::from(error).status_code, status, "{label}");
        }
    }

    #[test]
    fn image_unavailable_problem_names_the_image_and_remediation_without_the_registry_error() {
        let problem = Problem::from(SandboxError::ImageUnavailable {
            sandbox_id: "sbx_1".into(),
            image: "registry.example.test/team/img:1.0.0".into(),
            reason: "manifest unknown".into(),
        });
        assert_eq!(problem.status_code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(problem.body["title"], "Sandbox Image Unavailable");
        let detail = problem.body["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("registry.example.test/team/img:1.0.0"));
        assert!(detail.contains("sbx_1"));
        assert!(detail.contains("docker pull"));
        // The registry's own text stays in the server log, not the response.
        assert!(!detail.contains("manifest unknown"));
    }

    #[test]
    fn platform_admin_cannot_pass_create_sandbox_permission_guard() {
        let auth = AuthContext::new_session(test_user(), Role::PlatformAdmin);

        let problem =
            sandbox_permission_guard(&auth, Permission::SandboxesWrite, Permission::ProjectsWrite)
                .expect_err("PlatformAdmin must not be allowed to create a sandbox");

        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }

    /// Changing what a sandbox may reach is a write: readers are refused the
    /// guard `set_egress_policy` runs, users pass it.
    #[test]
    fn only_writers_may_change_a_sandbox_egress_policy() {
        for role in [Role::Reader, Role::ApiReader, Role::PlatformAdmin] {
            let auth = AuthContext::new_session(test_user(), role.clone());
            let problem = sandbox_permission_guard(
                &auth,
                Permission::SandboxesWrite,
                Permission::ProjectsWrite,
            )
            .expect_err("a reader must not change egress");
            assert_eq!(problem.status_code, StatusCode::FORBIDDEN, "{role:?}");
        }
        let user = AuthContext::new_session(test_user(), Role::User);
        assert!(sandbox_permission_guard(
            &user,
            Permission::SandboxesWrite,
            Permission::ProjectsWrite
        )
        .is_ok());
    }

    /// The request body is the policy itself; a typo in it is a 400, not
    /// a silently wider policy.
    #[test]
    fn the_egress_policy_body_rejects_unknown_fields() {
        let typo = r#"{"mode":"restricted","rulez":[{"host":"api.example.com"}]}"#;
        assert!(serde_json::from_str::<temps_agents::sandbox::SandboxEgressPolicy>(typo).is_err());
        let body: temps_agents::sandbox::SandboxEgressPolicy = serde_json::from_str(
            r#"{"mode":"restricted","rules":[{"host":"api.example.com","ports":[443]},{"cidr":"1.1.1.0/24"}]}"#,
        )
        .expect("documented shape");
        assert!(body.is_restricted());
        assert_eq!(body.rules.len(), 2);
    }

    // ── Tar extraction (SDK fs/write path) ─────────────────────────────────

    fn build_gzip_tar(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            for (name, content, mode) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(content.len() as u64);
                header.set_mode(*mode);
                header.set_cksum();
                builder.append(&header, *content).unwrap();
            }
            builder.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    #[test]
    fn extract_tar_gz_round_trips_entries() {
        let body = build_gzip_tar(&[
            ("hello.txt", b"hi", 0o644),
            ("subdir/data.bin", b"\x00\x01\x02", 0o600),
        ]);
        let entries = extract_tar_gz(&body).expect("decodes");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "hello.txt");
        assert_eq!(entries[0].content, b"hi");
        assert_eq!(entries[0].mode, 0o644);
        assert_eq!(entries[1].name, "subdir/data.bin");
        assert_eq!(entries[1].content, b"\x00\x01\x02");
        assert_eq!(entries[1].mode, 0o600);
    }

    #[test]
    fn extract_tar_gz_rejects_garbage() {
        assert!(extract_tar_gz(b"not a gzip stream").is_err());
    }

    #[test]
    fn resolve_extract_path_joins_cwd() {
        assert_eq!(
            resolve_extract_path("/workspace", "a.txt"),
            "/workspace/a.txt"
        );
        assert_eq!(
            resolve_extract_path("/workspace/", "a.txt"),
            "/workspace/a.txt"
        );
        assert_eq!(
            resolve_extract_path("/workspace", "./a.txt"),
            "/workspace/a.txt"
        );
    }

    #[test]
    fn resolve_extract_path_respects_absolute_entries() {
        assert_eq!(
            resolve_extract_path("/workspace", "/etc/config"),
            "/etc/config"
        );
    }

    #[test]
    fn resolve_extract_path_handles_empty_cwd() {
        assert_eq!(resolve_extract_path("", "a.txt"), "/a.txt");
    }

    #[test]
    fn job_status_running_serializes_without_exit_or_reason() {
        let resp = JobStatusResponse::from(JobState {
            status: JobStatus::Running,
            stdout: "x".into(),
            ..JobState::default()
        });
        assert_eq!(resp.status, "running");
        assert!(resp.exit_code.is_none());
        assert!(resp.reason.is_none());
    }

    #[test]
    fn job_status_exited_carries_code() {
        let resp = JobStatusResponse::from(JobState {
            status: JobStatus::Exited { exit_code: 7 },
            ..JobState::default()
        });
        assert_eq!(resp.status, "exited");
        assert_eq!(resp.exit_code, Some(7));
    }

    #[test]
    fn job_status_failed_carries_reason() {
        let resp = JobStatusResponse::from(JobState {
            status: JobStatus::Failed {
                reason: "provider down".into(),
            },
            ..JobState::default()
        });
        assert_eq!(resp.status, "failed");
        assert_eq!(resp.reason.as_deref(), Some("provider down"));
    }

    #[test]
    fn sandbox_response_converts_summary() {
        let now = Utc::now();
        let summary = SandboxSummary {
            allow_docker: false,
            public_id: "sbx_abc".into(),
            node_id: None,
            node_name: "control-plane".into(),
            egress_policy: Default::default(),
            name: "name".into(),
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            created_at: now,
            expires_at: now,
            preview_password_hint: None,
            preview_access: Default::default(),
            preview_allowed_cidrs: Vec::new(),
            ports: vec![],
            backend: None,
            disk_size_mb: None,
            agent_run_id: None,
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        };
        let r = SandboxResponse::from(summary);
        assert_eq!(r.sandbox.id, "sbx_abc");
        assert_eq!(r.sandbox.status, "running");
        // `@vercel/sandbox` expects epoch millisecond timestamps, not ISO strings.
        assert_eq!(r.sandbox.created_at, now.timestamp_millis());
        assert!(r.routes.is_empty());
    }

    #[test]
    fn sandbox_response_populates_routes_from_ports() {
        use crate::services::preview_urls::PreviewUrlParts;

        let now = Utc::now();
        let summary = SandboxSummary {
            allow_docker: false,
            public_id: "sbx_abcd1234ef567890".into(),
            node_id: None,
            node_name: "control-plane".into(),
            egress_policy: Default::default(),
            name: "name".into(),
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            created_at: now,
            expires_at: now,
            preview_password_hint: None,
            preview_access: Default::default(),
            preview_allowed_cidrs: Vec::new(),
            ports: vec![3000, 5173],
            backend: None,
            disk_size_mb: None,
            agent_run_id: None,
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        };
        let parts = PreviewUrlParts {
            protocol: "https".into(),
            domain: "localho.st".into(),
            port: None,
        };
        let r = SandboxResponse::with_template(summary, String::new(), &parts);
        assert_eq!(r.routes.len(), 2);
        assert_eq!(r.routes[0].port, 3000);
        assert_eq!(r.routes[0].subdomain, "ws-abcd1234ef567890-3000");
        assert_eq!(
            r.routes[0].url,
            "https://ws-abcd1234ef567890-3000.localho.st"
        );
        assert_eq!(r.routes[1].port, 5173);
    }

    #[test]
    fn allow_docker_defaults_off_and_round_trips() {
        let body: CreateSandboxBody = serde_json::from_str("{}").unwrap();
        assert!(!CreateSandboxRequest::from(body).allow_docker);
        let body: CreateSandboxBody =
            serde_json::from_str(r#"{"backend":"microsandbox","allow_docker":true}"#).unwrap();
        assert!(CreateSandboxRequest::from(body).allow_docker);
    }

    #[test]
    fn create_body_converts_to_request() {
        let body = CreateSandboxBody {
            image: Some("node:20".into()),
            timeout_secs: Some(120),
            ..Default::default()
        };
        let req: CreateSandboxRequest = body.into();
        assert_eq!(req.image.as_deref(), Some("node:20"));
        assert_eq!(req.timeout_secs, Some(120));
    }

    fn request_with_limits(
        cpu: Option<f64>,
        memory: Option<u64>,
        pids: Option<i64>,
        disk: Option<u64>,
    ) -> CreateSandboxRequest {
        CreateSandboxRequest::from(CreateSandboxBody {
            cpu_limit: cpu,
            memory_limit_mb: memory,
            pids_limit: pids,
            disk_size_mb: disk,
            ..Default::default()
        })
    }

    #[test]
    fn resource_limits_default_and_in_range_are_accepted() {
        assert!(validate_resource_limits(&request_with_limits(None, None, None, None)).is_ok());
        assert!(validate_resource_limits(&request_with_limits(
            Some(MAX_SANDBOX_CPU_CORES),
            Some(MAX_SANDBOX_MEMORY_MB),
            Some(MAX_SANDBOX_PIDS_LIMIT),
            Some(MAX_SANDBOX_DISK_MB),
        ))
        .is_ok());
        assert!(validate_resource_limits(&request_with_limits(
            Some(MIN_SANDBOX_CPU_CORES),
            Some(MIN_SANDBOX_MEMORY_MB),
            Some(1),
            Some(1)
        ))
        .is_ok());
    }

    #[test]
    fn unlimited_pids_limit_is_rejected() {
        for pids in [-1, 0, MAX_SANDBOX_PIDS_LIMIT + 1] {
            let err = validate_resource_limits(&request_with_limits(None, None, Some(pids), None))
                .unwrap_err();
            assert!(
                matches!(&err, SandboxError::Validation { message } if message.contains("pids_limit")),
                "pids_limit {pids} must be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn oversized_or_zero_memory_is_rejected() {
        // The value from the report: 4 TiB.
        for memory in [
            0,
            MIN_SANDBOX_MEMORY_MB - 1,
            MAX_SANDBOX_MEMORY_MB + 1,
            4_194_304,
        ] {
            let err =
                validate_resource_limits(&request_with_limits(None, Some(memory), None, None))
                    .unwrap_err();
            assert!(
                matches!(&err, SandboxError::Validation { message } if message.contains("memory_limit_mb")),
                "memory_limit_mb {memory} must be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn invalid_cpu_is_rejected() {
        for cpu in [
            0.0,
            -1.0,
            1e-12,
            MIN_SANDBOX_CPU_CORES / 2.0,
            f64::NAN,
            f64::INFINITY,
            MAX_SANDBOX_CPU_CORES + 0.5,
        ] {
            let err = validate_resource_limits(&request_with_limits(Some(cpu), None, None, None))
                .unwrap_err();
            assert!(
                matches!(&err, SandboxError::Validation { message } if message.contains("cpu_limit")),
                "cpu_limit {cpu} must be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn oversized_or_zero_disk_is_rejected() {
        for disk in [0, MAX_SANDBOX_DISK_MB + 1] {
            let err = validate_resource_limits(&request_with_limits(None, None, None, Some(disk)))
                .unwrap_err();
            assert!(
                matches!(&err, SandboxError::Validation { message } if message.contains("disk_size_mb")),
                "disk_size_mb {disk} must be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn sdk_resources_object_is_subject_to_the_same_ceilings() {
        let body = CreateSandboxBody {
            resources: Some(ResourcesBody {
                memory: Some(MAX_SANDBOX_MEMORY_MB + 1),
                vcpus: Some(1.0),
            }),
            ..Default::default()
        };
        let req = CreateSandboxRequest::from(body);
        assert!(validate_resource_limits(&req).is_err());
    }

    // ── POST /v1/sandboxes, through the router ──────────────────────────────
    //
    // These send real requests so the handler wiring is covered, not only the
    // validator: an account holding the default `user` role must get a 4xx for
    // limits that would otherwise reach the container runtime.

    struct AllowAll;

    #[async_trait::async_trait]
    impl temps_core::SensitiveActionAuthorizer for AllowAll {
        async fn authorize(
            &self,
            _action: &temps_core::SensitiveAction,
            _principal: &temps_core::SensitiveActionPrincipal,
        ) -> Result<
            temps_core::SensitiveActionDecision,
            temps_core::SensitiveActionAuthorizationError,
        > {
            Ok(temps_core::SensitiveActionDecision::Allow)
        }
    }

    fn settings_row(agent_sandbox: serde_json::Value) -> temps_entities::settings::Model {
        temps_entities::settings::Model {
            id: 1,
            data: serde_json::json!({ "agent_sandbox": agent_sandbox }),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn live_count_row(count: i64) -> std::collections::BTreeMap<String, sea_orm::Value> {
        std::collections::BTreeMap::from([("num_items".to_string(), count.into())])
    }

    /// Router over `db`, authenticated as a default-role user.
    fn create_app(db: sea_orm::DatabaseConnection) -> axum::Router {
        let service = Arc::new(
            crate::services::sandbox_service::tests::preview_test_service_with_db(Arc::new(db)),
        );
        let state = Arc::new(SandboxAppState {
            sandbox_service: service,
            snapshot_service: None,
            project_access_checker: None,
            audit_service: None,
            sensitive_action_authorizer: Arc::new(AllowAll),
        });
        crate::handlers::configure_routes()
            .with_state(state)
            .layer(axum::Extension(AuthContext::new_session(
                test_user(),
                Role::User,
            )))
    }

    /// Status and parsed Problem body of `POST /v1/sandboxes`.
    async fn post_create(
        app: axum::Router,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/sandboxes")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn create_rejects_unbounded_limits_before_touching_the_database() {
        // A database that answers nothing: every case must be decided by the
        // handler, or the unanswered query would surface as a 500.
        for body in [
            serde_json::json!({ "pids_limit": -1, "memory_limit_mb": 4_194_304, "cpu_limit": 1 }),
            serde_json::json!({ "pids_limit": 0 }),
            serde_json::json!({ "memory_limit_mb": 4_194_304 }),
            serde_json::json!({ "memory_limit_mb": 5 }),
            serde_json::json!({ "cpu_limit": 1e-12 }),
            serde_json::json!({ "resources": { "memory": 4_194_304 } }),
        ] {
            let db =
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
            let (status, problem) = post_create(create_app(db), body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {problem}");
            assert_eq!(problem["title"], "Validation Error", "{body}");
        }
    }

    #[tokio::test]
    async fn create_rejects_a_request_over_the_operator_maximum() {
        // Query order: the placement allow-list, then the limit settings.
        let row = settings_row(serde_json::json!({ "max_memory_limit_mb": 1024 }));
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![row.clone()], vec![row]])
            .into_connection();

        let (status, problem) = post_create(
            create_app(db),
            serde_json::json!({ "memory_limit_mb": 2048 }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
        let detail = problem["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("max_memory_limit_mb"), "{detail}");
    }

    #[tokio::test]
    async fn create_refuses_a_user_who_is_at_the_sandbox_cap() {
        let row = settings_row(serde_json::json!({ "max_sandboxes_per_user": 2 }));
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![row.clone()], vec![row]])
            // The user already has two live sandboxes.
            .append_query_results([vec![live_count_row(2)]])
            // The per-user advisory lock taken before the count.
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let (status, problem) = post_create(create_app(db), serde_json::json!({})).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["title"], "Sandbox Limit Reached");
        let detail = problem["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("already has 2") && detail.contains("limit is 2"),
            "{detail}"
        );
    }

    // ── DTO stability: unknown fields ───────────────────────────────────────
    //
    // SDK-facing bodies (`CreateSandboxBody`, `ExtendTimeoutBody`,
    // `PaginationParams`) deliberately *accept* unknown fields so that
    // `@vercel/sandbox` payloads carrying SDK-only extras like `projectId`,
    // `networkPolicy`, or `teamId` don't break us. See `tests/vercel_compat.rs`
    // for the forward-compat pin.
    //
    // Bodies the SDK doesn't send or sends with a fixed shape (exec, write,
    // mkdir, kill, fs queries, source) remain strict — a typo there is a
    // client bug we want to surface, not silently swallow.

    fn assert_rejects_unknown<T: for<'de> serde::Deserialize<'de>>(json: &str) {
        match serde_json::from_str::<T>(json) {
            Ok(_) => panic!(
                "expected unknown-field rejection, but deserialization succeeded for json: {}",
                json
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("unknown field"),
                    "expected unknown-field error, got: {}",
                    msg
                );
            }
        }
    }

    #[test]
    fn exec_body_rejects_unknown_field() {
        assert_rejects_unknown::<ExecBody>(r#"{"cmd":["ls"],"surprise":1}"#);
    }

    #[test]
    fn write_file_body_rejects_unknown_field() {
        assert_rejects_unknown::<WriteFileBody>(
            r#"{"path":"/a","contents_b64":"Zm9v","extra":true}"#,
        );
    }

    #[test]
    fn write_files_body_rejects_unknown_field() {
        assert_rejects_unknown::<WriteFilesBody>(r#"{"files":[],"oops":1}"#);
    }

    #[test]
    fn mkdir_body_rejects_unknown_field() {
        assert_rejects_unknown::<MkdirBody>(r#"{"path":"/x","extra":"y"}"#);
    }

    #[test]
    fn kill_job_body_rejects_unknown_field() {
        assert_rejects_unknown::<KillJobBody>(r#"{"force":true,"more":1}"#);
    }

    #[test]
    fn read_file_query_rejects_unknown_field() {
        assert_rejects_unknown::<ReadFileQuery>(r#"{"path":"/a","extra":"b"}"#);
    }

    #[test]
    fn stat_query_rejects_unknown_field() {
        assert_rejects_unknown::<StatQuery>(r#"{"path":"/a","extra":"b"}"#);
    }

    #[test]
    fn list_dir_query_rejects_unknown_field() {
        assert_rejects_unknown::<ListDirQuery>(r#"{"path":"/a","extra":"b"}"#);
    }

    #[test]
    fn list_dir_query_accepts_paging() {
        let query: ListDirQuery =
            serde_json::from_str(r#"{"path":"/a","page":3,"page_size":50}"#).expect("parses");
        assert_eq!((query.page, query.page_size), (Some(3), Some(50)));
    }

    #[test]
    fn list_dir_response_uses_snake_case_kinds_and_keeps_order() {
        use crate::services::fs::{DirEntry, DirEntryKind, DirListing};
        let response = DirectoryListingResponse::from(DirListing {
            path: "/work".into(),
            exists: true,
            is_dir: true,
            entries: vec![
                DirEntry {
                    name: "src".into(),
                    kind: DirEntryKind::Directory,
                },
                DirEntry {
                    name: "link".into(),
                    kind: DirEntryKind::Symlink,
                },
            ],
            page: 2,
            page_size: 20,
            total: 45,
            truncated: false,
            has_more: true,
        });
        let json = serde_json::to_value(&response).expect("serializes");
        assert_eq!(json["entries"][0]["kind"], "directory");
        assert_eq!(json["entries"][1]["kind"], "symlink");
        assert_eq!(json["page"], 2);
        assert_eq!(json["page_size"], 20);
        assert_eq!(json["total"], 45);
        assert_eq!(json["truncated"], false);
        assert_eq!(json["has_more"], true);
    }

    #[test]
    fn domain_query_rejects_unknown_field() {
        assert_rejects_unknown::<DomainQuery>(r#"{"port":8080,"extra":"b"}"#);
    }

    #[test]
    fn source_body_git_rejects_unknown_field() {
        assert_rejects_unknown::<SourceBody>(r#"{"type":"git","url":"https://x","other":1}"#);
    }

    #[test]
    fn create_body_tolerates_unknown_sdk_fields() {
        // `@vercel/sandbox` sends `projectId`, `networkPolicy`, `ports`, etc.
        // These must parse cleanly so the SDK works without modification.
        let body: CreateSandboxBody =
            serde_json::from_str(r#"{"projectId":"x","unknownFuture":42}"#)
                .expect("SDK extras must not break create");
        assert!(body.image.is_none());
    }

    #[test]
    fn extend_timeout_body_tolerates_unknown_sdk_fields() {
        let body: ExtendTimeoutBody = serde_json::from_str(r#"{"duration":60000,"extra":1}"#)
            .expect("SDK extras must not break extend-timeout");
        assert_eq!(body.resolve_secs(), Some(60));
    }

    // ── SourceBody::validate ────────────────────────────────────────────────
    //
    // These keep the credential-safety rules locked in: no embedded creds in
    // the URL, no conflicting auth modes, username/password must pair.

    #[test]
    fn validate_accepts_plain_git_url() {
        let body = SourceBody::Git {
            url: "https://github.com/foo/bar".into(),
            revision: None,
            depth: None,
            username: None,
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        assert!(body.validate().is_ok());
    }

    #[test]
    fn validate_rejects_url_with_embedded_creds() {
        let body = SourceBody::Git {
            url: "https://user:secret@github.com/foo/bar".into(),
            revision: None,
            depth: None,
            username: None,
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        let err = body.validate().expect_err("expected validation failure");
        assert!(
            matches!(err, crate::error::SandboxError::Validation { .. }),
            "expected Validation error, got {:?}",
            err
        );
    }

    #[test]
    fn validate_rejects_username_without_password() {
        let body = SourceBody::Git {
            url: "https://github.com/foo/bar".into(),
            revision: None,
            depth: None,
            username: Some("x-access-token".into()),
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        assert!(body.validate().is_err());
    }

    #[test]
    fn validate_rejects_connection_id_plus_explicit_creds() {
        let body = SourceBody::Git {
            url: "https://github.com/foo/bar".into(),
            revision: None,
            depth: None,
            username: Some("x-access-token".into()),
            password: Some("ghp_xxx".into()),
            git_connection_id: Some(7),
            destination: None,
            strip_git_metadata: false,
        };
        assert!(body.validate().is_err());
    }

    #[test]
    fn validate_accepts_connection_id_alone() {
        let body = SourceBody::Git {
            url: "https://github.com/foo/bar".into(),
            revision: Some("main".into()),
            depth: Some(1),
            username: None,
            password: None,
            git_connection_id: Some(7),
            destination: Some("projects/foo".into()),
            strip_git_metadata: true,
        };
        assert!(body.validate().is_ok());
        assert!(body.uses_stored_git_connection());
    }

    #[test]
    fn source_permission_classification_ignores_anonymous_and_tarball_sources() {
        let anonymous = SourceBody::Git {
            url: "https://github.com/foo/bar".into(),
            revision: None,
            depth: None,
            username: None,
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        let tarball = SourceBody::Tarball {
            url: "https://downloads.example/source.tar.gz".into(),
        };

        assert!(!anonymous.uses_stored_git_connection());
        assert!(!tarball.uses_stored_git_connection());
    }

    #[test]
    fn validate_accepts_paired_username_password() {
        let body = SourceBody::Git {
            url: "https://github.com/foo/bar".into(),
            revision: None,
            depth: None,
            username: Some("x-access-token".into()),
            password: Some("ghp_xxx".into()),
            git_connection_id: None,
            destination: None,
            strip_git_metadata: false,
        };
        assert!(body.validate().is_ok());
    }

    #[test]
    fn validate_rejects_unsafe_git_destinations() {
        for destination in ["", "/workspace/project", "../project", "projects/../escape"] {
            let body = SourceBody::Git {
                url: "https://github.com/foo/bar".into(),
                revision: None,
                depth: None,
                username: None,
                password: None,
                git_connection_id: None,
                destination: Some(destination.into()),
                strip_git_metadata: false,
            };
            assert!(body.validate().is_err(), "accepted {destination:?}");
        }
    }

    #[test]
    fn validate_requires_a_destination_before_stripping_git_metadata() {
        let body = SourceBody::Git {
            url: "https://github.com/foo/bar".into(),
            revision: None,
            depth: None,
            username: None,
            password: None,
            git_connection_id: None,
            destination: None,
            strip_git_metadata: true,
        };
        assert!(body.validate().is_err());
    }

    // ── Security: Fix #19 + #27 for sandbox extract_tar_gz ───────────────────

    /// Build a raw POSIX ustar 512-byte header entry, bypassing the tar crate's
    /// own `set_path`/`set_link_name` validation that rejects `..` components.
    fn build_raw_ustar_entry(
        entry_path: &str,
        typeflag: u8,
        link_target: &str,
        data: &[u8],
    ) -> Vec<u8> {
        const BLOCK: usize = 512;
        let mut header = [0u8; BLOCK];
        let name = entry_path.as_bytes();
        header[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
        header[100..108].copy_from_slice(b"0000644\0");
        let size_str = format!("{:011o} ", data.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        header[136..148].copy_from_slice(b"00000000000 ");
        header[156] = typeflag;
        let link = link_target.as_bytes();
        header[157..157 + link.len().min(100)].copy_from_slice(&link[..link.len().min(100)]);
        header[257..265].copy_from_slice(b"ustar  \0");
        header[148..156].copy_from_slice(b"        ");
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let ck = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(ck.as_bytes());
        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
        out.extend_from_slice(&vec![0u8; pad]);
        out.extend_from_slice(&[0u8; BLOCK * 2]);
        out
    }

    /// Compress raw tar bytes with gzip.
    fn gzip_compress(data: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Fix #19: symlink entries must be silently skipped, not extracted.
    #[test]
    fn extract_tar_gz_skips_symlink_entry() {
        let raw = build_raw_ustar_entry("evil_link", b'2', "../../../etc/shadow", &[]);
        let body = gzip_compress(&raw);
        let entries = extract_tar_gz(&body).expect("should succeed (entry skipped)");
        assert_eq!(
            entries.len(),
            0,
            "symlink entry must be skipped, not extracted"
        );
    }

    /// Fix #19: hardlink entries must also be silently skipped.
    #[test]
    fn extract_tar_gz_skips_hardlink_entry() {
        let raw = build_raw_ustar_entry("hard_link", b'1', "../../../etc/passwd", &[]);
        let body = gzip_compress(&raw);
        let entries = extract_tar_gz(&body).expect("should succeed (entry skipped)");
        assert_eq!(entries.len(), 0, "hardlink entry must be skipped");
    }

    /// Fix #27: a single entry whose declared header size exceeds MAX_ENTRY_BYTES (50 MiB)
    /// must be rejected before any content is read.
    #[test]
    fn extract_tar_gz_rejects_oversized_single_entry() {
        const MAX_ENTRY_BYTES: u64 = 50 * 1024 * 1024;
        let huge: u64 = MAX_ENTRY_BYTES + 1;
        const BLOCK: usize = 512;
        let mut header = [0u8; BLOCK];
        let name = b"huge.bin";
        header[..name.len()].copy_from_slice(name);
        header[100..108].copy_from_slice(b"0000644\0");
        let size_str = format!("{:011o} ", huge);
        header[124..136].copy_from_slice(size_str.as_bytes());
        header[136..148].copy_from_slice(b"00000000000 ");
        header[156] = b'0';
        header[257..265].copy_from_slice(b"ustar  \0");
        header[148..156].copy_from_slice(b"        ");
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let ck = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(ck.as_bytes());
        let mut raw = header.to_vec();
        raw.extend_from_slice(&[0u8; BLOCK * 2]);
        let body = gzip_compress(&raw);
        let err = extract_tar_gz(&body).unwrap_err();
        assert!(
            err.contains("per-entry limit") || err.contains("exceeds"),
            "expected per-entry limit error, got: {err}"
        );
    }

    /// Fix #27: an archive whose aggregate declared size exceeds 200 MiB total
    /// must be rejected. We use two entries each claiming 110 MiB (header-only,
    /// no actual data blocks) so the aggregate triggers before any I/O.
    #[test]
    fn extract_tar_gz_rejects_aggregate_size_bomb() {
        // Build a tar with two entries, each claiming 110 MiB (> 50 MiB per-entry limit).
        // The first entry alone will exceed MAX_ENTRY_BYTES, so we need entries that are
        // individually under 50 MiB but together exceed 200 MiB.
        // Use 8 entries × 30 MiB each = 240 MiB (each entry has real data so tar advances).
        let chunk_size = 30 * 1024 * 1024usize; // 30 MiB of zeros
        let zeros = vec![0u8; chunk_size];
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            for i in 0..8usize {
                let name = format!("chunk_{}.bin", i);
                let mut h = tar::Header::new_gnu();
                h.set_path(&name).unwrap();
                h.set_size(chunk_size as u64);
                h.set_mode(0o644);
                h.set_cksum();
                builder.append(&h, zeros.as_slice()).unwrap();
            }
            builder.finish().unwrap();
        }
        let body = gz.finish().unwrap();
        let err = extract_tar_gz(&body).unwrap_err();
        assert!(
            err.contains("total") || err.contains("limit"),
            "expected aggregate size limit error, got: {err}"
        );
    }

    /// Fix #17 (existing behaviour preserved): parent-dir traversal is still rejected.
    #[test]
    fn extract_tar_gz_still_rejects_parent_dir_traversal() {
        let raw = build_raw_ustar_entry("../escape.txt", b'0', "", b"pwned");
        let body = gzip_compress(&raw);
        let err = extract_tar_gz(&body).unwrap_err();
        assert!(
            err.contains("traversal") || err.contains(".."),
            "expected traversal error, got: {err}"
        );
    }
}
