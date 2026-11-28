// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::{
    environments, error_alert_fires, error_alert_rules, error_events, error_groups,
};
use tracing::{error, info, warn};

use super::types::ErrorTrackingError;

/// Error alert rules one project may hold (#1210). Rules are evaluated for
/// every ingested error, so an unbounded rule set is an unbounded per-event
/// cost an API caller controls.
pub const MAX_ERROR_ALERT_RULES_PER_PROJECT: u64 = 100;

/// Status-change notifications one bulk triage request may send. A bulk
/// resolve can select up to 1,000 groups; delivering one notification per
/// group per rule would turn a single click into an unbounded fan-out.
pub const BULK_STATUS_CHANGE_NOTIFICATION_LIMIT: usize = 50;

/// Trigger types that can fire an error alert
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AlertTriggerType {
    /// First event in a new error group
    NewIssue,
    /// Resolved/ignored issue gets a new event
    Regression,
    /// Error count exceeds threshold in time window
    Frequency,
    /// First event with a user context in this group
    NewUser,
    /// N unique users affected by this error group
    UserCount,
    /// Issue status changes (resolved, assigned, ignored)
    StatusChange,
}

impl AlertTriggerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NewIssue => "new_issue",
            Self::Regression => "regression",
            Self::Frequency => "frequency",
            Self::NewUser => "new_user",
            Self::UserCount => "user_count",
            Self::StatusChange => "status_change",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "new_issue" => Some(Self::NewIssue),
            "regression" => Some(Self::Regression),
            "frequency" => Some(Self::Frequency),
            "new_user" => Some(Self::NewUser),
            "user_count" => Some(Self::UserCount),
            "status_change" => Some(Self::StatusChange),
            _ => None,
        }
    }
}

/// Longest alert rule name, in characters.
pub const MAX_ALERT_RULE_NAME_CHARS: usize = 255;

/// Longest error type a rule can filter on, in characters.
pub const MAX_ERROR_TYPE_FILTER_CHARS: usize = 255;

/// Notification priorities a rule may use.
const VALID_PRIORITIES: [&str; 4] = ["Low", "Normal", "High", "Critical"];

fn invalid_rule(field: &'static str, reason: impl Into<String>) -> ErrorTrackingError {
    ErrorTrackingError::InvalidAlertRule {
        field,
        reason: reason.into(),
    }
}

/// A rule name, trimmed: required and at most [`MAX_ALERT_RULE_NAME_CHARS`].
fn normalize_rule_name(name: &str) -> Result<String, ErrorTrackingError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid_rule("name", "must not be empty"));
    }
    let chars = name.chars().count();
    if chars > MAX_ALERT_RULE_NAME_CHARS {
        return Err(invalid_rule(
            "name",
            format!("must be at most {MAX_ALERT_RULE_NAME_CHARS} characters, got {chars}"),
        ));
    }
    Ok(name.to_string())
}

/// The error type a rule filters on (`error_level_filter`), trimmed. An empty
/// one means "no filter": stored as an empty string it matched no error, so
/// a rule cleared that way never fired again.
fn normalize_error_type_filter(
    filter: Option<String>,
) -> Result<Option<String>, ErrorTrackingError> {
    let Some(filter) = filter else {
        return Ok(None);
    };
    let filter = filter.trim();
    if filter.is_empty() {
        return Ok(None);
    }
    let chars = filter.chars().count();
    if chars > MAX_ERROR_TYPE_FILTER_CHARS {
        return Err(invalid_rule(
            "error_level_filter",
            format!("must be at most {MAX_ERROR_TYPE_FILTER_CHARS} characters, got {chars}"),
        ));
    }
    Ok(Some(filter.to_string()))
}

fn validate_priority(priority: &str) -> Result<(), ErrorTrackingError> {
    if VALID_PRIORITIES.contains(&priority) {
        return Ok(());
    }
    Err(invalid_rule(
        "notification_priority",
        format!("'{priority}' is not one of {}", VALID_PRIORITIES.join(", ")),
    ))
}

fn validate_cooldown(cooldown_minutes: i32) -> Result<(), ErrorTrackingError> {
    if cooldown_minutes < 0 {
        return Err(invalid_rule(
            "cooldown_minutes",
            format!("must be zero or more, got {cooldown_minutes}"),
        ));
    }
    Ok(())
}

fn validate_trigger_type(trigger_type: &str) -> Result<(), ErrorTrackingError> {
    if AlertTriggerType::from_str(trigger_type).is_some() {
        return Ok(());
    }
    Err(invalid_rule(
        "trigger_type",
        format!(
            "'{trigger_type}' is not one of new_issue, regression, frequency, new_user, \
             user_count, status_change"
        ),
    ))
}

/// Keys a trigger type reads from its config. A key outside these is a typo
/// or a field meant for another type, and silently has no effect.
fn trigger_config_keys(trigger_type: &str) -> &'static [&'static str] {
    match AlertTriggerType::from_str(trigger_type) {
        Some(AlertTriggerType::Frequency) => &["count", "window_minutes"],
        // `threshold` is what consoles before #1749 saved.
        Some(AlertTriggerType::UserCount) => &["count", "threshold"],
        Some(AlertTriggerType::StatusChange) => &["statuses"],
        _ => &[],
    }
}

/// Refuses a supplied trigger config that is not an object or names a key
/// its trigger type does not read.
fn check_trigger_config_keys(
    trigger_type: &str,
    trigger_config: &serde_json::Value,
) -> Result<(), ErrorTrackingError> {
    let Some(object) = trigger_config.as_object() else {
        return Err(invalid_rule("trigger_config", "must be a JSON object"));
    };
    let allowed = trigger_config_keys(trigger_type);
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    let accepted = if allowed.is_empty() {
        "it takes no settings ({})".to_string()
    } else {
        format!("it accepts {}", allowed.join(", "))
    };
    Err(invalid_rule(
        "trigger_config",
        format!(
            "unknown key(s) {} for trigger type '{trigger_type}'; {accepted}",
            unknown.join(", ")
        ),
    ))
}

/// Longest window a frequency rule can count over (30 days).
pub const MAX_FREQUENCY_WINDOW_MINUTES: i64 = 30 * 24 * 60;

/// Refuse a trigger config that could never evaluate. A window that overflows
/// date arithmetic used to panic ingest for every later event in a group, so
/// one saved rule lost events until someone found and deleted it.
fn validate_trigger_config(
    trigger_type: &str,
    trigger_config: &serde_json::Value,
) -> Result<(), ErrorTrackingError> {
    match AlertTriggerType::from_str(trigger_type) {
        Some(AlertTriggerType::Frequency) => {}
        Some(AlertTriggerType::UserCount) => return validate_user_count_config(trigger_config),
        _ => return Ok(()),
    }
    let config: FrequencyConfig =
        serde_json::from_value(trigger_config.clone()).map_err(|error| {
            invalid_rule(
                "trigger_config",
                format!("'{trigger_type}' expects count and window_minutes: {error}"),
            )
        })?;
    if config.count < 1 {
        return Err(invalid_rule(
            "trigger_config",
            format!(
                "frequency rule count must be at least 1, got {}",
                config.count
            ),
        ));
    }
    if !(1..=MAX_FREQUENCY_WINDOW_MINUTES).contains(&config.window_minutes) {
        return Err(invalid_rule(
            "trigger_config",
            format!(
                "frequency rule window_minutes must be between 1 and \
                 {MAX_FREQUENCY_WINDOW_MINUTES}, got {}",
                config.window_minutes
            ),
        ));
    }
    Ok(())
}

/// A user count rule without a count of at least 1 could never fire, and
/// nothing would say so.
fn validate_user_count_config(
    trigger_config: &serde_json::Value,
) -> Result<(), ErrorTrackingError> {
    let config: UserCountConfig =
        serde_json::from_value(trigger_config.clone()).map_err(|error| {
            invalid_rule(
                "trigger_config",
                format!("'user_count' expects a user threshold in count: {error}"),
            )
        })?;
    if config.count < 1 {
        // The console calls this value the threshold; older clients sent it
        // as `threshold`.
        return Err(invalid_rule(
            "trigger_config",
            format!(
                "user count rule threshold (count) must be at least 1, got {}",
                config.count
            ),
        ));
    }
    Ok(())
}

/// Start of a frequency rule's window, or `None` when `window_minutes` can't be
/// represented. Saves refuse such a value, but a rule stored before that check
/// existed still reaches here on the ingest path, which must not panic.
fn frequency_window_start(
    now: chrono::DateTime<Utc>,
    window_minutes: i64,
) -> Option<chrono::DateTime<Utc>> {
    now.checked_sub_signed(Duration::try_minutes(window_minutes)?)
}

/// Configuration for frequency-based triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrequencyConfig {
    pub count: i64,
    pub window_minutes: i64,
}

/// Configuration for user count triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "StoredUserCountConfig")]
pub struct UserCountConfig {
    pub count: i64,
}

/// A user count config as stored. Consoles before #1749 saved the value as
/// `threshold`, which the server never read, and could also carry a `count`
/// left over from a frequency rule. `count` wins when both are present (as the
/// console reads it back); `threshold` alone is read as the count. A serde
/// alias would refuse the both-present shape as a duplicate field, silencing
/// rules that worked before.
#[derive(Deserialize)]
struct StoredUserCountConfig {
    count: Option<i64>,
    threshold: Option<i64>,
}

impl TryFrom<StoredUserCountConfig> for UserCountConfig {
    type Error = &'static str;

    fn try_from(stored: StoredUserCountConfig) -> Result<Self, Self::Error> {
        stored
            .count
            .or(stored.threshold)
            .map(|count| Self { count })
            .ok_or("missing field `count`")
    }
}

/// Configuration for status change triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusChangeConfig {
    /// Which statuses to trigger on (e.g., ["resolved", "assigned"])
    pub statuses: Vec<String>,
}

/// Result of alert evaluation — carries rich context for notification rendering
#[derive(Debug, Clone)]
pub struct AlertNotification {
    pub rule_name: String,
    pub trigger_type: String,
    pub group_title: String,
    pub group_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub priority: String,
    pub message: String,
    // Enriched fields (set by ErrorTrackingService after evaluation)
    pub project_name: Option<String>,
    pub project_slug: Option<String>,
    pub environment_name: Option<String>,
    pub error_type: String,
    pub total_count: i32,
    pub first_seen: String,
    pub last_seen: String,
}

/// Service for evaluating error alert rules and firing notifications
pub struct ErrorAlertService {
    db: Arc<DatabaseConnection>,
}

impl ErrorAlertService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Evaluate rules after a new error group is created.
    /// Checks: new_issue, new_user rules.
    pub async fn evaluate_new_group(
        &self,
        group: &error_groups::Model,
        has_user_context: bool,
    ) -> Vec<AlertNotification> {
        let mut notifications = Vec::new();

        let rules = match self.get_enabled_rules(group.project_id).await {
            Ok(rules) => rules,
            Err(e) => {
                error!(
                    "Failed to load alert rules for project {}: {}",
                    group.project_id, e
                );
                return notifications;
            }
        };

        for rule in &rules {
            if !self.matches_filters(rule, group) {
                continue;
            }

            let trigger_type = match AlertTriggerType::from_str(&rule.trigger_type) {
                Some(t) => t,
                None => continue,
            };

            let should_fire = match trigger_type {
                AlertTriggerType::NewIssue => true,
                AlertTriggerType::NewUser if has_user_context => true,
                _ => false,
            };

            if should_fire {
                if let Some(notification) = self.try_fire_rule(rule, group, &trigger_type).await {
                    notifications.push(notification);
                }
            }
        }

        notifications
    }

    /// Evaluate rules after an event is added to an existing group.
    /// Checks: regression, frequency, new_user, user_count rules.
    /// Uses pre_ingestion_status for regression detection (the status before the new event was added).
    pub async fn evaluate_event_added_with_status(
        &self,
        group: &error_groups::Model,
        has_user_context: bool,
        pre_ingestion_status: &str,
    ) -> Vec<AlertNotification> {
        let mut notifications = Vec::new();

        let rules = match self.get_enabled_rules(group.project_id).await {
            Ok(rules) => rules,
            Err(e) => {
                error!(
                    "Failed to load alert rules for project {}: {}",
                    group.project_id, e
                );
                return notifications;
            }
        };

        for rule in &rules {
            if !self.matches_filters(rule, group) {
                continue;
            }

            let trigger_type = match AlertTriggerType::from_str(&rule.trigger_type) {
                Some(t) => t,
                None => continue,
            };

            let should_fire = match &trigger_type {
                AlertTriggerType::Regression => {
                    // Use the pre-ingestion status to detect regression correctly
                    pre_ingestion_status == "resolved" || pre_ingestion_status == "ignored"
                }
                AlertTriggerType::Frequency => self.check_frequency_trigger(rule, group).await,
                AlertTriggerType::NewUser if has_user_context => {
                    self.check_new_user_trigger(group).await
                }
                AlertTriggerType::UserCount => self.check_user_count_trigger(rule, group).await,
                _ => false,
            };

            if should_fire {
                if let Some(notification) = self.try_fire_rule(rule, group, &trigger_type).await {
                    notifications.push(notification);
                }
            }
        }

        notifications
    }

    /// Evaluate rules after an error group status change.
    /// Checks: status_change rules.
    pub async fn evaluate_status_change(
        &self,
        group: &error_groups::Model,
        new_status: &str,
    ) -> Vec<AlertNotification> {
        let mut notifications = Vec::new();

        let rules = match self.get_enabled_rules(group.project_id).await {
            Ok(rules) => rules,
            Err(e) => {
                error!(
                    "Failed to load alert rules for project {}: {}",
                    group.project_id, e
                );
                return notifications;
            }
        };

        for rule in &rules {
            if !self.matches_filters(rule, group) {
                continue;
            }

            let trigger_type = match AlertTriggerType::from_str(&rule.trigger_type) {
                Some(t) => t,
                None => continue,
            };

            if trigger_type != AlertTriggerType::StatusChange {
                continue;
            }

            let should_fire =
                match serde_json::from_value::<StatusChangeConfig>(rule.trigger_config.clone()) {
                    Ok(config) => config.statuses.contains(&new_status.to_string()),
                    Err(_) => true, // No config means trigger on any status change
                };

            if should_fire {
                if let Some(notification) = self.try_fire_rule(rule, group, &trigger_type).await {
                    notifications.push(notification);
                }
            }
        }

        notifications
    }

    /// Evaluate `status_change` rules for many groups changed by one bulk
    /// triage request (ADR 111, D1).
    ///
    /// Same rules, filters and cooldown as [`Self::evaluate_status_change`],
    /// but the work is bounded by the number of rules rather than by the
    /// number of groups: one cooldown query and one fire insert per rule, not
    /// per group. Notifications are capped at
    /// [`BULK_STATUS_CHANGE_NOTIFICATION_LIMIT`], because one triage click
    /// must not fan out into a thousand deliveries; a fire is recorded only
    /// for a group that is actually notified, so the rest stay eligible for
    /// the next request.
    pub async fn evaluate_status_change_many(
        &self,
        project_id: i32,
        groups: &[error_groups::Model],
        new_status: &str,
    ) -> Vec<AlertNotification> {
        if groups.is_empty() {
            return Vec::new();
        }
        let rules = match self.get_enabled_rules(project_id).await {
            Ok(rules) => rules,
            Err(e) => {
                error!("Failed to load alert rules for project {project_id}: {e}");
                return Vec::new();
            }
        };
        let mut notifications = Vec::new();
        let mut budget = BULK_STATUS_CHANGE_NOTIFICATION_LIMIT;
        let mut suppressed = 0_usize;
        for rule in &rules {
            if AlertTriggerType::from_str(&rule.trigger_type)
                != Some(AlertTriggerType::StatusChange)
            {
                continue;
            }
            let fires_on_this_status =
                match serde_json::from_value::<StatusChangeConfig>(rule.trigger_config.clone()) {
                    Ok(config) => config.statuses.iter().any(|s| s == new_status),
                    // No config means "any status change", as for one group.
                    Err(_) => true,
                };
            if !fires_on_this_status {
                continue;
            }
            let candidates: Vec<&error_groups::Model> = groups
                .iter()
                .filter(|group| self.matches_filters(rule, group))
                .collect();
            if candidates.is_empty() {
                continue;
            }
            let cooling_down = self
                .groups_in_cooldown(rule, &candidates.iter().map(|g| g.id).collect::<Vec<_>>())
                .await;
            let eligible: Vec<&error_groups::Model> = candidates
                .into_iter()
                .filter(|group| !cooling_down.contains(&group.id))
                .collect();
            let (notify, over_budget) = eligible.split_at(eligible.len().min(budget));
            suppressed += over_budget.len();
            if notify.is_empty() {
                continue;
            }
            budget -= notify.len();
            self.record_fires(rule.id, &notify.iter().map(|g| g.id).collect::<Vec<_>>())
                .await;
            for group in notify {
                info!(
                    "Alert rule '{}' (type: status_change) fired for error group {} in project {}",
                    rule.name, group.id, group.project_id
                );
                notifications.push(AlertNotification {
                    rule_name: rule.name.clone(),
                    trigger_type: AlertTriggerType::StatusChange.as_str().to_string(),
                    group_title: group.title.clone(),
                    group_id: group.id,
                    project_id: group.project_id,
                    environment_id: group.environment_id,
                    priority: rule.notification_priority.clone(),
                    message: format!("Error group status changed to '{new_status}'."),
                    project_name: None,
                    project_slug: None,
                    environment_name: None,
                    error_type: group.error_type.clone(),
                    total_count: group.total_count,
                    first_seen: group.first_seen.to_rfc3339(),
                    last_seen: group.last_seen.to_rfc3339(),
                });
            }
        }
        if suppressed > 0 {
            warn!(
                project_id,
                suppressed,
                "Bulk status change produced more alert notifications than the per-request limit of {}; the rest were not sent and not recorded as fired",
                BULK_STATUS_CHANGE_NOTIFICATION_LIMIT
            );
        }
        notifications
    }

    /// Which of `group_ids` have fired `rule` inside its cooldown window.
    /// One query for the whole set.
    async fn groups_in_cooldown(
        &self,
        rule: &error_alert_rules::Model,
        group_ids: &[i32],
    ) -> std::collections::HashSet<i32> {
        let threshold = Utc::now() - Duration::minutes(i64::from(rule.cooldown_minutes));
        match error_alert_fires::Entity::find()
            .filter(error_alert_fires::Column::RuleId.eq(rule.id))
            .filter(error_alert_fires::Column::ErrorGroupId.is_in(group_ids.to_vec()))
            .filter(error_alert_fires::Column::FiredAt.gte(threshold))
            .all(self.db.as_ref())
            .await
        {
            Ok(fires) => fires.into_iter().map(|fire| fire.error_group_id).collect(),
            Err(e) => {
                error!(
                    "Failed to read alert fires for rule {}: {}; treating every group as cooling down",
                    rule.id, e
                );
                group_ids.iter().copied().collect()
            }
        }
    }

    /// Record one fire per group in a single insert.
    async fn record_fires(&self, rule_id: i32, group_ids: &[i32]) {
        if group_ids.is_empty() {
            return;
        }
        let now = Utc::now();
        let fires = group_ids
            .iter()
            .map(|group_id| error_alert_fires::ActiveModel {
                rule_id: Set(rule_id),
                error_group_id: Set(*group_id),
                fired_at: Set(now),
                notification_sent: Set(true),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        if let Err(e) = error_alert_fires::Entity::insert_many(fires)
            .exec(self.db.as_ref())
            .await
        {
            error!(
                "Failed to record {} alert fires for rule {}: {}",
                group_ids.len(),
                rule_id,
                e
            );
        }
    }

    // === CRUD operations for alert rules ===

    pub async fn list_rules(
        &self,
        project_id: i32,
    ) -> Result<Vec<error_alert_rules::Model>, ErrorTrackingError> {
        // A missing project is a 404, not an empty list of rules.
        if !temps_entities::projects::live_project_exists(self.db.as_ref(), project_id).await? {
            return Err(ErrorTrackingError::ProjectNotFound { project_id });
        }
        let rules = error_alert_rules::Entity::find()
            .filter(error_alert_rules::Column::ProjectId.eq(project_id))
            .order_by_desc(error_alert_rules::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;
        Ok(rules)
    }

    pub async fn get_rule(
        &self,
        rule_id: i32,
        project_id: i32,
    ) -> Result<error_alert_rules::Model, ErrorTrackingError> {
        error_alert_rules::Entity::find_by_id(rule_id)
            .filter(error_alert_rules::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(ErrorTrackingError::AlertRuleNotFound {
                project_id,
                rule_id,
            })
    }

    /// Refuses an environment filter naming an environment the project does
    /// not have (another project's, a deleted one, or none at all).
    async fn ensure_environment_in_project<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(), ErrorTrackingError> {
        let found = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .count(conn)
            .await?;
        if found == 0 {
            return Err(ErrorTrackingError::AlertRuleEnvironmentNotFound {
                project_id,
                environment_id,
            });
        }
        Ok(())
    }

    /// Refuses a name another rule of the project already has (compared
    /// case-insensitively). A project holds at most
    /// [`MAX_ERROR_ALERT_RULES_PER_PROJECT`] rules, so this reads a bounded set.
    async fn ensure_name_free<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
        name: &str,
        except_rule_id: Option<i32>,
    ) -> Result<(), ErrorTrackingError> {
        let names: Vec<(i32, String)> = error_alert_rules::Entity::find()
            .select_only()
            .column(error_alert_rules::Column::Id)
            .column(error_alert_rules::Column::Name)
            .filter(error_alert_rules::Column::ProjectId.eq(project_id))
            .into_tuple()
            .all(conn)
            .await?;
        let wanted = name.to_lowercase();
        if let Some((rule_id, existing)) = names
            .into_iter()
            .find(|(id, existing)| Some(*id) != except_rule_id && existing.to_lowercase() == wanted)
        {
            return Err(ErrorTrackingError::AlertRuleNameTaken {
                project_id,
                name: existing,
                rule_id,
            });
        }
        Ok(())
    }

    /// Locks the project row so concurrent writes to one project's rules
    /// serialize (limit and name checks), or reports it missing.
    async fn lock_project<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
    ) -> Result<(), ErrorTrackingError> {
        temps_entities::projects::Entity::find_by_id(project_id)
            .filter(temps_entities::projects::Column::DeletedAt.is_null())
            .lock_exclusive()
            .one(conn)
            .await?
            .map(|_| ())
            .ok_or(ErrorTrackingError::ProjectNotFound { project_id })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_rule(
        &self,
        project_id: i32,
        name: String,
        trigger_type: String,
        trigger_config: serde_json::Value,
        environment_filter: Option<i32>,
        error_level_filter: Option<String>,
        notification_priority: String,
        cooldown_minutes: i32,
        enabled: bool,
    ) -> Result<error_alert_rules::Model, ErrorTrackingError> {
        let name = normalize_rule_name(&name)?;
        validate_trigger_type(&trigger_type)?;
        validate_priority(&notification_priority)?;
        validate_cooldown(cooldown_minutes)?;
        check_trigger_config_keys(&trigger_type, &trigger_config)?;
        validate_trigger_config(&trigger_type, &trigger_config)?;
        let error_level_filter = normalize_error_type_filter(error_level_filter)?;

        let now = Utc::now();
        let rule = error_alert_rules::ActiveModel {
            project_id: Set(project_id),
            name: Set(name.clone()),
            trigger_type: Set(trigger_type),
            trigger_config: Set(trigger_config),
            environment_filter: Set(environment_filter),
            error_level_filter: Set(error_level_filter),
            notification_priority: Set(notification_priority),
            cooldown_minutes: Set(cooldown_minutes),
            enabled: Set(enabled),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        // Count and insert under a lock on the project row so concurrent
        // creations for one project serialize and cannot overshoot the limit.
        // Dropping the transaction on an early return rolls it back.
        let txn = self.db.begin().await?;
        Self::lock_project(&txn, project_id).await?;
        if let Some(environment_id) = environment_filter {
            Self::ensure_environment_in_project(&txn, project_id, environment_id).await?;
        }
        Self::ensure_name_free(&txn, project_id, &name, None).await?;
        let existing = error_alert_rules::Entity::find()
            .filter(error_alert_rules::Column::ProjectId.eq(project_id))
            .count(&txn)
            .await?;
        if existing >= MAX_ERROR_ALERT_RULES_PER_PROJECT {
            txn.rollback().await?;
            return Err(ErrorTrackingError::AlertRuleLimitReached {
                project_id,
                existing,
                limit: MAX_ERROR_ALERT_RULES_PER_PROJECT,
            });
        }
        let result = rule.insert(&txn).await?;
        txn.commit().await?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_rule(
        &self,
        rule_id: i32,
        project_id: i32,
        name: Option<String>,
        trigger_type: Option<String>,
        trigger_config: Option<serde_json::Value>,
        environment_filter: Option<Option<i32>>,
        error_level_filter: Option<Option<String>>,
        notification_priority: Option<String>,
        cooldown_minutes: Option<i32>,
        enabled: Option<bool>,
    ) -> Result<error_alert_rules::Model, ErrorTrackingError> {
        let name = name.as_deref().map(normalize_rule_name).transpose()?;
        if let Some(trigger_type) = trigger_type.as_deref() {
            validate_trigger_type(trigger_type)?;
        }
        if let Some(priority) = notification_priority.as_deref() {
            validate_priority(priority)?;
        }
        if let Some(cooldown) = cooldown_minutes {
            validate_cooldown(cooldown)?;
        }
        let error_level_filter = error_level_filter
            .map(normalize_error_type_filter)
            .transpose()?;

        // Dropping the transaction on an early return rolls it back.
        let txn = self.db.begin().await?;
        Self::lock_project(&txn, project_id).await?;
        let existing = error_alert_rules::Entity::find_by_id(rule_id)
            .filter(error_alert_rules::Column::ProjectId.eq(project_id))
            .one(&txn)
            .await?
            .ok_or(ErrorTrackingError::AlertRuleNotFound {
                project_id,
                rule_id,
            })?;
        // Judged on what the rule will hold after this update, so changing only
        // the type or only the config can't leave an invalid pair behind.
        let next_type = trigger_type.as_deref().unwrap_or(&existing.trigger_type);
        if let Some(config) = trigger_config.as_ref() {
            check_trigger_config_keys(next_type, config)?;
        }
        if trigger_type.is_some() || trigger_config.is_some() {
            validate_trigger_config(
                next_type,
                trigger_config.as_ref().unwrap_or(&existing.trigger_config),
            )?;
        }
        // Only an environment the update changes the filter to is checked, so
        // a rule whose environment was deleted can still be edited (#2111).
        if let Some(Some(environment_id)) = environment_filter {
            if existing.environment_filter != Some(environment_id) {
                Self::ensure_environment_in_project(&txn, project_id, environment_id).await?;
            }
        }
        if let Some(name) = name.as_deref() {
            Self::ensure_name_free(&txn, project_id, name, Some(rule_id)).await?;
        }
        let mut rule: error_alert_rules::ActiveModel = existing.into();

        if let Some(name) = name {
            rule.name = Set(name);
        }
        if let Some(trigger_type) = trigger_type {
            rule.trigger_type = Set(trigger_type);
        }
        if let Some(trigger_config) = trigger_config {
            rule.trigger_config = Set(trigger_config);
        }
        // `Some(None)` (an explicit null) clears a filter; absent keeps it.
        if let Some(environment_filter) = environment_filter {
            rule.environment_filter = Set(environment_filter);
        }
        if let Some(error_level_filter) = error_level_filter {
            rule.error_level_filter = Set(error_level_filter);
        }
        if let Some(priority) = notification_priority {
            rule.notification_priority = Set(priority);
        }
        if let Some(cooldown) = cooldown_minutes {
            rule.cooldown_minutes = Set(cooldown);
        }
        if let Some(enabled) = enabled {
            rule.enabled = Set(enabled);
        }

        rule.updated_at = Set(Utc::now());
        let result = rule.update(&txn).await?;
        txn.commit().await?;
        Ok(result)
    }

    pub async fn delete_rule(
        &self,
        rule_id: i32,
        project_id: i32,
    ) -> Result<(), ErrorTrackingError> {
        let result = error_alert_rules::Entity::delete_many()
            .filter(error_alert_rules::Column::Id.eq(rule_id))
            .filter(error_alert_rules::Column::ProjectId.eq(project_id))
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(ErrorTrackingError::AlertRuleNotFound {
                project_id,
                rule_id,
            });
        }

        Ok(())
    }

    /// Creates the two rules every new project starts with: "New issue
    /// detected" (`new_issue`) and "Regression detected" (`regression`), both
    /// High priority with a 30-minute cooldown. They are ordinary rules that
    /// can be changed or deleted.
    pub async fn create_default_rules(
        &self,
        project_id: i32,
    ) -> Result<Vec<error_alert_rules::Model>, ErrorTrackingError> {
        let mut rules = Vec::new();

        // Default rule: Notify on new issue
        rules.push(
            self.create_rule(
                project_id,
                "New issue detected".to_string(),
                "new_issue".to_string(),
                serde_json::json!({}),
                None,
                None,
                "High".to_string(),
                30,
                true,
            )
            .await?,
        );

        // Default rule: Notify on regression
        rules.push(
            self.create_rule(
                project_id,
                "Regression detected".to_string(),
                "regression".to_string(),
                serde_json::json!({}),
                None,
                None,
                "High".to_string(),
                30,
                true,
            )
            .await?,
        );

        Ok(rules)
    }

    // === Internal helpers ===

    async fn get_enabled_rules(
        &self,
        project_id: i32,
    ) -> Result<Vec<error_alert_rules::Model>, ErrorTrackingError> {
        let rules = error_alert_rules::Entity::find()
            .filter(error_alert_rules::Column::ProjectId.eq(project_id))
            .filter(error_alert_rules::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;
        tracing::debug!(
            "Found {} enabled alert rule(s) for project {}",
            rules.len(),
            project_id
        );
        Ok(rules)
    }

    fn matches_filters(
        &self,
        rule: &error_alert_rules::Model,
        group: &error_groups::Model,
    ) -> bool {
        // Check environment filter
        if let Some(env_filter) = rule.environment_filter {
            if group.environment_id != Some(env_filter) {
                return false;
            }
        }

        // Check error level filter
        if let Some(ref level_filter) = rule.error_level_filter {
            if group.error_type.to_lowercase() != level_filter.to_lowercase() {
                return false;
            }
        }

        true
    }

    async fn check_cooldown(&self, rule: &error_alert_rules::Model, group_id: i32) -> bool {
        let cooldown_threshold = Utc::now() - Duration::minutes(rule.cooldown_minutes as i64);

        let recent_fire = error_alert_fires::Entity::find()
            .filter(error_alert_fires::Column::RuleId.eq(rule.id))
            .filter(error_alert_fires::Column::ErrorGroupId.eq(group_id))
            .filter(error_alert_fires::Column::FiredAt.gte(cooldown_threshold))
            .one(self.db.as_ref())
            .await;

        match recent_fire {
            Ok(Some(_)) => false, // Still in cooldown
            Ok(None) => true,     // Cooldown expired, can fire
            Err(e) => {
                warn!("Failed to check cooldown for rule {}: {}", rule.id, e);
                false // Err on the side of not spamming
            }
        }
    }

    async fn record_fire(&self, rule_id: i32, group_id: i32, notification_sent: bool) {
        let fire = error_alert_fires::ActiveModel {
            rule_id: Set(rule_id),
            error_group_id: Set(group_id),
            fired_at: Set(Utc::now()),
            notification_sent: Set(notification_sent),
            ..Default::default()
        };

        if let Err(e) = fire.insert(self.db.as_ref()).await {
            error!(
                "Failed to record alert fire for rule {} group {}: {}",
                rule_id, group_id, e
            );
        }
    }

    async fn try_fire_rule(
        &self,
        rule: &error_alert_rules::Model,
        group: &error_groups::Model,
        trigger_type: &AlertTriggerType,
    ) -> Option<AlertNotification> {
        if !self.check_cooldown(rule, group.id).await {
            return None;
        }

        let message = self.build_notification_message(rule, group, trigger_type);

        self.record_fire(rule.id, group.id, true).await;

        info!(
            "Alert rule '{}' (type: {}) fired for error group {} in project {}",
            rule.name,
            trigger_type.as_str(),
            group.id,
            group.project_id
        );

        Some(AlertNotification {
            rule_name: rule.name.clone(),
            trigger_type: trigger_type.as_str().to_string(),
            group_title: group.title.clone(),
            group_id: group.id,
            project_id: group.project_id,
            environment_id: group.environment_id,
            priority: rule.notification_priority.clone(),
            message,
            // These get populated by enrich_notifications in ErrorTrackingService
            project_name: None,
            project_slug: None,
            environment_name: None,
            error_type: group.error_type.clone(),
            total_count: group.total_count,
            first_seen: group.first_seen.to_rfc3339(),
            last_seen: group.last_seen.to_rfc3339(),
        })
    }

    fn build_notification_message(
        &self,
        rule: &error_alert_rules::Model,
        group: &error_groups::Model,
        trigger_type: &AlertTriggerType,
    ) -> String {
        match trigger_type {
            AlertTriggerType::NewIssue => "A new error has been detected.".to_string(),
            AlertTriggerType::Regression => {
                "A previously resolved error has reappeared.".to_string()
            }
            AlertTriggerType::Frequency => {
                let config: FrequencyConfig = serde_json::from_value(rule.trigger_config.clone())
                    .unwrap_or(FrequencyConfig {
                        count: 0,
                        window_minutes: 0,
                    });
                format!(
                    "Error count exceeded {} events in {} minutes.",
                    config.count, config.window_minutes
                )
            }
            AlertTriggerType::NewUser => "A new user has been affected by this error.".to_string(),
            AlertTriggerType::UserCount => {
                let config: UserCountConfig = serde_json::from_value(rule.trigger_config.clone())
                    .unwrap_or(UserCountConfig { count: 0 });
                format!("Error has affected {} or more unique users.", config.count)
            }
            AlertTriggerType::StatusChange => {
                format!("Error group status changed to '{}'.", group.status)
            }
        }
    }

    /// Clean up old alert fire records beyond the retention period
    pub async fn cleanup_old_fires(&self, retention_days: i64) {
        let cutoff = Utc::now() - Duration::days(retention_days);
        let result = error_alert_fires::Entity::delete_many()
            .filter(error_alert_fires::Column::FiredAt.lt(cutoff))
            .exec(self.db.as_ref())
            .await;

        match result {
            Ok(res) => {
                if res.rows_affected > 0 {
                    info!(
                        "Cleaned up {} old alert fire records (older than {} days)",
                        res.rows_affected, retention_days
                    );
                }
            }
            Err(e) => {
                error!("Failed to clean up old alert fire records: {}", e);
            }
        }
    }

    async fn check_frequency_trigger(
        &self,
        rule: &error_alert_rules::Model,
        group: &error_groups::Model,
    ) -> bool {
        let config: FrequencyConfig = match serde_json::from_value(rule.trigger_config.clone()) {
            Ok(c) => c,
            Err(_) => return false,
        };

        let Some(window_start) = frequency_window_start(Utc::now(), config.window_minutes) else {
            warn!(
                rule_id = rule.id,
                project_id = rule.project_id,
                window_minutes = config.window_minutes,
                "Skipping frequency alert rule: window_minutes is out of range"
            );
            return false;
        };

        let count = error_events::Entity::find()
            .filter(error_events::Column::ErrorGroupId.eq(group.id))
            .filter(error_events::Column::CreatedAt.gte(window_start))
            .count(self.db.as_ref())
            .await
            .unwrap_or(0);

        count as i64 >= config.count
    }

    async fn check_new_user_trigger(&self, group: &error_groups::Model) -> bool {
        // Check if this is the first event with user context in this group
        // by counting events that have visitor_id set
        let user_event_count = error_events::Entity::find()
            .filter(error_events::Column::ErrorGroupId.eq(group.id))
            .filter(error_events::Column::VisitorId.is_not_null())
            .count(self.db.as_ref())
            .await
            .unwrap_or(0);

        // Fire only when first user event (count == 1 means we just added the first one)
        user_event_count == 1
    }

    async fn check_user_count_trigger(
        &self,
        rule: &error_alert_rules::Model,
        group: &error_groups::Model,
    ) -> bool {
        let config: UserCountConfig = match serde_json::from_value(rule.trigger_config.clone()) {
            Ok(c) => c,
            Err(_) => return false,
        };

        #[derive(Debug, FromQueryResult)]
        struct CountResult {
            count: i64,
        }

        let result: Option<CountResult> =
            sea_orm::FromQueryResult::find_by_statement(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT COUNT(DISTINCT visitor_id) as count FROM error_events WHERE error_group_id = $1 AND visitor_id IS NOT NULL",
                vec![group.id.into()],
            ))
            .one(self.db.as_ref())
            .await
            .unwrap_or(None);

        result.is_some_and(|r| r.count >= config.count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_trigger_type_roundtrip() {
        let types = vec![
            AlertTriggerType::NewIssue,
            AlertTriggerType::Regression,
            AlertTriggerType::Frequency,
            AlertTriggerType::NewUser,
            AlertTriggerType::UserCount,
            AlertTriggerType::StatusChange,
        ];

        for t in types {
            let s = t.as_str();
            let parsed = AlertTriggerType::from_str(s);
            assert!(parsed.is_some(), "Failed to parse: {}", s);
            assert_eq!(parsed.unwrap(), t);
        }
    }

    #[test]
    fn test_invalid_trigger_type() {
        assert!(AlertTriggerType::from_str("invalid").is_none());
        assert!(AlertTriggerType::from_str("").is_none());
    }

    #[test]
    fn frequency_rule_config_is_refused_outside_its_range() {
        let config = |count: i64, window_minutes: i64| serde_json::json!({"count": count, "window_minutes": window_minutes});
        assert!(validate_trigger_config("frequency", &config(1, 1)).is_ok());
        assert!(
            validate_trigger_config("frequency", &config(10, MAX_FREQUENCY_WINDOW_MINUTES)).is_ok()
        );

        for bad in [
            config(0, 60),
            config(-1, 60),
            config(10, 0),
            config(10, -1),
            config(10, MAX_FREQUENCY_WINDOW_MINUTES + 1),
            config(10, i64::MAX),
            serde_json::json!({"count": 10}),
        ] {
            assert!(
                matches!(
                    validate_trigger_config("frequency", &bad),
                    Err(ErrorTrackingError::InvalidAlertRule {
                        field: "trigger_config",
                        ..
                    })
                ),
                "{bad}"
            );
        }

        // Other trigger types don't read these fields.
        assert!(validate_trigger_config("new_issue", &config(0, 0)).is_ok());
    }

    #[test]
    fn frequency_window_for_a_rule_saved_out_of_range_does_not_panic() {
        let now = Utc::now();
        assert_eq!(
            frequency_window_start(now, 60),
            Some(now - Duration::minutes(60))
        );
        assert_eq!(frequency_window_start(now, i64::MAX), None);
        assert_eq!(frequency_window_start(now, i64::MIN), None);
    }

    #[test]
    fn test_frequency_config_deserialization() {
        let json = serde_json::json!({"count": 100, "window_minutes": 60});
        let config: FrequencyConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.count, 100);
        assert_eq!(config.window_minutes, 60);
    }

    #[test]
    fn test_user_count_config_deserialization() {
        let json = serde_json::json!({"count": 10});
        let config: UserCountConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.count, 10);
    }

    /// #1749: rules the console saved under `threshold` never fired, because
    /// the server only read `count`. They now read the same.
    #[test]
    fn user_count_rule_saved_as_threshold_reads_its_count() {
        let config: UserCountConfig =
            serde_json::from_value(serde_json::json!({"threshold": 3})).unwrap();
        assert_eq!(config.count, 3);
        assert!(
            validate_trigger_config("user_count", &serde_json::json!({"threshold": 3})).is_ok()
        );
    }

    /// A rule saved by an older console can hold both keys (a `count` left
    /// from a frequency rule plus the `threshold` it showed). It must still
    /// load, reading `count` first, rather than fail as a duplicate field.
    #[test]
    fn user_count_rule_with_both_count_and_threshold_reads_the_count() {
        let both = serde_json::json!({"count": 5, "threshold": 9, "window_minutes": 60});
        let config: UserCountConfig = serde_json::from_value(both.clone()).unwrap();
        assert_eq!(config.count, 5);
        assert!(validate_trigger_config("user_count", &both).is_ok());
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            serde_json::json!({"count": 5})
        );
    }

    /// #1749: a user count rule saved with no count never fired, silently.
    #[test]
    fn user_count_rule_config_is_refused_without_a_positive_count() {
        assert!(validate_trigger_config("user_count", &serde_json::json!({"count": 1})).is_ok());
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"count": 0}),
            serde_json::json!({"count": -2}),
            serde_json::json!({"count": "ten"}),
        ] {
            match validate_trigger_config("user_count", &bad) {
                Err(ErrorTrackingError::InvalidAlertRule {
                    field: "trigger_config",
                    reason,
                }) => {
                    assert!(
                        reason.contains("user_count") || reason.contains("user count"),
                        "{reason}"
                    );
                }
                other => panic!("{bad} must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_status_change_config_deserialization() {
        let json = serde_json::json!({"statuses": ["resolved", "assigned"]});
        let config: StatusChangeConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.statuses, vec!["resolved", "assigned"]);
    }

    async fn create_numbered_rule(
        service: &ErrorAlertService,
        project_id: i32,
        n: usize,
    ) -> Result<error_alert_rules::Model, ErrorTrackingError> {
        service
            .create_rule(
                project_id,
                format!("Rule {n}"),
                "new_issue".to_string(),
                serde_json::json!({}),
                None,
                None,
                "Normal".to_string(),
                30,
                true,
            )
            .await
    }

    /// #1210: rule creation stops at the per-project limit, counts only the
    /// project's own rules, and reports a missing project as not found.
    #[tokio::test]
    async fn create_rule_stops_at_the_per_project_limit() {
        use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping alert-rule limit test: {error}");
                return;
            }
            Err(error) => panic!("alert-rule limit test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let insert_project = |slug: &'static str| {
            let db = db.clone();
            async move {
                let now = Utc::now();
                temps_entities::projects::ActiveModel {
                    name: Set(slug.to_string()),
                    repo_name: Set("repo".to_string()),
                    repo_owner: Set("owner".to_string()),
                    directory: Set("/".to_string()),
                    main_branch: Set("main".to_string()),
                    slug: Set(format!("{slug}-{}", uuid::Uuid::new_v4())),
                    preset: Set(temps_entities::preset::Preset::NextJs),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(db.as_ref())
                .await
                .expect("insert project")
            }
        };
        let project = insert_project("limit-project").await;
        let other = insert_project("other-project").await;
        let service = ErrorAlertService::new(db.clone());

        for n in 0..MAX_ERROR_ALERT_RULES_PER_PROJECT as usize {
            create_numbered_rule(&service, project.id, n)
                .await
                .expect("rules under the limit are created");
        }
        let over = create_numbered_rule(&service, project.id, 999).await;
        assert!(
            matches!(
                over,
                Err(ErrorTrackingError::AlertRuleLimitReached {
                    project_id,
                    existing: 100,
                    limit: 100,
                }) if project_id == project.id
            ),
            "{over:?}"
        );

        // Another project's budget is independent.
        create_numbered_rule(&service, other.id, 0)
            .await
            .expect("another project is not affected");

        let missing_id = project.id + 100_000;
        let missing = create_numbered_rule(&service, missing_id, 0).await;
        assert!(
            matches!(missing, Err(ErrorTrackingError::ProjectNotFound { project_id }) if project_id == missing_id),
            "{missing:?}"
        );

        // Listing a missing project is the same 404, not an empty list.
        let listed = service.list_rules(missing_id).await;
        assert!(
            matches!(listed, Err(ErrorTrackingError::ProjectNotFound { project_id }) if project_id == missing_id),
            "{listed:?}"
        );
    }
}
