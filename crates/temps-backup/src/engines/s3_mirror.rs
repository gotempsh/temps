// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `S3MirrorEngine`: bucket-to-bucket mirror using `rc mirror --overwrite`
//! (the RustFS S3 CLI, an `mc`-compatible client), implemented against
//! `engine_v2::BackupEngine`.
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
//!    see "Why not a whole-account mirror" below.
//! 4. For each bucket mirrored, run a one-shot `rustfs/rc` container in
//!    `host` network mode with `RC_HOST_source` and `RC_HOST_dest` env vars.
//!    The container runs
//!    `rc mirror --overwrite source/<bucket>/ dest/<bucket>/<prefix>/[<bucket>/]`
//!    (see [`build_mirror_script`] for the session-token variant).
//!    Container exits when mirror exits.
//! 5. Compute the mirrored prefix's total size via list-objects, summed
//!    across all mirrored buckets.
//! 6. Write one `metadata.json` companion for the whole backup.
//!
//! ## Why not a whole-account mirror
//!
//! `mc mirror source/` (no bucket in the path) asked the source for an
//! account-level bucket listing to expand into per-bucket mirrors. RustFS's
//! S3 API accepts that same `ListBuckets` call fine when issued directly via
//! `aws-sdk-s3` (used for its own health check), but `mc` issuing the
//! equivalent call against the same credentials got `Access Denied` --
//! confirmed in production. Rather than depend on the mirror client's
//! account-level listing path working, this engine does its own
//! `ListBuckets` (the call already proven to work) and hands the client a
//! concrete, already-known bucket name every time.
//!
//! ## Why `rc` and not `mc`
//!
//! MinIO withdrew every public distribution of `mc` (`quay.io/minio/mc`,
//! Docker Hub `minio/mc`, dl.min.io), so the pinned image can no longer be
//! pulled. `rustfs/rc` is the RustFS project's `mc`-compatible client. Two
//! differences matter here:
//!
//! - Credentials come from `RC_HOST_<alias>` (it ignores `MC_HOST_*`), with
//!   the same percent-decoded `scheme://key:secret@host` shape.
//! - `rc` has no per-alias session token: a third userinfo field is folded
//!   into the secret (SignatureDoesNotMatch), and its global
//!   `-H x-amz-security-token:` header is sent to *every* endpoint, which a
//!   source RustFS rejects. A destination carrying a session token
//!   (Cloud-vended credentials) is therefore mirrored in two steps through
//!   a scratch directory inside the auto-removed container; see
//!   [`build_mirror_script`].

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

pub(crate) const ENGINE_KEY: &str = "s3_mirror";
/// RustFS S3 CLI (`rc`), multi-arch, pinned by tag *and* index digest so a
/// re-pushed tag can never run with these credentials.
const RC_IMAGE: &str =
    "rustfs/rc:v0.1.36@sha256:ab024bfebee49a750ce886b4c70963ccd9ddaa03f491704a90710641d7a26699";
/// Env var carrying the destination session token into the one-shot
/// container for the two-step mirror, so the token never appears in the
/// container's `Cmd` (`docker inspect`) or logs. `sh` expands it into the
/// `rc -H` argument, so it is in that `rc` process's argv for the life of the
/// call — rc has no env- or file-based way to set the header.
const DEST_SESSION_TOKEN_ENV: &str = "TEMPS_DEST_SESSION_TOKEN";

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

        // The mirror client's stderr may echo credential-bearing alias URLs
        // or request headers, and that stderr is folded into the
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
        // for why a whole-account mirror isn't used instead.
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
        super::image_pull::force_pull_image_v2(RC_IMAGE, ENGINE_KEY).await?;

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
                "S3MirrorEngine: starting rc mirror",
            );

            let spec = build_mirror_spec(
                format!("temps-s3mirror-{}-{}", backup_uuid, bucket),
                backup_id,
                &RcHost {
                    scheme: "http",
                    host: &format!("{}:{}", source_host, source_port),
                    access_key: &source_access_key,
                    secret_key: &source_secret_key,
                },
                &RcHost {
                    scheme: dest_scheme,
                    host: dest_hostpath,
                    access_key: &dest_access_key,
                    secret_key: &dest_secret_key,
                },
                dest_session_token.as_deref(),
                &source_path,
                &dest_path,
            );

            let result = match run_one_shot(&deps.docker, spec, &ctx.cancel).await {
                Ok(r) => r,
                Err(OneShotError::Cancelled) => return Err(BackupError::Cancelled),
                Err(e) => {
                    return Err(BackupError::Failed {
                        reason: format!("rc mirror one-shot failed for bucket {}: {}", bucket, e),
                    });
                }
            };
            if result.exit_code != 0 {
                return Err(BackupError::Failed {
                    reason: format!(
                        "rc mirror exited with code {} for bucket {}. stderr: {}. stdout: {}",
                        result.exit_code,
                        bucket,
                        sensitive_values.redact(result.stderr_tail.trim()),
                        sensitive_values.redact(result.stdout_tail.trim()),
                    ),
                });
            }
            // `mc mirror` exited 0 even when it could not list one side of the
            // mirror for comparison -- it logged the failure and fell back to
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
                        "rc mirror reported access denied while mirroring bucket {} \
                         (backup {}, service {}). It still exited 0 and copied what it \
                         could reach via direct PUT/GET, but \
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
                    "rc mirror stderr (warnings): {}",
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
                "backup_tool": "rc",
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

/// One side of an `rc mirror`: exported to the container as
/// `RC_HOST_<alias>=<scheme>://<key>:<secret>@<host>`.
struct RcHost<'a> {
    scheme: &'a str,
    /// `host[:port][/path]`, without scheme.
    host: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
}

impl RcHost<'_> {
    fn env(&self, alias: &str) -> String {
        // `rc` percent-decodes the userinfo like `mc` did, so the same
        // encoder applies. It has no session-token field: a third field would
        // be folded into the secret, so the token always travels separately
        // (see `build_mirror_script`).
        format!(
            "RC_HOST_{}={}://{}@{}",
            alias,
            self.scheme,
            temps_providers::externalsvc::mc_host_credential(
                self.access_key,
                self.secret_key,
                None
            ),
            self.host
        )
    }
}

/// The one-shot container that mirrors `source_path` (alias `source`) into
/// `dest_path` (alias `dest`) for a single bucket.
fn build_mirror_spec(
    name: String,
    backup_id: i32,
    source: &RcHost<'_>,
    dest: &RcHost<'_>,
    dest_session_token: Option<&str>,
    source_path: &str,
    dest_path: &str,
) -> OneShotSpec {
    let session_token = dest_session_token.filter(|t| !t.is_empty());
    let mut env = vec![source.env("source"), dest.env("dest")];
    if let Some(token) = session_token {
        env.push(format!("{}={}", DEST_SESSION_TOKEN_ENV, token));
    }
    OneShotSpec {
        image: RC_IMAGE.to_string(),
        name,
        engine: ENGINE_KEY,
        backup_id,
        entrypoint: vec!["sh".to_string(), "-c".to_string()],
        cmd: vec![build_mirror_script(
            source_path,
            dest_path,
            session_token.is_some(),
        )],
        env,
        binds: vec![],
        // Host network so the rc container can reach both the source
        // object store (typically `host:9000`) and the destination endpoint
        // (typically an internet S3) without extra routing.
        network_mode: Some("host".to_string()),
        user: None,
        // `mc mirror` used to exit 0 even when it could not list one side of
        // the mirror for comparison. `rc` exits non-zero on list failures,
        // but the watch stays as a cheap guard in case a per-object "access
        // denied" is ever logged on a 0 exit; see the exit_code==0 handling
        // in `run`.
        stderr_watch: Some("access denied"),
    }
}

/// Shell script run (via `sh -c`) inside the `rc` container for one bucket.
///
/// Without a destination session token it is a single
/// `rc mirror --overwrite <source> <dest>`.
///
/// With one, `rc` cannot scope the token to the destination alias: its only
/// token mechanism is the global `-H x-amz-security-token:` header, which
/// would also be signed into every *source* request, and a source RustFS
/// rejects an unknown token. So the bucket is first mirrored into a scratch
/// directory (no token), then mirrored from there to the destination with
/// the header -- the second step only ever talks to the destination. The
/// resulting object keys are identical to a direct mirror. The scratch copy
/// lives in the container's writable layer and is discarded with the
/// auto-removed container; the cost is local disk equal to the bucket size
/// for the duration of the run.
fn build_mirror_script(source_path: &str, dest_path: &str, dest_session_token: bool) -> String {
    let source = v2_common::shell_escape(source_path);
    let dest = v2_common::shell_escape(dest_path);
    if !dest_session_token {
        return format!("rc mirror --overwrite {source} {dest}");
    }
    format!(
        "set -e; stage=$(mktemp -d); \
         rc mirror --overwrite {source} \"$stage/\"; \
         rc -H \"x-amz-security-token: ${DEST_SESSION_TOKEN_ENV}\" mirror --overwrite \"$stage/\" {dest}"
    )
}

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
/// directly rather than delegating to the mirror client's equivalent call -- see the
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
    fn rc_client_image_is_release_and_digest_pinned() {
        assert!(RC_IMAGE.starts_with("rustfs/rc:v0.1.36@"));
        assert!(RC_IMAGE
            .ends_with("@sha256:ab024bfebee49a750ce886b4c70963ccd9ddaa03f491704a90710641d7a26699"));
    }

    #[test]
    fn mirror_script_without_session_token_is_a_single_direct_mirror() {
        let script = build_mirror_script("source/data/", "dest/backups/p/", false);
        assert_eq!(
            script,
            "rc mirror --overwrite 'source/data/' 'dest/backups/p/'"
        );
        assert!(!script.contains("x-amz-security-token"));
    }

    #[test]
    fn mirror_script_with_session_token_stages_and_scopes_the_header_to_dest() {
        let script = build_mirror_script("source/data/", "dest/backups/p/", true);
        // The token is read from the env var, never inlined into the script.
        assert!(script.contains(&format!("${DEST_SESSION_TOKEN_ENV}")));
        // Step 1 reads the source without the token header...
        let first = script
            .find("rc mirror --overwrite 'source/data/' \"$stage/\"")
            .expect("source -> stage step");
        // ...step 2 is the only one carrying it, and only talks to dest.
        let second = script
            .find("rc -H \"x-amz-security-token: ")
            .expect("stage -> dest step with header");
        assert!(first < second);
        assert!(script[second..].ends_with("mirror --overwrite \"$stage/\" 'dest/backups/p/'"));
        assert!(!script[..second].contains("x-amz-security-token"));
        assert!(script.starts_with("set -e;"));
    }

    // ---- Real `rc mirror` against a real RustFS ---------------------------
    //
    // These run the exact one-shot container `run` builds (image, entrypoint,
    // RC_HOST_* env, host networking) against a RustFS testcontainer that
    // plays both source and destination, so a drift in the rc CLI contract
    // (env var names, mirror path semantics, exit codes) fails here instead
    // of in a production backup.

    struct MirrorFixture {
        _container: testcontainers::ContainerAsync<testcontainers::GenericImage>,
        docker: bollard::Docker,
        client: S3Client,
        port: u16,
    }

    async fn mirror_fixture() -> Option<MirrorFixture> {
        use crate::test_rustfs::{
            rustfs_container_request, wait_for_rustfs_ready, RUSTFS_ACCESS_KEY, RUSTFS_S3_PORT,
            RUSTFS_SECRET_KEY,
        };
        use testcontainers::runners::AsyncRunner;

        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker not available, skipping rc mirror test: {e}");
                return None;
            }
        };
        if let Err(e) = docker.ping().await {
            println!("Docker daemon not reachable, skipping rc mirror test: {e}");
            return None;
        }
        if let Err(e) = super::super::image_pull::ensure_image_pulled_v2(RC_IMAGE, ENGINE_KEY).await
        {
            println!("Could not pull {RC_IMAGE}, skipping rc mirror test: {e}");
            return None;
        }

        let container = rustfs_container_request()
            .start()
            .await
            .expect("Failed to start RustFS container");
        let port = container
            .get_host_port_ipv4(RUSTFS_S3_PORT)
            .await
            .expect("Failed to get RustFS port");
        wait_for_rustfs_ready(port)
            .await
            .expect("RustFS did not become healthy");

        let conf = aws_sdk_s3::Config::builder()
            .region(Region::new("us-east-1"))
            .endpoint_url(format!("http://127.0.0.1:{port}"))
            .credentials_provider(Credentials::new(
                RUSTFS_ACCESS_KEY,
                RUSTFS_SECRET_KEY,
                None,
                None,
                "s3-mirror-test",
            ))
            .force_path_style(true)
            .behavior_version(BehaviorVersion::latest())
            .build();
        let client = S3Client::from_conf(conf);

        for bucket in ["mirror-src", "mirror-dest"] {
            client
                .create_bucket()
                .bucket(bucket)
                .send()
                .await
                .unwrap_or_else(|e| panic!("create bucket {bucket}: {e}"));
        }
        for (key, body) in [("a.txt", "alpha"), ("nested/dir/b.txt", "bravo")] {
            client
                .put_object()
                .bucket("mirror-src")
                .key(key)
                .body(aws_sdk_s3::primitives::ByteStream::from_static(
                    body.as_bytes(),
                ))
                .send()
                .await
                .unwrap_or_else(|e| panic!("seed {key}: {e}"));
        }

        Some(MirrorFixture {
            _container: container,
            docker,
            client,
            port,
        })
    }

    fn fixture_host<'a>(host: &'a str, secret_key: &'a str) -> RcHost<'a> {
        RcHost {
            scheme: "http",
            host,
            access_key: crate::test_rustfs::RUSTFS_ACCESS_KEY,
            secret_key,
        }
    }

    async fn dest_keys(client: &S3Client, prefix: &str) -> Vec<String> {
        let resp = client
            .list_objects_v2()
            .bucket("mirror-dest")
            .prefix(prefix)
            .send()
            .await
            .expect("list dest");
        let mut keys: Vec<String> = resp
            .contents()
            .iter()
            .filter_map(|o| o.key().map(String::from))
            .collect();
        keys.sort();
        keys
    }

    #[tokio::test]
    async fn rc_mirror_copies_a_bucket_under_the_destination_prefix() {
        let Some(fx) = mirror_fixture().await else {
            return;
        };
        let host = format!("127.0.0.1:{}", fx.port);
        let secret = crate::test_rustfs::RUSTFS_SECRET_KEY;
        let prefix = "external_services/s3/svc/backup-uuid";
        let spec = build_mirror_spec(
            format!("temps-s3mirror-test-{}", uuid::Uuid::new_v4()),
            0,
            &fixture_host(&host, secret),
            &fixture_host(&host, secret),
            None,
            "source/mirror-src/",
            &format!("dest/mirror-dest/{prefix}/"),
        );

        let result = run_one_shot(
            &fx.docker,
            spec,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("run rc mirror");

        assert_eq!(
            result.exit_code, 0,
            "rc mirror failed: stderr={} stdout={}",
            result.stderr_tail, result.stdout_tail
        );
        assert!(!result.stderr_watch_matched);
        assert_eq!(
            dest_keys(&fx.client, &format!("{prefix}/")).await,
            vec![
                format!("{prefix}/a.txt"),
                format!("{prefix}/nested/dir/b.txt"),
            ],
        );
        // The size accounting `run` does afterwards sees the mirrored bytes.
        let size = list_total_s3_size(&fx.client, "mirror-dest", &format!("{prefix}/"))
            .await
            .expect("size");
        assert_eq!(size, ("alpha".len() + "bravo".len()) as i64);
    }

    #[tokio::test]
    async fn rc_mirror_with_bad_destination_credentials_exits_non_zero_without_leaking_them() {
        let Some(fx) = mirror_fixture().await else {
            return;
        };
        let host = format!("127.0.0.1:{}", fx.port);
        let wrong_secret = "definitely-not-the-secret";
        let spec = build_mirror_spec(
            format!("temps-s3mirror-test-{}", uuid::Uuid::new_v4()),
            0,
            &fixture_host(&host, crate::test_rustfs::RUSTFS_SECRET_KEY),
            &fixture_host(&host, wrong_secret),
            None,
            "source/mirror-src/",
            "dest/mirror-dest/bad-creds/",
        );

        let result = run_one_shot(
            &fx.docker,
            spec,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("run rc mirror");

        assert_ne!(
            result.exit_code, 0,
            "bad dest credentials must fail the run"
        );
        assert!(!result.stderr_tail.contains(wrong_secret));
        assert!(!result.stdout_tail.contains(wrong_secret));
        assert!(dest_keys(&fx.client, "bad-creds/").await.is_empty());
    }

    /// With a destination session token the source is read without the token
    /// header (step 1 succeeds against a RustFS that would reject it) and only
    /// the destination step carries it. RustFS root credentials reject an
    /// unknown token, so step 2 failing -- after step 1 staged both objects --
    /// is exactly the observable proof that the header is scoped to `dest`.
    #[tokio::test]
    async fn rc_mirror_with_session_token_reads_source_without_the_token_header() {
        let Some(fx) = mirror_fixture().await else {
            return;
        };
        let host = format!("127.0.0.1:{}", fx.port);
        let secret = crate::test_rustfs::RUSTFS_SECRET_KEY;
        let spec = build_mirror_spec(
            format!("temps-s3mirror-test-{}", uuid::Uuid::new_v4()),
            0,
            &fixture_host(&host, secret),
            &fixture_host(&host, secret),
            Some("not-a-real-session-token"),
            "source/mirror-src/",
            "dest/mirror-dest/with-token/",
        );
        assert!(spec
            .env
            .iter()
            .any(|e| e == &format!("{DEST_SESSION_TOKEN_ENV}=not-a-real-session-token")));
        assert!(!spec.cmd[0].contains("not-a-real-session-token"));

        let result = run_one_shot(
            &fx.docker,
            spec,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("run rc mirror");

        assert!(
            result.stdout_tail.contains("a.txt") && result.stdout_tail.contains("nested/dir/b.txt"),
            "step 1 (source -> stage, no token) must have copied both objects: stdout={} stderr={}",
            result.stdout_tail,
            result.stderr_tail
        );
        assert_ne!(
            result.exit_code, 0,
            "step 2 must have sent the bogus token to the destination"
        );
        assert!(dest_keys(&fx.client, "with-token/").await.is_empty());
    }
}
