use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "external_services")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub service_type: String,
    pub version: Option<String>,
    pub status: String,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub slug: Option<String>,
    /// Encrypted JSON configuration for the service
    pub config: Option<String>,
    /// Node this service runs on. NULL = local node (single-node mode).
    pub node_id: Option<i32>,
    /// Service topology: 'standalone' (single container) or 'cluster' (multiple members).
    #[sea_orm(default_value = "standalone")]
    pub topology: String,
    /// Error message from failed initialization (null if no error).
    pub error_message: Option<String>,
    /// Latest health-check result: "operational" | "degraded" | "down".
    /// NULL means the service has not yet been probed.
    pub health_status: Option<String>,
    /// When the last health probe ran.
    pub last_health_check_at: Option<DBDateTime>,
    /// Error message from the most recent failed probe (cleared on recovery).
    pub last_health_error: Option<String>,
    /// Consecutive failed probes. Used to suppress flapping alerts.
    #[sea_orm(default_value = 0)]
    pub consecutive_health_failures: i32,
    /// Engine-specific health snapshots keyed by signal name (e.g.,
    /// `postgres_wal`). Populated by the background health monitor.
    /// NULL means no probe has populated any signal yet.
    pub health_metadata: Option<Json>,
    /// Whether the MetricsScraper should collect DB-level metrics for this
    /// service (pg_stat_*, Redis INFO, etc.).  Defaults to false — operators
    /// opt in explicitly.  Added by migration
    /// `m20260601_000002_add_monitoring_settings`.
    #[sea_orm(default_value = false)]
    pub metrics_enabled: bool,
    /// Whether the auto-provisioning reconcile loop has already created a
    /// default daily full-backup schedule for this service. Acts as a one-shot
    /// latch: provisioning happens exactly once (when `false`), then this is
    /// set to `true` so we never recreate a schedule the operator later
    /// deletes. Added by migration
    /// `m20260623_000001_add_external_services_default_backup_provisioned`.
    #[sea_orm(default_value = false)]
    pub default_backup_provisioned: bool,
    /// Real Docker container name this service's logs are collected under.
    /// Plaintext (unlike the encrypted `config`) so the log collector can map
    /// a running container back to its service without decryption. Set for
    /// IMPORTED services to the pre-existing container's actual name (which
    /// carries no `temps.*` labels); NULL for Temps-created services, which
    /// the collector already resolves via the `temps.service_name` Docker
    /// label. Added by `m20260707_000002_add_external_services_container_name`.
    pub container_name: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::external_service_backups::Entity")]
    Backups,
    #[sea_orm(has_many = "super::project_services::Entity")]
    ProjectServices,
    #[sea_orm(has_many = "super::service_members::Entity")]
    Members,
    #[sea_orm(has_many = "super::backup_schedule_services::Entity")]
    BackupScheduleServices,
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id"
    )]
    Node,
}

impl Related<super::external_service_backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backups.def()
    }
}

impl Related<super::project_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ProjectServices.def()
    }
}

impl Related<super::service_members::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Members.def()
    }
}

impl Related<super::backup_schedule_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BackupScheduleServices.def()
    }
}

impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Node.def()
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
