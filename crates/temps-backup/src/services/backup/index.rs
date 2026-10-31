// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The S3 backup index, backup metadata, orphan scans and listing the backups
//! stored in a source.

use super::*;

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

/// Walk the S3 source's `external_services/` prefix to find backups that
/// aren't represented in the local DB (e.g., backups produced by a
/// previous Temps instance). Returns synthesized `SourceBackupEntry`-shape
/// JSON values tagged with `source: "s3_scan"`.
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
) -> Result<Vec<serde_json::Value>, anyhow::Error> {
    let bucket = &s3_source.bucket_name;
    let prefix = build_s3_key(&s3_source.bucket_path, "external_services/");

    // First-level list: `external_services/<engine>/`. We use delimiter
    // '/' so we only get the top-level engine directories (CommonPrefixes).
    let engine_prefixes: Vec<String> = list_common_prefixes(s3_client, bucket, &prefix).await?;

    let mut out: Vec<serde_json::Value> = Vec::new();

    for engine_prefix in engine_prefixes {
        let engine = extract_trailing_segment(&engine_prefix);
        // Second-level list: `external_services/<engine>/<service>/`
        let service_prefixes = list_common_prefixes(s3_client, bucket, &engine_prefix).await?;

        for service_prefix in service_prefixes {
            let service_name = extract_trailing_segment(&service_prefix);

            // Look for a WAL-G backup under `<service_prefix>walg/`.
            let walg_prefix = format!("{}walg/", service_prefix);
            let walg_sentinels = list_walg_sentinels(s3_client, bucket, &walg_prefix).await?;
            for (name, last_modified, size) in walg_sentinels {
                // Canonical restore location is the walg root, not the
                // sentinel itself (wal-g backup-fetch takes a prefix).
                let endpoint_host = s3_source
                    .endpoint
                    .as_deref()
                    .and_then(|u| {
                        u.strip_prefix("http://")
                            .or_else(|| u.strip_prefix("https://"))
                    })
                    .unwrap_or("");
                let _ = endpoint_host; // silence unused — kept for future use
                let location = format!("s3://{}/{}", bucket, walg_prefix.trim_end_matches('/'));
                if seen_locations.contains(&location) {
                    continue;
                }
                out.push(serde_json::json!({
                    "id": 0,
                    "backup_id": "",
                    "name": format!("{} backup ({})", engine, service_name),
                    "type": "full",
                    "created_at": last_modified,
                    "size_bytes": size,
                    "location": location,
                    "metadata_location": "",
                    "engine": engine.clone(),
                    "origin_service_name": service_name.clone(),
                    "format": "walg",
                    "source": "s3_scan",
                    "state": "completed",
                    "scan_sentinel_key": name,
                }));
            }

            // Look for pg_dump-style objects under the service prefix.
            let dumps = list_dump_objects(s3_client, bucket, &service_prefix).await?;
            for (key, last_modified, size) in dumps {
                if seen_locations.contains(&key) {
                    continue;
                }
                let format = classify_backup_format(&key, Some(&engine))
                    .unwrap_or_else(|| "unknown".to_string());
                out.push(serde_json::json!({
                    "id": 0,
                    "backup_id": "",
                    "name": format!("{} backup ({})", engine, service_name),
                    "type": "full",
                    "created_at": last_modified,
                    "size_bytes": size,
                    "location": key,
                    "metadata_location": "",
                    "engine": engine.clone(),
                    "origin_service_name": service_name.clone(),
                    "format": format,
                    "source": "s3_scan",
                    "state": "completed",
                }));
            }
        }
    }

    Ok(out)
}

/// List CommonPrefixes under a given S3 prefix (with `/` delimiter).
/// Returns full subprefix paths (e.g. `external_services/postgres/`).
async fn list_common_prefixes(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<String>, anyhow::Error> {
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
        let resp = req.send().await?;
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
) -> Result<Vec<(String, String, Option<i32>)>, anyhow::Error> {
    let basebackups_prefix = format!("{}basebackups_005/", walg_prefix);
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&basebackups_prefix);
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req.send().await?;
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
            // i32 size cap — AWS gives i64; we store i32 in DB, match that.
            let size = obj.size().and_then(|s| i32::try_from(s).ok());
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
) -> Result<Vec<(String, String, Option<i32>)>, anyhow::Error> {
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
        let resp = req.send().await?;
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
            let size = obj.size().and_then(|s| i32::try_from(s).ok());
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
    ) -> Result<serde_json::Value, BackupError> {
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

        let mut entries: Vec<serde_json::Value> = Vec::with_capacity(db_rows.len());
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
                seen_locations.insert(backup.s3_location.clone());
            }

            entries.push(serde_json::json!({
                "id": backup.id,
                "backup_id": backup.backup_id,
                "name": display_name,
                "type": backup.backup_type,
                "created_at": backup.started_at.to_rfc3339(),
                // Retention deadline recorded on the row. `null` means the
                // backup has no schedule-driven expiry and is kept until
                // someone deletes it.
                "expires_at": backup.expires_at.map(|dt| {
                    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
                "size_bytes": backup.size_bytes,
                "location": backup.s3_location,
                "metadata_location": metadata_location,
                "engine": service_type,
                "origin_service_name": service_name,
                "format": format,
                "source": "db",
                "state": backup.state,
            }));
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
        // Dedupe rule: if any DB row already references a given
        // `origin_service_name`, we DROP all S3-scan hits for that service
        // (the DB is authoritative — even if the row's `s3_location` is
        // empty due to the old bug, the user should pick the DB row so
        // the restore runs through the DB-backed path). S3-scan fills the
        // gap only for services this Temps has no DB record of.
        if include_s3_scan {
            let db_tracked_services: std::collections::HashSet<String> = entries
                .iter()
                .filter_map(|e| {
                    e.get("origin_service_name")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .collect();

            if let Ok(s3_client) = self.create_s3_client(&s3_source).await {
                match scan_s3_for_orphan_backups(&s3_client, &s3_source, &seen_locations).await {
                    Ok(scanned) => {
                        // For DB rows with empty `s3_location` (pre-fix backup
                        // rows), steal the matching S3-scan location so the
                        // entry is still restorable. Key by
                        // `origin_service_name` since we don't know the exact
                        // backup id from the scan.
                        let fallback_locations: std::collections::HashMap<
                            String,
                            (String, Option<String>),
                        > = scanned
                            .iter()
                            .filter_map(|e| {
                                let svc = e
                                    .get("origin_service_name")
                                    .and_then(|v| v.as_str())?
                                    .to_string();
                                let loc = e.get("location").and_then(|v| v.as_str())?.to_string();
                                let fmt =
                                    e.get("format").and_then(|v| v.as_str()).map(String::from);
                                Some((svc, (loc, fmt)))
                            })
                            .collect();

                        for entry in entries.iter_mut() {
                            let needs_fill = entry
                                .get("location")
                                .and_then(|v| v.as_str())
                                .map(|s| s.is_empty())
                                .unwrap_or(true);
                            if !needs_fill {
                                continue;
                            }
                            let origin =
                                match entry.get("origin_service_name").and_then(|v| v.as_str()) {
                                    Some(s) => s.to_string(),
                                    None => continue,
                                };
                            if let Some((loc, fmt)) = fallback_locations.get(&origin) {
                                entry["location"] = serde_json::Value::String(loc.clone());
                                if let Some(fmt) = fmt {
                                    entry["format"] = serde_json::Value::String(fmt.clone());
                                }
                            }
                        }

                        // Emit scanned entries for services not tracked by DB.
                        for entry in scanned {
                            let origin = entry
                                .get("origin_service_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if db_tracked_services.contains(origin) {
                                continue;
                            }
                            entries.push(entry);
                        }
                    }
                    Err(e) => {
                        warn!(
                        "S3 scan for orphan backups on source {} failed (returning DB-only list): {}",
                        s3_source_id, e
                    );
                    }
                }
            } else {
                warn!(
                    "Skipping S3 scan on source {}: failed to build S3 client",
                    s3_source_id
                );
            }
        } // end if include_s3_scan

        // Final sort: newest first, regardless of source.
        entries.sort_by(|a, b| {
            let ak = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            let bk = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            bk.cmp(ak)
        });

        let last_updated = entries
            .iter()
            .filter_map(|e| {
                e.get("created_at")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .next()
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        Ok(serde_json::json!({
            "backups": entries,
            "last_updated": last_updated,
        }))
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
    async fn test_list_source_backups_includes_pending_rows_without_s3_location() {
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
        let backups_arr = index
            .get("backups")
            .and_then(|v| v.as_array())
            .expect("response must have a 'backups' array");

        assert_eq!(
            backups_arr.len(),
            1,
            "Expected 1 backup entry (the pending row), got {}; Bug 4 regression: \
             pending rows with empty s3_location were being filtered out",
            backups_arr.len()
        );

        let entry = &backups_arr[0];
        assert_eq!(
            entry.get("state").and_then(|v| v.as_str()),
            Some("pending"),
            "The returned entry should have state='pending'"
        );
        assert_eq!(
            entry.get("id").and_then(|v| v.as_i64()),
            Some(55),
            "The returned entry should have id=55"
        );
    }

    // -------------------------------------------------------------------------
    // TimescaleDB sidecar image selection
    // -------------------------------------------------------------------------
}
