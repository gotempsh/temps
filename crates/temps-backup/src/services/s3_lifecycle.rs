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
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QuerySelect, QueryTrait,
};
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

/// Process-level locks keyed by `s3_source_id`, serializing
/// `reconcile_bucket` calls for the same source. The lifecycle sweep and
/// every event-driven trigger (`BackupService::fire_lifecycle_reconcile`)
/// only ever run on the control-plane process, so an in-process lock is
/// sufficient — no cross-process coordination is needed. This mirrors
/// `ENVIRONMENT_LOCKS` in `temps-deployments/src/jobs/mark_deployment_complete.rs`,
/// which rejected PostgreSQL advisory locks for the same reason: Sea-ORM's
/// `DatabaseConnection` is pooled, so an advisory lock/unlock pair can hit
/// different pooled connections and leave the lock permanently held.
static SOURCE_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<i32, Arc<tokio::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn get_source_lock(s3_source_id: i32) -> Arc<tokio::sync::Mutex<()>> {
    SOURCE_LOCKS
        .lock()
        .expect("source locks poisoned")
        .entry(s3_source_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Retry policy for the two small bookkeeping writes
/// (`begin_reconcile_attempt`, `record_reconcile_attempt`) that guard the
/// retry marker: a handful of fast retries, since these are single-row
/// local DB round-trips, not calls to an external API. Enough to ride out a
/// dropped pool connection or a momentary Postgres restart without
/// meaningfully delaying the reconcile, but bounded so a genuine outage
/// still surfaces as an error rather than hanging.
fn reconcile_bookkeeping_retry() -> temps_core::retry::RetryConfig {
    temps_core::retry::RetryConfig::new(3)
        .with_base_delay(std::time::Duration::from_millis(100))
        .with_max_delay(std::time::Duration::from_secs(1))
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
    ///
    /// Two reconciles for the same source can overlap — an event-driven
    /// trigger and the hourly sweep, or two schedule mutations in quick
    /// succession — and this call is not itself ordered relative to them.
    /// Without serialization, an older attempt (reading stale schedule
    /// state) could push its `PutBucketLifecycleConfiguration` /
    /// `DeleteBucketLifecycle` call to S3 *after* a newer attempt's,
    /// silently overwriting the correct state with stale rules even though
    /// the generation guard on `record_reconcile_attempt` correctly
    /// prevents the stale attempt's DB write from clobbering the newer
    /// one's retry marker. Acquiring `get_source_lock` first makes the two
    /// attempts run strictly one after the other, so the second always
    /// recomputes from fresh schedule state and its S3 call is the one
    /// that actually lands last.
    pub async fn reconcile_bucket(
        &self,
        s3_source_id: i32,
    ) -> Result<ReconcileOutcome, BackupError> {
        let source_lock = get_source_lock(s3_source_id);
        let _guard = source_lock.lock().await;

        // Claim this attempt's generation first — before schedule lookup,
        // S3 client construction, or the S3 call itself, i.e. before
        // anything else in this function that can fail. `begin_reconcile_attempt`
        // itself confirms the source exists (its `UPDATE ... RETURNING`
        // reports `NotFound` if no row matched), so no separate existence
        // check is needed here. Every failure past this point (a
        // schedule-fetch DB error, a credential-decrypt error building the
        // client, a rejected S3 call) is captured by `do_reconcile` and
        // reported through `record_reconcile_attempt` under this same
        // generation. Claiming it any later would let those earlier
        // failure modes slip through with no retry marker ever recorded —
        // the source would then silently vanish from `sources_in_scope`
        // the moment its last schedule is disabled, with nothing left to
        // retry it.
        let generation = self.begin_reconcile_attempt(s3_source_id).await?;

        let result = self.do_reconcile(s3_source_id).await;

        // Record the outcome so a transient failure keeps this source in
        // `sources_in_scope` — and therefore in the hourly sweep's retry
        // path — even if it has no enabled schedule at the moment (e.g. the
        // schedule that triggered this reconcile was the one just
        // disabled). Cleared on the next success so a source that's
        // genuinely out of scope stops being swept once it converges.
        if let Err(e) = self
            .record_reconcile_attempt(s3_source_id, generation, result.is_err())
            .await
        {
            warn!(
                s3_source_id,
                error = %e,
                "failed to persist S3 lifecycle reconcile retry state"
            );
        }

        result
    }

    /// The actual reconcile work — schedule lookup through the S3 call.
    /// Split out from `reconcile_bucket` so every error path here (schedule
    /// fetch, S3 client construction, the S3 call itself) runs *after* a
    /// generation has already been claimed, letting the caller record the
    /// failure under that generation regardless of which step raised it.
    async fn do_reconcile(&self, s3_source_id: i32) -> Result<ReconcileOutcome, BackupError> {
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
            clear_temps_rules(&client, &source.bucket_name, s3_source_id).await
        } else {
            let rules = build_lifecycle_rules(&retentions);
            apply_lifecycle(&client, &source.bucket_name, rules, s3_source_id).await
        }
    }

    /// Atomically claim the next reconcile "generation" for `s3_source_id`
    /// and return it. Pairs with `record_reconcile_attempt`, which only
    /// applies its write while this is still the newest claimed generation.
    ///
    /// Retried with backoff: a bare transient DB error here (a dropped pool
    /// connection, a momentary Postgres restart) would otherwise abort the
    /// whole reconcile before it even reaches the S3 call, leaving no
    /// retry marker recorded — indistinguishable from the source having
    /// converged, and (once its last schedule is disabled) permanently
    /// excluded from the sweep with a stale lifecycle rule still in S3.
    async fn begin_reconcile_attempt(&self, s3_source_id: i32) -> Result<i32, BackupError> {
        reconcile_bookkeeping_retry()
            .retry(|| async {
                let row = self
                    .db
                    .query_one(sea_orm::Statement::from_sql_and_values(
                        sea_orm::DatabaseBackend::Postgres,
                        "UPDATE s3_sources SET lifecycle_reconcile_generation = lifecycle_reconcile_generation + 1 \
                         WHERE id = $1 RETURNING lifecycle_reconcile_generation",
                        [s3_source_id.into()],
                    ))
                    .await
                    .map_err(BackupError::Database)?
                    .ok_or_else(|| BackupError::NotFound {
                        resource: "s3_source".to_string(),
                        detail: format!("id {}", s3_source_id),
                    })?;
                row.try_get("", "lifecycle_reconcile_generation")
                    .map_err(BackupError::Database)
            })
            .await
    }

    /// Persist whether the reconcile attempt for `s3_source_id` failed, so
    /// `sources_in_scope` can keep retrying it independent of schedule
    /// state. Best-effort: a failure here doesn't change `reconcile_bucket`'s
    /// return value, since the S3 call itself already succeeded or failed on
    /// its own terms — this is only bookkeeping for the next sweep.
    ///
    /// Guarded by `generation` (claimed via `begin_reconcile_attempt` before
    /// the S3 call): the write only applies `WHERE
    /// lifecycle_reconcile_generation = generation`, i.e. only if no other
    /// attempt has *started* for this source since this one did. Two
    /// reconciles for the same source can overlap — an event-driven
    /// reconcile and the hourly sweep, or two schedule mutations in quick
    /// succession — and finish in either order. Without this guard, an
    /// older attempt that started first but finishes last could overwrite a
    /// newer attempt's failure marker with its own (stale) success,
    /// silently dropping the source from every future retry even though the
    /// newer, more-informed attempt actually failed. The generation check
    /// makes only the most-recently-*started* attempt's outcome authoritative;
    /// a stale write is simply dropped (`exec` reports 0 rows affected,
    /// which isn't an error — the newer attempt already recorded, or will
    /// record, the outcome that matters).
    ///
    /// Also retried with backoff, for the same reason as
    /// `begin_reconcile_attempt`: a bare transient DB error on this write
    /// alone (distinct from the S3 call succeeding or failing) must not be
    /// the difference between "retry marker recorded" and "source silently
    /// drops out of the sweep."
    async fn record_reconcile_attempt(
        &self,
        s3_source_id: i32,
        generation: i32,
        failed: bool,
    ) -> Result<(), BackupError> {
        reconcile_bookkeeping_retry()
            .retry(|| async {
                temps_entities::s3_sources::Entity::update_many()
                    .col_expr(
                        temps_entities::s3_sources::Column::LifecycleReconcileFailedAt,
                        sea_orm::sea_query::Expr::value(failed.then(chrono::Utc::now)),
                    )
                    .filter(temps_entities::s3_sources::Column::Id.eq(s3_source_id))
                    .filter(
                        temps_entities::s3_sources::Column::LifecycleReconcileGeneration
                            .eq(generation),
                    )
                    .exec(self.db.as_ref())
                    .await
                    .map(|_| ())
                    .map_err(BackupError::Database)
            })
            .await
    }

    /// S3 sources actually in scope for Temps-managed lifecycle rules:
    /// those with at least one enabled backup schedule, any bucket Temps
    /// Cloud provisioned itself (`managed_by_cloud`), or any source with a
    /// reconcile attempt still pending retry (`lifecycle_reconcile_failed_at`
    /// set).
    ///
    /// Deliberately excludes sources with no enabled schedule,
    /// `managed_by_cloud = false`, and no pending retry — buckets the
    /// operator configured with their own credentials but never attached to
    /// a backup schedule (e.g. an unrelated production bucket).
    /// `reconcile_bucket` has never had anything to apply for those, so
    /// calling `PutBucketLifecycleConfiguration` / `DeleteBucketLifecycle`
    /// against them is a paid API call against infrastructure Temps doesn't
    /// manage, for no behavioral benefit.
    ///
    /// The pending-retry clause exists because disabling a source's last
    /// enabled schedule fires an immediate reconcile to clear its rules; if
    /// that attempt hits a transient S3 error, the source would otherwise
    /// drop out of scope in the same moment it needed a retry, stranding a
    /// stale lifecycle rule in S3 with nothing left to clear it. Keeping it
    /// in scope until reconcile actually succeeds (which clears the flag —
    /// see `record_reconcile_attempt`) bounds staleness to the sweep
    /// interval instead of leaving it indefinite.
    pub async fn sources_in_scope(
        &self,
    ) -> Result<Vec<temps_entities::s3_sources::Model>, BackupError> {
        let scheduled_source_ids = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::Enabled.eq(true))
            .select_only()
            .column(temps_entities::backup_schedules::Column::S3SourceId)
            .into_query();

        temps_entities::s3_sources::Entity::find()
            .filter(
                Condition::any()
                    .add(temps_entities::s3_sources::Column::ManagedByCloud.eq(true))
                    .add(temps_entities::s3_sources::Column::Id.in_subquery(scheduled_source_ids))
                    .add(
                        temps_entities::s3_sources::Column::LifecycleReconcileFailedAt
                            .is_not_null(),
                    ),
            )
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)
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

    /// Regression: without `get_source_lock`, two overlapping
    /// `reconcile_bucket` calls for the same source can each push a
    /// different desired lifecycle state to S3 independently, and whichever
    /// HTTP call lands last on S3 wins regardless of which attempt started
    /// with fresher schedule state — the generation guard on
    /// `record_reconcile_attempt` only protects the DB bookkeeping, not the
    /// actual S3 mutation order. This proves the lock actually serializes
    /// same-source access: while the first guard is held, a second
    /// acquisition attempt for the same id must block until it's released.
    #[tokio::test]
    async fn source_lock_serializes_calls_for_the_same_source() {
        let order: Arc<tokio::sync::Mutex<Vec<&'static str>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));

        let first_lock = get_source_lock(90_001);
        let first_guard = first_lock.lock().await;

        let order_clone = order.clone();
        let waiter = tokio::spawn(async move {
            let second_lock = get_source_lock(90_001);
            let _second_guard = second_lock.lock().await;
            order_clone.lock().await.push("second acquired");
        });

        // Give the spawned task a chance to actually attempt (and block
        // on) the same-source lock before we record that the first guard
        // is still held.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        order.lock().await.push("first still holding");
        drop(first_guard);

        waiter.await.expect("waiter task completes");

        assert_eq!(
            *order.lock().await,
            vec!["first still holding", "second acquired"],
            "a second reconcile for the same source must block until the first releases the lock"
        );
    }

    /// Complement to the test above: two different source ids must not
    /// contend on the same lock, so the sweep isn't accidentally serialized
    /// across unrelated sources.
    #[tokio::test]
    async fn source_lock_does_not_serialize_different_sources() {
        let held_lock = get_source_lock(90_101);
        let _held_guard = held_lock.lock().await;

        let other_lock = get_source_lock(90_102);
        let other_acquired =
            tokio::time::timeout(std::time::Duration::from_millis(500), other_lock.lock()).await;

        assert!(
            other_acquired.is_ok(),
            "a different source id's lock must be acquirable while an unrelated source's lock is held"
        );
    }

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

    /// Regression: the hourly lifecycle sweep must not touch S3 sources
    /// the operator configured with their own credentials but never
    /// attached to a backup schedule — those API calls cost money against
    /// infrastructure Temps doesn't manage. Seeds four sources against a
    /// real Postgres: one with an enabled schedule (in scope), one with
    /// only a disabled schedule (out of scope), one `managed_by_cloud`
    /// with no schedule at all (in scope), and one plain unattached
    /// source (out of scope) — then asserts `sources_in_scope` returns
    /// exactly the two that should be reconciled.
    #[tokio::test]
    async fn sources_in_scope_excludes_unscheduled_operator_buckets() {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};

        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(d) => d,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();

        let insert_source = |name: &str, managed_by_cloud: bool| {
            let now = chrono::Utc::now();
            temps_entities::s3_sources::ActiveModel {
                id: sea_orm::ActiveValue::NotSet,
                name: Set(name.to_string()),
                bucket_name: Set(format!("{name}-bucket")),
                bucket_path: Set("/".to_string()),
                access_key_id: Set(String::new()),
                secret_key: Set(String::new()),
                session_token: Set(None),
                credentials_expire_at: Set(None),
                region: Set("us-east-1".to_string()),
                endpoint: Set(None),
                force_path_style: Set(Some(true)),
                is_default: Set(false),
                managed_by_cloud: Set(managed_by_cloud),
                lifecycle_reconcile_failed_at: Set(None),
                lifecycle_reconcile_generation: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
            }
        };

        let scheduled = insert_source("scheduled", false)
            .insert(db.as_ref())
            .await
            .expect("insert scheduled source");
        let disabled_only = insert_source("disabled-only", false)
            .insert(db.as_ref())
            .await
            .expect("insert disabled-only source");
        let cloud_managed = insert_source("cloud-managed", true)
            .insert(db.as_ref())
            .await
            .expect("insert cloud-managed source");
        let unattached = insert_source("unattached", false)
            .insert(db.as_ref())
            .await
            .expect("insert unattached source");

        let insert_schedule = |name: &str, s3_source_id: i32, enabled: bool| {
            let now = chrono::Utc::now();
            temps_entities::backup_schedules::ActiveModel {
                id: sea_orm::ActiveValue::NotSet,
                name: Set(name.to_string()),
                backup_type: Set("full".to_string()),
                retention_period: Set(7),
                s3_source_id: Set(s3_source_id),
                schedule_expression: Set("0 0 2 * * *".to_string()),
                enabled: Set(enabled),
                last_run: Set(None),
                next_run: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                description: Set(None),
                tags: Set("[]".to_string()),
                max_runtime_secs: Set(None),
                target_all_services: Set(false),
                include_control_plane: Set(true),
            }
        };

        insert_schedule("enabled-sched", scheduled.id, true)
            .insert(db.as_ref())
            .await
            .expect("insert enabled schedule");
        insert_schedule("disabled-sched", disabled_only.id, false)
            .insert(db.as_ref())
            .await
            .expect("insert disabled schedule");
        // `unattached` and `cloud_managed` intentionally have no schedule.

        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "test_encryption_key_1234567890ab",
        ));
        let svc = S3LifecycleService::new(db.clone(), encryption);

        let in_scope_ids: std::collections::HashSet<i32> = svc
            .sources_in_scope()
            .await
            .expect("sources_in_scope should succeed")
            .into_iter()
            .map(|s| s.id)
            .collect();

        assert!(
            in_scope_ids.contains(&scheduled.id),
            "source with an enabled schedule must be in scope"
        );
        assert!(
            in_scope_ids.contains(&cloud_managed.id),
            "managed_by_cloud source must be in scope even with no schedule"
        );
        assert!(
            !in_scope_ids.contains(&disabled_only.id),
            "source with only a disabled schedule must NOT be in scope"
        );
        assert!(
            !in_scope_ids.contains(&unattached.id),
            "unattached operator bucket must NOT be in scope — reconciling it wastes a paid S3 API call"
        );
    }

    /// Regression: disabling a source's last enabled schedule fires an
    /// immediate reconcile (`BackupService::fire_lifecycle_reconcile`). If
    /// that attempt hits a transient S3 error, the source must not simply
    /// fall out of scope — `lifecycle_reconcile_failed_at` (set by
    /// `record_reconcile_attempt`) has to keep it in the hourly sweep until
    /// a retry actually succeeds, or its stale lifecycle rule would persist
    /// in S3 indefinitely with nothing left to clear it.
    #[tokio::test]
    async fn sources_in_scope_includes_source_with_a_pending_reconcile_retry() {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};

        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(d) => d,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();
        let now = chrono::Utc::now();

        let source = temps_entities::s3_sources::ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            name: Set("disabled-with-pending-retry".to_string()),
            bucket_name: Set("disabled-with-pending-retry-bucket".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set(String::new()),
            secret_key: Set(String::new()),
            session_token: Set(None),
            credentials_expire_at: Set(None),
            region: Set("us-east-1".to_string()),
            endpoint: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(false),
            managed_by_cloud: Set(false),
            lifecycle_reconcile_failed_at: Set(None),
            lifecycle_reconcile_generation: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db.as_ref())
        .await
        .expect("insert source");

        temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            name: Set("disabled-sched".to_string()),
            backup_type: Set("full".to_string()),
            retention_period: Set(7),
            s3_source_id: Set(source.id),
            schedule_expression: Set("0 0 2 * * *".to_string()),
            enabled: Set(false),
            last_run: Set(None),
            next_run: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            description: Set(None),
            tags: Set("[]".to_string()),
            max_runtime_secs: Set(None),
            target_all_services: Set(false),
            include_control_plane: Set(true),
        }
        .insert(db.as_ref())
        .await
        .expect("insert disabled schedule");

        // Simulate the bookkeeping `record_reconcile_attempt` performs when
        // the disable-time fire-and-forget reconcile errors.
        temps_entities::s3_sources::ActiveModel {
            id: Set(source.id),
            lifecycle_reconcile_failed_at: Set(Some(now)),
            lifecycle_reconcile_generation: Set(0),
            ..Default::default()
        }
        .update(db.as_ref())
        .await
        .expect("mark reconcile attempt as failed");

        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "test_encryption_key_1234567890ab",
        ));
        let svc = S3LifecycleService::new(db.clone(), encryption);

        let in_scope_ids: std::collections::HashSet<i32> = svc
            .sources_in_scope()
            .await
            .expect("sources_in_scope should succeed")
            .into_iter()
            .map(|s| s.id)
            .collect();

        assert!(
            in_scope_ids.contains(&source.id),
            "a source with a pending reconcile retry must stay in scope even with no enabled \
             schedule, so the hourly sweep keeps retrying until it converges"
        );
    }

    /// Regression (Greptile P1 on the fix above): two reconciles for the
    /// same source can overlap and finish out of start order — an
    /// event-driven reconcile and the hourly sweep, or two schedule
    /// mutations in quick succession. Without a generation guard, an older
    /// attempt that starts first but finishes *last* could write its own
    /// (stale) success over a newer attempt's failure, silently clearing
    /// `lifecycle_reconcile_failed_at` and dropping the source out of every
    /// future retry even though the more recent, more-informed attempt
    /// actually failed. `record_reconcile_attempt` must refuse a write once
    /// a newer attempt has already claimed the generation.
    #[tokio::test]
    async fn a_stale_reconcile_attempt_cannot_clobber_a_newer_attempts_failure() {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};

        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(d) => d,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();
        let now = chrono::Utc::now();

        let source = temps_entities::s3_sources::ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            name: Set("overlapping-reconciles".to_string()),
            bucket_name: Set("overlapping-reconciles-bucket".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set(String::new()),
            secret_key: Set(String::new()),
            session_token: Set(None),
            credentials_expire_at: Set(None),
            region: Set("us-east-1".to_string()),
            endpoint: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(false),
            managed_by_cloud: Set(true),
            lifecycle_reconcile_failed_at: Set(None),
            lifecycle_reconcile_generation: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db.as_ref())
        .await
        .expect("insert source");

        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "test_encryption_key_1234567890ab",
        ));
        let svc = S3LifecycleService::new(db.clone(), encryption);

        // Two attempts start, in order: an older one (e.g. an event-driven
        // reconcile from an earlier schedule mutation), then a newer one
        // (e.g. the immediate reconcile fired by disabling the last
        // schedule).
        let older_generation = svc
            .begin_reconcile_attempt(source.id)
            .await
            .expect("begin older attempt");
        let newer_generation = svc
            .begin_reconcile_attempt(source.id)
            .await
            .expect("begin newer attempt");
        assert!(newer_generation > older_generation);

        // The newer attempt finishes first and fails.
        svc.record_reconcile_attempt(source.id, newer_generation, true)
            .await
            .expect("record newer failure");

        // The older attempt, despite having started first, finishes last
        // and succeeds. Its write must be dropped rather than clearing the
        // newer attempt's failure marker.
        svc.record_reconcile_attempt(source.id, older_generation, false)
            .await
            .expect("record older success (must be a no-op)");

        let refreshed = temps_entities::s3_sources::Entity::find_by_id(source.id)
            .one(db.as_ref())
            .await
            .expect("refetch source")
            .expect("source still exists");

        assert!(
            refreshed.lifecycle_reconcile_failed_at.is_some(),
            "a stale (older) attempt's success must not clear a newer attempt's failure marker"
        );
    }
}
