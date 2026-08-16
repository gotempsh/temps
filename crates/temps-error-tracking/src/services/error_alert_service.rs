use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult,
    PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::{error_alert_fires, error_alert_rules, error_events, error_groups};
use tracing::{error, info, warn};

use super::types::ErrorTrackingError;

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

/// Configuration for frequency-based triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrequencyConfig {
    pub count: i64,
    pub window_minutes: i64,
}

/// Configuration for user count triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCountConfig {
    pub count: i64,
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

    // === CRUD operations for alert rules ===

    pub async fn list_rules(
        &self,
        project_id: i32,
    ) -> Result<Vec<error_alert_rules::Model>, ErrorTrackingError> {
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
            .ok_or(ErrorTrackingError::Validation(format!(
                "Alert rule {} not found in project {}",
                rule_id, project_id
            )))
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
        // Validate trigger type
        if AlertTriggerType::from_str(&trigger_type).is_none() {
            return Err(ErrorTrackingError::Validation(format!(
                "Invalid trigger type '{}'. Valid types: new_issue, regression, frequency, new_user, user_count, status_change",
                trigger_type
            )));
        }

        // Validate priority
        let valid_priorities = ["Low", "Normal", "High", "Critical"];
        if !valid_priorities.contains(&notification_priority.as_str()) {
            return Err(ErrorTrackingError::Validation(format!(
                "Invalid notification priority '{}'. Valid: Low, Normal, High, Critical",
                notification_priority
            )));
        }

        if cooldown_minutes < 0 {
            return Err(ErrorTrackingError::Validation(
                "Cooldown minutes must be non-negative".to_string(),
            ));
        }

        let now = Utc::now();
        let rule = error_alert_rules::ActiveModel {
            project_id: Set(project_id),
            name: Set(name),
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

        let result = rule.insert(self.db.as_ref()).await?;
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
        let existing = self.get_rule(rule_id, project_id).await?;
        let mut rule: error_alert_rules::ActiveModel = existing.into();

        if let Some(name) = name {
            rule.name = Set(name);
        }
        if let Some(trigger_type) = trigger_type {
            if AlertTriggerType::from_str(&trigger_type).is_none() {
                return Err(ErrorTrackingError::Validation(format!(
                    "Invalid trigger type '{}'",
                    trigger_type
                )));
            }
            rule.trigger_type = Set(trigger_type);
        }
        if let Some(trigger_config) = trigger_config {
            rule.trigger_config = Set(trigger_config);
        }
        if let Some(environment_filter) = environment_filter {
            rule.environment_filter = Set(environment_filter);
        }
        if let Some(error_level_filter) = error_level_filter {
            rule.error_level_filter = Set(error_level_filter);
        }
        if let Some(priority) = notification_priority {
            let valid_priorities = ["Low", "Normal", "High", "Critical"];
            if !valid_priorities.contains(&priority.as_str()) {
                return Err(ErrorTrackingError::Validation(format!(
                    "Invalid notification priority '{}'",
                    priority
                )));
            }
            rule.notification_priority = Set(priority);
        }
        if let Some(cooldown) = cooldown_minutes {
            if cooldown < 0 {
                return Err(ErrorTrackingError::Validation(
                    "Cooldown minutes must be non-negative".to_string(),
                ));
            }
            rule.cooldown_minutes = Set(cooldown);
        }
        if let Some(enabled) = enabled {
            rule.enabled = Set(enabled);
        }

        rule.updated_at = Set(Utc::now());
        let result = rule.update(self.db.as_ref()).await?;
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
            return Err(ErrorTrackingError::Validation(format!(
                "Alert rule {} not found in project {}",
                rule_id, project_id
            )));
        }

        Ok(())
    }

    /// Create default alert rules for a project
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

        let window_start = Utc::now() - Duration::minutes(config.window_minutes);

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

    #[test]
    fn test_status_change_config_deserialization() {
        let json = serde_json::json!({"statuses": ["resolved", "assigned"]});
        let config: StatusChangeConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.statuses, vec!["resolved", "assigned"]);
    }
}
