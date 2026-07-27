//! Portainer Importer
//!
//! Implements the `WorkloadImporter` trait for [Portainer](https://portainer.io)
//! (CE/BE), accessed via its REST API (`{base_url}/api`). Portainer
//! authenticates with username + password, exchanged for a short-lived JWT
//! (`POST /api/auth`) — credentials are per-request and never persisted.
//!
//! # Model
//!
//! Portainer is not a PaaS: it manages Docker environments. The migration
//! path therefore treats a **stack** (its compose deployment) as the project
//! boundary:
//!
//! - the stack's containers are the workloads, discovered through Portainer's
//!   Docker proxy (`/endpoints/{id}/docker/...`) and grouped by the
//!   `com.docker.compose.project` label;
//! - containers running a known database image become **managed-service
//!   candidates**, with connection URLs reconstructed from their env vars and
//!   published host ports;
//! - published HTTP ports become the app's addresses (Portainer has no domain
//!   concept of its own, so there is nothing to import as a custom domain —
//!   the plan says so explicitly rather than silently producing none).
//!
//! Standalone containers that belong to no stack are importable too: each is
//! treated as a single-workload project.
//!
//! # Credentials
//!
//! - `credentials.base_url` — the Portainer URL (e.g. `https://1.2.3.4:9443`)
//! - `credentials.token` — the admin password
//! - `credentials.extra["username"]` — optional, defaults to `admin`

pub mod client;
pub mod error;
pub mod importer;
pub mod model;
pub mod validation;

pub use error::PortainerImportError;
pub use importer::PortainerImporter;
