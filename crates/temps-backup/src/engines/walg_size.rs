// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use aws_sdk_s3::Client as S3Client;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use temps_backup_core::engine_v2::BackupError;

const WROTE_BACKUP_MARKER: &str = "Wrote backup with name ";
const MAX_SENTINEL_SIZE_BYTES: usize = 1024 * 1024;

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
    let backup_name =
        backup_name_from_push_output(stdout, stderr).ok_or_else(|| BackupError::Failed {
            reason: "wal-g backup-push succeeded but did not report the written backup name"
                .to_string(),
        })?;
    let sentinel_key = format!(
        "{}basebackups_005/{}_backup_stop_sentinel.json",
        walg_key_prefix, backup_name
    );

    let response = client
        .get_object()
        .bucket(bucket)
        .key(&sentinel_key)
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
    size_from_sentinel(&body, backup_uuid, &format!("s3://{bucket}/{sentinel_key}"))
}

fn size_from_sentinel(
    sentinel_bytes: &[u8],
    backup_uuid: &str,
    location: &str,
) -> Result<i64, BackupError> {
    let sentinel: Value =
        serde_json::from_slice(sentinel_bytes).map_err(|error| BackupError::Failed {
            reason: format!("WAL-G stop-sentinel {location} is invalid JSON: {error}"),
        })?;

    let sentinel_backup_uuid = sentinel
        .get("UserData")
        .and_then(|user_data| user_data.get("temps_backup_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| BackupError::Failed {
            reason: format!("WAL-G stop-sentinel {location} has no Temps backup identity"),
        })?;
    if sentinel_backup_uuid != backup_uuid {
        return Err(BackupError::Failed {
            reason: format!(
                "WAL-G stop-sentinel {location} belongs to backup '{sentinel_backup_uuid}', \
                 expected '{backup_uuid}'"
            ),
        });
    }

    sentinel
        .get("CompressedSize")
        .and_then(Value::as_i64)
        .filter(|size| *size >= 0)
        .ok_or_else(|| BackupError::Failed {
            reason: format!("WAL-G stop-sentinel {location} has no valid CompressedSize"),
        })
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
