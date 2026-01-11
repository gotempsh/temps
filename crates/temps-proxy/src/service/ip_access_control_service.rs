use sea_orm::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::ip_access_control;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum IpAccessControlError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("IP address not found: {0}")]
    NotFound(String),

    #[error("Invalid IP address format: {0}")]
    InvalidIpAddress(String),

    #[error("Duplicate IP address: {0}")]
    DuplicateIp(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Response model for IP access control rules
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IpAccessControlResponse {
    pub id: i32,
    pub ip_address: String,
    pub action: String,
    pub reason: Option<String>,
    pub created_by: Option<i32>,
    #[schema(example = "2025-10-12T12:15:47.609Z")]
    pub created_at: String,
    #[schema(example = "2025-10-12T12:15:47.609Z")]
    pub updated_at: String,
}

impl From<ip_access_control::Model> for IpAccessControlResponse {
    fn from(model: ip_access_control::Model) -> Self {
        Self {
            id: model.id,
            ip_address: model.ip_address,
            action: model.action,
            reason: model.reason,
            created_by: model.created_by,
            created_at: model.created_at.to_rfc3339(),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

/// Request to create an IP access control rule
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateIpAccessControlRequest {
    /// IP address in CIDR notation (e.g., "192.168.1.1" or "10.0.0.0/24")
    #[schema(example = "192.168.1.100")]
    pub ip_address: String,
    /// Action to take: "block" or "allow"
    #[schema(example = "block")]
    pub action: String,
    /// Optional reason for the action
    #[schema(example = "Malicious activity detected")]
    pub reason: Option<String>,
}

/// Request to update an IP access control rule
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateIpAccessControlRequest {
    /// Optional new IP address
    pub ip_address: Option<String>,
    /// Optional new action
    pub action: Option<String>,
    /// Optional new reason
    pub reason: Option<String>,
}

pub struct IpAccessControlService {
    db: Arc<DatabaseConnection>,
}

impl IpAccessControlService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Create a new IP access control rule
    pub async fn create(
        &self,
        request: CreateIpAccessControlRequest,
        created_by: Option<i32>,
    ) -> Result<ip_access_control::Model, IpAccessControlError> {
        // Validate IP address format (basic validation)
        if !Self::is_valid_ip_or_cidr(&request.ip_address) {
            return Err(IpAccessControlError::InvalidIpAddress(
                request.ip_address.clone(),
            ));
        }

        // Validate action
        if request.action != "block" && request.action != "allow" {
            return Err(IpAccessControlError::InvalidIpAddress(format!(
                "Invalid action: {}. Must be 'block' or 'allow'",
                request.action
            )));
        }

        // Check for duplicates and insert using raw SQL with proper inet casting
        use sea_orm::{ConnectionTrait, Statement};

        let check_sql =
            "SELECT COUNT(*) as count FROM ip_access_control WHERE ip_address = $1::inet";

        let check_result = self
            .db
            .query_one(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                check_sql,
                vec![request.ip_address.clone().into()],
            ))
            .await?;

        if let Some(row) = check_result {
            let count: i64 = row.try_get("", "count")?;
            if count > 0 {
                return Err(IpAccessControlError::DuplicateIp(request.ip_address));
            }
        }

        let now = chrono::Utc::now();

        let sql = r#"
            INSERT INTO ip_access_control (ip_address, action, reason, created_by, created_at, updated_at)
            VALUES ($1::inet, $2, $3, $4, $5, $6)
            RETURNING id, ip_address::text, action, reason, created_by, created_at, updated_at
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                sql,
                vec![
                    request.ip_address.clone().into(),
                    request.action.into(),
                    request.reason.into(),
                    created_by.into(),
                    now.into(),
                    now.into(),
                ],
            ))
            .await?;

        if let Some(row) = result {
            let rule = ip_access_control::Model {
                id: row.try_get("", "id")?,
                ip_address: row.try_get("", "ip_address")?,
                action: row.try_get("", "action")?,
                reason: row.try_get("", "reason")?,
                created_by: row.try_get("", "created_by")?,
                created_at: row.try_get("", "created_at")?,
                updated_at: row.try_get("", "updated_at")?,
            };
            Ok(rule)
        } else {
            Err(IpAccessControlError::Internal(
                "Failed to insert rule".to_string(),
            ))
        }
    }

    /// List all IP access control rules
    pub async fn list(
        &self,
        action_filter: Option<String>,
    ) -> Result<Vec<ip_access_control::Model>, IpAccessControlError> {
        use sea_orm::{ConnectionTrait, Statement};

        let (sql, values) = if let Some(action) = action_filter {
            (
                "SELECT id, ip_address::text, action, reason, created_by, created_at, updated_at FROM ip_access_control WHERE action = $1 ORDER BY created_at DESC".to_string(),
                vec![action.into()],
            )
        } else {
            (
                "SELECT id, ip_address::text, action, reason, created_by, created_at, updated_at FROM ip_access_control ORDER BY created_at DESC".to_string(),
                vec![],
            )
        };

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let mut rules = Vec::new();
        for row in rows {
            rules.push(ip_access_control::Model {
                id: row.try_get("", "id")?,
                ip_address: row.try_get("", "ip_address")?,
                action: row.try_get("", "action")?,
                reason: row.try_get("", "reason")?,
                created_by: row.try_get("", "created_by")?,
                created_at: row.try_get("", "created_at")?,
                updated_at: row.try_get("", "updated_at")?,
            });
        }

        Ok(rules)
    }

    /// Get a single IP access control rule by ID
    pub async fn get_by_id(
        &self,
        id: i32,
    ) -> Result<ip_access_control::Model, IpAccessControlError> {
        use sea_orm::{ConnectionTrait, Statement};

        let sql = "SELECT id, ip_address::text, action, reason, created_by, created_at, updated_at FROM ip_access_control WHERE id = $1";

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                sql,
                vec![id.into()],
            ))
            .await?;

        if let Some(row) = result {
            Ok(ip_access_control::Model {
                id: row.try_get("", "id")?,
                ip_address: row.try_get("", "ip_address")?,
                action: row.try_get("", "action")?,
                reason: row.try_get("", "reason")?,
                created_by: row.try_get("", "created_by")?,
                created_at: row.try_get("", "created_at")?,
                updated_at: row.try_get("", "updated_at")?,
            })
        } else {
            Err(IpAccessControlError::NotFound(id.to_string()))
        }
    }

    /// Check if an IP address is blocked
    pub async fn is_blocked(&self, ip: &str) -> Result<bool, IpAccessControlError> {
        // Check if IP is blocked using PostgreSQL inet operators
        // This supports both exact matches and CIDR ranges
        // The <<= operator checks if the IP is contained within or equal to the stored CIDR/IP
        use sea_orm::{ConnectionTrait, Statement};

        let sql = r#"
            SELECT COUNT(*) as count
            FROM ip_access_control
            WHERE action = 'block'
            AND $1::inet <<= ip_address
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                sql,
                vec![ip.to_string().into()],
            ))
            .await?;

        if let Some(row) = result {
            let count: i64 = row.try_get("", "count")?;
            Ok(count > 0)
        } else {
            Ok(false)
        }
    }

    /// Update an IP access control rule
    pub async fn update(
        &self,
        id: i32,
        request: UpdateIpAccessControlRequest,
    ) -> Result<ip_access_control::Model, IpAccessControlError> {
        // Verify the rule exists
        let _ = self.get_by_id(id).await?;

        // Validate inputs
        if let Some(ref ip_address) = request.ip_address {
            if !Self::is_valid_ip_or_cidr(ip_address) {
                return Err(IpAccessControlError::InvalidIpAddress(ip_address.clone()));
            }
        }

        if let Some(ref action) = request.action {
            if action != "block" && action != "allow" {
                return Err(IpAccessControlError::InvalidIpAddress(format!(
                    "Invalid action: {}. Must be 'block' or 'allow'",
                    action
                )));
            }
        }

        // Build UPDATE query with proper inet casting
        use sea_orm::{ConnectionTrait, Statement};

        let now = chrono::Utc::now();
        let mut updates = Vec::new();
        let mut values: Vec<sea_orm::Value> = Vec::new();
        let mut param_counter = 1;

        if let Some(ip_address) = &request.ip_address {
            updates.push(format!("ip_address = ${}::inet", param_counter));
            values.push(ip_address.clone().into());
            param_counter += 1;
        }

        if let Some(action) = &request.action {
            updates.push(format!("action = ${}", param_counter));
            values.push(action.clone().into());
            param_counter += 1;
        }

        if let Some(reason) = &request.reason {
            updates.push(format!("reason = ${}", param_counter));
            values.push(Some(reason.clone()).into());
            param_counter += 1;
        }

        // Always update updated_at
        updates.push(format!("updated_at = ${}", param_counter));
        values.push(now.into());
        param_counter += 1;

        // Add id for WHERE clause
        values.push(id.into());

        let sql = format!(
            "UPDATE ip_access_control SET {} WHERE id = ${} RETURNING id, ip_address::text, action, reason, created_by, created_at, updated_at",
            updates.join(", "),
            param_counter
        );

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        if let Some(row) = result {
            let rule = ip_access_control::Model {
                id: row.try_get("", "id")?,
                ip_address: row.try_get("", "ip_address")?,
                action: row.try_get("", "action")?,
                reason: row.try_get("", "reason")?,
                created_by: row.try_get("", "created_by")?,
                created_at: row.try_get("", "created_at")?,
                updated_at: row.try_get("", "updated_at")?,
            };
            Ok(rule)
        } else {
            Err(IpAccessControlError::NotFound(id.to_string()))
        }
    }

    /// Delete an IP access control rule
    pub async fn delete(&self, id: i32) -> Result<(), IpAccessControlError> {
        let rule = self.get_by_id(id).await?;
        rule.delete(self.db.as_ref()).await?;
        Ok(())
    }

    /// Basic IP/CIDR validation (can be improved with proper IP parsing library)
    fn is_valid_ip_or_cidr(ip: &str) -> bool {
        // Allow IPv4 addresses and CIDR notation
        // This is a simple validation - PostgreSQL inet type will do final validation
        if ip.contains('/') {
            // CIDR notation
            let parts: Vec<&str> = ip.split('/').collect();
            if parts.len() != 2 {
                return false;
            }
            // Check if prefix length is valid
            if let Ok(prefix) = parts[1].parse::<u8>() {
                if prefix > 32 {
                    return false;
                }
            } else {
                return false;
            }
            // Validate the IP part
            Self::is_valid_ipv4(parts[0])
        } else {
            // Single IP address
            Self::is_valid_ipv4(ip)
        }
    }

    /// Basic IPv4 validation
    fn is_valid_ipv4(ip: &str) -> bool {
        let parts: Vec<&str> = ip.split('.').collect();
        if parts.len() != 4 {
            return false;
        }
        for part in parts {
            // parse::<u8>() already validates 0-255 range
            if part.parse::<u8>().is_err() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: Database tests are disabled because MockDatabase doesn't support
    // PostgreSQL-specific inet operators (::inet casting and <<= operator).
    // These require integration tests with a real PostgreSQL database.

    #[test]
    fn test_is_valid_ipv4() {
        assert!(IpAccessControlService::is_valid_ipv4("192.168.1.1"));
        assert!(IpAccessControlService::is_valid_ipv4("10.0.0.1"));
        assert!(IpAccessControlService::is_valid_ipv4("255.255.255.255"));
        assert!(!IpAccessControlService::is_valid_ipv4("256.1.1.1"));
        assert!(!IpAccessControlService::is_valid_ipv4("192.168.1"));
        assert!(!IpAccessControlService::is_valid_ipv4("invalid"));
    }

    #[test]
    fn test_is_valid_ip_or_cidr() {
        assert!(IpAccessControlService::is_valid_ip_or_cidr("192.168.1.1"));
        assert!(IpAccessControlService::is_valid_ip_or_cidr("10.0.0.0/24"));
        assert!(IpAccessControlService::is_valid_ip_or_cidr(
            "192.168.1.0/16"
        ));
        assert!(!IpAccessControlService::is_valid_ip_or_cidr("10.0.0.0/33"));
        assert!(!IpAccessControlService::is_valid_ip_or_cidr("256.1.1.1/24"));
        assert!(!IpAccessControlService::is_valid_ip_or_cidr("invalid"));
    }

    // Database tests are commented out because MockDatabase doesn't support PostgreSQL-specific features
    // For integration tests with real PostgreSQL database, see tests/ directory

    /*
    #[tokio::test]
    async fn test_create_with_valid_ip() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                // Check for duplicates query
                vec![Vec::<ip_access_control::Model>::new()],
                // Insert query result
                vec![ip_access_control::Model {
                    id: 1,
                    ip_address: "192.168.1.100".to_string(),
                    action: "block".to_string(),
                    reason: Some("Test reason".to_string()),
                    created_by: Some(1),
                    created_at: now,
                    updated_at: now,
                }],
            ])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));

        let request = CreateIpAccessControlRequest {
            ip_address: "192.168.1.100".to_string(),
            action: "block".to_string(),
            reason: Some("Test reason".to_string()),
        };

        let result = service.create(request, Some(1)).await;
        assert!(result.is_ok());

        let rule = result.unwrap();
        assert_eq!(rule.ip_address, "192.168.1.100");
        assert_eq!(rule.action, "block");
    }

    #[tokio::test]
    async fn test_create_with_invalid_ip() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = IpAccessControlService::new(Arc::new(db));

        let request = CreateIpAccessControlRequest {
            ip_address: "invalid-ip".to_string(),
            action: "block".to_string(),
            reason: None,
        };

        let result = service.create(request, None).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            IpAccessControlError::InvalidIpAddress(ip) => {
                assert_eq!(ip, "invalid-ip");
            }
            _ => panic!("Expected InvalidIpAddress error"),
        }
    }

    #[tokio::test]
    async fn test_create_with_invalid_action() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = IpAccessControlService::new(Arc::new(db));

        let request = CreateIpAccessControlRequest {
            ip_address: "192.168.1.1".to_string(),
            action: "invalid".to_string(),
            reason: None,
        };

        let result = service.create(request, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_with_cidr_notation() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                // Check for duplicates query
                vec![Vec::<ip_access_control::Model>::new()],
                // Insert query result
                vec![ip_access_control::Model {
                    id: 1,
                    ip_address: "10.0.0.0/24".to_string(),
                    action: "block".to_string(),
                    reason: None,
                    created_by: None,
                    created_at: now,
                    updated_at: now,
                }],
            ])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));

        let request = CreateIpAccessControlRequest {
            ip_address: "10.0.0.0/24".to_string(),
            action: "block".to_string(),
            reason: None,
        };

        let result = service.create(request, None).await;
        assert!(result.is_ok());

        let rule = result.unwrap();
        assert_eq!(rule.ip_address, "10.0.0.0/24");
    }

    #[tokio::test]
    async fn test_list_all_rules() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                ip_access_control::Model {
                    id: 1,
                    ip_address: "192.168.1.100".to_string(),
                    action: "block".to_string(),
                    reason: Some("Malicious".to_string()),
                    created_by: Some(1),
                    created_at: now,
                    updated_at: now,
                },
                ip_access_control::Model {
                    id: 2,
                    ip_address: "10.0.0.5".to_string(),
                    action: "allow".to_string(),
                    reason: Some("Trusted".to_string()),
                    created_by: Some(1),
                    created_at: now,
                    updated_at: now,
                },
            ]])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.list(None).await;

        assert!(result.is_ok());
        let rules = result.unwrap();
        assert_eq!(rules.len(), 2);
    }

    #[tokio::test]
    async fn test_list_filtered_by_action() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![ip_access_control::Model {
                id: 1,
                ip_address: "192.168.1.100".to_string(),
                action: "block".to_string(),
                reason: Some("Malicious".to_string()),
                created_by: Some(1),
                created_at: now,
                updated_at: now,
            }]])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.list(Some("block".to_string())).await;

        assert!(result.is_ok());
        let rules = result.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, "block");
    }

    #[tokio::test]
    async fn test_get_by_id_found() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![ip_access_control::Model {
                id: 1,
                ip_address: "192.168.1.100".to_string(),
                action: "block".to_string(),
                reason: Some("Test".to_string()),
                created_by: Some(1),
                created_at: now,
                updated_at: now,
            }]])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.get_by_id(1).await;

        assert!(result.is_ok());
        let rule = result.unwrap();
        assert_eq!(rule.id, 1);
        assert_eq!(rule.ip_address, "192.168.1.100");
    }

    #[tokio::test]
    async fn test_get_by_id_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![Vec::<ip_access_control::Model>::new()]])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.get_by_id(999).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            IpAccessControlError::NotFound(_) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_is_blocked_returns_true() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![ip_access_control::Model {
                id: 1,
                ip_address: "192.168.1.100".to_string(),
                action: "block".to_string(),
                reason: None,
                created_by: None,
                created_at: now,
                updated_at: now,
            }]])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.is_blocked("192.168.1.100").await;

        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_is_blocked_returns_false() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![Vec::<ip_access_control::Model>::new()]])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.is_blocked("192.168.1.100").await;

        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_update_rule() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                // get_by_id query
                vec![ip_access_control::Model {
                    id: 1,
                    ip_address: "192.168.1.100".to_string(),
                    action: "block".to_string(),
                    reason: Some("Old reason".to_string()),
                    created_by: Some(1),
                    created_at: now,
                    updated_at: now,
                }],
                // update query result
                vec![ip_access_control::Model {
                    id: 1,
                    ip_address: "192.168.1.100".to_string(),
                    action: "allow".to_string(),
                    reason: Some("Updated reason".to_string()),
                    created_by: Some(1),
                    created_at: now,
                    updated_at: chrono::Utc::now(),
                }],
            ])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));

        let request = UpdateIpAccessControlRequest {
            ip_address: None,
            action: Some("allow".to_string()),
            reason: Some("Updated reason".to_string()),
        };

        let result = service.update(1, request).await;
        assert!(result.is_ok());

        let rule = result.unwrap();
        assert_eq!(rule.action, "allow");
    }

    #[tokio::test]
    async fn test_delete_rule() {
        let now = chrono::Utc::now();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                // get_by_id query
                vec![ip_access_control::Model {
                    id: 1,
                    ip_address: "192.168.1.100".to_string(),
                    action: "block".to_string(),
                    reason: None,
                    created_by: None,
                    created_at: now,
                    updated_at: now,
                }],
            ])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.delete(1).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![Vec::<ip_access_control::Model>::new()]])
            .into_connection();

        let service = IpAccessControlService::new(Arc::new(db));
        let result = service.delete(999).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            IpAccessControlError::NotFound(_) => {}
            _ => panic!("Expected NotFound error"),
        }
    }
    */
}
