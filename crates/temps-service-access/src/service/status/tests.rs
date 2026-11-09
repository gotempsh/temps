// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

fn grant() -> ServiceGrantResponse {
    ServiceGrantResponse {
        id: "grant_test".into(),
        service_id: "svc_test".into(),
        service_name: "Test API".into(),
        sandbox_id: "sbx_test".into(),
        service_version: 1,
        policy: SandboxAccessPolicy {
            provider: None,
            origin: "https://api.example.com".into(),
            path_prefixes: vec!["/v1".into()],
            methods: vec![AccessMethod::Get],
            github_repository: None,
        },
        status: ServiceGrantStatus::PendingApproval,
        reason: String::new(),
        enforcement: GrantEnforcement::NotRequested,
        connected: false,
        version: 1,
        created_at: Utc::now(),
        expires_at: Utc::now() + Duration::minutes(30),
        revoked_at: None,
    }
}
fn receipt() -> EnforcementRow {
    EnforcementRow {
        grant_id: "grant_test".into(),
        lease_expires_at: Some(Utc::now() + Duration::seconds(20)),
        current_receipt: true,
        installed: true,
        cut_confirmed: false,
        closed: false,
        running: true,
    }
}
#[test]
fn only_a_current_live_receipt_of_the_exact_grant_reports_active() {
    let now = Utc::now();
    let mut g = grant();
    apply_receipt(&mut g, &receipt(), now);
    assert!(g.connected);
    assert_eq!(g.enforcement, GrantEnforcement::Active);
    assert_eq!(g.status, ServiceGrantStatus::Approved);
    for case in 0..6 {
        let mut r = receipt();
        let mut g = grant();
        match case {
            0 => r.current_receipt = false,
            1 => r.installed = false,
            2 => r.lease_expires_at = None,
            3 => r.closed = true,
            4 => r.running = false,
            _ => g.status = ServiceGrantStatus::PolicyChanged,
        }
        apply_receipt(&mut g, &r, now);
        assert!(!g.connected, "case {case}");
        assert_ne!(g.enforcement, GrantEnforcement::Active);
    }
}
#[test]
fn revocation_waits_for_removal_ack_or_signed_expiry() {
    let now = Utc::now();
    let mut g = grant();
    g.status = ServiceGrantStatus::Revoked;
    g.revoked_at = Some(now);
    let mut r = receipt();
    r.current_receipt = false;
    apply_receipt(&mut g, &r, now);
    assert_eq!(g.enforcement, GrantEnforcement::Stopping);
    r.current_receipt = true;
    r.installed = false;
    apply_receipt(&mut g, &r, now);
    assert_eq!(g.enforcement, GrantEnforcement::Stopped);
    r.current_receipt = false;
    r.installed = true;
    r.lease_expires_at = Some(now);
    apply_receipt(&mut g, &r, now);
    assert_eq!(g.enforcement, GrantEnforcement::Stopping);
    r.lease_expires_at = None;
    r.closed = true;
    r.cut_confirmed = true;
    apply_receipt(&mut g, &r, now);
    assert_eq!(g.enforcement, GrantEnforcement::Stopped);
}
#[tokio::test]
async fn receipt_reads_are_bounded_scoped_and_preserve_database_error_context() {
    use sea_orm::{DbErr, MockDatabase};
    let service = ServiceAccessService::new(
        Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("sensitive driver detail".into())])
                .into_connection(),
        ),
        crate::service::tests::encryption(),
        Arc::new(crate::broker::EmbeddedTokenlessBroker),
    );
    service.observe_grants(7, 42, &mut []).await.unwrap();
    let error = service
        .observe_grants(7, 42, &mut [grant()])
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("read worker enforcement receipts"));
    assert!(error.to_string().contains('7'));
    assert!(!error.to_string().contains("sensitive driver detail"));
}

#[test]
fn stale_snapshot_receipt_does_not_prove_a_shared_binding_stopped() {
    let now = Utc::now();
    let mut r = receipt();
    r.lease_expires_at = Some(now - Duration::seconds(1));
    r.current_receipt = false;
    let mut g = grant();
    apply_receipt(&mut g, &r, now);
    assert_eq!(g.enforcement, GrantEnforcement::Pending);
    assert!(!g.connected);
    g.expires_at = now;
    apply_receipt(&mut g, &r, now);
    assert_eq!(g.enforcement, GrantEnforcement::Stopped);
}
