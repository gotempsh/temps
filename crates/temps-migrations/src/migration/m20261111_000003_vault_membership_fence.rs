// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fence standalone policies when vault membership withdraws their coverage.
//! Only changed service memberships are inspected; unchanged members keep their
//! consent. Grant updates use the existing authority-revision/wakeup triggers.
//! Installing the trigger does not rewrite history. Down removes the trigger
//! without resurrecting consent already revoked by a membership change.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE OR REPLACE FUNCTION fence_vault_membership_consent() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE changed_services text[];
BEGIN
 IF TG_OP='INSERT' THEN
  changed_services := NEW.service_ids;
 ELSIF OLD.retired_at IS DISTINCT FROM NEW.retired_at THEN
  changed_services := ARRAY(SELECT unnest(OLD.service_ids) UNION SELECT unnest(NEW.service_ids));
 ELSE
  changed_services := ARRAY(
   (SELECT unnest(OLD.service_ids) EXCEPT SELECT unnest(NEW.service_ids))
   UNION
   (SELECT unnest(NEW.service_ids) EXCEPT SELECT unnest(OLD.service_ids))
  );
 END IF;
 IF cardinality(changed_services)=0 THEN RETURN NULL; END IF;
 UPDATE sandbox_service_grants g SET revoked_at=now(),version=g.version+1
  WHERE g.revoked_at IS NULL AND g.service_id=ANY(changed_services)
   AND NOT EXISTS (SELECT 1 FROM access_sessions s WHERE s.grant_ids @> ARRAY[g.id])
   AND NOT temps_standalone_vault_coverage(g.service_id,g.project_id,
    CASE WHEN g.assignment_id IS NULL THEN g.created_at ELSE
     (SELECT d.approved_at FROM sandbox_service_assignments d WHERE d.id=g.assignment_id) END);
 RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS fence_vault_membership_consent ON access_vaults;
CREATE TRIGGER fence_vault_membership_consent AFTER INSERT OR UPDATE OF service_ids,retired_at ON access_vaults
 FOR EACH ROW EXECUTE FUNCTION fence_vault_membership_consent();
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DROP TRIGGER IF EXISTS fence_vault_membership_consent ON access_vaults;
DROP FUNCTION IF EXISTS fence_vault_membership_consent();
"#).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement, TransactionTrait};

    async fn version(db: &sea_orm::DatabaseConnection, id: &str) -> i32 {
        db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT version FROM sandbox_service_grants WHERE id=$1", [id.into()]))
            .await.unwrap().unwrap().try_get("", "version").unwrap()
    }

    #[tokio::test]
    async fn membership_changes_revoke_only_uncovered_standalone_consent() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping membership fence: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection();
        db.execute_unprepared(r#"
CREATE TABLE access_vaults(id text PRIMARY KEY,project_id integer,service_ids text[] NOT NULL,retired_at timestamptz);
CREATE TABLE access_vault_links(id text PRIMARY KEY,vault_id text,project_id integer,revoked_at timestamptz);
CREATE TABLE sandbox_service_assignments(id text PRIMARY KEY,approved_at timestamptz);
CREATE TABLE sandbox_service_grants(id text PRIMARY KEY,project_id integer,service_id text,assignment_id text,created_at timestamptz,revoked_at timestamptz,version integer DEFAULT 1);
CREATE TABLE access_sessions(grant_ids text[]);
INSERT INTO access_vaults VALUES('original',7,ARRAY['removed','kept'],NULL);
INSERT INTO access_vault_links VALUES('link_legacy_original','original',7,NULL);
INSERT INTO sandbox_service_grants(id,project_id,service_id,created_at)
 SELECT name,7,name,now()-interval '1 day' FROM unnest(ARRAY['removed','kept','new_vault','new_member','scoped','unrelated']) AS name;
INSERT INTO access_sessions VALUES(ARRAY['scoped']);
"#).await.unwrap();
        let manager = SchemaManager::new(db);
        crate::LegacyStandaloneVaultCoverageMigration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
        // Reordering the same selection does not revoke or churn revisions.
        db.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['kept','removed']").await.unwrap();
        assert_eq!(version(db, "removed").await, 1);
        assert_eq!(version(db, "kept").await, 1);
        let tx = db.begin().await.unwrap();
        tx.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['kept']").await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(version(db, "removed").await, 1);
        db.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['kept']").await.unwrap();
        assert_eq!(version(db, "removed").await, 2);
        assert_eq!(version(db, "kept").await, 1);
        db.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['kept','removed','new_member']").await.unwrap();
        assert_eq!(version(db, "removed").await, 2);
        assert_eq!(version(db, "new_member").await, 2);
        assert_eq!(version(db, "kept").await, 1);
        db.execute_unprepared("INSERT INTO access_vaults VALUES('new',7,ARRAY['new_vault','scoped'],NULL,NULL,NULL,'{}')").await.unwrap();
        assert_eq!(version(db, "new_vault").await, 2);
        assert_eq!(version(db, "scoped").await, 1);
        assert_eq!(version(db, "unrelated").await, 1);
        db.execute_unprepared("UPDATE access_vaults SET retired_at=now() WHERE id='original'").await.unwrap();
        assert_eq!(version(db, "kept").await, 2);
        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();
        db.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['unrelated'] WHERE id='new'").await.unwrap();
        assert_eq!(version(db, "unrelated").await, 1);
        assert_eq!(version(db, "removed").await, 2);
        test.cleanup().await;
    }
}
