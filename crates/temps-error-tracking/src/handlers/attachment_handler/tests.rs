// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::handlers::group_workflow_handler::tests::{
    app_state, metadata, test_user, RecordingAuditLogger,
};
use axum_test::TestServer;
use chrono::Utc;
use temps_authz::Role;

fn state_without_queries() -> Arc<AppState> {
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    app_state(&db, Arc::new(RecordingAuditLogger::default()))
}

fn view(filename: &str, content_type: &str) -> AttachmentView {
    AttachmentView {
        id: 7,
        owner_kind: "error".into(),
        owner_id: 11,
        filename: filename.into(),
        content_type: content_type.into(),
        attachment_type: "event.attachment".into(),
        size_bytes: 3,
        created_at: Utc::now(),
    }
}

/// `view` as the database returns it.
fn row(view: &AttachmentView) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
    std::collections::BTreeMap::from([
        ("id", view.id.into()),
        ("owner_kind", view.owner_kind.clone().into()),
        ("owner_id", view.owner_id.into()),
        ("filename", view.filename.clone().into()),
        ("content_type", view.content_type.clone().into()),
        ("attachment_type", view.attachment_type.clone().into()),
        ("size_bytes", view.size_bytes.into()),
        ("created_at", view.created_at.into()),
    ])
}

#[tokio::test]
async fn without_a_token_every_route_answers_401() {
    let server = TestServer::new(routes().with_state(state_without_queries()));
    for uri in [
        "/projects/1/error-groups/2/events/3/attachments",
        "/projects/1/feedback/4/attachments",
        "/projects/1/error-attachments/usage",
        "/projects/1/error-attachments/5/content",
    ] {
        let response = server.get(uri).await;
        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    let response = server.delete("/projects/1/error-attachments/5").await;
    assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    let response = server
        .put("/projects/1/error-attachments/quota")
        .json(&serde_json::json!({ "quota_bytes": 1024 }))
        .await;
    assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_reader_can_neither_delete_an_attachment_nor_change_the_quota() {
    let problem = delete_error_attachment(
        State(state_without_queries()),
        RequireAuth(test_user(3, Role::Reader)),
        Path((1, 5)),
        Extension(metadata()),
    )
    .await
    .expect_err("a reader must be refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);

    let problem = set_error_attachment_quota(
        State(state_without_queries()),
        RequireAuth(test_user(3, Role::Reader)),
        Path(1),
        Extension(metadata()),
        Json(SetAttachmentQuotaRequest { quota_bytes: 1024 }),
    )
    .await
    .expect_err("a reader must be refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_role_without_error_tracking_read_cannot_download() {
    let problem = get_error_attachment_content(
        State(state_without_queries()),
        RequireAuth(test_user(3, Role::MetricsIngest)),
        Path((1, 5)),
    )
    .await
    .expect_err("refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_quota_out_of_range_is_a_400_naming_the_bounds_and_audits_nothing() {
    let audit = Arc::new(RecordingAuditLogger::default());
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    let state = app_state(&db, audit.clone());
    for quota_bytes in [-1, crate::services::attachments::MAX_QUOTA_BYTES + 1] {
        let problem = set_error_attachment_quota(
            State(state.clone()),
            RequireAuth(test_user(3, Role::User)),
            Path(1),
            Extension(metadata()),
            Json(SetAttachmentQuotaRequest { quota_bytes }),
        )
        .await
        .expect_err("out of range");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        let detail = problem
            .body
            .get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        assert!(detail.contains("between 0 and"), "{detail}");
        assert!(detail.contains(&quota_bytes.to_string()), "{detail}");
    }
    assert!(audit.entries.lock().expect("audit lock").is_empty());
}

#[tokio::test]
async fn without_blob_storage_a_download_is_a_503_pointing_at_the_storage_page() {
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([[row(&view("shot.png", "image/png"))]])
            .into_connection(),
    );
    let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));
    let problem = get_error_attachment_content(
        State(state),
        RequireAuth(test_user(3, Role::Reader)),
        Path((1, 7)),
    )
    .await
    .expect_err("no blob storage");
    assert_eq!(problem.status_code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        problem.body.get("setup_path").and_then(|v| v.as_str()),
        Some("/storage")
    );
    assert_eq!(
        problem.body.get("project_id").and_then(|v| v.as_i64()),
        Some(1)
    );
}

#[tokio::test]
async fn an_unknown_attachment_is_a_404_carrying_its_id() {
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()])
            .into_connection(),
    );
    let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));
    let problem = get_error_attachment_content(
        State(state),
        RequireAuth(test_user(3, Role::Reader)),
        Path((1, 99)),
    )
    .await
    .expect_err("not found");
    assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    assert_eq!(
        problem.body.get("attachment_id").and_then(|v| v.as_i64()),
        Some(99)
    );
    assert!(problem
        .body
        .get("detail")
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.contains("Attachment 99 not found in project 1")));
}

#[test]
fn raster_images_are_served_inline_and_everything_else_as_a_download() {
    let response = attachment_response(
        &view("shot.png", "image/png"),
        axum::body::Bytes::from_static(b"png"),
    );
    let header = |name: header::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(header(header::CONTENT_TYPE), "image/png");
    assert!(header(header::CONTENT_DISPOSITION).starts_with("inline;"));
    assert_eq!(header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert!(header(header::CONTENT_SECURITY_POLICY).contains("sandbox"));
    assert_eq!(header(header::CACHE_CONTROL), "private, no-store");

    for (filename, content_type) in [
        ("page.html", "text/html"),
        ("logo.svg", "image/svg+xml"),
        ("notes.txt", "text/plain"),
    ] {
        let response = attachment_response(
            &view(filename, content_type),
            axum::body::Bytes::from_static(b"<script>"),
        );
        let headers = response.headers();
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/octet-stream"),
            "{content_type}"
        );
        assert!(headers
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("attachment;")));
    }
}

#[test]
fn file_names_are_quoted_safely_and_kept_exactly_in_the_encoded_form() {
    assert_eq!(
        content_disposition(false, "report \"final\".txt"),
        "attachment; filename=\"report _final_.txt\"; filename*=UTF-8''report%20%22final%22.txt"
    );
    let value = content_disposition(true, "captura de pantalla ñ.png");
    assert!(
        value.starts_with("inline; filename=\"captura de pantalla _.png\""),
        "{value}"
    );
    assert!(
        value.ends_with("filename*=UTF-8''captura%20de%20pantalla%20%C3%B1.png"),
        "{value}"
    );
    assert!(HeaderValue::from_str(&content_disposition(false, "a\u{7f}b;c\\d%e")).is_ok());
}
