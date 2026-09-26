// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::AppState;
use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::{problemdetails::Problem, AuditOperation, RequestMetadata};
use temps_entities::compose_security::{
    ComposeSecurityCheck, ComposeSecurityCheckDefinition, ComposeSecurityPolicy,
};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct ComposeSecurityResponse {
    pub policy: ComposeSecurityPolicy,
    pub checks: Vec<ComposeSecurityCheckDefinition>,
    pub can_edit: bool,
    pub legacy_migration_pending: bool,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateComposeSecurityRequest {
    pub policy: ComposeSecurityPolicy,
    /// Policy read by the editor; compared while holding the project lock.
    pub expected_policy: ComposeSecurityPolicy,
    #[serde(default)]
    pub acknowledge_legacy_migration: bool,
    #[serde(default)]
    pub acknowledge_risks: bool,
}

#[utoipa::path(get, path = "/projects/{id}/compose-security", tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    responses((status = 200, body = ComposeSecurityResponse), (status = 403, description = "Forbidden"), (status = 404, description = "Project not found")))]
pub async fn get_compose_security(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<Json<ComposeSecurityResponse>, Problem> {
    project_permission_guard!(auth, ProjectsRead, id, state.project_access_checker);
    project_scope_guard!(auth, id);
    let policy = state.project_service.compose_security_policy(id).await?;
    Ok(Json(ComposeSecurityResponse {
        policy,
        legacy_migration_pending: state
            .project_service
            .compose_security_legacy_migration_pending(id)
            .await?,
        checks: ComposeSecurityCheck::catalog(),
        can_edit: auth.is_admin() && auth.has_permission(&temps_auth::Permission::ProjectsWrite),
    }))
}

#[utoipa::path(put, path = "/projects/{id}/compose-security", tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")), request_body = UpdateComposeSecurityRequest,
    responses((status = 200, body = ComposeSecurityResponse), (status = 400, description = "Invalid settings or missing acknowledgement"), (status = 409, description = "Policy changed; refresh before retrying"), (status = 403, description = "Instance administrator required"), (status = 404, description = "Project not found")))]
pub async fn update_compose_security(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateComposeSecurityRequest>,
) -> Result<Json<ComposeSecurityResponse>, Problem> {
    project_permission_guard!(auth, ProjectsWrite, id, state.project_access_checker);
    project_scope_guard!(auth, id);
    require_policy_admin(&auth)?;
    let previous = state
        .project_service
        .update_compose_security_policy(
            id,
            auth.user_id(),
            request.policy.clone(),
            request.acknowledge_risks,
            request.expected_policy,
            request.acknowledge_legacy_migration,
        )
        .await?;
    let event = ComposeSecurityAudit {
        project_id: id,
        actor: auth.user_id(),
        ip_address: metadata.ip_address.to_string(),
        user_agent: metadata.user_agent,
        previous,
        policy: request.policy.clone(),
        acknowledged_legacy_migration: request.acknowledge_legacy_migration,
    };
    if let Err(error) = state.audit_service.create_audit_log(&event).await {
        tracing::error!(project_id = id, actor = auth.user_id(), %error, "Failed to record Compose security policy audit event");
    }
    Ok(Json(ComposeSecurityResponse {
        policy: request.policy,
        legacy_migration_pending: state
            .project_service
            .compose_security_legacy_migration_pending(id)
            .await?,
        checks: ComposeSecurityCheck::catalog(),
        can_edit: true,
    }))
}

fn require_policy_admin(auth: &temps_auth::AuthContext) -> Result<(), Problem> {
    if !auth.is_admin() {
        return Err(temps_core::problemdetails::new(http::StatusCode::FORBIDDEN)
            .with_title("Instance administrator required")
            .with_detail("Only an instance administrator may change Compose security policies"));
    }
    Ok(())
}

#[derive(Serialize)]
struct ComposeSecurityAudit {
    project_id: i32,
    actor: i32,
    ip_address: String,
    user_agent: String,
    previous: ComposeSecurityPolicy,
    policy: ComposeSecurityPolicy,
    acknowledged_legacy_migration: bool,
}
impl AuditOperation for ComposeSecurityAudit {
    fn operation_type(&self) -> String {
        "project.compose_security.updated".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.actor)
    }
    fn ip_address(&self) -> Option<String> {
        Some(self.ip_address.clone())
    }
    fn user_agent(&self) -> &str {
        &self.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// Retire legacy grants so imports and generic project updates cannot bypass acknowledgment.
pub(crate) fn reject_legacy_sandbox_grants(
    config: Option<&serde_json::Value>,
) -> Result<(), Problem> {
    let grants = config
        .and_then(|value| value.get("unsandboxedServices"))
        .and_then(serde_json::Value::as_array);
    if grants.is_some_and(|grants| !grants.is_empty()) {
        return Err(temps_core::problemdetails::new(http::StatusCode::FORBIDDEN)
            .with_title("Use advanced Compose security settings")
            .with_detail("Per-service sandbox exceptions have been replaced by individually acknowledged policies in Advanced security settings. Clear unsandboxedServices and configure the required policies there."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::{AuthContext, Permission, Role};
    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: "Test admin".into(),
            email: "admin@example.test".into(),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
    #[test]
    fn only_effective_admin_credentials_can_grant_exceptions() {
        assert!(require_policy_admin(&AuthContext::new_session(user(), Role::Admin)).is_ok());
        for role in [Role::User, Role::Reader, Role::PlatformAdmin, Role::Custom] {
            assert!(require_policy_admin(&AuthContext::new_session(user(), role)).is_err());
        }
        let restricted_key = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::ProjectsWrite]),
            "project writer".into(),
            1,
        );
        assert!(require_policy_admin(&restricted_key).is_err());
        let token =
            AuthContext::new_deployment_token(7, None, None, 1, "deployment".into(), vec![]);
        assert!(require_policy_admin(&token).is_err());
    }
    #[test]
    fn generic_routes_cannot_grant_legacy_sandbox_exemptions() {
        let config = serde_json::json!({"preset":"docker-compose", "unsandboxedServices":["app"]});
        assert!(reject_legacy_sandbox_grants(Some(&config)).is_err());
        assert!(reject_legacy_sandbox_grants(Some(
            &serde_json::json!({"composePath":"compose.yml"})
        ))
        .is_ok());
        assert!(
            reject_legacy_sandbox_grants(Some(&serde_json::json!({"unsandboxedServices":[]})))
                .is_ok()
        );
    }
    #[test]
    fn request_rejects_unknown_policy_ids_and_extra_fields() {
        assert!(serde_json::from_value::<UpdateComposeSecurityRequest>(
            serde_json::json!({"policy":{"disabled_checks":["typo"]},"acknowledge_risks":true})
        )
        .is_err());
        assert!(serde_json::from_value::<UpdateComposeSecurityRequest>(
            serde_json::json!({"policy":{"disabled_checks":[]},"disable_everything":true})
        )
        .is_err());
    }
}
