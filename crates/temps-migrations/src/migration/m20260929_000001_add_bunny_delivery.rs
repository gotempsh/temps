// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE delivery_profiles DROP CONSTRAINT delivery_profiles_provider_kind_check;
ALTER TABLE delivery_profiles ADD CONSTRAINT delivery_profiles_provider_kind_check
  CHECK (provider_kind IN ('direct', 'cloudflare', 'bunny'));
ALTER TABLE delivery_profiles ADD COLUMN bunny_pull_zone_id bigint;
ALTER TABLE delivery_profiles ADD COLUMN bunny_hostname text;
ALTER TABLE delivery_profiles ADD COLUMN bunny_api_key_encrypted text;
ALTER TABLE delivery_profiles ADD CONSTRAINT bunny_profile_config_complete CHECK (
  (provider_kind = 'bunny' AND bunny_pull_zone_id > 0 AND bunny_hostname IS NOT NULL AND bunny_api_key_encrypted IS NOT NULL)
  OR (provider_kind <> 'bunny' AND bunny_pull_zone_id IS NULL AND bunny_hostname IS NULL AND bunny_api_key_encrypted IS NULL)
);
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM delivery_profiles WHERE provider_kind = 'bunny') THEN
    RAISE EXCEPTION 'Cannot downgrade Bunny delivery while Bunny profiles exist; remove their bindings and profiles first';
  END IF;
END $$;
ALTER TABLE delivery_profiles DROP CONSTRAINT bunny_profile_config_complete;
ALTER TABLE delivery_profiles DROP COLUMN bunny_api_key_encrypted;
ALTER TABLE delivery_profiles DROP COLUMN bunny_hostname;
ALTER TABLE delivery_profiles DROP COLUMN bunny_pull_zone_id;
ALTER TABLE delivery_profiles DROP CONSTRAINT delivery_profiles_provider_kind_check;
ALTER TABLE delivery_profiles ADD CONSTRAINT delivery_profiles_provider_kind_check
  CHECK (provider_kind IN ('direct', 'cloudflare'));
"#,
            )
            .await?;
        Ok(())
    }
}
