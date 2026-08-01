//! CapRover Platform Importer
//!
//! Implements the `WorkloadImporter` trait for [CapRover](https://caprover.com)
//! instances, accessed via their REST API (`{base_url}/api/v2`). CapRover
//! authenticates with the instance password (passed per-request in
//! `ImportCredentials.token`, exchanged for a short-lived session token —
//! never persisted).
//!
//! # Model
//!
//! CapRover has no project grouping — every workload is a flat "app
//! definition". The migration path therefore treats:
//!
//! - the selected app as the **primary workload**;
//! - apps running a known database image (`postgres`, `mysql`, `mariadb`,
//!   `mongo`, `redis` — CapRover one-click databases) as **managed-service
//!   candidates**, with connection URLs reconstructed from their `POSTGRES_*`
//!   / `MYSQL_*` env vars and published host ports;
//! - the app's `customDomain` list plus its generated
//!   `<app>.<root domain>` as **domains**;
//! - `appPushWebhook.repoInfo` as **git info** (its password field is
//!   treated as a secret and never copied into the plan).
//!
//! # Credentials
//!
//! - `credentials.base_url` — the CapRover instance URL (e.g. `http://1.2.3.4:3000`)
//! - `credentials.token` — the CapRover admin password

pub mod client;
pub mod error;
pub mod importer;
pub mod model;
pub mod validation;

pub use error::CaproverImportError;
pub use importer::CaproverImporter;
