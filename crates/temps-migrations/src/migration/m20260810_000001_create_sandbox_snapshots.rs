//! ADR-037: Sandbox snapshots table.
//!
//! A snapshot is a committed image layer derived from a sandbox container,
//! stored as a content-addressed tarball under `$TEMPS_DATA_DIR/snapshots/`.
//! Snapshots are user-managed; there is no automatic GC.
//!
//! Key design choices (from the ADR):
//! - `source_sandbox_id` is NOT a foreign key — the source sandbox may be
//!   destroyed after snapping. The sandbox destroy path nullifies this column.
//! - `content_digest` is the deduplication key: two snapshots with the same
//!   digest share one tarball on disk.
//! - `status` follows the state machine: creating → ready | failed → deleted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
CREATE TABLE IF NOT EXISTS sandbox_snapshots (
    id               SERIAL PRIMARY KEY,
    public_id        VARCHAR NOT NULL UNIQUE,
    user_id          INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- project_id has no FK intentionally: mirrors source_sandbox_id's design —
    -- a snapshot outlives the project it was taken in, and project deletion
    -- should not cascade-delete the user's snapshot artifacts.
    project_id       INTEGER,
    -- source_sandbox_id has no FK intentionally: the source sandbox may be
    -- destroyed after snapping. The destroy path nullifies this column.
    source_sandbox_id INTEGER,
    label            VARCHAR,
    status           VARCHAR(16) NOT NULL DEFAULT 'creating',
    backend          VARCHAR(32) NOT NULL,
    content_digest   VARCHAR NOT NULL,
    content_path     VARCHAR NOT NULL,
    size_bytes       BIGINT NOT NULL DEFAULT 0,
    image_ref        VARCHAR,
    metadata         JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
)
"#,
        )
        .await?;

        // Primary list query: user_id + status + created_at (newest first).
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandbox_snapshots_user_status ON sandbox_snapshots (user_id, status, created_at DESC)",
        )
        .await?;

        // Deduplication lookup: find an existing ready snapshot with the same
        // content digest before writing a new tarball to disk.
        conn.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_sandbox_snapshots_digest_ready ON sandbox_snapshots (content_digest) WHERE status = 'ready'",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP INDEX IF EXISTS idx_sandbox_snapshots_digest_ready")
            .await?;
        conn.execute_unprepared("DROP INDEX IF EXISTS idx_sandbox_snapshots_user_status")
            .await?;
        conn.execute_unprepared("DROP TABLE IF EXISTS sandbox_snapshots")
            .await?;
        Ok(())
    }
}
