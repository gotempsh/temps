use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "revenue_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: i32,
    pub integration_id: i32,
    pub provider: String,
    pub provider_event_id: String,
    pub event_type: String,
    pub customer_ref: Option<String>,
    pub subscription_ref: Option<String>,
    pub subscription_status: Option<String>,
    pub mrr_minor: Option<i64>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub occurred_at: DBDateTime,
    /// Raw provider payload for audit/debug. Never returned over the
    /// public API.
    #[serde(skip_serializing)]
    pub payload: serde_json::Value,
    pub created_at: DBDateTime,
    /// Opaque SKU/price reference (Stripe price_id, LemonSqueezy variant_id, …).
    /// NULL for events that have no natural price association (charges,
    /// customer.created) or when the provider didn't populate one.
    pub price_id: Option<String>,
    /// Opaque product reference (Stripe product_id, …). NULL mirrors `price_id`.
    pub product_id: Option<String>,

    /// Deployment that produced this revenue event, when the SDK supplied
    /// the `X-Temps-Deployment-Id` header. NULL for webhooks delivered
    /// outside a request context.
    pub deployment_id: Option<i32>,

    /// Environment that produced this revenue event, when known via the
    /// SDK header.
    pub environment_id: Option<i32>,

    /// OTel trace_id from the originating request, when supplied by the
    /// SDK header. Lets the Observe view link a payment to the request that
    /// triggered it.
    pub trace_id: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
