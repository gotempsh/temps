// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use aws_sdk_s3::Client as S3Client;
use serde_json::Value;

use temps_backup_core::engine_v2::BackupError;

const WROTE_BACKUP_MARKER: &str = "Wrote backup with name ";

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
    let body = response
        .body
        .collect()
        .await
        .map_err(|error| BackupError::Failed {
            reason: format!(
                "failed to read WAL-G stop-sentinel body s3://{bucket}/{sentinel_key}: {error}"
            ),
        })?;
    size_from_sentinel(
        &body.into_bytes(),
        backup_uuid,
        &format!("s3://{bucket}/{sentinel_key}"),
    )
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
    fn extracts_backup_name_from_walg_stderr() {
        let stderr = concat!(
            "INFO: 2026/09/22 10:00:00 Started backup with name base_0001 at LSN 0/100\n",
            "INFO: 2026/09/22 10:00:01 Wrote backup with name base_0001 to storage default\n",
        );

        assert_eq!(backup_name_from_push_output("", stderr), Some("base_0001"));
    }

    #[test]
    fn extracts_delta_backup_name() {
        let stdout = "Wrote backup with name base_0002_D_0001 to storage default\n";

        assert_eq!(
            backup_name_from_push_output(stdout, ""),
            Some("base_0002_D_0001")
        );
    }

    #[test]
    fn rejects_missing_or_unsafe_backup_names() {
        assert_eq!(backup_name_from_push_output("backup complete", ""), None);
        assert_eq!(
            backup_name_from_push_output("Wrote backup with name ../../sentinel", ""),
            None
        );
    }

    #[test]
    fn reads_current_backups_compressed_size_from_sentinel() {
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
    fn rejects_another_backups_sentinel() {
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
    fn rejects_negative_or_missing_compressed_size() {
        for sentinel in [
            br#"{"CompressedSize":-1,"UserData":{"temps_backup_id":"backup-current"}}"#.as_slice(),
            br#"{"UserData":{"temps_backup_id":"backup-current"}}"#.as_slice(),
        ] {
            assert!(size_from_sentinel(sentinel, "backup-current", "test-sentinel").is_err());
        }
    }
}
