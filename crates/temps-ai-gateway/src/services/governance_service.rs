use std::sync::Arc;

use chrono::{Datelike, TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, DatabaseTransaction, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    Statement, TransactionTrait,
};

use crate::error::AiGatewayError;
use crate::handlers::pricing::estimate_cost_microcents;
use tracing::warn;

use super::AiUsageAttribution;

/// An opaque token representing an accepted governance check. The request_id
/// links the open cost reservation row; the billing_period pins the UTC month
/// that was current when the reservation was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceReservation {
    request_id: String,
    billing_period: chrono::NaiveDate,
}

impl GovernanceReservation {
    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(crate) fn billing_period(&self) -> chrono::NaiveDate {
        self.billing_period
    }
}

#[derive(Debug, FromQueryResult)]
struct CountRow {
    count: Option<i64>,
    retry_after_seconds: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct CostRow {
    cost: Option<i64>,
}

#[derive(Debug)]
struct AppliedConfig {
    scope: String,
    max_requests_per_minute: Option<i64>,
    max_cost_per_month_microcents: Option<i64>,
}

/// Enforces persisted instance/project/environment/token AI gateway policy.
///
/// Rate events and conservative cost reservations live in PostgreSQL. Advisory
/// transaction locks serialize checks for each scope, so limits remain valid
/// when multiple console processes serve requests concurrently.
pub struct GovernanceService {
    db: Arc<DatabaseConnection>,
}

impl GovernanceService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Check whether a request is allowed by all applicable governance configs.
    ///
    /// Returns a `GovernanceReservation` that must be passed to
    /// `log_usage_with_context_and_reservation` when the request succeeds, or
    /// to `release_cost_reservation` when the request fails before any tokens
    /// are consumed.
    #[allow(clippy::too_many_arguments)]
    pub async fn check_request(
        &self,
        attribution: &AiUsageAttribution,
        model: &str,
        is_byok: bool,
        projected_input_tokens: Option<i64>,
        max_output_tokens: Option<i64>,
    ) -> Result<GovernanceReservation, AiGatewayError> {
        let scope_names = applicable_scope_names(attribution);
        let rows = temps_entities::ai_gateway_config::Entity::find()
            .filter(temps_entities::ai_gateway_config::Column::Scope.is_in(scope_names.clone()))
            .all(self.db.as_ref())
            .await?;

        let mut configs = Vec::new();
        for scope in scope_names {
            let Some(config) = rows.iter().find(|row| row.scope == scope) else {
                continue;
            };

            if let Some(allowed_models) = config.allowed_models.as_ref() {
                let allowed = allowed_models
                    .as_array()
                    .is_some_and(|models| models.iter().any(|entry| entry.as_str() == Some(model)));
                if !allowed {
                    return Err(AiGatewayError::ModelNotAllowed {
                        model: model.to_string(),
                        scope: scope.clone(),
                    });
                }
            }

            validate_nonnegative(
                &scope,
                "max_requests_per_minute",
                config.max_requests_per_minute,
            )?;
            validate_nonnegative(
                &scope,
                "max_cost_per_month_microcents",
                config.max_cost_per_month_microcents,
            )?;

            configs.push(AppliedConfig {
                scope,
                max_requests_per_minute: config.max_requests_per_minute,
                max_cost_per_month_microcents: config.max_cost_per_month_microcents,
            });
        }

        let budget_scope = configs
            .iter()
            .find(|config| config.max_cost_per_month_microcents.is_some())
            .map(|config| config.scope.clone());
        let projected_cost = if is_byok || budget_scope.is_none() {
            None
        } else {
            let output_tokens =
                max_output_tokens.ok_or_else(|| AiGatewayError::BudgetRequiresMaxTokens {
                    scope: budget_scope
                        .clone()
                        .unwrap_or_else(|| "instance".to_string()),
                })?;
            let input_tokens = projected_input_tokens.ok_or_else(|| {
                AiGatewayError::BudgetProjectionUnavailable {
                    scope: budget_scope
                        .clone()
                        .unwrap_or_else(|| "instance".to_string()),
                }
            })?;
            estimate_cost_microcents(model, input_tokens, output_tokens)
                .ok_or_else(|| AiGatewayError::PricingUnavailable {
                    model: model.to_string(),
                    scope: budget_scope.unwrap_or_else(|| "instance".to_string()),
                })?
                .into()
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let billing_period = current_month_start()?.date_naive();
        let has_rate_limit = configs
            .iter()
            .any(|config| config.max_requests_per_minute.is_some());
        if configs.is_empty() || (!has_rate_limit && projected_cost.is_none()) {
            return Ok(GovernanceReservation {
                request_id,
                billing_period,
            });
        }

        // Settle stale reservations in their own transaction so a subsequent
        // budget rejection cannot roll the conservative debit back.
        let cleanup_txn = self.db.begin().await?;
        self.cleanup_expired_state(&cleanup_txn).await?;
        cleanup_txn.commit().await?;

        let txn = self.db.begin().await?;
        self.lock_scopes(&txn, &configs).await?;
        self.check_rates_and_record(&txn, &configs, &request_id)
            .await?;
        if let Some(projected_cost) = projected_cost {
            self.check_budgets_and_reserve(
                &txn,
                attribution,
                &configs,
                &request_id,
                projected_cost,
                billing_period,
            )
            .await?;
        }
        txn.commit().await?;

        Ok(GovernanceReservation {
            request_id,
            billing_period,
        })
    }

    /// Release a cost reservation on explicit upstream failure. This is a
    /// best-effort operation; errors are logged by callers but do not surface
    /// to the client.
    pub async fn release_cost_reservation(
        &self,
        reservation: &GovernanceReservation,
    ) -> Result<(), AiGatewayError> {
        // Guard: only delete if the reservation has NOT already been promoted to
        // a conservative debit by `cleanup_expired_state`. A slow request that
        // errors after cleanup ran would otherwise silently erase a durable
        // debit, defeating the point of making it durable.
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM ai_gateway_cost_reservations WHERE request_id = $1 AND is_conservative_debit = FALSE",
                [reservation.request_id.clone().into()],
            ))
            .await?;
        Ok(())
    }

    /// List all governance configs ordered by scope.
    pub async fn list_configs(
        &self,
    ) -> Result<Vec<temps_entities::ai_gateway_config::Model>, AiGatewayError> {
        Ok(temps_entities::ai_gateway_config::Entity::find()
            .order_by_asc(temps_entities::ai_gateway_config::Column::Scope)
            .all(self.db.as_ref())
            .await?)
    }

    /// Upsert a governance config for the given scope. Creates a new row or
    /// updates the limits on an existing one. Preserves provider routing
    /// columns (`provider_type`, `agent_cli_provider_id`, etc.) on update.
    pub async fn upsert_config(
        &self,
        scope: &str,
        allowed_models: Option<serde_json::Value>,
        max_requests_per_minute: Option<i64>,
        max_cost_per_month_microcents: Option<i64>,
    ) -> Result<temps_entities::ai_gateway_config::Model, AiGatewayError> {
        validate_scope(scope)?;
        validate_nonnegative(scope, "max_requests_per_minute", max_requests_per_minute)?;
        validate_nonnegative(
            scope,
            "max_cost_per_month_microcents",
            max_cost_per_month_microcents,
        )?;
        validate_allowed_models(scope, allowed_models.as_ref())?;

        let existing = temps_entities::ai_gateway_config::Entity::find()
            .filter(temps_entities::ai_gateway_config::Column::Scope.eq(scope))
            .one(self.db.as_ref())
            .await?;

        let model = match existing {
            Some(existing) => {
                let mut active: temps_entities::ai_gateway_config::ActiveModel = existing.into();
                active.allowed_models = Set(allowed_models);
                active.max_requests_per_minute = Set(max_requests_per_minute);
                active.max_cost_per_month_microcents = Set(max_cost_per_month_microcents);
                active.update(self.db.as_ref()).await?
            }
            None => {
                temps_entities::ai_gateway_config::ActiveModel {
                    scope: Set(scope.to_string()),
                    allowed_models: Set(allowed_models),
                    max_requests_per_minute: Set(max_requests_per_minute),
                    max_cost_per_month_microcents: Set(max_cost_per_month_microcents),
                    ..Default::default()
                }
                .insert(self.db.as_ref())
                .await?
            }
        };

        Ok(model)
    }

    /// Delete the governance config for the given scope. Returns
    /// `GovernanceConfigNotFound` if no row with that scope exists.
    pub async fn delete_config(&self, scope: &str) -> Result<(), AiGatewayError> {
        validate_scope(scope)?;
        let result = temps_entities::ai_gateway_config::Entity::delete_many()
            .filter(temps_entities::ai_gateway_config::Column::Scope.eq(scope))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(AiGatewayError::GovernanceConfigNotFound {
                scope: scope.to_string(),
            });
        }
        Ok(())
    }

    /// Acquire advisory locks for all scopes that have limits, ordered to
    /// prevent deadlocks when multiple service instances compete.
    async fn lock_scopes(
        &self,
        txn: &DatabaseTransaction,
        configs: &[AppliedConfig],
    ) -> Result<(), AiGatewayError> {
        let mut scopes = configs
            .iter()
            .filter(|config| {
                config.max_requests_per_minute.is_some()
                    || config.max_cost_per_month_microcents.is_some()
            })
            .map(|config| config.scope.as_str())
            .collect::<Vec<_>>();
        scopes.sort_unstable();
        for scope in scopes {
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 908245731))",
                [scope.into()],
            ))
            .await?;
        }
        Ok(())
    }

    /// Public entry point for background cleanup jobs.  Opens its own
    /// transaction so callers do not need to manage one.
    pub async fn run_cleanup(&self) -> Result<(), AiGatewayError> {
        let txn = self.db.begin().await?;
        self.cleanup_expired_state(&txn).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Remove rate events older than the 60-second RPM window (plus a small
    /// buffer) and convert expired cost reservations into durable
    /// conservative debits instead of releasing them.
    async fn cleanup_expired_state(&self, txn: &DatabaseTransaction) -> Result<(), AiGatewayError> {
        let month_start = current_month_start()?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM ai_gateway_rate_events WHERE occurred_at < NOW() - INTERVAL '2 minutes'",
            [],
        ))
        .await?;
        // A reservation that outlives the provider timeout may represent a streamed
        // response whose final usage could not be persisted. Convert it to a durable
        // debit instead of releasing it, otherwise disconnecting clients could bypass
        // the monthly budget. Explicit upstream failures release their reservations.
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE ai_gateway_cost_reservations SET is_conservative_debit = TRUE WHERE billing_period = $1 AND expires_at <= NOW() AND is_conservative_debit = FALSE",
            [month_start.date_naive().into()],
        ))
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM ai_gateway_cost_reservations WHERE billing_period < $1",
            [month_start.date_naive().into()],
        ))
        .await?;
        Ok(())
    }

    /// Count in-window requests for each rate-limited scope, reject if at
    /// limit, then insert the rate event row for accepted requests.
    async fn check_rates_and_record(
        &self,
        txn: &DatabaseTransaction,
        configs: &[AppliedConfig],
        request_id: &str,
    ) -> Result<(), AiGatewayError> {
        for config in configs {
            let Some(limit) = config.max_requests_per_minute else {
                continue;
            };
            let row = CountRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT
                    COUNT(*)::INT8 AS count,
                    GREATEST(
                        1,
                        CEIL(EXTRACT(EPOCH FROM (MIN(occurred_at) + INTERVAL '60 seconds' - NOW())))
                    )::INT8 AS retry_after_seconds
                FROM ai_gateway_rate_events
                WHERE scope = $1 AND occurred_at > NOW() - INTERVAL '60 seconds'"#,
                [config.scope.clone().into()],
            ))
            .one(txn)
            .await?
            .unwrap_or(CountRow {
                count: Some(0),
                retry_after_seconds: Some(1),
            });
            if row.count.unwrap_or(0) >= limit {
                return Err(AiGatewayError::RateLimitExceeded {
                    scope: config.scope.clone(),
                    limit_per_minute: limit,
                    retry_after_seconds: row
                        .retry_after_seconds
                        .and_then(|seconds| u64::try_from(seconds).ok())
                        .unwrap_or(1),
                });
            }
        }

        for config in configs {
            if config.max_requests_per_minute.is_none() {
                continue;
            }
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO ai_gateway_rate_events (request_id, scope) VALUES ($1::uuid, $2)",
                [request_id.into(), config.scope.clone().into()],
            ))
            .await?;
        }
        Ok(())
    }

    /// Sum committed spend and open reservations for each budget-limited scope,
    /// reject if the projected cost would exceed the monthly limit, then insert
    /// a cost reservation for accepted requests.
    async fn check_budgets_and_reserve(
        &self,
        txn: &DatabaseTransaction,
        attribution: &AiUsageAttribution,
        configs: &[AppliedConfig],
        request_id: &str,
        projected_cost: i64,
        billing_period: chrono::NaiveDate,
    ) -> Result<(), AiGatewayError> {
        for config in configs {
            let Some(limit) = config.max_cost_per_month_microcents else {
                continue;
            };
            let row = CostRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT (
                    SELECT COALESCE(SUM(estimated_cost_microcents), 0)::INT8
                    FROM ai_usage_logs
                    WHERE is_byok = FALSE
                      AND ((billing_period = $1
                        AND timestamp >= $1::date
                        AND timestamp < $1::date + INTERVAL '1 month 6 minutes')
                        OR (billing_period IS NULL
                          AND timestamp >= $1::date
                          AND timestamp < $1::date + INTERVAL '1 month'))
                      AND ($2 = 'instance'
                        OR ($3::INT IS NOT NULL AND $2 LIKE 'project:%' AND project_id = $3)
                        OR ($4::INT IS NOT NULL AND $2 LIKE 'environment:%' AND environment_id = $4)
                        OR ($5::INT IS NOT NULL AND $2 LIKE 'token:%' AND deployment_token_id = $5))
                ) + (
                    SELECT COALESCE(SUM(reserved_microcents), 0)::INT8
                    FROM ai_gateway_cost_reservations
                    WHERE scope = $2 AND billing_period = $1
                ) AS cost"#,
                [
                    billing_period.into(),
                    config.scope.clone().into(),
                    attribution.project_id.into(),
                    attribution.environment_id.into(),
                    attribution.deployment_token_id.into(),
                ],
            ))
            .one(txn)
            .await?
            .unwrap_or(CostRow { cost: Some(0) });
            let spent = row.cost.unwrap_or(0);
            if spent.saturating_add(projected_cost) > limit {
                warn!(
                    scope = %config.scope,
                    spent_microcents = spent,
                    limit_microcents = limit,
                    projected_cost_microcents = projected_cost,
                    "AI gateway monthly budget exceeded"
                );
                return Err(AiGatewayError::MonthlyBudgetExceeded {
                    scope: config.scope.clone(),
                    spent_microcents: spent,
                    limit_microcents: limit,
                });
            }
        }

        for config in configs {
            if config.max_cost_per_month_microcents.is_none() {
                continue;
            }
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO ai_gateway_cost_reservations \
                 (request_id, scope, reserved_microcents, billing_period, expires_at) \
                 VALUES ($1, $2, $3, $4, NOW() + INTERVAL '5 minutes')",
                [
                    request_id.into(),
                    config.scope.clone().into(),
                    projected_cost.into(),
                    billing_period.into(),
                ],
            ))
            .await?;
        }
        Ok(())
    }
}

fn current_month_start() -> Result<chrono::DateTime<Utc>, AiGatewayError> {
    let now = Utc::now();
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .ok_or_else(|| AiGatewayError::Internal {
            message: format!(
                "Failed to calculate current billing month for {}-{}",
                now.year(),
                now.month()
            ),
        })
}

fn applicable_scope_names(attribution: &AiUsageAttribution) -> Vec<String> {
    let mut scopes = vec!["instance".to_string()];
    if let Some(project_id) = attribution.project_id {
        scopes.push(format!("project:{project_id}"));
    }
    if let Some(environment_id) = attribution.environment_id {
        scopes.push(format!("environment:{environment_id}"));
    }
    if let Some(token_id) = attribution.deployment_token_id {
        scopes.push(format!("token:{token_id}"));
    }
    scopes
}

fn validate_nonnegative(
    scope: &str,
    field: &'static str,
    value: Option<i64>,
) -> Result<(), AiGatewayError> {
    if let Some(value) = value {
        if value < 0 {
            return Err(AiGatewayError::InvalidGovernanceConfig {
                scope: scope.to_string(),
                field,
                value,
            });
        }
    }
    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), AiGatewayError> {
    if scope == "instance" {
        return Ok(());
    }
    let valid = ["project:", "environment:", "token:"].iter().any(|prefix| {
        scope
            .strip_prefix(prefix)
            .and_then(|raw_id| raw_id.parse::<i32>().ok().map(|id| (raw_id, id)))
            .is_some_and(|(raw_id, id)| id > 0 && raw_id == id.to_string())
    });
    if valid {
        Ok(())
    } else {
        Err(AiGatewayError::InvalidGovernanceScope {
            scope: scope.to_string(),
        })
    }
}

fn validate_allowed_models(
    scope: &str,
    allowed_models: Option<&serde_json::Value>,
) -> Result<(), AiGatewayError> {
    let Some(value) = allowed_models else {
        return Ok(());
    };
    let valid = value.as_array().is_some_and(|models| {
        models
            .iter()
            .all(|model| model.as_str().is_some_and(|model| !model.trim().is_empty()))
    });
    if valid {
        Ok(())
    } else {
        Err(AiGatewayError::Validation {
            message: format!(
                "allowed_models for AI gateway scope '{}' must be an array of non-empty model IDs",
                scope
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbErr, MockDatabase, MockExecResult};

    fn config(
        scope: &str,
        models: Option<serde_json::Value>,
        rpm: Option<i64>,
        budget: Option<i64>,
    ) -> temps_entities::ai_gateway_config::Model {
        temps_entities::ai_gateway_config::Model {
            id: 1,
            scope: scope.to_string(),
            allowed_models: models,
            max_requests_per_minute: rpm,
            max_cost_per_month_microcents: budget,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            provider_type: "gateway".to_string(),
            agent_cli_provider_id: None,
            interactive_bridge_enabled: false,
            summary_provider_id: None,
            summary_model: None,
            summary_thinking_level: None,
        }
    }

    #[test]
    fn scope_names_are_ordered_from_broadest_to_most_specific() {
        let attribution = AiUsageAttribution {
            project_id: Some(7),
            environment_id: Some(11),
            deployment_token_id: Some(17),
            ..Default::default()
        };
        assert_eq!(
            applicable_scope_names(&attribution),
            vec!["instance", "project:7", "environment:11", "token:17"]
        );
    }

    #[test]
    fn scope_validation_requires_canonical_positive_ids() {
        assert!(validate_scope("instance").is_ok());
        assert!(validate_scope("project:7").is_ok());
        assert!(validate_scope("environment:11").is_ok());
        assert!(validate_scope("token:17").is_ok());
        for scope in [
            "token:0",
            "token:017",
            "project:0",
            "project:007",
            "project:+7",
        ] {
            assert!(matches!(
                validate_scope(scope),
                Err(AiGatewayError::InvalidGovernanceScope { .. })
            ));
        }
    }

    #[test]
    fn invalid_negative_limit_has_context() {
        assert!(matches!(
            validate_nonnegative("project:7", "max_requests_per_minute", Some(-1)),
            Err(AiGatewayError::InvalidGovernanceConfig {
                ref scope,
                field: "max_requests_per_minute",
                value: -1,
            }) if scope == "project:7"
        ));
    }

    #[test]
    fn allowed_model_validation_rejects_malformed_values() {
        assert!(validate_allowed_models("instance", None).is_ok());
        assert!(validate_allowed_models("instance", Some(&serde_json::json!([]))).is_ok());
        assert!(
            validate_allowed_models("project:7", Some(&serde_json::json!(["gpt-5-mini"]))).is_ok()
        );
        assert!(validate_allowed_models("project:7", Some(&serde_json::json!([""]))).is_err());
        assert!(validate_allowed_models("project:7", Some(&serde_json::json!({}))).is_err());
    }

    #[tokio::test]
    async fn model_allowlist_rejects_before_opening_transaction() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![config(
                "project:7",
                Some(serde_json::json!(["gpt-5-mini"])),
                None,
                None,
            )]])
            .into_connection();
        let service = GovernanceService::new(Arc::new(db));
        let attribution = AiUsageAttribution {
            project_id: Some(7),
            ..Default::default()
        };

        assert!(matches!(
            service
                .check_request(&attribution, "claude-sonnet-4-6", false, Some(10), Some(10))
                .await,
            Err(AiGatewayError::ModelNotAllowed { ref scope, .. }) if scope == "project:7"
        ));
    }

    #[tokio::test]
    async fn budget_requires_bounded_output_before_opening_transaction() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![config("project:7", None, None, Some(100))]])
            .into_connection();
        let service = GovernanceService::new(Arc::new(db));

        assert!(matches!(
            service
                .check_request(
                    &AiUsageAttribution { project_id: Some(7), ..Default::default() },
                    "gpt-5-mini",
                    false,
                    Some(10),
                    None,
                )
                .await,
            Err(AiGatewayError::BudgetRequiresMaxTokens { ref scope }) if scope == "project:7"
        ));
    }

    #[tokio::test]
    async fn budget_rejects_input_without_safe_cost_projection() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![config("project:7", None, None, Some(100))]])
            .into_connection();
        let service = GovernanceService::new(Arc::new(db));

        assert!(matches!(
            service
                .check_request(
                    &AiUsageAttribution {
                        project_id: Some(7),
                        ..Default::default()
                    },
                    "gpt-5-mini",
                    false,
                    None,
                    Some(10),
                )
                .await,
            Err(AiGatewayError::BudgetProjectionUnavailable { ref scope })
                if scope == "project:7"
        ));
    }

    #[tokio::test]
    async fn byok_skips_operator_budget_reservation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![config("project:7", None, None, Some(1))]])
            .into_connection();
        let service = GovernanceService::new(Arc::new(db));
        let result = service
            .check_request(
                &AiUsageAttribution {
                    project_id: Some(7),
                    ..Default::default()
                },
                "custom-model",
                true,
                Some(10),
                None,
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn upsert_config_creates_valid_scope() {
        let expected = config("project:7", None, Some(30), Some(500));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                Vec::<temps_entities::ai_gateway_config::Model>::new(),
                vec![expected.clone()],
            ])
            .into_connection();
        let service = GovernanceService::new(Arc::new(db));

        let created = service
            .upsert_config("project:7", None, Some(30), Some(500))
            .await
            .expect("valid project config should be created");
        assert_eq!(created.scope, expected.scope);
    }

    #[tokio::test]
    async fn delete_config_returns_typed_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let service = GovernanceService::new(Arc::new(db));
        assert!(matches!(
            service.delete_config("project:7").await,
            Err(AiGatewayError::GovernanceConfigNotFound { ref scope }) if scope == "project:7"
        ));
    }

    #[tokio::test]
    async fn check_request_propagates_database_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("governance database unavailable".to_string())])
            .into_connection();
        let service = GovernanceService::new(Arc::new(db));
        assert!(matches!(
            service
                .check_request(
                    &AiUsageAttribution::default(),
                    "gpt-5-mini",
                    false,
                    Some(10),
                    Some(10),
                )
                .await,
            Err(AiGatewayError::Database(DbErr::Custom(ref message)))
                if message == "governance database unavailable"
        ));
    }
}
