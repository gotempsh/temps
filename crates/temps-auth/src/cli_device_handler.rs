//! CLI device-authorization flow (OAuth 2.0 RFC 8628-style).
//!
//! This is the only interactive login path the CLI exposes — credentials are
//! always entered in the web UI. The legacy password endpoint was removed:
//! workspace/sandbox terminals have nothing reasonable to prompt into, and
//! SSO / magic-link users have no password to type anyway. Headless callers
//! authenticate with a pre-minted API key from the dashboard.
//!
//! Flow:
//!
//!   1. CLI POSTs `/auth/cli/device/start` (anon) with an optional
//!      `client_name` (hostname). Server creates a `cli_login_sessions` row
//!      and returns `{ device_code, user_code, verification_uri,
//!      verification_uri_complete, expires_in, interval }`.
//!   2. CLI prints/opens `verification_uri_complete` and starts polling
//!      `/auth/cli/device/poll` (anon) with `{ device_code }`.
//!   3. The browser-side `/cli-login/:user_code` page calls
//!      `GET /auth/cli/device/lookup?user_code=...` (session-authenticated)
//!      to render device metadata, then `POST /auth/cli/device/approve`
//!      or `/auth/cli/device/deny`.
//!   4. On approve, the server mints a fresh API key, stores the plaintext
//!      on the session row, and flips status to `approved`. The next CLI
//!      poll returns the key and the plaintext is cleared from the row.
//!
//! Status transitions (all guarded by `expires_at`):
//!   pending -> approved (terminal, key delivered once)
//!   pending -> denied   (terminal)
//!   any     -> expired  (passive, observed when expires_at < now)

use std::sync::Arc;

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{Duration, Utc};
use rand::Rng;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use serde::{Deserialize, Serialize};
use temps_core::problemdetails::{new as problem_new, Problem};
use temps_core::{AuditContext, RequestMetadata};
use thiserror::Error;
use tracing::{debug, error, warn};
use utoipa::{IntoParams, ToSchema};

use crate::apikey_service::{ApiKeyServiceError, CreateApiKeyRequest};
use crate::audit::LoginAudit;
use crate::permissions::Role;
use crate::state::AuthState;
use crate::RequireAuth;

/// How long a device-code session is valid before it auto-expires.
const DEVICE_SESSION_TTL_SECS: i64 = 15 * 60;
/// Polling interval the server suggests to the CLI.
const POLL_INTERVAL_SECS: i64 = 2;
/// Minimum spacing between polls. Anything tighter triggers `slow_down`.
const MIN_POLL_SPACING_MS: i64 = 750;
/// Lifetime for the API key minted by an approved device-code session.
const CLI_KEY_TTL_DAYS: i64 = 90;
/// Audit `login_method` value for device-code approvals.
const CLI_DEVICE_LOGIN_METHOD: &str = "cli_device";

/// Status strings persisted on the `cli_login_sessions.status` column.
mod status {
    pub const PENDING: &str = "pending";
    pub const APPROVED: &str = "approved";
    pub const DENIED: &str = "denied";
}

// ─── Request / response DTOs ────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CliDeviceStartRequest {
    /// Friendly hostname / client identifier shown in the browser approval
    /// screen. Sanitized before display.
    #[schema(example = "dviejo-mac.local")]
    pub client_name: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CliDeviceStartResponse {
    /// Opaque secret the CLI polls with. Never display to a human.
    pub device_code: String,
    /// Short human-readable code the user types into the browser.
    #[schema(example = "ABCD-1234")]
    pub user_code: String,
    /// Base verification URL — the CLI may display this when the
    /// pre-filled URL is too long to be useful.
    #[schema(example = "https://temps.example.com/cli-login")]
    pub verification_uri: String,
    /// `verification_uri` with `user_code` pre-filled. Open this directly.
    #[schema(example = "https://temps.example.com/cli-login/ABCD-1234")]
    pub verification_uri_complete: String,
    /// Seconds until the device_code expires.
    pub expires_in: i64,
    /// Suggested polling interval, in seconds.
    pub interval: i64,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CliDevicePollRequest {
    pub device_code: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CliDevicePollResponse {
    /// Still waiting on the user to approve in the browser.
    AuthorizationPending,
    /// CLI is polling faster than the server-suggested interval.
    SlowDown,
    /// User denied the request in the browser.
    AccessDenied,
    /// The session has expired without approval.
    ExpiredToken,
    /// The session was approved; this is the only response that carries
    /// the API key. The key is returned exactly once and then cleared
    /// from the session row.
    Approved {
        user_id: i32,
        email: String,
        role: String,
        api_key: String,
        key_prefix: String,
        #[schema(value_type = Option<String>, format = "date-time")]
        expires_at: Option<temps_core::UtcDateTime>,
    },
}

#[derive(Debug, Deserialize, Serialize, IntoParams)]
pub struct CliDeviceLookupQuery {
    /// `user_code` as displayed in the CLI / pasted into the URL.
    pub user_code: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CliDeviceLookupResponse {
    pub user_code: String,
    /// `pending` | `approved` | `denied` | `expired`.
    pub status: String,
    pub client_name: Option<String>,
    pub requested_ip: Option<String>,
    #[schema(value_type = String, format = "date-time")]
    pub expires_at: temps_core::UtcDateTime,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CliDeviceApproveRequest {
    pub user_code: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CliDeviceApproveResponse {
    pub user_code: String,
    pub status: String,
}

// ─── Errors ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CliDeviceFlowError {
    #[error("Device session for user_code {user_code} not found")]
    NotFound { user_code: String },
    #[error("Device session for device_code prefix {device_code_prefix} not found")]
    NotFoundByDeviceCode { device_code_prefix: String },
    #[error("Device session {user_code} has already been resolved ({status}); cannot {action}")]
    AlreadyResolved {
        user_code: String,
        status: String,
        action: String,
    },
    #[error("Device session {user_code} has expired")]
    Expired { user_code: String },
    #[error("User {user_id} has no role assigned; cannot mint a CLI API key")]
    NoRoleAssigned { user_id: i32 },
    #[error("Failed to mint CLI API key for user {user_id}: {reason}")]
    ApiKeyMintFailed { user_id: i32, reason: String },
    #[error("Failed to load user {user_id}: {reason}")]
    UserLoadFailed { user_id: i32, reason: String },
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl From<CliDeviceFlowError> for Problem {
    fn from(err: CliDeviceFlowError) -> Self {
        match &err {
            CliDeviceFlowError::NotFound { .. }
            | CliDeviceFlowError::NotFoundByDeviceCode { .. } => problem_new(StatusCode::NOT_FOUND)
                .with_title("Device Session Not Found")
                .with_detail(err.to_string()),
            CliDeviceFlowError::AlreadyResolved { .. } => problem_new(StatusCode::CONFLICT)
                .with_title("Device Session Already Resolved")
                .with_detail(err.to_string()),
            CliDeviceFlowError::Expired { .. } => problem_new(StatusCode::GONE)
                .with_title("Device Session Expired")
                .with_detail(err.to_string()),
            CliDeviceFlowError::NoRoleAssigned { .. } => problem_new(StatusCode::FORBIDDEN)
                .with_title("No Role Assigned")
                .with_detail(err.to_string()),
            CliDeviceFlowError::ApiKeyMintFailed { .. }
            | CliDeviceFlowError::UserLoadFailed { .. }
            | CliDeviceFlowError::Database(_) => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(err.to_string()),
        }
    }
}

// ─── Handlers ───────────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/auth/cli/device/start",
    request_body = CliDeviceStartRequest,
    responses(
        (status = 200, description = "Device session created", body = CliDeviceStartResponse),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn cli_device_start(
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CliDeviceStartRequest>,
) -> Result<Json<CliDeviceStartResponse>, Problem> {
    let now = Utc::now();
    let expires_at = now + Duration::seconds(DEVICE_SESSION_TTL_SECS);

    // Retry on the (astronomically unlikely) collision of `device_code` or
    // `user_code` against the unique indexes. Three attempts is plenty —
    // each draw has ~1/10^30 collision odds for `device_code`.
    let mut last_err: Option<sea_orm::DbErr> = None;
    for _ in 0..3 {
        let device_code = generate_device_code();
        let user_code = generate_user_code();

        let active = temps_entities::cli_login_sessions::ActiveModel {
            device_code: Set(device_code.clone()),
            user_code: Set(user_code.clone()),
            status: Set(status::PENDING.to_string()),
            client_name: Set(sanitize_client_name(request.client_name.as_deref())),
            requested_ip: Set(Some(metadata.ip_address.to_string())),
            expires_at: Set(expires_at),
            ..Default::default()
        };

        match active.insert(state.db.as_ref()).await {
            Ok(_) => {
                let base_uri = "/cli-login";
                let verification_uri_complete = format!("{}/{}", base_uri, user_code);
                debug!(
                    "cli device flow: created session user_code={} (client={:?}, ip={})",
                    user_code, request.client_name, metadata.ip_address
                );
                return Ok(Json(CliDeviceStartResponse {
                    device_code,
                    user_code,
                    verification_uri: base_uri.to_string(),
                    verification_uri_complete,
                    expires_in: DEVICE_SESSION_TTL_SECS,
                    interval: POLL_INTERVAL_SECS,
                }));
            }
            Err(sea_orm::DbErr::Exec(_)) | Err(sea_orm::DbErr::Query(_)) => {
                // Probably a unique-index collision — retry with fresh codes.
                continue;
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }

    let err = last_err.unwrap_or_else(|| {
        sea_orm::DbErr::Custom(
            "cli device flow: exhausted device_code/user_code generation retries".into(),
        )
    });
    Err(CliDeviceFlowError::Database(err).into())
}

#[utoipa::path(
    post,
    path = "/auth/cli/device/poll",
    request_body = CliDevicePollRequest,
    responses(
        (status = 200, description = "Poll result; check `status` field", body = CliDevicePollResponse),
        (status = 404, description = "Unknown device_code"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication"
)]
pub async fn cli_device_poll(
    State(state): State<Arc<AuthState>>,
    Json(request): Json<CliDevicePollRequest>,
) -> Result<Json<CliDevicePollResponse>, Problem> {
    let now = Utc::now();

    let session = temps_entities::cli_login_sessions::Entity::find()
        .filter(
            temps_entities::cli_login_sessions::Column::DeviceCode.eq(request.device_code.clone()),
        )
        .one(state.db.as_ref())
        .await
        .map_err(CliDeviceFlowError::Database)?
        .ok_or_else(|| CliDeviceFlowError::NotFoundByDeviceCode {
            device_code_prefix: request.device_code.chars().take(6).collect::<String>(),
        })?;

    // Hard expiry takes precedence over status.
    if session.expires_at < now {
        return Ok(Json(CliDevicePollResponse::ExpiredToken));
    }

    // Slow-down: if the previous poll was less than MIN_POLL_SPACING_MS ago
    // we tell the CLI to back off. Doesn't mutate state — the spec is fine
    // with the same poll being repeated.
    if let Some(last) = session.last_polled_at {
        let delta_ms = (now - last).num_milliseconds();
        if (0..MIN_POLL_SPACING_MS).contains(&delta_ms) {
            return Ok(Json(CliDevicePollResponse::SlowDown));
        }
    }

    // Stamp the poll time so the next request can compute spacing.
    let session_id = session.id;
    let update = temps_entities::cli_login_sessions::ActiveModel {
        id: Set(session_id),
        last_polled_at: Set(Some(now)),
        ..Default::default()
    };
    if let Err(e) = update.update(state.db.as_ref()).await {
        warn!(
            "cli device poll: failed to stamp last_polled_at for session {}: {}",
            session_id, e
        );
    }

    match session.status.as_str() {
        status::PENDING => Ok(Json(CliDevicePollResponse::AuthorizationPending)),
        status::DENIED => Ok(Json(CliDevicePollResponse::AccessDenied)),
        status::APPROVED => deliver_approved(&state.db, session).await,
        other => {
            error!(
                "cli device poll: session {} has unexpected status {}",
                session_id, other
            );
            Ok(Json(CliDevicePollResponse::ExpiredToken))
        }
    }
}

#[utoipa::path(
    get,
    path = "/auth/cli/device/lookup",
    params(CliDeviceLookupQuery),
    responses(
        (status = 200, description = "Device session metadata", body = CliDeviceLookupResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Unknown user_code"),
        (status = 410, description = "Device session expired"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication",
    security(("session_token" = []))
)]
pub async fn cli_device_lookup(
    RequireAuth(_auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Query(query): Query<CliDeviceLookupQuery>,
) -> Result<Json<CliDeviceLookupResponse>, Problem> {
    let session = load_by_user_code(&state.db, &query.user_code).await?;
    Ok(Json(CliDeviceLookupResponse {
        user_code: session.user_code,
        status: effective_status(&session.status, session.expires_at),
        client_name: session.client_name,
        requested_ip: session.requested_ip,
        expires_at: session.expires_at,
    }))
}

#[utoipa::path(
    post,
    path = "/auth/cli/device/approve",
    request_body = CliDeviceApproveRequest,
    responses(
        (status = 200, description = "Session approved; CLI can now claim the API key", body = CliDeviceApproveResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Unknown user_code"),
        (status = 409, description = "Already resolved"),
        (status = 410, description = "Session expired"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication",
    security(("session_token" = []))
)]
pub async fn cli_device_approve(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CliDeviceApproveRequest>,
) -> Result<Json<CliDeviceApproveResponse>, Problem> {
    let user = auth.require_user().map_err(|msg| {
        problem_new(StatusCode::FORBIDDEN)
            .with_title("User Required")
            .with_detail(msg)
    })?;

    let session = load_by_user_code(&state.db, &request.user_code).await?;
    let now = Utc::now();

    if session.expires_at < now {
        return Err(CliDeviceFlowError::Expired {
            user_code: session.user_code,
        }
        .into());
    }
    if session.status != status::PENDING {
        return Err(CliDeviceFlowError::AlreadyResolved {
            user_code: session.user_code,
            status: session.status,
            action: "approve".to_string(),
        }
        .into());
    }

    // Determine the user's primary role to scope the new key, same logic
    // as cli_auth_handler::mint_key_and_respond.
    let user_with_roles = state
        .user_service
        .get_user_with_roles(user.id)
        .await
        .map_err(|e| CliDeviceFlowError::UserLoadFailed {
            user_id: user.id,
            reason: e.to_string(),
        })?;

    let role_name = pick_primary_role(&user_with_roles)
        .ok_or(CliDeviceFlowError::NoRoleAssigned { user_id: user.id })?;

    let device_label = session
        .client_name
        .as_deref()
        .map(sanitize_device_label)
        .unwrap_or_else(|| "cli".to_string());
    let key_name = format!("cli-device:{}-{}", device_label, now.timestamp());
    let expires_at = Some(now + Duration::days(CLI_KEY_TTL_DAYS));

    let created = state
        .api_key_service
        .create_api_key(
            user.id,
            CreateApiKeyRequest {
                name: key_name,
                role_type: role_name.clone(),
                permissions: None,
                expires_at,
            },
        )
        .await
        .map_err(
            |e: ApiKeyServiceError| CliDeviceFlowError::ApiKeyMintFailed {
                user_id: user.id,
                reason: e.to_string(),
            },
        )?;

    // Flip the row to approved and stash the plaintext key for the next poll.
    let session_id = session.id;
    let update = temps_entities::cli_login_sessions::ActiveModel {
        id: Set(session_id),
        status: Set(status::APPROVED.to_string()),
        user_id: Set(Some(user.id)),
        api_key_id: Set(Some(created.id)),
        api_key_plaintext: Set(Some(created.api_key.clone())),
        approved_at: Set(Some(now)),
        ..Default::default()
    };
    update
        .update(state.db.as_ref())
        .await
        .map_err(CliDeviceFlowError::Database)?;

    // Audit the approval as a login event — same shape as cli_login does.
    let audit = LoginAudit {
        context: AuditContext {
            user_id: user.id,
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent.as_str().to_string(),
        },
        success: true,
        login_method: CLI_DEVICE_LOGIN_METHOD.to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create CLI device approve audit log: {}", e);
    }

    Ok(Json(CliDeviceApproveResponse {
        user_code: session.user_code,
        status: status::APPROVED.to_string(),
    }))
}

#[utoipa::path(
    post,
    path = "/auth/cli/device/deny",
    request_body = CliDeviceApproveRequest,
    responses(
        (status = 200, description = "Session denied", body = CliDeviceApproveResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Unknown user_code"),
        (status = 409, description = "Already resolved"),
        (status = 410, description = "Session expired"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Authentication",
    security(("session_token" = []))
)]
pub async fn cli_device_deny(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AuthState>>,
    Json(request): Json<CliDeviceApproveRequest>,
) -> Result<Json<CliDeviceApproveResponse>, Problem> {
    let _ = auth.require_user().map_err(|msg| {
        problem_new(StatusCode::FORBIDDEN)
            .with_title("User Required")
            .with_detail(msg)
    })?;

    let session = load_by_user_code(&state.db, &request.user_code).await?;
    let now = Utc::now();

    if session.expires_at < now {
        return Err(CliDeviceFlowError::Expired {
            user_code: session.user_code,
        }
        .into());
    }
    if session.status != status::PENDING {
        return Err(CliDeviceFlowError::AlreadyResolved {
            user_code: session.user_code,
            status: session.status,
            action: "deny".to_string(),
        }
        .into());
    }

    let session_id = session.id;
    let update = temps_entities::cli_login_sessions::ActiveModel {
        id: Set(session_id),
        status: Set(status::DENIED.to_string()),
        denied_at: Set(Some(now)),
        ..Default::default()
    };
    update
        .update(state.db.as_ref())
        .await
        .map_err(CliDeviceFlowError::Database)?;

    Ok(Json(CliDeviceApproveResponse {
        user_code: session.user_code,
        status: status::DENIED.to_string(),
    }))
}

// ─── Internal helpers ───────────────────────────────────────────────────────

/// Load a session by `user_code`. Returns NotFound if missing.
async fn load_by_user_code(
    db: &Arc<DatabaseConnection>,
    user_code: &str,
) -> Result<temps_entities::cli_login_sessions::Model, CliDeviceFlowError> {
    temps_entities::cli_login_sessions::Entity::find()
        .filter(temps_entities::cli_login_sessions::Column::UserCode.eq(user_code))
        .one(db.as_ref())
        .await?
        .ok_or_else(|| CliDeviceFlowError::NotFound {
            user_code: user_code.to_string(),
        })
}

/// Deliver the minted key to the CLI exactly once. Reads the plaintext from
/// the session row, clears it, and returns it. If the plaintext was already
/// consumed by a prior poll we treat the session as expired — the CLI
/// already has its key, and re-delivery would let another caller steal it.
async fn deliver_approved(
    db: &Arc<DatabaseConnection>,
    session: temps_entities::cli_login_sessions::Model,
) -> Result<Json<CliDevicePollResponse>, Problem> {
    let session_id = session.id;
    let Some(api_key) = session.api_key_plaintext.clone() else {
        warn!(
            "cli device poll: session {} approved but plaintext already consumed",
            session_id
        );
        return Ok(Json(CliDevicePollResponse::ExpiredToken));
    };

    let user_id = session
        .user_id
        .ok_or_else(|| CliDeviceFlowError::UserLoadFailed {
            user_id: 0,
            reason: format!(
                "approved session {} has no user_id; data corruption",
                session_id
            ),
        })?;

    // Clear plaintext immediately so a subsequent poll cannot replay it.
    let clear = temps_entities::cli_login_sessions::ActiveModel {
        id: Set(session_id),
        api_key_plaintext: Set(None),
        ..Default::default()
    };
    clear
        .update(db.as_ref())
        .await
        .map_err(CliDeviceFlowError::Database)?;

    // Look up email + role for the response. We don't fail the delivery if
    // the role lookup fails — but we do need the user to exist.
    let user = temps_entities::users::Entity::find_by_id(user_id)
        .one(db.as_ref())
        .await
        .map_err(CliDeviceFlowError::Database)?
        .ok_or_else(|| CliDeviceFlowError::UserLoadFailed {
            user_id,
            reason: "user record missing".into(),
        })?;

    let api_key_row = match session.api_key_id {
        Some(id) => temps_entities::api_keys::Entity::find_by_id(id)
            .one(db.as_ref())
            .await
            .map_err(CliDeviceFlowError::Database)?,
        None => None,
    };

    let (role, key_prefix, expires_at) = match api_key_row {
        Some(k) => (k.role_type, k.key_prefix, k.expires_at),
        None => (
            "user".to_string(),
            api_key.chars().take(8).collect::<String>(),
            None,
        ),
    };

    Ok(Json(CliDevicePollResponse::Approved {
        user_id,
        email: user.email,
        role,
        api_key,
        key_prefix,
        expires_at,
    }))
}

/// Effective status string for the lookup response — folds the hard expiry
/// into a synthetic `expired` value so the browser approval page can show
/// "this code has expired" without computing the deadline itself.
fn effective_status(stored: &str, expires_at: temps_core::UtcDateTime) -> String {
    if stored == status::PENDING && expires_at < Utc::now() {
        return "expired".to_string();
    }
    stored.to_string()
}

/// Generate a 32-byte hex-encoded device_code. ~10^77 entropy.
fn generate_device_code() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Generate a short, human-readable user_code of the form `XXXX-XXXX`,
/// drawn from an unambiguous alphabet (no 0/O, 1/I/L). Entropy ~32 bits,
/// which is fine given expiry and rate-limit protection.
fn generate_user_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    let pick = |rng: &mut rand::rngs::ThreadRng| -> char {
        let idx = rng.gen_range(0..ALPHABET.len());
        ALPHABET[idx] as char
    };
    let mut out = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        out.push(pick(&mut rng));
    }
    out
}

/// Pick the user's primary role for API-key minting. Prefers admin > user >
/// reader, falling back to whatever role the user has if none match.
fn pick_primary_role(user_with_roles: &crate::user_service::UserWithRoles) -> Option<String> {
    if user_with_roles.roles.is_empty() {
        return None;
    }
    let priorities = [
        Role::Admin.to_string(),
        Role::User.to_string(),
        Role::Reader.to_string(),
    ];
    for preferred in &priorities {
        if user_with_roles
            .roles
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(preferred))
        {
            return Some(preferred.clone());
        }
    }
    user_with_roles.roles.first().map(|r| r.name.clone())
}

/// Clean up a CLI-supplied `client_name` before storing it. We bound length
/// and strip control characters so the value renders safely in the browser
/// approval screen. Empty input becomes `None`.
fn sanitize_client_name(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).take(128).collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Sanitize a device label before splicing it into an API-key name. Keeps
/// `[A-Za-z0-9._-]`, replaces every other byte with `_`, clamps to 64 chars,
/// and falls back to `cli` when the result would be empty.
fn sanitize_device_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(64));
    for c in raw.chars().take(64) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '_' || c == '.' || c == '-');
    if trimmed.is_empty() {
        "cli".to_string()
    } else {
        trimmed.to_string()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn user_code_is_dashed_eight_plus_dash() {
        for _ in 0..200 {
            let c = generate_user_code();
            assert_eq!(c.len(), 9, "got {:?}", c);
            assert_eq!(&c[4..5], "-");
            // No ambiguous characters.
            for ch in c.chars() {
                if ch == '-' {
                    continue;
                }
                assert!(
                    !matches!(ch, '0' | 'O' | '1' | 'I' | 'L'),
                    "ambiguous char {ch} in {c}"
                );
            }
        }
    }

    #[test]
    fn device_code_is_64_hex_chars() {
        let c = generate_device_code();
        assert_eq!(c.len(), 64);
        assert!(c.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn effective_status_folds_in_expiry() {
        let past = Utc::now() - Duration::seconds(1);
        let future = Utc::now() + Duration::hours(1);
        assert_eq!(effective_status(status::PENDING, past), "expired");
        assert_eq!(effective_status(status::PENDING, future), status::PENDING);
        // Already-resolved statuses pass through regardless of expiry.
        assert_eq!(effective_status(status::APPROVED, past), status::APPROVED);
        assert_eq!(effective_status(status::DENIED, past), status::DENIED);
    }

    #[test]
    fn sanitize_client_name_trims_control_chars_and_bounds_length() {
        assert_eq!(
            sanitize_client_name(Some("dviejo-mac.local")).as_deref(),
            Some("dviejo-mac.local")
        );
        assert_eq!(sanitize_client_name(Some("")), None);
        assert_eq!(sanitize_client_name(None), None);
        // Control characters are stripped.
        assert_eq!(
            sanitize_client_name(Some("host\u{0007}name\u{0000}")).as_deref(),
            Some("hostname")
        );
        // Length is clamped.
        let long = "x".repeat(500);
        let cleaned = sanitize_client_name(Some(&long)).unwrap();
        assert!(cleaned.len() <= 128);
    }

    #[test]
    fn cli_device_flow_error_status_codes() {
        let p: Problem = CliDeviceFlowError::NotFound {
            user_code: "ABCD-1234".into(),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::NOT_FOUND);

        let p: Problem = CliDeviceFlowError::AlreadyResolved {
            user_code: "ABCD-1234".into(),
            status: "approved".into(),
            action: "approve".into(),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::CONFLICT);

        let p: Problem = CliDeviceFlowError::Expired {
            user_code: "ABCD-1234".into(),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::GONE);

        let p: Problem = CliDeviceFlowError::NoRoleAssigned { user_id: 7 }.into();
        assert_eq!(p.status_code, StatusCode::FORBIDDEN);
    }
}
