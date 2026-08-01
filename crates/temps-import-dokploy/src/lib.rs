//! Dokploy Platform Importer
//!
//! Implements the `WorkloadImporter` trait for [Dokploy](https://dokploy.com)
//! instances, accessed via their REST API (`{base_url}/api/*`) with an API
//! key passed per-request in `ImportCredentials.token` (sent as `x-api-key`,
//! never persisted).
//!
//! # What it does
//!
//! - **Discover**: walks `project.all` — every project → environment →
//!   applications + standalone databases.
//! - **Describe**: turns one application into a
//!   [`temps_import_types::WorkloadSnapshot`] (git or docker source, env vars
//!   parsed from Dokploy's `K=V` lines, status).
//! - **Project snapshot**: the application's Dokploy *environment* is the
//!   project boundary — sibling applications become additional deployments,
//!   the environment's databases (postgres/mysql/mariadb/mongo/redis) become
//!   managed-service candidates, and application domains become domains
//!   (generated `*.traefik.me` hosts are planned as skipped).
//! - **Plan**: reviewable migration plan with per-database data-migration
//!   guidance and DNS-cutover manual actions.
//!
//! # Credentials
//!
//! - `credentials.base_url` — the Dokploy instance URL (e.g. `http://1.2.3.4:3000`)
//! - `credentials.token` — an API key. **Important**: Dokploy's default API
//!   keys are rate-limited to 10 requests/day — create one with rate limiting
//!   disabled, or discovery will die mid-import.

pub mod client;
pub mod error;
pub mod importer;
pub mod model;
pub mod validation;

pub use error::DokployImportError;
pub use importer::DokployImporter;
