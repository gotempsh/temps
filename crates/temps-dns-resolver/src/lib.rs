//! Per-node DNS resolver for the internal `*.temps.local` zone (ADR-011).
//!
//! Embedded in `temps-agent`. Listens on the bridge gateway IP (so every
//! container on the node sees it as their first nameserver) and on
//! `127.0.0.53` (so the host itself can `dig` for debugging).
//!
//! The resolver is fully decoupled from the control plane's database. It:
//!
//! 1. Reads zone state from a local in-memory [`ZoneStore`].
//! 2. Persists every applied generation to disk (`zone.json`) so a restart
//!    serves stale-but-correct records before the first sync completes.
//! 3. Long-polls `GET /internal/nodes/{id}/dns/changes?since=N` against the
//!    control plane via [`SyncClient`], applying diffs (or full snapshots)
//!    and ACKing back via `POST /internal/nodes/{id}/dns/ack`.
//! 4. Serves DNS over UDP and TCP via `hickory-server`.
//!
//! Failure model: every failure mode is "keep serving stale". Control plane
//! down = serve last snapshot. Disk write fails = log and keep serving from
//! memory. Bind fails = log and exit (the agent continues without DNS).

pub mod authority;
pub mod config;
pub mod error;
pub mod handle;
pub mod record;
pub mod sync_client;
pub mod upstream;
pub mod zone_store;

pub use config::ResolverConfig;
pub use error::ResolverError;
pub use handle::ResolverHandle;
pub use record::{OwnerKind, RecordKind, ZoneRecord};
pub use sync_client::SyncClient;
pub use zone_store::ZoneStore;

pub type Result<T> = std::result::Result<T, ResolverError>;
