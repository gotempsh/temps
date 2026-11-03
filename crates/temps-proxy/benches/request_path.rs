// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What one request costs in the proxy's request path: latency and heap
//! allocations of `request_filter`, per way a request can leave it, and of
//! the three hooks together for an admitted request.
//!
//! `cargo bench -p temps-proxy --bench request_path [-- <name filter>]`
//!
//! The proxy runs for real over an in-memory HTTP/1.1 session, with the real
//! route table and resolvers. The database is a mock that only ever serves
//! the settings row the 5-second settings cache refreshes from, so no scenario
//! does I/O beyond writing its answer to the in-memory client, and no Docker
//! is needed.
//!
//! Only the hook is timed: building the session and parsing the request
//! happen outside the measurement, which is what makes a change in the hook
//! visible. Allocation counts are per request on this thread and, unlike the
//! timings, do not depend on the machine.
//!
//! The harness is hand-rolled because it needs per-iteration setup outside
//! the measurement and the allocation count next to the timing. Without
//! `--bench` (`cargo test --benches`) it runs a few iterations as a smoke
//! test, and it answers nextest's `--list` with no tests.

// Bench target: clippy's `allow-*-in-tests` only covers test functions and
// `cfg(test)` modules, and a panic here is the intended failure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "benchmark fixtures; a panic fails the run"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::IpAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;

use pingora_proxy::{ProxyHttp, Session};
use sea_orm::{DatabaseBackend, MockDatabase};
use temps_core::ProjectIpGate;
use temps_entities::deployment_config::{
    DeploymentConfig, PasswordProtectionConfig, RateLimitConfig, SecurityConfig,
};
use temps_entities::{deployments, environments, projects, settings};
use temps_proxy::config::ProxyConfig;
use temps_proxy::proxy::{LoadBalancer, ProxyContext};
use temps_proxy::services::{ProjectContextResolverImpl, UpstreamResolverImpl};
use temps_proxy::traits::{ProjectContextResolver, UpstreamResolver};
use temps_routes::{BackendEntry, BackendType, CachedPeerTable, RouteInfo};
use tokio::io::AsyncWriteExt;

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    static ALLOCATED_BYTES: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn count(size: usize) {
    // `try_with`: the thread-local may be gone while a thread exits.
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            let _ = ALLOCATED_BYTES.try_with(|n| n.set(n.get() + size as u64));
        }
    });
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// One timed run of the hook.
#[derive(Clone, Copy, Default)]
struct Sample {
    nanos: u64,
    allocations: u64,
    bytes: u64,
}

/// Runs `body` with the clock and the allocation counters on.
async fn measure<T>(body: impl std::future::Future<Output = T>) -> (T, Sample) {
    ALLOCATIONS.with(|n| n.set(0));
    ALLOCATED_BYTES.with(|n| n.set(0));
    COUNTING.with(|on| on.set(true));
    let started = Instant::now();
    let value = body.await;
    let nanos = started.elapsed().as_nanos() as u64;
    COUNTING.with(|on| on.set(false));
    let sample = Sample {
        nanos,
        allocations: ALLOCATIONS.with(Cell::get),
        bytes: ALLOCATED_BYTES.with(Cell::get),
    };
    (value, sample)
}

const CLIENT_IP: &str = "203.0.113.10";

const PROJECT_OPEN: i32 = 1;
const PROJECT_IP_DENIED: i32 = 2;
const PROJECT_RATE_LIMITED: i32 = 3;
const PROJECT_PASSWORD: i32 = 4;
const PROJECT_FORCE_HTTPS: i32 = 5;

const HOST_OPEN: &str = "app.example.test";
const HOST_IP_DENIED: &str = "restricted.example.test";
const HOST_RATE_LIMITED: &str = "limited.example.test";
const HOST_PASSWORD: &str = "walled.example.test";
const HOST_FORCE_HTTPS: &str = "secure.example.test";
const HOST_REDIRECT: &str = "old.example.test";
const HOST_CONSOLE: &str = "console.example.test";

/// Denies one project's clients, the way an allow list that does not cover
/// them does.
struct DenyProject(i32);

impl ProjectIpGate for DenyProject {
    fn is_allowed(&self, project_id: i32, _environment_id: i32, _ip: IpAddr) -> bool {
        project_id != self.0
    }
}

fn project(id: i32) -> projects::Model {
    let now = chrono::Utc::now();
    projects::Model {
        id,
        name: format!("project-{id}"),
        repo_name: format!("project-{id}"),
        repo_owner: "bench".to_string(),
        directory: String::new(),
        pull_only_root_directory: false,
        main_branch: "main".to_string(),
        preset: temps_entities::preset::Preset::NextJs,
        preset_config: None,
        deployment_config: None,
        created_at: now,
        updated_at: now,
        slug: format!("project-{id}"),
        is_deleted: false,
        deleted_at: None,
        last_deployment: None,
        is_public_repo: false,
        git_url: None,
        git_provider_connection_id: None,
        attack_mode: false,
        shield_mode: "off".to_string(),
        ai_alert_summaries_enabled: None,
        ai_debug_chat_enabled: None,
        ai_write_actions_enabled: false,
        error_source_context_enabled: false,
        vulnerability_scanning_enabled: false,
        error_source_root: None,
        enable_preview_environments: false,
        preview_envs_on_demand: false,
        preview_envs_idle_timeout_seconds: 300,
        preview_envs_wake_timeout_seconds: 30,
        source_type: Default::default(),
        project_type: temps_entities::types::ProjectType::Server,
        allow_alternate_sources: None,
        hosting: Default::default(),
        template_slug: None,
        service_template: None,
        github_webhook_id: None,
        github_webhook_signing_token: None,
        serves_own_temps_api: false,
        gitlab_webhook_id: None,
        gitlab_webhook_signing_token: None,
        gitea_webhook_signing_token: None,
        bitbucket_webhook_token: None,
        bitbucket_webhook_hook_id: None,
        generic_webhook_token: None,
        cross_project_trace_sharing: false,
        ai_api_traffic_summary_enabled: None,
        image_retention_hours: None,
        cloud_telemetry_fidelity: Default::default(),
        cloud_telemetry_attribute_allowlist: Vec::new(),
        cloud_telemetry_write_mode: Default::default(),
        cloud_analytics_write_mode: Default::default(),
        instantiation_token: None,
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
    }
}

fn environment(id: i32, project_id: i32) -> environments::Model {
    let now = chrono::Utc::now();
    environments::Model {
        id,
        name: "production".into(),
        slug: "production".into(),
        subdomain: format!("app-{id}"),
        last_deployment: None,
        host: String::new(),
        upstreams: Default::default(),
        created_at: now,
        updated_at: now,
        project_id,
        current_deployment_id: None,
        branch: None,
        deleted_at: None,
        deployment_config: None,
        is_preview: false,
        protected: false,
        sleeping: false,
        attack_mode: None,
        shield_mode: None,
        shield_exempt_paths: Default::default(),
        // Every scenario is plain HTTP; only the redirect one wants HTTPS.
        force_https: Some(false),
        last_activity_at: None,
        ingress_mode: Default::default(),
        workload_mode: Default::default(),
    }
}

fn deployment(id: i32, project_id: i32, environment_id: i32) -> deployments::Model {
    let now = chrono::Utc::now();
    deployments::Model {
        id,
        project_id,
        environment_id,
        created_at: now,
        updated_at: now,
        slug: format!("d{id}"),
        state: "deployed".to_string(),
        metadata: None,
        deploying_at: None,
        ready_at: None,
        started_at: None,
        finished_at: None,
        context_vars: None,
        branch_ref: None,
        tag_ref: None,
        commit_sha: None,
        commit_message: None,
        commit_author: None,
        commit_json: None,
        cancelled_reason: None,
        static_dir_location: None,
        screenshot_location: None,
        image_name: None,
        deployment_config: None,
        promoted_from_deployment_id: None,
        upload_request_id: None,
        docker_socket_mounted: false,
    }
}

fn app_route(project_id: i32, configure: impl FnOnce(&mut environments::Model)) -> RouteInfo {
    let mut env = environment(project_id, project_id);
    configure(&mut env);
    RouteInfo {
        backend: BackendType::Upstream {
            backends: vec![BackendEntry {
                address: "127.0.0.1:9".to_string(),
                container_id: None,
                container_name: None,
            }],
            round_robin_counter: Arc::new(AtomicUsize::new(0)),
        },
        redirect_to: None,
        status_code: None,
        project: Some(Arc::new(project(project_id))),
        environment: Some(Arc::new(env)),
        deployment: Some(Arc::new(deployment(project_id, project_id, project_id))),
        cert_eligible: false,
    }
}

fn security(config: SecurityConfig) -> Option<DeploymentConfig> {
    Some(DeploymentConfig {
        security: Some(config),
        ..Default::default()
    })
}

/// The proxy under test, with one host per scenario in its route table.
fn load_balancer() -> LoadBalancer {
    let now = chrono::Utc::now();
    let settings_row = settings::Model {
        id: 1,
        data: serde_json::to_value(temps_config::AppSettings::default()).unwrap(),
        created_at: now,
        updated_at: now,
    };
    // One row per settings refresh, every 5 seconds: far more than a run needs.
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row]; 4096])
            .into_connection(),
    );
    let config = temps_config::ServerConfig::new(
        "127.0.0.1:3000".to_string(),
        "postgresql://bench@localhost/bench".to_string(),
        None,
        None,
    )
    .unwrap();
    let config_service = Arc::new(temps_config::ConfigService::new(
        Arc::new(config),
        db.clone(),
    ));

    let geoip = Arc::new(temps_geo::GeoIpService::Mock(
        temps_geo::MockGeoIpService::new(),
    ));
    let ip_service = Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip));
    let log_storage: Arc<dyn temps_proxy::storage::ProxyLogStorage> = Arc::new(
        temps_proxy::storage::TimescaleDbProxyLogStore::new(db.clone(), ip_service.clone()),
    );
    let (proxy_log_handle, tracking_handle, _writer) =
        temps_proxy::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
            db.clone(),
            ip_service,
            log_storage,
        );

    let route_table = Arc::new(CachedPeerTable::new(db.clone()));
    route_table.mark_loaded_for_test();
    route_table.insert_route_for_test(HOST_OPEN, app_route(PROJECT_OPEN, |_| {}));
    route_table.insert_route_for_test(HOST_IP_DENIED, app_route(PROJECT_IP_DENIED, |_| {}));
    route_table.insert_route_for_test(
        HOST_RATE_LIMITED,
        app_route(PROJECT_RATE_LIMITED, |env| {
            env.deployment_config = security(SecurityConfig {
                enabled: Some(true),
                rate_limiting: Some(RateLimitConfig {
                    max_requests_per_minute: Some(1),
                    max_requests_per_hour: Some(1),
                    whitelist_ips: Vec::new(),
                    blacklist_ips: Vec::new(),
                }),
                ..Default::default()
            });
        }),
    );
    route_table.insert_route_for_test(
        HOST_PASSWORD,
        app_route(PROJECT_PASSWORD, |env| {
            env.deployment_config = security(SecurityConfig {
                password_protection: Some(PasswordProtectionConfig {
                    enabled: true,
                    password_hash: "not-a-real-hash".to_string(),
                }),
                ..Default::default()
            });
        }),
    );
    route_table.insert_route_for_test(
        HOST_FORCE_HTTPS,
        app_route(PROJECT_FORCE_HTTPS, |env| env.force_https = Some(true)),
    );
    route_table.insert_route_for_test(
        HOST_REDIRECT,
        RouteInfo {
            backend: BackendType::Upstream {
                backends: Vec::new(),
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: Some("https://new.example.test".to_string()),
            status_code: Some(301),
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        },
    );

    let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
        Arc::new(ProxyConfig::default()),
        Arc::new(temps_proxy::service::lb_service::LbService::new(db.clone())),
        route_table.clone(),
    )) as Arc<dyn UpstreamResolver>;
    let project_context_resolver =
        Arc::new(ProjectContextResolverImpl::new(route_table)) as Arc<dyn ProjectContextResolver>;

    LoadBalancer::new(
        upstream_resolver,
        proxy_log_handle,
        tracking_handle,
        project_context_resolver,
        Arc::new(temps_core::CookieCrypto::new("default-32-byte-key-for-testing!").unwrap()),
        db.clone(),
        config_service,
        Arc::new(
            temps_proxy::service::ip_access_control_service::IpAccessControlService::new(
                db.clone(),
            ),
        ),
        Arc::new(DenyProject(PROJECT_IP_DENIED)),
        Arc::new(temps_shield::ChallengeGate::new(
            temps_shield::ShieldKeys::derive(|_| [7u8; 32]),
        )),
        Arc::new(temps_proxy::service::cert_host_cache::CertHostCache::new(
            db,
        )),
        false,
    )
}

/// What a browser sends: enough headers and cookies that the header capture
/// and the cookie lookups do their usual work.
fn browser_request(host: &str) -> String {
    format!(
        "GET /dashboard/settings?tab=profile HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0\r\n\
         Accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8\r\n\
         Accept-Language: en-US,en;q=0.5\r\n\
         Accept-Encoding: gzip, deflate, br\r\n\
         Referer: https://{host}/dashboard\r\n\
         Cookie: theme=dark; _temps_visitor_id=opaque-visitor-value; _temps_sid=opaque-session-value\r\n\
         Sec-Fetch-Dest: document\r\n\
         Sec-Fetch-Mode: navigate\r\n\
         Sec-Fetch-Site: same-origin\r\n\
         Upgrade-Insecure-Requests: 1\r\n\
         \r\n"
    )
}

struct Scenario {
    name: &'static str,
    host: &'static str,
    /// The `ctx.routing_status` that proves the request left where intended.
    routing_status: &'static str,
    /// `request_filter` answers it; otherwise it goes on to `upstream_peer`.
    answered: bool,
    /// Time the three hooks together instead of `request_filter` alone.
    all_hooks: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "app_forwarded",
        host: HOST_OPEN,
        routing_status: "routed",
        answered: false,
        all_hooks: false,
    },
    Scenario {
        name: "console_fall_through",
        host: HOST_CONSOLE,
        routing_status: "no_project",
        answered: false,
        all_hooks: false,
    },
    Scenario {
        name: "ip_restricted_403",
        host: HOST_IP_DENIED,
        routing_status: "project_ip_restricted",
        answered: true,
        all_hooks: false,
    },
    Scenario {
        name: "rate_limited_429",
        host: HOST_RATE_LIMITED,
        routing_status: "rate_limited",
        answered: true,
        all_hooks: false,
    },
    Scenario {
        name: "password_wall_form",
        host: HOST_PASSWORD,
        routing_status: "password_wall",
        answered: true,
        all_hooks: false,
    },
    Scenario {
        name: "https_redirect_301",
        host: HOST_FORCE_HTTPS,
        routing_status: "http_to_https_redirect",
        answered: true,
        all_hooks: false,
    },
    Scenario {
        name: "domain_redirect_301",
        host: HOST_REDIRECT,
        routing_status: "redirected",
        answered: true,
        all_hooks: false,
    },
    Scenario {
        name: "all_hooks_app_forwarded",
        host: HOST_OPEN,
        routing_status: "routed",
        answered: false,
        all_hooks: true,
    },
];

/// What one trip through the hook did, and what it cost.
async fn drive(lb: &LoadBalancer, scenario: &Scenario, request: &[u8]) -> (bool, String, Sample) {
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    client.write_all(request).await.unwrap();
    let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
    assert!(session.read_request().await.unwrap(), "request must parse");
    let mut ctx: ProxyContext = lb.new_ctx();

    let (answered, sample) = if scenario.all_hooks {
        measure(async {
            lb.early_request_filter(&mut session, &mut ctx)
                .await
                .unwrap();
            // The in-memory stream has no peer address to resolve.
            ctx.ip_address = Some(CLIENT_IP.to_string());
            let answered = lb.request_filter(&mut session, &mut ctx).await.unwrap();
            std::hint::black_box(lb.upstream_peer(&mut session, &mut ctx).await.unwrap());
            answered
        })
        .await
    } else {
        ctx.ip_address = Some(CLIENT_IP.to_string());
        let (answered, sample) = measure(lb.request_filter(&mut session, &mut ctx)).await;
        (answered.unwrap(), sample)
    };
    (answered, ctx.routing_status, sample)
}

async fn run_once(lb: &LoadBalancer, scenario: &Scenario, request: &[u8]) -> Sample {
    let (answered, routing_status, sample) = drive(lb, scenario, request).await;
    assert_eq!(
        (answered, routing_status.as_str()),
        (scenario.answered, scenario.routing_status),
        "scenario {} left the filter somewhere else",
        scenario.name
    );
    sample
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    sorted[(sorted.len() - 1) * percent / 100]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // nextest lists the tests of every target it builds; this one has none.
    if args.iter().any(|arg| arg == "--list") {
        return;
    }
    let full = args.iter().any(|arg| arg == "--bench");
    let filter = args.iter().find(|arg| !arg.starts_with('-')).cloned();
    let (rounds, iterations) = if full { (9, 3_000) } else { (1, 5) };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let lb = load_balancer();
        let scenarios: Vec<&Scenario> = SCENARIOS
            .iter()
            .filter(|scenario| filter.as_deref().is_none_or(|f| scenario.name.contains(f)))
            .collect();
        let requests: Vec<Vec<u8>> = scenarios
            .iter()
            .map(|scenario| browser_request(scenario.host).into_bytes())
            .collect();

        // Warm the settings cache and every lazily built table. The first
        // request spends the rate-limited scenario's whole budget.
        for (scenario, request) in scenarios.iter().zip(&requests) {
            drive(&lb, scenario, request).await;
            for _ in 0..iterations.min(50) {
                run_once(&lb, scenario, request).await;
            }
        }

        // Rounds, not one long run per scenario: slow drift on the machine
        // then lands on every scenario alike.
        let mut samples: Vec<Vec<Sample>> = vec![Vec::new(); scenarios.len()];
        for _ in 0..rounds {
            for ((scenario, request), samples) in scenarios.iter().zip(&requests).zip(&mut samples)
            {
                for _ in 0..iterations {
                    samples.push(run_once(&lb, scenario, request).await);
                }
            }
        }

        println!(
            "{:<26}{:>10}{:>10}{:>10}{:>10}{:>12}",
            "scenario", "median ns", "p90 ns", "p99 ns", "allocs", "bytes"
        );
        for (scenario, samples) in scenarios.iter().zip(&samples) {
            let sorted = |value: fn(&Sample) -> u64| {
                let mut values: Vec<u64> = samples.iter().map(value).collect();
                values.sort_unstable();
                values
            };
            let nanos = sorted(|s| s.nanos);
            println!(
                "{:<26}{:>10}{:>10}{:>10}{:>10}{:>12}",
                scenario.name,
                percentile(&nanos, 50),
                percentile(&nanos, 90),
                percentile(&nanos, 99),
                percentile(&sorted(|s| s.allocations), 50),
                percentile(&sorted(|s| s.bytes), 50),
            );
        }
    });
}
