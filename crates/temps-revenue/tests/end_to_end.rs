//! End-to-end integration tests for temps-revenue.
//!
//! These tests hit a real (TimescaleDB) database via `TestDatabase`.
//! When Docker is unavailable, each test returns early so CI without
//! Docker still passes (per the project's "skip gracefully at runtime"
//! rule — no `#[ignore]` annotations).

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sha2::Sha256;

use temps_core::EncryptionService;
use temps_database::test_utils::TestDatabase;
use temps_revenue::{
    CreateIntegrationInput, IngestOutcome, ProviderRegistry, RevenueAnalyticsService,
    RevenueIngestionService, RevenueIntegrationService,
};

type HmacSha256 = Hmac<Sha256>;

fn encryption() -> Arc<EncryptionService> {
    Arc::new(
        EncryptionService::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .unwrap(),
    )
}

async fn try_test_db() -> Option<TestDatabase> {
    // TestDatabase boots a TimescaleDB container; on machines without
    // Docker, we want to skip — not fail.
    match TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!(
                "Docker/TimescaleDB unavailable, skipping revenue integration test: {}",
                e
            );
            None
        }
    }
}

/// Insert a minimal `projects` row via raw SQL — enough for the FK from
/// `revenue_integrations.project_id` to be satisfied.
async fn insert_project(conn: &impl ConnectionTrait, id: i32, slug: &str) {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        INSERT INTO projects (
            id, name, repo_name, repo_owner, directory, main_branch,
            preset, preset_config, deployment_config, created_at, updated_at,
            slug, is_deleted, deleted_at, last_deployment, is_public_repo,
            git_url, git_provider_connection_id, attack_mode,
            enable_preview_environments, source_type
        )
        VALUES (
            $1, $2, 'repo', 'owner', '.', 'main',
            '"nextjs"'::jsonb, NULL, NULL, NOW(), NOW(),
            $3, FALSE, NULL, NULL, TRUE,
            NULL, NULL, FALSE, FALSE, 'git'
        )
        ON CONFLICT (id) DO NOTHING
        "#,
        [id.into(), format!("proj-{}", id).into(), slug.into()],
    );
    conn.execute(stmt).await.expect("insert project");
}

fn make_stripe_webhook(secret: &str, payload: &str) -> (HeaderMap, Bytes) {
    let ts = Utc::now().timestamp();
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{}.{}", ts, payload).as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());
    let header = format!("t={},v1={}", ts, sig);
    let mut headers = HeaderMap::new();
    headers.insert("stripe-signature", header.parse().unwrap());
    (headers, Bytes::from(payload.to_string()))
}

/// subscription.created with mrr = 2000 (usd), monthly interval
fn stripe_subscription_created_payload(sub_id: &str, customer: &str, amount: i64) -> String {
    format!(
        r#"{{
            "id": "evt_{sub}_created",
            "type": "customer.subscription.created",
            "created": {now},
            "data": {{
                "object": {{
                    "id": "{sub}",
                    "customer": "{cust}",
                    "status": "active",
                    "currency": "usd",
                    "items": {{
                        "data": [
                            {{"price": {{"unit_amount": {amt}, "recurring": {{"interval": "month", "interval_count": 1}}}}, "quantity": 1}}
                        ]
                    }}
                }}
            }}
        }}"#,
        sub = sub_id,
        cust = customer,
        amt = amount,
        now = Utc::now().timestamp(),
    )
}

#[tokio::test]
async fn ingest_then_summary_projects_correctly() {
    let Some(test_db) = try_test_db().await else {
        return;
    };
    let conn = test_db.connection_arc();
    insert_project(conn.as_ref(), 1001, "acme").await;

    let enc = encryption();
    let integrations = Arc::new(RevenueIntegrationService::new(
        conn.clone(),
        enc.clone(),
        ProviderRegistry::default_registry(),
    ));
    let ingestion = Arc::new(RevenueIngestionService::new(
        conn.clone(),
        integrations.clone(),
        ProviderRegistry::default_registry(),
    ));
    let analytics = RevenueAnalyticsService::new(conn.clone());

    let signing_secret = "whsec_test_secret_0123456789abcdef";
    let integration = integrations
        .create(CreateIntegrationInput {
            project_id: 1001,
            provider: "stripe".into(),
            signing_secret: signing_secret.into(),
        })
        .await
        .expect("create integration");

    // Send a subscription.created webhook with a correctly-signed body.
    let payload = stripe_subscription_created_payload("sub_123", "cus_abc", 2000);
    let (headers, body) = make_stripe_webhook(signing_secret, &payload);

    let outcome = ingestion
        .ingest(
            "stripe",
            &integration.webhook_path_token,
            headers.clone(),
            body.clone(),
        )
        .await
        .expect("ingest");

    assert!(
        matches!(outcome, IngestOutcome::Ingested(1)),
        "first call must ingest exactly one event, got {:?}",
        outcome
    );

    // Second call with identical payload must be an idempotent dedupe.
    let outcome2 = ingestion
        .ingest("stripe", &integration.webhook_path_token, headers, body)
        .await
        .expect("ingest dup");
    assert!(
        matches!(outcome2, IngestOutcome::Duplicate),
        "duplicate event must be deduped, got {:?}",
        outcome2
    );

    let summary = analytics.summary(1001, "usd").await.expect("summary");
    assert_eq!(summary.current_mrr_minor, 2000);
    assert_eq!(summary.current_arr_minor, 24000);
    assert_eq!(summary.active_subscriptions, 1);
    assert!(summary.active_customers >= 1);
}

#[tokio::test]
async fn invalid_signature_is_rejected() {
    let Some(test_db) = try_test_db().await else {
        return;
    };
    let conn = test_db.connection_arc();
    insert_project(conn.as_ref(), 1002, "sigtest").await;

    let integrations = Arc::new(RevenueIntegrationService::new(
        conn.clone(),
        encryption(),
        ProviderRegistry::default_registry(),
    ));
    let ingestion = RevenueIngestionService::new(
        conn.clone(),
        integrations.clone(),
        ProviderRegistry::default_registry(),
    );

    let integration = integrations
        .create(CreateIntegrationInput {
            project_id: 1002,
            provider: "stripe".into(),
            signing_secret: "whsec_real_secret".into(),
        })
        .await
        .unwrap();

    // Sign with the WRONG secret.
    let payload = stripe_subscription_created_payload("sub_bad", "cus_bad", 500);
    let (headers, body) = make_stripe_webhook("whsec_WRONG_secret", &payload);

    let err = ingestion
        .ingest("stripe", &integration.webhook_path_token, headers, body)
        .await
        .unwrap_err();
    assert!(
        format!("{}", err).contains("Provider error") || format!("{:?}", err).contains("Provider"),
        "expected provider error, got {:?}",
        err
    );
}

#[tokio::test]
async fn tenant_isolation_in_summary() {
    let Some(test_db) = try_test_db().await else {
        return;
    };
    let conn = test_db.connection_arc();
    insert_project(conn.as_ref(), 2001, "alpha").await;
    insert_project(conn.as_ref(), 2002, "beta").await;

    let integrations = Arc::new(RevenueIntegrationService::new(
        conn.clone(),
        encryption(),
        ProviderRegistry::default_registry(),
    ));
    let ingestion = RevenueIngestionService::new(
        conn.clone(),
        integrations.clone(),
        ProviderRegistry::default_registry(),
    );
    let analytics = RevenueAnalyticsService::new(conn.clone());

    // Project alpha: one $10/mo sub.
    let alpha_secret = "whsec_alpha";
    let alpha = integrations
        .create(CreateIntegrationInput {
            project_id: 2001,
            provider: "stripe".into(),
            signing_secret: alpha_secret.into(),
        })
        .await
        .unwrap();
    let payload_a = stripe_subscription_created_payload("sub_alpha", "cus_alpha", 1000);
    let (headers_a, body_a) = make_stripe_webhook(alpha_secret, &payload_a);
    ingestion
        .ingest("stripe", &alpha.webhook_path_token, headers_a, body_a)
        .await
        .unwrap();

    // Project beta: one $50/mo sub.
    let beta_secret = "whsec_beta";
    let beta = integrations
        .create(CreateIntegrationInput {
            project_id: 2002,
            provider: "stripe".into(),
            signing_secret: beta_secret.into(),
        })
        .await
        .unwrap();
    let payload_b = stripe_subscription_created_payload("sub_beta", "cus_beta", 5000);
    let (hb, bb) = make_stripe_webhook(beta_secret, &payload_b);
    ingestion
        .ingest("stripe", &beta.webhook_path_token, hb, bb)
        .await
        .unwrap();

    // Each project must see ONLY its own revenue.
    let a = analytics.summary(2001, "usd").await.unwrap();
    let b = analytics.summary(2002, "usd").await.unwrap();

    assert_eq!(
        a.current_mrr_minor, 1000,
        "alpha leaked another tenant's data"
    );
    assert_eq!(
        b.current_mrr_minor, 5000,
        "beta leaked another tenant's data"
    );
}

#[tokio::test]
async fn provider_mismatch_in_url_path() {
    let Some(test_db) = try_test_db().await else {
        return;
    };
    let conn = test_db.connection_arc();
    insert_project(conn.as_ref(), 3001, "mismatch").await;

    let integrations = Arc::new(RevenueIntegrationService::new(
        conn.clone(),
        encryption(),
        ProviderRegistry::default_registry(),
    ));
    let ingestion = RevenueIngestionService::new(
        conn.clone(),
        integrations.clone(),
        ProviderRegistry::default_registry(),
    );

    let integration = integrations
        .create(CreateIntegrationInput {
            project_id: 3001,
            provider: "stripe".into(),
            signing_secret: "whsec_x".into(),
        })
        .await
        .unwrap();

    // URL claims "paddle" but the integration was created as "stripe".
    let err = ingestion
        .ingest(
            "paddle",
            &integration.webhook_path_token,
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{:?}", err).contains("ProviderMismatch"),
        "expected ProviderMismatch, got {:?}",
        err
    );
}
