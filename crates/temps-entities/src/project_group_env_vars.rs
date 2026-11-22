// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// An environment variable set on a [`super::project_groups`] row and
/// inherited by every project in the group. `value` is always encrypted.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "project_group_env_vars")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub group_id: i32,
    pub key: String,
    #[serde(skip_serializing)]
    pub value: String,
    pub is_secret: bool,
    /// Environment slug this applies to; `None` means every environment.
    pub environment_slug: Option<String>,
    /// Whether an unscoped variable also reaches preview environments.
    pub include_in_preview: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project_groups::Entity",
        from = "Column::GroupId",
        to = "super::project_groups::Column::Id",
        on_delete = "Cascade"
    )]
    Group,
}

impl Related<super::project_groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Group.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Picks the group variables that apply to one environment: a variable
/// scoped to the environment's slug beats an unscoped one with the same key;
/// unscoped variables reach previews only when `include_in_preview` is set.
pub fn select_inherited(
    variables: Vec<Model>,
    environment_slug: &str,
    is_preview: bool,
) -> std::collections::BTreeMap<String, Model> {
    let mut selected: std::collections::BTreeMap<String, Model> = Default::default();
    for variable in variables {
        let scoped = match variable.environment_slug.as_deref() {
            Some(slug) if slug == environment_slug => true,
            Some(_) => continue,
            None if is_preview && !variable.include_in_preview => continue,
            None => false,
        };
        let replace = selected
            .get(&variable.key)
            .is_none_or(|current| scoped && current.environment_slug.is_none());
        if replace {
            selected.insert(variable.key.clone(), variable);
        }
    }
    selected
}

/// The group variables `project_id` inherits in `environment`, still
/// encrypted, with the id of the group they come from. `None` when the
/// project is in no group. The one place that decides what a service
/// inherits, shared by the deploy paths and the project-groups API.
pub async fn inherited_for_project<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
    environment: &super::environments::Model,
) -> Result<Option<(i32, std::collections::BTreeMap<String, Model>)>, DbErr> {
    let Some(membership) = super::project_group_members::Entity::find_by_id(project_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let variables = Entity::find()
        .filter(Column::GroupId.eq(membership.group_id))
        .all(db)
        .await?;
    Ok(Some((
        membership.group_id,
        select_inherited(variables, &environment.slug, environment.is_preview),
    )))
}
