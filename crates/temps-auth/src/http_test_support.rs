// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test-only harness for driving the auth routes end to end: the real
//! `configure_routes()` behind the production `AuthMiddleware`, backed by a
//! migrated test database.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::connect_info::MockConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection};
use temps_core::plugin::TempsMiddleware;
use temps_core::RequestMetadata;
use temps_entities::users;
use tower::ServiceExt;

use crate::state::AuthState;

pub(crate) struct NoopAudit;

#[async_trait::async_trait]
impl temps_core::AuditLogger for NoopAudit {
    async fn create_audit_log(
        &self,
        _operation: &dyn temps_core::audit::AuditOperation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

struct NoopNotifications;

#[async_trait::async_trait]
impl temps_core::notifications::NotificationService for NoopNotifications {
    async fn send_email(
        &self,
        _message: temps_core::notifications::EmailMessage,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }

    async fn send_notification(
        &self,
        _notification: temps_core::notifications::NotificationData,
    ) -> Result<(), temps_core::notifications::NotificationError> {
        Ok(())
    }

    async fn is_configured(&self) -> Result<bool, temps_core::notifications::NotificationError> {
        Ok(false)
    }
}

/// Auth state over `db` with no-op audit, notification and telemetry sinks.
pub(crate) fn auth_state(db: Arc<DatabaseConnection>) -> Arc<AuthState> {
    Arc::new(AuthState::new(
        db,
        Arc::new(NoopAudit),
        Arc::new(temps_core::EncryptionService::new_from_password(
            "auth-http-test",
        )),
        Arc::new(temps_core::CookieCrypto::from_bytes(&[9; 32])),
        Arc::new(NoopNotifications),
        Arc::new(temps_core::telemetry::NoopTelemetryReporter),
    ))
}

async fn insert_request_metadata(mut request: Request<Body>, next: Next) -> Response {
    let headers = request.headers().clone();
    request.extensions_mut().insert(RequestMetadata {
        ip_address: "127.0.0.1".to_string(),
        user_agent: "temps-auth-test".to_string(),
        headers,
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://temps.test".to_string(),
        scheme: "http".to_string(),
        host: "temps.test".to_string(),
        is_secure: false,
    });
    next.run(request).await
}

/// The auth routes behind the production session/API-key middleware.
pub(crate) fn router(state: Arc<AuthState>, db: Arc<DatabaseConnection>) -> axum::Router {
    let middleware = Arc::new(crate::temps_middleware::AuthMiddleware::new(
        state.api_key_service.clone(),
        state.auth_service.clone(),
        state.user_service.clone(),
        state.cookie_crypto.clone(),
        db,
        crate::permission_denial_recorder::PermissionDenialRecorder::new(Arc::new(NoopAudit)),
    ));
    crate::handlers::configure_routes()
        .layer(axum::middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let middleware = middleware.clone();
                async move { middleware.execute(request, next).await }
            },
        ))
        .layer(axum::middleware::from_fn(insert_request_metadata))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
        .with_state(state)
}

pub(crate) async fn insert_user(db: &DatabaseConnection, email: &str) -> users::Model {
    users::ActiveModel {
        email: Set(email.to_string()),
        name: Set(email.to_string()),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert test user")
}

/// `Cookie` header value for a fresh, fully authenticated session.
pub(crate) async fn session_cookie(state: &AuthState, user_id: i32) -> String {
    let token = state
        .auth_service
        .create_session(user_id)
        .await
        .expect("create session");
    let encrypted = state.cookie_crypto.encrypt(&token).expect("encrypt");
    format!("session={encrypted}")
}

/// `Cookie` header value for a pending sign-in challenge, as `/auth/login`
/// issues it once the password is accepted for an MFA account.
pub(crate) async fn mfa_challenge_cookie(state: &AuthState, user_id: i32) -> String {
    let token = state
        .auth_service
        .create_mfa_session(user_id, "password")
        .await
        .expect("create MFA challenge");
    let encrypted = state.cookie_crypto.encrypt(&token).expect("encrypt");
    format!("mfa_session={encrypted}")
}

/// Send one request; `auth` is a `(header, value)` pair such as
/// `("cookie", ...)` or `("authorization", "Bearer ...")`.
pub(crate) async fn send(
    router: &axum::Router,
    method: &str,
    uri: &str,
    auth: Option<(&str, &str)>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    send_with_headers(router, method, uri, auth.as_slice(), body).await
}

/// [`send`] with any number of request headers.
pub(crate) async fn send_with_headers(
    router: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (header, value) in headers {
        builder = builder.header(*header, *value);
    }
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    }
    .expect("build request");
    let response = router.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("read body");
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}
