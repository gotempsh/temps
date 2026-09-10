use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "domain_delivery_previews")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: i32,
    pub actor_user_id: i32,
    pub request: Json,
    pub plan: Json,
    pub config_fingerprint: String,
    pub status: String,
    pub last_error: Option<String>,
    pub expires_at: DBDateTime,
    pub created_at: DBDateTime,
    pub applied_at: Option<DBDateTime>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
