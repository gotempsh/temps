// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "external_plugin_actors")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub plugin_name: String,
    pub binary_sha256: String,
    pub source_identity: String,
    pub active: bool,
    /// Administrator consent for marked AI tools; false for existing and new actors.
    pub ai_tools_enabled: bool,
    /// How the plugin is isolated: `service` when it runs as a Temps service,
    /// `container` (the default for a new actor installed as an executable)
    /// or `process`. Actors that existed before the column keep `process`.
    pub isolation: String,
    /// The service a `service` plugin runs as. NULL for other plugins, and
    /// once that service has been deleted.
    pub project_id: Option<i32>,
    /// The environment of that service the plugin runs in.
    pub environment_id: Option<i32>,
    /// The manifest a `service` plugin last answered with, so it can be
    /// listed and routed to while its service is stopped.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub manifest: Option<Json>,
    /// Who stands behind the plugin: `community` (anyone; the least access)
    /// or `verified` (reviewed, from the signed registry).
    pub tier: String,
    /// Identifies the exact build an administrator approved. Temps attaches
    /// to no other build of the plugin's service. NULL until the plugin is
    /// first attached to.
    pub pinned_build: Option<String>,
    /// The deployment that build came from, for display.
    pub pinned_deployment_id: Option<i32>,
    /// A build the service runs that is not the approved one and waits for
    /// an administrator. NULL when no update is waiting.
    pub pending_build: Option<String>,
    pub pending_deployment_id: Option<i32>,
    /// The manifest the waiting build answered with.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub pending_manifest: Option<Json>,
    /// What an approval must quote: the waiting build and its manifest.
    pub pending_review: Option<String>,
    pub pending_since: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
