// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Business logic for feature flags.

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::{
    sea_query::Expr, AccessMode, ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait,
    DatabaseConnection, EntityTrait, IsolationLevel, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
};
use temps_entities::{environments, feature_flag_environments, feature_flags};
use tracing::{debug, info, warn};

use super::delivery::{DeliveredFlag, EnvironmentFlags, FlagDeliveryCache, Lookup, RefreshClaim};
use super::validation::{
    referenced_variants, validate_exposure_sample_rate, validate_rules, validate_variants,
};
use crate::error::FlagError;
use crate::eval::{
    evaluate, CompiledPatterns, EvalContext, Evaluation, FlagRule, FlagSnapshot, FlagValueType,
    FlagVariant, RuleServe,
};

/// Pagination defaults, per the project-wide convention.
const DEFAULT_PAGE_SIZE: u64 = 20;
const MAX_PAGE_SIZE: u64 = 100;

/// Upper bound on keys accepted in one exposure report. A project with more
/// flags than this simply takes two batches; the cap exists so a client cannot
/// hand the server an unbounded `IN` list.
pub const MAX_EXPOSURE_KEYS: usize = 500;

/// How stale `last_evaluated_at` must be before an exposure report rewrites
/// it. The column answers "is this flag still read at all", so hour-level
/// resolution is ample and second-level would be pure write amplification.
const EXPOSURE_STALENESS_SECS: i64 = 3600;

/// Maximum length of a flag key, matching the column width.
const MAX_KEY_LEN: usize = 128;
/// Maximum length of a flag description, matching the column width.
const MAX_DESCRIPTION_LEN: usize = 512;

/// A flag definition together with its per-environment overrides.
#[derive(Debug, Clone)]
pub struct FlagWithEnvironments {
    pub flag: feature_flags::Model,
    pub environments: Vec<feature_flag_environments::Model>,
}

/// What [`FlagService::update`] changed: the row as it was under the write
/// lock (the audit's before-image), the row as written, and the flag's
/// environments.
#[derive(Debug, Clone)]
pub struct UpdatedFlag {
    pub before: feature_flags::Model,
    pub flag: feature_flags::Model,
    pub environments: Vec<feature_flag_environments::Model>,
}

/// Fields accepted when creating a flag.
#[derive(Debug, Clone)]
pub struct CreateFlag {
    pub key: String,
    pub value_type: FlagValueType,
    pub default_value: serde_json::Value,
    pub description: Option<String>,
    pub client_visible: bool,
    pub variants: Vec<FlagVariant>,
    pub exposure_sample_rate: f64,
}

/// Fields accepted when updating a flag definition. `None` means "leave alone".
///
/// `key` and `value_type` are deliberately absent: keys leak into user source
/// code and analytics dimensions, and retyping would invalidate every stored
/// value. Both are immutable after create.
#[derive(Debug, Clone, Default)]
pub struct UpdateFlag {
    pub default_value: Option<serde_json::Value>,
    pub description: Option<Option<String>>,
    pub client_visible: Option<bool>,
    /// Replaces the whole variant list. A variant still served by a rule in
    /// any environment cannot be removed.
    pub variants: Option<Vec<FlagVariant>>,
    pub exposure_sample_rate: Option<f64>,
    /// When set, the update applies only if the flag's `updated_at` still
    /// equals this, so two editors cannot silently overwrite each other.
    pub expected_updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A replacement rule list for one environment of a flag.
#[derive(Debug, Clone, Default)]
pub struct SetRules {
    pub rules: Vec<FlagRule>,
    /// The `updated_at` of the environment row the caller edited, or `None`
    /// when it saw no row. The write is refused if the row changed since:
    /// rules are an ordered list, and a read-modify-write that lost a race
    /// would drop or reorder someone else's rule.
    pub expected_updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Fields accepted when setting a flag's value in one environment.
#[derive(Debug, Clone, Default)]
pub struct SetEnvironmentValue {
    /// `Some(None)` clears the override so the flag inherits its default.
    pub value: Option<Option<serde_json::Value>>,
    pub enabled: Option<bool>,
}

pub struct FlagService {
    db: Arc<DatabaseConnection>,
    /// Evaluated-endpoint cache. Owned here so every write path invalidates
    /// it; nothing outside the service can forget to.
    delivery: FlagDeliveryCache,
}

/// Two timestamps are the same revision when they agree to the microsecond,
/// which is Postgres' `timestamptz` precision.
fn same_revision(a: chrono::DateTime<chrono::Utc>, b: chrono::DateTime<chrono::Utc>) -> bool {
    a.timestamp_micros() == b.timestamp_micros()
}

/// Decode a flag's stored variants. A row that does not decode serves no
/// variants, so any rule naming one degrades to `ERROR` instead of failing.
pub fn parse_variants(flag: &feature_flags::Model) -> Vec<FlagVariant> {
    match serde_json::from_value(flag.variants.clone()) {
        Ok(variants) => variants,
        Err(error) => {
            warn!(
                flag_key = %flag.key,
                project_id = flag.project_id,
                %error,
                "Stored feature flag variants do not decode; serving none"
            );
            Vec::new()
        }
    }
}

/// Decode an environment's stored rules. `None` when they do not decode.
pub fn parse_rules(row: &feature_flag_environments::Model) -> Option<Vec<FlagRule>> {
    serde_json::from_value(row.rules.clone()).ok()
}

/// What an undecodable rule list evaluates as: one rule that cannot be
/// evaluated, so the flag serves its default with reason `ERROR` (ADR 034
/// R2.6) rather than silently ignoring the targeting someone configured.
fn unreadable_rules() -> Vec<FlagRule> {
    vec![FlagRule {
        description: Some("stored rules could not be decoded".to_string()),
        conditions: Vec::new(),
        rollout_percentage: None,
        serve: RuleServe::Split { split: Vec::new() },
    }]
}

impl FlagService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            delivery: FlagDeliveryCache::new(),
        }
    }

    // ---------------------------------------------------------------------
    // Validation
    // ---------------------------------------------------------------------

    /// Flag keys must match `[a-z0-9][a-z0-9._-]*`.
    ///
    /// Hand-rolled rather than pulling in a regex: the rule is small, and the
    /// error message can name the offending character, which a regex failure
    /// cannot.
    fn validate_key(key: &str) -> Result<(), FlagError> {
        if key.is_empty() {
            return Err(FlagError::InvalidKey {
                key: key.to_string(),
                reason: "key is empty".to_string(),
            });
        }
        if key.len() > MAX_KEY_LEN {
            return Err(FlagError::InvalidKey {
                key: key.to_string(),
                reason: format!("key is {} characters", key.len()),
            });
        }

        let mut chars = key.chars();
        // Unwrap-free: emptiness was rejected above, but handle it anyway.
        let Some(first) = chars.next() else {
            return Err(FlagError::InvalidKey {
                key: key.to_string(),
                reason: "key is empty".to_string(),
            });
        };
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(FlagError::InvalidKey {
                key: key.to_string(),
                reason: format!("first character '{first}' must be a-z or 0-9"),
            });
        }

        for c in chars {
            let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
            if !ok {
                return Err(FlagError::InvalidKey {
                    key: key.to_string(),
                    reason: format!("character '{c}' is not allowed"),
                });
            }
        }

        Ok(())
    }

    fn validate_value(
        key: &str,
        value_type: FlagValueType,
        value: &serde_json::Value,
        field: &str,
    ) -> Result<(), FlagError> {
        if value_type.matches(value) {
            return Ok(());
        }
        Err(FlagError::ValueTypeMismatch {
            key: key.to_string(),
            value_type: value_type.as_str().to_string(),
            value: value.to_string(),
            field: field.to_string(),
        })
    }

    fn validate_description(key: &str, description: &Option<String>) -> Result<(), FlagError> {
        if let Some(text) = description {
            if text.len() > MAX_DESCRIPTION_LEN {
                return Err(FlagError::Validation {
                    key: key.to_string(),
                    message: format!(
                        "description is {} characters, maximum is {MAX_DESCRIPTION_LEN}",
                        text.len()
                    ),
                });
            }
        }
        Ok(())
    }

    /// Generate a per-flag bucketing salt.
    ///
    /// Nothing reads this in Phase 1. It exists so percentage bucketing is
    /// stable from the first rollout — a salt introduced after subjects are
    /// already assigned would reshuffle every live experiment.
    fn generate_salt() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }

    // ---------------------------------------------------------------------
    // Reads
    // ---------------------------------------------------------------------

    /// List flags for a project, alphabetically by key, paginated.
    ///
    /// Key order rather than CLAUDE.md's usual `created_at` DESC: a flag list
    /// is scanned by name to find a specific key, not read as a timeline. Noted
    /// here because it is a deliberate deviation from the house default.
    ///
    /// Returns `(page_of_flags, total_matching_flags)`.
    ///
    /// Note the two queries rather than one `find_with_related`: a JOIN
    /// produces one row per (flag, environment) pair, so paginating it would
    /// slice *override rows*, not flags — a flag with three environments would
    /// consume three slots and the total would be wrong. Paginate the flags,
    /// then fetch their overrides in a single follow-up query (still no N+1).
    pub async fn list(
        &self,
        project_id: i32,
        include_archived: bool,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<FlagWithEnvironments>, u64), FlagError> {
        let (page, page_size) = normalize_pagination(page, page_size);
        self.require_project(project_id).await?;

        let mut query =
            feature_flags::Entity::find().filter(feature_flags::Column::ProjectId.eq(project_id));

        if !include_archived {
            query = query.filter(feature_flags::Column::ArchivedAt.is_null());
        }

        let paginator = query
            .order_by_asc(feature_flags::Column::Key)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let flags = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;

        if flags.is_empty() {
            return Ok((Vec::new(), total));
        }

        let flag_ids: Vec<i32> = flags.iter().map(|flag| flag.id).collect();
        let overrides = feature_flag_environments::Entity::find()
            .filter(feature_flag_environments::Column::FlagId.is_in(flag_ids))
            .all(self.db.as_ref())
            .await?;

        let mut by_flag: HashMap<i32, Vec<feature_flag_environments::Model>> = HashMap::new();
        for row in overrides {
            by_flag.entry(row.flag_id).or_default().push(row);
        }

        let entries = flags
            .into_iter()
            .map(|flag| {
                let environments = by_flag.remove(&flag.id).unwrap_or_default();
                FlagWithEnvironments { flag, environments }
            })
            .collect();

        Ok((entries, total))
    }

    /// A missing (or deleted) project is [`FlagError::ProjectNotFound`]: a
    /// 404, not an empty listing or a foreign-key violation on insert.
    async fn require_project(&self, project_id: i32) -> Result<(), FlagError> {
        if temps_entities::projects::live_project_exists(self.db.as_ref(), project_id).await? {
            Ok(())
        } else {
            Err(FlagError::ProjectNotFound { project_id })
        }
    }

    pub async fn get(&self, project_id: i32, key: &str) -> Result<FlagWithEnvironments, FlagError> {
        let flag = feature_flags::Entity::find()
            .filter(feature_flags::Column::ProjectId.eq(project_id))
            .filter(feature_flags::Column::Key.eq(key))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| FlagError::NotFound {
                project_id,
                key: key.to_string(),
            })?;

        let environments = feature_flag_environments::Entity::find()
            .filter(feature_flag_environments::Column::FlagId.eq(flag.id))
            .all(self.db.as_ref())
            .await?;

        Ok(FlagWithEnvironments { flag, environments })
    }

    // ---------------------------------------------------------------------
    // Writes
    // ---------------------------------------------------------------------

    pub async fn create(
        &self,
        project_id: i32,
        request: CreateFlag,
    ) -> Result<feature_flags::Model, FlagError> {
        Self::validate_key(&request.key)?;
        Self::validate_description(&request.key, &request.description)?;
        Self::validate_value(
            &request.key,
            request.value_type,
            &request.default_value,
            "default_value",
        )?;
        validate_variants(&request.key, request.value_type, &request.variants)?;
        validate_exposure_sample_rate(&request.key, request.exposure_sample_rate)?;
        let variants =
            serde_json::to_value(&request.variants).map_err(|e| FlagError::Serialization {
                key: request.key.clone(),
                reason: e.to_string(),
            })?;

        self.require_project(project_id).await?;
        let existing = feature_flags::Entity::find()
            .filter(feature_flags::Column::ProjectId.eq(project_id))
            .filter(feature_flags::Column::Key.eq(&request.key))
            .one(self.db.as_ref())
            .await?;
        if existing.is_some() {
            return Err(FlagError::DuplicateKey {
                project_id,
                key: request.key,
            });
        }

        let model = feature_flags::ActiveModel {
            project_id: Set(project_id),
            key: Set(request.key.clone()),
            value_type: Set(request.value_type.as_str().to_string()),
            default_value: Set(request.default_value),
            description: Set(request.description),
            salt: Set(Self::generate_salt()),
            client_visible: Set(request.client_visible),
            archived_at: Set(None),
            variants: Set(variants),
            exposure_sample_rate: Set(request.exposure_sample_rate),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        self.delivery.invalidate_project(project_id);

        info!(
            project_id,
            flag_key = %request.key,
            value_type = request.value_type.as_str(),
            "Created feature flag"
        );

        Ok(model)
    }

    pub async fn update(
        &self,
        project_id: i32,
        key: &str,
        request: UpdateFlag,
    ) -> Result<UpdatedFlag, FlagError> {
        let existing = self.get(project_id, key).await?;

        self.apply_update(key, existing, request).await
    }

    /// The update itself, given a flag the caller has already fetched.
    async fn apply_update(
        &self,
        key: &str,
        existing: FlagWithEnvironments,
        request: UpdateFlag,
    ) -> Result<UpdatedFlag, FlagError> {
        let FlagWithEnvironments {
            flag: existing,
            environments,
        } = existing;
        let project_id = existing.project_id;

        if let Some(expected) = request.expected_updated_at {
            if !same_revision(expected, existing.updated_at) {
                return Err(FlagError::StaleWrite {
                    project_id,
                    key: key.to_string(),
                    environment_id: None,
                });
            }
        }
        let previous_updated_at = existing.updated_at;
        let flag_id = existing.id;

        let value_type = FlagValueType::parse(&existing.value_type).ok_or_else(|| {
            FlagError::InvalidValueType {
                key: key.to_string(),
                value_type: existing.value_type.clone(),
            }
        })?;

        let mut active: feature_flags::ActiveModel = existing.into();

        if let Some(default_value) = request.default_value {
            Self::validate_value(key, value_type, &default_value, "default_value")?;
            active.default_value = Set(default_value);
        }
        if let Some(description) = request.description {
            Self::validate_description(key, &description)?;
            active.description = Set(description);
        }
        if let Some(client_visible) = request.client_visible {
            active.client_visible = Set(client_visible);
        }
        if let Some(rate) = request.exposure_sample_rate {
            validate_exposure_sample_rate(key, rate)?;
            active.exposure_sample_rate = Set(rate);
        }
        let new_variants = request.variants;
        if let Some(variants) = &new_variants {
            validate_variants(key, value_type, variants)?;
            // Fail fast on the rows already read; repeated under the lock below.
            Self::ensure_removed_variants_unused(project_id, key, variants, &environments)?;
            active.variants =
                Set(
                    serde_json::to_value(variants).map_err(|e| FlagError::Serialization {
                        key: key.to_string(),
                        reason: e.to_string(),
                    })?,
                );
        }

        // The checks above ran on a read that is not atomic with this write.
        // Repeat the ones that a concurrent writer can invalidate under a lock
        // on the flag row: the revision (compare-and-set), and whether a
        // removed variant is still served. `set_rules` takes a shared lock on
        // the same row before it writes, so a rule naming a variant cannot be
        // saved between this check and this write.
        let txn = self.db.begin().await?;
        let locked = feature_flags::Entity::find_by_id(flag_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or_else(|| FlagError::NotFound {
                project_id,
                key: key.to_string(),
            })?;
        if request.expected_updated_at.is_some()
            && !same_revision(locked.updated_at, previous_updated_at)
        {
            return Err(FlagError::StaleWrite {
                project_id,
                key: key.to_string(),
                environment_id: None,
            });
        }
        if let Some(variants) = &new_variants {
            let current_rows = feature_flag_environments::Entity::find()
                .filter(feature_flag_environments::Column::FlagId.eq(flag_id))
                .all(&txn)
                .await?;
            Self::ensure_removed_variants_unused(project_id, key, variants, &current_rows)?;
        }
        let model = active.update(&txn).await?;
        txn.commit().await?;
        self.delivery.invalidate_project(project_id);

        info!(project_id, flag_key = %key, "Updated feature flag");

        Ok(UpdatedFlag {
            before: locked,
            flag: model,
            environments,
        })
    }

    /// Refuse a variant list that drops a variant some environment's rules
    /// still serve. Removing it would turn those rules into `ERROR`
    /// evaluations in production.
    fn ensure_removed_variants_unused(
        project_id: i32,
        key: &str,
        variants: &[FlagVariant],
        environments: &[feature_flag_environments::Model],
    ) -> Result<(), FlagError> {
        let kept: std::collections::HashSet<&str> = variants
            .iter()
            .map(|variant| variant.key.as_str())
            .collect();
        for row in environments {
            let Some(rules) = parse_rules(row) else {
                continue;
            };
            if let Some(missing) = referenced_variants(&rules)
                .into_iter()
                .find(|name| !kept.contains(name))
            {
                return Err(FlagError::Validation {
                    key: key.to_string(),
                    message: format!(
                        "variant '{missing}' is still served by a rule in environment {} of project {project_id}; change that rule first",
                        row.environment_id
                    ),
                });
            }
        }
        Ok(())
    }

    /// Archive a flag. Deliberately a soft delete: an archived flag evaluates
    /// as `FLAG_NOT_FOUND`, so callers fall back to the default they compiled
    /// in rather than silently flipping behaviour.
    ///
    /// Idempotent. Re-archiving used to overwrite `archived_at` with `now()`,
    /// destroying the record of when the flag was actually retired — which is
    /// the one thing the timestamp is for.
    pub async fn archive(
        &self,
        project_id: i32,
        key: &str,
    ) -> Result<feature_flags::Model, FlagError> {
        let existing = self.get(project_id, key).await?.flag;

        if existing.archived_at.is_some() {
            return Ok(existing);
        }

        let mut active: feature_flags::ActiveModel = existing.into();
        active.archived_at = Set(Some(chrono::Utc::now()));

        let model = active.update(self.db.as_ref()).await?;
        self.delivery.invalidate_project(project_id);

        info!(project_id, flag_key = %key, "Archived feature flag");

        Ok(model)
    }

    /// Bring an archived flag back.
    ///
    /// Archiving is otherwise a one-way door with no route back through the
    /// API — an accidental `flags archive` would be unrecoverable, and the key
    /// stays reserved so it cannot even be re-created.
    pub async fn restore(
        &self,
        project_id: i32,
        key: &str,
    ) -> Result<feature_flags::Model, FlagError> {
        let existing = self.get(project_id, key).await?.flag;

        if existing.archived_at.is_none() {
            return Ok(existing);
        }

        let mut active: feature_flags::ActiveModel = existing.into();
        active.archived_at = Set(None);

        let model = active.update(self.db.as_ref()).await?;
        self.delivery.invalidate_project(project_id);

        info!(project_id, flag_key = %key, "Restored feature flag");

        Ok(model)
    }

    /// Set (or clear) the value of a flag in one environment, and/or flip the
    /// kill switch.
    pub async fn set_environment_value(
        &self,
        project_id: i32,
        key: &str,
        environment_id: i32,
        request: SetEnvironmentValue,
    ) -> Result<feature_flag_environments::Model, FlagError> {
        let flag = self.get(project_id, key).await?.flag;

        let value_type =
            FlagValueType::parse(&flag.value_type).ok_or_else(|| FlagError::InvalidValueType {
                key: key.to_string(),
                value_type: flag.value_type.clone(),
            })?;

        // A flag may only be overridden in an environment of its own project.
        // Without this a caller who can write project A's flags could attach a
        // value to project B's environment.
        self.ensure_environment_in_project(project_id, environment_id)
            .await?;

        if let Some(Some(value)) = &request.value {
            Self::validate_value(key, value_type, value, "value")?;
        }

        let txn = self.db.begin().await?;

        let existing = feature_flag_environments::Entity::find()
            .filter(feature_flag_environments::Column::FlagId.eq(flag.id))
            .filter(feature_flag_environments::Column::EnvironmentId.eq(environment_id))
            .one(&txn)
            .await?;

        let model = match existing {
            Some(row) => {
                let mut active: feature_flag_environments::ActiveModel = row.into();
                if let Some(value) = request.value {
                    active.value = Set(value);
                }
                if let Some(enabled) = request.enabled {
                    active.enabled = Set(enabled);
                }
                active.update(&txn).await?
            }
            None => {
                feature_flag_environments::ActiveModel {
                    flag_id: Set(flag.id),
                    environment_id: Set(environment_id),
                    enabled: Set(request.enabled.unwrap_or(true)),
                    value: Set(request.value.flatten()),
                    rules: Set(serde_json::json!([])),
                    ..Default::default()
                }
                .insert(&txn)
                .await?
            }
        };

        txn.commit().await?;
        self.delivery.invalidate_project(project_id);

        info!(
            project_id,
            flag_key = %key,
            environment_id,
            enabled = model.enabled,
            "Set feature flag environment value"
        );

        Ok(model)
    }

    async fn ensure_environment_in_project(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<environments::Model, FlagError> {
        let environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(FlagError::EnvironmentNotFound { environment_id })?;
        if environment.project_id != project_id {
            return Err(FlagError::EnvironmentNotInProject {
                project_id,
                environment_id,
            });
        }
        Ok(environment)
    }

    /// Replace the ordered targeting rules of a flag in one environment.
    ///
    /// Validated against the flag's type and declared variants before
    /// anything is written. Guarded by `expected_updated_at` (see
    /// [`SetRules`]), checked under a row lock so two concurrent editors
    /// cannot both pass.
    ///
    /// Returns the row before and after the write; the handler audits both.
    pub async fn set_rules(
        &self,
        project_id: i32,
        key: &str,
        environment_id: i32,
        request: SetRules,
    ) -> Result<
        (
            Option<feature_flag_environments::Model>,
            feature_flag_environments::Model,
        ),
        FlagError,
    > {
        let flag = self.get(project_id, key).await?.flag;
        let value_type =
            FlagValueType::parse(&flag.value_type).ok_or_else(|| FlagError::InvalidValueType {
                key: key.to_string(),
                value_type: flag.value_type.clone(),
            })?;
        self.ensure_environment_in_project(project_id, environment_id)
            .await?;
        validate_rules(key, value_type, &parse_variants(&flag), &request.rules)?;

        let rules = serde_json::to_value(&request.rules).map_err(|e| FlagError::Serialization {
            key: key.to_string(),
            reason: e.to_string(),
        })?;
        let stale = || FlagError::StaleWrite {
            project_id,
            key: key.to_string(),
            environment_id: Some(environment_id),
        };

        let txn = self.db.begin().await?;
        // Shared lock on the flag row: a concurrent variant edit (which locks
        // it exclusively) either finishes first, and the rules are validated
        // against its variants here, or waits until these rules are committed
        // and then sees them.
        let locked_flag = feature_flags::Entity::find_by_id(flag.id)
            .lock_shared()
            .one(&txn)
            .await?
            .ok_or_else(|| FlagError::NotFound {
                project_id,
                key: key.to_string(),
            })?;
        if locked_flag.variants != flag.variants {
            validate_rules(
                key,
                value_type,
                &parse_variants(&locked_flag),
                &request.rules,
            )?;
        }
        let existing = feature_flag_environments::Entity::find()
            .filter(feature_flag_environments::Column::FlagId.eq(flag.id))
            .filter(feature_flag_environments::Column::EnvironmentId.eq(environment_id))
            .lock_exclusive()
            .one(&txn)
            .await?;

        let (before, after) = match existing {
            Some(row) => {
                // An absent revision means the caller saw no row for this
                // environment; a row now exists, so someone else wrote first.
                match request.expected_updated_at {
                    Some(expected) if same_revision(expected, row.updated_at) => {}
                    _ => return Err(stale()),
                }
                let mut active: feature_flag_environments::ActiveModel = row.clone().into();
                active.rules = Set(rules);
                (Some(row), active.update(&txn).await?)
            }
            None => {
                // The caller edited a row that has since been deleted.
                if request.expected_updated_at.is_some() {
                    return Err(stale());
                }
                let inserted = feature_flag_environments::ActiveModel {
                    flag_id: Set(flag.id),
                    environment_id: Set(environment_id),
                    enabled: Set(true),
                    value: Set(None),
                    rules: Set(rules),
                    ..Default::default()
                }
                .insert(&txn)
                .await
                // Two first writes racing: the unique index rejects the
                // loser, which is a stale write, not a duplicate flag.
                .map_err(|error| match FlagError::from(error) {
                    FlagError::DuplicateKey { .. } => stale(),
                    other => other,
                })?;
                (None, inserted)
            }
        };
        txn.commit().await?;
        self.delivery.invalidate_project(project_id);

        info!(
            project_id,
            flag_key = %key,
            environment_id,
            rule_count = request.rules.len(),
            "Set feature flag targeting rules"
        );

        Ok((before, after))
    }

    /// Generate a new bucketing salt, reshuffling who falls inside every
    /// percentage rollout and split of this flag (ADR 034 R2.3).
    pub async fn reshuffle(
        &self,
        project_id: i32,
        key: &str,
    ) -> Result<feature_flags::Model, FlagError> {
        let existing = self.get(project_id, key).await?.flag;
        let mut active: feature_flags::ActiveModel = existing.into();
        active.salt = Set(Self::generate_salt());
        let model = active.update(self.db.as_ref()).await?;
        self.delivery.invalidate_project(project_id);

        info!(project_id, flag_key = %key, "Reshuffled feature flag rollout salt");

        Ok(model)
    }

    /// Evaluate one flag as an app in `environment_id` would, for the console's
    /// "test this flag" panel. Reads the database directly: it is an operator
    /// action, not a hot path, and must reflect a write made a moment ago.
    pub async fn evaluate_for_console(
        &self,
        project_id: i32,
        key: &str,
        environment_id: i32,
        context: &EvalContext,
    ) -> Result<Evaluation, FlagError> {
        let entry = self.get(project_id, key).await?;
        self.ensure_environment_in_project(project_id, environment_id)
            .await?;
        if entry.flag.archived_at.is_some() {
            return Ok(crate::eval::not_found(entry.flag.default_value.clone()));
        }
        match collapse_to_environment(entry.flag, entry.environments, environment_id) {
            Some(snapshot) => Ok(evaluate(&snapshot, context)),
            None => Err(FlagError::InvalidValueType {
                key: key.to_string(),
                value_type: "<unrecognised>".to_string(),
            }),
        }
    }

    /// Every live flag of one environment, ready to evaluate, from memory.
    ///
    /// The read path of `/api/_temps/flags` and OFREP. See
    /// [`super::delivery`] for the freshness and load bounds.
    pub async fn environment_flags(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Arc<EnvironmentFlags>, FlagError> {
        let entry = match self.delivery.lookup(environment_id) {
            Lookup::Hit {
                entry,
                refresh: false,
            } => entry,
            Lookup::Hit {
                entry,
                refresh: true,
            } => {
                // Released on every exit, including this request being
                // cancelled mid-load, so the entry never stays claimed. The
                // old entry is either replaced, or still cached because an
                // invalidation raced the load; then it must be refreshable.
                let _claim = RefreshClaim::new(entry.clone());
                // Refreshes share the load budget with misses. With no permit
                // free, serve what we have; a later request refreshes it.
                match self.delivery.load_permit().try_acquire() {
                    Err(_) => entry,
                    Ok(_permit) => {
                        let generation = self.delivery.generation();
                        match self.load_environment(project_id, environment_id).await {
                            Ok(fresh) => self.delivery.insert(fresh, generation),
                            Err(error) => {
                                // Keep serving what we had; let a later request retry.
                                warn!(
                                    project_id,
                                    environment_id,
                                    %error,
                                    "Failed to refresh cached feature flags; serving the previous set"
                                );
                                entry
                            }
                        }
                    }
                }
            }
            Lookup::Miss => {
                let _permit = self.delivery.load_permit().acquire().await.map_err(|_| {
                    FlagError::Validation {
                        key: "<delivery>".to_string(),
                        message: "flag delivery is shutting down".to_string(),
                    }
                })?;
                // Another request may have loaded it while this one waited.
                // A peek, not a lookup: it must not claim a refresh this
                // branch would never perform.
                match self.delivery.peek(environment_id) {
                    Some(entry) => entry,
                    None => {
                        let generation = self.delivery.generation();
                        let fresh = self.load_environment(project_id, environment_id).await?;
                        self.delivery.insert(fresh, generation)
                    }
                }
            }
        };

        // An environment id is cached once, whichever caller loaded it; never
        // hand one project another project's flags.
        if entry.project_id != project_id {
            return Err(FlagError::EnvironmentNotInProject {
                project_id,
                environment_id,
            });
        }
        Ok(entry)
    }

    async fn load_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<EnvironmentFlags, FlagError> {
        self.ensure_environment_in_project(project_id, environment_id)
            .await?;
        let flags = self.live_flags(project_id, environment_id, false).await?;
        let delivered = flags
            .into_iter()
            .filter_map(|(flag, overrides)| {
                let client_visible = flag.client_visible;
                let exposure_sample_rate = flag.exposure_sample_rate;
                collapse_to_environment(flag, overrides, environment_id).map(|snapshot| {
                    DeliveredFlag {
                        patterns: CompiledPatterns::for_flag(&snapshot),
                        snapshot,
                        client_visible,
                        exposure_sample_rate,
                    }
                })
            })
            .collect();
        Ok(EnvironmentFlags::new(project_id, environment_id, delivered))
    }

    // ---------------------------------------------------------------------
    // Exposure
    // ---------------------------------------------------------------------

    /// Record that a running app actually evaluated these flag keys.
    ///
    /// Returns how many keys were *accepted*, which is deliberately not the
    /// number of rows updated: reporting `rows_affected` would let a caller
    /// probe `{"keys":["candidate"]}` and read the result as "this flag
    /// exists". Accepted-count says nothing about what is stored.
    ///
    /// Writes only `last_evaluated_at`. It never touches a flag's value, which
    /// is what keeps "a deployment token cannot change what a flag serves"
    /// true even though this is a write path.
    pub async fn record_exposure(
        &self,
        project_id: i32,
        keys: &[String],
    ) -> Result<u64, FlagError> {
        if keys.is_empty() {
            return Ok(0);
        }

        // Reject rather than truncate. Silently dropping the tail would leave
        // flags that ARE being evaluated stamped `NULL` forever, and the whole
        // point of the column is answering "is it safe to delete this?".
        if keys.len() > MAX_EXPOSURE_KEYS {
            return Err(FlagError::Validation {
                key: "<exposure>".to_string(),
                message: format!(
                    "reported {} keys, maximum is {MAX_EXPOSURE_KEYS} per batch — send several batches",
                    keys.len()
                ),
            });
        }

        // A key longer than the column can hold cannot match anything, so
        // filter rather than round-trip it into the query.
        let candidates: Vec<&String> = keys.iter().filter(|k| k.len() <= MAX_KEY_LEN).collect();
        if candidates.is_empty() {
            return Ok(0);
        }

        // Only stamp rows whose timestamp is actually stale. Without this,
        // every instance rewrites up to MAX_EXPOSURE_KEYS rows on every flush,
        // forever — WAL churn, index bloat and autovacuum pressure on a table
        // shared by every tenant, to store a timestamp nobody reads at second
        // granularity. In steady state this collapses to zero writes.
        let stale_before = chrono::Utc::now() - chrono::Duration::seconds(EXPOSURE_STALENESS_SECS);

        let result = feature_flags::Entity::update_many()
            .col_expr(
                feature_flags::Column::LastEvaluatedAt,
                Expr::value(chrono::Utc::now()),
            )
            .filter(feature_flags::Column::ProjectId.eq(project_id))
            .filter(feature_flags::Column::Key.is_in(candidates.iter().map(|k| k.as_str())))
            .filter(
                feature_flags::Column::LastEvaluatedAt
                    .is_null()
                    .or(feature_flags::Column::LastEvaluatedAt.lt(stale_before)),
            )
            .exec(self.db.as_ref())
            .await?;

        debug!(
            project_id,
            accepted = candidates.len(),
            stamped = result.rows_affected,
            "Recorded feature flag exposure"
        );

        Ok(candidates.len() as u64)
    }

    // ---------------------------------------------------------------------
    // Delivery
    // ---------------------------------------------------------------------

    /// Every flag for one environment, already collapsed to what the evaluator
    /// needs. This is what the SDK caches in memory.
    ///
    /// Archived flags are excluded so they resolve to `FLAG_NOT_FOUND` in the
    /// SDK and the caller's own fallback wins.
    pub async fn snapshot(
        &self,
        project_id: i32,
        environment_id: i32,
        client_visible_only: bool,
    ) -> Result<Vec<FlagSnapshot>, FlagError> {
        self.ensure_environment_in_project(project_id, environment_id)
            .await?;

        let snapshots = self
            .live_flags(project_id, environment_id, client_visible_only)
            .await?
            .into_iter()
            .filter_map(|(flag, overrides)| {
                collapse_to_environment(flag, overrides, environment_id)
            })
            .collect();

        Ok(snapshots)
    }

    /// Unarchived flags of a project with their override row for one
    /// environment, sorted by key.
    ///
    /// Both queries run in one read-only REPEATABLE READ transaction: read
    /// separately, a variant added and a rule serving it saved in between
    /// could pair the new rule with the old variant list, and every matching
    /// subject would get `ERROR` until the next load.
    async fn live_flags(
        &self,
        project_id: i32,
        environment_id: i32,
        client_visible_only: bool,
    ) -> Result<Vec<(feature_flags::Model, Vec<feature_flag_environments::Model>)>, FlagError> {
        let txn = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await?;
        let flags =
            Self::live_flags_in(&txn, project_id, environment_id, client_visible_only).await?;
        txn.commit().await?;
        Ok(flags)
    }

    async fn live_flags_in<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
        environment_id: i32,
        client_visible_only: bool,
    ) -> Result<Vec<(feature_flags::Model, Vec<feature_flag_environments::Model>)>, FlagError> {
        let mut query = feature_flags::Entity::find()
            .filter(feature_flags::Column::ProjectId.eq(project_id))
            .filter(feature_flags::Column::ArchivedAt.is_null());

        if client_visible_only {
            query = query.filter(feature_flags::Column::ClientVisible.eq(true));
        }

        // Two queries, not one JOIN — the same shape `list()` uses, and for a
        // related reason.
        //
        // `find_with_related` emits a LEFT JOIN, and a `.filter()` on it lands
        // in the WHERE clause, evaluated *after* the join. A flag whose only
        // override belongs to a different environment then produces a joined
        // row satisfying neither `environment_id = $env` nor `id IS NULL`, so
        // the flag is dropped from the result entirely — it disappears from
        // this environment's snapshot instead of appearing with its default.
        // The SDK reads that as FLAG_NOT_FOUND and serves the caller's own
        // fallback, which for a kill switch is exactly the wrong value.
        // Fetching the overrides separately makes that failure impossible.
        let flags = query
            .order_by_asc(feature_flags::Column::Key)
            .all(conn)
            .await?;

        if flags.is_empty() {
            return Ok(Vec::new());
        }

        let flag_ids: Vec<i32> = flags.iter().map(|flag| flag.id).collect();
        let overrides = feature_flag_environments::Entity::find()
            .filter(feature_flag_environments::Column::FlagId.is_in(flag_ids))
            .filter(feature_flag_environments::Column::EnvironmentId.eq(environment_id))
            .all(conn)
            .await?;

        let mut by_flag: HashMap<i32, Vec<feature_flag_environments::Model>> = HashMap::new();
        for row in overrides {
            by_flag.entry(row.flag_id).or_default().push(row);
        }

        Ok(flags
            .into_iter()
            .map(|flag| {
                let overrides = by_flag.remove(&flag.id).unwrap_or_default();
                (flag, overrides)
            })
            .collect())
    }
}

/// Normalize caller-supplied pagination into `(page, page_size)`.
///
/// Shared with the handler on purpose: if the handler re-derived the clamp to
/// compute `total_pages`, a request for `page_size=1000` would be served 100
/// rows while being told the pages were 1000 wide.
pub fn normalize_pagination(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    (page, page_size)
}

/// Collapse one flag plus all of its override rows down to what the evaluator
/// needs for a single environment.
///
/// Free function rather than a method so it can be exercised directly: this is
/// where "no override row means live-and-inheriting" and "an unparsable stored
/// type is dropped" are decided, and both are easy to get wrong.
///
/// Returns `None` for a flag whose stored `value_type` no longer parses — data
/// corruption. Dropping it makes the SDK report `FLAG_NOT_FOUND` so the
/// caller's own fallback wins, which beats serving a wrongly-typed value.
fn collapse_to_environment(
    flag: feature_flags::Model,
    overrides: Vec<feature_flag_environments::Model>,
    environment_id: i32,
) -> Option<FlagSnapshot> {
    let Some(value_type) = FlagValueType::parse(&flag.value_type) else {
        debug!(
            flag_key = %flag.key,
            value_type = %flag.value_type,
            "Skipping feature flag with unrecognised value_type"
        );
        return None;
    };

    let this_environment = overrides
        .into_iter()
        .find(|row| row.environment_id == environment_id);

    let (enabled, environment_value, rules) = match this_environment {
        Some(row) => {
            let rules = parse_rules(&row).unwrap_or_else(|| {
                warn!(
                    flag_key = %flag.key,
                    environment_id,
                    "Stored feature flag rules do not decode; the flag will serve its default"
                );
                unreadable_rules()
            });
            (row.enabled, row.value, rules)
        }
        // No override row: the flag is live, untargeted, and inherits its
        // default.
        None => (true, None, Vec::new()),
    };

    let variants = parse_variants(&flag);
    Some(FlagSnapshot {
        key: flag.key,
        value_type,
        default_value: flag.default_value,
        enabled,
        environment_value,
        salt: flag.salt,
        variants,
        rules,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use sea_orm::{DatabaseBackend, MockDatabase};

    // =====================================================================
    // Fixtures
    // =====================================================================

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    fn flag_model(id: i32, project_id: i32, key: &str, value_type: &str) -> feature_flags::Model {
        feature_flags::Model {
            id,
            project_id,
            key: key.to_string(),
            value_type: value_type.to_string(),
            default_value: serde_json::json!(false),
            description: None,
            salt: "a1b2c3".to_string(),
            client_visible: false,
            archived_at: None,
            last_evaluated_at: None,
            variants: serde_json::json!([]),
            exposure_sample_rate: 0.0,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn override_model(
        flag_id: i32,
        environment_id: i32,
        enabled: bool,
        value: Option<serde_json::Value>,
    ) -> feature_flag_environments::Model {
        feature_flag_environments::Model {
            id: 1,
            flag_id,
            environment_id,
            enabled,
            value,
            rules: serde_json::json!([]),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn environment_model(id: i32, project_id: i32) -> environments::Model {
        environments::Model {
            id,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "prod".to_string(),
            last_deployment: None,
            host: "example.test".to_string(),
            upstreams: Default::default(),
            created_at: now(),
            updated_at: now(),
            project_id,
            current_deployment_id: None,
            branch: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            shield_mode: None,
            shield_exempt_paths: Default::default(),
            last_activity_at: None,
            force_https: None,
            ingress_mode: Default::default(),
            egress_recording: false,
            workload_mode: Default::default(),
        }
    }

    /// The row `live_project_exists` selects (only the id column).
    fn project_id_row(id: i32) -> std::collections::BTreeMap<String, sea_orm::Value> {
        std::collections::BTreeMap::from([("id".to_string(), sea_orm::Value::from(id))])
    }

    fn no_project() -> Vec<std::collections::BTreeMap<String, sea_orm::Value>> {
        Vec::new()
    }

    #[tokio::test]
    async fn listing_a_missing_project_is_project_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([no_project()]);
        assert!(matches!(
            service_with(db).list(999_999, false, None, None).await,
            Err(FlagError::ProjectNotFound {
                project_id: 999_999
            })
        ));
    }

    #[tokio::test]
    async fn creating_under_a_missing_project_is_project_not_found_before_inserting() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([no_project()]);
        let result = service_with(db)
            .create(
                999_999,
                CreateFlag {
                    key: "new-checkout".to_string(),
                    value_type: FlagValueType::Bool,
                    default_value: serde_json::json!(false),
                    description: None,
                    client_visible: false,
                    variants: Vec::new(),
                    exposure_sample_rate: 0.0,
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(FlagError::ProjectNotFound {
                project_id: 999_999
            })
        ));
    }

    fn service_with(db: MockDatabase) -> FlagService {
        FlagService::new(Arc::new(db.into_connection()))
    }

    // =====================================================================
    // Tenant isolation
    //
    // These are the highest-value tests in the crate: without them, the only
    // thing standing between project A and project B's environments is a
    // comparison nobody is watching.
    // =====================================================================

    #[tokio::test]
    async fn set_environment_value_rejects_an_environment_from_another_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get(): the flag, then its (empty) override rows.
            .append_query_results([vec![flag_model(1, 1, "checkout.v2", "bool")]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()])
            // The environment lookup returns an environment owned by project 2.
            .append_query_results([vec![environment_model(9, 2)]]);

        let result = service_with(db)
            .set_environment_value(1, "checkout.v2", 9, SetEnvironmentValue::default())
            .await;

        assert!(
            matches!(
                result,
                Err(FlagError::EnvironmentNotInProject {
                    project_id: 1,
                    environment_id: 9
                })
            ),
            "a flag must not be attachable to another project's environment"
        );
    }

    #[tokio::test]
    async fn snapshot_rejects_an_environment_from_another_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![environment_model(9, 2)]]);

        let result = service_with(db).snapshot(1, 9, false).await;

        assert!(
            matches!(
                result,
                Err(FlagError::EnvironmentNotInProject {
                    project_id: 1,
                    environment_id: 9
                })
            ),
            "a token for project 1 must not read project 2's environment"
        );
    }

    #[tokio::test]
    async fn snapshot_rejects_a_missing_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<environments::Model>::new()]);

        assert!(matches!(
            service_with(db).snapshot(1, 404, false).await,
            Err(FlagError::EnvironmentNotFound {
                environment_id: 404
            })
        ));
    }

    #[tokio::test]
    async fn set_environment_value_rejects_a_missing_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![flag_model(1, 1, "checkout.v2", "bool")]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()])
            .append_query_results([Vec::<environments::Model>::new()]);

        assert!(matches!(
            service_with(db)
                .set_environment_value(1, "checkout.v2", 404, SetEnvironmentValue::default())
                .await,
            Err(FlagError::EnvironmentNotFound {
                environment_id: 404
            })
        ));
    }

    // =====================================================================
    // CRUD error paths
    // =====================================================================

    #[tokio::test]
    async fn get_returns_not_found_with_the_key_that_was_searched_for() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<feature_flags::Model>::new()]);

        let Err(FlagError::NotFound { project_id, key }) =
            service_with(db).get(7, "missing.flag").await
        else {
            panic!("expected NotFound");
        };
        assert_eq!(project_id, 7);
        assert_eq!(key, "missing.flag");
    }

    #[tokio::test]
    async fn create_rejects_a_duplicate_key() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project_id_row(1)]])
            // The existence probe finds a flag already using this key.
            .append_query_results([vec![flag_model(1, 1, "checkout.v2", "bool")]]);

        let result = service_with(db)
            .create(
                1,
                CreateFlag {
                    key: "checkout.v2".to_string(),
                    value_type: FlagValueType::Bool,
                    default_value: serde_json::json!(false),
                    description: None,
                    client_visible: false,
                    variants: Vec::new(),
                    exposure_sample_rate: 0.0,
                },
            )
            .await;

        assert!(matches!(result, Err(FlagError::DuplicateKey { .. })));
    }

    /// Validation must run before the database is touched, so a bad key is a
    /// 400 rather than an opaque insert failure.
    #[tokio::test]
    async fn create_rejects_an_invalid_key_without_querying() {
        let db = MockDatabase::new(DatabaseBackend::Postgres);

        let result = service_with(db)
            .create(
                1,
                CreateFlag {
                    key: "Checkout/V2".to_string(),
                    value_type: FlagValueType::Bool,
                    default_value: serde_json::json!(false),
                    description: None,
                    client_visible: false,
                    variants: Vec::new(),
                    exposure_sample_rate: 0.0,
                },
            )
            .await;

        assert!(matches!(result, Err(FlagError::InvalidKey { .. })));
    }

    #[tokio::test]
    async fn create_rejects_a_default_that_does_not_match_the_declared_type() {
        let db = MockDatabase::new(DatabaseBackend::Postgres);

        let result = service_with(db)
            .create(
                1,
                CreateFlag {
                    key: "checkout.v2".to_string(),
                    value_type: FlagValueType::Bool,
                    default_value: serde_json::json!("yes"),
                    description: None,
                    client_visible: false,
                    variants: Vec::new(),
                    exposure_sample_rate: 0.0,
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(FlagError::ValueTypeMismatch { field, .. }) if field == "default_value"
        ));
    }

    // =====================================================================
    // Exposure
    // =====================================================================

    /// An empty report must not issue a query at all — apps that evaluated
    /// nothing this interval still call in.
    #[tokio::test]
    async fn empty_exposure_report_is_a_no_op() {
        let db = MockDatabase::new(DatabaseBackend::Postgres);

        assert_eq!(service_with(db).record_exposure(1, &[]).await.unwrap(), 0);
    }

    /// Reporting a key that does not exist must succeed quietly AND must not
    /// reveal that it was unknown — `recorded` is the accepted count, so it
    /// cannot be used to probe which keys exist.
    #[tokio::test]
    async fn unknown_keys_are_accepted_without_revealing_they_are_unknown() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([
            sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            },
        ]);

        let recorded = service_with(db)
            .record_exposure(1, &["nope.not.a.flag".to_string()])
            .await
            .unwrap();

        assert_eq!(
            recorded, 1,
            "accepted count, not rows_affected — otherwise this is an oracle"
        );
    }

    #[tokio::test]
    async fn exposure_returns_the_accepted_count() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([
            sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            },
        ]);

        let recorded = service_with(db)
            .record_exposure(1, &["a".to_string(), "b".to_string(), "c".to_string()])
            .await
            .unwrap();

        assert_eq!(recorded, 3, "all three were accepted regardless of matches");
    }

    /// Over-cap batches are rejected, not truncated. Silent truncation would
    /// leave flags that ARE evaluated stamped NULL forever.
    #[tokio::test]
    async fn oversized_exposure_batch_is_rejected_not_truncated() {
        let db = MockDatabase::new(DatabaseBackend::Postgres);
        let keys: Vec<String> = (0..MAX_EXPOSURE_KEYS + 1)
            .map(|i| format!("k{i}"))
            .collect();

        assert!(matches!(
            service_with(db).record_exposure(1, &keys).await,
            Err(FlagError::Validation { .. })
        ));
    }

    /// A key longer than the column cannot match anything, so it is dropped
    /// before it reaches the query rather than bloating the IN list.
    #[tokio::test]
    async fn over_long_keys_never_reach_the_query() {
        let db = MockDatabase::new(DatabaseBackend::Postgres);

        let recorded = service_with(db)
            .record_exposure(1, &["x".repeat(MAX_KEY_LEN + 1)])
            .await
            .unwrap();

        assert_eq!(recorded, 0, "no query issued, nothing accepted");
    }

    // =====================================================================
    // Snapshot collapsing
    // =====================================================================

    #[test]
    fn collapse_inherits_the_default_when_no_override_row_exists() {
        let snapshot =
            collapse_to_environment(flag_model(1, 1, "checkout.v2", "bool"), vec![], 1).unwrap();

        assert!(snapshot.enabled, "a flag with no override row is live");
        assert_eq!(snapshot.environment_value, None);
    }

    #[test]
    fn collapse_uses_only_the_requested_environments_override() {
        let snapshot = collapse_to_environment(
            flag_model(1, 1, "checkout.v2", "bool"),
            vec![
                override_model(1, 2, true, Some(serde_json::json!(true))),
                override_model(1, 3, false, Some(serde_json::json!(false))),
            ],
            2,
        )
        .unwrap();

        assert!(snapshot.enabled);
        assert_eq!(snapshot.environment_value, Some(serde_json::json!(true)));
    }

    /// The kill switch must survive into the snapshot with the override intact,
    /// so re-enabling restores the previous value rather than losing it.
    #[test]
    fn collapse_preserves_the_override_while_disabled() {
        let snapshot = collapse_to_environment(
            flag_model(1, 1, "checkout.v2", "bool"),
            vec![override_model(1, 2, false, Some(serde_json::json!(true)))],
            2,
        )
        .unwrap();

        assert!(!snapshot.enabled);
        assert_eq!(
            snapshot.environment_value,
            Some(serde_json::json!(true)),
            "disabling must not discard the stored value"
        );
    }

    #[test]
    fn collapse_drops_a_flag_with_an_unparsable_stored_type() {
        assert!(
            collapse_to_environment(flag_model(1, 1, "broken", "wat"), vec![], 1).is_none(),
            "corrupt rows must be dropped so the caller's fallback wins"
        );
    }

    // =====================================================================
    // Targeting writes (ADR 110 D3)
    // =====================================================================

    fn string_flag_with_variants(id: i32) -> feature_flags::Model {
        let mut flag = flag_model(id, 1, "checkout.v2", "string");
        flag.default_value = serde_json::json!("off");
        flag.variants = serde_json::json!([
            {"key": "control", "value": "old"},
            {"key": "test", "value": "new"}
        ]);
        flag
    }

    fn split_rules() -> Vec<FlagRule> {
        serde_json::from_value(serde_json::json!([
            {"serve": {"kind": "split", "split": [
                {"variant": "control", "weight": 50},
                {"variant": "test", "weight": 50}
            ]}}
        ]))
        .unwrap()
    }

    #[tokio::test]
    async fn set_rules_validates_against_the_flags_variants_before_writing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()])
            .append_query_results([vec![environment_model(2, 1)]]);

        let rules: Vec<FlagRule> = serde_json::from_value(serde_json::json!([
            {"serve": {"kind": "variant", "variant": "ghost"}}
        ]))
        .unwrap();
        let result = service_with(db)
            .set_rules(
                1,
                "checkout.v2",
                2,
                SetRules {
                    rules,
                    expected_updated_at: None,
                },
            )
            .await;

        assert!(
            matches!(&result, Err(FlagError::Validation { message, .. }) if message.contains("ghost")),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn set_rules_refuses_a_stale_revision() {
        let mut row = override_model(1, 2, true, None);
        row.updated_at = chrono::DateTime::parse_from_rfc3339("2026-11-19T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([vec![row.clone()]])
            .append_query_results([vec![environment_model(2, 1)]])
            // The locked re-reads inside the transaction: flag, then row.
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([vec![row]]);

        let seen = chrono::DateTime::parse_from_rfc3339("2026-11-19T09:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let result = service_with(db)
            .set_rules(
                1,
                "checkout.v2",
                2,
                SetRules {
                    rules: split_rules(),
                    expected_updated_at: Some(seen),
                },
            )
            .await;

        assert!(
            matches!(
                result,
                Err(FlagError::StaleWrite {
                    project_id: 1,
                    environment_id: Some(2),
                    ..
                })
            ),
            "{result:?}"
        );
    }

    /// The caller saw no row, but one exists now: someone else wrote first.
    #[tokio::test]
    async fn set_rules_refuses_a_first_write_when_a_row_appeared() {
        let row = override_model(1, 2, true, None);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()])
            .append_query_results([vec![environment_model(2, 1)]])
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([vec![row]]);

        let result = service_with(db)
            .set_rules(
                1,
                "checkout.v2",
                2,
                SetRules {
                    rules: split_rules(),
                    expected_updated_at: None,
                },
            )
            .await;

        assert!(
            matches!(
                result,
                Err(FlagError::StaleWrite {
                    environment_id: Some(2),
                    ..
                })
            ),
            "{result:?}"
        );
    }

    /// The caller saw a row that has since been deleted.
    #[tokio::test]
    async fn set_rules_treats_an_expected_row_that_vanished_as_stale() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()])
            .append_query_results([vec![environment_model(2, 1)]])
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()]);

        let result = service_with(db)
            .set_rules(
                1,
                "checkout.v2",
                2,
                SetRules {
                    rules: split_rules(),
                    expected_updated_at: Some(now()),
                },
            )
            .await;

        assert!(
            matches!(result, Err(FlagError::StaleWrite { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn set_rules_rejects_another_projects_environment() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()])
            .append_query_results([vec![environment_model(9, 2)]]);

        let result = service_with(db)
            .set_rules(1, "checkout.v2", 9, SetRules::default())
            .await;

        assert!(matches!(
            result,
            Err(FlagError::EnvironmentNotInProject {
                project_id: 1,
                environment_id: 9
            })
        ));
    }

    #[tokio::test]
    async fn removing_a_variant_a_rule_still_serves_is_refused() {
        let mut row = override_model(1, 2, true, None);
        row.rules = serde_json::to_value(split_rules()).unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![string_flag_with_variants(1)]])
            .append_query_results([vec![row]]);

        let result = service_with(db)
            .update(
                1,
                "checkout.v2",
                UpdateFlag {
                    variants: Some(vec![FlagVariant {
                        key: "control".into(),
                        value: serde_json::json!("old"),
                        description: None,
                    }]),
                    ..Default::default()
                },
            )
            .await;

        assert!(
            matches!(&result, Err(FlagError::Validation { message, .. })
                if message.contains("'test'") && message.contains("environment 2")),
            "{result:?}"
        );
    }

    /// The before-image is the row read under the write lock, not the earlier
    /// unlocked read, so the audit records the transition actually made.
    #[tokio::test]
    async fn update_returns_the_row_it_locked_as_the_before_image() {
        let read = flag_model(1, 1, "checkout.v2", "bool");
        let mut locked = read.clone();
        locked.client_visible = true;
        let mut written = locked.clone();
        written.default_value = serde_json::json!(true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![read]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()])
            .append_query_results([vec![locked.clone()]])
            .append_query_results([vec![written.clone()]]);

        let updated = service_with(db)
            .update(
                1,
                "checkout.v2",
                UpdateFlag {
                    default_value: Some(serde_json::json!(true)),
                    ..Default::default()
                },
            )
            .await
            .expect("an unconditional update succeeds");

        assert_eq!(updated.before, locked);
        assert_eq!(updated.flag, written);
    }

    #[tokio::test]
    async fn update_refuses_a_stale_revision() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![flag_model(1, 1, "checkout.v2", "bool")]])
            .append_query_results([Vec::<feature_flag_environments::Model>::new()]);

        let result = service_with(db)
            .update(
                1,
                "checkout.v2",
                UpdateFlag {
                    client_visible: Some(true),
                    expected_updated_at: Some(now() - chrono::Duration::hours(1)),
                    ..Default::default()
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(FlagError::StaleWrite {
                environment_id: None,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn create_rejects_an_out_of_range_exposure_rate_without_querying() {
        let result = service_with(MockDatabase::new(DatabaseBackend::Postgres))
            .create(
                1,
                CreateFlag {
                    key: "checkout.v2".to_string(),
                    value_type: FlagValueType::Bool,
                    default_value: serde_json::json!(false),
                    description: None,
                    client_visible: true,
                    variants: Vec::new(),
                    exposure_sample_rate: 2.0,
                },
            )
            .await;
        assert!(matches!(result, Err(FlagError::Validation { .. })));
    }

    // =====================================================================
    // Delivery cache
    // =====================================================================

    /// The hot-path guarantee: once an environment is loaded, evaluating it
    /// again costs no query. The mock has results for exactly one load, so a
    /// second query would fail the test.
    #[tokio::test]
    async fn a_cached_environment_is_served_without_a_query() {
        let mut visible = flag_model(1, 1, "public.flag", "bool");
        visible.client_visible = true;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![environment_model(2, 1)]])
            .append_query_results([vec![visible]])
            .append_query_results([vec![override_model(
                1,
                2,
                true,
                Some(serde_json::json!(true)),
            )]]);
        let service = service_with(db);

        let first = service.environment_flags(1, 2).await.unwrap();
        let second = service.environment_flags(1, 2).await.unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        let flag = first.get("public.flag").expect("loaded");
        assert!(flag.client_visible);
        assert_eq!(
            flag.snapshot.environment_value,
            Some(serde_json::json!(true))
        );
    }

    #[tokio::test]
    async fn a_cached_environment_is_never_served_to_another_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![environment_model(2, 1)]])
            .append_query_results([Vec::<feature_flags::Model>::new()]);
        let service = service_with(db);

        service.environment_flags(1, 2).await.unwrap();
        assert!(matches!(
            service.environment_flags(7, 2).await,
            Err(FlagError::EnvironmentNotInProject {
                project_id: 7,
                environment_id: 2
            })
        ));
    }

    #[test]
    fn collapse_carries_salt_variants_and_rules() {
        let mut row = override_model(1, 2, true, None);
        row.rules = serde_json::to_value(split_rules()).unwrap();
        let snapshot = collapse_to_environment(string_flag_with_variants(1), vec![row], 2).unwrap();

        assert_eq!(snapshot.salt, "a1b2c3");
        assert_eq!(snapshot.variants.len(), 2);
        assert_eq!(snapshot.rules, split_rules());
    }

    /// Rules that do not decode must not be silently dropped: the flag
    /// reports ERROR instead of quietly ignoring configured targeting.
    #[test]
    fn undecodable_rules_make_the_flag_report_an_error() {
        let mut row = override_model(1, 2, true, Some(serde_json::json!(true)));
        row.rules = serde_json::json!({"not": "a list"});
        let snapshot =
            collapse_to_environment(flag_model(1, 1, "checkout.v2", "bool"), vec![row], 2).unwrap();

        let result = evaluate(&snapshot, &EvalContext::default());
        assert_eq!(result.reason, crate::eval::EvalReason::Error);
        assert_eq!(result.value, serde_json::json!(false));
    }

    // =====================================================================
    // Pagination
    // =====================================================================

    #[test]
    fn pagination_defaults_to_the_first_page_of_twenty() {
        assert_eq!(normalize_pagination(None, None), (1, DEFAULT_PAGE_SIZE));
    }

    #[test]
    fn pagination_caps_page_size_and_floors_page() {
        assert_eq!(normalize_pagination(Some(3), Some(50)), (3, 50));
        assert_eq!(
            normalize_pagination(Some(1), Some(10_000)),
            (1, MAX_PAGE_SIZE),
            "an unbounded page_size must not be honoured"
        );
        assert_eq!(normalize_pagination(Some(0), Some(0)), (1, 1));
    }

    #[test]
    fn accepts_conventional_keys() {
        assert!(FlagService::validate_key("checkout.v2").is_ok());
        assert!(FlagService::validate_key("new-search").is_ok());
        assert!(FlagService::validate_key("worker_batch_size").is_ok());
        assert!(FlagService::validate_key("a").is_ok());
        assert!(FlagService::validate_key("2fa.enabled").is_ok());
    }

    #[test]
    fn rejects_empty_key() {
        assert!(matches!(
            FlagService::validate_key(""),
            Err(FlagError::InvalidKey { .. })
        ));
    }

    #[test]
    fn rejects_uppercase_and_spaces() {
        assert!(FlagService::validate_key("Checkout").is_err());
        assert!(FlagService::validate_key("checkout v2").is_err());
        assert!(FlagService::validate_key("checkout/v2").is_err());
    }

    #[test]
    fn rejects_leading_punctuation() {
        assert!(FlagService::validate_key(".checkout").is_err());
        assert!(FlagService::validate_key("-checkout").is_err());
        assert!(FlagService::validate_key("_checkout").is_err());
    }

    #[test]
    fn rejects_over_length_key() {
        let key = "a".repeat(MAX_KEY_LEN + 1);
        assert!(matches!(
            FlagService::validate_key(&key),
            Err(FlagError::InvalidKey { .. })
        ));
        assert!(FlagService::validate_key(&"a".repeat(MAX_KEY_LEN)).is_ok());
    }

    #[test]
    fn key_error_names_the_offending_character() {
        let Err(FlagError::InvalidKey { reason, .. }) = FlagService::validate_key("checkout/v2")
        else {
            panic!("expected InvalidKey");
        };
        assert!(
            reason.contains('/'),
            "reason did not name the character: {reason}"
        );
    }

    #[test]
    fn value_must_match_declared_type() {
        assert!(FlagService::validate_value(
            "f",
            FlagValueType::Bool,
            &serde_json::json!(true),
            "default_value"
        )
        .is_ok());

        assert!(matches!(
            FlagService::validate_value(
                "f",
                FlagValueType::Bool,
                &serde_json::json!("true"),
                "default_value"
            ),
            Err(FlagError::ValueTypeMismatch { .. })
        ));
    }

    #[test]
    fn rejects_over_length_description() {
        let description = Some("x".repeat(MAX_DESCRIPTION_LEN + 1));
        assert!(matches!(
            FlagService::validate_description("f", &description),
            Err(FlagError::Validation { .. })
        ));
    }

    #[test]
    fn salts_are_unique_per_flag() {
        let a = FlagService::generate_salt();
        let b = FlagService::generate_salt();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }
}
