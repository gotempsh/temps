// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod audit;
pub mod base;
pub mod bitbucket;
pub mod compose_preview_problem;
pub mod generic;
pub mod gitea;
pub mod github;
pub mod gitlab;
pub mod public;
pub mod repositories;
pub mod types;
pub mod update_token;

use crate::handlers::types::GitAppState as AppState;
use axum::Router;
use std::sync::Arc;

// Re-export the API documentation
pub use base::GitProvidersApiDoc;
pub use public::PublicRepositoriesApiDoc;

/// Request body ceiling for the GitHub and GitLab event receivers. Matches
/// what these routes accepted before they declared a limit (axum's default),
/// so no delivery that worked is refused; the other providers set their own.
pub(crate) const PROVIDER_EVENT_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Configure the session-authenticated routes for git providers: provider and
/// connection management, repository browsing, and the OAuth/installation
/// callbacks a provider redirects the operator's browser to.
pub fn configure_routes() -> Router<Arc<AppState>> {
    // Combine all route modules
    base::configure_routes()
        .merge(github::configure_routes())
        .merge(gitlab::configure_routes())
        .merge(public::configure_routes())
}

/// Configure the webhook receivers git providers deliver events to (ADR-051
/// D3).
///
/// These are server-to-server calls from the provider, so they are mounted on
/// the public router: reachable without a session and from clients the admin
/// gate denies. Each handler authenticates the request itself — a provider
/// signature (GitHub, GitLab, Gitea) or an unguessable delivery token in the
/// path (Bitbucket, generic) — and none may read the session cookie. Every
/// route declares a request body limit.
pub fn configure_public_routes() -> Router<Arc<AppState>> {
    github::configure_public_routes()
        .merge(gitlab::configure_public_routes())
        .merge(gitea::configure_public_routes())
        .merge(bitbucket::configure_public_routes())
        .merge(generic::configure_public_routes())
}

#[cfg(test)]
mod public_route_tests {
    //! ADR-051 D3: provider webhook receivers live on the public router, the
    //! browser-driven callbacks stay on the admin router, and every receiver
    //! bounds its request body.
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::{
        AuditLogger, AuditOperation, Job, JobQueue, JobReceiver, QueueError, SensitiveAction,
        SensitiveActionAuthorizationError, SensitiveActionAuthorizer, SensitiveActionDecision,
        SensitiveActionPrincipal,
    };
    use tower::ServiceExt; // for `oneshot`

    struct NoopAuditLogger;

    #[async_trait]
    impl AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn AuditOperation,
        ) -> temps_core::anyhow::Result<()> {
            Ok(())
        }
    }

    struct NoopJobQueue;

    #[async_trait]
    impl JobQueue for NoopJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            unreachable!("route tests never subscribe to the job queue")
        }
    }

    struct AllowSensitiveActions;

    #[async_trait]
    impl SensitiveActionAuthorizer for AllowSensitiveActions {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(SensitiveActionDecision::Allow)
        }
    }

    /// Handler state over a database that has nothing queued: any query a
    /// handler issues fails, so a response that needed no data proves the
    /// request was turned away before any database work.
    fn state() -> Arc<AppState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(
                temps_config::ServerConfig::new(
                    "127.0.0.1:3000".to_string(),
                    "postgresql://test".to_string(),
                    None,
                    Some("127.0.0.1:8000".to_string()),
                )
                .expect("ServerConfig::new"),
            ),
            db.clone(),
        ));
        let queue: Arc<dyn JobQueue> = Arc::new(NoopJobQueue);
        let git_provider_manager = Arc::new(
            crate::services::git_provider_manager::GitProviderManager::new(
                db.clone(),
                Arc::new(
                    temps_core::EncryptionService::new(
                        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    )
                    .expect("test encryption key"),
                ),
                queue.clone(),
                config_service.clone(),
            ),
        );
        let github_service = Arc::new(crate::services::github::GithubAppService::new(
            db.clone(),
            queue,
            git_provider_manager.clone(),
        ));
        crate::handlers::types::create_git_app_state(
            Arc::new(crate::services::repository::RepositoryService::new(
                db.clone(),
            )),
            git_provider_manager.clone(),
            config_service,
            Arc::new(NoopAuditLogger),
            github_service.clone(),
            Arc::new(crate::services::cache::GitProviderCacheManager::new()),
            Arc::new(
                crate::services::connection_health::ConnectionHealthService::new(
                    db,
                    git_provider_manager,
                    github_service,
                    "http://127.0.0.1:8000".to_string(),
                ),
            ),
            Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            Arc::new(AllowSensitiveActions),
        )
    }

    /// Dispatch one request to `router` in-process and return the status.
    async fn status(router: Router<Arc<AppState>>, request: Request<Body>) -> StatusCode {
        router
            .with_state(state())
            .oneshot(request)
            .await
            .expect("router responds")
            .status()
    }

    fn post(path: &str, body: impl Into<Body>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .body(body.into())
            .expect("request builds")
    }

    fn get(path: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("request builds")
    }

    const RECEIVERS: [&str; 7] = [
        "/webhook/git/github/events",
        "/webhook/source/github/events",
        "/webhook/git/github/repo-events",
        "/webhook/git/gitlab/events",
        "/webhook/git/gitea/events",
        "/webhook/git/bitbucket/events/not-a-registered-token",
        "/webhook/git/generic/events/not-a-registered-token",
    ];

    #[tokio::test]
    async fn receivers_are_on_the_public_router_and_not_the_admin_one() {
        for path in RECEIVERS {
            let on_public = status(configure_public_routes(), post(path, "{}")).await;
            assert_ne!(
                on_public,
                StatusCode::NOT_FOUND,
                "{path} must be mounted on the public router"
            );
            assert_ne!(
                on_public,
                StatusCode::METHOD_NOT_ALLOWED,
                "{path} must accept POST on the public router"
            );

            assert_eq!(
                status(configure_routes(), post(path, "{}")).await,
                StatusCode::NOT_FOUND,
                "{path} must no longer be mounted on the admin router"
            );
        }
    }

    #[tokio::test]
    async fn browser_callbacks_and_management_routes_stay_off_the_public_router() {
        for path in [
            "/webhook/git/github/auth",
            "/webhook/git/github/callback",
            "/webhook/git/github/install",
            "/webhook/source/github/install",
            "/webhook/git/gitlab/auth",
            "/git-providers",
            "/git-connections",
            "/repositories",
        ] {
            assert_eq!(
                status(configure_public_routes(), get(path)).await,
                StatusCode::NOT_FOUND,
                "{path} is session-bound and must not be on the public router"
            );
        }
    }

    #[tokio::test]
    async fn a_github_delivery_without_a_valid_signature_is_rejected() {
        for signature in [None, Some("sha256=deadbeef"), Some("not-a-signature")] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/webhook/git/github/events")
                .header("X-GitHub-Event", "push");
            if let Some(signature) = signature {
                request = request.header("X-Hub-Signature-256", signature);
            }
            let request = request
                .body(Body::from(r#"{"ref":"refs/heads/main"}"#))
                .expect("request builds");
            assert_eq!(
                status(configure_public_routes(), request).await,
                StatusCode::UNAUTHORIZED,
                "signature {signature:?} must not be accepted"
            );
        }
    }

    /// A repository webhook delivery is authenticated against the secret of
    /// a project connected to the repository it names. One that carries no
    /// usable `X-Hub-Signature-256` is turned away before any lookup (the
    /// state's database fails every query, so a 401 proves none was made).
    #[tokio::test]
    async fn a_github_repo_delivery_without_a_signature_is_rejected_before_lookup() {
        for signature in [None, Some("not-a-signature"), Some("sha1=deadbeef")] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/webhook/git/github/repo-events")
                .header("X-GitHub-Event", "push");
            if let Some(signature) = signature {
                request = request.header("X-Hub-Signature-256", signature);
            }
            let request = request
                .body(Body::from(
                    r#"{"ref":"refs/heads/main","after":"abc","repository":{"name":"r","owner":{"login":"o"}}}"#,
                ))
                .expect("request builds");
            assert_eq!(
                status(configure_public_routes(), request).await,
                StatusCode::UNAUTHORIZED,
                "signature {signature:?} must not be accepted"
            );
        }
    }

    #[tokio::test]
    async fn every_receiver_refuses_an_oversized_body() {
        // Above the largest limit any receiver declares.
        let oversized = vec![b'a'; PROVIDER_EVENT_BODY_LIMIT + 1];

        for path in RECEIVERS {
            assert_eq!(
                status(configure_public_routes(), post(path, oversized.clone())).await,
                StatusCode::PAYLOAD_TOO_LARGE,
                "{path} must bound its request body"
            );
        }
    }
}
