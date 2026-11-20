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
    /// For OTLP signals without an environment: the value of
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`. SDKs append `/v1/traces` or `/v1/logs`
    /// to it. `None` with an environment, whose per-signal URLs in
    /// `endpoints` go in `OTEL_EXPORTER_OTLP_{TRACES,LOGS}_ENDPOINT`.
    #[schema(example = "https://temps.example.com/api/otel")]
    pub otlp_endpoint: Option<String>,
    /// Whether the project has what this signal needs. `false` means a
    /// credential still has to be created. Logs and traces are always `true`:
    /// they authenticate with the caller's own API key, which is not project
    /// state; `auth.credential_path` and `auth.cli_command` say how to get one.
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
        // credential checks. A credential bound to a soft-deleted environment
        // is rejected by the ingest routes, so it does not count here either.
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
                         AND (k.environment_id IS NULL OR EXISTS (
                             SELECT 1 FROM environments ke
                             WHERE ke.id = k.environment_id AND ke.deleted_at IS NULL
                         ))
                   ) AS has_analytics_key,
                   EXISTS (
                       SELECT 1 FROM project_dsns d
                       WHERE d.project_id = p.id AND d.is_active
                         AND ($2::int IS NULL OR d.environment_id IS NULL OR d.environment_id = $2)
                         AND (d.environment_id IS NULL OR EXISTS (
                             SELECT 1 FROM environments de
                             WHERE de.id = d.environment_id AND de.deleted_at IS NULL
                         ))
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

/// Trim and validate the public ingress URL. It must be absolute `http(s)`
/// with a host, no credentials and no query or fragment; anything else would
/// produce URLs no client can call, or leak credentials into setup output.
fn normalize_base_url(project_id: i32, url: &str) -> Result<String, IngestSetupError> {
    let trimmed = url.trim().trim_end_matches('/');
    // The error is returned and logged, so it never repeats credentials.
    let invalid = |reason| IngestSetupError::InvalidPublicUrl {
        project_id,
        url: redact_userinfo(trimmed),
        reason,
    };
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| invalid("must start with http:// or https://"))?;
    // Checked before parsing: the parser reads `https:///x` as host `x`.
    if rest.is_empty() || rest.starts_with('/') {
        return Err(invalid("has no host"));
    }
    // Parsing validates the authority, e.g. rejects a port above 65535.
    let parsed = url::Url::parse(trimmed).map_err(|_| invalid("is not a valid URL"))?;
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(invalid("has no host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid("must not contain credentials"));
    }
    // Ingest paths are appended to the base, so a query or fragment would
    // swallow them (`https://h?x=1/api/_temps/event` posts to `/`).
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(invalid("must not contain a query string or fragment"));
    }
    Ok(trimmed.to_string())
}

/// `scheme://user:pass@host/...` with the userinfo replaced by `***`.
fn redact_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://***@{host}{tail}"),
        None => url.to_string(),
    }
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

    let api_key_auth = |otlp_project_header: bool| {
        IngestAuth {
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
        // The console's API-key form, named like the key the CLI creates.
        credential_path: format!("/settings/keys/new?name={slug}-telemetry"),
        // OTLP ingest accepts any active `tk_` key whose owner can access the
        // project, whatever its permissions, so the exporter key gets only
        // `otel:write`: no reads and no project or deployment management.
        // `--project-id` is left out because project-confined keys (`tkp_`)
        // are not accepted by the OTLP routes.
        cli_command: format!(
            "temps apikeys create --name {slug}-telemetry --role custom --permissions otel:write --yes"
        ),
    }
    };

    // With an environment, the path-based routes attribute data to it and
    // need no project header; without one, the header-based routes take the
    // project from `X-Temps-Project-Id`.
    let otlp_signal_url = |signal: &str| match environment_id {
        Some(env) => format!("{base}{OTLP_BASE_PATH}/v1/{project_id}/{env}/0/{signal}"),
        None => format!("{base}{OTLP_BASE_PATH}/v1/{signal}"),
    };
    // `OTEL_EXPORTER_OTLP_ENDPOINT` only works for the header-based routes:
    // SDKs append `/v1/{signal}` to it, which can't reach the environment's
    // path-based routes. With an environment, callers use the per-signal URLs.
    let otlp_base = environment_id
        .is_none()
        .then(|| format!("{base}{OTLP_BASE_PATH}"));
    let otlp =
        |signal: IngestSignal, name: &str, path: &str, setup_path: String| IngestSignalSetup {
            signal,
            endpoints: vec![endpoint(
                name,
                "POST",
                otlp_signal_url(path),
                "application/x-protobuf",
            )],
            auth: api_key_auth(environment_id.is_none()),
            otlp_endpoint: otlp_base.clone(),
            // The credential is the caller's own API key, not project state, so
            // there is nothing project-side to check; `auth` always says how to
            // create one.
            configured: true,
            reason: None,
            setup_path,
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
        otlp(
            IngestSignal::Logs,
            "otlp_logs",
            "logs",
            format!("{project_path}/telemetry-logs"),
        ),
        otlp(
            IngestSignal::Traces,
            "otlp_traces",
            "traces",
            format!("{project_path}/traces#traces-setup"),
        ),
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
        // SDKs append /v1/{signal} to OTEL_EXPORTER_OTLP_ENDPOINT, which would
        // reach the header-based route and lose the environment.
        for otlp in [IngestSignal::Logs, IngestSignal::Traces] {
            assert_eq!(signal(&setup, otlp).otlp_endpoint, None, "{otlp:?}");
        }
    }

    #[tokio::test]
    async fn logs_and_traces_point_at_their_own_setup_pages() {
        let setup = service(vec![row(true, true, true)])
            .get_ingest_setup(42, None, "https://temps.example.com")
            .await
            .expect("setup resolves");
        assert_eq!(
            signal(&setup, IngestSignal::Logs).setup_path,
            "/projects/my-app/telemetry-logs"
        );
        assert_eq!(
            signal(&setup, IngestSignal::Traces).setup_path,
            "/projects/my-app/traces#traces-setup"
        );
    }

    #[tokio::test]
    async fn the_exporter_key_command_grants_only_otel_write_without_prompts() {
        let setup = service(vec![row(true, true, true)])
            .get_ingest_setup(42, None, "https://temps.example.com")
            .await
            .expect("setup resolves");
        for otlp in [IngestSignal::Logs, IngestSignal::Traces] {
            let command = &signal(&setup, otlp).auth.cli_command;
            assert_eq!(
                command,
                "temps apikeys create --name my-app-telemetry --role custom --permissions otel:write --yes"
            );
            assert!(!command.contains("--role user"), "{otlp:?}: {command}");
            // The console registers the form under /settings.
            assert_eq!(
                signal(&setup, otlp).auth.credential_path,
                "/settings/keys/new?name=my-app-telemetry"
            );
        }
    }

    #[tokio::test]
    async fn credentials_of_deleted_environments_do_not_count() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![row(true, true, true)]])
                .into_connection(),
        );
        IngestSetupService::new(db.clone())
            .get_ingest_setup(42, None, "https://temps.example.com")
            .await
            .expect("setup resolves");
        let log = Arc::try_unwrap(db)
            .expect("the service is dropped")
            .into_transaction_log();
        let sql = format!("{log:?}");
        // The event, session-replay and Sentry routes reject a key or DSN
        // whose environment is soft-deleted; setup must agree.
        assert!(
            sql.contains("WHERE ke.id = k.environment_id AND ke.deleted_at IS NULL"),
            "analytics keys must skip deleted environments: {sql}"
        );
        assert!(
            sql.contains("WHERE de.id = d.environment_id AND de.deleted_at IS NULL"),
            "DSNs must skip deleted environments: {sql}"
        );
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

    /// Against real rows: a key or DSN bound to a soft-deleted environment is
    /// rejected by the ingest routes, so setup must not report it configured;
    /// live-environment and project-wide credentials still count.
    #[tokio::test]
    async fn real_rows_skip_credentials_of_deleted_environments() {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{
            analytics_ingest_keys, environments, project_dsns, projects,
            upstream_config::UpstreamList,
        };

        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                println!("Docker not available, skipping");
                return;
            }
            Err(error) => panic!("failed to create migrated test database: {error}"),
        };
        let db = test_db.db.clone();
        let now = chrono::Utc::now();

        let project = projects::ActiveModel {
            name: Set("My app".to_string()),
            slug: Set("my-app".to_string()),
            repo_name: Set("my-app".to_string()),
            repo_owner: Set("example".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_presets::PresetType::Static),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("project");
        let environment = |slug: &str, deleted: bool| environments::ActiveModel {
            project_id: Set(project.id),
            name: Set(slug.to_string()),
            slug: Set(slug.to_string()),
            subdomain: Set(format!("my-app-{slug}")),
            host: Set(format!("my-app-{slug}.apps.example.com")),
            upstreams: Set(UpstreamList::default()),
            deleted_at: Set(deleted.then_some(now)),
            ..Default::default()
        };
        let deleted = environment("preview", true)
            .insert(db.as_ref())
            .await
            .expect("deleted environment");
        let live = environment("production", false)
            .insert(db.as_ref())
            .await
            .expect("live environment");

        let analytics_key =
            |environment_id: Option<i32>, name: &str| analytics_ingest_keys::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment_id),
                name: Set(name.to_string()),
                public_key: Set(format!("pa_{name}")),
                is_active: Set(true),
                event_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
        let dsn = |environment_id: Option<i32>, name: &str| project_dsns::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment_id),
            name: Set(name.to_string()),
            public_key: Set(format!("pk_{name}")),
            secret_key: Set(format!("sk_{name}")),
            is_active: Set(true),
            event_count: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let configured = |setup: &ProjectIngestSetup| {
            (
                signal(setup, IngestSignal::Analytics).configured,
                signal(setup, IngestSignal::Errors).configured,
            )
        };
        let service = IngestSetupService::new(db.clone());
        let base = "https://temps.example.com";

        // Only credentials of the deleted environment: nothing usable.
        analytics_key(Some(deleted.id), "deleted-env-key")
            .insert(db.as_ref())
            .await
            .expect("key of deleted environment");
        dsn(Some(deleted.id), "deleted-env-dsn")
            .insert(db.as_ref())
            .await
            .expect("DSN of deleted environment");
        let setup = service
            .get_ingest_setup(project.id, None, base)
            .await
            .expect("setup resolves");
        assert_eq!(configured(&setup), (false, false));

        // A live-environment key and a project-wide DSN do count.
        analytics_key(Some(live.id), "live-env-key")
            .insert(db.as_ref())
            .await
            .expect("key of live environment");
        dsn(None, "project-dsn")
            .insert(db.as_ref())
            .await
            .expect("project-wide DSN");
        let setup = service
            .get_ingest_setup(project.id, None, base)
            .await
            .expect("setup resolves");
        assert_eq!(configured(&setup), (true, true));
        let setup = service
            .get_ingest_setup(project.id, Some(live.id), base)
            .await
            .expect("live environment resolves");
        assert_eq!(configured(&setup), (true, true));

        // The deleted environment itself is not found.
        assert!(matches!(
            service
                .get_ingest_setup(project.id, Some(deleted.id), base)
                .await,
            Err(IngestSetupError::EnvironmentNotFound { .. })
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
            // A query or fragment would swallow the appended ingest paths.
            "https://temps.example.com?mode=console",
            "https://temps.example.com/#console",
            // Not a valid authority.
            "https://temps.example.com:65536",
            "https://temps.example.com:port",
            // Credentials would end up in every copied URL.
            "https://user:pass@temps.example.com",
            "https://token@temps.example.com/",
        ] {
            assert!(
                matches!(
                    normalize_base_url(1, bad),
                    Err(IngestSetupError::InvalidPublicUrl { .. })
                ),
                "{bad:?} must be rejected"
            );
        }
        // Rejected URLs never repeat credentials, even with a query too.
        for with_credentials in [
            "https://user:pass@temps.example.com",
            "https://user:pass@temps.example.com?mode=console",
        ] {
            let err = normalize_base_url(1, with_credentials).expect_err("rejected");
            let message = err.to_string();
            assert!(!message.contains("pass@"), "{message}");
            assert!(!message.contains("user:"), "{message}");
            assert!(
                message.contains("https://***@temps.example.com"),
                "{message}"
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
