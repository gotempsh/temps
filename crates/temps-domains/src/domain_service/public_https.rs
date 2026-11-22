// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Public ingress diagnostics live on the domain alongside its certificate.
//! Reads use a narrow entity so the status path never loads its private key.

use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_core::public_https::PublicHttpsResult;
use temps_entities::domain_public_https as observation;
use thiserror::Error;

use super::DomainService;

#[derive(Debug, Error)]
pub enum ObservationError {
    #[error("Domain {id} was not found while reading public HTTPS status")]
    NotFound { id: i32 },
    #[error("Could not read public HTTPS status for domain {id}: {source}")]
    Read { id: i32, source: sea_orm::DbErr },
    #[error("Could not record public HTTPS status for domain {id}: {source}")]
    Write { id: i32, source: sea_orm::DbErr },
}

impl DomainService {
    pub async fn public_https_observation(
        &self,
        id: i32,
    ) -> Result<observation::Model, ObservationError> {
        observation::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ObservationError::Read { id, source })?
            .ok_or(ObservationError::NotFound { id })
    }

    /// Only replace an observation with one that began later. A slower
    /// probe must not overwrite the result of a subsequent invocation.
    pub async fn record_public_https(
        &self,
        id: i32,
        started_at: DateTime<Utc>,
        result: PublicHttpsResult,
    ) -> Result<observation::Model, ObservationError> {
        use sea_orm::{ActiveValue::Set, Condition};
        let change = observation::ActiveModel {
            public_https_checked_at: Set(Some(started_at)),
            public_https_result: Set(Some(observation::StoredPublicHttpsResult(result))),
            ..Default::default()
        };
        observation::Entity::update_many()
            .set(change)
            .filter(observation::Column::Id.eq(id))
            .filter(
                Condition::any()
                    .add(observation::Column::PublicHttpsCheckedAt.is_null())
                    .add(observation::Column::PublicHttpsCheckedAt.lt(started_at)),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|source| ObservationError::Write { id, source })?;
        self.public_https_observation(id).await
    }
}
