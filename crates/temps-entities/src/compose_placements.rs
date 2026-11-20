// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node a Compose environment's stack lives on (see the
//! `m20261028_000002_compose_placements` migration).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "compose_placements")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub environment_id: i32,
    /// The stack's home. `None` is the control plane.
    pub node_id: Option<i32>,
    /// When the stack first started on this home.
    pub pinned_at: DateTimeUtc,
    /// A move was requested and the old host still runs the stack.
    pub vacate_pending: bool,
    /// The old host to stop on the next deployment when `vacate_pending`.
    /// `None` with `vacate_pending` is the control plane.
    pub vacate_node_id: Option<i32>,
    pub updated_at: DateTimeUtc,
}

impl Model {
    /// The host the next deployment must stop first, when a move is pending:
    /// `Some(None)` is the control plane, `Some(Some(id))` a worker.
    pub fn pending_vacate(&self) -> Option<Option<i32>> {
        self.vacate_pending.then_some(self.vacate_node_id)
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Environment,
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Node,
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Node.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(vacate_pending: bool, vacate_node_id: Option<i32>) -> Model {
        Model {
            environment_id: 1,
            node_id: Some(3),
            pinned_at: chrono::Utc::now(),
            vacate_pending,
            vacate_node_id,
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn a_pending_vacate_distinguishes_the_control_plane_from_none() {
        assert_eq!(placement(false, Some(2)).pending_vacate(), None);
        assert_eq!(placement(true, None).pending_vacate(), Some(None));
        assert_eq!(placement(true, Some(2)).pending_vacate(), Some(Some(2)));
    }
}
