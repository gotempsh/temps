// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared helpers for `engine_v2`-style backup engines.
//!
//! Every engine ported off the queue follows the same shape:
//!
//! 1. Validate params + look up S3 source.
//! 2. Build an S3 client (decrypting credentials at rest).
//! 3. Run a one-shot Docker container (`super::oneshot::run_one_shot`).
//! 4. Upload the resulting file to S3, single-part or multipart.
//! 5. Write a `metadata.json` companion object.
//!
//! Steps 2, 4, and 5 are identical across engines. Pull them in from here
//! so the per-engine code only owns step 3 (its specific Docker command)
//! and the param-validation in step 1.

use std::sync::{Arc, OnceLock};

use aws_sdk_s3::config::SharedHttpClient;
use aws_sdk_s3::Client as S3Client;
use aws_smithy_http_client::tls::{
    rustls_provider::CryptoMode, Provider as TlsProvider, TlsContext, TrustStore,
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, IntoActiveModel, Set};
use serde_json::{json, Value};
use tracing::warn;

use temps_backup_core::engine_v2::BackupError;
use temps_core::EncryptionService;

/// Shared HTTPS client backed by the Mozilla CA bundle compiled in via
/// `webpki-root-certs`. Built once on first use, then reused for every
/// S3 client this crate constructs.
///
/// We bypass the SDK's default-https-client because it asks the OS for
/// trusted roots via `rustls-native-certs`. On some macOS dev machines
/// that returns zero parsed certs and `aws-smithy-http-client` then trips
/// a `debug_assert!`, panicking every test that touches the S3 builder.
/// Pinning a deterministic trust bundle makes the client constructable
/// in any environment (dev macOS, CI sandbox, minimal Linux container)
/// without depending on the OS trust store.
pub(crate) fn bundled_roots_http_client() -> SharedHttpClient {
    static CLIENT: OnceLock<SharedHttpClient> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let mut trust_store = TrustStore::empty().with_native_roots(false);
            for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
                let pem = pem::Pem::new("CERTIFICATE", der.to_vec());
                trust_store = trust_store.with_pem_certificate(pem::encode(&pem).into_bytes());
            }
            let tls_context = TlsContext::builder()
                .with_trust_store(trust_store)
                .build()
                .expect("static TLS context built from bundled roots");
            aws_smithy_http_client::Builder::new()
                .tls_provider(TlsProvider::Rustls(CryptoMode::AwsLc))
                .tls_context(tls_context)
                .build_https()
        })
        .clone()
}

/// Format an AWS SDK error into something a human can act on.
///
/// `Display` on `SdkError` collapses to a useless one-liner like
/// `service error` for any 4xx/5xx — it doesn't include the status code,
/// the request id (which Cloudflare R2/AWS support needs), the
/// service-specific error code (`AccessDenied`, `NoSuchBucket`, …), or
/// the response body. Operators staring at a failed backup deserve all
/// of those; this helper pulls them out via the typed
/// `ProvideErrorMetadata` trait and falls back to `Debug` for
/// transport-layer errors that don't carry SDK metadata.
///
/// Returned string is the operator-facing description; goes verbatim into
/// `backups.error_message` and bubbles up through the UI.
pub fn describe_sdk_error<E>(op: &str, err: &aws_sdk_s3::error::SdkError<E>) -> String
where
    E: std::fmt::Debug + aws_sdk_s3::error::ProvideErrorMetadata,
{
    use aws_sdk_s3::error::SdkError;
    use aws_sdk_s3::operation::RequestId;

    // Pieces we'll join with " | " so a single-line DB column stays
    // readable. Only push parts that actually carry information.
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{} failed", op));

    match err {
        SdkError::ConstructionFailure(_) => {
            parts.push("request construction failure".into());
        }
        SdkError::TimeoutError(_) => {
            parts.push("request timed out (operation-level)".into());
        }
        SdkError::DispatchFailure(d) => {
            // Network / TLS / DNS. Display gives "dispatch failure"; the
            // wrapped error has the actual cause.
            parts.push(format!("dispatch failure: {:?}", d));
        }
        SdkError::ResponseError(r) => {
            // Could not even parse the HTTP response. Surface what we have.
            parts.push(format!("invalid response: {:?}", r));
        }
        SdkError::ServiceError(s) => {
            // Typed service error: 4xx/5xx with a parsed XML body.
            let raw = s.err();
            let resp = s.raw();
            parts.push(format!("HTTP {}", resp.status().as_u16()));
            if let Some(code) = raw.code() {
                parts.push(format!("code={}", code));
            }
            if let Some(msg) = raw.message() {
                parts.push(format!("message={}", msg));
            }
            if let Some(rid) = raw.meta().request_id() {
                parts.push(format!("request_id={}", rid));
            }
            // Extended request id (`x-amz-id-2`) — AWS support asks for
            // this. Cloudflare R2 doesn't emit one, so it's optional.
            if let Some(eid) = resp.headers().get("x-amz-id-2") {
                parts.push(format!("extended_request_id={}", eid));
            }
            // Last resort: include the (truncated) response body so the
            // raw XML/JSON is visible. Storage providers sometimes put
            // diagnostic detail there that the SDK doesn't surface as
            // typed fields.
            if let Some(body_bytes) = resp.body().bytes() {
                if !body_bytes.is_empty() {
                    let body_str = String::from_utf8_lossy(body_bytes);
                    let trimmed = body_str.trim();
                    if !trimmed.is_empty() {
                        const MAX_BODY: usize = 512;
                        let body_excerpt: String = if trimmed.chars().count() > MAX_BODY {
                            let mut s: String = trimmed.chars().take(MAX_BODY).collect();
                            s.push('…');
                            s
                        } else {
                            trimmed.to_string()
                        };
                        parts.push(format!("body={}", body_excerpt));
                    }
                }
            }
        }
        _ => {
            // Future-proof: SdkError is #[non_exhaustive].
            parts.push(format!("{:?}", err));
        }
    }

    parts.join(" | ")
}

/// Multipart upload threshold. Files larger than this use multipart
/// upload instead of a single PUT.
pub const MULTIPART_THRESHOLD: i64 = 30 * 1024 * 1024;

/// S3 object tags applied to every backup upload. These drive tag-based
/// `BucketLifecycleConfiguration` rules so S3 (or compatible storage)
/// expires backups even when temps is offline.
///
/// Tag keys are namespaced with `temps-` so user-managed rules on the
/// same bucket don't collide. Values are kept simple (digits/words) to
/// avoid URL-encoding surprises across providers.
#[derive(Debug, Clone, Default)]
pub struct BackupTags {
    /// Retention in days. `None` means the backup is kept indefinitely
    /// from S3's perspective — only app-side deletion removes it.
    pub retention_days: Option<i32>,
    /// The schedule that produced this backup, if any.
    pub schedule_id: Option<i32>,
    /// The backup row id, for traceability in the S3 console.
    pub backup_id: Option<i32>,
}

impl BackupTags {
    /// Load tag context for `backup_id` from the database. Looks up the
    /// backup row to find `schedule_id`, then resolves
    /// `schedule.retention_period`. Ad-hoc backups (no schedule) get
    /// `retention_days = None` which renders as `temps-retention-days=never`.
    /// Returns a best-effort tag set even on partial DB failure — tagging
    /// is observability/lifecycle plumbing, never a reason to fail the
    /// upload.
    pub async fn load_for_backup(db: &sea_orm::DatabaseConnection, backup_id: i32) -> Self {
        use sea_orm::EntityTrait;
        let mut tags = BackupTags {
            retention_days: None,
            schedule_id: None,
            backup_id: Some(backup_id),
        };
        let backup = match temps_entities::backups::Entity::find_by_id(backup_id)
            .one(db)
            .await
        {
            Ok(Some(b)) => b,
            _ => return tags,
        };
        let Some(sched_id) = backup.schedule_id else {
            return tags;
        };
        tags.schedule_id = Some(sched_id);
        if let Ok(Some(s)) = temps_entities::backup_schedules::Entity::find_by_id(sched_id)
            .one(db)
            .await
        {
            if s.retention_period > 0 {
                tags.retention_days = Some(s.retention_period);
            }
        }
        tags
    }

    /// Structured form of the tag set. Used by the post-upload
    /// `PutObjectTagging` path (see `apply_object_tags`) because some
    /// S3-compatible stores — notably Cloudflare R2 — reject the
    /// `x-amz-tagging` request header on PutObject / CreateMultipartUpload
    /// with `501 NotImplemented`. Applying tags as a separate call works
    /// everywhere, which is why this is the only tag-rendering path: do
    /// not re-introduce a `to_tagging_string` helper for the upload header.
    pub fn to_tag_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = Vec::with_capacity(4);
        pairs.push(("temps-managed".to_string(), "true".to_string()));
        match self.retention_days {
            Some(days) if days > 0 => {
                pairs.push(("temps-retention-days".to_string(), days.to_string()));
            }
            _ => {
                pairs.push(("temps-retention-days".to_string(), "never".to_string()));
            }
        }
        if let Some(id) = self.schedule_id {
            pairs.push(("temps-schedule-id".to_string(), id.to_string()));
        }
        if let Some(id) = self.backup_id {
            pairs.push(("temps-backup-id".to_string(), id.to_string()));
        }
        pairs
    }
}

/// Apply tags to an S3 object **after** upload via `PutObjectTagging`.
///
/// History: we originally passed the tag set as the `Tagging` header on
/// the upload call itself. Cloudflare R2 returns `501 NotImplemented` on
/// that header for both `PutObject` and `CreateMultipartUpload`. Moving
/// to a follow-up `PutObjectTagging` call didn't help either — R2
/// returns the same `501 NotImplemented` on `PutObjectTagging`. Object
/// tagging is simply not implemented on R2.
///
/// So this call is **best-effort**: if the provider rejects it with a
/// "not implemented / not supported" style error, we log a warning and
/// continue. The backup data is already uploaded and tracked in our DB,
/// and app-side `enforce_retention` handles cleanup regardless. The only
/// thing that gets disabled on tag-less providers is the bucket-side
/// `BucketLifecycleConfiguration` reconciler that depends on tag filters
/// — which is also already best-effort (see `s3_lifecycle.rs`).
///
/// On AWS S3 / MinIO / any compliant store this still applies tags
/// normally and fails the backup if tagging is genuinely broken (auth,
/// network, etc.) so we don't silently drop diagnostic plumbing.
pub async fn apply_object_tags(
    client: &S3Client,
    bucket: &str,
    key: &str,
    tags: &BackupTags,
) -> Result<(), BackupError> {
    let mut tag_set_builder = aws_sdk_s3::types::Tagging::builder();
    for (k, v) in tags.to_tag_pairs() {
        let tag = aws_sdk_s3::types::Tag::builder()
            .key(k)
            .value(v)
            .build()
            .map_err(|e| BackupError::Failed {
                reason: format!("failed to build tag for s3://{}/{}: {}", bucket, key, e),
            })?;
        tag_set_builder = tag_set_builder.tag_set(tag);
    }
    let tagging = tag_set_builder.build().map_err(|e| BackupError::Failed {
        reason: format!(
            "failed to build Tagging payload for s3://{}/{}: {}",
            bucket, key, e
        ),
    })?;

    match client
        .put_object_tagging()
        .bucket(bucket)
        .key(key)
        .tagging(tagging)
        .send()
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => {
            let detail = describe_sdk_error(
                &format!("put_object_tagging on s3://{}/{}", bucket, key),
                &e,
            );
            if crate::services::s3_lifecycle::is_unsupported_error(&detail) {
                // Cloudflare R2 (and any other store without
                // PutObjectTagging) lands here. Don't fail the backup;
                // app-side retention (see `BackupService::enforce_retention`)
                // is the source of truth on these providers.
                warn!(
                    target: "temps_backup::tagging",
                    bucket = bucket,
                    key = key,
                    detail = %detail,
                    "S3 provider does not support PutObjectTagging — object stored, tags skipped; relying on app-side retention",
                );
                Ok(())
            } else {
                Err(BackupError::Failed { reason: detail })
            }
        }
    }
}

// ── S3 client construction ───────────────────────────────────────────────────

/// Load an S3 source row from the database. Maps not-found and DB errors
/// onto `BackupError` variants — not-found is permanent, DB-down is
/// transient.
pub async fn load_s3_source(
    db: &sea_orm::DatabaseConnection,
    s3_source_id: i32,
) -> Result<temps_entities::s3_sources::Model, BackupError> {
    use sea_orm::EntityTrait;
    temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
        .one(db)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!(
                "database error looking up s3_source {}: {}",
                s3_source_id, e
            ),
        })?
        .ok_or_else(|| BackupError::PermanentFailure {
            reason: format!("s3_source {} not found", s3_source_id),
        })
}

/// Return the stable public UUID assigned to the parent backup row.
///
/// Engines must use this value as their snapshot directory. Generating a new
/// UUID inside an engine makes retries write multiple snapshots and prevents
/// the deletion path from proving that an S3 prefix belongs to the row being
/// deleted.
pub async fn load_backup_uuid(
    db: &DatabaseConnection,
    backup_id: i32,
) -> Result<String, BackupError> {
    temps_entities::backups::Entity::find_by_id(backup_id)
        .one(db)
        .await
        .map_err(|error| BackupError::Failed {
            reason: format!("db error loading backup {}: {}", backup_id, error),
        })?
        .map(|backup| backup.backup_id)
        .ok_or_else(|| BackupError::PermanentFailure {
            reason: format!("backup {} not found", backup_id),
        })
}

/// Persist the exact WAL-G sentinel selector used for this backup.
/// Legacy rows without this marker are deliberately not individually
/// deletable because their shared repository cannot be mapped to one row.
pub async fn record_walg_identity(
    db: &DatabaseConnection,
    backup_id: i32,
    backup_uuid: &str,
) -> Result<(), BackupError> {
    let backup = temps_entities::backups::Entity::find_by_id(backup_id)
        .one(db)
        .await
        .map_err(|error| BackupError::Failed {
            reason: format!("db error loading WAL-G backup {}: {}", backup_id, error),
        })?
        .ok_or_else(|| BackupError::PermanentFailure {
            reason: format!("backup {} not found", backup_id),
        })?;
    let mut metadata =
        serde_json::from_str::<serde_json::Value>(&backup.metadata).map_err(|error| {
            BackupError::PermanentFailure {
                reason: format!(
                    "backup {} metadata is invalid JSON; WAL-G identity was not persisted: {}",
                    backup_id, error
                ),
            }
        })?;
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| BackupError::PermanentFailure {
            reason: format!("backup {} metadata is not a JSON object", backup_id),
        })?;
    object.insert("walg_identity_version".to_string(), serde_json::json!(1));
    object.insert(
        "walg_target_user_data".to_string(),
        serde_json::json!({ "temps_backup_id": backup_uuid }),
    );
    object.insert("walg_full_backup".to_string(), serde_json::json!(true));

    let mut active = backup.into_active_model();
    active.metadata = Set(metadata.to_string());
    active
        .update(db)
        .await
        .map_err(|error| BackupError::Failed {
            reason: format!(
                "failed to persist WAL-G identity for backup {}: {}",
                backup_id, error
            ),
        })?;
    Ok(())
}

/// Environment that makes one WAL-G snapshot independently deletable.
/// Explicitly disabling deltas overrides any inherited container setting;
/// otherwise deleting one target may also delete newer dependent snapshots.
pub fn walg_identity_env(backup_uuid: &str) -> [String; 2] {
    [
        "WALG_DELTA_MAX_STEPS=0".to_string(),
        format!(
            "WALG_SENTINEL_USER_DATA={}",
            json!({ "temps_backup_id": backup_uuid })
        ),
    ]
}

/// Decrypt the optional STS session token on an S3 source row.
///
/// `Ok(None)` for every long-lived, operator-configured credential — which is
/// every source that existed before Cloud-managed backup destinations — so
/// callers can hand the result straight to
/// `aws_sdk_s3::config::Credentials::new`'s third argument.
pub fn decrypt_session_token(
    s3_source: &temps_entities::s3_sources::Model,
    encryption_service: &Arc<EncryptionService>,
) -> Result<Option<String>, BackupError> {
    temps_entities::s3_sources::decrypt_session_token(encryption_service, s3_source).map_err(|e| {
        BackupError::PermanentFailure {
            reason: format!("failed to decrypt S3 session token: {}", e),
        }
    })
}

/// Build an S3 client from an already-loaded S3 source row. Decrypts the
/// access/secret keys via the supplied `EncryptionService` at call time —
/// the engine never holds plaintext credentials beyond this point.
pub fn build_s3_client(
    s3_source: &temps_entities::s3_sources::Model,
    encryption_service: &Arc<EncryptionService>,
    user_agent: &'static str,
) -> Result<S3Client, BackupError> {
    let access_key = encryption_service
        .decrypt_string(&s3_source.access_key_id)
        .map_err(|e| BackupError::PermanentFailure {
            reason: format!("failed to decrypt S3 access key: {}", e),
        })?;
    let secret_key = encryption_service
        .decrypt_string(&s3_source.secret_key)
        .map_err(|e| BackupError::PermanentFailure {
            reason: format!("failed to decrypt S3 secret key: {}", e),
        })?;
    // `None` for a long-lived credential, which leaves the signer behaving
    // exactly as it always has. `Some` only for a temporary (prefix-scoped)
    // credential, which SigV4 rejects unless the token is signed alongside it.
    let session_token = decrypt_session_token(s3_source, encryption_service)?;

    let creds = aws_sdk_s3::config::Credentials::new(
        access_key,
        secret_key,
        session_token,
        None,
        user_agent,
    );

    let builder = s3_config_builder(
        &s3_source.region,
        s3_source.force_path_style.unwrap_or(true),
        creds,
        s3_source.endpoint.as_deref(),
    );
    Ok(S3Client::from_conf(builder.build()))
}

/// Base configuration for every S3 client that talks to an operator's
/// backup destination. Callers add per-use settings (timeouts) on top.
///
/// Request checksums are computed only when an operation requires one.
/// The SDK's default (`WhenSupported`) adds a CRC32 to every
/// `PutObject`/`UploadPart`, and for a streaming body (a backup file read
/// from disk) sends it as an `aws-chunked` trailer with
/// `x-amz-content-sha256: STREAMING-UNSIGNED-PAYLOAD-TRAILER`. Many
/// S3-compatible stores don't implement that encoding: they compare the
/// sentinel against the body's real SHA-256 and reject the upload with
/// `XAmzContentSHA256Mismatch`, on every retry. With `WhenRequired` the
/// upload is a plain SigV4 PUT, which AWS and every compatible store
/// accept; operations that do require a checksum (`DeleteObjects`,
/// `PutBucketLifecycleConfiguration`) still get one.
///
/// Uploads still carry an integrity header: `Content-MD5`, added by
/// [`ContentMd5Interceptor`] for in-memory bodies and by
/// [`upload_single_part`] for a file streamed from disk. A bucket with a
/// default Object Lock retention period rejects an upload with neither
/// `Content-MD5` nor a flexible checksum, and `Content-MD5` is the one
/// every S3-compatible store accepts.
pub(crate) fn s3_config_builder(
    region: &str,
    force_path_style: bool,
    credentials: aws_sdk_s3::config::Credentials,
    endpoint: Option<&str>,
) -> aws_sdk_s3::config::Builder {
    let mut builder = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(region.to_string()))
        .force_path_style(force_path_style)
        .credentials_provider(credentials)
        .http_client(bundled_roots_http_client())
        .request_checksum_calculation(aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired)
        .interceptor(ContentMd5Interceptor);

    if let Some(endpoint) = endpoint {
        builder = builder.endpoint_url(crate::services::s3_probe::endpoint_url(endpoint));
    }

    builder
}

/// Adds `Content-MD5` to every `PutObject` and `UploadPart` whose body is
/// already in memory: metadata, backup indexes, multipart parts.
///
/// With checksums computed only when required (see [`s3_config_builder`]),
/// neither operation would otherwise carry an integrity header, and a
/// bucket with a default Object Lock retention period rejects such an
/// upload. Living on the client rather than at each call site means a new
/// upload can't forget it. A body streamed from disk has no bytes to hash
/// here; [`upload_single_part`] computes its digest before sending.
#[derive(Debug)]
struct ContentMd5Interceptor;

impl aws_sdk_s3::config::Intercept for ContentMd5Interceptor {
    fn name(&self) -> &'static str {
        "ContentMd5Interceptor"
    }

    fn modify_before_signing(
        &self,
        context: &mut aws_sdk_s3::config::interceptors::BeforeTransmitInterceptorContextMut<'_>,
        _runtime_components: &aws_sdk_s3::config::RuntimeComponents,
        cfg: &mut aws_sdk_s3::config::ConfigBag,
    ) -> Result<(), aws_smithy_runtime_api::box_error::BoxError> {
        let is_upload = cfg
            .load::<aws_smithy_runtime_api::client::orchestrator::Metadata>()
            .is_some_and(|metadata| matches!(metadata.name(), "PutObject" | "UploadPart"));
        if !is_upload {
            return Ok(());
        }
        let request = context.request_mut();
        let has_integrity_header = request
            .headers()
            .iter_bytes()
            .any(|(name, _)| name == "content-md5" || name.starts_with("x-amz-checksum-"));
        if has_integrity_header {
            return Ok(());
        }
        let Some(body) = request.body().bytes() else {
            return Ok(());
        };
        let digest = content_md5(body);
        request
            .headers_mut()
            .try_insert("content-md5", digest)
            .map_err(|e| format!("failed to set Content-MD5 on an S3 upload: {}", e))?;
        Ok(())
    }
}

/// Base64 MD5 digest of `bytes`, in the form the `Content-MD5` header takes.
fn content_md5(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(md5::compute(bytes).0)
}

/// `Content-MD5` of the file at `path`, read in fixed-size chunks so memory
/// stays bounded whatever the file's size.
async fn file_content_md5(path: &str) -> Result<String, BackupError> {
    use base64::Engine as _;
    use tokio::io::AsyncReadExt;

    let read_error = |e: std::io::Error| BackupError::Failed {
        reason: format!("failed to read {} to compute its Content-MD5: {}", path, e),
    };
    let mut file = tokio::fs::File::open(path).await.map_err(read_error)?;
    let mut context = md5::Context::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).await.map_err(read_error)?;
        if read == 0 {
            break;
        }
        context.consume(&buffer[..read]);
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(context.finalize().0))
}

/// Convenience wrapper: load the row + build the client in one call. Most
/// engines use this; only engines that need to inspect the row outside the
/// client (e.g. for the bucket name) call `load_s3_source` + `build_s3_client`
/// separately.
pub async fn load_and_build_s3_client(
    db: &sea_orm::DatabaseConnection,
    encryption_service: &Arc<EncryptionService>,
    s3_source_id: i32,
    user_agent: &'static str,
) -> Result<(temps_entities::s3_sources::Model, S3Client), BackupError> {
    let row = load_s3_source(db, s3_source_id).await?;
    let client = build_s3_client(&row, encryption_service, user_agent)?;
    Ok((row, client))
}

/// HEAD-bucket reachability check — cheap, fails fast on misconfigured S3
/// credentials or unreachable endpoint.
pub async fn assert_bucket_reachable(client: &S3Client, bucket: &str) -> Result<(), BackupError> {
    client
        .head_bucket()
        .bucket(bucket)
        .send()
        .await
        .map_err(|e| BackupError::Failed {
            reason: describe_sdk_error(&format!("head_bucket on '{}'", bucket), &e),
        })?;
    Ok(())
}

// ── S3 key derivation ────────────────────────────────────────────────────────

/// Build a dump S3 key for a **control-plane** backup.
///
/// Pattern: `<bucket_path>/backups/YYYY/MM/DD/<uuid>/<filename>`
pub fn build_dump_s3_key(bucket_path: &str, backup_uuid: &str, filename: &str) -> String {
    let prefix = bucket_path.trim_matches('/');
    let date = Utc::now().format("%Y/%m/%d");
    if prefix.is_empty() {
        format!("backups/{}/{}/{}", date, backup_uuid, filename)
    } else {
        format!("{}/backups/{}/{}/{}", prefix, date, backup_uuid, filename)
    }
}

/// Build a dump S3 key for an **external service** backup.
///
/// Pattern: `<bucket_path>/external_services/<engine>/<service_name>/YYYY/MM/DD/<uuid>/<filename>`
///
/// The per-engine sub-prefix (`postgres`, `redis`, `mongodb`, ...) lives in
/// `engine`. Including the uuid in the path means concurrent or same-day
/// backups of the same service write to distinct keys, so the
/// idempotent-skip check in `upload_*` only fires for genuine resumes.
pub fn build_external_service_s3_key(
    bucket_path: &str,
    engine: &str,
    service_name: &str,
    backup_uuid: &str,
    filename: &str,
) -> String {
    let prefix = bucket_path.trim_matches('/');
    let date = Utc::now().format("%Y/%m/%d");
    if prefix.is_empty() {
        format!(
            "external_services/{}/{}/{}/{}/{}",
            engine, service_name, date, backup_uuid, filename
        )
    } else {
        format!(
            "{}/external_services/{}/{}/{}/{}/{}",
            prefix, engine, service_name, date, backup_uuid, filename
        )
    }
}

/// Derive the `metadata.json` companion key from a dump key by replacing
/// the last path segment with `metadata.json`.
pub fn derive_metadata_key(dump_key: &str) -> String {
    let parts: Vec<&str> = dump_key.rsplitn(2, '/').collect();
    if parts.len() == 2 {
        format!("{}/metadata.json", parts[1])
    } else {
        format!("{}.metadata.json", dump_key)
    }
}

// ── Shell escaping ───────────────────────────────────────────────────────────

/// POSIX-safe single-quote escape for embedding in `sh -c` strings.
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ── S3 upload ────────────────────────────────────────────────────────────────

/// Single-part PUT upload. Use for files under [`MULTIPART_THRESHOLD`].
pub async fn upload_single_part(
    client: &S3Client,
    bucket: &str,
    key: &str,
    path: &str,
    content_type: &str,
    tags: Option<&BackupTags>,
) -> Result<(), BackupError> {
    // The body is streamed from disk, so `ContentMd5Interceptor` can't hash
    // it; the digest is computed here, one pass over a file that is at most
    // `MULTIPART_THRESHOLD` bytes.
    let content_md5 = file_content_md5(path).await?;
    let body = aws_sdk_s3::primitives::ByteStream::from_path(std::path::Path::new(path))
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("failed to create byte stream from {}: {}", path, e),
        })?;

    // Tags are applied via PutObjectTagging *after* the upload — see
    // `apply_object_tags` for the R2-compatibility rationale. We
    // deliberately do not pass `.tagging(...)` here.
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(body)
        .content_md5(content_md5)
        .content_type(content_type)
        .send()
        .await
        .map_err(|e| BackupError::Failed {
            reason: describe_sdk_error(
                &format!("single-part upload to s3://{}/{}", bucket, key),
                &e,
            ),
        })?;

    if let Some(tags) = tags {
        apply_object_tags(client, bucket, key, tags).await?;
    }
    Ok(())
}

/// Smallest part S3 and every compatible store accept for all but the last
/// part of a multipart upload.
pub const MIN_MULTIPART_PART_SIZE: u64 = 5 * 1024 * 1024;

/// Hard cap on parts per multipart upload (S3, R2, MinIO all enforce
/// 10,000). At a fixed 5 MiB part this used to cap every backup at 50 GB:
/// part 10,001 was rejected and the whole upload aborted.
pub const MAX_MULTIPART_PARTS: u64 = 10_000;

/// How many times one part, or the final completion call, is attempted
/// before the upload is abandoned. Between attempts the parts already
/// accepted by the store stay put, so a failure at 90% of a 200 GB upload
/// resumes at 90% rather than at zero.
pub const MULTIPART_PART_ATTEMPTS: u32 = 6;

/// Part size for a file of `file_size` bytes: at least
/// [`MIN_MULTIPART_PART_SIZE`], and large enough that the upload needs no
/// more than [`MAX_MULTIPART_PARTS`] parts. Rounded up to a whole MiB so
/// part boundaries are predictable in the store's `ListParts` output. One
/// part is the upload's whole memory footprint, so this is also the
/// memory bound: a 200 GB file uploads in 20 MiB parts, a 2 TB file in
/// 200 MiB parts.
pub fn multipart_part_size(file_size: u64) -> u64 {
    const MIB: u64 = 1024 * 1024;
    let needed = file_size.div_ceil(MAX_MULTIPART_PARTS);
    let rounded = needed.div_ceil(MIB).saturating_mul(MIB);
    rounded.max(MIN_MULTIPART_PART_SIZE)
}

/// Whether a failed S3 call is worth retrying with the same input.
///
/// Transport failures, timeouts, unparsable responses, throttling and
/// server-side errors are; a request the store understood and rejected
/// (`AccessDenied`, `NoSuchBucket`, `NoSuchUpload`, `InvalidPart`, …) is
/// not, because it will be rejected identically on every attempt.
pub fn is_retryable_sdk_error<E>(err: &aws_sdk_s3::error::SdkError<E>) -> bool
where
    E: std::fmt::Debug + aws_sdk_s3::error::ProvideErrorMetadata,
{
    use aws_sdk_s3::error::SdkError;
    match err {
        SdkError::ConstructionFailure(_) => false,
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_) => {
            true
        }
        SdkError::ServiceError(s) => {
            let status = s.raw().status().as_u16();
            status >= 500
                || status == 429
                || status == 408
                || matches!(
                    s.err().code(),
                    Some("SlowDown" | "RequestTimeout" | "InternalError" | "ServiceUnavailable")
                )
        }
        // `SdkError` is `#[non_exhaustive]`; an unknown variant is treated
        // as transient so a future SDK cannot silently turn every failure
        // permanent.
        _ => true,
    }
}

/// Run one S3 call up to [`MULTIPART_PART_ATTEMPTS`] times with exponential
/// backoff, stopping early on a non-retryable error or when `cancel`
/// fires. `describe` names the call in the error an operator eventually
/// sees.
async fn s3_call_with_retry<T, E, F, Fut>(
    describe: &str,
    cancel: &tokio_util::sync::CancellationToken,
    mut call: F,
) -> Result<T, BackupError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, aws_sdk_s3::error::SdkError<E>>>,
    E: std::fmt::Debug + aws_sdk_s3::error::ProvideErrorMetadata,
{
    let backoff = temps_core::retry::RetryConfig::new(MULTIPART_PART_ATTEMPTS)
        .with_base_delay(std::time::Duration::from_secs(2))
        .with_max_delay(std::time::Duration::from_secs(60));
    let mut attempt = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Err(BackupError::Cancelled);
        }
        match call().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                attempt += 1;
                let retryable = is_retryable_sdk_error(&err);
                if !retryable || attempt >= MULTIPART_PART_ATTEMPTS {
                    return Err(BackupError::Failed {
                        reason: format!(
                            "{} (attempt {}/{}{})",
                            describe_sdk_error(describe, &err),
                            attempt,
                            MULTIPART_PART_ATTEMPTS,
                            if retryable { "" } else { ", not retryable" }
                        ),
                    });
                }
                let delay = backoff.compute_delay(attempt - 1);
                warn!(
                    op = %describe,
                    attempt,
                    max_attempts = MULTIPART_PART_ATTEMPTS,
                    retry_in_secs = delay.as_secs(),
                    error = %describe_sdk_error(describe, &err),
                    "S3 call failed; retrying without discarding uploaded parts"
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = cancel.cancelled() => return Err(BackupError::Cancelled),
                }
            }
        }
    }
}

/// Multipart upload. Use for files over [`MULTIPART_THRESHOLD`].
///
/// Every part is retried on its own (see [`s3_call_with_retry`]) and the
/// parts the store already accepted are kept between attempts: S3
/// multipart *is* the resume protocol, the store holds completed parts
/// server-side under `upload_id` until the upload is completed or
/// aborted. The upload is aborted only once a part is given up on, on a
/// non-retryable rejection, or on cancel, so a bucket never accumulates
/// orphaned parts from this code path. (Parts orphaned by a process crash
/// are the bucket lifecycle rule's job; nothing here can reach them.)
///
/// Memory is bounded by one part ([`multipart_part_size`]) regardless of
/// file size.
pub async fn upload_multipart(
    client: &S3Client,
    bucket: &str,
    key: &str,
    path: &str,
    content_type: &str,
    tags: Option<&BackupTags>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), BackupError> {
    let file_size = tokio::fs::metadata(path)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("failed to stat {} for multipart upload: {}", path, e),
        })?
        .len();
    let part_size = multipart_part_size(file_size);
    let part_size_usize = usize::try_from(part_size).map_err(|_| BackupError::Failed {
        reason: format!(
            "multipart part size {} bytes does not fit this platform's address space",
            part_size
        ),
    })?;

    // Tags are applied via PutObjectTagging *after* the upload completes
    // — see `apply_object_tags` for the R2-compatibility rationale. We
    // deliberately do not pass `.tagging(...)` on the create call here;
    // doing so makes Cloudflare R2 fail the upload with 501 NotImplemented.
    let create_resp = s3_call_with_retry(
        &format!("create_multipart_upload for s3://{}/{}", bucket, key),
        cancel,
        || {
            client
                .create_multipart_upload()
                .bucket(bucket)
                .key(key)
                .content_type(content_type)
                .send()
        },
    )
    .await?;
    let upload_id = create_resp
        .upload_id()
        .ok_or_else(|| BackupError::Failed {
            reason: "create_multipart_upload returned no upload_id".into(),
        })?
        .to_string();

    let outcome = upload_multipart_parts(
        client,
        bucket,
        key,
        path,
        &upload_id,
        part_size_usize,
        file_size,
        cancel,
    )
    .await;
    if let Err(error) = outcome {
        abort_multipart_detached(client.clone(), bucket, key, &upload_id);
        return Err(error);
    }

    if let Some(tags) = tags {
        apply_object_tags(client, bucket, key, tags).await?;
    }
    Ok(())
}

/// The part loop of [`upload_multipart`], separated so every error path
/// aborts `upload_id` exactly once, in the caller.
#[allow(clippy::too_many_arguments)]
async fn upload_multipart_parts(
    client: &S3Client,
    bucket: &str,
    key: &str,
    path: &str,
    upload_id: &str,
    part_size: usize,
    file_size: u64,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), BackupError> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!("failed to open {} for multipart upload: {}", path, e),
        })?;

    let mut parts = aws_sdk_s3::types::CompletedMultipartUpload::builder();
    let mut part_number = 1i32;
    let mut uploaded_bytes = 0u64;
    loop {
        // Fill exactly one part (or whatever is left of the file).
        let mut buffer = Vec::with_capacity(part_size);
        while buffer.len() < part_size {
            let read = file
                .read_buf(&mut buffer)
                .await
                .map_err(|e| BackupError::Failed {
                    reason: format!("read error during multipart upload of {}: {}", path, e),
                })?;
            if read == 0 {
                break;
            }
        }
        if buffer.is_empty() {
            break;
        }
        let part_len = buffer.len() as u64;
        // `Bytes` clones are reference-counted, so a retry re-sends the
        // same part without a second copy of it in memory.
        let data = bytes::Bytes::from(buffer);
        let describe = format!(
            "upload_part {} ({} bytes at offset {}) for s3://{}/{}",
            part_number, part_len, uploaded_bytes, bucket, key
        );
        let part_resp = s3_call_with_retry(&describe, cancel, || {
            client
                .upload_part()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(part_number)
                .body(aws_sdk_s3::primitives::ByteStream::from(data.clone()))
                .send()
        })
        .await?;
        let e_tag = part_resp.e_tag().ok_or_else(|| BackupError::Failed {
            reason: format!(
                "upload_part {} for s3://{}/{} returned no ETag; the store cannot complete \
                 an upload whose parts it does not identify",
                part_number, bucket, key
            ),
        })?;
        parts = parts.parts(
            aws_sdk_s3::types::CompletedPart::builder()
                .e_tag(e_tag)
                .part_number(part_number)
                .build(),
        );
        uploaded_bytes += part_len;
        part_number += 1;
        if part_len < part_size as u64 {
            break;
        }
    }

    if uploaded_bytes != file_size {
        return Err(BackupError::Failed {
            reason: format!(
                "{} changed while it was being uploaded: read {} bytes, expected {}",
                path, uploaded_bytes, file_size
            ),
        });
    }

    let completed = parts.build();
    s3_call_with_retry(
        &format!("complete_multipart_upload for s3://{}/{}", bucket, key),
        cancel,
        || {
            client
                .complete_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .multipart_upload(completed.clone())
                .send()
        },
    )
    .await?;
    Ok(())
}

/// Auto-route between single-part and multipart based on file size.
///
/// `cancel` is polled between parts and while backing off before a retry,
/// so a cancelled backup stops uploading within one part rather than at
/// the end of the file.
#[allow(clippy::too_many_arguments)]
pub async fn upload_file(
    client: &S3Client,
    bucket: &str,
    key: &str,
    path: &str,
    content_type: &str,
    file_size: i64,
    tags: Option<&BackupTags>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), BackupError> {
    if file_size > MULTIPART_THRESHOLD {
        upload_multipart(client, bucket, key, path, content_type, tags, cancel).await
    } else {
        upload_single_part(client, bucket, key, path, content_type, tags).await
    }
}

fn abort_multipart_detached(client: S3Client, bucket: &str, key: &str, upload_id: &str) {
    let bucket = bucket.to_string();
    let key = key.to_string();
    let upload_id = upload_id.to_string();
    tokio::spawn(async move {
        if let Err(error) = client
            .abort_multipart_upload()
            .bucket(&bucket)
            .key(&key)
            .upload_id(&upload_id)
            .send()
            .await
        {
            warn!(
                bucket = %bucket,
                key = %key,
                error = %describe_sdk_error("abort_multipart_upload", &error),
                "could not abort an abandoned multipart upload; its parts stay billed until \
                 the bucket's AbortIncompleteMultipartUpload lifecycle rule removes them"
            );
        }
    });
}

// ── Metadata.json companion ──────────────────────────────────────────────────

/// Upload a `metadata.json` companion object next to the dump.
///
/// The body has a uniform shape across engines so the restore path can
/// inspect any dump's metadata without engine-specific decoding.
#[allow(clippy::too_many_arguments)]
pub async fn write_metadata_companion(
    client: &S3Client,
    bucket: &str,
    metadata_key: &str,
    engine: &str,
    backup_uuid: &str,
    dump_key: &str,
    size_bytes: i64,
    s3_source_id: i32,
    compression: &str,
    extra: Option<Value>,
) -> Result<(), BackupError> {
    let mut metadata = json!({
        "backup_uuid": backup_uuid,
        "type": "full",
        "engine": engine,
        "created_at": Utc::now().to_rfc3339(),
        "size_bytes": size_bytes,
        "compression_type": compression,
        "source": { "id": s3_source_id },
        "s3_location": dump_key,
    });
    if let (Some(extra), Some(obj)) = (extra, metadata.as_object_mut()) {
        if let Some(extra_obj) = extra.as_object() {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let body = serde_json::to_vec(&metadata).map_err(|e| BackupError::Failed {
        reason: format!("failed to serialise metadata.json: {}", e),
    })?;

    client
        .put_object()
        .bucket(bucket)
        .key(metadata_key)
        .body(body.into())
        .content_type("application/json")
        .send()
        .await
        .map_err(|e| BackupError::Failed {
            reason: describe_sdk_error(
                &format!("metadata.json upload to s3://{}/{}", bucket, metadata_key),
                &e,
            ),
        })?;
    Ok(())
}

// ── Param helpers ────────────────────────────────────────────────────────────

/// Extract an integer field from `ctx.params`, mapping a missing/bad field
/// to `BackupError::PermanentFailure` (no point retrying with the same
/// params).
pub fn require_i32_param(params: &Value, field: &str) -> Result<i32, BackupError> {
    params
        .get(field)
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .ok_or_else(|| BackupError::PermanentFailure {
            reason: format!("params.{} missing or not an integer", field),
        })
}

// ── Temp-file plumbing ───────────────────────────────────────────────────────

/// Create the engine-shared backup temp directory at
/// `<data_dir>/backups/tmp` and return the path. Idempotent.
pub async fn ensure_backup_tmpdir(
    config_service: &Arc<temps_config::ConfigService>,
) -> Result<std::path::PathBuf, BackupError> {
    let dir = config_service.data_dir().join("backups").join("tmp");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| BackupError::Failed {
            reason: format!(
                "failed to create backup temp directory {}: {}",
                dir.display(),
                e
            ),
        })?;
    Ok(dir)
}

/// Best-effort `unlink` of a path. Logs (and ignores) any failure — used
/// on cleanup paths where we must not turn a cleanup failure into a backup
/// failure.
pub async fn best_effort_remove(path: &std::path::Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(path = %path.display(), error = %e, "best_effort_remove: unlink failed (non-fatal)");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{decrypt_session_token, walg_identity_env};
    use temps_core::EncryptionService;

    fn s3_source_row(session_token: Option<String>) -> temps_entities::s3_sources::Model {
        let now = chrono::Utc::now();
        temps_entities::s3_sources::Model {
            id: 1,
            backing_service_id: None,
            name: "operator-source".to_string(),
            bucket_name: "backups".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_path: "prod".to_string(),
            access_key_id: "ciphertext".to_string(),
            secret_key: "ciphertext".to_string(),
            session_token,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: true,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Every engine calls this before signing. For an operator-configured
    /// source it must return `None`, which is what keeps
    /// `Credentials::new(.., None, ..)` — the pre-existing behaviour — in
    /// force for the installs already running their own S3 credentials.
    #[test]
    fn a_source_without_a_session_token_decrypts_to_none() {
        let encryption = Arc::new(EncryptionService::new_from_password("v2-common-tests"));
        assert!(decrypt_session_token(&s3_source_row(None), &encryption)
            .expect("no token is not an error")
            .is_none());
    }

    #[test]
    fn a_source_with_a_session_token_decrypts_it_for_the_signer() {
        let encryption = Arc::new(EncryptionService::new_from_password("v2-common-tests"));
        let sealed = encryption
            .encrypt_string("sts-session-token")
            .expect("encrypt");
        assert_eq!(
            decrypt_session_token(&s3_source_row(Some(sealed)), &encryption)
                .expect("decrypt")
                .as_deref(),
            Some("sts-session-token")
        );
    }

    /// A corrupt or wrong-key token is a permanent failure with the source
    /// named, not a silent fallback to an unsigned request that would fail
    /// later with an opaque 403 from the provider.
    #[test]
    fn an_undecryptable_session_token_is_a_contextual_permanent_failure() {
        let encryption = Arc::new(EncryptionService::new_from_password("v2-common-tests"));
        let other = EncryptionService::new_from_password("some-other-key");
        let sealed = other.encrypt_string("sts-session-token").expect("encrypt");

        let error = decrypt_session_token(&s3_source_row(Some(sealed)), &encryption)
            .expect_err("a token sealed with another key must not decode");
        let rendered = error.to_string();
        assert!(rendered.contains("session token"), "{rendered}");
        assert!(rendered.contains("operator-source"), "{rendered}");
    }

    #[test]
    fn walg_identity_env_forces_full_backup_and_exact_user_data() {
        let backup_id = "4dc29e1a-1234-4abc-8def-123456789abc";
        let env = walg_identity_env(backup_id);
        assert_eq!(env[0], "WALG_DELTA_MAX_STEPS=0");
        assert_eq!(
            env[1],
            format!(
                "WALG_SENTINEL_USER_DATA={{\"temps_backup_id\":\"{}\"}}",
                backup_id
            )
        );
    }

    #[test]
    fn multipart_part_size_respects_the_part_count_and_minimum_size() {
        use super::{multipart_part_size, MAX_MULTIPART_PARTS, MIN_MULTIPART_PART_SIZE};
        const MIB: u64 = 1024 * 1024;
        // Small files: the S3 minimum.
        assert_eq!(multipart_part_size(0), MIN_MULTIPART_PART_SIZE);
        assert_eq!(multipart_part_size(31 * MIB), MIN_MULTIPART_PART_SIZE);
        assert_eq!(multipart_part_size(50_000 * MIB), MIN_MULTIPART_PART_SIZE);
        // 200 GB used to need 40,000 five-MiB parts, four times the cap.
        let two_hundred_gb = 200 * 1000 * MIB;
        let part = multipart_part_size(two_hundred_gb);
        assert_eq!(part, 20 * MIB);
        assert!(two_hundred_gb.div_ceil(part) <= MAX_MULTIPART_PARTS);
        // Whole MiB boundaries, never more than the cap, for any size.
        for size in [50_001 * MIB, 123_457 * MIB, 2 * 1024 * 1024 * MIB] {
            let part = multipart_part_size(size);
            assert_eq!(part % MIB, 0);
            assert!(size.div_ceil(part) <= MAX_MULTIPART_PARTS, "{size}");
        }
    }

    /// Minimal S3 multipart stub: create / upload part / complete / abort.
    /// `fail_part` makes the first attempt of that part answer `status`,
    /// which is how a transient outage (503) and a hard rejection (403) are
    /// simulated.
    struct MultipartStub {
        creates: std::sync::atomic::AtomicUsize,
        part_attempts: std::sync::atomic::AtomicUsize,
        completes: std::sync::atomic::AtomicUsize,
        aborts: std::sync::atomic::AtomicUsize,
        fail_part: i32,
        status: axum::http::StatusCode,
        /// How many attempts of `fail_part` answer `status` before it is
        /// accepted. The SDK's own retry layer absorbs a couple of 5xx
        /// answers on its own; failing more often than that is what proves
        /// the resume in `upload_multipart` rather than the SDK's.
        fail_times: usize,
        failures: std::sync::atomic::AtomicUsize,
        parts: std::sync::Mutex<std::collections::BTreeMap<i32, Vec<u8>>>,
    }

    async fn multipart_stub_handler(
        axum::extract::State(state): axum::extract::State<Arc<MultipartStub>>,
        method: axum::http::Method,
        uri: axum::http::Uri,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;
        let query = uri.query().unwrap_or_default();
        let params: std::collections::HashMap<&str, &str> = query
            .split('&')
            .filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
            .collect();
        let xml = |body: String| {
            (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/xml")],
                body,
            )
                .into_response()
        };
        match method {
            axum::http::Method::POST if params.contains_key("uploads") => {
                state.creates.fetch_add(1, Ordering::SeqCst);
                xml(
                    "<InitiateMultipartUploadResult><Bucket>b</Bucket><Key>k</Key>\
                     <UploadId>upload-1</UploadId></InitiateMultipartUploadResult>"
                        .into(),
                )
            }
            axum::http::Method::PUT => {
                state.part_attempts.fetch_add(1, Ordering::SeqCst);
                let part: i32 = params
                    .get("partNumber")
                    .and_then(|value| value.parse().ok())
                    .expect("partNumber");
                if part == state.fail_part
                    && state.failures.fetch_add(1, Ordering::SeqCst) < state.fail_times
                {
                    let code = if state.status.is_server_error() {
                        "SlowDown"
                    } else {
                        "AccessDenied"
                    };
                    return (
                        state.status,
                        [(axum::http::header::CONTENT_TYPE, "application/xml")],
                        format!("<Error><Code>{code}</Code><Message>simulated</Message></Error>"),
                    )
                        .into_response();
                }
                state
                    .parts
                    .lock()
                    .expect("parts lock")
                    .insert(part, body.to_vec());
                (
                    axum::http::StatusCode::OK,
                    [(axum::http::header::ETAG, format!("\"etag-{part}\""))],
                    "",
                )
                    .into_response()
            }
            axum::http::Method::POST => {
                state.completes.fetch_add(1, Ordering::SeqCst);
                xml(
                    "<CompleteMultipartUploadResult><Location>l</Location><Bucket>b</Bucket>\
                     <Key>k</Key><ETag>\"done\"</ETag></CompleteMultipartUploadResult>"
                        .into(),
                )
            }
            axum::http::Method::DELETE => {
                state.aborts.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::NO_CONTENT.into_response()
            }
            _ => axum::http::StatusCode::METHOD_NOT_ALLOWED.into_response(),
        }
    }

    async fn spawn_multipart_stub(
        fail_part: i32,
        fail_times: usize,
        status: axum::http::StatusCode,
    ) -> Option<(aws_sdk_s3::Client, Arc<MultipartStub>)> {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping multipart stub test");
                return None;
            }
            Err(error) => panic!("bind multipart stub: {error}"),
        };
        let address = listener.local_addr().expect("stub address");
        let state = Arc::new(MultipartStub {
            creates: Default::default(),
            part_attempts: Default::default(),
            completes: Default::default(),
            aborts: Default::default(),
            fail_part,
            status,
            fail_times,
            failures: Default::default(),
            parts: Default::default(),
        });
        let app = axum::Router::new()
            .route(
                "/{bucket}/{*key}",
                axum::routing::any(multipart_stub_handler),
            )
            // Parts are 5 MiB; Axum's default body limit is 2 MiB.
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve multipart stub");
        });
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .force_path_style(true)
            .endpoint_url(format!("http://{address}"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test", "test", None, None, "test",
            ))
            .build();
        Some((aws_sdk_s3::Client::from_conf(config), state))
    }

    fn twelve_mib_file() -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        let chunk: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        for _ in 0..12 {
            file.write_all(&chunk).expect("write chunk");
        }
        file.flush().expect("flush");
        file
    }

    /// A transient failure on one part must not throw away the parts the
    /// store already holds: the part is retried, the upload is completed
    /// from where it was, and nothing is aborted.
    #[tokio::test]
    async fn a_transient_part_failure_resumes_the_upload_without_aborting() {
        use std::sync::atomic::Ordering;
        // The SDK retries a 503 a few times on its own before surfacing an
        // error; four failures force one retry through this crate's layer.
        let Some((client, state)) =
            spawn_multipart_stub(2, 4, axum::http::StatusCode::SERVICE_UNAVAILABLE).await
        else {
            return;
        };
        let file = twelve_mib_file();
        let path = file.path().to_str().expect("utf-8 path").to_string();
        let cancel = tokio_util::sync::CancellationToken::new();
        let started = std::time::Instant::now();
        super::upload_multipart(
            &client,
            "b",
            "k",
            &path,
            "application/octet-stream",
            None,
            &cancel,
        )
        .await
        .expect("upload completes after the retry");
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(2),
            "the crate-level retry backs off before re-sending the part"
        );
        assert_eq!(state.creates.load(Ordering::SeqCst), 1);
        // Three parts (5 + 5 + 2 MiB); part 2 needed five attempts.
        assert_eq!(state.part_attempts.load(Ordering::SeqCst), 7);
        assert_eq!(state.completes.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.aborts.load(Ordering::SeqCst),
            0,
            "no part was discarded"
        );
        let parts = state.parts.lock().expect("parts lock");
        let assembled: Vec<u8> = parts.values().flatten().copied().collect();
        assert_eq!(assembled, std::fs::read(&path).expect("file"));
        assert_eq!(parts.len(), 3);
    }

    /// A rejection the store will repeat (403) is not retried: the upload is
    /// abandoned at once and its parts aborted exactly once.
    #[tokio::test]
    async fn a_rejected_part_aborts_the_upload_without_retrying() {
        use std::sync::atomic::Ordering;
        let Some((client, state)) =
            spawn_multipart_stub(1, 1, axum::http::StatusCode::FORBIDDEN).await
        else {
            return;
        };
        let file = twelve_mib_file();
        let path = file.path().to_str().expect("utf-8 path").to_string();
        let cancel = tokio_util::sync::CancellationToken::new();
        let error = super::upload_multipart(
            &client,
            "b",
            "k",
            &path,
            "application/octet-stream",
            None,
            &cancel,
        )
        .await
        .expect_err("a 403 fails the upload");
        assert!(
            error.to_string().contains("not retryable"),
            "the reason says why it stopped: {error}"
        );
        assert_eq!(state.part_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(state.completes.load(Ordering::SeqCst), 0);
        // The abort is spawned detached; give it a moment to land.
        for _ in 0..50 {
            if state.aborts.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(state.aborts.load(Ordering::SeqCst), 1);
    }

    /// What a stub store saw for one upload request: the headers the SDK
    /// signed and the bytes that arrived on the wire.
    struct RecordedPut {
        headers: axum::http::HeaderMap,
        body: Vec<u8>,
    }

    type RecordedPuts = Arc<std::sync::Mutex<Vec<RecordedPut>>>;

    /// Records each `PUT`, so a test can check how the request was encoded
    /// rather than only that it succeeded, and enforces what a bucket with
    /// a default Object Lock retention period enforces: an upload with
    /// neither `Content-MD5` nor a flexible checksum is rejected, and so is
    /// one whose `Content-MD5` doesn't match the body.
    async fn recording_stub_handler(
        axum::extract::State(puts): axum::extract::State<RecordedPuts>,
        method: axum::http::Method,
        uri: axum::http::Uri,
        headers: axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let xml = |body: &'static str| {
            (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/xml")],
                body,
            )
                .into_response()
        };
        let rejected = |code: &str, message: &str| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/xml")],
                format!("<Error><Code>{code}</Code><Message>{message}</Message></Error>"),
            )
                .into_response()
        };
        let query = uri.query().unwrap_or_default();
        match method {
            axum::http::Method::PUT => {
                let content_md5 = headers
                    .get("content-md5")
                    .map(|value| value.to_str().expect("ascii header").to_string());
                let has_checksum = headers
                    .keys()
                    .any(|name| name.as_str().starts_with("x-amz-checksum-"));
                let body = body.to_vec();
                let digest_matches = content_md5
                    .as_deref()
                    .map(|md5| md5 == super::content_md5(&body));
                puts.lock()
                    .expect("puts lock")
                    .push(RecordedPut { headers, body });
                match (digest_matches, has_checksum) {
                    (None, false) => rejected(
                        "InvalidRequest",
                        "Content-MD5 OR x-amz-checksum- HTTP header is required for Put \
                         Object requests with Object Lock parameters",
                    ),
                    (Some(false), _) => rejected(
                        "BadDigest",
                        "The Content-MD5 you specified did not match what we received.",
                    ),
                    _ => (
                        axum::http::StatusCode::OK,
                        [(axum::http::header::ETAG, "\"etag\"")],
                        "",
                    )
                        .into_response(),
                }
            }
            axum::http::Method::POST if query.contains("uploads") => xml(
                "<InitiateMultipartUploadResult><Bucket>b</Bucket><Key>k</Key>\
                 <UploadId>upload-1</UploadId></InitiateMultipartUploadResult>",
            ),
            axum::http::Method::POST => xml(
                "<CompleteMultipartUploadResult><Location>l</Location><Bucket>b</Bucket>\
                 <Key>k</Key><ETag>\"done\"</ETag></CompleteMultipartUploadResult>",
            ),
            _ => axum::http::StatusCode::NO_CONTENT.into_response(),
        }
    }

    /// A client built by the production [`super::s3_config_builder`],
    /// pointed at a recording stub store.
    async fn spawn_recording_stub() -> Option<(aws_sdk_s3::Client, RecordedPuts)> {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox denied TCP bind; skipping recording stub test");
                return None;
            }
            Err(error) => panic!("bind recording stub: {error}"),
        };
        let address = listener.local_addr().expect("stub address");
        let puts: RecordedPuts = Default::default();
        let app = axum::Router::new()
            .route(
                "/{bucket}/{*key}",
                axum::routing::any(recording_stub_handler),
            )
            // Parts are 5 MiB; Axum's default body limit is 2 MiB.
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
            .with_state(puts.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve recording stub");
        });
        // A bare `host:port`, the way an operator enters a private endpoint.
        let endpoint = address.to_string();
        let config = super::s3_config_builder(
            "us-east-1",
            true,
            aws_sdk_s3::config::Credentials::new("test", "test", None, None, "test"),
            Some(&endpoint),
        )
        .build();
        Some((aws_sdk_s3::Client::from_conf(config), puts))
    }

    /// The upload must be a plain SigV4 request: no `aws-chunked` body, no
    /// streaming payload sentinel, and no optional flexible checksum.
    /// S3-compatible stores that don't implement the aws-chunked trailer
    /// format reject anything else with `XAmzContentSHA256Mismatch`. It
    /// must still carry a `Content-MD5` of exactly the bytes received,
    /// which a bucket with Object Lock retention requires.
    fn assert_plain_sigv4_put(put: &RecordedPut, what: &str) {
        let header = |name: &str| {
            put.headers
                .get(name)
                .map(|value| value.to_str().expect("ascii header").to_string())
        };
        let content_encoding = header("content-encoding").unwrap_or_default();
        assert!(
            !content_encoding.contains("aws-chunked"),
            "{what} was sent aws-chunked (content-encoding: {content_encoding})"
        );
        let content_sha256 = header("x-amz-content-sha256").expect("signed payload hash");
        assert!(
            !content_sha256.starts_with("STREAMING-"),
            "{what} used a streaming payload: x-amz-content-sha256 = {content_sha256}"
        );
        assert_eq!(header("x-amz-trailer"), None, "{what} declared a trailer");
        assert_eq!(
            header("x-amz-sdk-checksum-algorithm"),
            None,
            "{what} requested an optional flexible checksum"
        );
        let checksums: Vec<_> = put
            .headers
            .keys()
            .filter(|name| name.as_str().starts_with("x-amz-checksum-"))
            .collect();
        assert!(
            checksums.is_empty(),
            "{what} carried optional checksum headers {checksums:?}"
        );
        // `content_md5` itself is pinned to reference vectors in
        // `content_md5_matches_reference_digests`.
        assert_eq!(
            header("content-md5"),
            Some(super::content_md5(&put.body)),
            "{what} must carry the Content-MD5 of the bytes the store received"
        );
    }

    #[test]
    fn content_md5_matches_reference_digests() {
        assert_eq!(super::content_md5(b""), "1B2M2Y8AsgTpgAmY7PhCfg==");
        assert_eq!(
            super::content_md5(b"The quick brown fox jumps over the lazy dog"),
            "nhB9nTcrtoJr2B01QqQZ1g=="
        );
    }

    /// The streamed digest of a file spanning many read chunks equals the
    /// digest of its bytes in memory.
    #[tokio::test]
    async fn file_content_md5_matches_the_in_memory_digest() {
        let file = twelve_mib_file();
        let path = file.path().to_str().expect("utf-8 path").to_string();
        let streamed = super::file_content_md5(&path)
            .await
            .expect("digest of a readable file");
        assert_eq!(
            streamed,
            super::content_md5(&std::fs::read(&path).expect("file"))
        );
    }

    #[tokio::test]
    async fn file_content_md5_of_a_missing_file_names_the_path() {
        let error = super::file_content_md5("/nonexistent/backup.sql.gz")
            .await
            .expect_err("a missing file has no digest");
        assert!(
            error.to_string().contains("/nonexistent/backup.sql.gz"),
            "the reason names the file: {error}"
        );
    }

    /// A backup file below the multipart threshold is uploaded as a plain
    /// SigV4 PUT whose body is the file itself, not an aws-chunked stream.
    #[tokio::test]
    async fn a_single_part_upload_is_a_plain_sigv4_put() {
        let Some((client, puts)) = spawn_recording_stub().await else {
            return;
        };
        let file = twelve_mib_file();
        let path = file.path().to_str().expect("utf-8 path").to_string();
        super::upload_single_part(&client, "b", "k", &path, "application/gzip", None)
            .await
            .expect("single-part upload succeeds");

        let puts = puts.lock().expect("puts lock");
        assert_eq!(puts.len(), 1, "one PUT for a single-part upload");
        assert_plain_sigv4_put(&puts[0], "single-part upload");
        assert!(
            puts[0].body == std::fs::read(&path).expect("file"),
            "the store received the file bytes unframed"
        );
    }

    /// Every part of a multipart upload goes out without optional
    /// checksums, the same as a single-part upload.
    #[tokio::test]
    async fn multipart_upload_parts_are_plain_sigv4_puts() {
        let Some((client, puts)) = spawn_recording_stub().await else {
            return;
        };
        let file = twelve_mib_file();
        let path = file.path().to_str().expect("utf-8 path").to_string();
        let cancel = tokio_util::sync::CancellationToken::new();
        super::upload_multipart(
            &client,
            "b",
            "k",
            &path,
            "application/octet-stream",
            None,
            &cancel,
        )
        .await
        .expect("multipart upload succeeds");

        let puts = puts.lock().expect("puts lock");
        assert_eq!(puts.len(), 3, "three parts for a 12 MiB file");
        for (index, put) in puts.iter().enumerate() {
            assert_plain_sigv4_put(put, &format!("part {}", index + 1));
        }
    }

    /// An in-memory upload (the metadata companion) gets its `Content-MD5`
    /// from the client's interceptor, with no help from the call site.
    #[tokio::test]
    async fn an_in_memory_upload_gets_content_md5_from_the_client() {
        let Some((client, puts)) = spawn_recording_stub().await else {
            return;
        };
        super::write_metadata_companion(
            &client,
            "b",
            "backups/metadata.json",
            "postgres",
            "uuid-1",
            "backups/backup.sql.gz",
            42,
            7,
            "gzip",
            None,
        )
        .await
        .expect("metadata upload succeeds");

        let puts = puts.lock().expect("puts lock");
        assert_eq!(puts.len(), 1, "one PUT for the metadata companion");
        assert_plain_sigv4_put(&puts[0], "metadata upload");
    }

    // ── Object Lock against a real S3-compatible server ─────────────────────
    //
    // The stub above enforces AWS's documented rule for buckets with a
    // default Object Lock retention period (no integrity header ->
    // InvalidRequest) and checks each `Content-MD5` against this crate's own
    // `content_md5`. Self-hosted S3-compatible servers don't enforce that
    // rule (RustFS and LocalStack both accept such an upload), so a real
    // server can't reproduce the rejection itself. What one can check is
    // everything else: that our digests pass a real server's `Content-MD5`
    // verification, and that every backup upload lands in a locked bucket
    // under its default retention.

    const LOCKED_BUCKET: &str = "locked-backups";

    struct ObjectLockFixture {
        _container: testcontainers::ContainerAsync<testcontainers::GenericImage>,
        /// Built by the production [`super::s3_config_builder`].
        client: aws_sdk_s3::Client,
    }

    /// Start RustFS and create [`LOCKED_BUCKET`] with Object Lock and a
    /// one-day GOVERNANCE default retention. `None` when Docker is not
    /// available, so the test skips instead of failing.
    async fn object_lock_fixture() -> Option<ObjectLockFixture> {
        use crate::test_rustfs::{
            rustfs_container_request, wait_for_rustfs_ready, RUSTFS_ACCESS_KEY, RUSTFS_S3_PORT,
            RUSTFS_SECRET_KEY,
        };
        use testcontainers::runners::AsyncRunner;

        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                println!("Docker not available, skipping Object Lock test: {e}");
                return None;
            }
        };
        if let Err(e) = docker.ping().await {
            println!("Docker daemon not reachable, skipping Object Lock test: {e}");
            return None;
        }

        let container = rustfs_container_request()
            .start()
            .await
            .expect("start RustFS container");
        let port = container
            .get_host_port_ipv4(RUSTFS_S3_PORT)
            .await
            .expect("RustFS S3 port");
        wait_for_rustfs_ready(port)
            .await
            .expect("RustFS did not become healthy");

        let endpoint = format!("127.0.0.1:{port}");
        let credentials = aws_sdk_s3::config::Credentials::new(
            RUSTFS_ACCESS_KEY,
            RUSTFS_SECRET_KEY,
            None,
            None,
            "object-lock-test",
        );
        let client = aws_sdk_s3::Client::from_conf(
            super::s3_config_builder("us-east-1", true, credentials, Some(&endpoint)).build(),
        );

        client
            .create_bucket()
            .bucket(LOCKED_BUCKET)
            .object_lock_enabled_for_bucket(true)
            .send()
            .await
            .expect("create Object Lock bucket");
        client
            .put_object_lock_configuration()
            .bucket(LOCKED_BUCKET)
            .object_lock_configuration(
                aws_sdk_s3::types::ObjectLockConfiguration::builder()
                    .object_lock_enabled(aws_sdk_s3::types::ObjectLockEnabled::Enabled)
                    .rule(
                        aws_sdk_s3::types::ObjectLockRule::builder()
                            .default_retention(
                                aws_sdk_s3::types::DefaultRetention::builder()
                                    .mode(aws_sdk_s3::types::ObjectLockRetentionMode::Governance)
                                    .days(1)
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .send()
            .await
            .expect("set default retention");

        Some(ObjectLockFixture {
            _container: container,
            client,
        })
    }

    /// The retention mode the server recorded for `key`, proving the upload
    /// landed under the bucket's default retention rather than beside it.
    async fn retention_mode(client: &aws_sdk_s3::Client, key: &str) -> Option<String> {
        client
            .head_object()
            .bucket(LOCKED_BUCKET)
            .key(key)
            .send()
            .await
            .unwrap_or_else(|e| panic!("head_object {key}: {e:?}"))
            .object_lock_mode()
            .map(|mode| mode.as_str().to_string())
    }

    /// The server really verifies `Content-MD5`: a digest that doesn't match
    /// the body is rejected. Without this, the test below would pass
    /// against a server that ignores the header, proving nothing about the
    /// digests this crate computes.
    #[tokio::test]
    async fn a_locked_bucket_rejects_a_wrong_content_md5() {
        let Some(fixture) = object_lock_fixture().await else {
            return;
        };
        let error = fixture
            .client
            .put_object()
            .bucket(LOCKED_BUCKET)
            .key("wrong-digest.json")
            .body(b"{}".to_vec().into())
            // The MD5 of an empty body, not of `{}`. The interceptor leaves
            // a `Content-MD5` the caller already set untouched.
            .content_md5(super::content_md5(b""))
            .send()
            .await
            .expect_err("a wrong Content-MD5 is rejected");
        assert_eq!(
            error
                .raw_response()
                .map(|response| response.status().as_u16()),
            Some(400),
            "expected a 400 rejection: {}",
            super::describe_sdk_error("put_object with a wrong Content-MD5", &error)
        );
    }

    /// Every kind of backup upload succeeds on a bucket with a default
    /// Object Lock retention period, and each object is stored under it.
    #[tokio::test]
    async fn backup_uploads_succeed_on_a_bucket_with_default_object_lock_retention() {
        let Some(fixture) = object_lock_fixture().await else {
            return;
        };
        let client = &fixture.client;
        let file = twelve_mib_file();
        let path = file.path().to_str().expect("utf-8 path").to_string();
        let cancel = tokio_util::sync::CancellationToken::new();

        super::upload_single_part(
            client,
            LOCKED_BUCKET,
            "single/backup.sql.gz",
            &path,
            "application/gzip",
            None,
        )
        .await
        .expect("single-part upload to a locked bucket");
        super::upload_multipart(
            client,
            LOCKED_BUCKET,
            "multi/backup.sql.gz",
            &path,
            "application/gzip",
            None,
            &cancel,
        )
        .await
        .expect("multipart upload to a locked bucket");
        super::write_metadata_companion(
            client,
            LOCKED_BUCKET,
            "single/metadata.json",
            "postgres",
            "uuid-1",
            "single/backup.sql.gz",
            12 * 1024 * 1024,
            1,
            "gzip",
            None,
        )
        .await
        .expect("metadata upload to a locked bucket");

        for key in [
            "single/backup.sql.gz",
            "multi/backup.sql.gz",
            "single/metadata.json",
        ] {
            assert_eq!(
                retention_mode(client, key).await.as_deref(),
                Some("GOVERNANCE"),
                "{key} is stored under the bucket's default retention"
            );
        }
    }
}
