// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workload objects (ADR-055 §2, phase 2) and cron `job` mode (§14).
//!
//! - `workload_change_seq`: one sequence shared by every row the plane
//!   writes. A `BEFORE UPDATE` trigger takes a new value on every update,
//!   so `change_seq` is a resource version no writer can forget to bump.
//! - `workloads`: spec, status, `generation`/`observed_generation`,
//!   finalizers, soft delete. `phase` mirrors `status.phase` so the
//!   reconciler's queries are index scans over the rows that need work,
//!   never a scan of every workload; `expires_at` is when a finished
//!   run's time-to-live runs out.
//! - `workload_instances`: one row per attempt, with the driver handle.
//! - `workload_events`: bounded history; repeats increment `count`.
//! - `crons` gains `mode` (`http` default, so every existing cron keeps its
//!   behaviour) and the CronJob fields; `cron_executions` gains the run
//!   state a job run reports.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The plane's own tables: the sequence, the trigger, `workloads`,
/// `workload_instances` and `workload_events`. Public so the plane's tests
/// can create exactly these in an isolated schema.
pub const WORKLOAD_TABLES_SQL: &[&str] = &[
    "CREATE SEQUENCE IF NOT EXISTS workload_change_seq",
    "CREATE OR REPLACE FUNCTION workload_bump_change_seq() RETURNS trigger AS $$ \
             BEGIN \
                 NEW.change_seq := nextval('workload_change_seq'); \
                 NEW.updated_at := now(); \
                 RETURN NEW; \
             END; \
             $$ LANGUAGE plpgsql",
    "CREATE TABLE IF NOT EXISTS workloads ( \
                 id BIGSERIAL PRIMARY KEY, \
                 owner_kind TEXT NOT NULL, \
                 owner_id BIGINT NOT NULL, \
                 name TEXT NOT NULL, \
                 spec JSONB NOT NULL, \
                 generation BIGINT NOT NULL DEFAULT 1, \
                 observed_generation BIGINT NOT NULL DEFAULT 0, \
                 change_seq BIGINT NOT NULL DEFAULT nextval('workload_change_seq'), \
                 status JSONB NOT NULL DEFAULT '{}'::jsonb, \
                 phase TEXT NOT NULL DEFAULT 'pending', \
                 owner_workload_id BIGINT NULL REFERENCES workloads(id) ON DELETE CASCADE, \
                 finalizers TEXT[] NOT NULL DEFAULT '{}', \
                 expires_at TIMESTAMPTZ NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 deleted_at TIMESTAMPTZ NULL, \
                 CONSTRAINT workloads_phase_valid CHECK \
                     (phase IN ('pending', 'running', 'succeeded', 'failed', 'terminating')) \
             )",
    // One live workload per owner and name: re-applying is idempotent.
    "CREATE UNIQUE INDEX IF NOT EXISTS workloads_owner_name_live \
             ON workloads (owner_kind, owner_id, name) WHERE deleted_at IS NULL",
    "CREATE INDEX IF NOT EXISTS workloads_owner ON workloads (owner_kind, owner_id)",
    "CREATE INDEX IF NOT EXISTS workloads_owner_workload \
             ON workloads (owner_workload_id) WHERE owner_workload_id IS NOT NULL",
    // What the reconciler reads: live unfinished workloads, finished
    // ones holding finalizers or a time-to-live, and deleted ones.
    "CREATE INDEX IF NOT EXISTS workloads_active ON workloads (id) \
             WHERE deleted_at IS NULL AND phase NOT IN ('succeeded', 'failed')",
    "CREATE INDEX IF NOT EXISTS workloads_finalizing ON workloads (id) \
             WHERE cardinality(finalizers) > 0",
    "CREATE INDEX IF NOT EXISTS workloads_expires_at ON workloads (expires_at) \
             WHERE deleted_at IS NULL AND expires_at IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS workloads_deleted_at ON workloads (deleted_at) \
             WHERE deleted_at IS NOT NULL",
    "DROP TRIGGER IF EXISTS workloads_change_seq ON workloads",
    "CREATE TRIGGER workloads_change_seq BEFORE UPDATE ON workloads \
             FOR EACH ROW EXECUTE FUNCTION workload_bump_change_seq()",
    "CREATE TABLE IF NOT EXISTS workload_instances ( \
                 id BIGSERIAL PRIMARY KEY, \
                 workload_id BIGINT NOT NULL REFERENCES workloads(id) ON DELETE CASCADE, \
                 revision BIGINT NOT NULL, \
                 attempt INT NOT NULL DEFAULT 1, \
                 node_id INT NULL, \
                 driver TEXT NOT NULL, \
                 handle JSONB NOT NULL, \
                 address TEXT NULL, \
                 phase TEXT NOT NULL DEFAULT 'pending', \
                 ready BOOLEAN NOT NULL DEFAULT false, \
                 conditions JSONB NOT NULL DEFAULT '[]'::jsonb, \
                 exit_code BIGINT NULL, \
                 started_at TIMESTAMPTZ NULL, \
                 finished_at TIMESTAMPTZ NULL, \
                 change_seq BIGINT NOT NULL DEFAULT nextval('workload_change_seq'), \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 CONSTRAINT workload_instances_phase_valid CHECK \
                     (phase IN ('pending', 'running', 'succeeded', 'failed')) \
             )",
    "CREATE UNIQUE INDEX IF NOT EXISTS workload_instances_attempt \
             ON workload_instances (workload_id, attempt)",
    // A node's instances (NULL is the control plane, node 0).
    "CREATE INDEX IF NOT EXISTS workload_instances_node \
             ON workload_instances (node_id, workload_id)",
    "DROP TRIGGER IF EXISTS workload_instances_change_seq ON workload_instances",
    "CREATE TRIGGER workload_instances_change_seq BEFORE UPDATE ON workload_instances \
             FOR EACH ROW EXECUTE FUNCTION workload_bump_change_seq()",
    "CREATE TABLE IF NOT EXISTS workload_events ( \
                 id BIGSERIAL PRIMARY KEY, \
                 workload_id BIGINT NOT NULL REFERENCES workloads(id) ON DELETE CASCADE, \
                 instance_id BIGINT NULL REFERENCES workload_instances(id) ON DELETE SET NULL, \
                 reason TEXT NOT NULL, \
                 message TEXT NOT NULL, \
                 count INT NOT NULL DEFAULT 1, \
                 first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now() \
             )",
    "CREATE INDEX IF NOT EXISTS workload_events_workload \
             ON workload_events (workload_id, last_seen_at DESC)",
    "CREATE INDEX IF NOT EXISTS workload_events_last_seen \
             ON workload_events (last_seen_at)",
];

/// Cron `job` mode on `crons` and `cron_executions`.
const CRON_JOB_SQL: &[&str] = &[
    // Cron `job` mode. `http` is the default: existing crons are
    // untouched.
    "ALTER TABLE crons \
                 ADD COLUMN IF NOT EXISTS mode TEXT NOT NULL DEFAULT 'http', \
                 ADD COLUMN IF NOT EXISTS command JSONB NULL, \
                 ADD COLUMN IF NOT EXISTS concurrency_policy TEXT NOT NULL DEFAULT 'forbid', \
                 ADD COLUMN IF NOT EXISTS starting_deadline_seconds INT NULL, \
                 ADD COLUMN IF NOT EXISTS retry_limit INT NOT NULL DEFAULT 0, \
                 ADD COLUMN IF NOT EXISTS timeout_seconds INT NULL, \
                 ADD COLUMN IF NOT EXISTS ttl_seconds_after_finished INT NULL",
    "ALTER TABLE crons DROP CONSTRAINT IF EXISTS crons_mode_valid",
    "ALTER TABLE crons ADD CONSTRAINT crons_mode_valid CHECK (mode IN ('http', 'job'))",
    "ALTER TABLE crons DROP CONSTRAINT IF EXISTS crons_concurrency_policy_valid",
    "ALTER TABLE crons ADD CONSTRAINT crons_concurrency_policy_valid \
             CHECK (concurrency_policy IN ('allow', 'forbid', 'replace'))",
    "ALTER TABLE cron_executions \
                 ADD COLUMN IF NOT EXISTS workload_id BIGINT NULL \
                     REFERENCES workloads(id) ON DELETE SET NULL, \
                 ADD COLUMN IF NOT EXISTS state TEXT NULL, \
                 ADD COLUMN IF NOT EXISTS exit_code INT NULL, \
                 ADD COLUMN IF NOT EXISTS attempt INT NULL, \
                 ADD COLUMN IF NOT EXISTS finished_at TIMESTAMPTZ NULL, \
                 ADD COLUMN IF NOT EXISTS output_tail TEXT NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS cron_executions_workload \
             ON cron_executions (workload_id) WHERE workload_id IS NOT NULL",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for s in WORKLOAD_TABLES_SQL.iter().chain(CRON_JOB_SQL) {
            conn.execute_unprepared(s).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let stmts = [
            "DROP INDEX IF EXISTS cron_executions_workload",
            "ALTER TABLE cron_executions \
                 DROP COLUMN IF EXISTS output_tail, \
                 DROP COLUMN IF EXISTS finished_at, \
                 DROP COLUMN IF EXISTS attempt, \
                 DROP COLUMN IF EXISTS exit_code, \
                 DROP COLUMN IF EXISTS state, \
                 DROP COLUMN IF EXISTS workload_id",
            "ALTER TABLE crons DROP CONSTRAINT IF EXISTS crons_concurrency_policy_valid",
            "ALTER TABLE crons DROP CONSTRAINT IF EXISTS crons_mode_valid",
            // Job crons have no path an `http` cron could call; the old
            // binary would issue GETs to their identity key.
            "DELETE FROM crons WHERE mode = 'job'",
            "ALTER TABLE crons \
                 DROP COLUMN IF EXISTS ttl_seconds_after_finished, \
                 DROP COLUMN IF EXISTS timeout_seconds, \
                 DROP COLUMN IF EXISTS retry_limit, \
                 DROP COLUMN IF EXISTS starting_deadline_seconds, \
                 DROP COLUMN IF EXISTS concurrency_policy, \
                 DROP COLUMN IF EXISTS command, \
                 DROP COLUMN IF EXISTS mode",
            "DROP TABLE IF EXISTS workload_events",
            "DROP TABLE IF EXISTS workload_instances",
            "DROP TABLE IF EXISTS workloads",
            "DROP FUNCTION IF EXISTS workload_bump_change_seq()",
            "DROP SEQUENCE IF EXISTS workload_change_seq",
        ];
        for s in stmts {
            conn.execute_unprepared(s).await?;
        }
        Ok(())
    }
}
