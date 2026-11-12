// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attribute vault-related audit events to their vaults when they are written,
//! so a vault's audit page is one bounded index walk instead of one probe per
//! session, bundle, link, service and service version the vault ever had.
//!
//! `access_vault_audit_events` holds, per vault, the events the vault audit
//! reader used to find by enumerating the vault's children at read time:
//!
//! - events carrying a `service_scope_key`, for every vault that scope names
//!   in `access_service_audit_scopes`;
//! - events carrying neither `vault_id` nor `service_scope_key` whose
//!   `resource_id` is one of a vault's bundles, sessions or links, or a
//!   service the vault holds or held (`access_vault_service_history`).
//!
//! Events whose own payload names the vault (`resource_id` or `vault_id`) are
//! read straight from their `audit_logs` indexes and are not copied here.
//!
//! The vault set of an event can grow after it is written: a link captures a
//! scope row for a service version that already has events, and a service
//! joining a vault brings its unscoped history with it. `access_audit_event_keys`
//! records, per scope key and per service, the events such a later membership
//! row must pick up, so those triggers never scan `audit_logs`.
//!
//! An event and a membership row written by concurrent transactions would each
//! miss the other under READ COMMITTED. Both sides take a transaction advisory
//! lock on the key they share (`scope:<K>` or `service:<id>`) before reading the
//! other side: an event takes it exclusive, a membership row shared. Whichever
//! commits second then sees the first. An event takes at most one lock and
//! holds no other, so it can never be part of a deadlock cycle, and shared locks
//! never wait for each other.
//!
//! Audit rows are never modified. Events written before this migration are
//! attributed by `backfill_vault_audit_attribution` in committed, resumable
//! batches after startup. Until it finishes, readers keep the read-time
//! enumeration, so no event disappears in between.

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
CREATE TABLE IF NOT EXISTS access_vault_audit_events (
 vault_id text NOT NULL,
 audit_log_id integer NOT NULL REFERENCES audit_logs(id) ON DELETE CASCADE,
 audit_date timestamptz NOT NULL,
 PRIMARY KEY (vault_id, audit_log_id)
);
CREATE INDEX IF NOT EXISTS access_vault_audit_events_recent
 ON access_vault_audit_events(vault_id, audit_date DESC, audit_log_id DESC);
CREATE INDEX IF NOT EXISTS access_vault_audit_events_log ON access_vault_audit_events(audit_log_id);
CREATE TABLE IF NOT EXISTS access_audit_event_keys (
 key text NOT NULL,
 audit_log_id integer NOT NULL REFERENCES audit_logs(id) ON DELETE CASCADE,
 audit_date timestamptz NOT NULL,
 PRIMARY KEY (key, audit_log_id)
);
CREATE INDEX IF NOT EXISTS access_audit_event_keys_log ON access_audit_event_keys(audit_log_id);
CREATE INDEX IF NOT EXISTS access_vault_service_history_service
 ON access_vault_service_history(service_id, vault_id);
-- Vaults untouched since their service history began hold services that are
-- not in it yet (the reader added `service_ids` for them). Record them, so
-- history alone is a vault's whole service membership from here on; the
-- existing trigger keeps it that way.
INSERT INTO access_vault_service_history(vault_id, service_id)
 SELECT id, unnest(service_ids) FROM access_vaults ON CONFLICT DO NOTHING;

-- The vaults one event belongs to, written for it and for the keys a later
-- membership row looks it up by. `d` is the event's parsed payload. `p_lock`
-- takes the event's key lock (see above); the backfill passes false because it
-- holds SHARE locks on both membership tables for its whole batch instead.
CREATE OR REPLACE FUNCTION access_vault_audit_attribute(p_id integer, p_date timestamptz, d jsonb, p_lock boolean)
RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
 v_scope text := d->>'service_scope_key';
 v_resource text := d->>'resource_id';
 v_rows integer := 0;
 v_count integer;
BEGIN
 IF d IS NULL THEN
  RETURN 0;
 END IF;
 IF v_scope IS NOT NULL THEN
  IF p_lock THEN
   PERFORM pg_advisory_xact_lock(hashtextextended('access-vault-audit:scope:' || v_scope, 0));
  END IF;
  INSERT INTO access_audit_event_keys(key, audit_log_id, audit_date)
   VALUES ('scope:' || v_scope, p_id, p_date) ON CONFLICT DO NOTHING;
  INSERT INTO access_vault_audit_events(vault_id, audit_log_id, audit_date)
   SELECT s.vault_id, p_id, p_date FROM access_service_audit_scopes s WHERE s.scope_key = v_scope
   ON CONFLICT DO NOTHING;
  GET DIAGNOSTICS v_count = ROW_COUNT;
  v_rows := v_rows + v_count;
 END IF;
 IF v_resource IS NOT NULL AND NOT (d ? 'vault_id' OR d ? 'service_scope_key') THEN
  IF p_lock THEN
   PERFORM pg_advisory_xact_lock(hashtextextended('access-vault-audit:service:' || v_resource, 0));
  END IF;
  INSERT INTO access_vault_audit_events(vault_id, audit_log_id, audit_date)
   SELECT owner.vault_id, p_id, p_date FROM (
    SELECT vault_id FROM access_bundles WHERE id = v_resource
    UNION SELECT vault_id FROM access_sessions WHERE id = v_resource
    UNION SELECT vault_id FROM access_vault_links WHERE id = v_resource
    UNION SELECT vault_id FROM access_vault_service_history WHERE service_id = v_resource
   ) owner WHERE owner.vault_id IS NOT NULL
   ON CONFLICT DO NOTHING;
  GET DIAGNOSTICS v_count = ROW_COUNT;
  v_rows := v_rows + v_count;
  IF EXISTS (SELECT 1 FROM sandbox_access_services WHERE id = v_resource)
     OR EXISTS (SELECT 1 FROM access_vault_service_history WHERE service_id = v_resource) THEN
   INSERT INTO access_audit_event_keys(key, audit_log_id, audit_date)
    VALUES ('service:' || v_resource, p_id, p_date) ON CONFLICT DO NOTHING;
  END IF;
 END IF;
 RETURN v_rows;
END $$;

CREATE OR REPLACE FUNCTION access_vault_audit_on_log() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
 PERFORM access_vault_audit_attribute(NEW.id, NEW.audit_date, temps_audit_json(NEW.data), true);
 RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS access_vault_audit_on_log ON audit_logs;
CREATE TRIGGER access_vault_audit_on_log AFTER INSERT ON audit_logs
 FOR EACH ROW EXECUTE FUNCTION access_vault_audit_on_log();

-- A scope row captured after its version's events were written.
CREATE OR REPLACE FUNCTION access_vault_audit_on_scope() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
 PERFORM pg_advisory_xact_lock_shared(hashtextextended('access-vault-audit:scope:' || NEW.scope_key, 0));
 INSERT INTO access_vault_audit_events(vault_id, audit_log_id, audit_date)
  SELECT NEW.vault_id, k.audit_log_id, k.audit_date FROM access_audit_event_keys k
  WHERE k.key = 'scope:' || NEW.scope_key ON CONFLICT DO NOTHING;
 RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS access_vault_audit_on_scope ON access_service_audit_scopes;
CREATE TRIGGER access_vault_audit_on_scope AFTER INSERT ON access_service_audit_scopes
 FOR EACH ROW EXECUTE FUNCTION access_vault_audit_on_scope();

-- A service joining a vault brings its unscoped events, as the reader did.
CREATE OR REPLACE FUNCTION access_vault_audit_on_service_history() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
 PERFORM pg_advisory_xact_lock_shared(hashtextextended('access-vault-audit:service:' || NEW.service_id, 0));
 INSERT INTO access_vault_audit_events(vault_id, audit_log_id, audit_date)
  SELECT NEW.vault_id, k.audit_log_id, k.audit_date FROM access_audit_event_keys k
  WHERE k.key = 'service:' || NEW.service_id ON CONFLICT DO NOTHING;
 RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS access_vault_audit_on_service_history ON access_vault_service_history;
CREATE TRIGGER access_vault_audit_on_service_history AFTER INSERT ON access_vault_service_history
 FOR EACH ROW EXECUTE FUNCTION access_vault_audit_on_service_history();

-- Events up to here are attributed by the backfill; later ones by the
-- trigger, created above in this transaction, so none falls between.
CREATE TABLE IF NOT EXISTS access_vault_audit_backfill (
 singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
 cursor_id integer NOT NULL DEFAULT 0,
 upper_id integer NOT NULL
);
-- A database with no audit rows has nothing to backfill and gets no cursor.
INSERT INTO access_vault_audit_backfill(singleton, upper_id)
 SELECT true, max(id) FROM audit_logs HAVING max(id) IS NOT NULL ON CONFLICT DO NOTHING;
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Attribution is derived from the audit rows and membership tables,
        // which stay; an older reader enumerates children again. The service
        // history rows recorded above match what that reader already merged
        // from `service_ids`, so they are kept.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP TRIGGER IF EXISTS access_vault_audit_on_log ON audit_logs;
DROP TRIGGER IF EXISTS access_vault_audit_on_scope ON access_service_audit_scopes;
DROP TRIGGER IF EXISTS access_vault_audit_on_service_history ON access_vault_service_history;
DROP FUNCTION IF EXISTS access_vault_audit_on_log();
DROP FUNCTION IF EXISTS access_vault_audit_on_scope();
DROP FUNCTION IF EXISTS access_vault_audit_on_service_history();
DROP FUNCTION IF EXISTS access_vault_audit_attribute(integer, timestamptz, jsonb, boolean);
DROP TABLE IF EXISTS access_vault_audit_backfill;
DROP TABLE IF EXISTS access_audit_event_keys;
DROP TABLE IF EXISTS access_vault_audit_events;
DROP INDEX IF EXISTS access_vault_service_history_service;
"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

    async fn attributed(db: &sea_orm::DatabaseConnection, vault_id: &str) -> Vec<i32> {
        db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT audit_log_id FROM access_vault_audit_events WHERE vault_id=$1 ORDER BY audit_log_id",
            [vault_id.into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get("", "audit_log_id").unwrap())
        .collect()
    }

    async fn insert_event(db: &sea_orm::DatabaseConnection, data: &str) -> i32 {
        db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO audit_logs(user_agent,operation_type,data,audit_date,created_at) VALUES('fixture','FIXTURE',$1,now(),now()) RETURNING id",
            [data.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "id")
        .unwrap()
    }

    /// Writes attribute events, later membership rows pick up the events they
    /// cover, and the reversal drops only the derived tables.
    #[tokio::test]
    async fn events_are_attributed_on_write_and_on_later_membership() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping vault audit attribution: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection();
        db.execute_unprepared(
            "INSERT INTO access_vaults(id,name,service_ids) VALUES('vault_a','A',ARRAY['svc_fixture']),('vault_b','B','{}');
             INSERT INTO access_service_audit_scopes VALUES('svc_fixture:1','vault_a');",
        )
        .await
        .unwrap();
        let scoped = insert_event(
            db,
            r#"{"resource_id":"svc_fixture","service_scope_key":"svc_fixture:1"}"#,
        )
        .await;
        let unscoped = insert_event(db, r#"{"resource_id":"svc_fixture"}"#).await;
        let own = insert_event(db, r#"{"resource_id":"vault_a"}"#).await;
        let named = insert_event(db, r#"{"resource_id":"svc_fixture","vault_id":"vault_a"}"#).await;
        insert_event(db, "not JSON").await;
        // Events naming the vault themselves are read from their own indexes.
        assert_eq!(attributed(db, "vault_a").await, vec![scoped, unscoped]);
        assert!(!attributed(db, "vault_a").await.contains(&own));
        assert!(!attributed(db, "vault_a").await.contains(&named));
        assert!(attributed(db, "vault_b").await.is_empty());

        // A link captures the existing version's scope for vault_b, and the
        // service joins vault_b: both bring their earlier events along.
        db.execute_unprepared(
            "INSERT INTO access_service_audit_scopes VALUES('svc_fixture:1','vault_b')",
        )
        .await
        .unwrap();
        assert_eq!(attributed(db, "vault_b").await, vec![scoped]);
        db.execute_unprepared("UPDATE access_vaults SET service_ids=ARRAY['svc_fixture'] WHERE id='vault_b'")
            .await
            .unwrap();
        assert_eq!(attributed(db, "vault_b").await, vec![scoped, unscoped]);

        // Audit retention deletes attribution with its row.
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM audit_logs WHERE id=$1",
            [unscoped.into()],
        ))
        .await
        .unwrap();
        assert_eq!(attributed(db, "vault_a").await, vec![scoped]);

        let manager = SchemaManager::new(db);
        Migration.down(&manager).await.unwrap();
        let gone: bool = db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT to_regclass('access_vault_audit_events') IS NULL AND to_regclass('access_audit_event_keys') IS NULL AS gone",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "gone")
            .unwrap();
        assert!(gone);
        // Writes keep working without the triggers.
        insert_event(db, r#"{"resource_id":"svc_fixture"}"#).await;
        Migration.up(&manager).await.unwrap();
        test.cleanup().await;
    }

    /// An event and a membership row written by concurrent transactions:
    /// whichever commits second sees the first, in both orders.
    #[tokio::test]
    async fn concurrent_events_and_membership_rows_see_each_other() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping concurrent vault audit attribution: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection_arc();
        db.execute_unprepared(
            "INSERT INTO access_vaults(id,name,service_ids) VALUES('vault_a','A','{}'),('vault_c','C',ARRAY['svc_fixture'])",
        )
        .await
        .unwrap();
        let pause = std::time::Duration::from_millis(300);

        // A link's scope row is still uncommitted when the version's event is
        // written: the event waits for it, then sees it.
        let tx = db.begin().await.unwrap();
        tx.execute_unprepared(
            "INSERT INTO access_service_audit_scopes VALUES('svc_fixture:1','vault_a')",
        )
        .await
        .unwrap();
        let writer = {
            let db = db.clone();
            tokio::spawn(async move {
                insert_event(
                    &db,
                    r#"{"resource_id":"svc_fixture","service_scope_key":"svc_fixture:1"}"#,
                )
                .await
            })
        };
        tokio::time::sleep(pause).await;
        assert!(!writer.is_finished(), "the event did not wait for the scope row");
        tx.commit().await.unwrap();
        let scoped = writer.await.unwrap();
        assert_eq!(attributed(&db, "vault_a").await, vec![scoped]);

        // An unscoped event is still uncommitted when its service joins the
        // vault: the join waits for it, then picks it up.
        let tx = db.begin().await.unwrap();
        let unscoped: i32 = tx
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                r#"INSERT INTO audit_logs(user_agent,operation_type,data,audit_date,created_at) VALUES('fixture','FIXTURE','{"resource_id":"svc_fixture"}',now(),now()) RETURNING id"#,
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "id")
            .unwrap();
        let joiner = {
            let db = db.clone();
            tokio::spawn(async move {
                db.execute_unprepared(
                    "UPDATE access_vaults SET service_ids=ARRAY['svc_fixture'] WHERE id='vault_a'",
                )
                .await
                .unwrap();
            })
        };
        tokio::time::sleep(pause).await;
        assert!(!joiner.is_finished(), "the join did not wait for the event");
        tx.commit().await.unwrap();
        joiner.await.unwrap();
        assert_eq!(attributed(&db, "vault_a").await, vec![scoped, unscoped]);
        test.cleanup().await;
    }
}
