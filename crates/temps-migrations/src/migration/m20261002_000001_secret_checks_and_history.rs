// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extends credential checks to project secrets and adds local expiry checks.
//!
//! `http_checks` gains a `kind` (`http` calls an issuer; `local` reads expiring
//! items such as certificates, SSH certificates, OpenPGP keys, kubeconfigs and
//! JWTs on the host) and an optional `secret_id`. Secrets get the same
//! detection, suppression and trigger-maintained history tables that env vars
//! already have, and env vars are brought to parity with secrets: every env var
//! is re-scanned once for the new local formats, and moving a manual check
//! between credentials re-runs detection for both.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;

/// The check-history trigger body as shipped in `m20260921_000002_env_check_history`,
/// restored verbatim by `down`.
const ENV_ONLY_CHECK_HISTORY_FUNCTION: &str = r#"
CREATE OR REPLACE FUNCTION record_env_check_history() RETURNS TRIGGER AS $$
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
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE http_checks
 ADD COLUMN kind TEXT NOT NULL DEFAULT 'http',
 ADD COLUMN secret_id INTEGER REFERENCES secrets(id) ON DELETE CASCADE;
ALTER TABLE http_checks ADD CONSTRAINT http_checks_kind_check CHECK (kind IN ('http','local'));
ALTER TABLE http_checks ADD CONSTRAINT http_checks_single_credential_source CHECK (num_nonnulls(env_var_id,secret_id,encrypted_credential) <= 1);
CREATE INDEX http_checks_secret_idx ON http_checks(secret_id);
CREATE UNIQUE INDEX http_checks_automatic_secret ON http_checks(secret_id) WHERE automatic_provider IS NOT NULL;
CREATE TABLE secret_check_detection (
 secret_id INTEGER PRIMARY KEY REFERENCES secrets(id) ON DELETE CASCADE,
 retry_after TIMESTAMPTZ
);
CREATE TABLE secret_check_suppressions (
 secret_id INTEGER NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
 automatic_provider TEXT NOT NULL,
 PRIMARY KEY(secret_id,automatic_provider)
);
CREATE TABLE secret_history (
 id BIGSERIAL PRIMARY KEY,
 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 secret_id INTEGER NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,
 kind TEXT NOT NULL,
 details JSONB NOT NULL DEFAULT '{}',
 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX secret_history_lookup ON secret_history(project_id,secret_id,id DESC);
INSERT INTO secret_history(project_id,secret_id,kind) SELECT project_id,id,'tracking_started' FROM secrets;
CREATE FUNCTION record_secret_history() RETURNS TRIGGER AS $$
BEGIN
 IF TG_OP='INSERT' THEN
  INSERT INTO secret_history(project_id,secret_id,kind) VALUES(NEW.project_id,NEW.id,'created');
  RETURN NEW;
 END IF;
 IF OLD.value IS DISTINCT FROM NEW.value THEN
  INSERT INTO secret_history(project_id,secret_id,kind) VALUES(NEW.project_id,NEW.id,'value_changed');
 END IF;
 IF OLD.key IS DISTINCT FROM NEW.key OR OLD.include_in_preview IS DISTINCT FROM NEW.include_in_preview THEN
  INSERT INTO secret_history(project_id,secret_id,kind,details) VALUES(NEW.project_id,NEW.id,'settings_changed',jsonb_build_object('key',NEW.key,'include_in_preview',NEW.include_in_preview));
 END IF;
 -- Only a new value or name can change what the credential is; scope and preview edits do not.
 IF OLD.value IS DISTINCT FROM NEW.value OR OLD.key IS DISTINCT FROM NEW.key THEN
  DELETE FROM secret_check_detection WHERE secret_id=NEW.id;
  UPDATE http_checks SET next_check_at=NOW(),last_result=NULL,last_checked_at=NULL,lease_until=NULL,lease_token=NULL,consecutive_unknowns=0 WHERE secret_id=NEW.id;
 END IF;
 RETURN NEW;
END; $$ LANGUAGE plpgsql;
CREATE TRIGGER secret_history_changed AFTER INSERT OR UPDATE ON secrets FOR EACH ROW EXECUTE FUNCTION record_secret_history();
-- Env vars scanned before local checks existed are re-scanned once, except those
-- whose automatic checks are all suppressed: a re-scan could not create one.
DELETE FROM env_check_detection AS detection WHERE NOT EXISTS(SELECT 1 FROM env_check_suppressions AS suppression WHERE suppression.env_var_id=detection.env_var_id AND suppression.automatic_provider='*');
CREATE OR REPLACE FUNCTION record_env_check_history() RETURNS TRIGGER AS $$
DECLARE variable_id INTEGER; secret INTEGER; project INTEGER; event_kind TEXT; payload JSONB; rebound BOOLEAN;
BEGIN
 IF TG_OP='DELETE' THEN
  variable_id=OLD.env_var_id; secret=OLD.secret_id; project=OLD.project_id; event_kind='check_removed'; payload=jsonb_build_object('check_name',OLD.name);
 ELSE
  variable_id=NEW.env_var_id; secret=NEW.secret_id; project=NEW.project_id; payload=jsonb_build_object('check_name',NEW.name);
  IF TG_OP='INSERT' THEN
   rebound=TRUE;
  ELSE
   rebound=NEW.env_var_id IS DISTINCT FROM OLD.env_var_id OR NEW.secret_id IS DISTINCT FROM OLD.secret_id;
  END IF;
  -- A manual check replaces the automatic check of the credential it now reads, and
  -- the credential it stopped reading may need its automatic check back: re-run both.
  IF rebound AND NEW.automatic_provider IS NULL THEN
   DELETE FROM env_check_detection WHERE env_var_id=NEW.env_var_id;
   DELETE FROM secret_check_detection WHERE secret_id=NEW.secret_id;
   IF TG_OP='UPDATE' THEN
    DELETE FROM env_check_detection WHERE env_var_id=OLD.env_var_id;
    DELETE FROM secret_check_detection WHERE secret_id=OLD.secret_id;
   END IF;
  END IF;
  IF TG_OP='INSERT' THEN
   event_kind='check_added';
  ELSIF rebound THEN
   -- The check moved: the credential it left loses it, the one it now reads gains it.
   IF OLD.env_var_id IS NOT NULL AND EXISTS(SELECT 1 FROM env_vars WHERE id=OLD.env_var_id) THEN
    INSERT INTO env_var_history(project_id,env_var_id,kind,details) VALUES(OLD.project_id,OLD.env_var_id,'check_removed',jsonb_build_object('check_name',OLD.name));
   END IF;
   IF OLD.secret_id IS NOT NULL AND EXISTS(SELECT 1 FROM secrets WHERE id=OLD.secret_id) THEN
    INSERT INTO secret_history(project_id,secret_id,kind,details) VALUES(OLD.project_id,OLD.secret_id,'check_removed',jsonb_build_object('check_name',OLD.name));
   END IF;
   event_kind='check_added';
  ELSIF NEW.last_checked_at IS DISTINCT FROM OLD.last_checked_at AND NEW.last_result IS NOT NULL THEN
   event_kind='verification'; payload=payload || jsonb_build_object('result',NEW.last_result);
  ELSIF NEW.enabled IS DISTINCT FROM OLD.enabled THEN event_kind=CASE WHEN NEW.enabled THEN 'check_resumed' ELSE 'check_paused' END;
  ELSIF NEW.encrypted_spec IS DISTINCT FROM OLD.encrypted_spec OR NEW.kind IS DISTINCT FROM OLD.kind OR NEW.name IS DISTINCT FROM OLD.name OR NEW.interval_seconds IS DISTINCT FROM OLD.interval_seconds THEN event_kind='check_updated';
  ELSE RETURN NEW; END IF;
 END IF;
 IF variable_id IS NOT NULL AND EXISTS(SELECT 1 FROM env_vars WHERE id=variable_id) THEN
  INSERT INTO env_var_history(project_id,env_var_id,kind,details) VALUES(project,variable_id,event_kind,payload);
 END IF;
 IF secret IS NOT NULL AND EXISTS(SELECT 1 FROM secrets WHERE id=secret) THEN
  INSERT INTO secret_history(project_id,secret_id,kind,details) VALUES(project,secret,event_kind,payload);
 END IF;
 RETURN COALESCE(NEW,OLD);
END; $$ LANGUAGE plpgsql;
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        // Restore the env-only trigger first: the rows deleted below fire it, and it must
        // not reference the columns and tables dropped afterwards. Secret-bound and
        // local checks cannot be represented by the previous schema.
        connection
            .execute_unprepared(ENV_ONLY_CHECK_HISTORY_FUNCTION)
            .await?;
        connection.execute_unprepared(r#"
DELETE FROM http_checks WHERE secret_id IS NOT NULL OR kind <> 'http';
DROP TRIGGER secret_history_changed ON secrets;
DROP FUNCTION record_secret_history();
DROP TABLE secret_history;
DROP TABLE secret_check_suppressions;
DROP TABLE secret_check_detection;
DROP INDEX http_checks_automatic_secret;
DROP INDEX http_checks_secret_idx;
ALTER TABLE http_checks DROP CONSTRAINT http_checks_single_credential_source, DROP CONSTRAINT http_checks_kind_check, DROP COLUMN secret_id, DROP COLUMN kind;
"#).await?;
        Ok(())
    }
}
