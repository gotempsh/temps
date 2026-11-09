// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::types::AccessMethod;

fn policy(prefix: &str) -> SandboxAccessPolicy {
    SandboxAccessPolicy {
        origin: "https://api.example.com".into(),
        path_prefixes: vec![prefix.into()],
        methods: vec![AccessMethod::Get],
    }
}

fn request(now: DateTime<Utc>) -> PrepareSession {
    PrepareSession {
        target: BindingTarget {
            project_id: 1,
            sandbox_id: 42,
            incarnation_id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
        },
        grant_id: format!("grant_{}", Uuid::new_v4().simple()),
        service_id: format!("svc_{}", Uuid::new_v4().simple()),
        service_version: 1,
        policy: policy("/v1"),
        expires_at: now + chrono::Duration::seconds(60),
        max_ttl_seconds: 60,
    }
}

pub(crate) fn prepared_session() -> PreparedSession {
    let now = Utc::now();
    PreparedSession::prepare(request(now), now).unwrap()
}

#[test]
fn sdk_policy_preserves_segment_boundaries_and_denies_other_destinations() {
    let (_, scope) = compile_policy(&policy("/v1")).unwrap();
    for path in ["/v1", "/v1/", "/v1/models"] {
        assert!(scope.allows("api.example.com", 443, "GET", path), "{path}");
    }
    for path in ["/", "/v10", "/v1-admin", "/v2/models"] {
        assert!(!scope.allows("api.example.com", 443, "GET", path), "{path}");
    }
    assert!(!scope.allows("other.example.com", 443, "GET", "/v1"));
    assert!(!scope.allows("api.example.com", 8443, "GET", "/v1"));
    assert!(!scope.allows("api.example.com", 443, "POST", "/v1"));
    assert!(!scope.allows_tool("arbitrary-tool"));
}

#[test]
fn sdk_policy_preserves_root_and_trailing_slash_semantics() {
    let (_, root) = compile_policy(&policy("/")).unwrap();
    assert!(root.allows("api.example.com", 443, "GET", "/"));
    assert!(root.allows("api.example.com", 443, "GET", "/anything"));
    let (_, subtree) = compile_policy(&policy("/v1/")).unwrap();
    assert!(!subtree.allows("api.example.com", 443, "GET", "/v1"));
    assert!(subtree.allows("api.example.com", 443, "GET", "/v1/"));
    assert!(subtree.allows("api.example.com", 443, "GET", "/v1/models"));
}

#[test]
fn sdk_policy_bounds_and_deduplicates_patterns_without_widening_methods() {
    let mut input = policy("/v1");
    input.path_prefixes = (0..32).map(|i| format!("/v{i}")).collect();
    input.methods = vec![
        AccessMethod::Get,
        AccessMethod::Head,
        AccessMethod::Post,
        AccessMethod::Put,
        AccessMethod::Patch,
        AccessMethod::Delete,
    ];
    let (_, scope) = compile_policy(&input).unwrap();
    assert_eq!(scope.paths.len(), 64);
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"] {
        assert!(scope.allows("api.example.com", 443, method, "/v0"));
    }
    assert!(!scope.allows("api.example.com", 443, "OPTIONS", "/v0"));
    input.path_prefixes.push("/extra".into());
    assert!(compile_policy(&input).is_err());
    input.path_prefixes = vec!["/v1".into(), "/v1".into()];
    input.methods = vec![AccessMethod::Get, AccessMethod::Get];
    let (_, scope) = compile_policy(&input).unwrap();
    assert_eq!(scope.paths.len(), 2);
    assert_eq!(scope.methods.len(), 1);
}

#[test]
fn sdk_boundary_revalidates_stored_policies_and_normalizes_origin() {
    for origin in [
        "http://api.example.com",
        "https://api.example.com:8443",
        "https://user:secret@api.example.com",
        "https://127.0.0.1",
        "https://*.example.com",
        "https://api.example.com/path",
        "https://api.example.com?key=secret",
    ] {
        let mut input = policy("/v1");
        input.origin = origin.into();
        let error = compile_policy(&input).unwrap_err().to_string();
        assert!(!error.contains("secret"));
    }
    for prefix in ["/v1*", "/a/../b", "/%2fadmin", "/v1?key=value", "/a//b", ""] {
        assert!(compile_policy(&policy(prefix)).is_err(), "{prefix}");
    }
    let mut input = policy("/v1");
    input.origin = "https://API.Example.com:443/".into();
    assert_eq!(
        compile_policy(&input).unwrap().0.origin,
        "https://api.example.com"
    );
    input.methods.clear();
    assert!(compile_policy(&input).is_err());
    input.methods = vec![AccessMethod::Get];
    input.path_prefixes.clear();
    assert!(compile_policy(&input).is_err());
}

#[test]
fn prepared_claims_bind_project_node_incarnation_grant_and_service_version() {
    let now = Utc::now();
    let input = request(now);
    let result = PreparedSession::prepare(input.clone(), now).unwrap();
    let claims = result.claims();
    assert_eq!(claims.scope.as_str(), "temps:project:1");
    assert_eq!(claims.aud, Some(NodeId(input.target.node_id)));
    assert_eq!(
        claims.sub.as_deref(),
        Some(format!("{}/1", input.target.incarnation_id).as_str())
    );
    assert_eq!(
        claims.rules,
        vec![RuleId(parse_record_id(&input.grant_id, "grant_").unwrap())]
    );
    assert_eq!(claims.labels["service_id"], input.service_id.to_string());
    assert_eq!(claims.labels["service_version"], "1");
    assert_eq!(claims.exp, input.expires_at);
    assert!(result.rule().allows("api.example.com", 443, "GET", "/v1"));
    let mut restored = input.clone();
    restored.target.incarnation_id = Uuid::new_v4();
    assert_ne!(
        PreparedSession::prepare(restored, now)
            .unwrap()
            .claims()
            .sub,
        claims.sub
    );
    let mut moved = input.clone();
    moved.target.node_id = Uuid::new_v4();
    assert_ne!(
        PreparedSession::prepare(moved, now).unwrap().claims().aud,
        claims.aud
    );
    let mut other_project = input;
    other_project.target.project_id = 2;
    assert_ne!(
        PreparedSession::prepare(other_project, now)
            .unwrap()
            .claims()
            .scope,
        claims.scope
    );
}

#[test]
fn preparation_and_repreparation_never_extend_absolute_expiry() {
    let now = Utc::now();
    let input = request(now);
    let later = now + chrono::Duration::milliseconds(1500);
    let prepared = PreparedSession::prepare(input.clone(), later).unwrap();
    assert!(prepared.claims().exp <= input.expires_at);
    assert_eq!(
        prepared.claims().exp,
        input.expires_at - chrono::Duration::milliseconds(500)
    );
    for late in [
        input.expires_at - chrono::Duration::milliseconds(500),
        input.expires_at,
        input.expires_at + chrono::Duration::seconds(1),
    ] {
        assert!(
            matches!(PreparedSession::prepare(input.clone(), late), Err(PreparationError::Invalid { grant_id, .. }) if grant_id == input.grant_id)
        );
    }
    let mut over_cap = input;
    over_cap.expires_at += chrono::Duration::milliseconds(1);
    assert!(PreparedSession::prepare(over_cap, now).is_err());
}

#[test]
fn preparation_rejects_missing_identity_invalid_caps_and_policy() {
    let now = Utc::now();
    let valid = request(now);
    for index in 0..7 {
        let mut input = valid.clone();
        match index {
            0 => input.target.project_id = 0,
            1 => input.service_version = 0,
            2 => input.target.sandbox_id = 0,
            3 => input.target.incarnation_id = Uuid::nil(),
            4 => input.target.node_id = Uuid::nil(),
            5 => input.grant_id = "invalid".into(),
            _ => input.service_id = "invalid".into(),
        }
        assert!(matches!(
            PreparedSession::prepare(input, now),
            Err(PreparationError::Invalid { .. })
        ));
    }
    for cap in [0, 59, 86401, i32::MAX] {
        let mut input = valid.clone();
        input.max_ttl_seconds = cap;
        assert!(PreparedSession::prepare(input, now).is_err());
    }
    let mut input = valid;
    input.policy.origin = "http://api.example.com".into();
    assert!(matches!(
        PreparedSession::prepare(input, now),
        Err(PreparationError::Policy { .. })
    ));
}

#[test]
fn pinned_sdk_cannot_be_mistaken_for_live_enforcement() {
    assert!(include_str!("../../Cargo.toml").contains(SDK_REVISION));
    assert!(BLOCKERS.contains(&IntegrationBlocker::HardStreamExpiry));
    assert!(BLOCKERS.contains(&IntegrationBlocker::RevocationAcknowledgement));
    assert!(BLOCKERS.contains(&IntegrationBlocker::TrustedHostTransport));
}

#[test]
fn prepared_sessions_require_hard_expiry_without_traffic_grace() {
    let session = prepared_session();
    assert!(session.claims().strict);
    assert_eq!(
        session.claims().open_traffic_deadline(),
        session.claims().exp
    );
    assert!(session.rule().git.is_none());
}

#[test]
fn binding_preserves_independent_same_host_grant_deadlines() {
    let now = Utc::now();
    let a = request(now);
    let mut b = a.clone();
    b.grant_id = format!("grant_{}", Uuid::new_v4().simple());
    b.policy = policy("/v2");
    b.expires_at = now + chrono::Duration::seconds(120);
    b.max_ttl_seconds = 120;
    let plan = PreparedBinding::prepare(vec![a, b], 1, now).unwrap();
    assert!(plan.claims().strict);
    assert_eq!(plan.claims().exp, now + chrono::Duration::seconds(120));
    for grant in plan.grants() {
        assert_eq!(
            plan.claims().rule_deadline(&grant.claims().rules[0]),
            grant.claims().exp
        );
    }
}

#[test]
fn binding_rejects_duplicates_limits_and_identity_mixing() {
    let now = Utc::now();
    let a = request(now);
    assert!(PreparedBinding::prepare(vec![], 1, now).is_err());
    assert!(PreparedBinding::prepare(vec![a.clone()], 0, now).is_err());
    assert!(PreparedBinding::prepare(vec![a.clone(); 17], 1, now).is_err());
    assert!(PreparedBinding::prepare(vec![a.clone(), a.clone()], 1, now).is_err());
    for index in 0..4 {
        let mut b = a.clone();
        b.grant_id = format!("grant_{}", Uuid::new_v4().simple());
        match index {
            0 => b.target.project_id += 1,
            1 => b.target.sandbox_id += 1,
            2 => b.target.node_id = Uuid::new_v4(),
            _ => b.target.incarnation_id = Uuid::new_v4(),
        }
        assert!(PreparedBinding::prepare(vec![a.clone(), b], 1, now).is_err());
    }
}
