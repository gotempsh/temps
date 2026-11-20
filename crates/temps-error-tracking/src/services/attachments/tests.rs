// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

/// Blob storage in memory. `failing_puts` and `failing_deletes` make
/// those calls answer as an unreachable store would.
#[derive(Default)]
pub(crate) struct MemoryStore {
    objects: Mutex<BTreeMap<(i32, String), Bytes>>,
    pub(crate) failing_puts: AtomicBool,
    pub(crate) failing_deletes: AtomicBool,
}

impl MemoryStore {
    pub(crate) fn keys(&self, project_id: i32) -> Vec<String> {
        self.objects
            .lock()
            .expect("store lock")
            .keys()
            .filter(|(project, _)| *project == project_id)
            .map(|(_, key)| key.clone())
            .collect()
    }
}

#[async_trait]
impl PayloadBlobStore for MemoryStore {
    async fn put(&self, project_id: i32, key: &str, body: Bytes) -> Result<(), PayloadBlobError> {
        if self.failing_puts.load(Ordering::SeqCst) {
            return Err(PayloadBlobError::Unavailable {
                project_id,
                reason: "synthetic outage".into(),
            });
        }
        self.objects
            .lock()
            .expect("store lock")
            .insert((project_id, key.to_string()), body);
        Ok(())
    }

    async fn get(
        &self,
        project_id: i32,
        key: &str,
        _max_bytes: usize,
    ) -> Result<Bytes, PayloadBlobError> {
        self.objects
            .lock()
            .expect("store lock")
            .get(&(project_id, key.to_string()))
            .cloned()
            .ok_or_else(|| PayloadBlobError::NotFound {
                project_id,
                key: key.to_string(),
            })
    }

    async fn delete_prefix(&self, project_id: i32, prefix: &str) -> Result<u64, PayloadBlobError> {
        if self.failing_deletes.load(Ordering::SeqCst) {
            return Err(PayloadBlobError::Unavailable {
                project_id,
                reason: "synthetic outage".into(),
            });
        }
        let mut objects = self.objects.lock().expect("store lock");
        let before = objects.len();
        objects.retain(|(project, key), _| !(*project == project_id && key.starts_with(prefix)));
        Ok((before - objects.len()) as u64)
    }
}

#[test]
fn file_names_lose_directories_and_control_characters() {
    assert_eq!(clean_filename(Some("../../etc/passwd")), "passwd");
    assert_eq!(
        clean_filename(Some("C:\\Users\\someone\\shot.png")),
        "shot.png"
    );
    assert_eq!(clean_filename(Some("re\u{0}port\n.txt")), "report.txt");
    assert_eq!(clean_filename(Some("  ")), "attachment");
    assert_eq!(clean_filename(Some("..")), "attachment");
    assert_eq!(clean_filename(None), "attachment");
    assert_eq!(
        clean_filename(Some(&"a".repeat(500))).chars().count(),
        MAX_FILENAME_CHARS
    );
}

#[test]
fn content_types_keep_their_essence_or_become_octet_stream() {
    assert_eq!(
        clean_content_type(Some("Image/PNG; charset=binary")),
        "image/png"
    );
    assert_eq!(clean_content_type(Some("text/plain")), "text/plain");
    assert_eq!(
        clean_content_type(Some("not a type")),
        "application/octet-stream"
    );
    assert_eq!(
        clean_content_type(Some("text/<script>")),
        "application/octet-stream"
    );
    assert_eq!(clean_content_type(None), "application/octet-stream");
    let long = format!("application/{}", "x".repeat(200));
    assert_eq!(clean_content_type(Some(&long)), "application/octet-stream");
}

#[test]
fn only_raster_images_are_shown_inline() {
    for inline in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        assert!(is_inline_image(inline), "{inline}");
    }
    for download in [
        "image/svg+xml",
        "text/html",
        "application/pdf",
        "text/plain",
    ] {
        assert!(!is_inline_image(download), "{download}");
    }
}

#[test]
fn event_ids_are_normalized_like_the_source_keys() {
    let id = "0123456789abcdef0123456789abcdef";
    assert_eq!(normalize_event_id(id).as_deref(), Some(id));
    assert_eq!(
        normalize_event_id("01234567-89AB-CDEF-0123-456789ABCDEF").as_deref(),
        Some(id)
    );
    assert_eq!(normalize_event_id("00000000000000000000000000000000"), None);
    assert_eq!(normalize_event_id("not-an-id"), None);
    assert_eq!(normalize_event_id(""), None);
}

#[test]
fn objects_of_different_attachments_never_share_a_prefix() {
    assert_eq!(object_key(12), ".temps/error-attachments/12/data");
    assert!(!object_key(120).starts_with(&object_prefix(12)));
    assert!(object_key(12).starts_with(&object_prefix(12)));
}

mod storage {
    use super::super::*;
    use super::MemoryStore;
    use crate::handlers::group_workflow_handler::tests::seed;
    use crate::services::feedback_service::{FeedbackService, FeedbackSubmission};
    use sea_orm::{ActiveModelTrait, Set};
    use temps_entities::error_events;
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    struct Fixture {
        _db: TestDatabase,
        db: Arc<DatabaseConnection>,
        service: Arc<AttachmentService>,
        store: Arc<MemoryStore>,
        project_id: i32,
        group_id: i32,
    }

    async fn fixture(name: &str) -> Option<Fixture> {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping {name}: {error}");
                return None;
            }
            Err(error) => panic!("{name} database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let (project_id, group_id, _) = seed(db.as_ref()).await;
        let service = Arc::new(AttachmentService::new(db.clone()));
        let store = Arc::new(MemoryStore::default());
        service.set_store(store.clone());
        Some(Fixture {
            _db: test_db,
            db,
            service,
            store,
            project_id,
            group_id,
        })
    }

    /// An error event sent by an SDK as `sdk_event_id`, `age` ago.
    async fn event(f: &Fixture, sdk_event_id: &str, age: chrono::Duration) -> i64 {
        let at = Utc::now() - age;
        let event = error_events::ActiveModel {
            error_group_id: Set(f.group_id),
            fingerprint_hash: Set("synthetic".into()),
            timestamp: Set(at),
            exception_type: Set("TypeError".into()),
            exception_value: Set(None),
            source: Set(Some("sentry".into())),
            data: Set(None),
            project_id: Set(f.project_id),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(f.db.as_ref())
        .await
        .expect("insert event");
        f.db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO error_event_source_keys(project_id,sdk_event_id,error_group_id,error_event_id,received_at) \
             VALUES ($1,$2,$3,$4,NOW())",
            [
                f.project_id.into(),
                sdk_event_id.into(),
                f.group_id.into(),
                event.id.into(),
            ],
        ))
        .await
        .expect("insert source key");
        event.id
    }

    fn file(name: &str, body: &'static [u8]) -> IncomingAttachment {
        IncomingAttachment {
            filename: name.into(),
            content_type: "text/plain".into(),
            attachment_type: STORED_ATTACHMENT_TYPE.into(),
            body: Bytes::from_static(body),
        }
    }

    fn batch(f: &Fixture, sdk_event_id: &str, files: Vec<IncomingAttachment>) -> AttachmentBatch {
        AttachmentBatch {
            project_id: f.project_id,
            sdk_event_id: sdk_event_id.into(),
            attachments: files,
        }
    }

    const SDK_EVENT: &str = "0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn stores_an_events_attachments_once_and_counts_their_bytes() {
        let Some(f) = fixture("attachment store test").await else {
            return;
        };
        let event_id = event(&f, SDK_EVENT, chrono::Duration::minutes(5)).await;
        let files = vec![file("first.txt", b"one"), file("second.txt", b"three")];

        let stored = f
            .service
            .store_batch(f.store.as_ref(), &batch(&f, SDK_EVENT, files.clone()))
            .await
            .expect("store");
        assert_eq!(stored.stored, 2, "{stored:?}");
        // An SDK retry of the same envelope stores nothing twice.
        let retried = f
            .service
            .store_batch(f.store.as_ref(), &batch(&f, SDK_EVENT, files))
            .await
            .expect("retry");
        assert_eq!((retried.stored, retried.duplicates), (0, 2), "{retried:?}");

        let listed = f
            .service
            .list(f.project_id, AttachmentOwner::Error(event_id))
            .await
            .expect("list");
        let names: Vec<&str> = listed.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["first.txt", "second.txt"]);
        let (view, bytes) = f
            .service
            .content(f.project_id, listed[1].id)
            .await
            .expect("content");
        assert_eq!((view.size_bytes, &bytes[..]), (5, &b"three"[..]));

        let usage = f.service.usage(f.project_id).await.expect("usage");
        assert!(usage.configured, "{usage:?}");
        assert_eq!((usage.used_bytes, usage.files), (8, 2));
        assert_eq!(usage.quota_bytes, DEFAULT_QUOTA_BYTES);
        assert_eq!(f.store.keys(f.project_id).len(), 2);

        // Another project cannot read it.
        let other = f.service.content(f.project_id + 1, listed[1].id).await;
        assert!(matches!(
            other,
            Err(ErrorTrackingError::AttachmentNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn attachments_over_the_quota_or_without_an_owner_are_dropped() {
        let Some(f) = fixture("attachment quota test").await else {
            return;
        };
        event(&f, SDK_EVENT, chrono::Duration::minutes(5)).await;
        let (previous, usage) = f.service.set_quota(f.project_id, 4).await.expect("quota");
        assert_eq!((previous, usage.quota_bytes), (DEFAULT_QUOTA_BYTES, 4));

        let outcome = f
            .service
            .store_batch(
                f.store.as_ref(),
                &batch(
                    &f,
                    SDK_EVENT,
                    vec![file("fits.txt", b"abc"), file("over.txt", b"de")],
                ),
            )
            .await
            .expect("store");
        assert_eq!((outcome.stored, outcome.over_quota), (1, 1), "{outcome:?}");

        let unknown = f
            .service
            .store_batch(
                f.store.as_ref(),
                &batch(
                    &f,
                    "fedcba9876543210fedcba9876543210",
                    vec![file("a.txt", b"a")],
                ),
            )
            .await
            .expect("store");
        assert_eq!(unknown.without_owner, 1, "{unknown:?}");
        let usage = f.service.usage(f.project_id).await.expect("usage");
        assert_eq!((usage.used_bytes, usage.files), (3, 1));
    }

    #[tokio::test]
    async fn a_failed_write_leaves_no_row_and_gives_the_bytes_back() {
        let Some(f) = fixture("attachment write failure test").await else {
            return;
        };
        let event_id = event(&f, SDK_EVENT, chrono::Duration::minutes(5)).await;
        f.store.failing_puts.store(true, Ordering::SeqCst);
        let outcome = f
            .service
            .store_batch(
                f.store.as_ref(),
                &batch(&f, SDK_EVENT, vec![file("a.txt", b"abc")]),
            )
            .await
            .expect("store");
        assert_eq!(outcome.failed, 1, "{outcome:?}");
        assert!(f
            .service
            .list(f.project_id, AttachmentOwner::Error(event_id))
            .await
            .expect("list")
            .is_empty());
        let usage = f.service.usage(f.project_id).await.expect("usage");
        assert_eq!((usage.used_bytes, usage.files), (0, 0));

        // The retry, once storage is back, stores it.
        f.store.failing_puts.store(false, Ordering::SeqCst);
        let retried = f
            .service
            .store_batch(
                f.store.as_ref(),
                &batch(&f, SDK_EVENT, vec![file("a.txt", b"abc")]),
            )
            .await
            .expect("retry");
        assert_eq!(retried.stored, 1, "{retried:?}");
    }

    #[tokio::test]
    async fn deleting_an_attachment_or_its_feedback_deletes_the_object_and_frees_the_quota() {
        let Some(f) = fixture("attachment delete test").await else {
            return;
        };
        let event_id = event(&f, SDK_EVENT, chrono::Duration::minutes(5)).await;
        f.service
            .store_batch(
                f.store.as_ref(),
                &batch(&f, SDK_EVENT, vec![file("a.txt", b"abc")]),
            )
            .await
            .expect("store");
        let listed = f
            .service
            .list(f.project_id, AttachmentOwner::Error(event_id))
            .await
            .expect("list");
        let deleted = f
            .service
            .delete(f.project_id, listed[0].id)
            .await
            .expect("delete");
        assert_eq!(deleted.filename, "a.txt");
        assert!(f.store.keys(f.project_id).is_empty());
        assert!(matches!(
            f.service.delete(f.project_id, listed[0].id).await,
            Err(ErrorTrackingError::AttachmentNotFound { attachment_id, .. }) if attachment_id == listed[0].id
        ));

        // A feedback screenshot goes with its feedback.
        let feedback_id_text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        FeedbackService::new(f.db.clone())
            .ingest(
                f.project_id,
                None,
                None,
                vec![FeedbackSubmission {
                    source: "sentry".into(),
                    source_id: feedback_id_text.into(),
                    timestamp: Utc::now(),
                    message: "The button does nothing".into(),
                    name: None,
                    email: None,
                    url: None,
                    associated_event_id: None,
                    replay_id: None,
                    release: None,
                    environment: None,
                    context: serde_json::json!({}),
                }],
            )
            .await
            .expect("feedback");
        let shot = IncomingAttachment {
            filename: "screenshot.png".into(),
            content_type: "image/png".into(),
            attachment_type: STORED_ATTACHMENT_TYPE.into(),
            body: Bytes::from_static(b"png"),
        };
        let outcome = f
            .service
            .store_batch(f.store.as_ref(), &batch(&f, feedback_id_text, vec![shot]))
            .await
            .expect("store screenshot");
        assert_eq!(outcome.stored, 1, "{outcome:?}");
        let feedback_id: i64 =
            f.db.query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id FROM project_feedback WHERE project_id = $1",
                [f.project_id.into()],
            ))
            .await
            .expect("query")
            .expect("feedback row")
            .try_get("", "id")
            .expect("id");
        assert_eq!(
            f.service
                .list(f.project_id, AttachmentOwner::Feedback(feedback_id))
                .await
                .expect("list")
                .len(),
            1
        );
        assert_eq!(
            f.service
                .delete_owner(f.project_id, AttachmentOwner::Feedback(feedback_id))
                .await
                .expect("delete owner"),
            1
        );
        assert!(f.store.keys(f.project_id).is_empty());
        let usage = f.service.usage(f.project_id).await.expect("usage");
        assert_eq!((usage.used_bytes, usage.files), (0, 0));
    }

    #[tokio::test]
    async fn the_sweeper_removes_expired_unfinished_orphaned_and_deleted_project_attachments() {
        let Some(f) = fixture("attachment sweep test").await else {
            return;
        };
        let kept_event = event(&f, SDK_EVENT, chrono::Duration::minutes(5)).await;
        let expiring = "11111111111111111111111111111111";
        let expired_event = event(&f, expiring, chrono::Duration::days(3)).await;
        for sdk_event_id in [SDK_EVENT, expiring] {
            f.service
                .store_batch(
                    f.store.as_ref(),
                    &batch(&f, sdk_event_id, vec![file("a.txt", b"abc")]),
                )
                .await
                .expect("store");
        }
        // Retention drops the old event's chunk.
        f.db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM error_events WHERE id = $1",
            [expired_event.into()],
        ))
        .await
        .expect("expire event");
        // A reservation whose write never finished, two hours ago.
        f.db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO error_attachments (project_id, owner_kind, owner_id, ordinal, event_time, \
                 filename, content_type, attachment_type, size_bytes, created_at) \
             SELECT $1, 'error', $2, 5, timestamp, 'b.txt', 'text/plain', 'event.attachment', 2, \
                 NOW() - INTERVAL '2 hours' FROM error_events WHERE id = $2",
            [f.project_id.into(), kept_event.into()],
        ))
        .await
        .expect("stale row");
        // A feedback attachment whose feedback is gone.
        f.db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO error_attachments (project_id, owner_kind, owner_id, ordinal, filename, \
                 content_type, attachment_type, size_bytes, stored_at) \
             VALUES ($1, 'feedback', 987654, 0, 'c.png', 'image/png', 'event.attachment', 1, NOW())",
            [f.project_id.into()],
        ))
        .await
        .expect("orphan row");

        let outcome = f
            .service
            .sweep(f.store.as_ref(), Utc::now())
            .await
            .expect("sweep");
        assert_eq!(
            (
                outcome.expired,
                outcome.stale,
                outcome.orphaned_feedback,
                outcome.deleted_projects
            ),
            (1, 1, 1, 0),
            "{outcome:?}"
        );
        let left = f
            .service
            .list(f.project_id, AttachmentOwner::Error(kept_event))
            .await
            .expect("list");
        assert_eq!(left.len(), 1);
        assert_eq!(f.store.keys(f.project_id).len(), 1);
        // A second pass finds nothing more.
        assert_eq!(
            f.service
                .sweep(f.store.as_ref(), Utc::now())
                .await
                .expect("sweep"),
            SweepOutcome::default()
        );

        // An object that cannot be deleted keeps its row for the next pass.
        f.db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE projects SET is_deleted = TRUE WHERE id = $1",
            [f.project_id.into()],
        ))
        .await
        .expect("delete project");
        f.store.failing_deletes.store(true, Ordering::SeqCst);
        let blocked = f
            .service
            .sweep(f.store.as_ref(), Utc::now())
            .await
            .expect("sweep");
        assert_eq!(blocked.deleted_projects, 0, "{blocked:?}");
        assert_eq!(f.store.keys(f.project_id).len(), 1);
        f.store.failing_deletes.store(false, Ordering::SeqCst);
        let swept = f
            .service
            .sweep(f.store.as_ref(), Utc::now())
            .await
            .expect("sweep");
        assert_eq!(swept.deleted_projects, 1, "{swept:?}");
        assert!(f.store.keys(f.project_id).is_empty());
        let usage = f.service.usage(f.project_id).await.expect("usage");
        assert_eq!((usage.used_bytes, usage.files), (0, 0));
    }

    #[tokio::test]
    async fn without_blob_storage_usage_says_what_is_missing_and_ingest_drops() {
        let Some(f) = fixture("attachment usage test").await else {
            return;
        };
        let bare = AttachmentService::new(f.db.clone());
        let usage = bare.usage(f.project_id).await.expect("usage");
        assert!(!usage.configured);
        assert_eq!(usage.setup_path.as_deref(), Some(SETUP_PATH));
        assert!(usage
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("No blob storage")));
        let offered = bare.offer(batch(&f, SDK_EVENT, vec![file("a.txt", b"a")]));
        assert_eq!((offered.queued, offered.dropped), (0, 1));

        f.store.failing_deletes.store(false, Ordering::SeqCst);
        let configured = f.service.usage(f.project_id).await.expect("usage");
        assert!(configured.configured && configured.setup_path.is_none());
    }
}
