// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable per-host TLS authorization records for Traefik-discovered routes.
//!
//! Rows here outlive the container whose route they authorize — by design.
//! `traefik_discovered_routes` rows are deleted when the container stops;
//! this table survives container replacement so authorization is not silently
//! revoked by `docker compose restart`.
//!
//! See ADR-041 §2 for the full design rationale, and §2a for the container
//! identity / drift-detection columns.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "traefik_route_certificates")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Normalized (lowercased) hostname. The natural key for the authorization.
    #[sea_orm(unique)]
    pub host: String,

    /// Whether the operator has explicitly authorized TLS for this host.
    ///
    /// `enabled` (on `traefik_discovered_routes`) answers "route HTTP traffic".
    /// `cert_authorized` answers "the operator accepts responsibility for a
    /// certificate". Kept separate, deliberately.
    pub cert_authorized: bool,

    /// When the authorization was granted.
    pub authorized_at: Option<DBDateTime>,

    /// Which user granted the authorization. Nullable: ON DELETE SET NULL so
    /// a deleted user does not delete the authorization record.
    pub authorized_by_user_id: Option<i32>,

    /// Discovery network at authorization time. Repointing
    /// `TEMPS_TRAEFIK_DISCOVERY_NETWORK` makes *new* operations reject against
    /// the old network; existing certs keep serving (the cert loader ignores
    /// this table entirely).
    pub authorized_network: String,

    /// Full Docker container ID of the container that held the host at
    /// authorization time. Used for drift detection (ADR-041 §2a).
    pub authorized_container_id: String,

    /// Docker container name at authorization time.
    pub authorized_container_name: String,

    /// Set when the currently-serving container differs from the authorized
    /// identity. Cleared only by explicit operator re-authorization or
    /// deauthorization — **never** auto-cleared, because auto-clearing would
    /// not remove the certificate and would be a DoS primitive (ADR-041 §2a).
    pub container_drift_detected_at: Option<DBDateTime>,

    /// The container ID that was already alarmed for drift on this host.
    /// Without this column the drift alarm re-fires every reconcile pass for
    /// the same container, which is noisy and useless. Compare the current
    /// container ID against this value before firing; only fire and update when
    /// the value differs.
    pub last_drift_alarmed_container_id: Option<String>,

    /// Challenge type used for renewal. CHECK-constrained to `http-01` or
    /// `dns-01` — the two values the renewal scheduler understands. A third
    /// value would produce a cert that is never renewed; the constraint makes
    /// that unrepresentable.
    pub renewal_method: String,

    /// How the certificate was obtained.
    /// - `"acme"`: operator-triggered ACME issuance (Path A).
    /// - `"imported"`: operator imported Traefik's `acme.json` (Path B).
    pub source: String,

    /// FK to the `domains` row holding the actual certificate material.
    /// Nullable while issuance is in flight; ON DELETE SET NULL so a domain
    /// deletion does not cascade to this authorization.
    pub certificate_id: Option<i32>,

    /// Timestamp of the last successful import (Path B only).
    pub imported_at: Option<DBDateTime>,

    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::domains::Entity",
        from = "Column::CertificateId",
        to = "super::domains::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Domain,
}

impl Related<super::domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Domain.def()
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
