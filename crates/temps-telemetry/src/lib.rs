//! Anonymous product telemetry reporter for Temps.
//!
//! Temps optionally reports **anonymous** product-usage events (e.g. "an
//! instance attempted a deploy" vs "an instance deployed successfully") to a
//! central endpoint so the maintainers can tell whether the product is working
//! for self-hosters and where to invest. The data is anonymous by design:
//!
//! - A stable random `anonymous_id` (UUID v4) is generated on the instance and
//!   persisted in the data directory. It is never derived from anything
//!   machine-identifying.
//! - Events carry only the event name and a small bag of **non-identifying**
//!   properties (counts, enum labels, durations). No emails, IPs, repo names,
//!   domains, env-var names/values, or free-form user text are ever sent.
//! - Reporting is fire-and-forget and time-bounded: a dead or slow endpoint has
//!   zero effect on the running server.
//!
//! Operators opt out by setting `TEMPS_TELEMETRY=0` (also `false`/`off`/`no`).
//! The ingest endpoint is overridable with `TEMPS_TELEMETRY_ENDPOINT`.
//!
//! The abstraction (`TelemetryReporter`, `TelemetryEvent`, `TelemetryEventKind`)
//! lives in [`temps_core::telemetry`] so feature crates depend on the trait,
//! not on this crate.

mod plugin;
mod service;

pub use plugin::TelemetryPlugin;
pub use service::{
    TelemetryInitError, TelemetryService, ANONYMOUS_ID_FILE, DEFAULT_TELEMETRY_ENDPOINT,
};

// Convenience re-exports so consumers can pull the event vocabulary from one
// place.
pub use temps_core::telemetry::{
    NoopTelemetryReporter, TelemetryEvent, TelemetryEventKind, TelemetryReporter,
};
