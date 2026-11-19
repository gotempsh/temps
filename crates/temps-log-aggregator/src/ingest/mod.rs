// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP log ingest for sources outside Temps (ADR 073 D1).
//!
//! `POST /logs/ingest` takes newline-delimited JSON authenticated by a
//! project's log ingest key. A host, a database, a CDN or an app that Temps
//! did not deploy can send logs without speaking OTLP, and Vector or Fluent
//! Bit can forward syslog and files to it, so Temps runs no syslog listener.

pub mod keys;
pub mod ndjson;
pub mod service;

pub use keys::{
    AuthenticatedKey, CreatedLogIngestKey, LogIngestKeyService, NewLogIngestKey,
    LOG_INGEST_KEY_PREFIX, MAX_ACTIVE_KEYS_PER_PROJECT,
};
pub use service::{IngestOutcome, LogIngestService, MAX_SERVICES_PER_KEY};
