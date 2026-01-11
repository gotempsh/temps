use anyhow::Result;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use temps_entities::challenge_sessions;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ChallengeError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Challenge not found")]
    NotFound,
    #[error("Challenge expired")]
    Expired,
}

pub struct ChallengeService {
    db: Arc<DatabaseConnection>,
}

impl ChallengeService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Check if a challenge has been completed for the given environment and identifier
    /// Returns true if a valid (non-expired) challenge session exists
    pub async fn is_challenge_completed(
        &self,
        environment_id: i32,
        identifier: &str,
        identifier_type: &str,
    ) -> Result<bool, ChallengeError> {
        let now = Utc::now();

        let session = challenge_sessions::Entity::find()
            .filter(challenge_sessions::Column::EnvironmentId.eq(environment_id))
            .filter(challenge_sessions::Column::Identifier.eq(identifier))
            .filter(challenge_sessions::Column::IdentifierType.eq(identifier_type))
            .filter(challenge_sessions::Column::ExpiresAt.gt(now))
            .one(self.db.as_ref())
            .await?;

        Ok(session.is_some())
    }

    /// Mark a challenge as completed for the given environment and identifier
    /// Challenge sessions expire after 24 hours by default
    pub async fn mark_challenge_completed(
        &self,
        environment_id: i32,
        identifier: &str,
        identifier_type: &str,
        user_agent: Option<String>,
        ttl_hours: i64,
    ) -> Result<challenge_sessions::Model, ChallengeError> {
        let now = Utc::now();
        let expires_at = now + Duration::hours(ttl_hours);

        // Check if a session already exists (update it if so)
        if let Some(existing) = challenge_sessions::Entity::find()
            .filter(challenge_sessions::Column::EnvironmentId.eq(environment_id))
            .filter(challenge_sessions::Column::Identifier.eq(identifier))
            .filter(challenge_sessions::Column::IdentifierType.eq(identifier_type))
            .one(self.db.as_ref())
            .await?
        {
            let mut active: challenge_sessions::ActiveModel = existing.into();
            active.completed_at = Set(now);
            active.expires_at = Set(expires_at);
            if let Some(ua) = user_agent {
                active.user_agent = Set(Some(ua));
            }
            let updated = active.update(self.db.as_ref()).await?;
            return Ok(updated);
        }

        // Create new session
        let session = challenge_sessions::ActiveModel {
            environment_id: Set(environment_id),
            identifier: Set(identifier.to_string()),
            identifier_type: Set(identifier_type.to_string()),
            user_agent: Set(user_agent),
            completed_at: Set(now),
            expires_at: Set(expires_at),
            ..Default::default()
        };

        let result = session.insert(self.db.as_ref()).await?;
        Ok(result)
    }

    /// Clear expired challenge sessions for cleanup
    pub async fn clear_expired_sessions(&self) -> Result<u64, ChallengeError> {
        let now = Utc::now();

        let result = challenge_sessions::Entity::delete_many()
            .filter(challenge_sessions::Column::ExpiresAt.lt(now))
            .exec(self.db.as_ref())
            .await?;

        Ok(result.rows_affected)
    }

    /// Clear all challenge sessions for a specific environment (when disabling attack mode)
    pub async fn clear_environment_sessions(
        &self,
        environment_id: i32,
    ) -> Result<u64, ChallengeError> {
        let result = challenge_sessions::Entity::delete_many()
            .filter(challenge_sessions::Column::EnvironmentId.eq(environment_id))
            .exec(self.db.as_ref())
            .await?;

        Ok(result.rows_affected)
    }
}
