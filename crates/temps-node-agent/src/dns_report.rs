// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What this node's DNS resolver saw, for the control plane (ADR-095 §2).
//!
//! - `GET /agent/dns/report?after=<seq>&instance=<id>` serves the resolver's
//!   state, its cumulative counters and the logged queries newer than the
//!   caller's cursor. The control plane pulls it on its scrape interval and
//!   stamps everything with its own receive time; entries carry only their
//!   age.
//! - `POST /agent/dns/resolve` answers a test resolution from this node's
//!   resolver, optionally as if a given container address had asked, so its
//!   network policy applies.
//!
//! Both are behind node-token authentication. The observer is created once
//! per agent process and handed to every resolver the sync loop starts, so a
//! resolver restart keeps the counters and the log.

use std::net::IpAddr;
use std::sync::{Arc, OnceLock, RwLock};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use temps_authz::permission_guard;
use temps_core::problemdetails::{self, Problem};
use temps_node_dns::observe::{DnsObserver, DnsReport, DnsResolverState};
use temps_node_dns::resolve::{parse_test_query_name, parse_test_query_type, ResolveAnswer};
use temps_node_dns::ResolverView;
use utoipa::OpenApi;

use crate::auth::{require_agent_auth, AgentAuth, RequireAgentAuth};

/// Path of the DNS report endpoint.
pub const DNS_REPORT_PATH: &str = "/agent/dns/report";
/// Path of the test-resolution endpoint.
pub const DNS_RESOLVE_PATH: &str = "/agent/dns/resolve";

/// Version of the binary running the resolver, as the report states it.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The running resolver as the sync loop last saw it.
struct Published {
    tasks_alive: bool,
    /// Why the resolver is not usable even though it runs (it did not
    /// answer its readiness probe), or why it failed to start.
    error: Option<String>,
    view: Option<ResolverView>,
}

/// This node's resolver, shared between the network-sync loop (which
/// starts and stops it) and the DNS report endpoints.
#[derive(Default)]
pub struct AgentDns {
    observer: OnceLock<Arc<DnsObserver>>,
    current: RwLock<Option<Published>>,
}

pub type SharedAgentDns = Arc<AgentDns>;

impl AgentDns {
    /// The process's observer, created on first use. Must be called within
    /// a Tokio runtime.
    pub fn observer(&self) -> Arc<DnsObserver> {
        self.observer.get_or_init(DnsObserver::spawn).clone()
    }

    /// The resolver runs; `error` says why it is not answering, if it is
    /// not.
    pub fn publish_running(&self, view: ResolverView, tasks_alive: bool, error: Option<String>) {
        self.store(Some(Published {
            tasks_alive,
            error,
            view: Some(view),
        }));
    }

    /// No resolver runs: cluster DNS is off (`error` is `None`) or it failed
    /// to start.
    pub fn publish_stopped(&self, error: Option<String>) {
        self.store(error.map(|error| Published {
            tasks_alive: false,
            error: Some(error),
            view: None,
        }));
    }

    fn store(&self, published: Option<Published>) {
        match self.current.write() {
            Ok(mut slot) => *slot = published,
            Err(poisoned) => *poisoned.into_inner() = published,
        }
    }

    fn view(&self) -> Option<ResolverView> {
        let slot = self.current.read().unwrap_or_else(|p| p.into_inner());
        slot.as_ref().and_then(|p| p.view.clone())
    }

    fn state(&self) -> DnsResolverState {
        let slot = self.current.read().unwrap_or_else(|p| p.into_inner());
        match slot.as_ref() {
            Some(Published {
                tasks_alive,
                error,
                view: Some(view),
            }) => {
                let mut state = view.state(*tasks_alive);
                if let Some(error) = error {
                    state.last_error = Some(error.clone());
                }
                state
            }
            Some(Published { error, .. }) => DnsResolverState {
                last_error: error.clone(),
                ..DnsResolverState::default()
            },
            None => DnsResolverState::default(),
        }
    }

    /// The report for a cursor (`after` from instance `instance`).
    pub fn report(&self, after: Option<u64>, instance: Option<&str>) -> DnsReport {
        let observation = self.observer().snapshot(after, instance);
        DnsReport::assemble(observation, BUILD_VERSION, self.state())
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ReportQuery {
    /// Return only entries with a larger sequence number.
    after: Option<u64>,
    /// The instance id the cursor belongs to; another id reads everything.
    instance: Option<String>,
}

/// A test resolution request.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ResolveRequest {
    /// The name to resolve, e.g. `production.shop.temps.local`.
    pub name: String,
    /// `A`, `AAAA`, `CNAME`, `SRV` or `TXT`. Defaults to `A`.
    pub record_type: Option<String>,
    /// Answer as if this address had asked (a container on this node), so
    /// its network policy applies. Defaults to the resolver's own address.
    #[schema(value_type = Option<String>, example = "10.88.0.12")]
    pub source_ip: Option<IpAddr>,
}

/// `GET /agent/dns/report` and `POST /agent/dns/resolve`, behind node-token
/// authentication.
pub fn router(dns: SharedAgentDns, auth: Arc<AgentAuth>) -> Router {
    Router::new()
        .route(DNS_REPORT_PATH, get(report))
        .route(DNS_RESOLVE_PATH, post(resolve))
        .layer(middleware::from_fn(require_agent_auth))
        .layer(Extension(auth))
        .with_state(dns)
}

/// This node's resolver state, counters and recent queries (ADR-095).
#[utoipa::path(
    get,
    path = "/agent/dns/report",
    tag = "DNS",
    params(ReportQuery),
    responses(
        (status = 200, description = "DNS report (ADR-095)"),
        (status = 401, description = "Node authentication required")
    ),
    security(("bearer_auth" = []))
)]
async fn report(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(dns): State<SharedAgentDns>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<DnsReport>, Problem> {
    permission_guard!(auth, DeploymentsRead);
    Ok(Json(dns.report(query.after, query.instance.as_deref())))
}

/// Resolve a name from this node's resolver (ADR-095 §5).
#[utoipa::path(
    post,
    path = "/agent/dns/resolve",
    tag = "DNS",
    request_body = ResolveRequest,
    responses(
        (status = 200, description = "The answer, its response code and latency"),
        (status = 400, description = "Invalid name or record type"),
        (status = 401, description = "Node authentication required"),
        (status = 409, description = "The resolver is not running on this node"),
        (status = 502, description = "The resolver did not answer")
    ),
    security(("bearer_auth" = []))
)]
async fn resolve(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(dns): State<SharedAgentDns>,
    Json(request): Json<ResolveRequest>,
) -> Result<Json<ResolveAnswer>, Problem> {
    permission_guard!(auth, DeploymentsRead);
    let bad_request = |detail: String| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid test resolution")
            .with_detail(detail)
    };
    let name = parse_test_query_name(&request.name).map_err(|e| bad_request(e.to_string()))?;
    let qtype = parse_test_query_type(request.record_type.as_deref().unwrap_or("A"))
        .map_err(|e| bad_request(e.to_string()))?;
    let Some(view) = dns.view() else {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("DNS resolver not running")
            .with_detail(
                "This node's DNS resolver is not running: cluster DNS is off, or the resolver \
                 failed to start (see the node's DNS status).",
            ));
    };
    let source = request
        .source_ip
        .or_else(|| view.listen_addr().map(|addr| addr.ip()))
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let answer = view.resolve_as(source, name, qtype).await.map_err(|e| {
        problemdetails::new(StatusCode::BAD_GATEWAY)
            .with_title("Test resolution failed")
            .with_detail(e.to_string())
    })?;
    Ok(Json(answer))
}

#[derive(OpenApi)]
#[openapi(paths(report, resolve), components(schemas(ResolveRequest)))]
pub struct DnsReportApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::response::Response;
    use std::path::PathBuf;
    use std::time::Duration;
    use temps_node_dns::observe::{QueryObservation, QueryOutcome, ZoneClass};
    use temps_node_dns::{ResolverConfig, ResolverHandle, ZoneRecord};
    use tower::ServiceExt;

    const TOKEN: &str = "fixture-node-token";

    fn app(dns: SharedAgentDns) -> Router {
        router(dns, Arc::new(AgentAuth::new(TOKEN)))
    }

    async fn call(
        router: &Router,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> Response {
        let mut request = axum::http::Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if body.is_some() {
            request = request.header("Content-Type", "application/json");
        }
        router
            .clone()
            .oneshot(
                request
                    .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    async fn json<T: serde::de::DeserializeOwned>(response: Response) -> T {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn report_requires_the_node_token() {
        let router = app(Arc::new(AgentDns::default()));
        let response = call(&router, "GET", DNS_REPORT_PATH, None, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = call(&router, "GET", DNS_REPORT_PATH, Some("wrong"), None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = call(
            &router,
            "POST",
            DNS_RESOLVE_PATH,
            None,
            Some(r#"{"name":"a.temps.local"}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn report_without_a_resolver_says_it_is_not_running() {
        let dns = Arc::new(AgentDns::default());
        dns.publish_stopped(Some("port 53 already bound".into()));
        let router = app(dns);
        let response = call(&router, "GET", DNS_REPORT_PATH, Some(TOKEN), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let report: DnsReport = json(response).await;
        assert_eq!(report.version, temps_node_dns::observe::DNS_REPORT_VERSION);
        assert!(!report.resolver.running);
        assert_eq!(
            report.resolver.last_error.as_deref(),
            Some("port 53 already bound")
        );
        assert_eq!(report.build_version, BUILD_VERSION);
        assert!(!report.instance_id.is_empty());
    }

    #[tokio::test]
    async fn resolve_without_a_resolver_is_a_conflict() {
        let router = app(Arc::new(AgentDns::default()));
        let response = call(
            &router,
            "POST",
            DNS_RESOLVE_PATH,
            Some(TOKEN),
            Some(r#"{"name":"a.temps.local"}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn resolve_rejects_bad_input() {
        let router = app(Arc::new(AgentDns::default()));
        let response = call(
            &router,
            "POST",
            DNS_RESOLVE_PATH,
            Some(TOKEN),
            Some(r#"{"name":"a.temps.local","record_type":"AXFR"}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = call(
            &router,
            "POST",
            DNS_RESOLVE_PATH,
            Some(TOKEN),
            Some(r#"{"name":""}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    fn record(fqdn: &str, ip: &str) -> ZoneRecord {
        ZoneRecord {
            id: 1,
            fqdn: fqdn.into(),
            record_type: "A".into(),
            target_ip: Some(ip.into()),
            target_port: None,
            ttl: 5,
            owner_kind: "environment".into(),
            owner_id: 1,
            node_id: None,
            generation: 1,
        }
    }

    /// A real resolver behind the endpoints: the report carries its state
    /// and the queries it was asked, and a test resolution answers from its
    /// zone without being logged.
    #[tokio::test]
    async fn report_and_resolve_against_a_running_resolver() {
        let dir = std::env::temp_dir().join(format!(
            "temps-agent-dns-report-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let mut config = ResolverConfig::new_local_feed(
            0,
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            PathBuf::from(&dir),
        );
        config.listen_addrs = vec!["127.0.0.1:0".parse().expect("addr")];
        config.upstream_resolvers.clear();
        let dns = Arc::new(AgentDns::default());
        let handle = ResolverHandle::start_observed(config, None, Some(dns.observer()))
            .await
            .expect("resolver starts");
        handle.zone.replace(
            3,
            vec![record("production.shop.temps.local", "10.237.1.10")],
        );
        dns.publish_running(handle.view(), true, None);
        dns.observer().record(QueryObservation {
            name: "missing.shop.temps.local.",
            qtype: temps_node_dns::observe::RecordType::A,
            zone: ZoneClass::Internal,
            outcome: QueryOutcome::Nxdomain,
            latency: Duration::from_micros(200),
            answers: 0,
            source: "10.237.1.20".parse().expect("ip"),
        });
        // The observer's own task moves a logged query into the ring; wait
        // for it rather than for a fixed time a loaded runner can outlast.
        tokio::time::timeout(Duration::from_secs(10), async {
            while dns.observer().snapshot(None, None).queries.is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the observer logged the query within 10 s");

        let router = app(dns.clone());
        let response = call(
            &router,
            "POST",
            DNS_RESOLVE_PATH,
            Some(TOKEN),
            Some(r#"{"name":"production.shop.temps.local","record_type":"A","source_ip":"10.237.1.20"}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let answer: ResolveAnswer = json(response).await;
        assert_eq!(answer.rcode, "NOERROR");
        assert_eq!(answer.outcome, QueryOutcome::Noerror);
        assert!(answer.authoritative);
        assert_eq!(answer.answers.len(), 1);
        assert_eq!(answer.answers[0].data, "10.237.1.10");

        let response = call(
            &router,
            "POST",
            DNS_RESOLVE_PATH,
            Some(TOKEN),
            Some(r#"{"name":"nothing.temps.local"}"#),
        )
        .await;
        let answer: ResolveAnswer = json(response).await;
        assert_eq!(answer.rcode, "NXDOMAIN");

        let response = call(&router, "GET", DNS_REPORT_PATH, Some(TOKEN), None).await;
        let report: DnsReport = json(response).await;
        assert!(report.resolver.running);
        assert!(report.resolver.tasks_alive);
        assert_eq!(report.resolver.zone_generation, 3);
        assert_eq!(report.resolver.record_count, 1);
        assert_eq!(report.resolver.last_sync_age_secs, None, "local feed");
        assert_eq!(
            report.queries.len(),
            1,
            "test resolutions are not logged: {:?}",
            report.queries
        );
        assert_eq!(
            report.queries[0].name.as_deref(),
            Some("missing.shop.temps.local")
        );
        assert_eq!(report.counters.internal.nxdomain, 1);

        let uri = format!(
            "{DNS_REPORT_PATH}?after={}&instance={}",
            report.queries[0].seq, report.instance_id
        );
        let response = call(&router, "GET", &uri, Some(TOKEN), None).await;
        let newer: DnsReport = json(response).await;
        assert!(newer.queries.is_empty(), "the cursor skips what was read");

        handle.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
