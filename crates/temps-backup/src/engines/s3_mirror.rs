// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `S3MirrorEngine`: bucket-to-bucket mirror using `mc mirror --overwrite`,
//! implemented against `engine_v2::BackupEngine`.
//!
//! ## Flow
//!
//! 1. Validate the destination S3 source + bucket reachability.
//! 2. Load the source service config (it's a temps-managed MinIO/RustFS-like
//!    service; the host/port/credentials live in the service's encrypted
//!    config blob).
//! 3. If the service config names a single bucket (`bucket_name`/`bucket`,
//!    the normal MinIO/S3-compatible case), mirror that one bucket. If it
//!    does not (RustFS: every *project* gets its own bucket, and there is no
//!    per-service bucket at all), enumerate the account's buckets via
//!    `ListBuckets` over `aws-sdk-s3` and mirror each one individually --
//!    see "Why not `mc mirror source/`" below.
//! 4. For each bucket mirrored, run a one-shot `minio/mc` container in
//!    `host` network mode with `MC_HOST_source` and `MC_HOST_dest` env vars.
//!    The container's entrypoint is
//!    `mc mirror --overwrite source/<bucket>/ dest/<bucket>/<prefix>/[<bucket>/]`.
//!    Container exits when mirror exits.
//! 5. Compute the mirrored prefix's total size via list-objects, summed
//!    across all mirrored buckets.
//! 6. Write one `metadata.json` companion for the whole backup.
//!
//! ## Why not `mc mirror source/` for a whole account
//!
//! `mc mirror source/` (no bucket in the path) asks the source for an
//! account-level bucket listing to expand into per-bucket mirrors. RustFS's
//! S3 API accepts that same `ListBuckets` call fine when issued directly via
//! `aws-sdk-s3` (used for its own health check), but `mc` issuing the
//! equivalent call against the same credentials gets `Access Denied` --
//! confirmed in production. Rather than depend on `mc`'s account-level
//! listing path working, this engine does its own `ListBuckets` (the call
//! already proven to work) and hands `mc` a concrete, already-known bucket
//! name every time, so `mc` never needs to perform that call itself.

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::Client as S3Client;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::oneshot::{run_one_shot, OneShotError, OneShotSpec};
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};
use temps_providers::externalsvc::SensitiveValues;

const ENGINE_KEY: &str = "s3_mirror";
const MC_IMAGE: &str = "minio/mc:RELEASE.2025-08-13T08-35-41Z@sha256:a7fe349ef4bd8521fb8497f55c6042871b2ae640607cf99d9bede5e9bdf11727";

pub struct S3MirrorDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct S3MirrorEngine {
    deps: Arc<S3MirrorDeps>,
}

impl S3MirrorEngine {
    pub fn new(deps: S3MirrorDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for S3MirrorEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;

        // ── Destination S3 source + bucket reachability ──────────────────────
        let s3_dest = v2_common::load_s3_source(deps.db.as_ref(), s3_source_id).await?;
        let s3_dest_client =
            v2_common::build_s3_client(&s3_dest, &deps.encryption_service, "s3-mirror-engine")?;
        v2_common::assert_bucket_reachable(&s3_dest_client, &s3_dest.bucket_name).await?;

        // ── Source service config (host/port/creds) ──────────────────────────
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error loading service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {} not found", service_id),
            })?;

        let service_config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| "{}".to_string());
        let src: Value = serde_json::from_str(&service_config_json).unwrap_or_else(|_| json!({}));
        let source_access_key = src
            .get("access_key")
            .or_else(|| src.get("access_key_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let source_secret_key = src
            .get("secret_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let source_host = src
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("localhost")
            .to_string();
        let source_port = src
            .get("port")
            .and_then(|v| v.as_str().or_else(|| v.as_u64().map(|_| "9000")))
            .unwrap_or("9000")
            .to_string();
        let source_bucket = src
            .get("bucket_name")
            .or_else(|| src.get("bucket"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let source_region = src
            .get("region")
            .and_then(|v| v.as_str())
            .unwrap_or("us-east-1")
            .to_string();

        let dest_access_key = deps
            .encryption_service
            .decrypt_string(&s3_dest.access_key_id)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt dest access key: {}", e),
            })?;
        let dest_secret_key = deps
            .encryption_service
            .decrypt_string(&s3_dest.secret_key)
            .map_err(|e| BackupError::PermanentFailure {
                reason: format!("decrypt dest secret key: {}", e),
            })?;
        let dest_session_token =
            v2_common::decrypt_session_token(&s3_dest, &deps.encryption_service)?;

        let dest_endpoint = s3_dest.endpoint.as_deref().unwrap_or("").to_string();
        let dest_endpoint = if dest_endpoint.is_empty() {
            format!("http://{}:9000", s3_dest.bucket_name)
        } else {
            dest_endpoint
        };
        // Preserve the dest endpoint's scheme — hard-coding `http://` would
        // break HTTPS endpoints like Cloudflare R2.
        let (dest_scheme, dest_hostpath) =
            if let Some(rest) = dest_endpoint.strip_prefix("https://") {
                ("https", rest)
            } else if let Some(rest) = dest_endpoint.strip_prefix("http://") {
                ("http", rest)
            } else {
                ("http", dest_endpoint.as_str())
            };

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let dest_prefix = build_dest_prefix(&s3_dest.bucket_path, &service.name, &backup_uuid);

        // mc echoes the credential-bearing `MC_HOST_*` URL it could not use
        // straight into stderr, and that stderr is folded into the
        // `BackupError::Failed` reason persisted on the backup row and surfaced
        // through the API. A Cloud-vended destination credential carries a
        // session token that is not individually revocable, so it must be
        // scrubbed alongside the keys.
        let sensitive_values = SensitiveValues::new()
            .credential(&source_access_key, &source_secret_key, None)
            .credential(
                &dest_access_key,
                &dest_secret_key,
                dest_session_token.as_deref(),
            );

        // The source service config names a bucket for the normal MinIO/S3
        // case; RustFS never does (every *project* gets its own bucket, not
        // the service). When it doesn't, enumerate the account's actual
        // buckets and mirror each individually -- see the module doc comment
        // for why `mc mirror source/` (whole account) isn't used instead.
        let enumerated = source_bucket.is_empty();
        let buckets = if enumerated {
            list_source_buckets(
                &source_access_key,
                &source_secret_key,
                &source_host,
                &source_port,
                &source_region,
            )
            .await?
        } else {
            vec![source_bucket.clone()]
        };

        if buckets.is_empty() {
            warn!(
                backup_id,
                service_id, "S3MirrorEngine: source account has no buckets to mirror"
            );
        }

        // Refresh the registry tag once before the whole backup run, rather
        // than per bucket, so a poisoned local cache entry cannot execute
        // with these secrets -- and so N buckets don't each pay a pull.
        super::image_pull::force_pull_image_v2(MC_IMAGE, ENGINE_KEY).await?;

        let mut total_size_bytes: i64 = 0;
        let mut mirrored_buckets: Vec<Value> = Vec::with_capacity(buckets.len());

        for bucket in &buckets {
            let source_path = format!("source/{}/", bucket);
            // A single explicitly-configured bucket keeps the original,
            // un-nested destination layout; enumerated buckets each get
            // their own subfolder so they can't collide with each other.
            let bucket_dest_prefix = if enumerated {
                format!("{}/{}", dest_prefix.trim_matches('/'), bucket)
            } else {
                dest_prefix.trim_matches('/').to_string()
            };
            let dest_path = format!("dest/{}/{}/", s3_dest.bucket_name, bucket_dest_prefix);

            info!(
                backup_id,
                source = %source_path,
                dest = %dest_path,
                "S3MirrorEngine: starting mc mirror",
            );

            let env_vars = vec![
                format!(
                    "MC_HOST_source=http://{}:{}@{}:{}",
                    source_access_key, source_secret_key, source_host, source_port
                ),
                format!(
                    "MC_HOST_dest={}://{}@{}",
                    dest_scheme,
                    temps_providers::externalsvc::mc_host_credential(
                        &dest_access_key,
                        &dest_secret_key,
                        dest_session_token.as_deref(),
                    ),
                    dest_hostpath
                ),
            ];

            let mirror_cmd = format!(
                "mc mirror --overwrite {} {}",
                v2_common::shell_escape(&source_path),
                v2_common::shell_escape(&dest_path),
            );

            let spec = OneShotSpec {
                image: MC_IMAGE.to_string(),
                name: format!("temps-s3mirror-{}-{}", backup_uuid, bucket),
                engine: ENGINE_KEY,
                backup_id,
                entrypoint: vec!["sh".to_string(), "-c".to_string()],
                cmd: vec![mirror_cmd],
                env: env_vars,
                binds: vec![],
                // Host network so the mc container can reach both the source
                // MinIO (typically `host:9000`) and the destination endpoint
                // (typically an internet S3) without extra routing.
                network_mode: Some("host".to_string()),
                user: None,
                // `mc mirror` exits 0 even when it could not list one side of
                // the mirror for comparison; see the exit_code==0 handling
                // below for why that fallback needs to be caught rather than
                // trusted.
                stderr_watch: Some("access denied"),
            };

            let result = match run_one_shot(&deps.docker, spec, &ctx.cancel).await {
                Ok(r) => r,
                Err(OneShotError::Cancelled) => return Err(BackupError::Cancelled),
                Err(e) => {
                    return Err(BackupError::Failed {
                        reason: format!("mc mirror one-shot failed for bucket {}: {}", bucket, e),
                    });
                }
            };
            if result.exit_code != 0 {
                return Err(BackupError::Failed {
                    reason: format!(
                        "mc mirror exited with code {} for bucket {}. stderr: {}. stdout: {}",
                        result.exit_code,
                        bucket,
                        sensitive_values.redact(result.stderr_tail.trim()),
                        sensitive_values.redact(result.stdout_tail.trim()),
                    ),
                });
            }
            // `mc mirror` exits 0 even when it could not list one side of the
            // mirror for comparison -- it logs the failure and falls back to
            // copying everything it *can* reach via plain PUT/GET, rather
            // than treating a failed diff as fatal. That fallback silently
            // turns a real access/listing problem into a "completed" backup
            // that never actually diffed against what's already there, with
            // no visible signal to the operator beyond a log line buried in
            // server output. `stderr_watch_matched` is checked here rather
            // than re-scanning `stderr_tail`: the tail only keeps the last
            // 4 KiB, and a mirror producing enough later stderr (many
            // objects, retried entries) could evict this exact diagnostic
            // before we ever look at it, so detection has to happen as the
            // stream arrives, not after the fact.
            if result.stderr_watch_matched {
                return Err(BackupError::Failed {
                    reason: format!(
                        "mc mirror could not list one side of the mirror for comparison \
                         (access denied) for bucket {} (backup {}, service {}). mc still \
                         exited 0 and copied what it could reach via direct PUT/GET, but \
                         the result may be an incomplete, non-diffed copy -- check that \
                         both the source and destination S3 credentials have list \
                         permission on their bucket. stderr: {}",
                        bucket,
                        backup_id,
                        service_id,
                        sensitive_values.redact(result.stderr_tail.trim()),
                    ),
                });
            }
            if !result.stderr_tail.trim().is_empty() {
                info!(
                    backup_id,
                    bucket = %bucket,
                    "mc mirror stderr (warnings): {}",
                    sensitive_values.redact(result.stderr_tail.trim()),
                );
            }

            // Trailing slash makes this a directory-boundary prefix, not a
            // plain string prefix: without it, an enumerated bucket named
            // "data" would also match objects mirrored under a sibling
            // "database" bucket's prefix, inflating this bucket's size with
            // another bucket's bytes.
            let bucket_size_prefix = format!("{}/", bucket_dest_prefix);
            let bucket_size_bytes = list_total_s3_size(
                &s3_dest_client,
                &s3_dest.bucket_name,
                &bucket_size_prefix,
            )
            .await
            .unwrap_or_else(|e| {
                warn!(backup_id, bucket = %bucket, error = %e, "s3_mirror: could not compute size");
                0
            });
            total_size_bytes += bucket_size_bytes;
            mirrored_buckets.push(json!({
                "bucket": bucket,
                "prefix": bucket_dest_prefix,
                "size_bytes": bucket_size_bytes,
            }));
        }

        // ── Metadata ──────────────────────────────────────────────────────────
        let metadata_key = format!("{}/metadata.json", dest_prefix.trim_matches('/'));
        v2_common::write_metadata_companion(
            &s3_dest_client,
            &s3_dest.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &dest_prefix,
            total_size_bytes,
            s3_source_id,
            "none",
            Some(json!({
                "backup_tool": "mc",
                "service": { "id": service_id, "name": service.name },
                "buckets": mirrored_buckets,
            })),
        )
        .await?;

        info!(
            backup_id,
            %dest_prefix,
            size_bytes = total_size_bytes,
            bucket_count = buckets.len(),
            "S3MirrorEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: dest_prefix,
            size_bytes: Some(total_size_bytes),
            compression: "none".to_string(),
        })
    }
}

// ── Local helpers ────────────────────────────────────────────────────────────

fn build_dest_prefix(bucket_path: &str, service_name: &str, backup_uuid: &str) -> String {
    let base = bucket_path.trim_matches('/');
    if base.is_empty() {
        format!("external_services/s3/{}/{}", service_name, backup_uuid)
    } else {
        format!(
            "{}/external_services/s3/{}/{}",
            base, service_name, backup_uuid
        )
    }
}

/// List the bucket names visible to a source account, used when the service
/// config names no single bucket (RustFS). Uses `aws-sdk-s3`'s `ListBuckets`
/// directly rather than delegating to `mc`'s equivalent call -- see the
/// module doc comment for why.
async fn list_source_buckets(
    access_key: &str,
    secret_key: &str,
    host: &str,
    port: &str,
    region: &str,
) -> Result<Vec<String>, BackupError> {
    let endpoint = format!("http://{}:{}", host, port);
    let creds = Credentials::new(
        access_key,
        secret_key,
        None,
        None,
        "s3-mirror-engine-source",
    );
    let s3_config = aws_sdk_s3::Config::builder()
        .region(Region::new(region.to_string()))
        .endpoint_url(&endpoint)
        .credentials_provider(creds)
        .force_path_style(true)
        .behavior_version(BehaviorVersion::latest())
        .build();
    let client = S3Client::from_conf(s3_config);

    let mut bucket_names = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = client.list_buckets();
        if let Some(tok) = continuation {
            req = req.continuation_token(tok);
        }
        let resp = req.send().await.map_err(|e| BackupError::Failed {
            reason: format!(
                "could not enumerate source buckets at {} (account-level ListBuckets): {}",
                endpoint, e
            ),
        })?;
        bucket_names.extend(
            resp.buckets()
                .iter()
                .filter_map(|b| b.name())
                .map(String::from),
        );
        continuation = resp.continuation_token().map(|s| s.to_string());
        if continuation.is_none() {
            break;
        }
    }

    Ok(bucket_names)
}

async fn list_total_s3_size(
    client: &S3Client,
    bucket: &str,
    prefix: &str,
) -> Result<i64, BackupError> {
    let mut total: i64 = 0;
    let mut continuation: Option<String> = None;
    loop {
        let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(tok) = continuation {
            req = req.continuation_token(tok);
        }
        let resp = req.send().await.map_err(|e| BackupError::Failed {
            reason: format!("list objects: {}", e),
        })?;
        for obj in resp.contents() {
            total += obj.size().unwrap_or(0);
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
        } else {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minio_client_image_is_release_and_digest_pinned() {
        assert!(MC_IMAGE.contains("minio/mc:RELEASE.2025-08-13T08-35-41Z@"));
        assert!(MC_IMAGE
            .contains("sha256:a7fe349ef4bd8521fb8497f55c6042871b2ae640607cf99d9bede5e9bdf11727"));
        assert!(MC_IMAGE.contains("@sha256:"));
    }
}
