// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preserve pre-upgrade standalone consent under its original project links.
//! PostgreSQL's constant defaults mark existing vaults without rewriting them.
//! Membership is snapshotted lazily before its first edit; removals are retained
//! so removing and readding a service cannot resurrect old consent. New vaults
//! have no compatibility cutoff. No approval or grant history backfill is needed.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE access_vaults ADD COLUMN IF NOT EXISTS legacy_approval_before timestamptz DEFAULT CURRENT_TIMESTAMP;
ALTER TABLE access_vaults ALTER COLUMN legacy_approval_before SET DEFAULT NULL;
ALTER TABLE access_vaults ADD COLUMN IF NOT EXISTS legacy_service_ids text[];
ALTER TABLE access_vaults ADD COLUMN IF NOT EXISTS legacy_removed_service_ids text[] NOT NULL DEFAULT '{}';
CREATE OR REPLACE FUNCTION retain_legacy_standalone_membership() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
 IF OLD.legacy_approval_before IS NOT NULL THEN
  NEW.legacy_approval_before := OLD.legacy_approval_before;
  NEW.legacy_service_ids := COALESCE(OLD.legacy_service_ids, OLD.service_ids);
  NEW.legacy_removed_service_ids := ARRAY(
   SELECT unnest(OLD.legacy_removed_service_ids)
   UNION (SELECT unnest(NEW.legacy_service_ids) EXCEPT SELECT unnest(NEW.service_ids))
  );
 END IF;
 RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS retain_legacy_standalone_membership ON access_vaults;
CREATE TRIGGER retain_legacy_standalone_membership BEFORE UPDATE OF service_ids ON access_vaults
 FOR EACH ROW EXECUTE FUNCTION retain_legacy_standalone_membership();
CREATE OR REPLACE FUNCTION temps_standalone_vault_coverage(service text, project integer, approved timestamptz)
RETURNS boolean LANGUAGE sql STABLE AS $$
 SELECT NOT EXISTS (
  SELECT 1 FROM access_vaults v
  WHERE (v.retired_at IS NULL AND v.service_ids @> ARRAY[service]
    OR v.legacy_approval_before IS NOT NULL AND approved < v.legacy_approval_before AND COALESCE(v.legacy_service_ids,v.service_ids) @> ARRAY[service])
   AND NOT COALESCE((approved < v.legacy_approval_before
    AND v.project_id=project AND v.retired_at IS NULL
    AND COALESCE(v.legacy_service_ids,v.service_ids) @> ARRAY[service]
    AND v.service_ids @> ARRAY[service]
    AND NOT v.legacy_removed_service_ids @> ARRAY[service]
    AND EXISTS (SELECT 1 FROM access_vault_links l
     WHERE l.id='link_legacy_' || v.id AND l.vault_id=v.id
       AND l.project_id=project AND l.revoked_at IS NULL)), false)
 )
$$;
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM access_vaults WHERE legacy_approval_before IS NOT NULL) THEN
  RAISE EXCEPTION 'Cannot downgrade while vaults retain legacy standalone approval provenance';
 END IF;
END $$;
DROP FUNCTION IF EXISTS temps_standalone_vault_coverage(text,integer,timestamptz);
DROP TRIGGER IF EXISTS retain_legacy_standalone_membership ON access_vaults;
DROP FUNCTION IF EXISTS retain_legacy_standalone_membership();
ALTER TABLE access_vaults DROP COLUMN IF EXISTS legacy_removed_service_ids;
ALTER TABLE access_vaults DROP COLUMN IF EXISTS legacy_service_ids;
ALTER TABLE access_vaults DROP COLUMN IF EXISTS legacy_approval_before;
"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement};

    async fn covered(db: &sea_orm::DatabaseConnection, service: &str, old: bool) -> bool {
        db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT temps_standalone_vault_coverage($1,7,now()+CASE WHEN $2 THEN interval '-1 day' ELSE interval '1 day' END) AS allowed",
            [service.into(),old.into()])).await.unwrap().unwrap().try_get("", "allowed").unwrap()
    }

    #[tokio::test]
    async fn original_membership_and_links_bound_pre_upgrade_consent() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping legacy standalone coverage: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection();
        db.execute_unprepared("CREATE TABLE access_vaults(id text PRIMARY KEY,project_id integer,service_ids text[] NOT NULL,retired_at timestamptz); CREATE TABLE access_vault_links(id text PRIMARY KEY,vault_id text,project_id integer,revoked_at timestamptz)").await.unwrap();
        let manager = SchemaManager::new(db);
        Migration.up(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();
        db.execute_unprepared("INSERT INTO access_vaults VALUES('original',7,ARRAY['existing','removed'],NULL); INSERT INTO access_vault_links VALUES('link_legacy_original','original',7,NULL)").await.unwrap();
        Migration.up(&manager).await.unwrap();
        assert!(covered(db, "existing", true).await);
        assert!(!covered(db, "existing", false).await);
        assert!(covered(db, "standalone", false).await);
        assert!(Migration
            .down(&manager)
            .await
            .unwrap_err()
            .to_string()
            .contains("legacy standalone approval provenance"));
        // A name-only or membership-preserving edit does not stop access.
        db.execute_unprepared(
            "UPDATE access_vaults SET service_ids=ARRAY['existing','removed','added']",
        )
        .await
        .unwrap();
        assert!(covered(db, "existing", true).await);
        assert!(!covered(db, "added", true).await);
        db.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['existing','added']")
            .await
            .unwrap();
        assert!(!covered(db, "removed", true).await);
        db.execute_unprepared(
            "UPDATE access_vaults SET service_ids=ARRAY['existing','removed','added']",
        )
        .await
        .unwrap();
        assert!(!covered(db, "removed", true).await);
        // New vaults cannot borrow an old approval's timestamp.
        db.execute_unprepared("INSERT INTO access_vaults(id,project_id,service_ids) VALUES('new',7,ARRAY['new_service'])").await.unwrap();
        assert!(!covered(db, "new_service", true).await);
        Migration.up(&manager).await.unwrap();
        assert!(!covered(db, "new_service", true).await);
        assert!(covered(db, "existing", true).await);
        db.execute_unprepared("UPDATE access_vault_links SET revoked_at=now(); INSERT INTO access_vault_links VALUES('replacement','original',7,NULL)").await.unwrap();
        assert!(!covered(db, "existing", true).await);
        db.execute_unprepared("DELETE FROM access_vault_links WHERE id='link_legacy_original'")
            .await
            .unwrap();
        assert!(!covered(db, "existing", true).await);
        test.cleanup().await;
    }
}
