// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Domain status: certificate present and serving normally.
pub const STATUS_ACTIVE: &str = "active";

/// Domain status: a valid certificate is still present and MUST keep being served,
/// but the most recent renewal attempt failed. This is a degraded-but-serving state
/// so operators can be alerted before the existing certificate actually expires.
pub const STATUS_ACTIVE_RENEWAL_FAILED: &str = "active_renewal_failed";

/// The set of domain statuses for which a stored certificate should be served by the
/// proxy / synced to edge nodes. Any query that loads certificates for serving MUST
/// filter on this set (not a bare `= "active"`), otherwise a domain with a valid cert
/// whose renewal failed would silently stop serving HTTPS.
pub const CERT_SERVING_STATUSES: [&str; 2] = [STATUS_ACTIVE, STATUS_ACTIVE_RENEWAL_FAILED];

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "domains")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub domain: String,
    pub certificate: Option<String>,
    #[serde(skip_serializing)]
    pub private_key: Option<String>,
    pub expiration_time: Option<DBDateTime>,
    pub last_renewed: Option<DBDateTime>,
    pub status: String,
    pub dns_challenge_token: Option<String>,
    pub dns_challenge_value: Option<String>,
    pub http_challenge_token: Option<String>,
    pub http_challenge_key_authorization: Option<String>,
    pub last_error: Option<String>,
    pub last_error_type: Option<String>,
    pub is_wildcard: bool,
    pub verification_method: String,
    /// On-demand TLS negative cache (ADR-018 §4 Layer 2). When a hostname's
    /// on-demand issuance fails, this is set to `now + exponential_delay`; the
    /// proxy's `certificate_callback` refuses to re-enqueue a job for the same
    /// hostname until this timestamp elapses. `None` means "no active backoff".
    pub on_demand_backoff_until: Option<DBDateTime>,
    /// On-demand issuance failures in a row, which picks the next rung of the
    /// backoff ladder. Written in the same update as `on_demand_backoff_until`
    /// and reset to 0 when a certificate is issued or imported.
    pub on_demand_failure_streak: i32,
    /// When the last on-demand failure in the streak happened; a streak older
    /// than the quiet gap starts over. `None` when there is no streak.
    pub on_demand_last_failure_at: Option<DBDateTime>,
    /// Why the last certificate attempt failed, as a typed cause
    /// (`unreachable`, `dns_lookup`, `upstream_answered`, `rate_limited`, ...;
    /// `temps_domains::CertFailureCause`). Cleared when a certificate is
    /// issued or imported, or a new order starts. `None` for rows that have not failed
    /// since the column was added.
    pub failure_cause: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::custom_routes::Entity")]
    CustomRoutes,
    #[sea_orm(has_many = "super::project_custom_domains::Entity")]
    ProjectCustomDomains,
}

impl Related<super::custom_routes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CustomRoutes.def()
    }
}

impl Related<super::project_custom_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ProjectCustomDomains.def()
    }
}

impl ActiveModel {
    /// Forget why earlier certificate attempts failed, for a domain that now
    /// holds a certificate (issued by the CA or imported): the error and its
    /// typed cause, and the on-demand backoff and failure streak, so a later failure starts the
    /// ladder from the bottom. Every path that installs a certificate calls
    /// this, so a new failure field cannot be cleared on one and missed on
    /// another.
    pub fn clear_certificate_failure(&mut self) {
        self.last_error = Set(None);
        self.last_error_type = Set(None);
        self.on_demand_backoff_until = Set(None);
        self.on_demand_failure_streak = Set(0);
        self.on_demand_last_failure_at = Set(None);
        self.failure_cause = Set(None);
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializing_a_domain_leaves_out_its_private_key() {
        let now = chrono::Utc::now();
        let domain = Model {
            id: 1,
            domain: "app.example.com".to_string(),
            certificate: None,
            private_key: Some("stored-private-key".to_string()),
            expiration_time: None,
            last_renewed: None,
            status: "active".to_string(),
            dns_challenge_token: None,
            dns_challenge_value: None,
            http_challenge_token: None,
            http_challenge_key_authorization: None,
            last_error: None,
            last_error_type: None,
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            on_demand_backoff_until: None,
            on_demand_failure_streak: 0,
            on_demand_last_failure_at: None,
            failure_cause: None,
            created_at: now,
            updated_at: now,
        };

        let json = serde_json::to_string(&domain).expect("serializes");

        assert!(!json.contains("stored-private-key"), "{json}");
        assert!(json.contains("app.example.com"), "{json}");
    }

    #[test]
    fn clearing_certificate_failure_resets_the_error_its_cause_and_the_on_demand_streak() {
        let now = chrono::Utc::now();
        let failed = Model {
            id: 7,
            domain: "failed.example.com".to_string(),
            certificate: None,
            private_key: None,
            expiration_time: None,
            last_renewed: None,
            status: "on_demand_failed".to_string(),
            dns_challenge_token: None,
            dns_challenge_value: None,
            http_challenge_token: None,
            http_challenge_key_authorization: None,
            last_error: Some("connection refused".to_string()),
            last_error_type: Some("challenge_mismatch".to_string()),
            is_wildcard: false,
            verification_method: "http-01".to_string(),
            on_demand_backoff_until: Some(now + chrono::Duration::hours(4)),
            on_demand_failure_streak: 4,
            on_demand_last_failure_at: Some(now),
            failure_cause: Some("unreachable".to_string()),
            created_at: now,
            updated_at: now,
        };

        let mut active: ActiveModel = failed.into();
        active.clear_certificate_failure();

        assert_eq!(active.last_error, Set(None));
        assert_eq!(active.last_error_type, Set(None));
        assert_eq!(active.on_demand_backoff_until, Set(None));
        assert_eq!(active.on_demand_failure_streak, Set(0));
        assert_eq!(active.on_demand_last_failure_at, Set(None));
        assert_eq!(active.failure_cause, Set(None));
        // Only failure state: the status and the rest are the caller's.
        assert_eq!(
            active.status,
            sea_orm::ActiveValue::Unchanged("on_demand_failed".to_string())
        );
    }
}
