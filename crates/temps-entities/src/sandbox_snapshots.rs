// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Entity for the `sandbox_snapshots` table (ADR-037).
//!
//! A snapshot is backend-specific filesystem state stored as a
//! content-addressed artifact on the host filesystem. Docker stores an image
//! tarball plus a workspace archive; Firecracker rebuilds a sanitized ext4
//! filesystem. See the ADR for the credential-scrubbing and deduplication design.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Status state machine for a sandbox snapshot.
///
/// Transitions:
/// - `creating` → `ready` (artifact written and digest verified)
/// - `creating` → `failed` (provider error or scrub failure)
/// - `ready` → `deleted` (explicit DELETE; tarball removed if no other ready
///   row shares the `content_digest`)
///
/// `failed` rows are kept for audit; `deleted` rows use soft-delete so the
/// audit trail is preserved. Hard deletes are not used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotStatus {
    Creating,
    Ready,
    Failed,
    Deleted,
}

impl SnapshotStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for SnapshotStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SnapshotStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "creating" => Ok(Self::Creating),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            "deleted" => Ok(Self::Deleted),
            other => Err(format!("unknown snapshot status '{}'", other)),
        }
    }
}

/// A snapshot of a sandbox's filesystem state (ADR-037).
///
/// Produced by `docker commit` + workspace export for Docker sandboxes, or by
/// rebuilding a sanitized ext4 filesystem for Firecracker microVMs.
/// Credentials are scrubbed before capture (see ADR-037 §4).
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sandbox_snapshots")]
pub struct Model {
    /// Monotonic internal ID.
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Opaque public identifier (e.g. `snap_a1b2c3d4e5f6`). Never expose `id`.
    #[sea_orm(unique)]
    pub public_id: String,

    /// Owner of the snapshot. Cascades on user delete.
    pub user_id: i32,

    /// Project this snapshot is attributed to. Nullable, no FK (mirrors
    /// `sandboxes.project_id` — deleting a project must not destroy snapshots).
    pub project_id: Option<i32>,

    /// The sandbox this was snapped from. Nullable, no FK — the source sandbox
    /// may be destroyed after snapping. The sandbox destroy path sets this to
    /// NULL when it removes the sandbox row.
    pub source_sandbox_id: Option<i32>,

    /// Optional human-readable label.
    pub label: Option<String>,

    /// Lifecycle status: `creating` | `ready` | `failed` | `deleted`.
    pub status: String,

    /// Which backend produced this artifact: `docker` | `firecracker`.
    /// Cross-backend restore is rejected with 422.
    pub backend: String,

    /// SHA-256 of the complete logical snapshot — the canonical dedup key.
    /// Two rows with the same `content_digest` share artifacts on disk. The
    /// partial index `idx_sandbox_snapshots_digest_ready` accelerates lookup
    /// while allowing distinct user-owned rows to share one artifact.
    pub content_digest: String,

    /// Absolute path to the primary artifact on the host filesystem.
    pub content_path: String,

    /// Approximate total size of the primary and companion artifacts in bytes.
    pub size_bytes: i64,

    /// For Docker: the daemon image tag `temps-snapshot/<public_id>:latest`.
    /// Used to load the snapshot back into the daemon on restore if the image
    /// is not already present. `None` for non-Docker backends.
    pub image_ref: Option<String>,

    /// Backend-specific fields that don't warrant a migration. Docker stores
    /// workspace companion artifact metadata here.
    #[sea_orm(column_type = "JsonBinary")]
    pub metadata: Option<serde_json::Value>,

    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
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
