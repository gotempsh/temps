use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "log_chunks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: i32,
    /// Set when this chunk belongs to an imported/managed external service
    /// (Postgres, MariaDB, Redis, MongoDB, MinIO, …) rather than a deployment.
    /// External-service chunks store `project_id = 0` (sentinel) and key on
    /// this instead — a service isn't owned by a single project.
    pub external_service_id: Option<i32>,
    pub env: String,
    pub service: String,
    pub container_id: String,
    pub deploy_id: Option<i32>,
    /// Worker node this chunk's container ran on. `NULL` = control-plane-local
    /// container (collected via the local Docker daemon). `Some` = a remote
    /// worker node, collected by the remote log collector over mTLS.
    pub node_id: Option<i32>,
    /// Human-readable node name, denormalized at write time so history results
    /// can display the source node without a join.
    pub node_name: Option<String>,
    pub started_at: DBDateTime,
    pub ended_at: DBDateTime,
    pub storage_key: String,
    pub line_count: i32,
    pub compressed_size_bytes: i32,
    pub has_errors: bool,
    /// Byte offset of every 100th line (uncompressed) for partial retrieval
    pub line_offsets: Vec<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
