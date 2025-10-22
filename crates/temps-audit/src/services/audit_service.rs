use anyhow::Context;
use chrono::Utc;
use sea_orm::{prelude::*, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use serde::Serialize;
use std::sync::Arc;
use temps_core::{AuditLogger, AuditOperation, UtcDateTime};
use temps_database::DbConnection;
use temps_entities::{audit_logs, ip_geolocations, users};
use temps_geo::IpAddressService;
use tracing::warn;

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
        let ip_address = operation.ip_address();
        let ip_address_id_val = match ip_address {
            Some(ip_address) => match self.ip_service.get_or_create_ip(&ip_address).await {
                Ok(ip_address) => Some(ip_address.id),
                Err(err) => {
                    warn!("Error getting ip address: {}", err);
                    None
                }
            },
            None => None,
        };

        // Serialize the operation to JSON
        let data_json = operation.serialize()?;

        let new_audit_log = audit_logs::ActiveModel {
            user_id: Set(operation.user_id()),
            operation_type: Set(operation.operation_type()),
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
            .context("Failed to load filtered audit logs")?;

        // Fetch related user and IP geolocation data for each log
        let mut audit_details = Vec::new();
        for log in logs {
            // Fetch related user
            let user = temps_entities::users::Entity::find_by_id(log.user_id)
                .one(self.db.as_ref())
                .await?;

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
            .context("Failed to get audit log by ID")?;

        if let Some(log) = log {
            // Fetch related user
            let user = temps_entities::users::Entity::find_by_id(log.user_id)
                .one(self.db.as_ref())
                .await?;

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
