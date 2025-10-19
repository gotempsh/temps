use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "tls_acme_certificates")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub domain: String,
    pub certificate: String,
    pub private_key: String,
    pub expires_at: DBDateTime,
    pub issued_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}