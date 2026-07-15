use sea_orm_migration::prelude::*;

/// Per-managed-domain public hostname configuration (reworks the global
/// `public_hostnames` setting from the original flat-hostname PR).
///
/// Adds to `dns_managed_domains`:
/// - `generated_hostname_mode` — `'standard'` (default) or `'flat'`. Selected by
///   the operator when configuring a provider (Cloudflare advertises the flat
///   capability for Universal SSL).
/// - `sync_generated_records` — opt-in: reconcile generated hostnames into the
///   provider's DNS zone.
/// - `zone_access_ok` / `zone_access_error` — cached result of the token
///   zone-access check, so the UI can flag a token that cannot manage the zone.
///
/// Backward-compat: instances that set the now-removed global
/// `public_hostnames.strategy = "flat"` have that intent carried onto every
/// existing managed domain so generated hostnames don't silently revert to
/// Standard on upgrade. `AppSettings` ignores the now-unknown JSON key, so the
/// settings row is left as-is.
///
/// **Safely re-runnable:** column adds are guarded with `IF NOT EXISTS`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'dns_managed_domains'
          AND column_name = 'generated_hostname_mode'
    ) THEN
        ALTER TABLE dns_managed_domains
            ADD COLUMN generated_hostname_mode VARCHAR(16) NOT NULL DEFAULT 'standard';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'dns_managed_domains'
          AND column_name = 'sync_generated_records'
    ) THEN
        ALTER TABLE dns_managed_domains
            ADD COLUMN sync_generated_records BOOLEAN NOT NULL DEFAULT false;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'dns_managed_domains'
          AND column_name = 'zone_access_ok'
    ) THEN
        ALTER TABLE dns_managed_domains
            ADD COLUMN zone_access_ok BOOLEAN;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'dns_managed_domains'
          AND column_name = 'zone_access_error'
    ) THEN
        ALTER TABLE dns_managed_domains
            ADD COLUMN zone_access_error TEXT;
    END IF;
END $$;
            "#,
        )
        .await?;

        // Carry any pre-existing global flat-hostname intent onto existing
        // managed domains. The settings table is a singleton (id = 1) whose
        // `data` JSON may contain the removed `public_hostnames.strategy` key.
        db.execute_unprepared(
            r#"
UPDATE dns_managed_domains
SET generated_hostname_mode = 'flat'
WHERE EXISTS (
    SELECT 1 FROM settings
    WHERE settings.data #>> '{public_hostnames,strategy}' = 'flat'
);
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE dns_managed_domains
    DROP COLUMN IF EXISTS generated_hostname_mode,
    DROP COLUMN IF EXISTS sync_generated_records,
    DROP COLUMN IF EXISTS zone_access_ok,
    DROP COLUMN IF EXISTS zone_access_error;
            "#,
        )
        .await?;

        Ok(())
    }
}
