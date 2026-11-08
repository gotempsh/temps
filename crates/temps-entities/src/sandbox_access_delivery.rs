// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable, secret-free worker commands. Deliberately independent of foreign
//! keys: a retirement command must outlive deletion of its sandbox/project.
use sea_orm::entity::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum DeliveryOperation {
    #[sea_orm(string_value = "reconcile")]
    Reconcile,
    #[sea_orm(string_value = "retire")]
    Retire,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sandbox_access_delivery")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub runtime_id: Uuid,
    pub node_id: Option<i32>,
    pub desired_version: i64,
    pub operation: DeliveryOperation,
    pub attempts: i32,
    pub available_at: temps_core::DBDateTime,
    pub claim_id: Option<Uuid>,
    pub claimed_until: Option<temps_core::DBDateTime>,
    pub completed_at: Option<temps_core::DBDateTime>,
    pub created_at: temps_core::DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ActiveEnum;
    #[test]
    fn operations_match_database_constraints() {
        assert_eq!(DeliveryOperation::Reconcile.to_value(), "reconcile");
        assert_eq!(DeliveryOperation::Retire.to_value(), "retire");
        assert!(DeliveryOperation::try_from_value(&"connected".into()).is_err());
    }
}
