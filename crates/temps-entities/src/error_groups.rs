use sea_orm::entity::prelude::*;
use sea_orm::DeriveValueType;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

// Custom type for pgvector to handle vector columns
// Uses DeriveValueType which handles the conversion automatically
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, DeriveValueType)]
pub struct PgVector(pub Vec<f32>);

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "error_groups")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    // Group identification
    pub title: String,
    pub error_type: String,
    pub message_template: Option<String>,

    // Vector embedding for similarity search using pgvector extension
    // Stored as vector(384) in PostgreSQL using custom PgVector type
    // select_as tells SeaORM to read it as FLOAT4[] array from PostgreSQL
    #[sea_orm(nullable, select_as = "FLOAT4[]")]
    pub embedding: Option<PgVector>,

    // Timestamps and metrics
    pub first_seen: DBDateTime,
    pub last_seen: DBDateTime,
    pub total_count: i32,

    // Status management
    pub status: String, // 'unresolved', 'resolved', 'ignored', 'assigned'
    pub assigned_to: Option<String>, // User ID or email

    // Relations
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,

    // Metadata
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Projects,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Environments,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Deployments,
    #[sea_orm(
        belongs_to = "super::visitor::Entity",
        from = "Column::VisitorId",
        to = "super::visitor::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Visitor,
    #[sea_orm(has_many = "super::error_events::Entity")]
    ErrorEvents,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environments.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployments.def()
    }
}

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl Related<super::error_events::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ErrorEvents.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Tokenize the error message using the provided tokenizer
    pub fn tokenize_message(
        &self,
        tokenizer: &dyn crate::tokenizer::Tokenizer,
    ) -> Result<Vec<u32>, crate::tokenizer::TokenizerError> {
        let text = self.message_template.as_deref().unwrap_or(&self.title);
        tokenizer.encode(text)
    }

    /// Create a vector embedding from tokenized message
    /// This converts token IDs to a fixed-size vector representation
    pub fn create_embedding_from_tokens(
        tokens: &[u32],
        embedding_size: usize,
    ) -> PgVector {
        // Simple approach: use token frequency as features
        let mut embedding = vec![0.0; embedding_size];

        for &token in tokens {
            let idx = (token as usize) % embedding_size;
            embedding[idx] += 1.0;
        }

        // Normalize
        let sum: f32 = embedding.iter().sum();
        if sum > 0.0 {
            for val in &mut embedding {
                *val /= sum;
            }
        }

        PgVector(embedding)
    }

    /// Convenience method to tokenize and create embedding in one step
    pub fn tokenize_and_embed(
        &self,
        tokenizer: &dyn crate::tokenizer::Tokenizer,
        embedding_size: usize,
    ) -> Result<PgVector, crate::tokenizer::TokenizerError> {
        let tokens = self.tokenize_message(tokenizer)?;
        Ok(Self::create_embedding_from_tokens(&tokens, embedding_size))
    }
}