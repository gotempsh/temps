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
                    warn!("Error getting ip address {:?}: {}", ip_address, err);
                    None
                }
            },
            None => None,
        };

        // Serialize the operation to JSON
        let data_json = operation.serialize()?;

        let new_audit_log = audit_logs::ActiveModel {
            user_id: Set(Some(operation.user_id())),
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

/// Errors from the audit `data` PII scrub operation.
#[derive(Debug, thiserror::Error)]
pub enum AuditScrubError {
    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Database error while scrubbing audit data: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// Result of a PII scrub pass over `audit_logs.data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubOutcome {
    /// Rows whose `data` payload was inspected.
    pub rows_scanned: u64,
    /// Rows whose `data` payload had at least one value redacted.
    pub rows_scrubbed: u64,
}

/// Marker written in place of a redacted value.
pub const REDACTION_MARKER: &str = "[REDACTED]";

/// Minimum identifier length accepted by the scrub — redacting very short
/// strings would rewrite unrelated payload values wholesale.
const MIN_SCRUB_IDENTIFIER_LEN: usize = 3;

/// Replace every JSON string value that exactly equals one of `targets`
/// (case-insensitive) with [`REDACTION_MARKER`], recursing through objects
/// and arrays. Returns the number of values replaced.
///
/// Matching is by full value only — identifiers embedded inside longer
/// strings are not detected. `targets` must already be lowercased.
fn redact_matching_values(value: &mut serde_json::Value, targets: &[String]) -> u64 {
    match value {
        serde_json::Value::String(s) => {
            if targets.iter().any(|t| s.to_lowercase() == *t) {
                *value = serde_json::Value::String(REDACTION_MARKER.to_string());
                1
            } else {
                0
            }
        }
        serde_json::Value::Object(map) => map
            .values_mut()
            .map(|v| redact_matching_values(v, targets))
            .sum(),
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|v| redact_matching_values(v, targets))
            .sum(),
        _ => 0,
    }
}

impl AuditService {
    /// Redact the given identifier values (a deleted user's email, username,
    /// name) from every `audit_logs.data` payload, in place. The structural
    /// record — row, operation type, timestamps, non-matching context — is
    /// preserved; only string values exactly matching an identifier become
    /// [`REDACTION_MARKER`]. This covers both rows the user authored and rows
    /// *about* them (e.g. the deletion event recorded under the acting
    /// admin's identity).
    ///
    /// Explicit operator action for erasure requests — never invoked
    /// automatically. Callers are responsible for auditing the scrub itself.
    ///
    /// Content-integrity plugins that fingerprint audit payloads will flag
    /// scrubbed rows as modified; that is the honest signal for an in-place
    /// redaction and is documented at the API layer.
    pub async fn scrub_pii_values(
        &self,
        identifiers: Vec<String>,
    ) -> Result<ScrubOutcome, AuditScrubError> {
        let targets: Vec<String> = identifiers
            .iter()
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .collect();

        if targets.is_empty() {
            return Err(AuditScrubError::Validation {
                message: "At least one identifier value to redact is required".to_string(),
            });
        }
        if let Some(short) = targets.iter().find(|v| v.len() < MIN_SCRUB_IDENTIFIER_LEN) {
            return Err(AuditScrubError::Validation {
                message: format!(
                    "Identifier of length {} is too short to scrub safely (minimum {} characters)",
                    short.len(),
                    MIN_SCRUB_IDENTIFIER_LEN
                ),
            });
        }

        let mut rows_scanned = 0u64;
        let mut rows_scrubbed = 0u64;

        // Page by ascending id so concurrent inserts (which get higher ids)
        // cannot shift earlier pages under the scan.
        let mut pages = temps_entities::audit_logs::Entity::find()
            .order_by_asc(temps_entities::audit_logs::Column::Id)
            .paginate(self.db.as_ref(), 500);

        while let Some(batch) = pages.fetch_and_next().await? {
            for row in batch {
                rows_scanned += 1;

                let mut payload: serde_json::Value = match serde_json::from_str(&row.data) {
                    Ok(v) => v,
                    Err(e) => {
                        // Non-JSON payloads can't be selectively redacted;
                        // leave them intact rather than destroying the record.
                        warn!(
                            audit_log_id = row.id,
                            "Skipping audit row with unparsable data payload during scrub: {e}"
                        );
                        continue;
                    }
                };

                if redact_matching_values(&mut payload, &targets) == 0 {
                    continue;
                }

                let serialized =
                    serde_json::to_string(&payload).map_err(|e| AuditScrubError::Validation {
                        message: format!(
                            "Failed to re-serialize scrubbed payload for audit row {}: {e}",
                            row.id
                        ),
                    })?;

                let mut active: temps_entities::audit_logs::ActiveModel = row.into();
                active.data = Set(serialized);
                active.update(self.db.as_ref()).await?;
                rows_scrubbed += 1;
            }
        }

        Ok(ScrubOutcome {
            rows_scanned,
            rows_scrubbed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
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

    // ── PII scrub ─────────────────────────────────────────────────────────

    #[test]
    fn redact_replaces_exact_matches_recursively() {
        let mut payload = serde_json::json!({
            "email": "Jane@Example.com",
            "nested": { "username": "jane.doe", "role": "admin" },
            "list": ["jane.doe", "other"],
            "count": 3,
            "message": "sent to jane@example.com yesterday"
        });
        let targets = vec!["jane@example.com".to_string(), "jane.doe".to_string()];

        let replaced = redact_matching_values(&mut payload, &targets);

        // Case-insensitive full-value matches only: the email field, the
        // nested username, and the array element — not the substring inside
        // "message", and never non-string values.
        assert_eq!(replaced, 3);
        assert_eq!(payload["email"], REDACTION_MARKER);
        assert_eq!(payload["nested"]["username"], REDACTION_MARKER);
        assert_eq!(payload["nested"]["role"], "admin");
        assert_eq!(payload["list"][0], REDACTION_MARKER);
        assert_eq!(payload["list"][1], "other");
        assert_eq!(payload["count"], 3);
        assert_eq!(payload["message"], "sent to jane@example.com yesterday");
    }

    #[tokio::test]
    async fn test_scrub_rejects_empty_identifier_list() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = service_with(db);

        let result = service.scrub_pii_values(vec!["   ".to_string()]).await;

        assert!(matches!(
            result.unwrap_err(),
            AuditScrubError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_scrub_rejects_too_short_identifier() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = service_with(db);

        let result = service
            .scrub_pii_values(vec!["jane@example.com".to_string(), "ab".to_string()])
            .await;

        assert!(matches!(
            result.unwrap_err(),
            AuditScrubError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_scrub_updates_only_matching_rows() {
        fn row(id: i32, data: &str) -> audit_logs::Model {
            audit_logs::Model {
                id,
                data: data.to_string(),
                ..log_row(Some(1))
            }
        }

        let matching = row(1, r#"{"email":"jane@example.com"}"#);
        let scrubbed = row(1, r#"{"email":"[REDACTED]"}"#);
        let unrelated = row(2, r#"{"email":"other@example.com"}"#);
        let unparsable = row(3, "not-json");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Page 1: three rows (one match, one non-match, one unparsable).
            .append_query_results([vec![matching, unrelated, unparsable]])
            // UPDATE ... RETURNING for the matching row.
            .append_query_results([vec![scrubbed]])
            // Page 2: empty — ends the scan.
            .append_query_results([Vec::<audit_logs::Model>::new()])
            .into_connection();
        let service = service_with(db);

        let outcome = service
            .scrub_pii_values(vec!["jane@example.com".to_string()])
            .await
            .expect("scrub should succeed");

        assert_eq!(outcome.rows_scanned, 3);
        assert_eq!(outcome.rows_scrubbed, 1);
    }
}
