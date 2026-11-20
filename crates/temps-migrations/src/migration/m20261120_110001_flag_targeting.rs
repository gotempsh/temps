// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Feature-flag targeting (ADR 034 Phase 2, ADR 110 D3).
//!
//! - `feature_flags.variants`: named values a rule may serve, so a rollout or
//!   an experiment keys off a stable name rather than a serialized value.
//! - `feature_flags.exposure_sample_rate`: share of subjects, `0..=1`, for
//!   whom a browser records a `flag_exposure` event when it reads the flag.
//!   `0` (the default) keeps exposure off: exposures are high volume and
//!   opt-in per flag (ADR 034 R6.1).
//!
//! Rules need no column: `feature_flag_environments.rules` has existed, always
//! `[]`, since Phase 1. Both columns have constant defaults, so adding them is
//! a catalog-only change; `feature_flags` is a small control-plane table, so
//! validating the CHECK constraint is cheap.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE feature_flags
    ADD COLUMN IF NOT EXISTS variants JSONB NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS exposure_sample_rate DOUBLE PRECISION NOT NULL DEFAULT 0;

ALTER TABLE feature_flags DROP CONSTRAINT IF EXISTS ck_feature_flags_exposure_sample_rate;
ALTER TABLE feature_flags
    ADD CONSTRAINT ck_feature_flags_exposure_sample_rate
    CHECK (exposure_sample_rate >= 0 AND exposure_sample_rate <= 1);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE feature_flags
    DROP CONSTRAINT IF EXISTS ck_feature_flags_exposure_sample_rate,
    DROP COLUMN IF EXISTS exposure_sample_rate,
    DROP COLUMN IF EXISTS variants;
"#,
            )
            .await?;
        Ok(())
    }
}
