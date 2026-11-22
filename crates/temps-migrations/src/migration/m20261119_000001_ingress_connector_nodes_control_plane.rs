// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let the control plane (node `0`) run an ingress connector (ADR-107 D1).
//!
//! `ingress_connector_nodes.node_id` referenced `nodes(id)`, and the control
//! plane has no `nodes` row. The foreign key is replaced by two triggers that
//! keep what it guaranteed for every other id:
//!
//! - before a row is written, a non-zero `node_id` must name a `nodes` row,
//!   locked `FOR KEY SHARE` exactly as the foreign key check locks it, so a
//!   concurrent node delete cannot leave an orphan;
//! - deleting a node deletes its rows, as `ON DELETE CASCADE` did.
//!
//! `0` stays an ordinary integer rather than `NULL`, so a binary rolled back
//! after this upgrade still decodes every row (it shows the control-plane
//! replica as an unknown node and never runs it). No backfill: no existing
//! row uses `0`.
//!
//! `down` restores the foreign key. It refuses while a connector is placed on
//! the control plane: dropping that placement silently would take the
//! operator's tunnel down. Move the connector off node `0` first.

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
DO $$
DECLARE fk text;
BEGIN
    FOR fk IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'ingress_connector_nodes'::regclass
          AND contype = 'f'
          AND confrelid = 'nodes'::regclass
    LOOP
        EXECUTE format('ALTER TABLE ingress_connector_nodes DROP CONSTRAINT %I', fk);
    END LOOP;
END $$;

CREATE OR REPLACE FUNCTION ingress_connector_nodes_node_exists() RETURNS trigger AS $$
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

DROP TRIGGER IF EXISTS ingress_connector_nodes_node_exists ON ingress_connector_nodes;
CREATE TRIGGER ingress_connector_nodes_node_exists
    BEFORE INSERT OR UPDATE OF node_id ON ingress_connector_nodes
    FOR EACH ROW EXECUTE FUNCTION ingress_connector_nodes_node_exists();

CREATE OR REPLACE FUNCTION ingress_connector_nodes_drop_node() RETURNS trigger AS $$
BEGIN
    DELETE FROM ingress_connector_nodes WHERE node_id = OLD.id;
    RETURN OLD;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS ingress_connector_nodes_drop_node ON nodes;
CREATE TRIGGER ingress_connector_nodes_drop_node
    AFTER DELETE ON nodes
    FOR EACH ROW EXECUTE FUNCTION ingress_connector_nodes_drop_node();
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
DO $$
BEGIN
    LOCK TABLE ingress_connector_nodes IN ACCESS EXCLUSIVE MODE;
    IF EXISTS (SELECT 1 FROM ingress_connector_nodes WHERE node_id = 0) THEN
        RAISE EXCEPTION 'Cannot restore the nodes foreign key on ingress_connector_nodes while a connector runs on the control plane (node 0); move those connectors to a worker or delete them before downgrading';
    END IF;
    DROP TRIGGER IF EXISTS ingress_connector_nodes_node_exists ON ingress_connector_nodes;
    DROP TRIGGER IF EXISTS ingress_connector_nodes_drop_node ON nodes;
    DROP FUNCTION IF EXISTS ingress_connector_nodes_node_exists();
    DROP FUNCTION IF EXISTS ingress_connector_nodes_drop_node();
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'ingress_connector_nodes'::regclass
          AND contype = 'f'
          AND confrelid = 'nodes'::regclass
    ) THEN
        ALTER TABLE ingress_connector_nodes
            ADD CONSTRAINT ingress_connector_nodes_node_id_fkey
            FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;
    END IF;
END $$;
"#,
            )
            .await?;
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
    async fn the_control_plane_is_node_zero_and_workers_keep_their_reference() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping ingress connector node migration: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::new().await.unwrap();
        let db = test.connection_arc();
        db.execute_unprepared(
            "CREATE TABLE nodes (id integer PRIMARY KEY);
             CREATE TABLE ingress_connectors (id integer PRIMARY KEY);
             CREATE TABLE ingress_connector_nodes (
                 id SERIAL PRIMARY KEY,
                 connector_id INTEGER NOT NULL REFERENCES ingress_connectors(id) ON DELETE CASCADE,
                 node_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                 UNIQUE (connector_id, node_id)
             );
             INSERT INTO nodes VALUES (4), (5);
             INSERT INTO ingress_connectors VALUES (1);
             INSERT INTO ingress_connector_nodes (connector_id, node_id) VALUES (1, 4);",
        )
        .await
        .unwrap();
        // Before: the control plane is refused by the foreign key.
        assert!(db
            .execute_unprepared(
                "INSERT INTO ingress_connector_nodes (connector_id, node_id) VALUES (1, 0)"
            )
            .await
            .is_err());

        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await.unwrap();
        // Idempotent.
        Migration.up(&manager).await.unwrap();

        db.execute_unprepared(
            "INSERT INTO ingress_connector_nodes (connector_id, node_id) VALUES (1, 0), (1, 5)",
        )
        .await
        .unwrap();
        let unknown = db
            .execute_unprepared(
                "INSERT INTO ingress_connector_nodes (connector_id, node_id) VALUES (1, 9)",
            )
            .await
            .expect_err("node 9 has no row");
        assert!(unknown.to_string().contains("node 9 does not exist"), "{unknown}");
        assert!(db
            .execute_unprepared("UPDATE ingress_connector_nodes SET node_id = 9 WHERE node_id = 5")
            .await
            .is_err());
        // The unique placement still holds for the control plane.
        assert!(db
            .execute_unprepared(
                "INSERT INTO ingress_connector_nodes (connector_id, node_id) VALUES (1, 0)"
            )
            .await
            .is_err());

        // Deleting a worker still removes its placements, and only its own.
        db.execute_unprepared("DELETE FROM nodes WHERE id = 4")
            .await
            .unwrap();
        assert_eq!(
            count(&db, "SELECT count(*) AS count FROM ingress_connector_nodes").await,
            2
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS count FROM ingress_connector_nodes WHERE node_id = 4"
            )
            .await,
            0
        );

        // Down refuses while the control plane runs a connector.
        let refused = Migration.down(&manager).await.expect_err("node 0 placed");
        assert!(refused.to_string().contains("control plane"), "{refused}");
        db.execute_unprepared("DELETE FROM ingress_connector_nodes WHERE node_id = 0")
            .await
            .unwrap();
        Migration.down(&manager).await.unwrap();
        assert!(db
            .execute_unprepared(
                "INSERT INTO ingress_connector_nodes (connector_id, node_id) VALUES (1, 0)"
            )
            .await
            .is_err());
        db.execute_unprepared("DELETE FROM nodes WHERE id = 5")
            .await
            .unwrap();
        assert_eq!(
            count(&db, "SELECT count(*) AS count FROM ingress_connector_nodes").await,
            0,
            "the restored foreign key cascades again"
        );
        test.cleanup().await;
    }
}
