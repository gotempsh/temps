//! Coolify Platform Importer
//!
//! Implements the `WorkloadImporter` trait for [Coolify](https://coolify.io)
//! instances, accessed via their REST API (`{base_url}/api/v1`) with a
//! Sanctum bearer token passed per-request in `ImportCredentials.token`
//! (never persisted).
//!
//! # What it does
//!
//! - **Discover**: lists applications and standalone databases across the
//!   instance.
//! - **Describe**: turns one application into a
//!   [`temps_import_types::WorkloadSnapshot`] (image or git build, env vars
//!   resolved through `/applications/{uuid}/envs`, exposed ports, status).
//! - **Project snapshot**: treats the application's Coolify *environment* as
//!   the project boundary — sibling applications become additional
//!   deployments, standalone databases become managed-service candidates
//!   (including their `external_db_url` so data can be dumped/restored), and
//!   application FQDNs become domains. Coolify-generated `*.sslip.io`
//!   domains are recognized and planned as skipped.
//! - **Plan**: generates a reviewable migration plan with per-database data
//!   migration commands and DNS-cutover manual actions.
//!
//! # Credentials
//!
//! - `credentials.base_url` — the Coolify instance URL (e.g. `http://1.2.3.4:8000`)
//! - `credentials.token` — an API token (Coolify: *Keys & Tokens → API tokens*;
//!   the instance's API access toggle must be enabled)

pub mod client;
pub mod error;
pub mod importer;
pub mod model;
pub mod validation;

pub use error::CoolifyImportError;
pub use importer::CoolifyImporter;
