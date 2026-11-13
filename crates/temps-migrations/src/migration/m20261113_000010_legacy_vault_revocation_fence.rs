// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Revoke preserved standalone consent atomically with its original vault link.
//! Grant updates advance the existing worker authority revision, fencing plans
//! read before unlink/retirement. Durable approvals retain their provenance;
//! renewal already retires them when their original link no longer covers them.
//! No historical rows are rewritten during installation. Downgrade removes the
//! extra triggers but never restores revoked consent.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE OR REPLACE FUNCTION revoke_legacy_vault_standalone_consent() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE vault access_vaults%ROWTYPE;
BEGIN
 IF TG_OP='UPDATE' AND (OLD.revoked_at IS NOT NULL OR NEW.revoked_at IS NULL) THEN
  RETURN NULL;
 END IF;
 SELECT * INTO vault FROM access_vaults v
  WHERE v.id=OLD.vault_id AND OLD.id='link_legacy_' || v.id
    AND v.project_id=OLD.project_id AND v.legacy_approval_before IS NOT NULL;
 IF FOUND THEN
  -- Grant updates also fence snapshots through the existing revision trigger.
  UPDATE sandbox_service_grants g SET revoked_at=now(),version=g.version+1
   WHERE g.revoked_at IS NULL AND g.project_id=OLD.project_id
    AND g.service_id=ANY(COALESCE(vault.legacy_service_ids,vault.service_ids))
    AND (CASE WHEN g.assignment_id IS NULL THEN g.created_at ELSE
      (SELECT d.approved_at FROM sandbox_service_assignments d WHERE d.id=g.assignment_id)
     END) < vault.legacy_approval_before
    AND NOT EXISTS (SELECT 1 FROM access_sessions s WHERE s.grant_ids @> ARRAY[g.id]);
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS legacy_vault_standalone_revocation ON access_vault_links;
CREATE TRIGGER legacy_vault_standalone_revocation AFTER UPDATE OF revoked_at ON access_vault_links
 FOR EACH ROW EXECUTE FUNCTION revoke_legacy_vault_standalone_consent();
DROP TRIGGER IF EXISTS legacy_vault_standalone_deletion ON access_vault_links;
CREATE TRIGGER legacy_vault_standalone_deletion BEFORE DELETE ON access_vault_links
 FOR EACH ROW EXECUTE FUNCTION revoke_legacy_vault_standalone_consent();
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DROP TRIGGER IF EXISTS legacy_vault_standalone_revocation ON access_vault_links;
DROP TRIGGER IF EXISTS legacy_vault_standalone_deletion ON access_vault_links;
DROP FUNCTION IF EXISTS revoke_legacy_vault_standalone_consent();
"#).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement, TransactionTrait};

    #[tokio::test]
    async fn legacy_revocation_is_atomic_scoped_and_reversible() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping legacy revocation: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection();
        db.execute_unprepared(r#"
CREATE TABLE access_vaults(id text PRIMARY KEY,project_id integer,service_ids text[],legacy_service_ids text[],legacy_approval_before timestamptz);
CREATE TABLE access_vault_links(id text PRIMARY KEY,vault_id text,project_id integer,revoked_at timestamptz);
CREATE TABLE sandbox_service_assignments(id text PRIMARY KEY,approved_at timestamptz);
CREATE TABLE sandbox_service_grants(id text PRIMARY KEY,project_id integer,service_id text,assignment_id text,created_at timestamptz,revoked_at timestamptz,version integer DEFAULT 1);
CREATE TABLE access_sessions(grant_ids text[]);
INSERT INTO access_vaults VALUES('original',7,ARRAY['service'],ARRAY['service','removed'],now()),('other',8,ARRAY['service'],NULL,now());
INSERT INTO access_vault_links VALUES('link_legacy_original','original',7,NULL),('replacement','original',7,NULL),('link_legacy_other','other',8,NULL);
INSERT INTO sandbox_service_assignments VALUES('approval',now()-interval '1 day');
INSERT INTO sandbox_service_grants(id,project_id,service_id,assignment_id,created_at) VALUES
 ('one-shot',7,'service',NULL,now()-interval '1 day'),
 ('renewed',7,'service','approval',now()+interval '1 day'),
 ('removed',7,'removed',NULL,now()-interval '1 day'),
 ('other-project',8,'service',NULL,now()-interval '1 day'),
 ('other-service',7,'other',NULL,now()-interval '1 day'),
 ('new-consent',7,'service',NULL,now()+interval '1 day'),
 ('session',7,'service',NULL,now()-interval '1 day');
INSERT INTO access_sessions VALUES(ARRAY['session']);
"#).await.unwrap();
        let manager = SchemaManager::new(db);
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
        let tx = db.begin().await.unwrap();
        tx.execute_unprepared("UPDATE access_vault_links SET revoked_at=now() WHERE id='link_legacy_original'").await.unwrap();
        tx.rollback().await.unwrap();
        db.execute_unprepared("DELETE FROM access_vault_links WHERE id='replacement'").await.unwrap();
        let revoked = db.query_one(Statement::from_string(DbBackend::Postgres,
            "SELECT count(*) AS count FROM sandbox_service_grants WHERE revoked_at IS NOT NULL")).await.unwrap().unwrap();
        assert_eq!(revoked.try_get::<i64>("", "count").unwrap(), 0);
        db.execute_unprepared("UPDATE access_vault_links SET revoked_at=now() WHERE id='link_legacy_original'").await.unwrap();
        let rows = db.query_all(Statement::from_string(DbBackend::Postgres,
            "SELECT id,version,revoked_at IS NOT NULL AS revoked FROM sandbox_service_grants ORDER BY id")).await.unwrap();
        for row in rows {
            let id: String = row.try_get("", "id").unwrap();
            let affected = matches!(id.as_str(), "one-shot" | "renewed" | "removed");
            assert_eq!(row.try_get::<bool>("", "revoked").unwrap(), affected, "{id}");
            assert_eq!(row.try_get::<i32>("", "version").unwrap(), if affected { 2 } else { 1 }, "{id}");
        }
        // Repeated revocation/deletion does not increment already revoked grants.
        db.execute_unprepared("DELETE FROM access_vault_links WHERE id='link_legacy_original'").await.unwrap();
        let version = db.query_one(Statement::from_string(DbBackend::Postgres,
            "SELECT version FROM sandbox_service_grants WHERE id='one-shot'")).await.unwrap().unwrap();
        assert_eq!(version.try_get::<i32>("", "version").unwrap(), 2);
        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();
        db.execute_unprepared("DELETE FROM access_vault_links WHERE id='link_legacy_other'").await.unwrap();
        let other = db.query_one(Statement::from_string(DbBackend::Postgres,
            "SELECT revoked_at IS NULL AS live FROM sandbox_service_grants WHERE id='other-project'")).await.unwrap().unwrap();
        assert!(other.try_get::<bool>("", "live").unwrap());
        test.cleanup().await;
    }
}
