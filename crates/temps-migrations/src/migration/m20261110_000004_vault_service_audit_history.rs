// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preserve service audit membership when a vault selection changes or is deleted.
//! No backfill is needed: readers include current membership, and the trigger
//! captures both sides of the first change to an existing vault. IDs deliberately
//! have no cascading foreign keys: deleting a resource must retain its audit scope.
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
CREATE TABLE IF NOT EXISTS access_vault_service_history (
 vault_id text NOT NULL,
 service_id text NOT NULL,
 PRIMARY KEY (vault_id, service_id)
);
CREATE OR REPLACE FUNCTION preserve_vault_service_history() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
 IF TG_OP <> 'INSERT' THEN
  INSERT INTO access_vault_service_history(vault_id,service_id)
   SELECT OLD.id, service_id FROM unnest(OLD.service_ids) AS service_id
   ON CONFLICT DO NOTHING;
 END IF;
 IF TG_OP <> 'DELETE' THEN
  INSERT INTO access_vault_service_history(vault_id,service_id)
   SELECT NEW.id, service_id FROM unnest(NEW.service_ids) AS service_id
   ON CONFLICT DO NOTHING;
 END IF;
 RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS preserve_vault_service_history ON access_vaults;
CREATE TRIGGER preserve_vault_service_history
 AFTER INSERT OR UPDATE OF service_ids OR DELETE ON access_vaults
 FOR EACH ROW EXECUTE FUNCTION preserve_vault_service_history();
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM access_vault_service_history) THEN
  RAISE EXCEPTION 'Cannot downgrade while access_vault_service_history contains retained audit membership';
 END IF;
END $$;
DROP TRIGGER IF EXISTS preserve_vault_service_history ON access_vaults;
DROP FUNCTION IF EXISTS preserve_vault_service_history();
DROP TABLE IF EXISTS access_vault_service_history;
"#).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement};

    #[tokio::test]
    async fn audit_membership_survives_selection_changes_deletion_and_rollback() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping vault audit membership: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        db.execute_unprepared("CREATE TABLE access_vaults(id text PRIMARY KEY, service_ids text[] NOT NULL); INSERT INTO access_vaults VALUES('vault_legacy',ARRAY['service_old'])").await.unwrap();
        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Empty history is reversible, including when pre-upgrade vaults exist.
        Migration.down(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
        db.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['service_new']; UPDATE access_vaults SET service_ids=ARRAY['service_new']; DELETE FROM access_vaults; INSERT INTO access_vaults VALUES('vault_new',ARRAY['service_added']); DELETE FROM access_vaults").await.unwrap();
        let rows = db.query_all(Statement::from_string(DbBackend::Postgres,
            "SELECT vault_id,service_id FROM access_vault_service_history ORDER BY vault_id,service_id")).await.unwrap();
        let pairs: Vec<(String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r.try_get("", "vault_id").unwrap(),
                    r.try_get("", "service_id").unwrap(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("vault_legacy".into(), "service_new".into()),
                ("vault_legacy".into(), "service_old".into()),
                ("vault_new".into(), "service_added".into())
            ]
        );
        assert!(Migration
            .down(&manager)
            .await
            .unwrap_err()
            .to_string()
            .contains("retained audit membership"));
        test.cleanup().await;
    }
}
