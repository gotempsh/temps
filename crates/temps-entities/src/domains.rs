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
