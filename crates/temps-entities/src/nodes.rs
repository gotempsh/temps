use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "nodes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub token_hash: String,
    /// Encrypted plaintext token (AES-256-GCM via EncryptionService)
    /// Used by control plane to authenticate with the agent for remote deployments
    pub token_encrypted: Option<String>,
    /// Agent API address, e.g. "https://203.0.113.50:3100"
    pub address: String,
    /// WireGuard IP or user-provided private address, e.g. "10.100.0.2"
    pub private_address: String,
    /// WireGuard endpoint for peer connections, e.g. "203.0.113.50:51820"
    pub public_endpoint: Option<String>,
    /// WireGuard public key (base64-encoded, 44 chars)
    pub wg_public_key: Option<String>,
    /// "worker" or "control"
    pub role: String,
    /// "pending", "active", "offline", "draining"
    pub status: String,
    /// Arbitrary key-value labels for scheduling, e.g. {"region": "us-east"}
    pub labels: serde_json::Value,
    /// Resource capacity metrics from heartbeats
    pub capacity: serde_json::Value,
    pub last_heartbeat: Option<DBDateTime>,
    /// X25519 public key for edge nodes (base64-encoded, used for ECIES cert encryption)
    pub edge_public_key: Option<String>,
    /// Per-node CIDR for the multi-host overlay (e.g. "172.20.5.0/24"). Other
    /// nodes route this CIDR to us via the configured transport. Allocated by
    /// the control-plane `ComputeNetworkAllocator` when the node joins.
    /// Stored as text to mirror `private_address` / `public_endpoint`; parsed
    /// to `ipnet::Ipv4Net` at the application boundary.
    pub compute_cidr: Option<String>,
    /// Address other nodes use to reach this one over the underlay. Cloud
    /// private IP for same-DC clusters, public IP for cross-DC. Parsed to
    /// `std::net::IpAddr` at the application boundary.
    pub underlay_address: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::deployment_containers::Entity")]
    DeploymentContainers,
    #[sea_orm(has_many = "super::external_services::Entity")]
    ExternalServices,
}

impl Related<super::deployment_containers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::DeploymentContainers.def()
    }
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalServices.def()
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
