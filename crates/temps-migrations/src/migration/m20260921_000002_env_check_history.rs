// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE http_checks ADD COLUMN automatic_provider TEXT;
CREATE UNIQUE INDEX http_checks_automatic_env ON http_checks(env_var_id) WHERE automatic_provider IS NOT NULL;
CREATE TABLE env_check_detection (
 env_var_id INTEGER PRIMARY KEY REFERENCES env_vars(id) ON DELETE CASCADE,
 observed_updated_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE env_var_history (
 id BIGSERIAL PRIMARY KEY,
 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 env_var_id INTEGER NOT NULL REFERENCES env_vars(id) ON DELETE CASCADE,
 kind TEXT NOT NULL,
 details JSONB NOT NULL DEFAULT '{}',
 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX env_var_history_lookup ON env_var_history(project_id,env_var_id,id DESC);
INSERT INTO env_var_history(project_id,env_var_id,kind) SELECT project_id,id,'tracking_started' FROM env_vars;
CREATE FUNCTION record_env_var_history() RETURNS TRIGGER AS $$
BEGIN
 IF TG_OP='INSERT' THEN
  INSERT INTO env_var_history(project_id,env_var_id,kind) VALUES(NEW.project_id,NEW.id,'created');
 ELSE
  IF OLD.value IS DISTINCT FROM NEW.value OR OLD.is_encrypted IS DISTINCT FROM NEW.is_encrypted THEN
   INSERT INTO env_var_history(project_id,env_var_id,kind) VALUES(NEW.project_id,NEW.id,'value_changed');
  END IF;
  IF OLD.key IS DISTINCT FROM NEW.key OR OLD.is_secret IS DISTINCT FROM NEW.is_secret OR OLD.include_in_preview IS DISTINCT FROM NEW.include_in_preview THEN
   INSERT INTO env_var_history(project_id,env_var_id,kind,details) VALUES(NEW.project_id,NEW.id,'settings_changed',jsonb_build_object('key',NEW.key,'is_secret',NEW.is_secret,'include_in_preview',NEW.include_in_preview));
  END IF;
  DELETE FROM env_check_detection WHERE env_var_id=NEW.id;
  IF OLD.key IS DISTINCT FROM NEW.key THEN
   UPDATE http_checks SET lease_token=NULL,lease_until=NULL,last_result=NULL,last_checked_at=NULL,next_check_at=NOW() WHERE env_var_id=NEW.id;
  END IF;
 END IF;
 RETURN NEW;
END; $$ LANGUAGE plpgsql;
CREATE TRIGGER env_var_history_changed AFTER INSERT OR UPDATE ON env_vars FOR EACH ROW EXECUTE FUNCTION record_env_var_history();
CREATE FUNCTION record_env_check_history() RETURNS TRIGGER AS $$
DECLARE variable_id INTEGER; project INTEGER; event_kind TEXT; payload JSONB;
BEGIN
 IF TG_OP='DELETE' THEN
  variable_id=OLD.env_var_id; project=OLD.project_id; event_kind='check_removed'; payload=jsonb_build_object('check_name',OLD.name);
 ELSE
  variable_id=NEW.env_var_id; project=NEW.project_id; payload=jsonb_build_object('check_name',NEW.name);
  IF TG_OP='INSERT' THEN
   event_kind='check_added';
   IF NEW.automatic_provider IS NULL THEN DELETE FROM env_check_detection WHERE env_var_id=NEW.env_var_id; END IF;
  ELSIF NEW.last_checked_at IS DISTINCT FROM OLD.last_checked_at AND NEW.last_result IS NOT NULL THEN
   event_kind='verification'; payload=payload || jsonb_build_object('result',NEW.last_result);
  ELSIF NEW.enabled IS DISTINCT FROM OLD.enabled THEN event_kind=CASE WHEN NEW.enabled THEN 'check_resumed' ELSE 'check_paused' END;
  ELSIF NEW.encrypted_spec IS DISTINCT FROM OLD.encrypted_spec OR NEW.env_var_id IS DISTINCT FROM OLD.env_var_id OR NEW.name IS DISTINCT FROM OLD.name OR NEW.interval_seconds IS DISTINCT FROM OLD.interval_seconds THEN event_kind='check_updated';
  ELSE RETURN NEW; END IF;
 END IF;
 IF variable_id IS NOT NULL AND EXISTS(SELECT 1 FROM env_vars WHERE id=variable_id) THEN
  INSERT INTO env_var_history(project_id,env_var_id,kind,details) VALUES(project,variable_id,event_kind,payload);
 END IF;
 RETURN COALESCE(NEW,OLD);
END; $$ LANGUAGE plpgsql;
CREATE TRIGGER env_check_history_changed AFTER INSERT OR UPDATE OR DELETE ON http_checks FOR EACH ROW EXECUTE FUNCTION record_env_check_history();
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TRIGGER env_check_history_changed ON http_checks; DROP FUNCTION record_env_check_history(); DROP TRIGGER env_var_history_changed ON env_vars; DROP FUNCTION record_env_var_history(); DROP TABLE env_var_history; DROP TABLE env_check_detection; DROP INDEX http_checks_automatic_env; ALTER TABLE http_checks DROP COLUMN automatic_provider;").await?;
        Ok(())
    }
}
