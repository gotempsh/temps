// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! S3 bucket lifecycle reconciliation.
//!
//! Backup retention has historically been enforced application-side only:
//! [`super::backup::BackupService::enforce_retention`] sweeps the database
//! and issues `DeleteObject` for each expired backup. That works while
//! temps is running, but leaves a soft failure mode — if the control plane
//! is offline for a week, no S3 cleanup happens and storage costs balloon.
//!
//! This module pushes the same retention policy onto the bucket itself
//! via `PutBucketLifecycleConfiguration`. Every backup upload tags the
//! object with `temps-managed=true` + `temps-retention-days=N` (see
//! [`crate::engines::v2_common::BackupTags`]). We then create one
//! lifecycle rule per distinct retention value pointing at that tag
//! filter, so S3 expires the object after N days regardless of whether
//! temps is running.
//!
//! ## Why tag-based filters, not prefix-based
//!
//! Per-schedule prefixes were the obvious first design but would have
//! changed S3 key layout, breaking restore for every existing backup.
//! Tag filters require zero key changes — existing backups (with no
//! tags) are simply invisible to the lifecycle rules; only objects
//! written after this change carry the tags and get expired.
//!
//! ## Provider portability
//!
//! Tag-filtered lifecycle rules are supported by AWS S3, MinIO, OVH
//! Object Storage (High Performance), and RustFS. Cloudflare R2 and
//! Backblaze B2 have rougher support; we treat any provider that
//! rejects the configuration call as "unsupported" and fall back
//! silently to application-side retention — this module never fails
//! the caller because S3 didn't accept a lifecycle rule.
//!
//! ## Reconciliation, not one-shot
//!
//! [`S3LifecycleService::reconcile_bucket`] is idempotent: it computes
//! the desired set of rules from current schedule state and overwrites
//! the bucket's lifecycle config. Drift (manual edits in the AWS
//! console, transient `Put` failures, schedule deletions) is corrected
//! by the next reconcile.

use std::sync::Arc;

use aws_sdk_s3::types::{
    BucketLifecycleConfiguration, ExpirationStatus, LifecycleExpiration, LifecycleRule,
    LifecycleRuleFilter, Tag,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tracing::{debug, info, warn};

use temps_core::EncryptionService;

use crate::engines::v2_common;
use crate::services::backup::BackupError;

/// User-Agent stamped on the S3 client when reconciling lifecycle rules.
/// Distinct from the upload UA so it shows up separately in S3 access
/// logs.
const USER_AGENT: &str = "temps-s3-lifecycle";

/// Result of one reconcile pass. Surfaces "we tried but the provider
/// said no" distinctly from "everything is in sync".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The bucket's lifecycle config was updated to match the desired
    /// state. Carries the rule count for log/metric attribution.
    Applied { rule_count: usize },
    /// The bucket already had the desired config — no API call made.
    NoChange,
    /// No retention rules to apply (no schedules pointing at this
    /// bucket, or all schedules have `retention_period <= 0`). We
    /// proactively clear any existing temps-managed rules so we don't
    /// strand stale ones.
    Cleared,
    /// The provider rejected `PutBucketLifecycleConfiguration`. Either
    /// the API isn't implemented, the credentials lack
    /// `s3:PutLifecycleConfiguration`, or the request shape isn't
    /// supported on this storage backend. App-side retention still
    /// runs, so backups will still be cleaned up — just by temps, not
    /// by S3.
    Unsupported { reason: String },
}

/// Reconciles S3 bucket lifecycle policies with the retention values
/// configured on `backup_schedules`. Stateless — every call recomputes
/// the desired state from the database.
pub struct S3LifecycleService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
}

impl S3LifecycleService {
    pub fn new(db: Arc<DatabaseConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Reconcile lifecycle rules for one S3 source. Loads all enabled
    /// schedules pointing at this source, collects the distinct
    /// retention values, and pushes one rule per value to the bucket.
    pub async fn reconcile_bucket(
        &self,
        s3_source_id: i32,
    ) -> Result<ReconcileOutcome, BackupError> {
        let source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?
            .ok_or_else(|| BackupError::NotFound {
                resource: "s3_source".to_string(),
                detail: format!("id {}", s3_source_id),
            })?;

        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::S3SourceId.eq(s3_source_id))
            .filter(temps_entities::backup_schedules::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        let retentions = distinct_retentions(&schedules);

        let client = v2_common::build_s3_client(&source, &self.encryption_service, USER_AGENT)
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "failed to build S3 client for source {}: {}",
                    s3_source_id, e
                ),
            })?;

        if retentions.is_empty() {
            return clear_temps_rules(&client, &source.bucket_name, s3_source_id).await;
        }

        let rules = build_lifecycle_rules(&retentions);
        apply_lifecycle(&client, &source.bucket_name, rules, s3_source_id).await
    }
}

/// Collect distinct, positive retention values across schedules. Sorted
/// ascending so the rule order in the S3 console is human-readable.
fn distinct_retentions(schedules: &[temps_entities::backup_schedules::Model]) -> Vec<i32> {
    let mut vals: Vec<i32> = schedules
        .iter()
        .map(|s| s.retention_period)
        .filter(|n| *n > 0)
        .collect();
    vals.sort_unstable();
    vals.dedup();
    vals
}

/// Build the lifecycle rule set. One rule per distinct retention value;
/// each rule filters on the `temps-retention-days` tag.
pub fn build_lifecycle_rules(retentions: &[i32]) -> Vec<LifecycleRule> {
    retentions
        .iter()
        .map(|days| {
            let tag = Tag::builder()
                .key("temps-retention-days")
                .value(days.to_string())
                .build()
                .expect("Tag with both key and value always builds");

            let filter = LifecycleRuleFilter::builder().tag(tag).build();

            let expiration = LifecycleExpiration::builder().days(*days).build();

            LifecycleRule::builder()
                .id(format!("temps-retention-{}d", days))
                .status(ExpirationStatus::Enabled)
                .filter(filter)
                .expiration(expiration)
                .build()
                .expect("LifecycleRule with id+status+filter+expiration always builds")
        })
        .collect()
}

async fn apply_lifecycle(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    rules: Vec<LifecycleRule>,
    s3_source_id: i32,
) -> Result<ReconcileOutcome, BackupError> {
    let rule_count = rules.len();
    let config = BucketLifecycleConfiguration::builder()
        .set_rules(Some(rules))
        .build()
        .expect("BucketLifecycleConfiguration with rules always builds");

    let resp = client
        .put_bucket_lifecycle_configuration()
        .bucket(bucket)
        .lifecycle_configuration(config)
        .send()
        .await;

    match resp {
        Ok(_) => {
            info!(
                s3_source_id,
                bucket, rule_count, "Applied S3 lifecycle configuration"
            );
            Ok(ReconcileOutcome::Applied { rule_count })
        }
        Err(err) => {
            let msg = err.to_string();
            if is_unsupported_error(&msg) {
                warn!(
                    s3_source_id,
                    bucket,
                    error = %msg,
                    "S3 provider rejected lifecycle config — falling back to app-side retention"
                );
                Ok(ReconcileOutcome::Unsupported { reason: msg })
            } else {
                Err(BackupError::S3(format!(
                    "put_bucket_lifecycle_configuration on bucket {} failed: {}",
                    bucket, msg
                )))
            }
        }
    }
}

/// When there are no temps-managed retention rules to apply, attempt to
/// clear the bucket's lifecycle config so stale rules from a previous
/// reconcile don't keep deleting objects after the user disables every
/// schedule. Provider errors here are tolerated — same reasoning as
/// `apply_lifecycle`.
async fn clear_temps_rules(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    s3_source_id: i32,
) -> Result<ReconcileOutcome, BackupError> {
    let resp = client.delete_bucket_lifecycle().bucket(bucket).send().await;

    match resp {
        Ok(_) => {
            debug!(
                s3_source_id,
                bucket, "Cleared S3 lifecycle configuration (no active retention)"
            );
            Ok(ReconcileOutcome::Cleared)
        }
        Err(err) => {
            let msg = err.to_string();
            if is_unsupported_error(&msg) {
                Ok(ReconcileOutcome::Unsupported { reason: msg })
            } else {
                // Bucket may simply not have a lifecycle config yet — that's
                // a non-event, not an error. AWS returns
                // `NoSuchLifecycleConfiguration` in that case.
                if msg.contains("NoSuchLifecycleConfiguration") {
                    Ok(ReconcileOutcome::NoChange)
                } else {
                    Err(BackupError::S3(format!(
                        "delete_bucket_lifecycle on bucket {} failed: {}",
                        bucket, msg
                    )))
                }
            }
        }
    }
}

/// Heuristic for "this provider does not support lifecycle config".
/// We can't pattern-match by error variant because the AWS SDK returns
/// these as generic service errors; the response body text is the only
/// signal. The strings here cover AWS, MinIO, OVH, R2, and B2 rejections
/// observed in practice.
///
/// Re-exported as `pub` so the upload path (`apply_object_tags`) can use
/// the same matching to decide whether a tag-write failure is "this
/// provider can't" (warn + continue) vs "the upload is genuinely broken"
/// (fail the backup).
pub fn is_unsupported_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("notimplemented")
        || m.contains("not implemented")
        || m.contains("methodnotallowed")
        || m.contains("method not allowed")
        || m.contains("malformedxml")
        || (m.contains("invalidargument") && m.contains("lifecycle"))
        || m.contains("accessdenied")
        || m.contains("access denied")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule_with_retention(
        id: i32,
        retention: i32,
        enabled: bool,
    ) -> temps_entities::backup_schedules::Model {
        let now = chrono::Utc::now();
        temps_entities::backup_schedules::Model {
            id,
            name: format!("sched-{}", id),
            backup_type: "full".to_string(),
            retention_period: retention,
            s3_source_id: 1,
            schedule_expression: "0 0 * * *".to_string(),
            enabled,
            last_run: None,
            next_run: None,
            created_at: now,
            updated_at: now,
            description: None,
            tags: "{}".to_string(),
            max_runtime_secs: None,
            target_all_services: true,
            include_control_plane: true,
        }
    }

    #[test]
    fn distinct_retentions_dedups_and_filters() {
        let schedules = vec![
            schedule_with_retention(1, 7, true),
            schedule_with_retention(2, 7, true),
            schedule_with_retention(3, 30, true),
            schedule_with_retention(4, 0, true), // zero == "no retention"
            schedule_with_retention(5, -1, true), // negative defensive
            schedule_with_retention(6, 90, true),
        ];
        assert_eq!(distinct_retentions(&schedules), vec![7, 30, 90]);
    }

    #[test]
    fn build_lifecycle_rules_one_per_retention() {
        let rules = build_lifecycle_rules(&[7, 30]);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].id(), Some("temps-retention-7d"));
        assert_eq!(rules[1].id(), Some("temps-retention-30d"));
        for rule in &rules {
            assert_eq!(rule.status(), &ExpirationStatus::Enabled);
            assert!(rule.expiration().is_some());
            assert!(rule.filter().is_some());
        }
    }

    #[test]
    fn build_lifecycle_rules_empty_when_no_retentions() {
        assert!(build_lifecycle_rules(&[]).is_empty());
    }

    #[test]
    fn is_unsupported_error_recognises_known_strings() {
        assert!(is_unsupported_error("NotImplemented: not supported"));
        assert!(is_unsupported_error("MethodNotAllowed"));
        assert!(is_unsupported_error("MalformedXML"));
        assert!(is_unsupported_error("AccessDenied: missing permission"));
        assert!(is_unsupported_error(
            "InvalidArgument: lifecycle filter not supported"
        ));
        assert!(!is_unsupported_error("InternalError: 500"));
        assert!(!is_unsupported_error("NoSuchBucket"));
    }

    /// Regression: R2 returns this exact shape when `PutObjectTagging`
    /// is called. The upload-path uses `is_unsupported_error` on the
    /// rendered `describe_sdk_error` string to decide whether to fail
    /// the backup or warn + continue.
    #[test]
    fn is_unsupported_error_matches_r2_put_object_tagging() {
        let r2_describe = "put_object_tagging on s3://bucket/key failed | HTTP 501 \
             | code=NotImplemented | message=PutObjectTagging not implemented \
             | body=<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error>\
             <Code>NotImplemented</Code><Message>PutObjectTagging not implemented\
             </Message></Error>";
        assert!(
            is_unsupported_error(r2_describe),
            "must recognise the R2 PutObjectTagging 501 shape"
        );
    }

    /// Regression: R2 also returns the same `NotImplemented` family when
    /// the `x-amz-tagging` header is passed on a put/create-multipart
    /// upload. The upload path no longer sends that header, but if a
    /// future change re-introduces it the `is_unsupported_error` matcher
    /// must still classify it correctly.
    #[test]
    fn is_unsupported_error_matches_r2_x_amz_tagging() {
        let r2_describe = "create_multipart_upload failed | HTTP 501 \
             | code=NotImplemented | message=Header 'x-amz-tagging' with value \
             'temps-managed=true&temps-retention-days=7' not implemented";
        assert!(is_unsupported_error(r2_describe));
    }

    /// Build an S3 client pointed at an arbitrary endpoint with hardcoded
    /// credentials. Mirrors `engines::v2_common::build_s3_client` but
    /// bypasses the encryption layer so testcontainer fixtures stay terse.
    fn test_s3_client(endpoint: &str, access: &str, secret: &str) -> aws_sdk_s3::Client {
        let cfg = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                access,
                secret,
                None,
                None,
                "temps-s3-lifecycle-test",
            ))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .http_client(crate::engines::v2_common::bundled_roots_http_client())
            .build();
        aws_sdk_s3::Client::from_conf(cfg)
    }

    /// End-to-end roundtrip: push lifecycle rules to a bucket, then read
    /// them back via `get_bucket_lifecycle_configuration` and assert the
    /// shape matches.
    ///
    /// We don't go through the full `S3LifecycleService::reconcile_bucket`
    /// here — that would require seeding rows in a Postgres testcontainer
    /// just to drive the SDK call. The interesting failure mode is
    /// "provider rejects the SDK request body," which is fully covered by
    /// `apply_lifecycle` + `build_lifecycle_rules`.
    async fn assert_lifecycle_roundtrip(
        client: &aws_sdk_s3::Client,
        bucket: &str,
        retentions: &[i32],
    ) {
        let rules = build_lifecycle_rules(retentions);
        let outcome = apply_lifecycle(client, bucket, rules, 999)
            .await
            .expect("apply_lifecycle should succeed against test backend");

        match outcome {
            ReconcileOutcome::Applied { rule_count } => {
                assert_eq!(rule_count, retentions.len());
            }
            other => panic!("expected Applied, got {:?}", other),
        }

        let read_back = client
            .get_bucket_lifecycle_configuration()
            .bucket(bucket)
            .send()
            .await
            .expect("get_bucket_lifecycle_configuration");

        let rules = read_back.rules();
        assert_eq!(rules.len(), retentions.len(), "rule count mismatch");

        for days in retentions {
            let expected_id = format!("temps-retention-{}d", days);
            let rule = rules
                .iter()
                .find(|r| r.id() == Some(expected_id.as_str()))
                .unwrap_or_else(|| panic!("missing rule {}", expected_id));
            assert_eq!(rule.status(), &ExpirationStatus::Enabled);
            let exp = rule.expiration().expect("expiration set");
            assert_eq!(exp.days(), Some(*days));
            let filter = rule.filter().expect("filter set");
            let tag = filter.tag().expect("tag filter set");
            assert_eq!(tag.key(), "temps-retention-days");
            assert_eq!(tag.value(), days.to_string());
        }
    }

    #[tokio::test]
    async fn test_lifecycle_against_minio() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping MinIO lifecycle test");
            return;
        }
        use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

        let container = match GenericImage::new("minio/minio", "latest")
            .with_env_var("MINIO_ROOT_USER", "minioadmin")
            .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
            .with_cmd(vec!["server", "/data", "--console-address", ":9001"])
            .start()
            .await
        {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to start MinIO container ({}), skipping", e);
                return;
            }
        };

        let port = container
            .get_host_port_ipv4(9000)
            .await
            .expect("Failed to get MinIO port");
        let endpoint = format!("http://localhost:{}", port);
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        let client = test_s3_client(&endpoint, "minioadmin", "minioadmin");
        let bucket = "lifecycle-test";
        client
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .expect("Failed to create bucket");

        assert_lifecycle_roundtrip(&client, bucket, &[7, 30, 90]).await;
    }

    #[tokio::test]
    async fn test_lifecycle_against_rustfs() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping RustFS lifecycle test");
            return;
        }
        use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

        // RustFS is API-compatible with MinIO; default access/secret is
        // `rustfsadmin` per the project's quickstart docs. The S3 port is
        // 9000, same as MinIO.
        let container = match GenericImage::new("rustfs/rustfs", "latest")
            .with_env_var("RUSTFS_ROOT_USER", "rustfsadmin")
            .with_env_var("RUSTFS_ROOT_PASSWORD", "rustfsadmin")
            .start()
            .await
        {
            Ok(c) => c,
            Err(e) => {
                println!(
                    "Failed to start RustFS container ({}) — image may not be \
                     available on this host, skipping",
                    e
                );
                return;
            }
        };

        let port = match container.get_host_port_ipv4(9000).await {
            Ok(p) => p,
            Err(e) => {
                println!("RustFS port mapping failed ({}), skipping", e);
                return;
            }
        };
        let endpoint = format!("http://localhost:{}", port);
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        let client = test_s3_client(&endpoint, "rustfsadmin", "rustfsadmin");
        let bucket = "lifecycle-test";
        if let Err(e) = client.create_bucket().bucket(bucket).send().await {
            println!(
                "Failed to create RustFS bucket ({}), skipping — likely the \
                 image isn't running or the credentials differ on this version",
                e
            );
            return;
        }

        assert_lifecycle_roundtrip(&client, bucket, &[14, 60]).await;
    }
}
