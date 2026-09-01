// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use sea_orm::{
    prelude::*, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, Statement,
};
use serde::Serialize;
use std::sync::Arc;
use temps_core::{AuditLogger, AuditOperation, UtcDateTime};
use temps_database::DbConnection;
use temps_entities::{audit_logs, ip_geolocations, users};
use temps_geo::IpAddressService;
use tracing::warn;

pub const PERMISSION_DENIED_RETENTION_DAYS: i64 = 90;
pub const PERMISSION_DENIED_PRUNE_BATCH_SIZE: u64 = 2_048;
pub const PERMISSION_DENIED_PRUNE_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(60 * 60);
const PERMISSION_DENIED_OPERATION: &str = "PERMISSION_DENIED";

#[derive(Debug, thiserror::Error)]
pub enum AuditMaintenanceError {
    #[error("permission-denied retention batch size {batch_size} is outside 1..={max_batch_size}")]
    InvalidBatchSize {
        batch_size: u64,
        max_batch_size: u64,
    },
    #[error(
        "failed to prune permission-denied audit rows older than {cutoff} with batch size \
         {batch_size}: {source}"
    )]
    PrunePermissionDenied {
        cutoff: DateTimeUtc,
        batch_size: u64,
        #[source]
        source: DbErr,
    },
}

/// Audit log with enriched user and IP geolocation data
#[derive(Debug, Clone, Serialize)]
pub struct AuditLogWithDetails {
    pub log: audit_logs::Model,
    pub user: Option<users::Model>,
    pub ip_address: Option<ip_geolocations::Model>,
}

pub struct AuditService {
    db: Arc<DbConnection>,
    ip_service: Arc<IpAddressService>,
}

impl AuditService {
    pub fn new(db: Arc<DbConnection>, ip_service: Arc<IpAddressService>) -> Self {
        Self { db, ip_service }
    }

    pub async fn create_audit_log_typed<T: AuditOperation + ?Sized>(
        &self,
        operation: &T,
    ) -> anyhow::Result<temps_entities::audit_logs::Model> {
        let now = Utc::now();
        let operation_type = operation.operation_type();
        let ip_address = operation.ip_address();
        // Permission denials are attacker-amplifiable and already retain their
        // safe origin string inside the bounded audit JSON. Creating a durable
        // geolocation row for each rotating denial IP would outlive the 90-day
        // audit row and turn the security signal into an unbounded side table.
        let ip_address_id_val = match (operation_type.as_str(), ip_address) {
            (PERMISSION_DENIED_OPERATION, _) => None,
            (_, Some(ip_address)) => match self.ip_service.get_or_create_ip(&ip_address).await {
                Ok(ip_address) => Some(ip_address.id),
                Err(err) => {
                    warn!("Error getting ip address {:?}: {}", ip_address, err);
                    None
                }
            },
            (_, None) => None,
        };

        // Serialize the operation to JSON
        let data_json = operation.serialize()?;

        let new_audit_log = audit_logs::ActiveModel {
            user_id: Set(operation.user_id()),
            operation_type: Set(operation_type),
            user_agent: Set(operation.user_agent().to_string()),
            ip_address_id: Set(ip_address_id_val),
            audit_date: Set(now),
            created_at: Set(now),
            data: Set(data_json),
            ..Default::default()
        };

        let result = new_audit_log
            .insert(self.db.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create audit log: {}", e))?;

        Ok(result)
    }

    /// Delete one bounded batch of expired permission-denial rows.
    ///
    /// Production runs this hourly with a 2,048-row batch and 90-day cutoff.
    /// The recorder can persist at most 17 rows/minute (1,020/hour), so one
    /// successful pass removes more than twice the maximum rows created in a
    /// cadence interval while keeping every delete transaction bounded.
    pub async fn prune_permission_denied_before(
        &self,
        cutoff: DateTimeUtc,
        batch_size: u64,
    ) -> Result<u64, AuditMaintenanceError> {
        if batch_size == 0 || batch_size > PERMISSION_DENIED_PRUNE_BATCH_SIZE {
            return Err(AuditMaintenanceError::InvalidBatchSize {
                batch_size,
                max_batch_size: PERMISSION_DENIED_PRUNE_BATCH_SIZE,
            });
        }

        let statement = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
WITH expired AS (
    SELECT id
    FROM audit_logs
    WHERE operation_type = $1 AND audit_date < $2
    ORDER BY audit_date ASC, id ASC
    LIMIT $3
)
DELETE FROM audit_logs
WHERE id IN (SELECT id FROM expired)
"#,
            vec![
                PERMISSION_DENIED_OPERATION.into(),
                cutoff.into(),
                (batch_size as i64).into(),
            ],
        );
        self.db
            .execute(statement)
            .await
            .map(|result| result.rows_affected())
            .map_err(|source| AuditMaintenanceError::PrunePermissionDenied {
                cutoff,
                batch_size,
                source,
            })
    }

    pub async fn get_user_audit_logs(
        &self,
        user_id_param: i32,
        limit: Option<u64>,
    ) -> anyhow::Result<Vec<temps_entities::audit_logs::Model>> {
        let mut query = temps_entities::audit_logs::Entity::find()
            .filter(temps_entities::audit_logs::Column::UserId.eq(user_id_param))
            .order_by_desc(temps_entities::audit_logs::Column::AuditDate);

        if let Some(limit_val) = limit {
            query = query.limit(limit_val);
        }

        let results = query.all(self.db.as_ref()).await?;
        Ok(results)
    }

    pub async fn get_recent_audit_logs(
        &self,
        limit: u64,
    ) -> anyhow::Result<Vec<temps_entities::audit_logs::Model>> {
        let results = temps_entities::audit_logs::Entity::find()
            .order_by_desc(temps_entities::audit_logs::Column::AuditDate)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;

        Ok(results)
    }
    pub async fn filter_audit_logs(
        &self,
        action: Option<&str>,
        user_id_p: Option<i32>,
        from: Option<UtcDateTime>,
        to: Option<UtcDateTime>,
        limit: i32,
        offset: i32,
    ) -> anyhow::Result<Vec<AuditLogWithDetails>> {
        let mut query = temps_entities::audit_logs::Entity::find();

        // Apply filters
        if let Some(action_filter) = action {
            query = query
                .filter(temps_entities::audit_logs::Column::OperationType.contains(action_filter));
        }
        if let Some(uid) = user_id_p {
            query = query.filter(temps_entities::audit_logs::Column::UserId.eq(uid));
        }
        if let Some(from_date) = from {
            query = query.filter(temps_entities::audit_logs::Column::AuditDate.gte(from_date));
        }
        if let Some(to_date) = to {
            query = query.filter(temps_entities::audit_logs::Column::AuditDate.lte(to_date));
        }

        // Apply pagination and ordering, then fetch basic audit logs
        let logs = query
            .order_by_desc(temps_entities::audit_logs::Column::AuditDate)
            .limit(limit as u64)
            .offset(offset as u64)
            .all(self.db.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load filtered audit logs: {}", e))?;

        // Fetch related user and IP geolocation data for each log
        let mut audit_details = Vec::new();
        for log in logs {
            // Fetch related user (user_id is None once the account is deleted)
            let user = match log.user_id {
                Some(uid) => {
                    temps_entities::users::Entity::find_by_id(uid)
                        .one(self.db.as_ref())
                        .await?
                }
                None => None,
            };

            // Fetch related IP geolocation if present
            let ip_address = if let Some(ip_address_id) = log.ip_address_id {
                temps_entities::ip_geolocations::Entity::find_by_id(ip_address_id)
                    .one(self.db.as_ref())
                    .await?
            } else {
                None
            };

            audit_details.push(AuditLogWithDetails {
                log,
                user,
                ip_address,
            });
        }

        Ok(audit_details)
    }

    pub async fn get_log_by_id(&self, log_id: i32) -> anyhow::Result<Option<AuditLogWithDetails>> {
        let log = temps_entities::audit_logs::Entity::find_by_id(log_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get audit log by ID {}: {}", log_id, e))?;

        if let Some(log) = log {
            // Fetch related user (user_id is None once the account is deleted)
            let user = match log.user_id {
                Some(uid) => {
                    temps_entities::users::Entity::find_by_id(uid)
                        .one(self.db.as_ref())
                        .await?
                }
                None => None,
            };

            // Fetch related IP geolocation if present
            let ip_address = if let Some(ip_address_id) = log.ip_address_id {
                temps_entities::ip_geolocations::Entity::find_by_id(ip_address_id)
                    .one(self.db.as_ref())
                    .await?
            } else {
                None
            };

            Ok(Some(AuditLogWithDetails {
                log,
                user,
                ip_address,
            }))
        } else {
            Ok(None)
        }
    }
}

// Implement the AuditLogger trait for AuditService
#[async_trait::async_trait]
impl AuditLogger for AuditService {
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
        self.create_audit_log_typed(operation).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use temps_geo::geoip_service::{GeoIpService, MockGeoIpService};

    fn service_with(db: sea_orm::DatabaseConnection) -> AuditService {
        let db = Arc::new(db);
        let geoip = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip));
        AuditService::new(db, ip_service)
    }

    fn log_row(user_id: Option<i32>) -> audit_logs::Model {
        let now = Utc::now();
        audit_logs::Model {
            id: 1,
            user_id,
            user_agent: "test-agent".to_string(),
            operation_type: "user.login".to_string(),
            ip_address_id: None,
            audit_date: now,
            created_at: now,
            data: "{}".to_string(),
        }
    }

    #[derive(Serialize)]
    struct TestAuditOperation {
        user_id: Option<i32>,
        operation_type: &'static str,
        ip_address: Option<String>,
    }

    impl AuditOperation for TestAuditOperation {
        fn operation_type(&self) -> String {
            self.operation_type.to_string()
        }

        fn user_id(&self) -> Option<i32> {
            self.user_id
        }

        fn ip_address(&self) -> Option<String> {
            self.ip_address.clone()
        }

        fn user_agent(&self) -> &str {
            "test-agent"
        }

        fn serialize(&self) -> anyhow::Result<String> {
            serde_json::to_string(self)
                .map_err(|error| anyhow::anyhow!("Failed to serialize test audit: {}", error))
        }
    }

    async fn persisted_actor_value<T: AuditOperation>(
        operation: &T,
        user_id: Option<i32>,
    ) -> Value {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![log_row(user_id)]])
                .into_connection(),
        );
        let geoip = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip));
        let service = AuditService::new(db.clone(), ip_service);

        service
            .create_audit_log_typed(operation)
            .await
            .expect("test audit should be inserted");

        drop(service);
        let transactions = Arc::try_unwrap(db)
            .expect("audit service should release the database connection")
            .into_transaction_log();
        let statement = &transactions
            .first()
            .expect("audit insert should execute one statement")
            .statements()[0];
        statement
            .values
            .as_ref()
            .expect("audit insert should bind values")
            .0
            .first()
            .expect("user_id should be the first bound audit value")
            .clone()
    }

    #[tokio::test]
    async fn test_create_audit_log_persists_null_actor() {
        let operation = TestAuditOperation {
            user_id: None,
            operation_type: "TEST_OPERATION",
            ip_address: None,
        };
        assert_eq!(
            persisted_actor_value(&operation, operation.user_id()).await,
            Value::Int(None)
        );
    }

    #[tokio::test]
    async fn test_create_audit_log_persists_known_actor() {
        let operation = TestAuditOperation {
            user_id: Some(42),
            operation_type: "TEST_OPERATION",
            ip_address: None,
        };
        assert_eq!(
            persisted_actor_value(&operation, operation.user_id()).await,
            Value::Int(Some(42))
        );
    }

    #[tokio::test]
    async fn permission_denial_keeps_ip_in_json_without_geolocation_row() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![log_row(Some(42))]])
                .into_connection(),
        );
        let geoip = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip));
        let service = AuditService::new(db.clone(), ip_service);
        let operation = TestAuditOperation {
            user_id: Some(42),
            operation_type: PERMISSION_DENIED_OPERATION,
            ip_address: Some("203.0.113.91".to_string()),
        };

        service
            .create_audit_log_typed(&operation)
            .await
            .expect("permission denial audit should persist");
        drop(service);
        let transactions = Arc::try_unwrap(db)
            .expect("audit service should release database")
            .into_transaction_log();
        let statements: Vec<_> = transactions
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect();
        assert_eq!(statements.len(), 1, "denial must only insert the audit row");
        assert!(statements[0].sql.contains("INSERT INTO \"audit_logs\""));
        assert!(!statements[0].sql.contains("ip_geolocations"));
        assert!(AuditOperation::serialize(&operation)
            .expect("test audit should serialize")
            .contains("203.0.113.91"));
    }

    #[tokio::test]
    async fn normal_audit_still_enriches_existing_ip() {
        let now = Utc::now();
        let ip = temps_entities::ip_geolocations::Model {
            id: 77,
            ip_address: "203.0.113.92".to_string(),
            latitude: None,
            longitude: None,
            region: None,
            city: None,
            country: "".to_string(),
            country_code: None,
            timezone: None,
            is_eu: false,
            asn_org: None,
            is_hosting_provider: None,
            created_at: now,
            updated_at: now,
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![ip]])
                .append_query_results([vec![log_row(Some(42))]])
                .into_connection(),
        );
        let geoip = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip));
        let service = AuditService::new(db.clone(), ip_service);
        let operation = TestAuditOperation {
            user_id: Some(42),
            operation_type: "NORMAL_AUDIT",
            ip_address: Some("203.0.113.92".to_string()),
        };

        service
            .create_audit_log_typed(&operation)
            .await
            .expect("normal audit should enrich IP");
        drop(service);
        let transactions = Arc::try_unwrap(db)
            .expect("audit service should release database")
            .into_transaction_log();
        let sql = transactions
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sql.contains("FROM \"ip_geolocations\""));
        assert!(sql.contains("INSERT INTO \"audit_logs\""));
    }

    #[tokio::test]
    async fn test_get_log_by_id_without_user_skips_user_lookup() {
        // Only the audit row itself is prepared. If the service issued a
        // users lookup for a log whose user_id is NULL (account deleted),
        // the mock would have no result for it and the call would error.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![log_row(None)]])
            .into_connection();
        let service = service_with(db);

        let details = service
            .get_log_by_id(1)
            .await
            .expect("audit log 1 with NULL user_id should load")
            .expect("audit log 1 should exist");

        assert_eq!(details.log.user_id, None);
        assert!(details.user.is_none());
    }

    #[tokio::test]
    async fn test_get_log_by_id_with_user_id_performs_user_lookup() {
        // A second (empty) result set is prepared for the users query: the
        // service must consume it when user_id is present, and tolerate the
        // referenced user row being absent.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![log_row(Some(7))]])
            .append_query_results([Vec::<temps_entities::users::Model>::new()])
            .into_connection();
        let service = service_with(db);

        let details = service
            .get_log_by_id(1)
            .await
            .expect("audit log 1 with user_id 7 should load")
            .expect("audit log 1 should exist");

        assert_eq!(details.log.user_id, Some(7));
        assert!(details.user.is_none());
    }

    #[tokio::test]
    async fn permission_denied_retention_is_bounded_and_parameterized() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 37,
                }])
                .into_connection(),
        );
        let geoip = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
        let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip));
        let service = AuditService::new(db.clone(), ip_service);
        let cutoff = Utc::now() - chrono::Duration::days(PERMISSION_DENIED_RETENTION_DAYS);

        let deleted = service
            .prune_permission_denied_before(cutoff, 128)
            .await
            .expect("bounded retention delete should succeed");
        assert_eq!(deleted, 37);

        drop(service);
        let transactions = Arc::try_unwrap(db)
            .expect("audit service should release the database connection")
            .into_transaction_log();
        let statement = &transactions
            .first()
            .expect("retention should execute one statement")
            .statements()[0];
        assert!(statement.sql.contains("operation_type = $1"));
        assert!(statement.sql.contains("audit_date < $2"));
        assert!(statement.sql.contains("LIMIT $3"));
        assert!(statement.sql.contains("ORDER BY audit_date ASC, id ASC"));
        assert_eq!(
            statement
                .values
                .as_ref()
                .expect("retention statement binds values")
                .0
                .len(),
            3
        );
        assert!(!statement.sql.contains(PERMISSION_DENIED_OPERATION));
    }

    #[tokio::test]
    async fn permission_denied_retention_rejects_unbounded_batches() {
        let service = service_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        for batch_size in [0, PERMISSION_DENIED_PRUNE_BATCH_SIZE + 1] {
            let error = service
                .prune_permission_denied_before(Utc::now(), batch_size)
                .await
                .expect_err("invalid batch must fail before querying");
            assert!(matches!(
                error,
                AuditMaintenanceError::InvalidBatchSize {
                    batch_size: actual,
                    max_batch_size: PERMISSION_DENIED_PRUNE_BATCH_SIZE,
                } if actual == batch_size
            ));
        }
    }

    #[tokio::test]
    async fn permission_denied_retention_database_error_has_context() {
        let service = service_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_errors([DbErr::Custom("retention unavailable".to_string())])
                .into_connection(),
        );
        let cutoff = Utc::now() - chrono::Duration::days(PERMISSION_DENIED_RETENTION_DAYS);

        let error = service
            .prune_permission_denied_before(cutoff, 256)
            .await
            .expect_err("database failure should be typed");
        assert!(matches!(
            error,
            AuditMaintenanceError::PrunePermissionDenied {
                cutoff: actual_cutoff,
                batch_size: 256,
                ..
            } if actual_cutoff == cutoff
        ));
    }

    #[test]
    fn retention_capacity_exceeds_maximum_permission_denied_creation_rate() {
        const MAX_ROWS_PER_MINUTE: u64 = 17;
        let intervals_per_hour = PERMISSION_DENIED_PRUNE_INTERVAL.as_secs() / 60;
        assert_eq!(intervals_per_hour, 60);
        assert!(
            PERMISSION_DENIED_PRUNE_BATCH_SIZE > MAX_ROWS_PER_MINUTE * intervals_per_hour,
            "cleanup must delete faster than the recorder can create rows"
        );
    }
}
