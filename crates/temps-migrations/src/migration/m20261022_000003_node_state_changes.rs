// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-node desired-state change log (ADR-055 §7, phase 3).
//!
//! - `node_state_changes`: one payload-free marker per object a node must
//!   re-read, written by triggers **in the object's own transaction**, so
//!   no code path can change an object without telling the nodes. `xid` is
//!   the writing transaction's id; the stream's cursor is a visibility
//!   watermark over it, not a sequence, so a transaction that took a lower
//!   id and commits later is still delivered.
//! - Markers per object:
//!   - `workload_instances`: one for the instance's node (`NULL` there is
//!     the control plane, node 0, marked as `0`), and one for the old *and*
//!     the new node when `node_id` changes, so the old node learns to stop
//!     it;
//!   - `workloads`: one for every node hosting one of its instances,
//!     written `BEFORE DELETE` so a cascading delete still finds them;
//!   - `dns_name_claims`: one with `node_id NULL` (every node serves the
//!     whole zone). A claim's object id is `hashtextextended(fqdn, 0)`:
//!     the table is keyed by name, and an expression index answers the
//!     stream's lookup by that id.
//! - `node_state_acks`: the cursor each node acknowledged, per object kind.
//! - `node_state_retention`: the horizon below which markers were deleted.
//!   A node whose cursor is below it gets a full snapshot.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The change log, its acks and retention row, and the triggers on the
/// workload tables. Public so the plane's tests can create exactly these
/// in an isolated schema (after [`crate::WORKLOAD_TABLES_SQL`]).
pub const NODE_STATE_TABLES_SQL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS node_state_changes ( \
         id BIGSERIAL PRIMARY KEY, \
         node_id INT NULL, \
         object_kind TEXT NOT NULL, \
         object_id BIGINT NOT NULL, \
         xid xid8 NOT NULL DEFAULT pg_current_xact_id(), \
         created_at TIMESTAMPTZ NOT NULL DEFAULT now() \
     )",
    // The stream's one range scan per read: `(node_id, xid)`, and the
    // `node_id IS NULL` markers through the same index.
    "CREATE INDEX IF NOT EXISTS node_state_changes_node_xid \
         ON node_state_changes (node_id, xid)",
    // The retention sweep: by cursor and by age.
    "CREATE INDEX IF NOT EXISTS node_state_changes_xid ON node_state_changes (xid)",
    "CREATE INDEX IF NOT EXISTS node_state_changes_created_at \
         ON node_state_changes (created_at)",
    "CREATE TABLE IF NOT EXISTS node_state_acks ( \
         node_id INT NOT NULL, \
         object_kind TEXT NOT NULL, \
         acked_xid xid8 NOT NULL, \
         updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
         PRIMARY KEY (node_id, object_kind) \
     )",
    "CREATE TABLE IF NOT EXISTS node_state_retention ( \
         id INT PRIMARY KEY CHECK (id = 1), \
         horizon xid8 NOT NULL DEFAULT '0'::xid8, \
         swept_at TIMESTAMPTZ NULL \
     )",
    "INSERT INTO node_state_retention (id) VALUES (1) ON CONFLICT (id) DO NOTHING",
    "CREATE OR REPLACE FUNCTION node_state_mark_instance() RETURNS trigger AS $$ \
     BEGIN \
         IF TG_OP = 'INSERT' THEN \
             INSERT INTO node_state_changes (node_id, object_kind, object_id) \
             VALUES (COALESCE(NEW.node_id, 0), 'instance', NEW.id); \
             RETURN NEW; \
         ELSIF TG_OP = 'DELETE' THEN \
             INSERT INTO node_state_changes (node_id, object_kind, object_id) \
             VALUES (COALESCE(OLD.node_id, 0), 'instance', OLD.id); \
             RETURN OLD; \
         END IF; \
         INSERT INTO node_state_changes (node_id, object_kind, object_id) \
         VALUES (COALESCE(NEW.node_id, 0), 'instance', NEW.id); \
         IF OLD.node_id IS DISTINCT FROM NEW.node_id THEN \
             INSERT INTO node_state_changes (node_id, object_kind, object_id) \
             VALUES (COALESCE(OLD.node_id, 0), 'instance', NEW.id); \
         END IF; \
         RETURN NEW; \
     END; \
     $$ LANGUAGE plpgsql",
    "DROP TRIGGER IF EXISTS workload_instances_node_state ON workload_instances",
    "DROP TRIGGER IF EXISTS workload_instances_node_state_update ON workload_instances",
    "CREATE TRIGGER workload_instances_node_state \
         AFTER INSERT OR DELETE ON workload_instances \
         FOR EACH ROW EXECUTE FUNCTION node_state_mark_instance()",
    // Only what a node acts on: its node, its desired phase, its handle,
    // its revision. A status-only update writes no marker.
    "CREATE TRIGGER workload_instances_node_state_update \
         AFTER UPDATE ON workload_instances \
         FOR EACH ROW WHEN ( \
             OLD.node_id IS DISTINCT FROM NEW.node_id \
             OR OLD.phase IS DISTINCT FROM NEW.phase \
             OR OLD.handle IS DISTINCT FROM NEW.handle \
             OR OLD.revision IS DISTINCT FROM NEW.revision \
         ) EXECUTE FUNCTION node_state_mark_instance()",
    "CREATE OR REPLACE FUNCTION node_state_mark_workload() RETURNS trigger AS $$ \
     DECLARE \
         target RECORD; \
     BEGIN \
         IF TG_OP = 'DELETE' THEN \
             target := OLD; \
         ELSE \
             target := NEW; \
         END IF; \
         INSERT INTO node_state_changes (node_id, object_kind, object_id) \
         SELECT DISTINCT COALESCE(i.node_id, 0), 'workload', target.id \
         FROM workload_instances i WHERE i.workload_id = target.id; \
         IF TG_OP = 'DELETE' THEN \
             RETURN OLD; \
         END IF; \
         RETURN NEW; \
     END; \
     $$ LANGUAGE plpgsql",
    "DROP TRIGGER IF EXISTS workloads_node_state_update ON workloads",
    "DROP TRIGGER IF EXISTS workloads_node_state_delete ON workloads",
    // A new workload has no instances yet: its first instance's insert
    // tells the node, which reads the workload with it.
    "CREATE TRIGGER workloads_node_state_update \
         AFTER UPDATE ON workloads \
         FOR EACH ROW WHEN ( \
             OLD.spec IS DISTINCT FROM NEW.spec \
             OR OLD.generation IS DISTINCT FROM NEW.generation \
             OR OLD.phase IS DISTINCT FROM NEW.phase \
             OR OLD.deleted_at IS DISTINCT FROM NEW.deleted_at \
         ) EXECUTE FUNCTION node_state_mark_workload()",
    // BEFORE: the cascade that deletes the instances runs before any
    // AFTER trigger on the workload would see them.
    "CREATE TRIGGER workloads_node_state_delete \
         BEFORE DELETE ON workloads \
         FOR EACH ROW EXECUTE FUNCTION node_state_mark_workload()",
];

/// The trigger on `dns_name_claims` (created by `m20261022_000001`).
pub const NODE_STATE_DNS_SQL: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS dns_name_claims_object_id \
         ON dns_name_claims (hashtextextended(fqdn, 0))",
    "CREATE OR REPLACE FUNCTION node_state_mark_dns_claim() RETURNS trigger AS $$ \
     BEGIN \
         IF TG_OP = 'DELETE' THEN \
             INSERT INTO node_state_changes (node_id, object_kind, object_id) \
             VALUES (NULL, 'dns_claim', hashtextextended(OLD.fqdn, 0)); \
             RETURN OLD; \
         END IF; \
         INSERT INTO node_state_changes (node_id, object_kind, object_id) \
         VALUES (NULL, 'dns_claim', hashtextextended(NEW.fqdn, 0)); \
         IF TG_OP = 'UPDATE' AND OLD.fqdn IS DISTINCT FROM NEW.fqdn THEN \
             INSERT INTO node_state_changes (node_id, object_kind, object_id) \
             VALUES (NULL, 'dns_claim', hashtextextended(OLD.fqdn, 0)); \
         END IF; \
         RETURN NEW; \
     END; \
     $$ LANGUAGE plpgsql",
    "DROP TRIGGER IF EXISTS dns_name_claims_node_state ON dns_name_claims",
    "CREATE TRIGGER dns_name_claims_node_state \
         AFTER INSERT OR UPDATE OR DELETE ON dns_name_claims \
         FOR EACH ROW EXECUTE FUNCTION node_state_mark_dns_claim()",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for s in NODE_STATE_TABLES_SQL.iter().chain(NODE_STATE_DNS_SQL) {
            conn.execute_unprepared(s).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let stmts = [
            "DROP TRIGGER IF EXISTS dns_name_claims_node_state ON dns_name_claims",
            "DROP FUNCTION IF EXISTS node_state_mark_dns_claim()",
            "DROP INDEX IF EXISTS dns_name_claims_object_id",
            "DROP TRIGGER IF EXISTS workloads_node_state_delete ON workloads",
            "DROP TRIGGER IF EXISTS workloads_node_state_update ON workloads",
            "DROP FUNCTION IF EXISTS node_state_mark_workload()",
            "DROP TRIGGER IF EXISTS workload_instances_node_state_update ON workload_instances",
            "DROP TRIGGER IF EXISTS workload_instances_node_state ON workload_instances",
            "DROP FUNCTION IF EXISTS node_state_mark_instance()",
            "DROP TABLE IF EXISTS node_state_retention",
            "DROP TABLE IF EXISTS node_state_acks",
            "DROP TABLE IF EXISTS node_state_changes",
        ];
        for s in stmts {
            conn.execute_unprepared(s).await?;
        }
        Ok(())
    }
}
