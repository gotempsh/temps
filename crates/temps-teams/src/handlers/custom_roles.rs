use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::permissions::Permission;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use utoipa::ToSchema;

use crate::custom_role_service::{
    CreateCustomRoleRequest, CustomRoleListResponse, CustomRoleResponse, UpdateCustomRoleRequest,
};

use super::TeamsAppState;

pub(crate) fn router() -> Router<Arc<TeamsAppState>> {
    Router::new()
        .route(
            "/custom-roles",
            get(list_custom_roles).post(create_custom_role),
        )
        .route(
            "/custom-roles/{role_id}",
            get(get_custom_role)
                .patch(update_custom_role)
                .delete(delete_custom_role),
        )
}

// ---------------------------------------------------------------------------
// Privilege-escalation ceiling
//
// Same shape as `temps_auth::apikey_handler::enforce_custom_permission_ceiling`.
// A custom role must never be created or updated to include a permission
// the acting user doesn't themselves hold, or any actor with UsersWrite
// could mint itself (or anyone else) a role carrying SystemAdmin.
// ---------------------------------------------------------------------------

pub(crate) fn enforce_permission_ceiling(
    auth: &temps_auth::context::AuthContext,
    permissions: &[String],
) -> Result<(), Problem> {
    for perm_str in permissions {
        if let Some(perm) = Permission::from_str(perm_str) {
            if !auth.has_permission(&perm) {
                return Err(
                    temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                        .type_("https://temps.sh/probs/permission-ceiling-exceeded")
                        .title("Forbidden")
                        .detail(format!(
                            "Cannot grant permission '{perm_str}': it exceeds your own permissions"
                        ))
                        .value("error_code", "PERMISSION_CEILING_EXCEEDED")
                        .build(),
                );
            }
        }
        // Unparsable strings are left to the service layer's existing
        // validation, which returns 400.
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Audit event types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct CustomRoleCreatedAudit {
    #[serde(flatten)]
    context: AuditContext,
    role_id: i32,
    role_slug: String,
    permissions: Vec<String>,
}

impl AuditOperation for CustomRoleCreatedAudit {
    fn operation_type(&self) -> String {
        "CUSTOM_ROLE_CREATED".to_string()
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize CustomRoleCreatedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct CustomRoleUpdatedAudit {
    #[serde(flatten)]
    context: AuditContext,
    role_id: i32,
    permissions: Vec<String>,
}

impl AuditOperation for CustomRoleUpdatedAudit {
    fn operation_type(&self) -> String {
        "CUSTOM_ROLE_UPDATED".to_string()
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize CustomRoleUpdatedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct CustomRoleDeletedAudit {
    #[serde(flatten)]
    context: AuditContext,
    role_id: i32,
}

impl AuditOperation for CustomRoleDeletedAudit {
    fn operation_type(&self) -> String {
        "CUSTOM_ROLE_DELETED".to_string()
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize CustomRoleDeletedAudit: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListCustomRolesQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[utoipa::path(
    tag = "Teams",
    get,
    path = "/custom-roles",
    params(("page" = Option<u64>, Query), ("page_size" = Option<u64>, Query)),
    responses(
        (status = 200, description = "Paginated custom roles", body = CustomRoleListResponse),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_custom_roles(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Query(q): Query<ListCustomRolesQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersRead);
    let (roles, total) = state
        .custom_role_service
        .list_custom_roles(q.page, q.page_size)
        .await?;
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q
        .page_size
        .unwrap_or(crate::service::DEFAULT_PAGE_SIZE)
        .clamp(1, crate::service::MAX_PAGE_SIZE);
    Ok(Json(CustomRoleListResponse {
        roles: roles
            .into_iter()
            .map(|(role, permissions)| to_response(role, permissions))
            .collect(),
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "Teams",
    post,
    path = "/custom-roles",
    request_body = CreateCustomRoleRequest,
    responses(
        (status = 201, description = "Custom role created", body = CustomRoleResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "A requested permission exceeds the actor's own permissions"),
        (status = 409, description = "Slug already taken"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_custom_role(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<CreateCustomRoleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);
    enforce_permission_ceiling(&auth, &req.permissions)?;

    let (role, permissions) = state
        .custom_role_service
        .create_custom_role(auth.user_id(), req)
        .await?;

    let audit = CustomRoleCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        role_id: role.id,
        role_slug: role.slug.clone(),
        permissions: permissions.clone(),
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write custom role create audit log");
    }

    Ok((StatusCode::CREATED, Json(to_response(role, permissions))))
}

#[utoipa::path(
    tag = "Teams",
    get,
    path = "/custom-roles/{role_id}",
    params(("role_id" = i32, Path)),
    responses(
        (status = 200, description = "Custom role", body = CustomRoleResponse),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_custom_role(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(role_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersRead);
    let (role, permissions) = state.custom_role_service.get_custom_role(role_id).await?;
    Ok(Json(to_response(role, permissions)))
}

#[utoipa::path(
    tag = "Teams",
    patch,
    path = "/custom-roles/{role_id}",
    params(("role_id" = i32, Path)),
    request_body = UpdateCustomRoleRequest,
    responses(
        (status = 200, description = "Updated custom role", body = CustomRoleResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "A requested permission exceeds the actor's own permissions"),
        (status = 404, description = "Not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_custom_role(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(role_id): Path<i32>,
    Json(req): Json<UpdateCustomRoleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);
    if let Some(ref permissions) = req.permissions {
        // Ceiling-checked against the FULL new set, not just the delta —
        // simpler and at least as safe. A permission that was already on
        // the role but that the actor doesn't hold is still blocked, which
        // is intentional: an under-privileged admin should not be able to
        // "lock in" a role's over-broad grant just because they didn't add
        // it themselves.
        enforce_permission_ceiling(&auth, permissions)?;
    }

    let (role, permissions) = state
        .custom_role_service
        .update_custom_role(role_id, req)
        .await?;

    // A role's permission set changing affects every member holding this
    // custom_role_id, without any membership row changing — narrower
    // invalidation (by user/project) can't target that, so flush
    // everything. Cheap: `invalidate_all` just marks entries expired.
    state.checker.invalidate_all();

    let audit = CustomRoleUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        role_id,
        permissions: permissions.clone(),
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write custom role update audit log");
    }

    Ok(Json(to_response(role, permissions)))
}

#[utoipa::path(
    tag = "Teams",
    delete,
    path = "/custom-roles/{role_id}",
    params(("role_id" = i32, Path)),
    responses(
        (status = 204, description = "Custom role deleted"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_custom_role(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(role_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersDelete);
    state
        .custom_role_service
        .delete_custom_role(role_id)
        .await?;

    // Members whose custom_role_id pointed at this role revert to their
    // fixed `role` column (DB-level ON DELETE SET NULL) — their resolved
    // permissions change without any row the checker watches changing in a
    // way `invalidate_user`/`invalidate_project` could target.
    state.checker.invalidate_all();

    let audit = CustomRoleDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        role_id,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write custom role delete audit log");
    }

    Ok(StatusCode::NO_CONTENT)
}

fn to_response(
    role: temps_entities::custom_roles::Model,
    permissions: Vec<String>,
) -> CustomRoleResponse {
    CustomRoleResponse {
        id: role.id,
        name: role.name,
        slug: role.slug,
        description: role.description,
        created_by: role.created_by,
        permissions,
        created_at: role.created_at,
        updated_at: role.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    fn test_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
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

    fn user_auth(role: Role) -> AuthContext {
        AuthContext::new_session(test_user(42), role)
    }

    /// The escalation this check exists to prevent.
    #[test]
    fn ceiling_denies_permission_actor_does_not_hold() {
        let auth = user_auth(Role::User);
        let err = enforce_permission_ceiling(&auth, &["system:admin".to_string()]).unwrap_err();
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn ceiling_allows_permission_actor_holds() {
        let auth = user_auth(Role::User);
        assert!(enforce_permission_ceiling(&auth, &["projects:read".to_string()]).is_ok());
    }

    #[test]
    fn admin_ceiling_allows_any_permission() {
        let auth = user_auth(Role::Admin);
        assert!(enforce_permission_ceiling(
            &auth,
            &["system:admin".to_string(), "users:delete".to_string()]
        )
        .is_ok());
    }

    #[test]
    fn ceiling_ignores_unparsable_permission_strings() {
        let auth = user_auth(Role::User);
        assert!(enforce_permission_ceiling(&auth, &["not-a-real-permission".to_string()]).is_ok());
    }

    #[test]
    fn ceiling_denies_on_first_disallowed_permission_in_a_mixed_set() {
        let auth = user_auth(Role::User);
        // projects:read is held by Role::User; system:admin is not.
        let err = enforce_permission_ceiling(
            &auth,
            &["projects:read".to_string(), "system:admin".to_string()],
        )
        .unwrap_err();
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }
}
