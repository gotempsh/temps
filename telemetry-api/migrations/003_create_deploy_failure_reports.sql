-- Deploy-failure reports: a user-reviewed, user-editable copy of a failed
-- deployment's build trace, sent only when a self-hoster explicitly clicks
-- "Send failure report" (see crates/temps-deployments/src/services/failure_report_service.rs).
--
-- Unlike telemetry_events, this table holds free text the user chose to
-- send — never inserted anywhere else in this schema. project_id/deployment_id
-- are the SENDING instance's own local IDs, not globally meaningful; they're
-- kept only as debugging context for whoever triages a report.

CREATE TABLE IF NOT EXISTS deploy_failure_reports (
    id               BIGSERIAL PRIMARY KEY,
    report_text      TEXT        NOT NULL,
    temps_version    TEXT,
    project_id       INTEGER,
    deployment_id    INTEGER,
    failed_job_id    TEXT        NOT NULL,
    failed_job_type  TEXT        NOT NULL,
    country          TEXT,
    received_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_deploy_failure_reports_received_at
    ON deploy_failure_reports (received_at DESC);

CREATE INDEX IF NOT EXISTS idx_deploy_failure_reports_job_type
    ON deploy_failure_reports (failed_job_type, received_at DESC);
