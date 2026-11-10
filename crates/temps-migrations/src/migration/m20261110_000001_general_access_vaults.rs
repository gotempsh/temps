// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Separate vault eligibility from historical project ownership. Existing
//! sessions retain their consent and deadlines and acquire their original link.
//! Downgrade refuses to discard links or authorization provenance.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE sandbox_access_services ADD COLUMN IF NOT EXISTS retired_at timestamptz;
ALTER TABLE access_vaults ADD COLUMN IF NOT EXISTS retired_at timestamptz;
ALTER TABLE access_bundles ADD COLUMN IF NOT EXISTS retired_at timestamptz;
CREATE TABLE IF NOT EXISTS access_vault_links (
 id text PRIMARY KEY,
 vault_id text NOT NULL REFERENCES access_vaults(id) ON DELETE RESTRICT,
 project_id integer REFERENCES projects(id) ON DELETE CASCADE,
 sandbox_id integer REFERENCES sandboxes(id) ON DELETE CASCADE,
 created_at timestamptz NOT NULL DEFAULT now(),
 revoked_at timestamptz,
 CHECK ((project_id IS NOT NULL)::integer + (sandbox_id IS NOT NULL)::integer = 1)
);
CREATE UNIQUE INDEX IF NOT EXISTS access_vault_links_project ON access_vault_links(vault_id, project_id) WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS access_vault_links_sandbox ON access_vault_links(vault_id, sandbox_id) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS access_vault_links_project_lookup ON access_vault_links(project_id, created_at DESC, id DESC) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS access_vault_links_sandbox_lookup ON access_vault_links(sandbox_id, created_at DESC, id DESC) WHERE revoked_at IS NULL;
INSERT INTO access_vault_links(id, vault_id, project_id, created_at)
 SELECT 'link_legacy_' || id, id, project_id, created_at FROM access_vaults WHERE project_id IS NOT NULL
 ON CONFLICT DO NOTHING;
ALTER TABLE access_sessions ADD COLUMN IF NOT EXISTS link_id text REFERENCES access_vault_links(id) ON DELETE SET NULL;
DO $$ DECLARE affected integer; BEGIN
 LOOP
  WITH batch AS (
   SELECT s.id,l.id AS link_id FROM access_sessions s JOIN access_vault_links l
     ON l.vault_id=s.vault_id AND l.project_id=s.project_id
   WHERE s.link_id IS NULL ORDER BY s.id LIMIT 1000
  ) UPDATE access_sessions s SET link_id=batch.link_id FROM batch WHERE s.id=batch.id;
  GET DIAGNOSTICS affected=ROW_COUNT;
  EXIT WHEN affected=0;
 END LOOP;
END $$;
CREATE INDEX IF NOT EXISTS access_sessions_link ON access_sessions(link_id);
CREATE INDEX IF NOT EXISTS access_sessions_grants ON access_sessions USING gin(grant_ids);
CREATE INDEX IF NOT EXISTS access_vaults_page ON access_vaults(created_at DESC,id DESC) WHERE retired_at IS NULL;
CREATE INDEX IF NOT EXISTS access_vaults_services ON access_vaults USING gin(service_ids);
CREATE INDEX IF NOT EXISTS access_bundles_page ON access_bundles(vault_id,created_at DESC,id DESC) WHERE retired_at IS NULL;
CREATE INDEX IF NOT EXISTS access_sessions_page ON access_sessions(created_at DESC,id DESC);
CREATE INDEX IF NOT EXISTS access_sessions_sandbox ON access_sessions(sandbox_id,created_at DESC,id DESC);
CREATE INDEX IF NOT EXISTS sandbox_access_services_page ON sandbox_access_services(created_at DESC,id DESC);
ALTER TABLE access_vaults ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE access_bundles ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE sandbox_access_services ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE access_vaults DROP CONSTRAINT IF EXISTS access_vaults_project_id_fkey;
ALTER TABLE access_vaults ADD CONSTRAINT access_vaults_project_id_fkey FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL;
ALTER TABLE sandbox_access_services DROP CONSTRAINT IF EXISTS sandbox_access_services_project_id_fkey;
ALTER TABLE sandbox_access_services ADD CONSTRAINT sandbox_access_services_project_id_fkey FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL;
ALTER TABLE access_bundles DROP CONSTRAINT IF EXISTS access_bundles_project_id_vault_id_fkey;
ALTER TABLE access_bundles ADD CONSTRAINT access_bundles_vault_id_fkey FOREIGN KEY(vault_id) REFERENCES access_vaults(id) ON DELETE RESTRICT;
ALTER TABLE access_sessions DROP CONSTRAINT IF EXISTS access_sessions_project_id_vault_id_fkey;
ALTER TABLE access_sessions ADD CONSTRAINT access_sessions_vault_id_fkey FOREIGN KEY(vault_id) REFERENCES access_vaults(id) ON DELETE RESTRICT;
ALTER TABLE sandbox_service_grants DROP CONSTRAINT IF EXISTS sandbox_service_grants_service_id_project_id_fkey;
ALTER TABLE sandbox_service_grants ADD CONSTRAINT sandbox_service_grants_service_id_fkey FOREIGN KEY(service_id) REFERENCES sandbox_access_services(id) ON DELETE RESTRICT;
ALTER TABLE access_sessions ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE sandbox_service_grants ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE sandbox_access_runtimes ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE sandbox_access_authorizations ALTER COLUMN project_id DROP NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS sandbox_service_grant_consumer ON sandbox_service_grants(id,sandbox_id);
CREATE UNIQUE INDEX IF NOT EXISTS sandbox_access_runtime_consumer ON sandbox_access_runtimes(id,sandbox_id);
ALTER TABLE sandbox_access_authorizations ADD CONSTRAINT sandbox_access_authorizations_grant_consumer FOREIGN KEY(grant_id,sandbox_id) REFERENCES sandbox_service_grants(id,sandbox_id) ON DELETE CASCADE;
ALTER TABLE sandbox_access_authorizations ADD CONSTRAINT sandbox_access_authorizations_runtime_consumer FOREIGN KEY(runtime_id,sandbox_id) REFERENCES sandbox_access_runtimes(id,sandbox_id) ON DELETE CASCADE;
CREATE OR REPLACE FUNCTION access_vault_link_revoked() RETURNS trigger AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  UPDATE sandbox_service_grants g SET revoked_at=now(), version=g.version+1
   WHERE g.revoked_at IS NULL AND EXISTS (SELECT 1 FROM access_sessions s WHERE s.link_id=OLD.id AND s.grant_ids @> ARRAY[g.id]);
  UPDATE access_sessions SET revoked_at=now() WHERE link_id=OLD.id AND revoked_at IS NULL;
  RETURN OLD;
 END IF;
 IF OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL THEN
  UPDATE sandbox_service_grants g SET revoked_at=NEW.revoked_at, version=g.version+1
   WHERE g.revoked_at IS NULL AND EXISTS (
    SELECT 1 FROM access_sessions s WHERE s.link_id=OLD.id AND s.grant_ids @> ARRAY[g.id]);
  UPDATE access_sessions SET revoked_at=NEW.revoked_at WHERE link_id=OLD.id AND revoked_at IS NULL;
 END IF;
 RETURN NULL;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS access_vault_link_revocation ON access_vault_links;
CREATE TRIGGER access_vault_link_revocation AFTER UPDATE OF revoked_at ON access_vault_links
 FOR EACH ROW EXECUTE FUNCTION access_vault_link_revoked();
DROP TRIGGER IF EXISTS access_vault_link_deletion ON access_vault_links;
CREATE TRIGGER access_vault_link_deletion BEFORE DELETE ON access_vault_links
 FOR EACH ROW EXECUTE FUNCTION access_vault_link_revoked();
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Empty databases remain reversible; never reinterpret live approvals.
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM access_vaults) OR EXISTS(SELECT 1 FROM access_bundles)
 OR EXISTS(SELECT 1 FROM access_sessions) OR EXISTS(SELECT 1 FROM sandbox_access_services)
 OR EXISTS(SELECT 1 FROM sandbox_service_grants) OR EXISTS(SELECT 1 FROM sandbox_access_runtimes)
 OR EXISTS(SELECT 1 FROM sandbox_access_authorizations) THEN
  RAISE EXCEPTION 'Cannot downgrade general access vaults while credential or authorization data exists';
 END IF;
END $$;
ALTER TABLE access_sessions DROP COLUMN link_id;
DROP TABLE access_vault_links;
DROP FUNCTION access_vault_link_revoked();
DROP INDEX access_sessions_grants;
DROP INDEX access_vaults_page;
DROP INDEX access_vaults_services;
DROP INDEX access_bundles_page;
DROP INDEX access_sessions_page;
DROP INDEX access_sessions_sandbox;
DROP INDEX sandbox_access_services_page;
ALTER TABLE sandbox_access_authorizations DROP CONSTRAINT sandbox_access_authorizations_grant_consumer;
ALTER TABLE sandbox_access_authorizations DROP CONSTRAINT sandbox_access_authorizations_runtime_consumer;
DROP INDEX sandbox_service_grant_consumer;
DROP INDEX sandbox_access_runtime_consumer;
ALTER TABLE access_vaults DROP CONSTRAINT access_vaults_project_id_fkey;
ALTER TABLE access_vaults ADD CONSTRAINT access_vaults_project_id_fkey FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE;
ALTER TABLE sandbox_access_services DROP CONSTRAINT sandbox_access_services_project_id_fkey;
ALTER TABLE sandbox_access_services ADD CONSTRAINT sandbox_access_services_project_id_fkey FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE;
ALTER TABLE access_bundles DROP CONSTRAINT access_bundles_vault_id_fkey;
ALTER TABLE access_bundles ADD CONSTRAINT access_bundles_project_id_vault_id_fkey FOREIGN KEY(project_id,vault_id) REFERENCES access_vaults(project_id,id) ON DELETE CASCADE;
ALTER TABLE access_sessions DROP CONSTRAINT access_sessions_vault_id_fkey;
ALTER TABLE access_sessions ADD CONSTRAINT access_sessions_project_id_vault_id_fkey FOREIGN KEY(project_id,vault_id) REFERENCES access_vaults(project_id,id) ON DELETE CASCADE;
ALTER TABLE sandbox_service_grants DROP CONSTRAINT sandbox_service_grants_service_id_fkey;
ALTER TABLE sandbox_service_grants ADD CONSTRAINT sandbox_service_grants_service_id_project_id_fkey FOREIGN KEY(service_id,project_id) REFERENCES sandbox_access_services(id,project_id) ON DELETE CASCADE;
ALTER TABLE access_vaults ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE access_bundles ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE sandbox_access_services ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE access_sessions ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE sandbox_service_grants ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE sandbox_access_runtimes ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE sandbox_access_authorizations ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE access_vaults DROP COLUMN retired_at;
ALTER TABLE access_bundles DROP COLUMN retired_at;
ALTER TABLE sandbox_access_services DROP COLUMN retired_at;
"#).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    #[tokio::test]
    async fn existing_vaults_keep_their_sessions_and_gain_a_project_link() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping general vault migration: Docker unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection();
        let migrations = crate::Migrator::migrations();
        let previous = migrations
            .iter()
            .position(|m| m.name() == Migration.name())
            .unwrap();
        crate::Migrator::up(db, Some(previous as u32))
            .await
            .unwrap();
        // The empty-schema rollback contract is separate from refusing to lose data.
        let manager = SchemaManager::new(db);
        Migration.up(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();
        db.execute_unprepared(r#"
INSERT INTO users(id,name,email) VALUES(9921,'Fixture','migration-fixture@example.com');
INSERT INTO projects(id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES(9921,'Fixture','repo','owner','/','main','nodejs','migration-vault-fixture',now(),now());
INSERT INTO sandboxes(id,public_id,name,user_id,project_id,status,backend) VALUES(9921,'sbx_migration_fixture','Fixture',9921,9921,'running','docker');
INSERT INTO sandbox_access_services(id,project_id,name,policy,max_ttl_seconds,enabled,credential_encrypted) VALUES('svc_migration',9921,'Fixture','{}',60,true,'encrypted-fixture');
INSERT INTO access_vaults(id,project_id,name,service_ids) VALUES('vault_migration',9921,'Fixture',ARRAY['svc_migration']);
INSERT INTO access_bundles(id,project_id,vault_id,name,service_ids,ttl_seconds) VALUES('bundle_migration',9921,'vault_migration','Fixture',ARRAY['svc_migration'],60);
INSERT INTO sandbox_service_grants(id,project_id,service_id,sandbox_id,service_version,policy_snapshot,created_by,expires_at) VALUES('grant_migration',9921,'svc_migration',9921,1,'{}',9921,now()+interval '1 hour');
INSERT INTO access_sessions(id,project_id,vault_id,bundle_id,bundle_name,bundle_version,consumer_kind,consumer_id,sandbox_id,created_by,consent,grant_ids,created_at,expires_at) VALUES('session_migration',9921,'vault_migration','bundle_migration','Fixture',1,'sandbox','sbx_migration_fixture',9921,9921,'{"immutable":"consent"}',ARRAY['grant_migration'],now(),now()+interval '1 hour');
"#).await.unwrap();
        crate::Migrator::up(db, None).await.unwrap();
        let row=db.query_one(Statement::from_string(DatabaseBackend::Postgres,
            "SELECT s.link_id,s.revoked_at IS NULL AND g.revoked_at IS NULL AS live, s.consent->>'immutable' AS consent, s.expires_at=g.expires_at AS lifetime,l.project_id FROM access_sessions s JOIN access_vault_links l ON l.id=s.link_id JOIN sandbox_service_grants g ON g.id='grant_migration' WHERE s.id='session_migration'".to_owned())).await.unwrap().unwrap();
        assert_eq!(
            row.try_get::<String>("", "link_id").unwrap(),
            "link_legacy_vault_migration"
        );
        assert_eq!(row.try_get::<i32>("", "project_id").unwrap(), 9921);
        assert_eq!(row.try_get::<String>("", "consent").unwrap(), "consent");
        assert!(row.try_get::<bool>("", "live").unwrap());
        assert!(row.try_get::<bool>("", "lifetime").unwrap());
        assert!(Migration
            .down(&SchemaManager::new(db))
            .await
            .unwrap_err()
            .to_string()
            .contains("Cannot downgrade"));
        // The retained history survives consumer deletion only until the sandbox
        // cascades; the trigger still invalidates its grants before link removal.
        db.execute_unprepared(
            "DELETE FROM access_vault_links WHERE id='link_legacy_vault_migration'",
        )
        .await
        .unwrap();
        let revoked=db.query_one(Statement::from_string(DatabaseBackend::Postgres,"SELECT revoked_at IS NOT NULL AS revoked FROM sandbox_service_grants WHERE id='grant_migration'".to_owned())).await.unwrap().unwrap();
        assert!(revoked.try_get::<bool>("", "revoked").unwrap());
        test.cleanup().await;
    }
}
