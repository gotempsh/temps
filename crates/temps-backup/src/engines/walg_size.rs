// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use aws_sdk_s3::Client as S3Client;
use serde::Deserialize;
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::execution::engine_v2::BackupError;

const WROTE_BACKUP_MARKER: &str = "Wrote backup with name ";
const MAX_SENTINEL_SIZE_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
struct WalgStopSentinel {
    #[serde(rename = "CompressedSize")]
    compressed_size: i64,
    #[serde(rename = "UserData")]
    user_data: WalgSentinelUserData,
}

#[derive(Deserialize)]
struct WalgSentinelUserData {
    temps_backup_id: String,
}

/// Read the compressed size WAL-G recorded for the backup that just finished.
///
/// `backup-push` writes the new backup name to its output. That lets us fetch
/// one stop-sentinel instead of summing (or retaining snapshots of) the whole
/// repository, which also contains every prior base backup and archived WAL.
pub(crate) async fn load_backup_size_bytes(
    client: &S3Client,
    bucket: &str,
    walg_key_prefix: &str,
    stdout: &str,
    stderr: &str,
    backup_uuid: &str,
) -> Result<i64, BackupError> {
    let backup_name = pushed_snapshot_name(stdout, stderr)?;
    let sentinel_key = format!(
        "{}basebackups_005/{}_backup_stop_sentinel.json",
        walg_key_prefix, backup_name
    );

    let body = read_sentinel(client, bucket, &sentinel_key).await?;
    size_from_sentinel(&body, backup_uuid, &format!("s3://{bucket}/{sentinel_key}"))
}

/// How many stop sentinels are read from S3 at once. A repository can hold
/// hundreds of base backups; one at a time made a lookup hundreds of
/// sequential round trips. Each read holds at most one sentinel body
/// (`MAX_SENTINEL_SIZE_BYTES`), so this also bounds the memory in flight.
pub(crate) const SENTINEL_READ_CONCURRENCY: usize = 8;

/// What a stop sentinel records about its base backup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SentinelSummary {
    /// The `temps_backup_id` in the sentinel's user data. Legacy or externally
    /// written snapshots may carry arbitrary user data, or none.
    pub backup_id: Option<String>,
    /// The compressed size of the base backup (`CompressedSize`), the same
    /// figure a tracked backup records as its size. `None` when absent or
    /// invalid, so an odd size never hides the backup's identity.
    pub compressed_size: Option<i64>,
}

/// Why a stop sentinel told us nothing.
#[derive(Debug, Error)]
pub(crate) enum SentinelReadError {
    #[error("Could not read")]
    Unreadable(#[source] BackupError),
    #[error("Could not parse")]
    Malformed(#[source] serde_json::Error),
}

pub(crate) fn sentinel_summary(bytes: &[u8]) -> Result<SentinelSummary, serde_json::Error> {
    #[derive(Deserialize)]
    struct Marker {
        #[serde(rename = "UserData")]
        user_data: Option<serde_json::Value>,
        #[serde(rename = "CompressedSize")]
        compressed_size: Option<serde_json::Value>,
    }
    let marker: Marker = serde_json::from_slice(bytes)?;
    Ok(SentinelSummary {
        backup_id: marker
            .user_data
            .and_then(|data| data.get("temps_backup_id")?.as_str().map(str::to_owned)),
        compressed_size: marker
            .compressed_size
            .and_then(|size| size.as_i64())
            .filter(|size| *size >= 0),
    })
}

/// Read and summarize the stop sentinels `keys` name, up to
/// [`SENTINEL_READ_CONCURRENCY`] at a time, in the order given. Only the
/// summaries are kept, never the bodies.
pub(crate) async fn summarize_sentinels(
    client: &S3Client,
    bucket: &str,
    keys: Vec<String>,
) -> Vec<(String, Result<SentinelSummary, SentinelReadError>)> {
    use futures::StreamExt;
    futures::stream::iter(keys)
        .map(|key| async move {
            let summary = match read_sentinel(client, bucket, &key).await {
                Ok(bytes) => sentinel_summary(&bytes).map_err(SentinelReadError::Malformed),
                Err(error) => Err(SentinelReadError::Unreadable(error)),
            };
            (key, summary)
        })
        .buffered(SENTINEL_READ_CONCURRENCY)
        .collect()
        .await
}

/// Only an exact UUID in a valid stop sentinel can identify a tracked snapshot.
///
/// Every sentinel in the repository is read, concurrently, because two can
/// carry the same UUID (a retried backup-push) and the lookup must refuse to
/// guess between them.
pub(crate) async fn find_snapshot(
    client: &S3Client,
    bucket: &str,
    repository_key: &str,
    backup_id: &str,
) -> Result<Option<String>, BackupError> {
    let prefix = format!("{}/basebackups_005/", repository_key.trim_end_matches('/'));
    let mut pages = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(&prefix)
        .delimiter("/")
        .into_paginator()
        .send();
    let mut keys = Vec::new();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|error| BackupError::Failed {
            reason: format!("Could not list {prefix} for backup {backup_id}: {error}"),
        })?;
        keys.extend(
            page.contents()
                .iter()
                .filter_map(|object| object.key())
                .filter(|key| temps_providers::externalsvc::postgres::walg_snapshot(key).is_some())
                .map(str::to_owned),
        );
    }
    let summaries = summarize_sentinels(client, bucket, keys).await;
    pick_snapshot(bucket, &prefix, backup_id, summaries)
}

/// Choose the one snapshot whose sentinel names `backup_id`.
///
/// A match is returned even when other sentinels could not be read: the
/// unreadable ones are logged, not fatal. With no match, an unreadable
/// sentinel is an error, because the backup may be the one that failed.
fn pick_snapshot(
    bucket: &str,
    prefix: &str,
    backup_id: &str,
    summaries: Vec<(String, Result<SentinelSummary, SentinelReadError>)>,
) -> Result<Option<String>, BackupError> {
    let total = summaries.len();
    let mut matched = Vec::new();
    let mut unreadable = Vec::new();
    for (key, summary) in summaries {
        match summary {
            Ok(summary) if summary.backup_id.as_deref() == Some(backup_id) => matched.push(key),
            Ok(_) => {}
            Err(SentinelReadError::Malformed(error)) => {
                tracing::warn!(key, backup_id, %error, "Skipping malformed WAL-G stop sentinel")
            }
            Err(SentinelReadError::Unreadable(error)) => {
                tracing::warn!(key, backup_id, %error, "Skipping unreadable WAL-G stop sentinel");
                unreadable.push(error);
            }
        }
    }
    match matched.as_slice() {
        [_, _, ..] => Err(BackupError::PermanentFailure {
            reason: format!(
                "Multiple WAL-G snapshots identify backup {backup_id} in {prefix} ({}). Select the individual snapshot from S3.",
                matched.join(", ")
            ),
        }),
        [key] => {
            if !unreadable.is_empty() {
                tracing::warn!(
                    backup_id,
                    key,
                    unreadable = unreadable.len(),
                    total,
                    "Found the WAL-G snapshot for a tracked backup; some other stop sentinels in its repository could not be read"
                );
            }
            Ok(Some(format!("s3://{bucket}/{key}")))
        }
        [] => {
            let failed = unreadable.len();
            match unreadable.into_iter().next() {
                Some(first) => Err(BackupError::Failed {
                    reason: format!(
                        "No readable WAL-G stop sentinel in {prefix} identifies backup {backup_id}, and {failed} of {total} could not be read, so the backup may be one of them. First failure: {first}"
                    ),
                }),
                None => Ok(None),
            }
        }
    }
}

/// When WAL-G started the base backup a stop sentinel describes. WAL-G
/// records `StartTime` in its version-2 sentinel; older sentinels omit it, and
/// Go's zero time means it was never set. Either is `None`: unknown, not early.
pub(crate) fn sentinel_start_time(
    bytes: &[u8],
) -> Result<Option<chrono::DateTime<chrono::Utc>>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Started {
        #[serde(rename = "StartTime")]
        start_time: Option<chrono::DateTime<chrono::Utc>>,
    }
    let started: Started = serde_json::from_slice(bytes)?;
    Ok(started
        .start_time
        .filter(|time| chrono::Datelike::year(time) > 1))
}

/// Read the start time of the base backup whose stop sentinel is
/// `sentinel_key` in `bucket`. `Ok(None)` when the sentinel does not record it.
pub(crate) async fn load_snapshot_start_time(
    client: &S3Client,
    bucket: &str,
    sentinel_key: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, BackupError> {
    let bytes = read_sentinel(client, bucket, sentinel_key).await?;
    sentinel_start_time(&bytes).map_err(|error| BackupError::Failed {
        reason: format!(
            "WAL-G stop-sentinel s3://{bucket}/{sentinel_key} is not valid JSON: {error}"
        ),
    })
}

pub(crate) async fn read_sentinel(
    client: &S3Client,
    bucket: &str,
    sentinel_key: &str,
) -> Result<Vec<u8>, BackupError> {
    let response = client
        .get_object()
        .bucket(bucket)
        .key(sentinel_key)
        .send()
        .await
        .map_err(|error| BackupError::Failed {
            reason: format!(
                "failed to read WAL-G stop-sentinel s3://{bucket}/{sentinel_key}: {error}"
            ),
        })?;
    if response.content_length().is_some_and(|content_length| {
        content_length > i64::try_from(MAX_SENTINEL_SIZE_BYTES).unwrap_or(i64::MAX)
    }) {
        return Err(BackupError::Failed {
            reason: format!(
                "WAL-G stop-sentinel s3://{bucket}/{sentinel_key} exceeds the {} byte limit",
                MAX_SENTINEL_SIZE_BYTES
            ),
        });
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|content_length| usize::try_from(content_length).ok())
            .unwrap_or_default()
            .min(MAX_SENTINEL_SIZE_BYTES),
    );
    response
        .body
        .into_async_read()
        .take((MAX_SENTINEL_SIZE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .await
        .map_err(|error| BackupError::Failed {
            reason: format!(
                "failed to read WAL-G stop-sentinel body s3://{bucket}/{sentinel_key}: {error}"
            ),
        })?;
    if body.len() > MAX_SENTINEL_SIZE_BYTES {
        return Err(BackupError::Failed {
            reason: format!(
                "WAL-G stop-sentinel s3://{bucket}/{sentinel_key} exceeds the {} byte limit",
                MAX_SENTINEL_SIZE_BYTES
            ),
        });
    }
    Ok(body)
}

fn size_from_sentinel(
    sentinel_bytes: &[u8],
    backup_uuid: &str,
    location: &str,
) -> Result<i64, BackupError> {
    let sentinel: WalgStopSentinel =
        serde_json::from_slice(sentinel_bytes).map_err(|error| BackupError::Failed {
            reason: format!("WAL-G stop-sentinel {location} is invalid JSON: {error}"),
        })?;

    let sentinel_backup_uuid = sentinel.user_data.temps_backup_id;
    if sentinel_backup_uuid != backup_uuid {
        return Err(BackupError::Failed {
            reason: format!(
                "WAL-G stop-sentinel {location} belongs to backup '{sentinel_backup_uuid}', \
                 expected '{backup_uuid}'"
            ),
        });
    }

    if sentinel.compressed_size < 0 {
        Err(BackupError::Failed {
            reason: format!("WAL-G stop-sentinel {location} has no valid CompressedSize"),
        })
    } else {
        Ok(sentinel.compressed_size)
    }
}

pub(crate) fn pushed_snapshot_name<'a>(
    stdout: &'a str,
    stderr: &'a str,
) -> Result<&'a str, BackupError> {
    backup_name_from_push_output(stdout, stderr)
        .filter(|name| name.starts_with("base_"))
        .ok_or_else(|| BackupError::Failed {
            reason: "wal-g backup-push succeeded but did not report a valid base backup name"
                .into(),
        })
}

/// Expand the per-attempt name recorded alongside the shared repository.
pub(crate) fn recorded_location(repository: &str, metadata: &serde_json::Value) -> Option<String> {
    let name = metadata.get("walg_snapshot_name")?.as_str()?;
    let location = format!(
        "{}/basebackups_005/{name}_backup_stop_sentinel.json",
        repository.trim_end_matches('/')
    );
    let (root, parsed) = temps_providers::externalsvc::postgres::walg_snapshot(&location)?;
    (root == repository.trim_end_matches('/') && parsed == name).then_some(location)
}

fn backup_name_from_push_output<'a>(stdout: &'a str, stderr: &'a str) -> Option<&'a str> {
    stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| {
            line.split_once(WROTE_BACKUP_MARKER)
                .map(|(_, suffix)| suffix)
        })
        .filter_map(|suffix| suffix.split_whitespace().next())
        .find(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_with_one_uuid_keep_the_completed_attempt_location() -> Result<(), BackupError> {
        let repo = "s3://backups-bucket/service-a/walg";
        for name in ["base_001", "base_002"] {
            let output = format!("Wrote backup with name {name} to storage default");
            let name = pushed_snapshot_name("", &output)?;
            let metadata =
                serde_json::json!({"walg_snapshot_name": name, "temps_backup_id":"backup-a"});
            assert_eq!(
                recorded_location(repo, &metadata),
                Some(format!(
                    "{repo}/basebackups_005/{name}_backup_stop_sentinel.json"
                ))
            );
        }
        Ok(())
    }

    #[test]
    fn test_backup_name_from_push_output_stderr_returns_backup_name() {
        let stderr = concat!(
            "INFO: 2026/09/22 10:00:00 Started backup with name base_0001 at LSN 0/100\n",
            "INFO: 2026/09/22 10:00:01 Wrote backup with name base_0001 to storage default\n",
        );

        assert_eq!(backup_name_from_push_output("", stderr), Some("base_0001"));
    }

    #[test]
    fn test_backup_name_from_push_output_delta_name_returns_backup_name() {
        let stdout = "Wrote backup with name base_0002_D_0001 to storage default\n";

        assert_eq!(
            backup_name_from_push_output(stdout, ""),
            Some("base_0002_D_0001")
        );
    }

    #[test]
    fn test_backup_name_from_push_output_missing_or_unsafe_name_returns_none() {
        assert_eq!(backup_name_from_push_output("backup complete", ""), None);
        assert_eq!(
            backup_name_from_push_output("Wrote backup with name ../../sentinel", ""),
            None
        );
    }

    #[test]
    fn test_size_from_sentinel_current_backup_returns_compressed_size() {
        let sentinel = br#"{
            "CompressedSize": 4194304,
            "UserData": {"temps_backup_id": "backup-current"}
        }"#;

        assert_eq!(
            size_from_sentinel(sentinel, "backup-current", "test-sentinel").ok(),
            Some(4_194_304)
        );
    }

    #[test]
    fn test_size_from_sentinel_other_backup_returns_error() {
        let sentinel = br#"{
            "CompressedSize": 4194304,
            "UserData": {"temps_backup_id": "backup-previous"}
        }"#;

        let error = size_from_sentinel(sentinel, "backup-current", "test-sentinel")
            .expect_err("a different backup identity must not supply the recorded size");

        assert!(error.to_string().contains("backup-previous"));
        assert!(error.to_string().contains("backup-current"));
    }

    #[test]
    fn test_size_from_sentinel_invalid_fields_return_error() {
        for sentinel in [
            br#"{"CompressedSize":-1,"UserData":{"temps_backup_id":"backup-current"}}"#.as_slice(),
            br#"{"UserData":{"temps_backup_id":"backup-current"}}"#.as_slice(),
            br#"{"CompressedSize":"12","UserData":{"temps_backup_id":"backup-current"}}"#
                .as_slice(),
            br#"{"CompressedSize":12,"UserData":{}}"#.as_slice(),
            br#"not-json"#.as_slice(),
        ] {
            assert!(size_from_sentinel(sentinel, "backup-current", "test-sentinel").is_err());
        }
    }

    #[test]
    fn test_size_from_sentinel_zero_size_returns_zero() {
        let sentinel = br#"{"CompressedSize":0,"UserData":{"temps_backup_id":"backup-current"}}"#;

        assert_eq!(
            size_from_sentinel(sentinel, "backup-current", "test-sentinel").ok(),
            Some(0)
        );
    }

    async fn spawn_s3_stub(
        body: Vec<u8>,
    ) -> Option<(S3Client, tokio::sync::oneshot::Receiver<String>)> {
        use axum::{
            extract::OriginalUri,
            http::{header, StatusCode},
            response::IntoResponse,
            routing::get,
            Router,
        };

        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping S3 sentinel stub test");
                return None;
            }
            Err(error) => panic!("bind S3 sentinel stub: {error}"),
        };
        let address = listener.local_addr().expect("S3 sentinel stub address");
        let (path_sender, path_receiver) = tokio::sync::oneshot::channel();
        let path_sender = std::sync::Arc::new(std::sync::Mutex::new(Some(path_sender)));
        let app = Router::new().route(
            "/{bucket}/{*key}",
            get({
                let path_sender = path_sender.clone();
                move |OriginalUri(uri): OriginalUri| {
                    let path_sender = path_sender.clone();
                    let body = body.clone();
                    async move {
                        if let Some(sender) = path_sender.lock().expect("path sender lock").take() {
                            let _ = sender.send(uri.path().to_string());
                        }
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json")],
                            body,
                        )
                            .into_response()
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve S3 sentinel stub");
        });

        let config = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .force_path_style(true)
            .endpoint_url(format!("http://{address}"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test",
                "test",
                None,
                None,
                "walg-size-test",
            ))
            .build();
        Some((S3Client::from_conf(config), path_receiver))
    }

    #[tokio::test]
    async fn test_load_backup_size_bytes_current_sentinel_fetches_exact_key() {
        let body = br#"{
            "CompressedSize": 8192,
            "UserData": {"temps_backup_id": "backup-current"}
        }"#
        .to_vec();
        let Some((client, path_receiver)) = spawn_s3_stub(body).await else {
            return;
        };

        let size = load_backup_size_bytes(
            &client,
            "backups",
            "tenant/postgres/service/walg/",
            "",
            "INFO: Wrote backup with name base_0001 to storage default",
            "backup-current",
        )
        .await
        .expect("current sentinel should supply its compressed size");

        assert_eq!(size, 8192);
        assert_eq!(
            path_receiver.await.expect("captured S3 request path"),
            "/backups/tenant/postgres/service/walg/basebackups_005/base_0001_backup_stop_sentinel.json"
        );
    }

    #[test]
    fn sentinel_summary_reads_identity_and_size_and_ignores_an_invalid_size() {
        let summary = sentinel_summary(
            br#"{"CompressedSize":52428800,"UserData":{"temps_backup_id":"backup-a"}}"#,
        )
        .expect("valid sentinel");
        assert_eq!(summary.backup_id.as_deref(), Some("backup-a"));
        assert_eq!(summary.compressed_size, Some(52_428_800));

        for sentinel in [
            br#"{"CompressedSize":-1,"UserData":{"temps_backup_id":"backup-a"}}"#.as_slice(),
            br#"{"CompressedSize":"12","UserData":{"temps_backup_id":"backup-a"}}"#.as_slice(),
            br#"{"UserData":{"temps_backup_id":"backup-a"}}"#.as_slice(),
        ] {
            let summary = sentinel_summary(sentinel).expect("valid sentinel");
            assert_eq!(summary.backup_id.as_deref(), Some("backup-a"));
            assert_eq!(summary.compressed_size, None);
        }
        assert_eq!(
            sentinel_summary(br#"{"CompressedSize":1}"#).expect("valid"),
            SentinelSummary {
                backup_id: None,
                compressed_size: Some(1)
            }
        );
        assert!(sentinel_summary(b"not json").is_err());
    }

    fn summary_of(
        key: &str,
        backup_id: &str,
    ) -> (String, Result<SentinelSummary, SentinelReadError>) {
        (
            key.to_string(),
            Ok(SentinelSummary {
                backup_id: Some(backup_id.to_string()),
                compressed_size: None,
            }),
        )
    }

    fn unreadable_summary(key: &str) -> (String, Result<SentinelSummary, SentinelReadError>) {
        (
            key.to_string(),
            Err(SentinelReadError::Unreadable(BackupError::Failed {
                reason: format!("AccessDenied reading {key}"),
            })),
        )
    }

    #[test]
    fn pick_snapshot_returns_a_match_despite_unreadable_sentinels() {
        let picked = pick_snapshot(
            "backups",
            "svc/walg/basebackups_005/",
            "wanted",
            vec![
                unreadable_summary("svc/walg/basebackups_005/base_0001_backup_stop_sentinel.json"),
                summary_of(
                    "svc/walg/basebackups_005/base_0002_backup_stop_sentinel.json",
                    "wanted",
                ),
            ],
        )
        .expect("a found snapshot is returned even when another sentinel is unreadable");
        assert_eq!(
            picked.as_deref(),
            Some("s3://backups/svc/walg/basebackups_005/base_0002_backup_stop_sentinel.json")
        );
    }

    #[test]
    fn pick_snapshot_without_a_match_fails_only_when_a_sentinel_was_unreadable() {
        let error = pick_snapshot(
            "backups",
            "svc/walg/basebackups_005/",
            "wanted",
            vec![
                unreadable_summary("svc/walg/basebackups_005/base_0001_backup_stop_sentinel.json"),
                summary_of(
                    "svc/walg/basebackups_005/base_0002_backup_stop_sentinel.json",
                    "other",
                ),
            ],
        )
        .expect_err("the backup may be the unreadable one");
        let message = error.to_string();
        assert!(message.contains("1 of 2 could not be read"), "{message}");
        assert!(message.contains("wanted"), "{message}");

        assert_eq!(
            pick_snapshot(
                "backups",
                "svc/walg/basebackups_005/",
                "wanted",
                vec![summary_of(
                    "svc/walg/basebackups_005/base_0002_backup_stop_sentinel.json",
                    "other"
                )],
            )
            .expect("every sentinel was read"),
            None
        );
    }

    #[test]
    fn pick_snapshot_refuses_two_snapshots_with_one_identity() {
        let error = pick_snapshot(
            "backups",
            "svc/walg/basebackups_005/",
            "wanted",
            vec![
                summary_of(
                    "svc/walg/basebackups_005/base_0001_backup_stop_sentinel.json",
                    "wanted",
                ),
                summary_of(
                    "svc/walg/basebackups_005/base_0002_backup_stop_sentinel.json",
                    "wanted",
                ),
            ],
        )
        .expect_err("two snapshots claim the backup");
        assert!(matches!(error, BackupError::PermanentFailure { .. }));
        assert!(error.to_string().contains("base_0001"));
        assert!(error.to_string().contains("base_0002"));
    }

    /// A repository of 16 snapshots, one of which carries the wanted UUID and
    /// one of which is unreadable. The lookup reads sentinels several at a
    /// time (never more than the limit) and still returns the match (#2030).
    #[tokio::test]
    async fn find_snapshot_reads_sentinels_concurrently_and_skips_unreadable_ones() {
        use axum::{
            extract::{OriginalUri, Query},
            http::StatusCode,
            response::IntoResponse,
            Router,
        };
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping concurrent sentinel test");
                return;
            }
            Err(error) => panic!("bind S3 stub: {error}"),
        };
        let address = listener.local_addr().expect("S3 stub address");
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback({
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            move |OriginalUri(uri): OriginalUri, Query(query): Query<HashMap<String, String>>| {
                let in_flight = in_flight.clone();
                let max_in_flight = max_in_flight.clone();
                async move {
                    if uri.path() == "/backups/" {
                        let prefix = query.get("prefix").cloned().unwrap_or_default();
                        let contents: String = (1..=16)
                            .map(|n| {
                                format!("<Contents><Key>{prefix}base_{n:04}_backup_stop_sentinel.json</Key></Contents>")
                            })
                            .collect();
                        return (
                            StatusCode::OK,
                            format!("<ListBucketResult>{contents}</ListBucketResult>"),
                        )
                            .into_response();
                    }
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    max_in_flight.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let path = uri.path();
                    if path.contains("base_0003_") {
                        return (
                            StatusCode::FORBIDDEN,
                            "<Error><Code>AccessDenied</Code></Error>".to_string(),
                        )
                            .into_response();
                    }
                    let id = if path.contains("base_0011_") { "wanted" } else { "other" };
                    (
                        StatusCode::OK,
                        format!(r#"{{"CompressedSize":10,"UserData":{{"temps_backup_id":"{id}"}}}}"#),
                    )
                        .into_response()
                }
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .force_path_style(true)
            .endpoint_url(format!("http://{address}"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test",
                "test",
                None,
                None,
                "walg-size-test",
            ))
            .build();
        let client = S3Client::from_conf(config);

        let found = find_snapshot(&client, "backups", "svc/walg", "wanted").await;
        server.abort();

        assert_eq!(
            found.expect("the match is returned despite base_0003 being unreadable"),
            Some(
                "s3://backups/svc/walg/basebackups_005/base_0011_backup_stop_sentinel.json"
                    .to_string()
            )
        );
        let max = max_in_flight.load(Ordering::SeqCst);
        assert!(max > 1, "sentinels were read one at a time");
        assert!(
            max <= SENTINEL_READ_CONCURRENCY,
            "{max} reads in flight exceeds the limit of {SENTINEL_READ_CONCURRENCY}"
        );
    }

    #[test]
    fn sentinel_start_time_is_read_and_unknown_when_absent_or_unset() {
        let v2 = br#"{"StartTime":"2026-11-22T03:50:53.123456Z","UserData":{}}"#;
        assert_eq!(
            sentinel_start_time(v2).expect("valid sentinel"),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-11-22T03:50:53.123456Z")
                    .expect("timestamp")
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(
            sentinel_start_time(br#"{"UserData":{}}"#).expect("valid"),
            None
        );
        assert_eq!(
            sentinel_start_time(br#"{"StartTime":"0001-01-01T00:00:00Z"}"#).expect("valid"),
            None
        );
        assert!(sentinel_start_time(b"not json").is_err());
    }

    #[tokio::test]
    async fn snapshot_start_time_reads_the_selected_sentinel() {
        let body = br#"{"StartTime":"2026-11-22T03:50:53Z","CompressedSize":1}"#.to_vec();
        let Some((client, path_receiver)) = spawn_s3_stub(body).await else {
            return;
        };
        let started = load_snapshot_start_time(
            &client,
            "backups-bucket",
            "service-a/walg/basebackups_005/base_0001_backup_stop_sentinel.json",
        )
        .await
        .expect("sentinel should be read");
        assert_eq!(
            started.map(|time| time.to_rfc3339()),
            Some("2026-11-22T03:50:53+00:00".to_string())
        );
        assert_eq!(
            path_receiver.await.expect("captured S3 request path"),
            "/backups-bucket/service-a/walg/basebackups_005/base_0001_backup_stop_sentinel.json"
        );
    }

    #[tokio::test]
    async fn test_load_backup_size_bytes_oversized_sentinel_returns_error() {
        let Some((client, _)) = spawn_s3_stub(vec![b'x'; MAX_SENTINEL_SIZE_BYTES + 1]).await else {
            return;
        };

        let error = load_backup_size_bytes(
            &client,
            "backups",
            "tenant/postgres/service/walg/",
            "Wrote backup with name base_0001 to storage default",
            "",
            "backup-current",
        )
        .await
        .expect_err("oversized sentinel must be rejected before JSON parsing");

        assert!(error.to_string().contains("exceeds the 1048576 byte limit"));
    }
}
