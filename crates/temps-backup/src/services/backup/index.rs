// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The S3 backup index, backup metadata, orphan scans and listing the backups
//! stored in a source.

use super::*;

/// Backups visible in an S3 source and any failure encountered scanning it.
#[derive(Debug, Serialize, Deserialize)]
pub struct SourceBackupIndex {
    pub backups: Vec<SourceBackupIndexEntry>,
    pub last_updated: String,
    pub scan_errors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SourceBackupIndexEntry {
    pub id: i32,
    pub backup_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub backup_type: String,
    pub created_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    pub size_bytes: Option<i64>,
    pub location: String,
    pub metadata_location: String,
    pub engine: Option<String>,
    pub origin_service_name: Option<String>,
    pub format: Option<String>,
    pub source: String,
    pub state: String,
}

/// Classify a backup location into one of the known storage formats.
/// Returns `None` for non-postgres / unknown locations so the UI can show
/// a neutral badge without guessing.
///
/// The `engine` hint is used to disambiguate formats that share location
/// shapes. Object-store backups (s3/rustfs/blob/minio) are always a
/// bucket-to-bucket mirror — their path has no extension — so we tag them
/// `"mirror"` when the engine identifies them as such. MariaDB's logical
/// dump shares the `.sql.gz` suffix with Postgres's legacy pg_dump, so it
/// needs the hint too; its physical base (`base.mbstream.gz`) does not.
fn classify_backup_format(location: &str, engine: Option<&str>) -> Option<String> {
    if location.is_empty() {
        return None;
    }
    // Engine-first: object-store backups are always mc-mirror dumps regardless
    // of location shape. No extension-based inference applies.
    if let Some(e) = engine {
        let e = e.to_ascii_lowercase();
        if matches!(e.as_str(), "s3" | "rustfs" | "blob" | "minio") {
            return Some("mirror".to_string());
        }
    }
    // Extension-based classification runs first — it's unambiguous when
    // the file suffix is present, regardless of whether the location is
    // an s3:// URL or a bare key.
    //
    // MariaDB's physical base carries a suffix no other engine produces, so
    // it needs no engine hint.
    if location.ends_with(".mbstream.gz") {
        return Some("mariadb_physical".to_string());
    }
    // `.sql.gz` is the ONE genuinely ambiguous suffix: Postgres's legacy
    // pg_dump and MariaDB's logical `dump.sql.gz` share it. Filenames can't
    // separate them, so the engine hint decides — checked BEFORE the generic
    // Postgres branch, which would otherwise label every MariaDB dump
    // `pg_dump` and send the restore planner down the pg_restore path.
    if location.ends_with(".sql.gz") && engine.is_some_and(|e| e.eq_ignore_ascii_case("mariadb")) {
        return Some("mariadb_dump".to_string());
    }
    if location.ends_with(".sql.gz") || location.ends_with(".pgdump.gz") {
        return Some("pg_dump".to_string());
    }
    if location.ends_with(".rdb.gz") {
        return Some("rdb".to_string());
    }
    if location.ends_with(".bson.gz") || location.ends_with(".archive") {
        return Some("mongodump".to_string());
    }
    // WAL-G backups are uploaded by the wal-g binary as a *prefix*, not a
    // single file. The orchestrator records the WAL-G root prefix
    // (e.g. `s3://bucket/external_services/postgres/svc/walg`) as the
    // backup's location. Match by path segment, NOT by `s3://` prefix —
    // pg_dump / mongodump / rdb backups also live under `s3://...` and
    // would otherwise get misclassified as walg.
    let trimmed = location.trim_end_matches('/');
    if trimmed.ends_with("/walg") || trimmed.contains("/walg/") {
        return Some("walg".to_string());
    }
    None
}

#[derive(Debug, Error)]
#[error("Could not list backup objects under {prefix} ({reason})")]
struct ScanListError {
    prefix: String,
    reason: String,
}

impl ScanListError {
    fn from_request(
        prefix: &str,
        error: aws_sdk_s3::error::SdkError<
            aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error,
        >,
    ) -> Self {
        Self {
            prefix: prefix.to_owned(),
            reason: error
                .as_service_error()
                .and_then(|service| service.code())
                .unwrap_or("S3 request failed")
                .to_owned(),
        }
    }
}

/// Walk the S3 source's `external_services/` prefix to find backups that
/// aren't represented in the local DB (e.g., backups produced by a
/// previous Temps instance). Returns entries tagged with `source: "s3_scan"`.
///
/// Paths we recognize (written by the backup pipeline):
/// - `external_services/<engine>/<service>/<YYYY>/<MM>/<DD>/*.sql.gz`
///   and `*.pgdump.gz` (pg_dump legacy)
/// - `external_services/<engine>/<service>/walg/basebackups_005/*_backup_stop_sentinel.json`
///   (WAL-G marker objects)
async fn scan_s3_for_orphan_backups(
    s3_client: &aws_sdk_s3::Client,
    s3_source: &temps_entities::s3_sources::Model,
    seen_locations: &std::collections::HashSet<String>,
    entries: &mut Vec<SourceBackupIndexEntry>,
    scan_errors: &mut Vec<String>,
) -> Result<(), ScanListError> {
    let bucket = &s3_source.bucket_name;
    let prefix = build_s3_key(&s3_source.bucket_path, "external_services/");

    // First-level list: `external_services/<engine>/`. We use delimiter
    // '/' so we only get the top-level engine directories (CommonPrefixes).
    let engine_prefixes: Vec<String> = list_common_prefixes(s3_client, bucket, &prefix).await?;

    let by_id: std::collections::HashMap<_, _> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.backup_id.clone(), index))
        .collect();
    let missing_locations = entries.iter().any(|entry| entry.location.is_empty());

    for engine_prefix in engine_prefixes {
        let engine = extract_trailing_segment(&engine_prefix);
        // Second-level list: `external_services/<engine>/<service>/`
        let service_prefixes = list_common_prefixes(s3_client, bucket, &engine_prefix).await?;

        for service_prefix in service_prefixes {
            let service_name = extract_trailing_segment(&service_prefix);

            // Look for a WAL-G backup under `<service_prefix>walg/`.
            let walg_prefix = format!("{}walg/", service_prefix);
            let walg_sentinels = list_walg_sentinels(s3_client, bucket, &walg_prefix).await?;
            for (sentinel, last_modified, _) in walg_sentinels {
                let repository = format!("s3://{}/{}", bucket, walg_prefix.trim_end_matches('/'));
                let location = if engine == "postgres" {
                    let location = format!("s3://{bucket}/{sentinel}");
                    if missing_locations || seen_locations.contains(&repository) {
                        let identity =
                            crate::engines::walg_size::read_sentinel(s3_client, bucket, &sentinel)
                                .await
                                .map_err(|_| "Could not read")
                                .and_then(|bytes| {
                                    crate::engines::walg_size::sentinel_backup_id(&bytes)
                                        .map_err(|_| "Could not parse")
                                });
                        match identity {
                            Ok(id) => {
                                if let Some(index) = id.as_ref().and_then(|id| by_id.get(id)) {
                                    fill_tracked_location(&mut entries[*index], &location, "walg");
                                    continue;
                                }
                            }
                            Err(reason) => {
                                scan_errors.push(format!("{reason} WAL-G metadata for {sentinel}"))
                            }
                        }
                    }
                    location
                } else {
                    if seen_locations.contains(&repository) {
                        continue;
                    }
                    repository
                };
                if seen_locations.contains(&location) {
                    continue;
                }
                entries.push(SourceBackupIndexEntry {
                    id: 0,
                    backup_id: String::new(),
                    name: format!("{engine} backup ({service_name})"),
                    backup_type: "full".into(),
                    created_at: last_modified,
                    expires_at: None,
                    size_bytes: None,
                    location,
                    metadata_location: String::new(),
                    engine: Some(engine.clone()),
                    origin_service_name: Some(service_name.clone()),
                    format: Some("walg".to_string()),
                    source: "s3_scan".into(),
                    state: "completed".into(),
                });
            }

            // Look for pg_dump-style objects under the service prefix.
            let dumps = list_dump_objects(s3_client, bucket, &service_prefix).await?;
            for (key, last_modified, size) in dumps {
                if seen_locations.contains(&key) {
                    continue;
                }
                let format = classify_backup_format(&key, Some(&engine))
                    .unwrap_or_else(|| "unknown".to_string());
                if let Some(index) = key.rsplit('/').nth(1).and_then(|id| by_id.get(id)) {
                    fill_tracked_location(&mut entries[*index], &key, &format);
                    continue;
                }
                entries.push(SourceBackupIndexEntry {
                    id: 0,
                    backup_id: String::new(),
                    name: format!("{engine} backup ({service_name})"),
                    backup_type: "full".into(),
                    created_at: last_modified,
                    expires_at: None,
                    size_bytes: size,
                    location: key,
                    metadata_location: String::new(),
                    engine: Some(engine.clone()),
                    origin_service_name: Some(service_name.clone()),
                    format: Some(format),
                    source: "s3_scan".into(),
                    state: "completed".into(),
                });
            }
        }
    }

    Ok(())
}

fn fill_tracked_location(entry: &mut SourceBackupIndexEntry, location: &str, format: &str) {
    if entry.location.is_empty() {
        entry.location = location.to_owned();
        entry.format = Some(format.to_owned());
    }
}

/// List CommonPrefixes under a given S3 prefix (with `/` delimiter).
/// Returns full subprefix paths (e.g. `external_services/postgres/`).
async fn list_common_prefixes(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<String>, ScanListError> {
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .delimiter("/");
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ScanListError::from_request(prefix, e))?;
        for cp in resp.common_prefixes() {
            if let Some(p) = cp.prefix() {
                out.push(p.to_string());
            }
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(out)
}

/// Find WAL-G backup-stop-sentinel objects under a walg prefix and return
/// (key, last_modified_rfc3339, size_bytes) for each. WAL-G names them
/// `base_<timestamp>_backup_stop_sentinel.json`. The presence of the
/// sentinel is what marks a WAL-G backup as complete.
async fn list_walg_sentinels(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    walg_prefix: &str,
) -> Result<Vec<(String, String, Option<i64>)>, ScanListError> {
    let basebackups_prefix = format!("{}basebackups_005/", walg_prefix);
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&basebackups_prefix)
            .delimiter("/");
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ScanListError::from_request(&basebackups_prefix, e))?;
        for obj in resp.contents() {
            let key = match obj.key() {
                Some(k) => k.to_string(),
                None => continue,
            };
            if !key.ends_with("_backup_stop_sentinel.json") {
                continue;
            }
            let lm = obj
                .last_modified()
                .and_then(|d| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(d.secs(), d.subsec_nanos())
                        .map(|c| c.to_rfc3339())
                })
                .unwrap_or_default();
            let size = obj.size();
            out.push((key, lm, size));
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(out)
}

/// Find pg_dump / rdb / bson / mariadb dump-or-base objects under a service
/// prefix.
///
/// MariaDB is the one engine here with no `/walg/` prefix, so
/// `scan_s3_for_orphan_backups`'s sentinel pass cannot see it at all: if a
/// MariaDB suffix is missing from this allowlist, its backups are invisible
/// to the disaster-recovery scan entirely.
async fn list_dump_objects(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    service_prefix: &str,
) -> Result<Vec<(String, String, Option<i64>)>, ScanListError> {
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(service_prefix);
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ScanListError::from_request(service_prefix, e))?;
        for obj in resp.contents() {
            let key = match obj.key() {
                Some(k) => k.to_string(),
                None => continue,
            };
            // Skip walg internals — they're captured by the sentinel pass.
            if key.contains("/walg/") {
                continue;
            }
            if !(key.ends_with(".sql.gz")
                || key.ends_with(".pgdump.gz")
                || key.ends_with(".rdb.gz")
                || key.ends_with(".bson.gz")
                || key.ends_with(".archive")
                // MariaDB physical base (`base.mbstream.gz`). Its logical
                // dump is already covered by `.sql.gz` above.
                || key.ends_with(".mbstream.gz"))
            {
                continue;
            }
            let lm = obj
                .last_modified()
                .and_then(|d| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(d.secs(), d.subsec_nanos())
                        .map(|c| c.to_rfc3339())
                })
                .unwrap_or_default();
            let size = obj.size();
            out.push((key, lm, size));
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(out)
}

/// Given `external_services/postgres/` returns `postgres`. Returns empty
/// string when the prefix has no trailing segment.
fn extract_trailing_segment(prefix: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(s) => s.to_string(),
        None => String::new(),
    }
}

impl BackupService {
    /// Remove a control-plane backup from the optional discovery index.
    /// Conditional writes prevent one concurrent backup/index update from
    /// silently resurrecting another entry.
    pub(super) async fn remove_backup_from_index(
        &self,
        client: &S3Client,
        source: &S3Source,
        backup_id: &str,
    ) -> Result<(), BackupError> {
        let key = build_s3_key(&source.bucket_path, "backups/index.json");
        for attempt in 1..=3 {
            let response = match client
                .get_object()
                .bucket(&source.bucket_name)
                .key(&key)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error)
                    if error.as_service_error().and_then(|e| e.code()) == Some("NoSuchKey") =>
                {
                    return Ok(());
                }
                Err(error) => {
                    return Err(BackupError::S3(format!(
                        "Failed to read s3://{}/{}: {}",
                        source.bucket_name, key, error
                    )));
                }
            };
            let etag = response.e_tag().map(str::to_owned).ok_or_else(|| {
                BackupError::S3(format!(
                    "S3 provider omitted ETag for s3://{}/{}; refusing an unconditional index update",
                    source.bucket_name, key
                ))
            })?;
            let bytes = response.body.collect().await.map_err(|error| {
                BackupError::S3(format!(
                    "Failed to read s3://{}/{} body: {}",
                    source.bucket_name, key, error
                ))
            })?;
            let mut index: serde_json::Value = serde_json::from_slice(&bytes.into_bytes())?;
            let entries = index
                .get_mut("backups")
                .and_then(serde_json::Value::as_array_mut)
                .ok_or_else(|| {
                    BackupError::Validation(format!(
                        "S3 backup index s3://{}/{} has no backups array",
                        source.bucket_name, key
                    ))
                })?;
            let original_len = entries.len();
            entries.retain(|entry| {
                entry.get("backup_id").and_then(serde_json::Value::as_str) != Some(backup_id)
            });
            if entries.len() == original_len {
                return Ok(());
            }
            index["last_updated"] = json!(Utc::now().to_rfc3339());
            let mut request = client
                .put_object()
                .bucket(&source.bucket_name)
                .key(&key)
                .body(serde_json::to_vec(&index)?.into())
                .content_type("application/json");
            request = request.if_match(etag);
            match request.send().await {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.as_service_error().and_then(|e| e.code())
                        == Some("PreconditionFailed")
                        && attempt < 3 =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(BackupError::S3(format!(
                        "Failed to update s3://{}/{}: {}",
                        source.bucket_name, key, error
                    )));
                }
            }
        }
        Err(BackupError::S3(format!(
            "Concurrent updates prevented removing backup {} from s3://{}/{}",
            backup_id, source.bucket_name, key
        )))
    }

    /// Generate metadata for a backup
    pub(super) fn generate_backup_metadata(
        &self,
        backup: &Backup,
        s3_source: &temps_entities::s3_sources::Model,
        external_backups: &[(
            temps_entities::external_service_backups::Model,
            temps_entities::external_services::Model,
        )],
    ) -> Result<serde_json::Value, BackupError> {
        let config_yaml =
            yaml_serde::to_string(&self.config_service.get_server_config()).map_err(|error| {
                BackupError::Configuration(format!(
                    "Failed to serialize server config for backup {}: {}",
                    backup.backup_id, error
                ))
            })?;
        let s3_secret = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|error| {
                BackupError::Configuration(format!(
                    "Failed to decrypt S3 secret for backup {}: {}",
                    backup.backup_id, error
                ))
            })?;
        let encrypted_server_config =
            temps_core::EncryptionService::encrypt_string_with_password(&s3_secret, &config_yaml)
                .map_err(|error| BackupError::Internal {
                message: format!(
                    "Failed to encrypt server config for backup {}: {}",
                    backup.backup_id, error
                ),
            })?;

        // Map external backups to the required format
        let external_backups = external_backups
            .iter()
            .map(|(b, service)| {
                json!({
                    "backup_id": b.backup_id,
                    "service_id": b.service_id,
                    "s3_location": b.s3_location,
                    "state": b.state,
                    "size_bytes": b.size_bytes,
                    "type": "full",
                    "metadata": {
                        "service_type": service.service_type,
                        "service_name": service.name
                    }
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "backup_id": backup.backup_id,
            "name": backup.name,
            "type": backup.backup_type,
            "created_at": backup.started_at.to_rfc3339(),
            "created_by": backup.created_by,
            "size_bytes": backup.size_bytes,
            "compression_type": backup.compression_type,
            "source": {
                "id": s3_source.id,
                "name": s3_source.name,
                "bucket": s3_source.bucket_name,
                "path": s3_source.bucket_path
            },
            "schedule_id": backup.schedule_id,
            "state": backup.state,
            "tags": serde_json::from_str::<Vec<String>>(&backup.tags).unwrap_or_default(),
            "checksum": backup.checksum,
            "server_config_encrypted": encrypted_server_config,
            "server_config_encryption": "aes-256-gcm+s3-secret-argon2id",
            "external_service_backups": external_backups,
            "metadata": serde_json::from_str::<serde_json::Value>(&backup.metadata).unwrap_or_default()
        }))
    }

    /// Update the source's backup index
    pub(super) async fn update_backup_index(
        &self,
        s3_client: &S3Client,
        s3_source: &temps_entities::s3_sources::Model,
        backup: &Backup,
    ) -> Result<()> {
        let index_key = build_s3_key(&s3_source.bucket_path, "backups/index.json");
        for attempt in 1..=3 {
            let (mut index, etag) = match s3_client
                .get_object()
                .bucket(&s3_source.bucket_name)
                .key(&index_key)
                .send()
                .await
            {
                Ok(response) => {
                    let etag = response.e_tag().map(str::to_owned).ok_or_else(|| {
                        anyhow::anyhow!(
                            "S3 provider omitted ETag for s3://{}/{}",
                            s3_source.bucket_name,
                            index_key
                        )
                    })?;
                    let data = response.body.collect().await?.to_vec();
                    (
                        serde_json::from_slice::<serde_json::Value>(&data)?,
                        Some(etag),
                    )
                }
                Err(error)
                    if error.as_service_error().and_then(|value| value.code())
                        == Some("NoSuchKey") =>
                {
                    (
                        json!({
                            "backups": [],
                            "last_updated": Utc::now().to_rfc3339()
                        }),
                        None,
                    )
                }
                Err(error) => return Err(error.into()),
            };
            let backups = index
                .get_mut("backups")
                .and_then(serde_json::Value::as_array_mut)
                .ok_or_else(|| anyhow::anyhow!("S3 backup index has no backups array"))?;
            backups.retain(|entry| {
                entry.get("backup_id").and_then(serde_json::Value::as_str)
                    != Some(backup.backup_id.as_str())
            });
            backups.push(json!({
                "id": backup.id,
                "backup_id": backup.backup_id,
                "name": backup.name,
                "type": backup.backup_type,
                "created_at": backup.started_at.to_rfc3339(),
                "size_bytes": backup.size_bytes,
                "location": backup.s3_location.clone(),
                "metadata_location": backup.s3_location
                    .replace("backup.sql.gz", "metadata.json")
                    .replace("backup.postgresql.gz", "metadata.json")
            }));
            index["last_updated"] = json!(Utc::now().to_rfc3339());

            let mut request = s3_client
                .put_object()
                .bucket(&s3_source.bucket_name)
                .key(&index_key)
                .body(serde_json::to_vec(&index)?.into())
                .content_type("application/json");
            request = match etag {
                Some(etag) => request.if_match(etag),
                None => request.if_none_match("*"),
            };
            match request.send().await {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.as_service_error().and_then(|value| value.code())
                        == Some("PreconditionFailed")
                        && attempt < 3 =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(anyhow::anyhow!(
            "Concurrent updates prevented writing s3://{}/{}",
            s3_source.bucket_name,
            index_key
        ))
    }

    /// List every backup visible on an S3 source.
    ///
    /// Returns a union of two sources of truth, intended for the restore
    /// UI (both regular and cross-service disaster-recovery):
    ///
    /// 1. **DB rows** — backups this Temps instance recorded. Cheap,
    ///    trusted, has the canonical backup_id / state / size.
    /// 2. **S3 scan** (only when `include_s3_scan` is `true`) — objects
    ///    discovered by walking
    ///    `s3://<bucket>/<bucket_path>/external_services/<engine>/<service>/`.
    ///    This is how DR works when you've restored a Temps instance and
    ///    need to browse backups made by a previous instance whose DB you
    ///    no longer have. S3-scan entries get `id: 0`, `backup_id: ""`,
    ///    and `source: "s3_scan"` — the restore orchestrator keys off
    ///    `location` in that case, not `backup_id`.
    ///
    /// Setting `include_s3_scan = false` (the default) skips the bucket
    /// walk entirely and returns DB rows only — completing in <100 ms
    /// regardless of S3 endpoint latency.
    pub async fn list_source_backups(
        &self,
        s3_source_id: i32,
        include_s3_scan: bool,
    ) -> Result<SourceBackupIndex, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // ---- Pass 1: DB-tracked backups ------------------------------------
        // `backup_external_service` inserts with state='running' and (now)
        // updates to 'completed' on success. Older rows may still be stuck
        // in 'running' — we show them anyway so they're visible/debuggable;
        // the UI badges them differently from completed ones.
        let db_rows = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::S3SourceId.eq(s3_source_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;

        let mut entries: Vec<SourceBackupIndexEntry> = Vec::with_capacity(db_rows.len());
        // Collect locations we've seen to avoid surfacing the same backup
        // twice (once from DB + once from S3 scan).
        let mut seen_locations: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Cache for external_services lookups so we don't refetch the same
        // row N times within a single listing. Keyed by external_services.id.
        let mut ext_service_cache: std::collections::HashMap<i32, Option<(String, String)>> =
            std::collections::HashMap::new();

        for backup in db_rows {
            let metadata: serde_json::Value =
                serde_json::from_str(&backup.metadata).unwrap_or(serde_json::Value::Null);
            let mut service_name = metadata
                .get("service_name")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut service_type = metadata
                .get("service_type")
                .and_then(|v| v.as_str())
                .map(String::from);

            // ADR-014 async runner rows write only `external_service_id` into
            // metadata (not `service_name`/`service_type`). Without filling
            // those in, the frontend ServiceDetail.tsx page filters them out
            // (it matches by `origin_service_name === serviceName`) and the
            // user's failed/pending backups become invisible. Look up the
            // external service once per id and cache.
            if service_name.is_none() || service_type.is_none() {
                if let Some(ext_id) = metadata
                    .get("external_service_id")
                    .and_then(|v| v.as_i64())
                    .and_then(|v| i32::try_from(v).ok())
                {
                    let cached = ext_service_cache.entry(ext_id).or_insert_with_key(|_| None);
                    if cached.is_none() {
                        // Cache miss — try the DB. A None result means we
                        // already failed once; don't refetch. We can't reuse
                        // the entry api's value because we need an async call.
                        if let Ok(Some(svc)) =
                            temps_entities::external_services::Entity::find_by_id(ext_id)
                                .one(self.db.as_ref())
                                .await
                        {
                            *cached = Some((svc.name.clone(), svc.service_type.clone()));
                        }
                    }
                    if let Some((n, t)) = cached.clone() {
                        if service_name.is_none() {
                            service_name = Some(n);
                        }
                        if service_type.is_none() {
                            service_type = Some(t);
                        }
                    }
                }
            }

            // Skip control-plane backups — this endpoint powers the
            // "restore into an external service" UI, and whole-Temps-DB
            // backups (stored under `backups/...`, no service_type in
            // metadata) are not valid candidates for that flow. They'd
            // render as "pg_dump" with blank engine and confuse users
            // into thinking they could be restored onto their service.
            //
            // Rows created by the ADR-014 async runner for external services
            // may have an empty `s3_location` while pending (the location is
            // filled in by `mark_job_completed` on `Done`). These rows carry
            // `external_service_id` in their metadata — that field is the
            // canonical signal that the row belongs to an external service.
            // Using the `s3_location` alone to classify pending/failed rows
            // is the root cause of the "invisible backups" bug (Bug 4).
            let has_external_service_id = metadata.get("external_service_id").is_some();
            let is_control_plane =
                metadata.get("engine").and_then(|v| v.as_str()) == Some("control_plane");
            let is_external_service_location = backup.s3_location.contains("external_services/");

            // Include the row only if it is clearly an external-service backup.
            // Rule: skip if none of the three external-service signals are present.
            if !has_external_service_id
                && !is_external_service_location
                && service_type.is_none()
                && !is_control_plane
            {
                // Not enough signal — could be legacy orphan data. Skip.
                continue;
            }
            // Always skip confirmed control-plane backups.
            if is_control_plane && !is_external_service_location {
                continue;
            }

            let display_name = match (&service_name, &service_type) {
                (Some(n), Some(t)) => format!("{} backup ({})", t, n),
                _ => backup.name.clone(),
            };

            let format = classify_backup_format(&backup.s3_location, service_type.as_deref());

            let metadata_location = if backup.s3_location.is_empty() {
                String::new()
            } else {
                backup
                    .s3_location
                    .replace("backup.sql.gz", "metadata.json")
                    .replace("backup.postgresql.gz", "metadata.json")
            };

            if !backup.s3_location.is_empty() {
                seen_locations.insert(backup.s3_location.trim_end_matches('/').to_owned());
                if let Some(key) = backup
                    .s3_location
                    .strip_prefix(&format!("s3://{}/", s3_source.bucket_name))
                {
                    seen_locations.insert(key.to_owned());
                }
            }

            entries.push(SourceBackupIndexEntry {
                id: backup.id,
                backup_id: backup.backup_id,
                name: display_name,
                backup_type: backup.backup_type,
                created_at: backup
                    .started_at
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
                expires_at: backup
                    .expires_at
                    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                size_bytes: backup.size_bytes,
                location: backup.s3_location,
                metadata_location,
                engine: service_type,
                origin_service_name: service_name,
                format,
                source: "db".into(),
                state: backup.state,
            });
        }

        // ---- Pass 2: S3 scan for orphan backups (opt-in) ------------------
        // Only executed when `include_s3_scan` is true.  Skipped by default
        // because each scan may issue dozens of sequential LIST_OBJECTS_V2
        // calls against slow S3-compatible endpoints (e.g. OVH Object Storage
        // can take 5-30 s for a bucket with many prefixes).
        //
        // Best-effort: if the S3 client can't talk to the bucket we just
        // skip this pass and return the DB-based list. We never fail the
        // whole endpoint just because a bucket scan failed — the UI's
        // happy-path for normal users doesn't depend on it.
        //
        // Scanned entries are matched by their storage location, never by
        // service name: distinct servers may use the same name.
        let mut scan_errors = Vec::new();
        if include_s3_scan {
            if let Ok(s3_client) = self.create_s3_client(&s3_source).await {
                if let Err(e) = scan_s3_for_orphan_backups(
                    &s3_client,
                    &s3_source,
                    &seen_locations,
                    &mut entries,
                    &mut scan_errors,
                )
                .await
                {
                    warn!(s3_source_id, error = %e, "S3 backup scan failed");
                    scan_errors.push(format!("S3 source {s3_source_id}: {e}"));
                }
            } else {
                warn!(
                    s3_source_id,
                    "Could not initialise S3 client for backup scan"
                );
                scan_errors.push(format!("Could not initialise S3 client for source {s3_source_id}; check its credentials and settings"));
            }
        } // end if include_s3_scan

        // Final sort: newest first, regardless of source.
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let last_updated = entries
            .first()
            .map(|entry| entry.created_at.clone())
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        Ok(SourceBackupIndex {
            backups: entries,
            last_updated,
            scan_errors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::EncryptionService;

    #[test]
    fn classify_pgdump_by_extension() {
        let loc = "s3://bucket/external_services/postgres/svc/2026/05/01/uuid/backup.sql.gz";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("pg_dump".to_string())
        );
    }

    #[test]
    fn classify_walg_by_prefix_segment() {
        let loc = "s3://bucket/external_services/postgres/svc/walg";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("walg".to_string())
        );
    }

    #[test]
    fn classify_walg_with_trailing_slash() {
        let loc = "s3://bucket/external_services/postgres/svc/walg/";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("walg".to_string())
        );
    }

    #[test]
    fn classify_walg_sentinel_object_under_prefix() {
        // S3 scan may pass the sentinel key directly — still walg.
        let loc =
            "s3://bucket/external_services/postgres/svc/walg/basebackups_005/base_000_backup_stop_sentinel.json";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("walg".to_string())
        );
    }

    #[test]
    fn classify_redis_rdb() {
        let loc = "s3://bucket/external_services/redis/svc/2026/05/01/uuid/dump.rdb.gz";
        assert_eq!(
            classify_backup_format(loc, Some("redis")),
            Some("rdb".to_string())
        );
    }

    #[test]
    fn classify_mongodump() {
        let loc = "s3://bucket/external_services/mongodb/svc/2026/05/01/uuid/dump.archive";
        assert_eq!(
            classify_backup_format(loc, Some("mongodb")),
            Some("mongodump".to_string())
        );
    }

    #[test]
    fn classify_s3_mirror_is_engine_driven() {
        // The location for an s3-mirror backup doesn't have a meaningful
        // extension; engine name carries the classification.
        let loc = "s3://bucket/external_services/s3/svc/2026/05/01/uuid";
        assert_eq!(
            classify_backup_format(loc, Some("s3")),
            Some("mirror".to_string())
        );
    }

    #[test]
    fn classify_empty_location_returns_none() {
        assert_eq!(classify_backup_format("", Some("postgres")), None);
    }

    #[test]
    fn classify_does_not_default_s3_uris_to_walg() {
        // Regression: any `s3://...` location used to be classified as
        // walg, mislabeling every pg_dump / rdb / mongodump backup that
        // happened to live in S3 (which is all of them). The classifier
        // must require an explicit `walg` path segment.
        let loc = "s3://bucket/external_services/postgres/svc/2026/05/01/uuid/backup.sql.gz";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("pg_dump".to_string())
        );

        // Unknown extension, no walg segment, not an object-store engine —
        // we genuinely don't know. Better to return None than to
        // confidently mislabel.
        let unknown = "s3://bucket/external_services/postgres/svc/some/random/key";
        assert_eq!(classify_backup_format(unknown, Some("postgres")), None);
    }

    #[test]
    fn classify_mariadb_physical_base_is_filename_driven() {
        // `base.mbstream.gz` is produced by no other engine, so it classifies
        // without an engine hint at all.
        let loc = "s3://bucket/external_services/mariadb/svc/2026/05/01/uuid/base.mbstream.gz";
        assert_eq!(
            classify_backup_format(loc, Some("mariadb")),
            Some("mariadb_physical".to_string())
        );
        assert_eq!(
            classify_backup_format(loc, None),
            Some("mariadb_physical".to_string())
        );
    }

    #[test]
    fn classify_mariadb_dump_is_engine_driven_not_filename_driven() {
        // Regression: MariaDB's logical dump is literally named
        // `dump.sql.gz`, which the generic `.sql.gz` branch labels `pg_dump`.
        // Only the engine hint can tell the two apart.
        let mariadb = "s3://bucket/external_services/mariadb/svc/2026/05/01/uuid/dump.sql.gz";
        assert_eq!(
            classify_backup_format(mariadb, Some("mariadb")),
            Some("mariadb_dump".to_string())
        );
        assert_eq!(
            classify_backup_format(mariadb, Some("MariaDB")),
            Some("mariadb_dump".to_string()),
            "engine hint must be matched case-insensitively"
        );

        // Postgres behavior is unchanged: same suffix, different engine.
        let postgres = "s3://bucket/external_services/postgres/svc/2026/05/01/uuid/dump.sql.gz";
        assert_eq!(
            classify_backup_format(postgres, Some("postgres")),
            Some("pg_dump".to_string())
        );
        // ...including when no hint is available at all.
        assert_eq!(
            classify_backup_format(postgres, None),
            Some("pg_dump".to_string())
        );
    }

    #[tokio::test]
    async fn another_servers_dump_is_discovered_and_restored(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::services::{BackupSelector, RestoreRequestMode, RestoreService};
        use crate::test_rustfs;
        use std::io::Write;
        use temps_entities::{backups, external_services, s3_sources, users};
        use temps_test_utils::TestDatabase;
        use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

        if !temps_test_utils::docker_available().await {
            eprintln!("Docker unavailable, skipping cross-server restore");
            return Ok(());
        }
        let a = TestDatabase::with_migrations().await?;
        let b = TestDatabase::with_migrations().await?;
        let store = test_rustfs::rustfs_container_request().start().await?;
        let port = store
            .get_host_port_ipv4(test_rustfs::RUSTFS_S3_PORT)
            .await?;
        test_rustfs::wait_for_rustfs_ready(port).await?;
        let bucket = "backups-bucket";
        let backups_a = make_scope_test_service(a.db.clone());
        let backups_b = make_scope_test_service(b.db.clone());
        let key = "external_services/postgres/service-a/server-a/backup.sql.gz";
        let mut destination = None;
        let repository = format!("s3://{bucket}/external_services/postgres/service-a/walg");
        for (service, object_key) in [
            (&backups_a, key),
            (
                &backups_b,
                "external_services/postgres/service-a/server-b/backup.sql.gz",
            ),
        ] {
            let db = service.db.as_ref();
            let source = s3_sources::ActiveModel {
                name: Set("shared".into()),
                bucket_name: Set(bucket.into()),
                bucket_path: Set(String::new()),
                region: Set("us-east-1".into()),
                endpoint: Set(Some(format!("http://127.0.0.1:{port}"))),
                access_key_id: Set(service
                    .encryption_service
                    .encrypt_string(test_rustfs::RUSTFS_ACCESS_KEY)?),
                secret_key: Set(service
                    .encryption_service
                    .encrypt_string(test_rustfs::RUSTFS_SECRET_KEY)?),
                force_path_style: Set(Some(true)),
                ..Default::default()
            }
            .insert(db)
            .await?;
            let user = users::ActiveModel {
                name: Set("operator".into()),
                email: Set("operator@example.invalid".into()),
                ..Default::default()
            }
            .insert(db)
            .await?;
            let mut row = make_test_backup_model(1).into_active_model();
            row.id = sea_orm::ActiveValue::NotSet;
            row.s3_location = Set(object_key.into());
            row.backup_id = Set(Uuid::new_v4().to_string());
            row.s3_source_id = Set(source.id);
            row.created_by = Set(user.id);
            row.metadata =
                Set(json!({"service_name":"service-a","service_type":"postgres"}).to_string());
            row.reset_all().insert(db).await?;
            if object_key == key {
                let s3 = service.create_s3_client(&source).await?;
                s3.create_bucket().bucket(bucket).send().await?;
                let mut dump =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                dump.write_all(b"CREATE TABLE restore_probe AS SELECT 42 AS value;")?;
                s3.put_object()
                    .bucket(bucket)
                    .key(key)
                    .body(dump.finish()?.into())
                    .send()
                    .await?;
                for (name, uid) in [
                    ("base_0001", "local-backup"),
                    ("base_0002", "foreign-backup"),
                ] {
                    let sentinel = format!("external_services/postgres/service-a/walg/basebackups_005/{name}_backup_stop_sentinel.json");
                    s3.put_object()
                        .bucket(bucket)
                        .key(sentinel)
                        .body(
                            serde_json::to_vec(&json!({"UserData":{"temps_backup_id":uid}}))?
                                .into(),
                        )
                        .send()
                        .await?;
                }
            } else {
                let mut physical = make_test_backup_model(2).into_active_model();
                physical.id = sea_orm::ActiveValue::NotSet;
                physical.s3_source_id = Set(source.id);
                physical.created_by = Set(user.id);
                physical.s3_location = Set(repository.clone());
                physical.backup_id = Set("local-backup".into());
                physical.reset_all().insert(db).await?;
                destination = Some((source.id, user.id));
            }
        }
        let (source_id, user_id) = destination.ok_or("missing destination")?;
        let plain = backups_b.list_source_backups(source_id, false).await?;
        assert_eq!(plain.backups.len(), 2);
        let scanned = backups_b.list_source_backups(source_id, true).await?;
        assert!(scanned.scan_errors.is_empty(), "{:?}", scanned.scan_errors);
        assert_eq!(scanned.backups.len(), 4);
        assert!(scanned.backups.iter().any(|entry| entry
            .location
            .ends_with("base_0002_backup_stop_sentinel.json")));
        assert!(!scanned.backups.iter().any(|entry| entry
            .location
            .ends_with("base_0001_backup_stop_sentinel.json")));
        let foreign = scanned
            .backups
            .into_iter()
            .find(|row| row.source == "s3_scan" && row.format.as_deref() == Some("pg_dump"))
            .ok_or("missing scanned backup")?;
        assert_eq!(foreign.location, key);
        let name = format!("service-{}", Uuid::new_v4().simple());
        let password = Uuid::new_v4().to_string();
        let postgres = GenericImage::new("gotempsh/postgres-walg", "17-bookworm")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_container_name(format!("postgres-{name}"))
            .with_env_var("POSTGRES_PASSWORD", &password)
            .start()
            .await?;
        let pg_port = postgres.get_host_port_ipv4(5432).await?;
        let url = format!("postgres://postgres:{password}@127.0.0.1:{pg_port}/postgres");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_secs(30))
            .connect(&url)
            .await?;
        let config = json!({"host":"127.0.0.1","port":pg_port.to_string(),"database":"postgres","username":"postgres","password":password,"docker_image":"gotempsh/postgres-walg:17-bookworm"});
        let enc = backups_b.encryption_service.clone();
        let target = external_services::ActiveModel {
            name: Set(name),
            service_type: Set("postgres".into()),
            status: Set("running".into()),
            config: Set(Some(enc.encrypt_string(&config.to_string())?)),
            ..Default::default()
        }
        .insert(b.db.as_ref())
        .await?;
        let manager = Arc::new(temps_providers::ExternalServiceManager::new(
            b.db.clone(),
            enc.clone(),
            Arc::new(bollard::Docker::connect_with_local_defaults()?),
            Arc::new(temps_providers::DnsRegistry::new(b.db.clone())),
        ));
        let restores = RestoreService::new(b.db.clone(), manager, enc);
        let run = restores
            .start_restore(
                target.id,
                BackupSelector::Location {
                    location: foreign.location,
                    engine: "postgres".into(),
                    s3_source_id: source_id,
                },
                RestoreRequestMode::InPlace,
                user_id,
            )
            .await?;
        assert_eq!(run.source_backup_id, None);
        assert_eq!(run.source_backup.location.as_deref(), Some(key));
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            loop {
                let state = restores.get_restore_run(run.id).await?;
                if state.status != "running" {
                    assert_eq!(state.status, "completed", "{:?}", state.error_message);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Ok::<_, crate::services::RestoreError>(())
        })
        .await??;
        assert_eq!(
            sqlx::query_scalar::<_, i32>("SELECT value FROM restore_probe")
                .fetch_one(&pool)
                .await?,
            42
        );
        assert_eq!(backups::Entity::find().count(b.db.as_ref()).await?, 2);
        Ok(())
    }

    /// Regression test for the "invisible backups" bug (Bug 4).
    ///
    /// ADR-014 async-runner-created backups start with `s3_location = ""` because
    /// the location is only filled in by `mark_job_completed` when `Done` fires.
    /// Before the fix, the `list_source_backups` query skipped any row where
    /// `s3_location` was empty AND `s3_location` didn't contain `"external_services/"`.
    /// This made every pending/failed backup invisible in the UI.
    ///
    /// The fix: rows that carry `external_service_id` in their JSON metadata are
    /// always included, even with an empty `s3_location`.
    #[tokio::test]
    async fn test_list_source_backups_includes_pending_rows_without_s3_location(
    ) -> Result<(), BackupError> {
        use temps_entities::{backups, s3_sources};

        let s3_src = s3_sources::Model {
            id: 1,
            name: "test-src".to_string(),
            bucket_name: "bucket".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_path: "/backups".to_string(),
            access_key_id: "key".to_string(),
            secret_key: "secret".to_string(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            worker_credentials: "scoped".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            backing_service_id: None,
        };

        // A runner-created external service backup in `pending` state with empty
        // `s3_location`.  The metadata carries `external_service_id` which is the
        // signal introduced by the fix.
        let pending_backup = backups::Model {
            id: 55,
            name: "Backup abc-123".to_string(),
            backup_id: "abc-123".to_string(),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "full".to_string(),
            state: "pending".to_string(),
            started_at: Utc::now(),
            finished_at: None,
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: String::new(), // empty — the bug trigger
            error_message: None,
            metadata: serde_json::json!({
                "external_service_id": 42,
                "async_runner": true,
                "timestamp": Utc::now().to_rfc3339(),
            })
            .to_string(),
            checksum: None,
            compression_type: "none".to_string(),
            created_by: 1,
            expires_at: None,
            tags: "[]".to_string(),
        };

        // MockDatabase query sequence for `list_source_backups`:
        // 1. SELECT s3_sources WHERE id = 1   → returns our s3_src row
        // 2. SELECT backups WHERE s3_source_id = 1 → returns pending_backup
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![s3_src.clone()]])
                .append_query_results(vec![vec![pending_backup.clone()]])
                .append_query_results([Vec::<temps_entities::external_services::Model>::new()])
                .append_query_results(vec![vec![s3_src]])
                .append_query_results(vec![vec![pending_backup]])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let alarm_service = create_mock_alarm_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            alarm_service,
            config_service,
            encryption_service,
            Arc::new(NoopJobQueue),
        );

        // DB-only path (include_s3_scan = false) — no S3 access in tests.
        let result = backup_service.list_source_backups(1, false).await;
        assert!(
            result.is_ok(),
            "list_source_backups should not fail: {:?}",
            result
        );

        let index = result.unwrap();
        let backups_arr = index.backups;

        assert_eq!(
            backups_arr.len(),
            1,
            "Expected 1 backup entry (the pending row), got {}; Bug 4 regression: \
             pending rows with empty s3_location were being filtered out",
            backups_arr.len()
        );

        let entry = &backups_arr[0];
        assert_eq!(
            entry.state, "pending",
            "The returned entry should have state='pending'"
        );
        assert_eq!(entry.id, 55, "The returned entry should have id=55");
        assert!(index.scan_errors.is_empty());
        // The stored fixture credentials cannot be decrypted. A requested scan
        // must tell the caller it failed while preserving the tracked backup.
        let scanned = backup_service.list_source_backups(1, true).await?;
        assert_eq!(scanned.backups[0].id, 55);
        assert_eq!(scanned.scan_errors.len(), 1);
        Ok(())
    }

    // -------------------------------------------------------------------------
    // TimescaleDB sidecar image selection
    // -------------------------------------------------------------------------
}
