//! `S3MirrorEngine`: bucket-to-bucket mirror using `mc mirror --overwrite`,
//! implemented against `engine_v2::BackupEngine`.
//!
//! ## Flow
//!
//! 1. Validate the destination S3 source + bucket reachability.
//! 2. Load the source service config (it's a temps-managed MinIO/RustFS-like
//!    service; the host/port/credentials live in the service's encrypted
//!    config blob).
//! 3. Run a one-shot `minio/mc` container in `host` network mode with
//!    `MC_HOST_source` and `MC_HOST_dest` env vars. The container's
//!    entrypoint is `mc mirror --overwrite source/<bucket>/ dest/<bucket>/<prefix>/`.
//!    Container exits when mirror exits.
//! 4. Compute the mirrored prefix's total size via list-objects.
//! 5. Write the `metadata.json` companion.

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use super::oneshot::{run_one_shot, OneShotError, OneShotSpec};
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

const ENGINE_KEY: &str = "s3_mirror";
const MC_IMAGE: &str = "minio/mc:latest";

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

        let backup_uuid = Uuid::new_v4().to_string();
        let dest_prefix = build_dest_prefix(&s3_dest.bucket_path, &service.name, &backup_uuid);

        let source_path = if source_bucket.is_empty() {
            "source/".to_string()
        } else {
            format!("source/{}/", source_bucket)
        };
        let dest_path = format!(
            "dest/{}/{}/",
            s3_dest.bucket_name,
            dest_prefix.trim_matches('/'),
        );

        info!(
            backup_id,
            source = %source_path,
            dest = %dest_path,
            "S3MirrorEngine: starting mc mirror",
        );

        // ── One-shot mc mirror container ─────────────────────────────────────
        super::image_pull::ensure_image_pulled_v2(MC_IMAGE, ENGINE_KEY).await?;

        let env_vars = vec![
            format!(
                "MC_HOST_source=http://{}:{}@{}:{}",
                source_access_key, source_secret_key, source_host, source_port
            ),
            format!(
                "MC_HOST_dest={}://{}:{}@{}",
                dest_scheme, dest_access_key, dest_secret_key, dest_hostpath
            ),
        ];

        let mirror_cmd = format!(
            "mc mirror --overwrite {} {}",
            v2_common::shell_escape(&source_path),
            v2_common::shell_escape(&dest_path),
        );

        let spec = OneShotSpec {
            image: MC_IMAGE.to_string(),
            name: format!("temps-s3mirror-{}", backup_uuid),
            engine: ENGINE_KEY,
            backup_id,
            entrypoint: vec!["sh".to_string(), "-c".to_string()],
            cmd: vec![mirror_cmd],
            env: env_vars,
            binds: vec![],
            // Host network so the mc container can reach both the source MinIO
            // (typically `host:9000`) and the destination endpoint (typically
            // an internet S3) without extra routing.
            network_mode: Some("host".to_string()),
            user: None,
        };

        let result = match run_one_shot(&deps.docker, spec, &ctx.cancel).await {
            Ok(r) => r,
            Err(OneShotError::Cancelled) => return Err(BackupError::Cancelled),
            Err(e) => {
                return Err(BackupError::Failed {
                    reason: format!("mc mirror one-shot failed: {}", e),
                });
            }
        };
        if result.exit_code != 0 {
            return Err(BackupError::Failed {
                reason: format!(
                    "mc mirror exited with code {}. stderr: {}. stdout: {}",
                    result.exit_code,
                    result.stderr_tail.trim(),
                    result.stdout_tail.trim(),
                ),
            });
        }
        if !result.stderr_tail.trim().is_empty() {
            info!(
                backup_id,
                "mc mirror stderr (warnings): {}",
                result.stderr_tail.trim(),
            );
        }

        // ── Compute size + metadata ──────────────────────────────────────────
        let size_bytes = list_total_s3_size(
            &s3_dest_client,
            &s3_dest.bucket_name,
            dest_prefix.trim_matches('/'),
        )
        .await
        .unwrap_or_else(|e| {
            warn!(backup_id, error = %e, "s3_mirror: could not compute size");
            0
        });

        let metadata_key = format!("{}/metadata.json", dest_prefix.trim_matches('/'));
        v2_common::write_metadata_companion(
            &s3_dest_client,
            &s3_dest.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &dest_prefix,
            size_bytes,
            s3_source_id,
            "none",
            Some(json!({
                "backup_tool": "mc",
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            %dest_prefix,
            size_bytes,
            "S3MirrorEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: dest_prefix,
            size_bytes: Some(size_bytes),
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
