// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let the control plane (node `0`) serve private addresses (ADR-114 D1).
//!
//! `environment_domains.private_node_id`, `project_custom_domains.private_node_id`
//! and `node_public_ingress.node_id` referenced `nodes(id)`, and the control
//! plane has no `nodes` row. As in ADR-107 D1, each foreign key is replaced by
//! triggers that keep what it guaranteed for every other id:
//!
//! - before a row is written, a non-zero id must name a `nodes` row, locked
//!   `FOR KEY SHARE` exactly as the foreign key check locks it;
//! - deleting a node does what the foreign key did: `SET NULL` for
//!   `private_node_id`, `CASCADE` for `node_public_ingress`.
//!
//! `0` stays an ordinary integer rather than `NULL`, so a binary rolled back
//! after this upgrade still decodes every row. No backfill: no row uses `0`.
//!
//! `down` restores the foreign keys. It refuses while node `0` serves a
//! private address or has an ingress report: dropping them silently would
//! take the operator's private addresses down.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
DO $$
DECLARE t text; fk text;
BEGIN
    FOREACH t IN ARRAY ARRAY['environment_domains', 'project_custom_domains', 'node_public_ingress'] LOOP
        FOR fk IN
            SELECT conname FROM pg_constraint
            WHERE conrelid = t::regclass
              AND contype = 'f'
              AND confrelid = 'nodes'::regclass
        LOOP
            EXECUTE format('ALTER TABLE %I DROP CONSTRAINT %I', t, fk);
        END LOOP;
    END LOOP;
END $$;

CREATE OR REPLACE FUNCTION private_node_exists() RETURNS trigger AS $$
BEGIN
    IF NEW.private_node_id IS NOT NULL AND NEW.private_node_id <> 0 THEN
        PERFORM 1 FROM nodes WHERE id = NEW.private_node_id FOR KEY SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'node % does not exist (0 is the control plane)', NEW.private_node_id
                USING ERRCODE = 'foreign_key_violation';
        END IF;
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS environment_domains_private_node_exists ON environment_domains;
CREATE TRIGGER environment_domains_private_node_exists
    BEFORE INSERT OR UPDATE OF private_node_id ON environment_domains
    FOR EACH ROW EXECUTE FUNCTION private_node_exists();

DROP TRIGGER IF EXISTS project_custom_domains_private_node_exists ON project_custom_domains;
CREATE TRIGGER project_custom_domains_private_node_exists
    BEFORE INSERT OR UPDATE OF private_node_id ON project_custom_domains
    FOR EACH ROW EXECUTE FUNCTION private_node_exists();

CREATE OR REPLACE FUNCTION node_public_ingress_node_exists() RETURNS trigger AS $$
BEGIN
    IF NEW.node_id <> 0 THEN
        PERFORM 1 FROM nodes WHERE id = NEW.node_id FOR KEY SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'node % does not exist (0 is the control plane)', NEW.node_id
                USING ERRCODE = 'foreign_key_violation';
        END IF;
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS node_public_ingress_node_exists ON node_public_ingress;
CREATE TRIGGER node_public_ingress_node_exists
    BEFORE INSERT OR UPDATE OF node_id ON node_public_ingress
    FOR EACH ROW EXECUTE FUNCTION node_public_ingress_node_exists();

CREATE OR REPLACE FUNCTION private_ingress_drop_node() RETURNS trigger AS $$
BEGIN
    UPDATE environment_domains SET private_node_id = NULL WHERE private_node_id = OLD.id;
    UPDATE project_custom_domains SET private_node_id = NULL WHERE private_node_id = OLD.id;
    DELETE FROM node_public_ingress WHERE node_id = OLD.id;
    RETURN OLD;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS private_ingress_drop_node ON nodes;
CREATE TRIGGER private_ingress_drop_node
    AFTER DELETE ON nodes
    FOR EACH ROW EXECUTE FUNCTION private_ingress_drop_node();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    LOCK TABLE environment_domains, project_custom_domains, node_public_ingress
        IN ACCESS EXCLUSIVE MODE;
    IF EXISTS (SELECT 1 FROM environment_domains WHERE private_node_id = 0)
       OR EXISTS (SELECT 1 FROM project_custom_domains WHERE private_node_id = 0)
       OR EXISTS (SELECT 1 FROM node_public_ingress WHERE node_id = 0) THEN
        RAISE EXCEPTION 'Cannot restore the nodes foreign keys while the control plane (node 0) serves private addresses; make those addresses public or move them to a worker, and turn private ingress off, before downgrading';
    END IF;
    DROP TRIGGER IF EXISTS environment_domains_private_node_exists ON environment_domains;
    DROP TRIGGER IF EXISTS project_custom_domains_private_node_exists ON project_custom_domains;
    DROP TRIGGER IF EXISTS node_public_ingress_node_exists ON node_public_ingress;
    DROP TRIGGER IF EXISTS private_ingress_drop_node ON nodes;
    DROP FUNCTION IF EXISTS private_node_exists();
    DROP FUNCTION IF EXISTS node_public_ingress_node_exists();
    DROP FUNCTION IF EXISTS private_ingress_drop_node();
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'environment_domains'::regclass AND contype = 'f'
          AND confrelid = 'nodes'::regclass
    ) THEN
        ALTER TABLE environment_domains
            ADD CONSTRAINT environment_domains_private_node_id_fkey
            FOREIGN KEY (private_node_id) REFERENCES nodes(id) ON DELETE SET NULL;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'project_custom_domains'::regclass AND contype = 'f'
          AND confrelid = 'nodes'::regclass
    ) THEN
        ALTER TABLE project_custom_domains
            ADD CONSTRAINT project_custom_domains_private_node_id_fkey
            FOREIGN KEY (private_node_id) REFERENCES nodes(id) ON DELETE SET NULL;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'node_public_ingress'::regclass AND contype = 'f'
          AND confrelid = 'nodes'::regclass
    ) THEN
        ALTER TABLE node_public_ingress
            ADD CONSTRAINT node_public_ingress_node_id_fkey
            FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;
    END IF;
END $$;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

    async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
        db.query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "count")
            .unwrap()
    }

    #[tokio::test]
    async fn the_control_plane_serves_private_addresses_as_node_zero() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping control plane private node migration: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        db.execute_unprepared(
            "CREATE TABLE nodes (id integer PRIMARY KEY);
             CREATE TABLE environment_domains (
                 id SERIAL PRIMARY KEY,
                 private_node_id INTEGER REFERENCES nodes(id) ON DELETE SET NULL
             );
             CREATE TABLE project_custom_domains (
                 id SERIAL PRIMARY KEY,
                 private_node_id INTEGER REFERENCES nodes(id) ON DELETE SET NULL
             );
             CREATE TABLE node_public_ingress (
                 node_id INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE
             );
             INSERT INTO nodes VALUES (4), (5);
             INSERT INTO environment_domains (private_node_id) VALUES (4), (NULL);
             INSERT INTO project_custom_domains (private_node_id) VALUES (4);
             INSERT INTO node_public_ingress VALUES (4);",
        )
        .await
        .unwrap();
        // Before: the control plane is refused by the foreign keys.
        assert!(db
            .execute_unprepared("INSERT INTO environment_domains (private_node_id) VALUES (0)")
            .await
            .is_err());
        assert!(db
            .execute_unprepared("INSERT INTO node_public_ingress VALUES (0)")
            .await
            .is_err());

        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Idempotent.
        Migration.up(&manager).await.unwrap();

        db.execute_unprepared(
            "INSERT INTO environment_domains (private_node_id) VALUES (0), (5);
             INSERT INTO project_custom_domains (private_node_id) VALUES (0), (NULL);
             INSERT INTO node_public_ingress VALUES (0), (5);",
        )
        .await
        .unwrap();
        for sql in [
            "INSERT INTO environment_domains (private_node_id) VALUES (9)",
            "INSERT INTO project_custom_domains (private_node_id) VALUES (9)",
            "INSERT INTO node_public_ingress VALUES (9)",
            "UPDATE environment_domains SET private_node_id = 9 WHERE private_node_id = 5",
        ] {
            let err = db.execute_unprepared(sql).await.expect_err(sql);
            assert!(err.to_string().contains("node 9 does not exist"), "{sql}: {err}");
        }

        // Deleting a worker clears what it served, and only that.
        db.execute_unprepared("DELETE FROM nodes WHERE id = 4")
            .await
            .unwrap();
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS count FROM environment_domains WHERE private_node_id IS NULL"
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS count FROM project_custom_domains WHERE private_node_id = 0"
            )
            .await,
            1
        );
        assert_eq!(
            count(&db, "SELECT count(*) AS count FROM node_public_ingress").await,
            2,
            "node 4's report is gone, node 0's and node 5's stay"
        );

        // Down refuses while the control plane serves privately.
        let refused = Migration.down(&manager).await.expect_err("node 0 serves");
        assert!(refused.to_string().contains("control plane"), "{refused}");
        db.execute_unprepared(
            "UPDATE environment_domains SET private_node_id = NULL WHERE private_node_id = 0;
             UPDATE project_custom_domains SET private_node_id = NULL WHERE private_node_id = 0;
             DELETE FROM node_public_ingress WHERE node_id = 0;",
        )
        .await
        .unwrap();
        Migration.down(&manager).await.unwrap();
        assert!(db
            .execute_unprepared("INSERT INTO node_public_ingress VALUES (0)")
            .await
            .is_err());
        db.execute_unprepared("DELETE FROM nodes WHERE id = 5")
            .await
            .unwrap();
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS count FROM environment_domains WHERE private_node_id IS NOT NULL"
            )
            .await,
            0,
            "the restored foreign key sets null again"
        );
        assert_eq!(
            count(&db, "SELECT count(*) AS count FROM node_public_ingress").await,
            0,
            "the restored foreign key cascades again"
        );
        test.cleanup().await;
    }
}
