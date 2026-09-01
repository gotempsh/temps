// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email sending handlers

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::{bad_request, conflict, forbidden, internal_server_error, not_found},
    problemdetails::Problem,
    RequestMetadata,
};
use tracing::error;
use uuid::Uuid;

use super::audit::{DeploymentEmailPrincipal, EmailSendRequestedAudit, EmailSentAudit};
use super::types::{
    AppState, EmailResponse, EmailStatsResponse, ListEmailsQuery, PaginatedEmailsResponse,
    SendEmailRequestBody, SendEmailResponseBody,
};
use crate::services::{ListEmailsOptions, SendEmailRequest};

#[derive(Serialize)]
struct CanonicalEmailAuditRequest<'a> {
    from: &'a str,
    from_name: &'a Option<String>,
    to: &'a [String],
    cc: &'a Option<Vec<String>>,
    bcc: &'a Option<Vec<String>>,
    reply_to: &'a Option<String>,
    subject: &'a str,
    html: &'a Option<String>,
    text: &'a Option<String>,
    headers: BTreeMap<&'a str, &'a str>,
    tags: &'a Option<Vec<String>>,
    track_opens: Option<bool>,
    track_clicks: Option<bool>,
}

fn email_request_fingerprint(request: &SendEmailRequestBody) -> Result<String, serde_json::Error> {
    let headers = request
        .headers
        .as_ref()
        .map(|headers| {
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let canonical = CanonicalEmailAuditRequest {
        from: &request.from,
        from_name: &request.from_name,
        to: &request.to,
        cc: &request.cc,
        bcc: &request.bcc,
        reply_to: &request.reply_to,
        subject: &request.subject,
        html: &request.html,
        text: &request.text,
        headers,
        tags: &request.tags,
        track_opens: request.track_opens,
        track_clicks: request.track_clicks,
    };
    serde_json::to_vec(&canonical).map(|bytes| hex::encode(Sha256::digest(bytes)))
}

/// Configure email routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/emails", post(send_email).get(list_emails))
        .route("/emails/{id}", get(get_email))
        .route("/emails/stats", get(get_email_stats))
}

/// Send an email
#[utoipa::path(
    tag = "Emails",
    post,
    path = "/emails",
    request_body = SendEmailRequestBody,
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "Required for deployment-token requests. Reusing a key with the same payload returns the original delivery; reusing it with a different payload returns 409.")
    ),
    responses(
        (status = 201, description = "Email sent successfully", body = SendEmailResponseBody),
        (status = 400, description = "Invalid request or domain not verified"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Idempotency key was already used with a different payload"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn send_email(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    headers: HeaderMap,
    Json(request): Json<SendEmailRequestBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsSend);

    // Validate request
    if request.to.is_empty() {
        return Err(bad_request()
            .detail("At least one recipient is required")
            .build());
    }

    if request.html.is_none() && request.text.is_none() {
        return Err(bad_request()
            .detail("Either html or text body is required")
            .build());
    }

    let send_request = SendEmailRequest {
        from: request.from.clone(),
        from_name: request.from_name.clone(),
        to: request.to.clone(),
        cc: request.cc.clone(),
        bcc: request.bcc.clone(),
        reply_to: request.reply_to.clone(),
        subject: request.subject.clone(),
        html: request.html.clone(),
        text: request.text.clone(),
        headers: request.headers.clone(),
        tags: request.tags.clone(),
        track_opens: request.track_opens.unwrap_or(false),
        track_clicks: request.track_clicks.unwrap_or(false),
    };

    let deployment = auth.deployment_token_info();
    let deployment_idempotency_key = deployment
        .as_ref()
        .map(|_| deployment_idempotency_key(&headers))
        .transpose()?;
    let deployment_principal = deployment
        .as_ref()
        .map(|principal| DeploymentEmailPrincipal {
            token_id: principal.token_id,
            token_name: principal.token_name.clone(),
            project_id: principal.project_id,
            environment_id: principal.environment_id,
            deployment_id: principal.deployment_id,
        });
    let correlation_id = Uuid::new_v4();
    let sender_domain = request
        .from
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_ascii_lowercase())
        .unwrap_or_else(|| "invalid".to_string());
    let recipient_count = request.to.len()
        + request.cc.as_ref().map_or(0, Vec::len)
        + request.bcc.as_ref().map_or(0, Vec::len);
    let request_fingerprint =
        email_request_fingerprint(&request).map_err(|serialization_error| {
            error!("Could not fingerprint email request for auditing: {serialization_error}");
            internal_server_error()
                .title("Email Audit Preparation Failed")
                .detail("The email was not sent because its audit fingerprint could not be created")
                .build()
        })?;
    let request_audit = EmailSendRequestedAudit {
        user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        deployment_principal: deployment_principal.clone(),
        correlation_id,
        sender_domain: sender_domain.clone(),
        recipient_count,
        request_fingerprint: request_fingerprint.clone(),
    };
    state
        .audit_service
        .create_audit_log(&request_audit)
        .await
        .map_err(|audit_error| {
            error!("Refusing unaudited email send: {audit_error}");
            internal_server_error()
                .title("Audit Log Unavailable")
                .detail("The email was not sent because its audit record could not be stored")
                .build()
        })?;

    let result = if let Some(deployment) = deployment.as_ref() {
        let Some(idempotency_key) = deployment_idempotency_key else {
            return Err(internal_server_error()
                .title("Idempotency Validation Failed")
                .detail("The validated deployment idempotency key was unavailable")
                .build());
        };
        state
            .email_service
            .send_for_project(send_request, deployment.project_id, idempotency_key)
            .await
    } else {
        state.email_service.send(send_request).await
    }
    .map_err(|e| {
        error!("Failed to send email: {}", e);
        match &e {
            crate::errors::EmailError::DomainNotVerified(msg) => {
                bad_request().detail(msg.clone()).build()
            }
            crate::errors::EmailError::Validation(msg) => bad_request().detail(msg.clone()).build(),
            crate::errors::EmailError::DomainNotAuthorized { .. } => {
                forbidden().detail(e.to_string()).build()
            }
            crate::errors::EmailError::IdempotencyConflict { .. } => {
                conflict().detail(e.to_string()).build()
            }
            _ => internal_server_error()
                .detail(format!("Failed to send email: {}", e))
                .build(),
        }
    })?;

    // Create audit log
    let audit = EmailSentAudit {
        user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        deployment_principal,
        correlation_id,
        email_id: result.id,
        sender_domain,
        recipient_count,
        request_fingerprint,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = SendEmailResponseBody {
        id: result.id.to_string(),
        status: result.status,
        provider_message_id: result.provider_message_id,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

fn deployment_idempotency_key(headers: &HeaderMap) -> Result<String, Problem> {
    const HEADER: &str = "idempotency-key";
    let value = headers
        .get(HEADER)
        .ok_or_else(|| {
            bad_request()
                .detail("Deployment-token email requests require an Idempotency-Key header")
                .build()
        })?
        .to_str()
        .map_err(|_| {
            bad_request()
                .detail("Idempotency-Key must contain visible ASCII characters")
                .build()
        })?;

    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        return Err(bad_request()
            .detail(
                "Idempotency-Key must be 1-128 characters using letters, digits, '-', '_', ':' or '.'",
            )
            .build());
    }

    Ok(value.to_string())
}

/// List emails with optional filtering
#[utoipa::path(
    tag = "Emails",
    get,
    path = "/emails",
    params(ListEmailsQuery),
    responses(
        (status = 200, description = "List of emails", body = PaginatedEmailsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_emails(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListEmailsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let options = ListEmailsOptions {
        domain_id: query.domain_id,
        project_id: query.project_id,
        status: query.status,
        from_address: query.from_address,
        page: query.page,
        page_size: query.page_size,
    };

    let (emails, total) = state.email_service.list(options).await.map_err(|e| {
        error!("Failed to list emails: {}", e);
        internal_server_error()
            .detail("Failed to list emails")
            .build()
    })?;

    let data: Vec<EmailResponse> = emails
        .into_iter()
        .map(|e| EmailResponse {
            id: e.id.to_string(),
            domain_id: e.domain_id,
            project_id: e.project_id,
            from_address: e.from_address,
            from_name: e.from_name,
            to_addresses: parse_json_array(e.to_addresses),
            cc_addresses: e.cc_addresses.map(parse_json_array),
            bcc_addresses: e.bcc_addresses.map(parse_json_array),
            reply_to: e.reply_to,
            subject: e.subject,
            html_body: e.html_body,
            tracked_html_body: e.tracked_html_body,
            text_body: e.text_body,
            headers: e.headers.and_then(parse_json_map),
            tags: e.tags.map(parse_json_array),
            status: e.status,
            provider_message_id: e.provider_message_id,
            error_message: e.error_message,
            sent_at: e.sent_at.map(|dt| dt.to_rfc3339()),
            created_at: e.created_at.to_rfc3339(),
            track_opens: e.track_opens,
            track_clicks: e.track_clicks,
            open_count: e.open_count,
            click_count: e.click_count,
            first_opened_at: e.first_opened_at.map(|dt| dt.to_rfc3339()),
            first_clicked_at: e.first_clicked_at.map(|dt| dt.to_rfc3339()),
        })
        .collect();

    let response = PaginatedEmailsResponse {
        data,
        total,
        page: query.page.unwrap_or(1),
        page_size: query.page_size.unwrap_or(20),
    };

    Ok(Json(response))
}

/// Get an email by ID
#[utoipa::path(
    tag = "Emails",
    get,
    path = "/emails/{id}",
    responses(
        (status = 200, description = "Email details", body = EmailResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Email not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = String, Path, description = "Email ID (UUID)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let email_id = Uuid::parse_str(&id)
        .map_err(|_| bad_request().detail("Invalid email ID format").build())?;

    let email = state.email_service.get(email_id).await.map_err(|e| {
        error!("Failed to get email: {}", e);
        not_found().detail("Email not found").build()
    })?;

    let response = EmailResponse {
        id: email.id.to_string(),
        domain_id: email.domain_id,
        project_id: email.project_id,
        from_address: email.from_address,
        from_name: email.from_name,
        to_addresses: parse_json_array(email.to_addresses),
        cc_addresses: email.cc_addresses.map(parse_json_array),
        bcc_addresses: email.bcc_addresses.map(parse_json_array),
        reply_to: email.reply_to,
        subject: email.subject,
        html_body: email.html_body,
        tracked_html_body: email.tracked_html_body,
        text_body: email.text_body,
        headers: email.headers.and_then(parse_json_map),
        tags: email.tags.map(parse_json_array),
        status: email.status,
        provider_message_id: email.provider_message_id,
        error_message: email.error_message,
        sent_at: email.sent_at.map(|dt| dt.to_rfc3339()),
        created_at: email.created_at.to_rfc3339(),
        track_opens: email.track_opens,
        track_clicks: email.track_clicks,
        open_count: email.open_count,
        click_count: email.click_count,
        first_opened_at: email.first_opened_at.map(|dt| dt.to_rfc3339()),
        first_clicked_at: email.first_clicked_at.map(|dt| dt.to_rfc3339()),
    };

    Ok(Json(response))
}

/// Get email statistics
#[utoipa::path(
    tag = "Emails",
    get,
    path = "/emails/stats",
    params(
        ("domain_id" = Option<i32>, Query, description = "Optional domain ID to filter stats")
    ),
    responses(
        (status = 200, description = "Email statistics", body = EmailStatsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email_stats(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<StatsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let stats = state
        .email_service
        .count_by_status(query.domain_id)
        .await
        .map_err(|e| {
            error!("Failed to get email stats: {}", e);
            internal_server_error()
                .detail("Failed to get email statistics")
                .build()
        })?;

    let response = EmailStatsResponse {
        total: stats.total,
        sent: stats.sent,
        failed: stats.failed,
        queued: stats.queued,
        captured: stats.captured,
        sending: stats.sending,
        delivery_unknown: stats.delivery_unknown,
    };

    Ok(Json(response))
}

#[derive(Debug, serde::Deserialize)]
pub struct StatsQuery {
    pub domain_id: Option<i32>,
}

/// Parse a serde_json::Value array to Vec<String>
fn parse_json_array(value: serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Parse a serde_json::Value object to HashMap<String, String>
fn parse_json_map(value: serde_json::Value) -> Option<std::collections::HashMap<String, String>> {
    match value {
        serde_json::Value::Object(obj) => {
            let map: std::collections::HashMap<String, String> = obj
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect();
            if map.is_empty() {
                None
            } else {
                Some(map)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_idempotency_key_is_required_and_bounded() {
        let mut headers = HeaderMap::new();
        assert!(deployment_idempotency_key(&headers).is_err());

        headers.insert(
            "idempotency-key",
            "notification:tenant.delivery".parse().unwrap(),
        );
        assert_eq!(
            deployment_idempotency_key(&headers).unwrap(),
            "notification:tenant.delivery"
        );

        headers.insert("idempotency-key", "contains spaces".parse().unwrap());
        assert!(deployment_idempotency_key(&headers).is_err());
        headers.insert("idempotency-key", "x".repeat(129).parse().unwrap());
        assert!(deployment_idempotency_key(&headers).is_err());
    }

    // ============================================
    // HTTP-level regression tests
    //
    // These drive the real `send_email` handler (not `EmailService` directly)
    // so a guard macro placed before the deployment-token branch — as
    // `deny_deployment_token!` was — fails the test instead of only being
    // caught by reading the code. A deployment token must be able to reach
    // `send_for_project` through this route.
    // ============================================

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::middleware;
    use axum::Router;
    use http_body_util::BodyExt;
    use sea_orm::{
        ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseBackend, Statement,
    };
    use temps_auth::{AuthContext, Role};
    use temps_core::{AuditLogger, AuditOperation, RequestMetadata};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{deployment_tokens::DeploymentTokenPermission, email_domains, users};
    use tower::ServiceExt;

    #[test]
    fn audit_fingerprint_is_independent_of_header_insertion_order() {
        let request_with_headers = |headers| SendEmailRequestBody {
            from: "sender@example.com".to_string(),
            from_name: Some("Sender".to_string()),
            to: vec!["recipient@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "Canonical audit".to_string(),
            html: None,
            text: Some("body".to_string()),
            headers: Some(headers),
            tags: Some(vec!["audit".to_string()]),
            track_opens: Some(false),
            track_clicks: Some(false),
        };
        let first = request_with_headers(std::collections::HashMap::from([
            ("X-Zeta".to_string(), "two".to_string()),
            ("X-Alpha".to_string(), "one".to_string()),
        ]));
        let second = request_with_headers(std::collections::HashMap::from([
            ("X-Alpha".to_string(), "one".to_string()),
            ("X-Zeta".to_string(), "two".to_string()),
        ]));

        assert_eq!(
            email_request_fingerprint(&first).unwrap(),
            email_request_fingerprint(&second).unwrap()
        );
    }

    struct MockAuditLogger;

    #[async_trait::async_trait]
    impl AuditLogger for MockAuditLogger {
        async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FailingAuditLogger;

    #[async_trait::async_trait]
    impl AuditLogger for FailingAuditLogger {
        async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> anyhow::Result<()> {
            anyhow::bail!("intentional audit storage failure")
        }
    }

    fn create_test_encryption_service() -> Arc<temps_core::EncryptionService> {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(temps_core::EncryptionService::new(key).unwrap())
    }

    fn test_request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test-agent".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost:3000".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn test_user() -> users::Model {
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
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

    async fn setup_test_env_with_audit(
        audit_service: Arc<dyn AuditLogger>,
    ) -> Option<(TestDatabase, Arc<AppState>)> {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string())
                {
                    eprintln!("Skipping Docker-dependent email handler test: {error}");
                    return None;
                }
                panic!("Email handler test database or migrations failed: {error}");
            }
        };
        let encryption_service = create_test_encryption_service();
        let provider_service = Arc::new(crate::services::ProviderService::new(
            db.db.clone(),
            encryption_service,
        ));
        let domain_service = Arc::new(crate::services::DomainService::new(
            db.db.clone(),
            provider_service.clone(),
        ));
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "0.0.0.0:3000".to_string(),
            database_url: "postgres://localhost/test".to_string(),
            tls_address: None,
            console_address: "0.0.0.0:3001".to_string(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: std::path::PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-encryption-key-32bytes!!!!!".to_string(),
            api_base_url: "http://localhost:3000".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        });
        let config_service = Arc::new(temps_config::ConfigService::new(
            server_config,
            db.db.clone(),
        ));
        let tracking_setup_service = Arc::new(crate::services::TrackingSetupService::new(
            provider_service.clone(),
            db.db.clone(),
        ));
        let tracking_service = Arc::new(crate::services::TrackingService::with_base_url(
            db.db.clone(),
            config_service.clone(),
            "http://localhost:3000".to_string(),
        ));
        let suppression_service = Arc::new(crate::services::SuppressionService::new(db.db.clone()));
        let email_service = Arc::new(crate::services::EmailService::new(
            db.db.clone(),
            provider_service.clone(),
            domain_service.clone(),
            tracking_service.clone(),
            suppression_service,
        ));
        let validation_service = Arc::new(crate::services::ValidationService::new(
            crate::services::ValidationConfig::default(),
        ));

        let app_state = Arc::new(AppState {
            provider_service,
            domain_service,
            email_service,
            validation_service,
            tracking_service,
            audit_service,
            project_access_checker: None,
            dns_provider_service: None,
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            tracking_setup_service,
            config_service,
        });

        Some((db, app_state))
    }

    async fn setup_test_env() -> Option<(TestDatabase, Arc<AppState>)> {
        setup_test_env_with_audit(Arc::new(MockAuditLogger)).await
    }

    async fn create_test_provider(
        service: &crate::services::ProviderService,
    ) -> temps_entities::email_providers::Model {
        use crate::providers::{EmailProviderType, SesCredentials};
        use crate::services::{CreateProviderRequest, ProviderCredentials};

        let request = CreateProviderRequest {
            name: format!("Test Provider {}", uuid::Uuid::new_v4()),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        service.create(request).await.unwrap()
    }

    async fn create_test_domain(
        db: &Arc<sea_orm::DatabaseConnection>,
        provider_id: i32,
        domain_name: &str,
    ) -> email_domains::Model {
        let domain = email_domains::ActiveModel {
            provider_id: Set(provider_id),
            domain: Set(domain_name.to_string()),
            status: Set("pending".to_string()),
            spf_record_name: Set(Some(domain_name.to_string())),
            spf_record_value: Set(Some("v=spf1 include:mock.example.com ~all".to_string())),
            dkim_selector: Set(Some("mock".to_string())),
            dkim_record_name: Set(Some(format!("mock._domainkey.{}", domain_name))),
            dkim_record_value: Set(Some("v=DKIM1; k=rsa; p=MOCKPUBLICKEY".to_string())),
            mx_record_name: Set(Some(domain_name.to_string())),
            mx_record_value: Set(Some("feedback-smtp.mock.example.com".to_string())),
            mx_record_priority: Set(Some(10)),
            provider_identity_id: Set(Some(format!("mock-identity-{}", domain_name))),
            ..Default::default()
        };

        domain.insert(db.as_ref()).await.unwrap()
    }

    async fn create_test_project(db: &Arc<sea_orm::DatabaseConnection>) -> i32 {
        let slug = format!("email-handler-test-{}", uuid::Uuid::new_v4());
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "INSERT INTO projects \
                     (name,repo_name,repo_owner,directory,main_branch,preset,created_at,updated_at,slug) \
                     VALUES ('Email Handler Test','email-handler-test','tests','.','main','python',now(),now(),'{slug}') \
                     RETURNING id"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get("", "id").unwrap()
    }

    /// Build the `/emails` route with a deployment-token auth context injected,
    /// mirroring how the real auth middleware would populate it for a
    /// deployment-token-authenticated request.
    fn build_deployment_token_app(state: Arc<AppState>, project_id: i32) -> Router {
        let auth_middleware = middleware::from_fn(
            move |mut req: Request<Body>, next: axum::middleware::Next| async move {
                let auth_context = AuthContext::new_deployment_token(
                    project_id,
                    None,
                    None,
                    1,
                    "test-deployment-token".to_string(),
                    vec![DeploymentTokenPermission::EmailsSend],
                );
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(test_request_metadata());
                next.run(req).await
            },
        );

        routes().layer(auth_middleware).with_state(state)
    }

    /// Build the `/emails` route with a regular session auth context, for
    /// comparison against the deployment-token path.
    fn build_session_app(state: Arc<AppState>) -> Router {
        let auth_middleware = middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                let auth_context = AuthContext::new_session(test_user(), Role::Admin);
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(test_request_metadata());
                next.run(req).await
            },
        );

        routes().layer(auth_middleware).with_state(state)
    }

    fn send_request(domain: &str, idempotency_key: Option<&str>) -> Request<Body> {
        let body = serde_json::json!({
            "from": format!("sender@{domain}"),
            "to": ["recipient@test.com"],
            "subject": "Test",
            "html": "<p>Test</p>",
        });
        let mut builder = Request::builder()
            .method("POST")
            .uri("/emails")
            .header("content-type", "application/json");
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn deployment_token_reaches_project_scoped_send_not_a_blanket_403() {
        let Some((db, state)) = setup_test_env().await else {
            return;
        };
        let provider = create_test_provider(&state.provider_service).await;
        let domain = create_test_domain(&db.db, provider.id, "handler-test.example.com").await;
        let project_id = create_test_project(&db.db).await;
        state
            .domain_service
            .authorize_project(domain.id, project_id)
            .await
            .unwrap();

        let app = build_deployment_token_app(state, project_id);
        let response = app
            .oneshot(send_request(
                "handler-test.example.com",
                Some("handler-test-1"),
            ))
            .await
            .unwrap();

        // Must not be the blanket 403 `deny_deployment_token!` used to
        // return — the request has to actually reach `send_for_project` and
        // either succeed or fail on domain/provider state, never on "deployment
        // tokens are not permitted".
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "captured"); // no verified provider identity in this test setup
    }

    #[tokio::test]
    async fn deployment_token_send_requires_idempotency_key() {
        let Some((db, state)) = setup_test_env().await else {
            return;
        };
        let provider = create_test_provider(&state.provider_service).await;
        let domain = create_test_domain(&db.db, provider.id, "no-key-test.example.com").await;
        let project_id = create_test_project(&db.db).await;
        state
            .domain_service
            .authorize_project(domain.id, project_id)
            .await
            .unwrap();

        let app = build_deployment_token_app(state, project_id);
        let response = app
            .oneshot(send_request("no-key-test.example.com", None))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn audit_failure_prevents_provider_delivery_and_email_persistence() {
        let Some((db, state)) = setup_test_env_with_audit(Arc::new(FailingAuditLogger)).await
        else {
            return;
        };
        let provider = create_test_provider(&state.provider_service).await;
        create_test_domain(&db.db, provider.id, "audit-failure.example.com").await;

        let app = build_session_app(state);
        let response = app
            .oneshot(send_request("audit-failure.example.com", None))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let persisted = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS count FROM emails".to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "count")
            .unwrap();
        assert_eq!(persisted, 0, "unaudited sends must not reach persistence");
    }

    #[tokio::test]
    async fn deployment_token_send_denied_for_unauthorized_domain() {
        let Some((db, state)) = setup_test_env().await else {
            return;
        };
        let provider = create_test_provider(&state.provider_service).await;
        create_test_domain(&db.db, provider.id, "unauthorized.example.com").await;
        let project_id = create_test_project(&db.db).await;
        // Note: no `authorize_project` call — this project has no grant.

        let app = build_deployment_token_app(state, project_id);
        let response = app
            .oneshot(send_request(
                "unauthorized.example.com",
                Some("unauthorized-1"),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn session_auth_still_uses_the_non_project_scoped_send_path() {
        let Some((db, state)) = setup_test_env().await else {
            return;
        };
        let provider = create_test_provider(&state.provider_service).await;
        create_test_domain(&db.db, provider.id, "session-test.example.com").await;

        let app = build_session_app(state);
        // No idempotency-key header — the project-scoped path would reject
        // this, but session auth never takes that path.
        let response = app
            .oneshot(send_request("session-test.example.com", None))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }
}
