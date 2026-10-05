// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use temps_entities::{external_services, monitoring_alert_rules};

/// Maximum number of monitoring alert rules a single external service may
/// carry (built-in defaults included).
///
/// The alert evaluator issues one latest-value query per enabled rule every
/// cycle, so an unbounded rule count is a resource-exhaustion vector against
/// the shared database pool. Rules are keyed per service, which is also the
/// unit an operator reasons about; 100 is an order of magnitude above the
/// built-in defaults and any realistic hand-written set.
pub(crate) use temps_monitoring::MAX_ALERT_RULES_PER_SERVICE;

/// Why creating an alert rule failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AlertRuleCreateError {
    #[error(
        "External service {service_id} already has {existing} alert rules; the limit is {limit} \
         per service. Delete or reuse an existing rule before creating another."
    )]
    LimitReached {
        service_id: i32,
        existing: u64,
        limit: u64,
    },
    #[error("External service {service_id} not found while creating an alert rule")]
    ServiceNotFound { service_id: i32 },
    #[error("Database error while creating an alert rule for service {service_id}: {source}")]
    Database {
        service_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// Insert `rule` for `service_id` unless the service already has `limit`
/// rules.
///
/// The service row is locked `FOR UPDATE` for the duration of the count +
/// insert, so concurrent creations for the same service serialize and cannot
/// overshoot the limit.
pub(crate) async fn insert_alert_rule_within_limit(
    db: &sea_orm::DatabaseConnection,
    service_id: i32,
    rule: monitoring_alert_rules::ActiveModel,
    limit: u64,
) -> Result<monitoring_alert_rules::Model, AlertRuleCreateError> {
    use sea_orm::{PaginatorTrait, QuerySelect, TransactionTrait};

    let db_err = |source: sea_orm::DbErr| AlertRuleCreateError::Database { service_id, source };

    let txn = db.begin().await.map_err(db_err)?;
    if external_services::Entity::find_by_id(service_id)
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(db_err)?
        .is_none()
    {
        txn.rollback().await.map_err(db_err)?;
        return Err(AlertRuleCreateError::ServiceNotFound { service_id });
    }

    let existing = monitoring_alert_rules::Entity::find()
        .filter(monitoring_alert_rules::Column::ServiceId.eq(service_id))
        .count(&txn)
        .await
        .map_err(db_err)?;
    if existing >= limit {
        txn.rollback().await.map_err(db_err)?;
        return Err(AlertRuleCreateError::LimitReached {
            service_id,
            existing,
            limit,
        });
    }

    let inserted = rule.insert(&txn).await.map_err(db_err)?;
    txn.commit().await.map_err(db_err)?;
    Ok(inserted)
}
