//! Teams + project-scoped RBAC schema.
//!
//! Five tables land together because they are one feature and are useless
//! individually — a team with no membership table grants nothing, and a
//! custom role with no permission rows is an empty set:
//!
//! - `teams` — named groups of users.
//! - `team_members` — user↔team membership, carrying either a fixed role
//!   (`owner|admin|deployer|viewer`) or a `custom_role_id`.
//! - `project_team_access` — which teams may reach which projects, and
//!   with what role. A project with **zero** rows here is unrestricted;
//!   adding the first grant is what makes it team-gated.
//! - `custom_roles` / `custom_role_permissions` — admin-defined named
//!   permission sets, one row per (role, permission) pair.
//!
//! Roles are stored as `varchar(32)` rather than a Postgres enum so the
//! role set can evolve in pure migration code — an `ALTER TYPE ... ADD
//! VALUE` cannot run inside a transaction and cannot be reversed, which
//! would make a downgrade path impossible.
//!
//! Permissions on `custom_role_permissions` are stored as their string
//! form (`"deployments:write"`) and validated against the `Permission`
//! enum in the service layer, not by a DB constraint: the vocabulary is
//! owned by `temps-auth` and grows with the binary, so a CHECK constraint
//! here would have to be migrated on every new permission.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // -------------------------------------------------------------
        // teams
        // -------------------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(Teams::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Teams::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Teams::Name).string_len(255).not_null())
                    .col(
                        ColumnDef::new(Teams::Slug)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Teams::Description).text().null())
                    .col(ColumnDef::new(Teams::CreatedBy).integer().not_null())
                    .col(
                        ColumnDef::new(Teams::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Teams::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // -------------------------------------------------------------
        // custom_roles (before team_members — that table FKs into this one)
        // -------------------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(CustomRoles::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CustomRoles::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CustomRoles::Name).string_len(255).not_null())
                    .col(
                        ColumnDef::new(CustomRoles::Slug)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(CustomRoles::Description).text().null())
                    .col(ColumnDef::new(CustomRoles::CreatedBy).integer().not_null())
                    .col(
                        ColumnDef::new(CustomRoles::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(CustomRoles::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // -------------------------------------------------------------
        // custom_role_permissions — composite PK, one row per permission
        // -------------------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(CustomRolePermissions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CustomRolePermissions::RoleId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CustomRolePermissions::Permission)
                            .string_len(128)
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(CustomRolePermissions::RoleId)
                            .col(CustomRolePermissions::Permission),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_custom_role_permissions_role")
                            .from(CustomRolePermissions::Table, CustomRolePermissions::RoleId)
                            .to(CustomRoles::Table, CustomRoles::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // -------------------------------------------------------------
        // team_members
        // -------------------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(TeamMembers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TeamMembers::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TeamMembers::TeamId).integer().not_null())
                    .col(ColumnDef::new(TeamMembers::UserId).integer().not_null())
                    .col(ColumnDef::new(TeamMembers::Role).string_len(32).not_null())
                    // NULL = use the fixed `role` column above. ON DELETE
                    // SET NULL so deleting a custom role degrades members
                    // to their fixed role rather than blocking the delete.
                    .col(ColumnDef::new(TeamMembers::CustomRoleId).integer().null())
                    .col(ColumnDef::new(TeamMembers::AddedBy).integer().not_null())
                    .col(
                        ColumnDef::new(TeamMembers::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(TeamMembers::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_team_members_team")
                            .from(TeamMembers::Table, TeamMembers::TeamId)
                            .to(Teams::Table, Teams::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    // A deleted user must not keep silently granting their
                    // team's project access to a recycled user id.
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_team_members_user")
                            .from(TeamMembers::Table, TeamMembers::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_team_members_custom_role")
                            .from(TeamMembers::Table, TeamMembers::CustomRoleId)
                            .to(CustomRoles::Table, CustomRoles::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // One membership row per (team, user) — the service relies on this
        // to turn a duplicate insert into a 409 rather than creating a
        // second membership with a conflicting role.
        manager
            .create_index(
                Index::create()
                    .name("idx_team_members_team_user")
                    .table(TeamMembers::Table)
                    .col(TeamMembers::TeamId)
                    .col(TeamMembers::UserId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // The access check's step 2 filters on user_id first.
        manager
            .create_index(
                Index::create()
                    .name("idx_team_members_user")
                    .table(TeamMembers::Table)
                    .col(TeamMembers::UserId)
                    .to_owned(),
            )
            .await?;

        // -------------------------------------------------------------
        // project_team_access
        // -------------------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(ProjectTeamAccess::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectTeamAccess::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ProjectTeamAccess::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTeamAccess::TeamId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTeamAccess::Role)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTeamAccess::GrantedBy)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTeamAccess::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ProjectTeamAccess::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_team_access_team")
                            .from(ProjectTeamAccess::Table, ProjectTeamAccess::TeamId)
                            .to(Teams::Table, Teams::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    // Deleting a project must take its grants with it —
                    // otherwise a recycled project id would inherit the
                    // old project's team gating.
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_team_access_project")
                            .from(ProjectTeamAccess::Table, ProjectTeamAccess::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Required by the grant upsert's ON CONFLICT target.
        manager
            .create_index(
                Index::create()
                    .name("idx_project_team_access_unique")
                    .table(ProjectTeamAccess::Table)
                    .col(ProjectTeamAccess::ProjectId)
                    .col(ProjectTeamAccess::TeamId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // The access check's step 1 ("does this project have any grants?")
        // runs on every project-scoped request that misses the cache.
        manager
            .create_index(
                Index::create()
                    .name("idx_project_team_access_project")
                    .table(ProjectTeamAccess::Table)
                    .col(ProjectTeamAccess::ProjectId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_project_team_access_team")
                    .table(ProjectTeamAccess::Table)
                    .col(ProjectTeamAccess::TeamId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse creation order; each drop takes its own indexes and
        // foreign keys with it.
        manager
            .drop_table(Table::drop().table(ProjectTeamAccess::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TeamMembers::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(CustomRolePermissions::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(CustomRoles::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Teams::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Teams {
    Table,
    Id,
    Name,
    Slug,
    Description,
    CreatedBy,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum TeamMembers {
    Table,
    Id,
    TeamId,
    UserId,
    Role,
    CustomRoleId,
    AddedBy,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum ProjectTeamAccess {
    Table,
    Id,
    ProjectId,
    TeamId,
    Role,
    GrantedBy,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum CustomRoles {
    Table,
    Id,
    Name,
    Slug,
    Description,
    CreatedBy,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum CustomRolePermissions {
    Table,
    RoleId,
    Permission,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
