// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let a stored catalog entry say "the catalog does not say" for a
//! capability.
//!
//! `ai_model_catalog_entries.supports_tools`, `supports_vision` and
//! `supports_reasoning` were `NOT NULL DEFAULT FALSE`, so a catalog that omits
//! those fields (the one published in the Temps repository omits all three)
//! was stored as "no", and listed every model as unable to call tools or
//! reason. They become nullable, with no default: `NULL` is "unknown".
//!
//! Existing rows keep their values (`FALSE` included: there is no telling a
//! stated "no" from an omitted one); the next refresh, which replaces every
//! row, stores `NULL` where the catalog is silent. `down` stores unknown as
//! `FALSE` and restores the constraint and default; the table is a cache that
//! the next refresh rewrites.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const COLUMNS: [&str; 3] = ["supports_tools", "supports_vision", "supports_reasoning"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for column in COLUMNS {
            db.execute_unprepared(&format!(
                "ALTER TABLE ai_model_catalog_entries \
                 ALTER COLUMN {column} DROP NOT NULL, \
                 ALTER COLUMN {column} DROP DEFAULT"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for column in COLUMNS {
            db.execute_unprepared(&format!(
                "UPDATE ai_model_catalog_entries SET {column} = FALSE WHERE {column} IS NULL"
            ))
            .await?;
            db.execute_unprepared(&format!(
                "ALTER TABLE ai_model_catalog_entries \
                 ALTER COLUMN {column} SET DEFAULT FALSE, \
                 ALTER COLUMN {column} SET NOT NULL"
            ))
            .await?;
        }
        Ok(())
    }
}
