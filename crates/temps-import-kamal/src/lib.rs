// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kamal Importer
//!
//! Implements the `WorkloadImporter` trait for [Kamal](https://kamal-deploy.org)
//! by reading its **`config/deploy.yml`** rather than talking to a server.
//!
//! Kamal has no control plane: it is a CLI that ships containers to hosts over
//! SSH, and the entire desired state lives in the repository. `deploy.yml` is
//! therefore the source of truth, and it is the most declarative source temps
//! imports from — everything a migration needs is stated explicitly:
//!
//! | Kamal | temps |
//! |---|---|
//! | `service` / `image` | project + deployment image |
//! | `servers.<role>` | the primary role becomes the deployment; other roles become additional deployments (with their `cmd`) |
//! | `proxy.host` | a real custom domain (not an IP-derived one) |
//! | `proxy.app_port` | the container port |
//! | `env.clear` | environment variables |
//! | `env.secret` | environment variables flagged `is_secret`, with **no value** — Kamal keeps them in `.kamal/secrets`, outside the config |
//! | `accessories.*` | managed-service candidates (databases/caches) |
//! | `builder.dockerfile` | build configuration |
//! | `volumes` | reported as unsupported (temps does not copy volume contents) |
//!
//! # Credentials
//!
//! There is nothing to authenticate against. The config arrives per-request:
//!
//! - `credentials.extra["deploy_yml"]` — the contents of `config/deploy.yml`
//!
//! Modelled against Kamal 2.12's own `kamal init` template and validated with
//! `kamal config` (which resolves roles, hosts, and accessories).

pub mod error;
pub mod importer;
pub mod model;
pub mod validation;

pub use error::KamalImportError;
pub use importer::KamalImporter;
