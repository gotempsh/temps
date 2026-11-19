// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where an app sends each kind of telemetry, and with which credential.
//!
//! One source of truth for the console's framework-agnostic setup
//! ("Plain HTTP / any framework") and its "Copy prompt": the resolved public
//! URL of every ingest route, the auth scheme each one expects, and whether
//! the project already has the credential it needs. URLs are built from the
//! public ingress URL (ADR-051), so a self-hosted instance with a custom
//! domain or a separate public ingress gets addresses that actually reach it.
//!
//! Nothing here returns a credential. Each auth entry names the environment
//! variable an app should read it from and where to create it; header values
//! are templates such as `Bearer $TEMPS_API_KEY`.

use std::sync::Arc;

use sea_orm::{DatabaseBackend, DatabaseConnection, DbErr, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

/// Ingest routes, relative to the public ingress URL. Kept beside the
/// handlers they name so a renamed route breaks a test here.
const ANALYTICS_EVENT_PATH: &str = "/api/_temps/event";
const SESSION_REPLAY_INIT_PATH: &str = "/api/_temps/session-replay/init";
const SESSION_REPLAY_EVENTS_PATH: &str = "/api/_temps/session-replay/events";
const OTLP_BASE_PATH: &str = "/api/otel";

/// Environment variable names the setup and the prompt tell an app to use.
pub const ANALYTICS_KEY_ENV_VAR: &str = "TEMPS_ANALYTICS_KEY";
pub const SENTRY_DSN_ENV_VAR: &str = "SENTRY_DSN";
pub const API_KEY_ENV_VAR: &str = "TEMPS_API_KEY";

#[derive(Debug, Error)]
pub enum IngestSetupError {
    #[error("Project {project_id} not found while resolving its ingest setup")]
    ProjectNotFound { project_id: i32 },
    #[error("Environment {environment_id} not found in project {project_id} while resolving its ingest setup")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },
    #[error("Invalid public ingress URL {url:?} for project {project_id}: {reason}")]
    InvalidPublicUrl {
        project_id: i32,
        url: String,
        reason: &'static str,
    },
    #[error("Database error while resolving the ingest setup of project {project_id}: {source}")]
    Database {
        project_id: i32,
        #[source]
        source: DbErr,
    },
}

/// A kind of telemetry an app can send to Temps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestSignal {
    /// Page views and custom events.
    Analytics,
    /// rrweb session recordings.
    SessionReplay,
    /// Errors, through the Sentry-compatible ingest.
    Errors,
    /// Logs, over OTLP/HTTP.
    Logs,
    /// Traces (spans), over OTLP/HTTP.
    Traces,
}

/// The credential a signal's ingest route expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestAuthKind {
    /// A project analytics ingest key (`pa_…`). Public by design, write-only.
    AnalyticsIngestKey,
    /// A Sentry-compatible DSN. Its key is public by design, write-only.
    SentryDsn,
    /// A Temps API key (`tk_…`). Secret: server-side only.
    ApiKey,
}

/// A header a request must carry. `value` is a template naming an
/// environment variable, never a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IngestHeader {
    #[schema(example = "Authorization")]
    pub name: String,
    #[schema(example = "Bearer $TEMPS_API_KEY")]
    pub value: String,
}

/// One ingest route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IngestEndpoint {
    /// Stable identifier of the route within its signal (`event`, `envelope`,
    /// `otlp_traces`, …).
    #[schema(example = "event")]
    pub name: String,
    #[schema(example = "POST")]
    pub method: String,
    /// Absolute URL, built from the public ingress URL.
    #[schema(example = "https://temps.example.com/api/_temps/event")]
    pub url: String,
    /// The `Content-Type` the route accepts.
    #[schema(example = "application/json")]
    pub content_type: String,
}

/// How to authenticate a signal's requests, without the credential itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IngestAuth {
    pub kind: IngestAuthKind,
    /// Environment variable the app should read the credential from.
    #[schema(example = "TEMPS_ANALYTICS_KEY")]
    pub env_var: String,
    /// Headers to send, as templates over `env_var`.
    pub headers: Vec<IngestHeader>,
    /// Query-string fallback for clients that cannot set headers
    /// (`navigator.sendBeacon`), when the route accepts one.
    #[schema(example = "temps_key")]
    pub query_param: Option<String>,
    /// Whether the credential may ship in browser code. Analytics keys and
    /// DSN keys are public by design; an API key never is.
    pub browser_safe: bool,
    /// Console path where the credential is created or shown.
    #[schema(example = "/projects/my-app/analytics/setup")]
    pub credential_path: String,
    /// `temps` CLI command that creates the credential.
    #[schema(example = "temps analytics keys create --project my-app")]
    pub cli_command: String,
}

/// Everything an app needs to send one signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IngestSignalSetup {
    pub signal: IngestSignal,
    pub endpoints: Vec<IngestEndpoint>,
    pub auth: IngestAuth,
    /// For OTLP signals: the value of `OTEL_EXPORTER_OTLP_ENDPOINT`. SDKs
    /// append `/v1/traces` or `/v1/logs` to it.
    #[schema(example = "https://temps.example.com/api/otel")]
    pub otlp_endpoint: Option<String>,
    /// Whether the project has what this signal needs. `false` means a
    /// credential still has to be created.
    pub configured: bool,
    /// What is missing when `configured` is `false`.
    pub reason: Option<String>,
    /// Console path that fixes what is missing, or the signal's setup page.
    #[schema(example = "/projects/my-app/errors/setup")]
    pub setup_path: String,
}

/// The ingest setup of one project (and optionally one environment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectIngestSetup {
    pub project_id: i32,
    #[schema(example = "my-app")]
    pub project_slug: String,
    /// The environment the URLs attribute telemetry to, when one was asked for.
    pub environment_id: Option<i32>,
    /// Public ingress URL every endpoint is built from, without a trailing slash.
    #[schema(example = "https://temps.example.com")]
    pub public_base_url: String,
    pub signals: Vec<IngestSignalSetup>,
}

#[derive(Debug, FromQueryResult)]
struct IngestSetupRow {
    slug: String,
    environment_found: bool,
    has_analytics_key: bool,
    has_dsn: bool,
}

/// Resolves [`ProjectIngestSetup`] for a project.
pub struct IngestSetupService {
    db: Arc<DatabaseConnection>,
}

impl IngestSetupService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// The ingest setup of `project_id`, with every URL built on
    /// `public_base_url` (the ADR-051 public ingress URL).
    ///
    /// `environment_id`, when given, must belong to the project. It scopes the
    /// credential checks (a key bound to another environment does not count)
    /// and attributes OTLP data to that environment through the path-based
    /// OTLP routes.
    pub async fn get_ingest_setup(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        public_base_url: &str,
    ) -> Result<ProjectIngestSetup, IngestSetupError> {
        let base = normalize_base_url(project_id, public_base_url)?;

        // One round trip: the project, the environment check and both
        // credential checks.
        let row = IngestSetupRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT p.slug,
                   ($2::int IS NULL OR EXISTS (
                       SELECT 1 FROM environments e
                       WHERE e.id = $2 AND e.project_id = p.id AND e.deleted_at IS NULL
                   )) AS environment_found,
                   EXISTS (
                       SELECT 1 FROM analytics_ingest_keys k
                       WHERE k.project_id = p.id AND k.is_active AND k.revoked_at IS NULL
                         AND ($2::int IS NULL OR k.environment_id IS NULL OR k.environment_id = $2)
                   ) AS has_analytics_key,
                   EXISTS (
                       SELECT 1 FROM project_dsns d
                       WHERE d.project_id = p.id AND d.is_active
                         AND ($2::int IS NULL OR d.environment_id IS NULL OR d.environment_id = $2)
                   ) AS has_dsn
            FROM projects p
            WHERE p.id = $1 AND p.is_deleted = false
            "#,
            vec![project_id.into(), environment_id.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| IngestSetupError::Database { project_id, source })?
        .ok_or(IngestSetupError::ProjectNotFound { project_id })?;

        if let (Some(environment_id), false) = (environment_id, row.environment_found) {
            return Err(IngestSetupError::EnvironmentNotFound {
                project_id,
                environment_id,
            });
        }

        Ok(build_setup(
            project_id,
            environment_id,
            &row.slug,
            &base,
            row.has_analytics_key,
            row.has_dsn,
        ))
    }
}

/// Trim and validate the public ingress URL. It must be absolute `http(s)`;
/// anything else would produce URLs no client can call.
fn normalize_base_url(project_id: i32, url: &str) -> Result<String, IngestSetupError> {
    let trimmed = url.trim().trim_end_matches('/');
    let invalid = |reason| IngestSetupError::InvalidPublicUrl {
        project_id,
        url: url.to_string(),
        reason,
    };
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| invalid("must start with http:// or https://"))?;
    if rest.is_empty() || rest.starts_with('/') {
        return Err(invalid("has no host"));
    }
    Ok(trimmed.to_string())
}

fn endpoint(name: &str, method: &str, url: String, content_type: &str) -> IngestEndpoint {
    IngestEndpoint {
        name: name.to_string(),
        method: method.to_string(),
        url,
        content_type: content_type.to_string(),
    }
}

fn header(name: &str, value: String) -> IngestHeader {
    IngestHeader {
        name: name.to_string(),
        value,
    }
}

fn build_setup(
    project_id: i32,
    environment_id: Option<i32>,
    slug: &str,
    base: &str,
    has_analytics_key: bool,
    has_dsn: bool,
) -> ProjectIngestSetup {
    let project_path = format!("/projects/{slug}");
    let analytics_setup = format!("{project_path}/analytics/setup");
    let errors_setup = format!("{project_path}/errors/setup");
    let env_flag = environment_id
        .map(|id| format!(" --environment-id {id}"))
        .unwrap_or_default();

    let analytics_auth = IngestAuth {
        kind: IngestAuthKind::AnalyticsIngestKey,
        env_var: ANALYTICS_KEY_ENV_VAR.to_string(),
        headers: vec![header(
            "X-Temps-Analytics-Key",
            format!("${ANALYTICS_KEY_ENV_VAR}"),
        )],
        query_param: Some("temps_key".to_string()),
        browser_safe: true,
        credential_path: analytics_setup.clone(),
        cli_command: format!("temps analytics keys create --project {slug}{env_flag}"),
    };
    let (analytics_configured, analytics_reason) = if has_analytics_key {
        (true, None)
    } else {
        (
            false,
            Some("No active analytics ingest key exists for this project yet".to_string()),
        )
    };

    let api_key_auth = |otlp_project_header: bool| IngestAuth {
        kind: IngestAuthKind::ApiKey,
        env_var: API_KEY_ENV_VAR.to_string(),
        headers: {
            let mut headers = vec![header(
                "Authorization",
                format!("Bearer ${API_KEY_ENV_VAR}"),
            )];
            if otlp_project_header {
                headers.push(header("X-Temps-Project-Id", project_id.to_string()));
            }
            headers
        },
        query_param: None,
        browser_safe: false,
        credential_path: "/keys/new".to_string(),
        cli_command: format!("temps apikeys create --name {slug}-telemetry --role user"),
    };

    // With an environment, the path-based routes attribute data to it and
    // need no project header; without one, the header-based routes take the
    // project from `X-Temps-Project-Id`.
    let otlp_signal_url = |signal: &str| match environment_id {
        Some(env) => format!("{base}{OTLP_BASE_PATH}/v1/{project_id}/{env}/0/{signal}"),
        None => format!("{base}{OTLP_BASE_PATH}/v1/{signal}"),
    };
    let otlp = |signal: IngestSignal, name: &str, path: &str| IngestSignalSetup {
        signal,
        endpoints: vec![endpoint(
            name,
            "POST",
            otlp_signal_url(path),
            "application/x-protobuf",
        )],
        auth: api_key_auth(environment_id.is_none()),
        otlp_endpoint: Some(format!("{base}{OTLP_BASE_PATH}")),
        configured: true,
        reason: None,
        setup_path: format!("{project_path}/traces#traces-setup"),
    };

    let signals = vec![
        IngestSignalSetup {
            signal: IngestSignal::Analytics,
            endpoints: vec![endpoint(
                "event",
                "POST",
                format!("{base}{ANALYTICS_EVENT_PATH}"),
                "application/json",
            )],
            auth: analytics_auth.clone(),
            otlp_endpoint: None,
            configured: analytics_configured,
            reason: analytics_reason.clone(),
            setup_path: analytics_setup.clone(),
        },
        IngestSignalSetup {
            signal: IngestSignal::SessionReplay,
            endpoints: vec![
                endpoint(
                    "init",
                    "POST",
                    format!("{base}{SESSION_REPLAY_INIT_PATH}"),
                    "application/json",
                ),
                endpoint(
                    "events",
                    "POST",
                    format!("{base}{SESSION_REPLAY_EVENTS_PATH}"),
                    "application/json",
                ),
            ],
            auth: analytics_auth,
            otlp_endpoint: None,
            configured: analytics_configured,
            reason: analytics_reason,
            setup_path: analytics_setup,
        },
        IngestSignalSetup {
            signal: IngestSignal::Errors,
            endpoints: vec![
                endpoint(
                    "envelope",
                    "POST",
                    format!("{base}/api/{project_id}/envelope/"),
                    "application/x-sentry-envelope",
                ),
                endpoint(
                    "store",
                    "POST",
                    format!("{base}/api/{project_id}/store/"),
                    "application/json",
                ),
            ],
            auth: IngestAuth {
                kind: IngestAuthKind::SentryDsn,
                env_var: SENTRY_DSN_ENV_VAR.to_string(),
                headers: vec![header(
                    "X-Sentry-Auth",
                    "Sentry sentry_version=7, sentry_key=<public key from $SENTRY_DSN>".to_string(),
                )],
                query_param: Some("sentry_key".to_string()),
                browser_safe: true,
                credential_path: errors_setup.clone(),
                cli_command: format!("temps dsn get-or-create --project-id {project_id}{env_flag}"),
            },
            otlp_endpoint: None,
            configured: has_dsn,
            reason: (!has_dsn).then(|| "No active DSN exists for this project yet".to_string()),
            setup_path: errors_setup,
        },
        otlp(IngestSignal::Logs, "otlp_logs", "logs"),
        otlp(IngestSignal::Traces, "otlp_traces", "traces"),
    ];

    ProjectIngestSetup {
        project_id,
        project_slug: slug.to_string(),
        environment_id,
        public_base_url: base.to_string(),
        signals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{MockDatabase, Value};
    use std::collections::BTreeMap;

    fn row(environment_found: bool, analytics: bool, dsn: bool) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("slug", Value::from("my-app")),
            ("environment_found", Value::from(environment_found)),
            ("has_analytics_key", Value::from(analytics)),
            ("has_dsn", Value::from(dsn)),
        ])
    }

    fn service(rows: Vec<BTreeMap<&'static str, Value>>) -> IngestSetupService {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows])
            .into_connection();
        IngestSetupService::new(Arc::new(db))
    }

    fn signal(setup: &ProjectIngestSetup, signal: IngestSignal) -> &IngestSignalSetup {
        setup
            .signals
            .iter()
            .find(|s| s.signal == signal)
            .expect("every signal is listed")
    }

    #[tokio::test]
    async fn builds_every_signal_on_the_public_ingress_url() {
        let setup = service(vec![row(true, true, true)])
            .get_ingest_setup(42, None, " https://temps.example.com/ ")
            .await
            .expect("setup resolves");

        assert_eq!(setup.project_slug, "my-app");
        assert_eq!(setup.public_base_url, "https://temps.example.com");
        assert_eq!(setup.signals.len(), 5);

        let analytics = signal(&setup, IngestSignal::Analytics);
        assert_eq!(
            analytics.endpoints[0].url,
            "https://temps.example.com/api/_temps/event"
        );
        assert_eq!(analytics.auth.headers[0].name, "X-Temps-Analytics-Key");
        assert_eq!(analytics.auth.query_param.as_deref(), Some("temps_key"));
        assert!(analytics.auth.browser_safe);
        assert!(analytics.configured);

        let replay = signal(&setup, IngestSignal::SessionReplay);
        assert_eq!(
            replay.endpoints[1].url,
            "https://temps.example.com/api/_temps/session-replay/events"
        );

        let errors = signal(&setup, IngestSignal::Errors);
        assert_eq!(
            errors.endpoints[0].url,
            "https://temps.example.com/api/42/envelope/"
        );
        assert_eq!(errors.auth.env_var, "SENTRY_DSN");
        assert!(errors.configured);

        let traces = signal(&setup, IngestSignal::Traces);
        assert_eq!(
            traces.endpoints[0].url,
            "https://temps.example.com/api/otel/v1/traces"
        );
        assert_eq!(
            traces.otlp_endpoint.as_deref(),
            Some("https://temps.example.com/api/otel")
        );
        assert!(!traces.auth.browser_safe);
        // Without an environment the project travels in a header.
        assert!(traces
            .auth
            .headers
            .iter()
            .any(|h| h.name == "X-Temps-Project-Id" && h.value == "42"));

        let logs = signal(&setup, IngestSignal::Logs);
        assert_eq!(
            logs.endpoints[0].url,
            "https://temps.example.com/api/otel/v1/logs"
        );
        assert_eq!(logs.endpoints[0].content_type, "application/x-protobuf");
    }

    #[tokio::test]
    async fn an_environment_uses_the_path_based_otlp_routes() {
        let setup = service(vec![row(true, true, true)])
            .get_ingest_setup(42, Some(7), "http://localhost:8080")
            .await
            .expect("setup resolves");

        assert_eq!(setup.environment_id, Some(7));
        let traces = signal(&setup, IngestSignal::Traces);
        assert_eq!(
            traces.endpoints[0].url,
            "http://localhost:8080/api/otel/v1/42/7/0/traces"
        );
        assert!(traces
            .auth
            .headers
            .iter()
            .all(|h| h.name != "X-Temps-Project-Id"));
        assert!(signal(&setup, IngestSignal::Analytics)
            .auth
            .cli_command
            .ends_with("--environment-id 7"));
    }

    #[tokio::test]
    async fn missing_credentials_onboard_instead_of_hiding() {
        let setup = service(vec![row(true, false, false)])
            .get_ingest_setup(42, None, "https://temps.example.com")
            .await
            .expect("setup resolves");

        let analytics = signal(&setup, IngestSignal::Analytics);
        assert!(!analytics.configured);
        assert!(analytics
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("analytics ingest key")));
        assert_eq!(analytics.setup_path, "/projects/my-app/analytics/setup");

        let errors = signal(&setup, IngestSignal::Errors);
        assert!(!errors.configured);
        assert!(errors.reason.as_deref().is_some_and(|r| r.contains("DSN")));
        assert_eq!(errors.setup_path, "/projects/my-app/errors/setup");
    }

    #[tokio::test]
    async fn no_header_carries_a_credential() {
        let setup = service(vec![row(true, true, true)])
            .get_ingest_setup(42, None, "https://temps.example.com")
            .await
            .expect("setup resolves");
        for signal in &setup.signals {
            for header in &signal.auth.headers {
                assert!(
                    header.value.contains('$') || header.name == "X-Temps-Project-Id",
                    "{:?} header {} must be a template, got {}",
                    signal.signal,
                    header.name,
                    header.value
                );
            }
        }
    }

    #[tokio::test]
    async fn unknown_project_is_not_found_with_its_id() {
        let err = service(vec![])
            .get_ingest_setup(99, None, "https://temps.example.com")
            .await
            .expect_err("no row");
        assert!(matches!(
            err,
            IngestSetupError::ProjectNotFound { project_id: 99 }
        ));
        assert!(err.to_string().contains("99"));
    }

    #[tokio::test]
    async fn environment_of_another_project_is_not_found() {
        let err = service(vec![row(false, true, true)])
            .get_ingest_setup(42, Some(8), "https://temps.example.com")
            .await
            .expect_err("environment not in project");
        assert!(matches!(
            err,
            IngestSetupError::EnvironmentNotFound {
                project_id: 42,
                environment_id: 8
            }
        ));
    }

    #[tokio::test]
    async fn database_errors_carry_the_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let err = IngestSetupService::new(Arc::new(db))
            .get_ingest_setup(42, None, "https://temps.example.com")
            .await
            .expect_err("db fails");
        assert!(matches!(
            err,
            IngestSetupError::Database { project_id: 42, .. }
        ));
    }

    #[test]
    fn public_url_must_be_absolute_http() {
        for bad in [
            "",
            "temps.example.com",
            "ftp://temps.example.com",
            "https://",
            "https:///x",
        ] {
            assert!(
                matches!(
                    normalize_base_url(1, bad),
                    Err(IngestSetupError::InvalidPublicUrl { .. })
                ),
                "{bad:?} must be rejected"
            );
        }
        assert_eq!(
            normalize_base_url(1, "https://temps.example.com:8443/")
                .ok()
                .as_deref(),
            Some("https://temps.example.com:8443")
        );
    }
}
