//! Backup engine implementations for `temps-backup` (ADR-014 Phase 1–4).
//!
//! Each module implements the [`temps_backup_core::BackupEngine`] trait for a
//! specific backup target. The runner in `temps-backup-core` dispatches to these
//! engines by matching `backup_jobs.engine` against `BackupEngine::engine()`.
//!
//! ## Phase 1 engines
//! - [`control_plane`]: Control-plane (Temps server's own PostgreSQL database).
//!
//! ## Phase 2–4 engines (external services)
//! - [`redis`]: Redis via BGSAVE or WAL-G.
//! - [`mongodb`]: MongoDB via mongodump.
//! - [`postgres_pgdump`]: Postgres via pg_dump sidecar (fallback).
//! - [`postgres_walg`]: Postgres via WAL-G (preferred when available).
//! - [`postgres_cluster`]: Postgres cluster (pg_auto_failover) via WAL-G.
//! - [`mariadb_physical`]: MariaDB via `mariadb-backup` physical base (PITR).
//! - [`mariadb_dump`]: MariaDB via `mariadb-dump` logical dump (fallback).
//! - [`s3_mirror`]: S3-compatible object storage via `mc mirror`.
//! - [`dispatch`]: Engine-key resolution helper (`resolve_engine_key`).
//!
//! ## Adding a new engine
//! 1. Create `src/engines/<name>.rs` implementing `BackupEngine`.
//! 2. Add `pub mod <name>;` here.
//! 3. Instantiate and register in `plugin.rs` (`BackupPlugin::register_services`).

pub mod control_plane;
pub mod dispatch;
pub mod image_pull;
pub mod mariadb_dump;
pub mod mariadb_exec;
pub mod mariadb_physical;
pub mod mongodb;
pub mod oneshot;
pub mod postgres_cluster;
pub mod postgres_pgdump;
pub mod postgres_walg;
pub mod redis;
pub mod ring_buffer;
pub mod s3_mirror;
pub mod sidecar;
pub mod v2_common;
