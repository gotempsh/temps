use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Standalone sandboxes created via the `/v1/sandbox` API — the temps
/// counterpart to `@vercel/sandbox`. Separate from workspace sessions
/// (which add chat + AI provider on top of a sandbox) and agent runs
/// (which add multi-phase workflow + PR creation).
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sandboxes")]
pub struct Model {
    /// Monotonic internal ID. Used to key into the underlying
    /// `SandboxProvider`'s handle map (it expects `i32` for historical
    /// reasons — the agent-runs code paths use `run_id` there).
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Opaque public identifier surfaced to API callers (e.g.
    /// `sbx_a1b2c3d4e5f6`). Never expose `id` in responses.
    #[sea_orm(unique)]
    pub public_id: String,

    /// Owner of the sandbox. All sandbox operations require the
    /// authenticated user to match this column (or have admin override).
    pub user_id: i32,

    /// Container name used by the sandbox provider.
    pub name: String,

    /// Lifecycle state: "running" | "stopped" | "destroyed".
    /// A "destroyed" row is kept for audit/listing purposes but the
    /// underlying container is gone.
    pub status: String,

    /// Optional Docker image override. When null, the platform default
    /// is used (the same image agent-runs use).
    pub image: Option<String>,

    /// Absolute path inside the container where the caller's working
    /// directory is rooted. Defaults to `/workspace`.
    pub work_dir: String,

    /// Timeout in seconds before the sandbox is considered idle and
    /// eligible for teardown by the periodic sweeper. `extend_timeout`
    /// pushes `expires_at` forward by this many seconds.
    pub timeout_secs: i32,

    /// Resource + network config as JSON. Optional, falls back to
    /// provider defaults.
    #[sea_orm(column_type = "JsonBinary")]
    pub metadata: Option<serde_json::Value>,

    /// Isolation backend the sandbox actually runs on: "docker" or
    /// "firecracker". Recorded at create time from the effective backend
    /// the provider chose (which may be the host default when the request
    /// omitted one). `None` on rows created before this column existed.
    pub backend: Option<String>,

    pub created_at: DBDateTime,
    pub last_activity_at: DBDateTime,
    pub expires_at: DBDateTime,

    /// Optional argon2 PHC hash of a user-supplied password that protects
    /// the sandbox's preview URLs. When null, the gateway allows any
    /// request that can reach the sandbox's unguessable hex hostname.
    /// When set, Pingora presents a login form and expects a cookie
    /// minted against this hash.
    pub preview_password_hash: Option<String>,

    /// Last 4 characters of the plaintext password. Safe to surface in
    /// the UI so users can tell two passwords apart without storing the
    /// full plaintext anywhere.
    pub preview_password_hint: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_delete = "Cascade"
    )]
    User,
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
