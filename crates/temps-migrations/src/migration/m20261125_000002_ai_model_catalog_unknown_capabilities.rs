// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record whether the catalog stated each capability of a stored entry.
//!
//! `ai_model_catalog_entries.supports_tools`, `supports_vision` and
//! `supports_reasoning` are `NOT NULL DEFAULT FALSE`, so a catalog that omits
//! those fields (the one published in the Temps repository omits all three)
//! was listed as unable to call tools or reason. `tools_stated`,
//! `vision_stated` and `reasoning_stated` say whether the `supports_*` value
//! came from the catalog; when one is `FALSE` the capability is unknown and
//! its `supports_*` column holds `FALSE`.
//!
//! The `supports_*` columns are left untouched so the previous binary still
//! reads every row after a binary-only rollback: it selects only the columns
//! it knows, and the rows it writes get `TRUE`, its own meaning. Existing rows
//! also get `TRUE` (there is no telling a stated "no" from an omitted one);
//! the next refresh, which replaces every row, records what the catalog says.
//! `down` drops the three columns.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const COLUMNS: [&str; 3] = ["tools_stated", "vision_stated", "reasoning_stated"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for column in COLUMNS {
            db.execute_unprepared(&format!(
                "ALTER TABLE ai_model_catalog_entries \
                 ADD COLUMN IF NOT EXISTS {column} BOOLEAN NOT NULL DEFAULT TRUE"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for column in COLUMNS {
            db.execute_unprepared(&format!(
                "ALTER TABLE ai_model_catalog_entries DROP COLUMN IF EXISTS {column}"
            ))
            .await?;
        }
        Ok(())
    }
}
