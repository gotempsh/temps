// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP: &str = r#"
CREATE SEQUENCE runtime_health_epoch;
CREATE TABLE runtime_health_revisions (
    id UUID PRIMARY KEY,
    deployment_id INTEGER NOT NULL UNIQUE REFERENCES deployments(id) ON DELETE CASCADE,
    environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    configuration JSONB NOT NULL CHECK (octet_length(configuration::text) <= 8192),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX runtime_health_revisions_environment ON runtime_health_revisions(environment_id);
CREATE FUNCTION runtime_health_immutable_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'runtime health revisions are immutable'; END $$;
CREATE TRIGGER runtime_health_revision_immutable BEFORE UPDATE ON runtime_health_revisions
FOR EACH ROW EXECUTE FUNCTION runtime_health_immutable_revision();

CREATE TABLE runtime_health_coordinator (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    owner UUID NOT NULL,
    term BIGINT NOT NULL DEFAULT nextval('runtime_health_epoch'),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE runtime_health_collections (
    node_key INTEGER PRIMARY KEY CHECK(node_key>=0),
    coordinator UUID NOT NULL,
    epoch BIGINT NOT NULL DEFAULT nextval('runtime_health_epoch'),
    last_success_at TIMESTAMPTZ
);
CREATE TABLE runtime_health_assignments (
    id UUID PRIMARY KEY,
    revision_id UUID NOT NULL REFERENCES runtime_health_revisions(id) ON DELETE CASCADE,
    container_record_id INTEGER NOT NULL UNIQUE REFERENCES deployment_containers(id) ON DELETE CASCADE,
    node_id INTEGER REFERENCES nodes(id) ON DELETE CASCADE,
    execution_owner JSONB,
    session UUID,
    epoch BIGINT NOT NULL DEFAULT nextval('runtime_health_epoch'),
    renewal BIGINT NOT NULL DEFAULT 0 CHECK (renewal >= 0),
    lease_until TIMESTAMPTZ,
    last_sequence BIGINT NOT NULL DEFAULT 0 CHECK (last_sequence >= 0),
    observed_at TIMESTAMPTZ,
    valid_until TIMESTAMPTZ,
    state TEXT NOT NULL DEFAULT 'unknown' CHECK (state IN ('healthy','unhealthy','unknown')),
    reason TEXT NOT NULL DEFAULT 'not_checked' CHECK (length(reason) <= 64),
    next_reconcile_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    failure_streak INTEGER NOT NULL DEFAULT 0 CHECK (failure_streak BETWEEN 0 AND 1000),
    recovery_streak INTEGER NOT NULL DEFAULT 0 CHECK (recovery_streak BETWEEN 0 AND 1000),
    monitoring_lost BOOLEAN NOT NULL DEFAULT FALSE,
    unknown_since TIMESTAMPTZ DEFAULT clock_timestamp(),
    alarm_state TEXT NOT NULL DEFAULT 'unknown' CHECK (alarm_state IN ('healthy','unhealthy','unknown')),
    activated_at TIMESTAMPTZ,
    retired_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX runtime_health_assignments_due ON runtime_health_assignments(next_reconcile_at,id);
CREATE INDEX runtime_health_assignments_expiry ON runtime_health_assignments(valid_until) WHERE retired_at IS NULL;
CREATE INDEX runtime_health_assignments_revision ON runtime_health_assignments(revision_id);

CREATE TABLE runtime_health_alarm_outbox (
    id UUID PRIMARY KEY,
    assignment_id UUID NOT NULL,
    event_type TEXT NOT NULL CHECK(event_type IN ('fire_probe','resolve_probe','fire_lost','resolve_lost')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    ready_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    delivered_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX runtime_health_alarm_outbox_retention ON runtime_health_alarm_outbox(delivered_at) WHERE delivered_at IS NOT NULL;
CREATE INDEX runtime_health_alarm_outbox_due ON runtime_health_alarm_outbox(ready_at,id) WHERE delivered_at IS NULL;
CREATE UNIQUE INDEX runtime_health_alarm_event ON alarms((metadata->>'runtime_health_event')) WHERE alarm_type IN ('runtime_probe_failed','runtime_monitoring_lost');
CREATE INDEX runtime_health_alarm_assignment ON alarms((metadata->>'runtime_health_assignment'),alarm_type) WHERE status IN ('firing','acknowledged') AND alarm_type IN ('runtime_probe_failed','runtime_monitoring_lost');

CREATE TABLE runtime_health_samples (
    assignment_id UUID NOT NULL REFERENCES runtime_health_assignments(id) ON DELETE CASCADE,
    epoch BIGINT NOT NULL,
    sequence BIGINT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    state TEXT NOT NULL CHECK (state IN ('healthy','unhealthy','unknown')),
    reason TEXT NOT NULL CHECK (length(reason) <= 64),
    valid_until TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(assignment_id,epoch,sequence)
);
CREATE INDEX runtime_health_samples_retention ON runtime_health_samples(received_at);

CREATE TABLE runtime_health_environments (
    environment_id INTEGER PRIMARY KEY REFERENCES environments(id) ON DELETE CASCADE,
    revision_id UUID REFERENCES runtime_health_revisions(id) ON DELETE SET NULL,
    state TEXT NOT NULL DEFAULT 'unknown' CHECK (state IN ('healthy','unhealthy','unknown')),
    reason TEXT NOT NULL DEFAULT 'not_checked',
    valid_until TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    accounted_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    eligible_since TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    paused BOOLEAN NOT NULL DEFAULT FALSE,
    accounted_excluded BOOLEAN NOT NULL DEFAULT FALSE,
    required_healthy INTEGER CHECK (required_healthy BETWEEN 1 AND 1000)
);
CREATE INDEX runtime_health_environment_expiry ON runtime_health_environments(valid_until,environment_id) WHERE state IN ('healthy','unhealthy');
CREATE TABLE runtime_health_buckets (
    revision_id UUID NOT NULL REFERENCES runtime_health_revisions(id) ON DELETE CASCADE,
    environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    bucket_at TIMESTAMPTZ NOT NULL,
    healthy_seconds DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (healthy_seconds >= 0),
    unhealthy_seconds DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (unhealthy_seconds >= 0),
    unknown_seconds DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (unknown_seconds >= 0),
    excluded_seconds DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (excluded_seconds >= 0),
    PRIMARY KEY(environment_id,revision_id,bucket_at)
);
CREATE INDEX runtime_health_buckets_retention ON runtime_health_buckets(bucket_at);
-- Invalidate freshness atomically with known placement changes. A missing
-- replica must not inherit the previous container's remaining green window.
CREATE FUNCTION runtime_health_invalidate_assignment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 UPDATE runtime_health_environments SET valid_until=LEAST(valid_until,CASE WHEN OLD.observed_at>accounted_at THEN OLD.observed_at ELSE clock_timestamp() END) WHERE revision_id=OLD.revision_id;
 RETURN OLD;
END $$;
CREATE TRIGGER runtime_health_assignment_removed BEFORE DELETE ON runtime_health_assignments
FOR EACH ROW EXECUTE FUNCTION runtime_health_invalidate_assignment();
CREATE FUNCTION runtime_health_invalidate_container() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 UPDATE runtime_health_environments h SET valid_until=LEAST(h.valid_until,clock_timestamp()) FROM runtime_health_revisions r WHERE r.deployment_id=OLD.deployment_id AND h.revision_id=r.id;
 UPDATE runtime_health_assignments SET valid_until=LEAST(valid_until,clock_timestamp()),retired_at=COALESCE(retired_at,clock_timestamp()),state='unknown',reason='placement_changed',next_reconcile_at=clock_timestamp() WHERE container_record_id=OLD.id;
 RETURN NEW;
END $$;
CREATE TRIGGER runtime_health_container_changed AFTER UPDATE OF container_id,container_port,node_id,deleted_at ON deployment_containers
FOR EACH ROW WHEN (OLD.container_id IS DISTINCT FROM NEW.container_id OR OLD.container_port IS DISTINCT FROM NEW.container_port OR OLD.node_id IS DISTINCT FROM NEW.node_id OR OLD.deleted_at IS DISTINCT FROM NEW.deleted_at)
EXECUTE FUNCTION runtime_health_invalidate_container();
CREATE FUNCTION runtime_health_invalidate_environment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 UPDATE runtime_health_environments SET valid_until=LEAST(valid_until,clock_timestamp()) WHERE environment_id=NEW.id;
 UPDATE runtime_health_assignments a SET next_reconcile_at=clock_timestamp() FROM runtime_health_revisions r WHERE r.id=a.revision_id AND r.environment_id=NEW.id AND (a.retired_at IS NULL OR a.lease_until>clock_timestamp());
 RETURN NEW;
END $$;
CREATE TRIGGER runtime_health_environment_changed AFTER UPDATE OF current_deployment_id,sleeping ON environments
FOR EACH ROW WHEN (OLD.current_deployment_id IS DISTINCT FROM NEW.current_deployment_id OR OLD.sleeping IS DISTINCT FROM NEW.sleeping)
EXECUTE FUNCTION runtime_health_invalidate_environment();

"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TRIGGER runtime_health_environment_changed ON environments; DROP TRIGGER runtime_health_container_changed ON deployment_containers; DROP FUNCTION runtime_health_invalidate_environment(), runtime_health_invalidate_container(); DROP INDEX runtime_health_alarm_event, runtime_health_alarm_assignment; DROP TABLE runtime_health_alarm_outbox, runtime_health_buckets, runtime_health_environments, runtime_health_samples, runtime_health_assignments, runtime_health_collections, runtime_health_coordinator, runtime_health_revisions; DROP FUNCTION runtime_health_immutable_revision(), runtime_health_invalidate_assignment(); DROP SEQUENCE runtime_health_epoch;").await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DatabaseBackend, Statement};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn runtime_health_schema_roundtrips_without_seeding_readiness_as_uptime(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker unavailable; skipping runtime-health migration integration");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker unavailable; skipping runtime-health migration integration");
            return Ok(());
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let requested_port = listener.local_addr()?.port();
        drop(listener);
        let container = GenericImage::new("postgres", "17-alpine")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "runtime-health-test")
            .with_mapped_port(
                requested_port,
                testcontainers::core::ContainerPort::Tcp(5432),
            )
            .start()
            .await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let db = Database::connect(format!(
            "postgres://postgres:runtime-health-test@127.0.0.1:{port}/postgres"
        ))
        .await?;
        db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY); CREATE TABLE environments(id INTEGER PRIMARY KEY,current_deployment_id INTEGER,sleeping BOOLEAN DEFAULT FALSE); CREATE TABLE deployments(id INTEGER PRIMARY KEY); CREATE TABLE deployment_containers(id INTEGER PRIMARY KEY,deployment_id INTEGER,container_id TEXT,node_id INTEGER,deleted_at TIMESTAMPTZ,container_port INTEGER,health_state TEXT); CREATE TABLE nodes(id INTEGER PRIMARY KEY); CREATE TABLE alarms(id INTEGER PRIMARY KEY,metadata JSONB,alarm_type TEXT,status TEXT); INSERT INTO projects VALUES(1); INSERT INTO environments(id) VALUES(2); INSERT INTO deployments VALUES(3); INSERT INTO deployment_containers(id,container_port,health_state) VALUES(4,NULL,'healthy');").await?;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await?;
        let count = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*)::bigint AS count FROM runtime_health_samples".to_string(),
            ))
            .await?
            .unwrap();
        assert_eq!(count.try_get::<i64>("", "count")?, 0);
        db.execute_unprepared("INSERT INTO runtime_health_revisions(id,project_id,environment_id,deployment_id,configuration) VALUES('00000000-0000-4000-8000-000000000001',1,2,3,'{\"kind\":\"process\"}'); INSERT INTO runtime_health_assignments(id,revision_id,container_record_id) VALUES('00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000001',4)").await?;
        assert!(db
            .execute_unprepared("UPDATE runtime_health_revisions SET configuration='{}'")
            .await
            .is_err());
        assert!(db
            .execute_unprepared("UPDATE runtime_health_assignments SET state='passing'")
            .await
            .is_err());
        assert!(db
            .execute_unprepared("UPDATE runtime_health_assignments SET renewal=-1")
            .await
            .is_err());
        db.execute_unprepared("INSERT INTO runtime_health_alarm_outbox(id,assignment_id,event_type) VALUES('00000000-0000-4000-8000-000000000003','00000000-0000-4000-8000-000000000002','fire_probe')").await?;
        assert!(db.execute_unprepared("INSERT INTO runtime_health_alarm_outbox(id,assignment_id,event_type) VALUES('00000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000002','restart')").await.is_err());
        Migration.down(&manager).await?;
        Migration.up(&manager).await?;
        db.close().await?;
        Ok(())
    }

    #[test]
    fn runtime_health_migration_is_registered_after_deploy_health() {
        let names = crate::Migrator::migrations()
            .into_iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>();
        let phase_one = names
            .iter()
            .position(|name| name == "m20261017_000001_deployment_health_probes")
            .unwrap();
        let runtime = names
            .iter()
            .position(|name| name == "m20261017_000002_runtime_health")
            .unwrap();
        assert!(phase_one < runtime);
    }
}
