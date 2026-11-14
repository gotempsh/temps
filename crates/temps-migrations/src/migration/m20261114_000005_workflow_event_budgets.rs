// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retained event budgets include writes from older binaries and managed
//! journal commits. A one-time, database-side backfill runs under a write lock;
//! ongoing admission and deletion update one counter row, never scan history.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE workflow_environment_settings
 ADD COLUMN max_retained_events BIGINT NOT NULL DEFAULT 10000 CHECK(max_retained_events BETWEEN 1 AND 1000000),
 ADD COLUMN max_retained_event_bytes BIGINT NOT NULL DEFAULT 67108864 CHECK(max_retained_event_bytes BETWEEN 1 AND 1073741824);
LOCK TABLE workflow_events IN SHARE ROW EXCLUSIVE MODE;
CREATE TABLE workflow_event_usage (
 environment_id INTEGER PRIMARY KEY REFERENCES environments(id) ON DELETE CASCADE,
 event_count BIGINT NOT NULL DEFAULT 0 CHECK(event_count >= 0),
 payload_bytes BIGINT NOT NULL DEFAULT 0 CHECK(payload_bytes >= 0));
INSERT INTO workflow_event_usage(environment_id,event_count,payload_bytes)
 SELECT environment_id,count(*),coalesce(sum(payload_bytes),0) FROM workflow_events GROUP BY environment_id;
CREATE FUNCTION workflow_event_budget() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE used_count BIGINT; used_bytes BIGINT; count_limit BIGINT; byte_limit BIGINT;
BEGIN
 IF TG_OP = 'DELETE' THEN
  UPDATE workflow_event_usage SET event_count=event_count-1,payload_bytes=payload_bytes-OLD.payload_bytes
   WHERE environment_id=OLD.environment_id;
  RETURN OLD;
 END IF;
 INSERT INTO workflow_event_usage(environment_id,event_count,payload_bytes)
  VALUES(NEW.environment_id,1,NEW.payload_bytes)
  ON CONFLICT(environment_id) DO UPDATE SET event_count=workflow_event_usage.event_count+1,
    payload_bytes=workflow_event_usage.payload_bytes+EXCLUDED.payload_bytes
  RETURNING event_count,payload_bytes INTO used_count,used_bytes;
 SELECT max_retained_events,max_retained_event_bytes INTO count_limit,byte_limit
  FROM workflow_environment_settings WHERE environment_id=NEW.environment_id;
 IF used_count > coalesce(count_limit,10000) OR used_bytes > coalesce(byte_limit,67108864) THEN
  RAISE EXCEPTION 'workflow_event_budget_exceeded' USING ERRCODE='23514', CONSTRAINT='workflow_event_budget';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER workflow_event_budget_insert BEFORE INSERT ON workflow_events FOR EACH ROW EXECUTE FUNCTION workflow_event_budget();
CREATE TRIGGER workflow_event_budget_delete AFTER DELETE ON workflow_events FOR EACH ROW EXECUTE FUNCTION workflow_event_budget();
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TRIGGER workflow_event_budget_insert ON workflow_events;
DROP TRIGGER workflow_event_budget_delete ON workflow_events;
DROP FUNCTION workflow_event_budget(); DROP TABLE workflow_event_usage;
ALTER TABLE workflow_environment_settings DROP COLUMN max_retained_events, DROP COLUMN max_retained_event_bytes;").await?;
        Ok(())
    }
}
