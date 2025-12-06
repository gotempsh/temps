use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Route type determines how the proxy matches incoming requests
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(10))")]
pub enum RouteType {
    /// Match on HTTP Host header (Layer 7) - default
    /// Works for both HTTP and HTTPS (uses Host header after TLS termination)
    #[sea_orm(string_value = "http")]
    Http,

    /// Match on TLS SNI hostname (Layer 4/5)
    /// Routes based on SNI before TLS termination - useful for TCP passthrough
    #[sea_orm(string_value = "tls")]
    Tls,
}

impl Default for RouteType {
    fn default() -> Self {
        RouteType::Http
    }
}

impl std::fmt::Display for RouteType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteType::Http => write!(f, "http"),
            RouteType::Tls => write!(f, "tls"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "custom_routes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub domain: String,
    pub host: String,
    pub port: i32,
    pub domain_id: Option<i32>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub enabled: bool,
    /// Route type: 'http' (default) or 'tls'
    /// - 'http': Match on HTTP Host header (Layer 7)
    /// - 'tls': Match on TLS SNI hostname (Layer 4/5)
    #[sea_orm(default_value = "http")]
    pub route_type: RouteType,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::domains::Entity",
        from = "Column::DomainId",
        to = "super::domains::Column::Id"
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
