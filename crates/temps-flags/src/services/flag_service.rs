//! Business logic for feature flags.

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use temps_entities::{environments, feature_flag_environments, feature_flags};
use tracing::{debug, info};

use crate::error::FlagError;
use crate::eval::{FlagSnapshot, FlagValueType};

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

/// Fields accepted when creating a flag.
#[derive(Debug, Clone)]
pub struct CreateFlag {
    pub key: String,
    pub value_type: FlagValueType,
    pub default_value: serde_json::Value,
    pub description: Option<String>,
    pub client_visible: bool,
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
}

impl FlagService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
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

        let mut query =
            feature_flags::Entity::find().filter(feature_flags::Column::ProjectId.eq(project_id));

        if !include_archived {
            query = query.filter(feature_flags::Column::ArchivedAt.is_null());
        }

        let paginator = query
            .order_by_asc(feature_flags::Column::Key)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let flags = paginator.fetch_page(page - 1).await?;

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
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

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
    ) -> Result<feature_flags::Model, FlagError> {
        let existing = self.get(project_id, key).await?.flag;

        self.apply_update(key, existing, request).await
    }

    /// The update itself, given a flag the caller has already fetched.
    async fn apply_update(
        &self,
        key: &str,
        existing: feature_flags::Model,
        request: UpdateFlag,
    ) -> Result<feature_flags::Model, FlagError> {
        let project_id = existing.project_id;

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

        let model = active.update(self.db.as_ref()).await?;

        info!(project_id, flag_key = %key, "Updated feature flag");

        Ok(model)
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
                    // Phase 1 never writes rules. The column exists so targeting
                    // can land without a migration or a resolution-order change.
                    rules: Set(serde_json::json!([])),
                    ..Default::default()
                }
                .insert(&txn)
                .await?
            }
        };

        txn.commit().await?;

        info!(
            project_id,
            flag_key = %key,
            environment_id,
            enabled = model.enabled,
            "Set feature flag environment value"
        );

        Ok(model)
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
            .all(self.db.as_ref())
            .await?;

        if flags.is_empty() {
            return Ok(Vec::new());
        }

        let flag_ids: Vec<i32> = flags.iter().map(|flag| flag.id).collect();
        let overrides = feature_flag_environments::Entity::find()
            .filter(feature_flag_environments::Column::FlagId.is_in(flag_ids))
            .filter(feature_flag_environments::Column::EnvironmentId.eq(environment_id))
            .all(self.db.as_ref())
            .await?;

        let mut by_flag: HashMap<i32, Vec<feature_flag_environments::Model>> = HashMap::new();
        for row in overrides {
            by_flag.entry(row.flag_id).or_default().push(row);
        }

        let snapshots = flags
            .into_iter()
            .filter_map(|flag| {
                let overrides = by_flag.remove(&flag.id).unwrap_or_default();
                collapse_to_environment(flag, overrides, environment_id)
            })
            .collect();

        Ok(snapshots)
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

    let (enabled, environment_value) = match this_environment {
        Some(row) => (row.enabled, row.value),
        // No override row: the flag is live and inherits its default.
        None => (true, None),
    };

    Some(FlagSnapshot {
        key: flag.key,
        value_type,
        default_value: flag.default_value,
        enabled,
        environment_value,
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
            last_activity_at: None,
            force_https: None,
        }
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
