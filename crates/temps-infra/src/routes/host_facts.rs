// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET /platform/host-facts`: the control plane's machine, for the welcome
//! flow's first screen.

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use temps_authz::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use utoipa::OpenApi;

use crate::services::host_facts::{
    DiskFact, DockerFact, HostFacts, HostFactsService, InboundCheck, MemoryFact, PortFact,
    PortHolder, TailscaleFact,
};

#[derive(OpenApi)]
#[openapi(
    paths(get_host_facts),
    components(schemas(
        HostFacts,
        MemoryFact,
        DiskFact,
        DockerFact,
        PortFact,
        PortHolder,
        TailscaleFact,
        InboundCheck
    ))
)]
pub struct HostFactsApiDoc;

pub fn configure_host_facts_routes() -> Router<Arc<HostFactsService>> {
    Router::new().route("/platform/host-facts", get(get_host_facts))
}

/// What Temps reads about the control plane's machine: processor, memory,
/// swap, disk, Docker, ports 80 and 443, its addresses and Tailscale.
///
/// Read-only. Whether ports 80 and 443 answer from the internet is reported
/// as not checked: the machine cannot test itself from outside.
#[utoipa::path(
    tag = "Platform",
    get,
    path = "/platform/host-facts",
    responses(
        (status = 200, description = "Facts about the control plane's machine", body = HostFacts),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_host_facts(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<HostFactsService>>,
) -> Result<Json<HostFacts>, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(service.facts().await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::PlatformInfoService;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use temps_authz::{AuthContext, Permission, Role};
    use tower::ServiceExt;

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 5,
            name: "Operator".into(),
            email: "operator@example.test".into(),
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

    fn app() -> Router {
        let service = Arc::new(HostFactsService::new(
            Arc::new(PlatformInfoService::without_docker()),
            Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "no Docker in this test",
            )),
            vec![8080],
            std::env::temp_dir(),
        ));
        configure_host_facts_routes().with_state(service)
    }

    async fn get(auth: Option<AuthContext>) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .uri("/platform/host-facts")
            .body(Body::empty())
            .unwrap();
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn needs_authentication() {
        let (status, _) = get(None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn needs_settings_read() {
        let key = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::ProjectsRead]),
            "projects-only".into(),
            3,
        );
        let (status, body) = get(Some(key)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.get("title").is_some(), "Problem Details: {body}");
    }

    #[tokio::test]
    async fn an_admin_gets_the_facts_with_docker_said_missing() {
        let admin = AuthContext::new_session(user(), Role::Admin);
        let (status, body) = get(Some(admin)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["cpu_cores"].as_u64().unwrap_or(0) >= 1);
        assert_eq!(body["docker"]["running"], false);
        assert!(body["docker"]["error"].is_string());
        assert_eq!(body["inbound"]["checked"], false);
        assert_eq!(body["ports"].as_array().map(Vec::len), Some(2));
    }
}
