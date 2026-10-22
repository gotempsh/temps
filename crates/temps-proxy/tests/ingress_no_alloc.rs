// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The proxy's compiled-ingress request path allocates nothing (ADR-055
//! §11, Scalability). Measured with a counting allocator, per request:
//!
//! - one route-snapshot endpoint lookup for the environment;
//! - normalising the host for grant checks;
//! - an enforcing evaluation of every derived policy (IP allow list,
//!   blacklist, live rate limit against the proxy's limiter, captcha with
//!   resume, password wall) plus a stored `TempsLogin` whose session grant
//!   is read from the `Cookie` header and verified;
//! - a shadow evaluation of the same request from recorded facts.
//!
//! One test per binary, so nothing else allocates on the measured thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::net::IpAddr;

use pingora_http::RequestHeader;
use temps_proxy::ingress::{
    enforce_step, grant_host, shadow_step, AccessOutcome, EndpointRequest, GrantFacts, LegacyFacts,
    PasswordFacts, Step, SESSION_COOKIE_NAME,
};
use temps_proxy::rate_limiter::{RateLimitDecision, RateLimiter};
use temps_routes::ingress::{environment_endpoint, EnvironmentAccess, IngressSnapshot};
use temps_workload_ingress::grant::{self, GrantClaims, GrantKey, GrantUse, Revocations};
use temps_workload_ingress::{AccessPolicy, IpRule, ProductKind, ProductRef, UnresolvedClient};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`: the thread-local may be gone while a thread exits.
        let _ = COUNTING.try_with(|on| {
            if on.get() {
                let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const ENVIRONMENT_ID: i32 = 7;
const PROJECT_ID: i32 = 3;
const NOW: u64 = 1_800_000_000;

fn project() -> temps_entities::projects::Model {
    let now = chrono::Utc::now();
    let mut project: temps_entities::projects::Model = serde_json::from_value(serde_json::json!({
        "id": PROJECT_ID, "name": "shop", "slug": "shop", "repo_name": "shop",
        "repo_owner": "example", "directory": ".", "main_branch": "main", "preset": "dockerfile",
        "created_at": now, "updated_at": now, "pull_only_root_directory": false,
        "is_deleted": false, "is_public_repo": false, "attack_mode": true,
        "ai_write_actions_enabled": false, "error_source_context_enabled": false,
        "vulnerability_scanning_enabled": false, "enable_preview_environments": false,
        "preview_envs_on_demand": false, "preview_envs_idle_timeout_seconds": 300,
        "preview_envs_wake_timeout_seconds": 30, "source_type": "docker_image",
        "project_type": temps_entities::types::ProjectType::Server,
        "hosting": temps_entities::types::Hosting::Hosted,
        "cross_project_trace_sharing": true, "cloud_telemetry_fidelity": "metered",
        "cloud_telemetry_attribute_allowlist": [], "cloud_telemetry_write_mode": "local",
        "cloud_analytics_write_mode": "local"
    }))
    .expect("project fixture");
    project.attack_mode = true;
    project
}

fn environment() -> temps_entities::environments::Model {
    serde_json::from_value(serde_json::json!({
        "id": ENVIRONMENT_ID, "name": "production", "slug": "production",
        "subdomain": "app", "last_deployment": null, "host": "app.example.test",
        "upstreams": [], "created_at": "2026-09-28T00:00:00Z",
        "updated_at": "2026-09-28T00:00:00Z", "project_id": PROJECT_ID,
        "current_deployment_id": null, "branch": null, "deleted_at": null,
        "deployment_config": {
            "security": {
                "enabled": true,
                "rateLimiting": {
                    "maxRequestsPerMinute": 1000000,
                    "maxRequestsPerHour": 10000000,
                    "whitelistIps": ["198.51.100.0/24"],
                    "blacklistIps": ["203.0.113.0/24"]
                },
                "passwordProtection": { "enabled": true, "passwordHash": "fixture-hash" }
            }
        },
        "is_preview": false, "protected": false, "sleeping": false, "attack_mode": null,
        "force_https": null, "last_activity_at": null, "ingress_mode": "enforce"
    }))
    .expect("environment fixture")
}

fn ip_rules() -> Vec<temps_entities::project_ip_access_rules::Model> {
    use temps_entities::project_ip_access_rules::{IpAccessCidrEntry, IpAccessCidrs, Model};
    let now = chrono::Utc::now();
    vec![Model {
        id: 1,
        project_id: PROJECT_ID,
        environment_id: None,
        mode: "allow".into(),
        cidrs: IpAccessCidrs(
            ["192.0.2.0/24", "2001:db8::/32"]
                .into_iter()
                .map(|cidr| IpAccessCidrEntry {
                    cidr: cidr.into(),
                    description: None,
                })
                .collect(),
        ),
        updated_by: None,
        revision: 1,
        created_at: now,
        updated_at: now,
    }]
}

#[test]
fn deciding_an_environment_request_through_its_compiled_endpoint_does_not_allocate() {
    let project = project();
    let environment = environment();
    let rules = ip_rules();
    let instance = temps_core::RateLimitSettings::default();
    let access = EnvironmentAccess {
        environment: &environment,
        project: &project,
        instance_rate_limiting: &instance,
        ip_rules: &rules,
    };
    let stored = vec![
        AccessPolicy::IpRules {
            rules: vec![IpRule {
                cidr: "192.0.2.128/25".into(),
                allow: false,
            }],
            unresolved: UnresolvedClient::Deny,
        },
        AccessPolicy::TempsLogin {
            permission: "projects:read".into(),
        },
    ];
    let snapshot = IngressSnapshot::build(
        vec![environment_endpoint(11, &access, None, stored)],
        HashMap::new(),
        Revocations::new([99, 100]),
    );

    let master = [4u8; 32];
    let owner = ProductRef {
        kind: ProductKind::Environment,
        id: i64::from(ENVIRONMENT_ID),
    };
    let token = grant::mint(
        &GrantKey::derive(&master, owner),
        &GrantClaims {
            grant_id: 5,
            usage: GrantUse::Session,
            owner,
            host: "app.example.test",
            permission: "projects:read",
            user_id: 2,
            expires_at: NOW + 3600,
        },
    )
    .expect("mint");
    let mut header = RequestHeader::build("GET", b"/dashboard", None).expect("request");
    header
        .append_header(
            "cookie",
            format!("theme=dark; {SESSION_COOKIE_NAME}={token}; _temps_pw=x"),
        )
        .expect("cookie header");

    let limiter = RateLimiter::new();
    let client: IpAddr = "192.0.2.10".parse().expect("ip");
    let password = Some(PasswordFacts {
        verify_post: false,
        cookie_valid: true,
    });
    let facts = LegacyFacts {
        rate: Some(RateLimitDecision::Allow),
        captcha_completed: Some(true),
    };

    let decide_one = |raw_host: &str| -> (AccessOutcome, AccessOutcome) {
        let Some(endpoint) = snapshot.environment_endpoint(ENVIRONMENT_ID, "/dashboard") else {
            return (AccessOutcome::Unavailable, AccessOutcome::Unavailable);
        };
        let mut buffer = [0u8; 256];
        let request = EndpointRequest {
            request: &header,
            environment_id: ENVIRONMENT_ID,
            client_ip: Some(client),
            captcha_path: false,
            has_tls_fingerprint: true,
            password,
            grants: GrantFacts {
                master: &master,
                revocations: snapshot.revocations(),
                host: grant_host(raw_host, &mut buffer),
                now_secs: NOW,
            },
        };
        let enforced = match enforce_step(endpoint, &request, &limiter, false, 0) {
            Step::Done(outcome) => outcome,
            // The captcha cache said yes: resume after the captcha.
            Step::NeedsCaptcha { resume_at } => {
                match enforce_step(endpoint, &request, &limiter, true, resume_at) {
                    Step::Done(outcome) => outcome,
                    Step::NeedsCaptcha { .. } => AccessOutcome::CaptchaChallenge,
                }
            }
        };
        (enforced, shadow_step(endpoint, &request, &facts))
    };

    // Warm up: the limiter's first sight of the client creates its entry.
    assert_eq!(
        decide_one("App.Example.Test:443"),
        (AccessOutcome::Allow, AccessOutcome::Allow)
    );

    COUNTING.with(|on| on.set(true));
    let mut allowed = 0;
    for _ in 0..10_000 {
        if decide_one("App.Example.Test:443") == (AccessOutcome::Allow, AccessOutcome::Allow) {
            allowed += 1;
        }
    }
    // A request whose grant names another host is challenged, still
    // without allocating.
    let other = decide_one("other.example.test");
    COUNTING.with(|on| on.set(false));

    assert_eq!(allowed, 10_000);
    assert_eq!(
        other,
        (AccessOutcome::LoginRequired, AccessOutcome::LoginRequired)
    );
    assert_eq!(
        ALLOCATIONS.with(Cell::get),
        0,
        "the compiled-ingress request path allocated"
    );
}
