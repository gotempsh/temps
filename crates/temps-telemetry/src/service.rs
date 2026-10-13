// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Concrete anonymous-telemetry reporter.
//!
//! See [`crate`] and [`temps_core::telemetry`] for the abstraction and privacy
//! contract. This service:
//! - persists a stable random `anonymous_id` in the data directory (local
//!   installs) or reads the random one stored in PostgreSQL (stateless
//!   control planes, whose replicas share no data directory),
//! - honours the `TEMPS_TELEMETRY` opt-out env var (a host-level kill switch
//!   that always wins) and the admin preference stored in the settings row
//!   (`anonymous_telemetry_enabled`, applied at runtime via
//!   [`TelemetryService::apply_admin_preference`]),
//! - sends each event as a fire-and-forget timed HTTP POST so a dead endpoint
//!   never affects the running server.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde::Serialize;
use temps_core::telemetry::{TelemetryEvent, TelemetryReporter};
use thiserror::Error;

/// File (relative to the data dir) holding the stable anonymous instance id.
pub const ANONYMOUS_ID_FILE: &str = "anonymous_id";

/// Default central ingest endpoint. Overridable with `TEMPS_TELEMETRY_ENDPOINT`
/// (e.g. when self-hosting your own ingest, or pointing at a local dev server).
pub const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://telemetry.temps.sh/v1/events";

/// Whether anonymous telemetry is on when neither the environment kill switch
/// nor an admin preference says otherwise. Opt-out by default; whether this
/// stays `true` for GA is a maintainer decision tracked with the Settings ›
/// Telemetry page that discloses it.
pub const DEFAULT_TELEMETRY_ENABLED: bool = true;

/// Encodings of the cached admin preference in [`Inner::admin_preference`].
const PREFERENCE_UNSET: u8 = 0;
const PREFERENCE_ON: u8 = 1;
const PREFERENCE_OFF: u8 = 2;

/// Resolve whether events are sent, from the two inputs that can decide it.
/// The environment opt-out always wins; otherwise the admin preference, and
/// failing that [`DEFAULT_TELEMETRY_ENABLED`].
pub fn effective_enabled(env_opted_out: bool, admin_preference: Option<bool>) -> bool {
    !env_opted_out && admin_preference.unwrap_or(DEFAULT_TELEMETRY_ENABLED)
}

/// How long a single telemetry POST is allowed to take before being abandoned.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Error, Debug)]
pub enum TelemetryInitError {
    #[error("Failed to read/write anonymous id file at '{path}': {reason}")]
    AnonymousIdIo { path: String, reason: String },

    #[error("Failed to build telemetry HTTP client: {reason}")]
    HttpClient { reason: String },
}

/// The wire payload sent to the ingest API. Matches the Bun `telemetry-api`
/// `POST /v1/events` body.
#[derive(Debug, Serialize)]
struct EventPayload<'a> {
    anonymous_id: &'a str,
    event_type: &'a str,
    properties: &'a std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temps_version: Option<&'a str>,
}

/// Anonymous product-telemetry reporter.
#[derive(Clone)]
pub struct TelemetryService {
    inner: Arc<Inner>,
}

struct Inner {
    /// `TEMPS_TELEMETRY` set to an opt-out value at process start. Host-level
    /// kill switch: nothing an admin does in the console can override it.
    env_opted_out: bool,
    /// Cached admin preference (`PREFERENCE_*`). An atomic so `is_enabled()`
    /// stays a lock-free load on every call site, including hot paths that
    /// gate `report_once` on it.
    admin_preference: AtomicU8,
    anonymous_id: String,
    temps_version: String,
    endpoint: String,
    client: reqwest::Client,
    /// Database connection used to persist once-per-instance milestone claims
    /// (see [`TelemetryReporter::report_once`]). `None` until wired by the
    /// plugin; when absent, `report_once` falls back to the in-process cache
    /// only (still once-per-process, just not durable across restarts).
    db: Mutex<Option<Arc<DatabaseConnection>>>,
    /// In-process set of milestones already claimed this process. This is the
    /// hot-path guard: after the first emit of a given milestone, `report_once`
    /// returns on a cheap set lookup and NEVER touches the DB again — so a busy
    /// instance pays no per-event cost on the analytics/AI/deploy hot paths.
    claimed: Mutex<HashSet<&'static str>>,
}

impl TelemetryService {
    /// Build a reporter rooted at `data_dir`.
    ///
    /// `temps_version` is stamped onto every event (pass the server's
    /// git-describe `TEMPS_VERSION`, not `CARGO_PKG_VERSION` -- the latter is
    /// a static Cargo.toml value shared by nightly, beta, and release
    /// builds alike). Telemetry is enabled unless the operator opted out
    /// via `TEMPS_TELEMETRY` set to `0`/`false`/`off`/`no`. The anonymous id is
    /// always loaded/generated (even when disabled) so flipping telemetry back
    /// on doesn't churn the instance identity.
    pub fn new(
        data_dir: &Path,
        temps_version: impl Into<String>,
    ) -> Result<Self, TelemetryInitError> {
        Self::new_for_installation(data_dir, temps_version, None)
    }

    /// Build a reporter using the telemetry identity already resolved from
    /// PostgreSQL. Stateless callers pass the random anonymous ID stored in
    /// `stateless_control_plane.telemetry_anonymous_id` (see
    /// `temps_config::stateless_telemetry_anonymous_id`); local callers pass
    /// `None` and retain the data-directory identity file.
    pub fn new_for_installation(
        data_dir: &Path,
        temps_version: impl Into<String>,
        stateless_anonymous_id: Option<&str>,
    ) -> Result<Self, TelemetryInitError> {
        let version = temps_version.into();
        let env_opted_out = !Self::enabled_from_env();
        let endpoint = std::env::var("TEMPS_TELEMETRY_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_TELEMETRY_ENDPOINT.to_string());

        let anonymous_id = Self::load_or_create_anonymous_id(data_dir, stateless_anonymous_id)?;

        let client = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .user_agent(format!("temps-telemetry/{version}"))
            .build()
            .map_err(|e| TelemetryInitError::HttpClient {
                reason: e.to_string(),
            })?;

        Ok(Self {
            inner: Arc::new(Inner {
                env_opted_out,
                admin_preference: AtomicU8::new(PREFERENCE_UNSET),
                anonymous_id,
                temps_version: version,
                endpoint,
                client,
                db: Mutex::new(None),
                claimed: Mutex::new(HashSet::new()),
            }),
        })
    }

    /// Wire the database connection used to make [`TelemetryReporter::report_once`]
    /// durable across restarts (and across the split proxy/console processes,
    /// which share the same Postgres). Called by the telemetry plugin once the DB
    /// service is available. Without it, once-guarding still works but only
    /// per-process (an in-memory set), so a restart could re-emit a milestone
    /// once — acceptable, but the DB makes it truly once-per-instance.
    pub fn set_db(&self, db: Arc<DatabaseConnection>) {
        if let Ok(mut guard) = self.inner.db.lock() {
            *guard = Some(db);
        }
    }

    /// Apply the admin preference stored in the settings row
    /// (`anonymous_telemetry_enabled`). Takes effect for the next event: no
    /// restart, and in-flight sends re-check before going out.
    pub fn apply_admin_preference(&self, preference: Option<bool>) {
        let encoded = match preference {
            None => PREFERENCE_UNSET,
            Some(true) => PREFERENCE_ON,
            Some(false) => PREFERENCE_OFF,
        };
        let previous = self.inner.admin_preference.swap(encoded, Ordering::Relaxed);
        if previous != encoded {
            self.log_effective_state();
        }
    }

    /// The cached admin preference (`None` = not chosen, default applies).
    pub fn admin_preference(&self) -> Option<bool> {
        match self.inner.admin_preference.load(Ordering::Relaxed) {
            PREFERENCE_ON => Some(true),
            PREFERENCE_OFF => Some(false),
            _ => None,
        }
    }

    /// Whether `TEMPS_TELEMETRY` forced telemetry off for this process.
    pub fn env_opted_out(&self) -> bool {
        self.inner.env_opted_out
    }

    /// The ingest endpoint events are POSTed to.
    pub fn endpoint(&self) -> &str {
        &self.inner.endpoint
    }

    /// The version string stamped onto every event.
    pub fn temps_version(&self) -> &str {
        &self.inner.temps_version
    }

    /// Log one line stating whether telemetry is on and what decided it, so
    /// the server log always tells the operator the current state and how to
    /// change it.
    pub fn log_effective_state(&self) {
        if self.inner.env_opted_out {
            tracing::info!(
                "Anonymous product telemetry is DISABLED by TEMPS_TELEMETRY; \
                 the environment variable overrides Settings > Telemetry."
            );
            return;
        }
        match self.admin_preference() {
            Some(true) => tracing::info!(
                anonymous_id = %self.inner.anonymous_id,
                endpoint = %self.inner.endpoint,
                "Anonymous product telemetry is ENABLED by an admin in Settings > Telemetry. \
                 No PII is collected."
            ),
            Some(false) => tracing::info!(
                "Anonymous product telemetry is DISABLED by an admin in Settings > Telemetry."
            ),
            None if DEFAULT_TELEMETRY_ENABLED => tracing::info!(
                anonymous_id = %self.inner.anonymous_id,
                endpoint = %self.inner.endpoint,
                "Anonymous product telemetry is ENABLED (default). No PII is collected. \
                 Turn it off in Settings > Telemetry or with TEMPS_TELEMETRY=0."
            ),
            None => tracing::info!(
                "Anonymous product telemetry is DISABLED (default). \
                 An admin can turn it on in Settings > Telemetry."
            ),
        }
    }

    /// Read the `TEMPS_TELEMETRY` opt-out flag. Enabled by default; treats
    /// `0`/`false`/`off`/`no`/`disabled` (case-insensitive) as opt-out.
    pub fn enabled_from_env() -> bool {
        match std::env::var("TEMPS_TELEMETRY") {
            Ok(v) => !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "off" | "no" | "disabled"
            ),
            Err(_) => true,
        }
    }

    fn anonymous_id_path(data_dir: &Path) -> PathBuf {
        data_dir.join(ANONYMOUS_ID_FILE)
    }

    /// Load the persisted anonymous id, generating and persisting a new random
    /// one on first run. The id is a random UUID v4 — not derived from anything
    /// machine-identifying.
    ///
    /// Stateless control planes pass the random id already stored in
    /// PostgreSQL; it is used verbatim and no local file is written.
    fn load_or_create_anonymous_id(
        data_dir: &Path,
        stateless_anonymous_id: Option<&str>,
    ) -> Result<String, TelemetryInitError> {
        if let Some(id) = stateless_anonymous_id {
            let id = id.trim();
            if id.is_empty() {
                return Err(TelemetryInitError::AnonymousIdIo {
                    path: "stateless_control_plane.telemetry_anonymous_id".to_string(),
                    reason: "persisted stateless telemetry identity is empty".to_string(),
                });
            }
            return Ok(id.to_string());
        }

        let path = Self::anonymous_id_path(data_dir);

        if path.exists() {
            let raw =
                std::fs::read_to_string(&path).map_err(|e| TelemetryInitError::AnonymousIdIo {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                })?;
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
            // Empty/corrupt file: fall through and regenerate.
        }

        let id = format!("inst_{}", uuid::Uuid::new_v4().simple());

        // Best-effort create the data dir; it normally already exists.
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &id).map_err(|e| TelemetryInitError::AnonymousIdIo {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

        Ok(id)
    }

    /// Send one event and wait for the request to finish (bounded by the
    /// client timeout). Only for paths where the process is about to exit, such
    /// as a failed startup, where a fire-and-forget task would never run.
    /// Never returns an error: telemetry must not change the caller's outcome.
    pub async fn send_now(&self, event: TelemetryEvent) {
        if !self.is_enabled() {
            return;
        }
        let payload = EventPayload {
            anonymous_id: &self.inner.anonymous_id,
            event_type: &event.event_type,
            properties: &event.properties,
            temps_version: if self.inner.temps_version.is_empty() {
                None
            } else {
                Some(&self.inner.temps_version)
            },
        };
        match self
            .inner
            .client
            .post(&self.inner.endpoint)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => tracing::debug!(
                event = %event.event_type,
                status = %response.status(),
                "telemetry endpoint rejected the event (ignored)"
            ),
            Err(e) => tracing::debug!(
                event = %event.event_type,
                error = %e,
                "telemetry send failed (ignored)"
            ),
        }
    }

    /// The stable anonymous id for this instance (exposed for diagnostics).
    pub fn anonymous_id(&self) -> &str {
        &self.inner.anonymous_id
    }
}

#[async_trait::async_trait]
impl TelemetryReporter for TelemetryService {
    fn report(&self, event: TelemetryEvent) {
        if !self.is_enabled() {
            return;
        }

        // Clone the small amount of state the background task needs. The whole
        // point is to return immediately; all network work happens in a
        // detached task with its own timeout (the client is configured with
        // SEND_TIMEOUT).
        let inner = self.inner.clone();
        let reporter = self.clone();

        tokio::spawn(async move {
            // An admin may have turned telemetry off between `report()` and
            // this task running; honour that rather than send one more event.
            if !reporter.is_enabled() {
                return;
            }
            let payload = EventPayload {
                anonymous_id: &inner.anonymous_id,
                event_type: &event.event_type,
                properties: &event.properties,
                temps_version: if inner.temps_version.is_empty() {
                    None
                } else {
                    Some(&inner.temps_version)
                },
            };

            match inner
                .client
                .post(&inner.endpoint)
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::trace!(
                        event = %event.event_type,
                        "telemetry event sent"
                    );
                }
                Ok(resp) => {
                    // Non-2xx is not an error worth surfacing loudly — telemetry
                    // is best-effort. Debug level keeps it out of normal logs.
                    tracing::debug!(
                        event = %event.event_type,
                        status = %resp.status(),
                        "telemetry endpoint returned non-success status"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        event = %event.event_type,
                        error = %e,
                        "telemetry send failed (ignored)"
                    );
                }
            }
        });
    }

    fn report_once(&self, milestone: &'static str, event: TelemetryEvent) {
        if !self.is_enabled() {
            return;
        }

        // ── Hot-path guard ──
        // After the first emit of this milestone in this process, this is a
        // cheap set lookup and we return WITHOUT touching the DB or spawning a
        // task. This is what keeps the analytics/AI/deploy ingestion paths free
        // of any per-event telemetry cost.
        {
            let mut claimed = match self.inner.claimed.lock() {
                Ok(g) => g,
                // A poisoned lock should never happen (we hold it only for these
                // tiny critical sections), but if it does, fail safe: don't emit.
                Err(_) => return,
            };
            if claimed.contains(milestone) {
                return;
            }
            // Optimistically mark claimed-in-process so concurrent callers also
            // short-circuit. The DB (below) is the cross-process / cross-restart
            // arbiter of whether we actually emit.
            claimed.insert(milestone);
        }

        let inner = self.inner.clone();
        let reporter = self.clone();
        tokio::spawn(async move {
            // Snapshot the DB handle (if wired) without holding the lock across
            // the await.
            let db = inner.db.lock().ok().and_then(|g| g.clone());

            match db {
                Some(db) => {
                    // Durable claim: only the FIRST claimant across all processes
                    // and restarts inserts a row; everyone else is a no-op. We
                    // emit the event only when we won the claim — a single-row
                    // INSERT ... ON CONFLICT DO NOTHING yields rows_affected 1
                    // (won) or 0 (already claimed).
                    let stmt = Statement::from_sql_and_values(
                        db.get_database_backend(),
                        "INSERT INTO telemetry_milestones (milestone) VALUES ($1) \
                         ON CONFLICT (milestone) DO NOTHING",
                        [milestone.into()],
                    );
                    match db.execute(stmt).await {
                        Ok(res) if res.rows_affected() >= 1 => {
                            // We won the claim — emit exactly once.
                            reporter.report(event);
                        }
                        Ok(_) => {
                            // Already claimed by a prior run/process — don't emit.
                            tracing::trace!(
                                milestone = %milestone,
                                "telemetry milestone already claimed; skipping emit"
                            );
                        }
                        Err(e) => {
                            // DB error claiming the milestone. Best-effort: do NOT
                            // emit (avoid re-introducing the firehose if the DB is
                            // flaky); the in-process set still prevents retries
                            // this process.
                            tracing::debug!(
                                milestone = %milestone,
                                error = %e,
                                "telemetry milestone claim failed; skipping emit"
                            );
                        }
                    }
                }
                None => {
                    // No DB wired (e.g. early startup): fall back to the
                    // in-process guard we already set above — once per process.
                    reporter.report(event);
                }
            }
        });
    }

    fn is_enabled(&self) -> bool {
        effective_enabled(self.inner.env_opted_out, self.admin_preference())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::telemetry::TelemetryEventKind;

    /// `TEMPS_TELEMETRY` is process-wide, and the test harness runs tests in
    /// parallel: one test opting out would flip another's `enabled` mid-run.
    /// Every test that reads or writes the variable holds this lock.
    static TELEMETRY_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_telemetry_env() -> std::sync::MutexGuard<'static, ()> {
        // A panicking test poisons the lock; the variable is reset by the next
        // holder anyway, so the poison carries no information.
        TELEMETRY_ENV
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A temp dir helper that doesn't pull in extra deps.
    fn temp_dir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!("temps-telemetry-test-{}", uuid::Uuid::new_v4().simple());
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn anonymous_id_is_stable_across_loads() {
        let dir = temp_dir();
        let id1 = TelemetryService::load_or_create_anonymous_id(&dir, None).unwrap();
        let id2 = TelemetryService::load_or_create_anonymous_id(&dir, None).unwrap();
        assert_eq!(id1, id2, "anonymous id must be stable once generated");
        assert!(id1.starts_with("inst_"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn anonymous_id_regenerated_when_file_empty() {
        let dir = temp_dir();
        let path = TelemetryService::anonymous_id_path(&dir);
        std::fs::write(&path, "   ").unwrap();
        let id = TelemetryService::load_or_create_anonymous_id(&dir, None).unwrap();
        assert!(id.starts_with("inst_"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persisted_stateless_identity_is_used_verbatim_without_a_local_file() {
        let dir = temp_dir();
        let stored = "inst_0123456789abcdef0123456789abcdef";
        let service =
            TelemetryService::new_for_installation(&dir, "0.0.0-test", Some(stored)).unwrap();

        // The random id stored in PostgreSQL is reported as-is: nothing is
        // derived from the operator-chosen instance name.
        assert_eq!(service.anonymous_id(), stored);
        assert!(!TelemetryService::anonymous_id_path(&dir).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_persisted_stateless_identity_is_rejected() {
        let dir = temp_dir();
        let error = TelemetryService::load_or_create_anonymous_id(&dir, Some("  "))
            .expect_err("an empty stored id must not become the telemetry identity");
        assert!(error
            .to_string()
            .contains("stateless_control_plane.telemetry_anonymous_id"));
        assert!(!TelemetryService::anonymous_id_path(&dir).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn disabled_service_is_noop_and_reports_disabled() {
        let dir = temp_dir();
        let svc = {
            // Force opt-out for this construction only; the env lock must not
            // be held across the await below.
            let _env = lock_telemetry_env();
            std::env::set_var("TEMPS_TELEMETRY", "0");
            let svc = TelemetryService::new(&dir, "0.0.0-test").unwrap();
            std::env::remove_var("TEMPS_TELEMETRY");
            svc
        };

        assert!(!svc.is_enabled());
        // Must not panic and must not spawn a request.
        svc.report(TelemetryEvent::new(TelemetryEventKind::ProjectCreated));
        // The synchronous path honours the opt-out too: it returns without
        // building a request, so it cannot wait on the network.
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            svc.send_now(TelemetryEvent::new(TelemetryEventKind::UpgradeFailed)),
        )
        .await
        .expect("opted-out send_now must return immediately");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn report_once_records_milestone_in_process_guard() {
        let _env = lock_telemetry_env();
        let dir = temp_dir();
        std::env::remove_var("TEMPS_TELEMETRY");
        // Never let a unit test reach the real ingest endpoint: the discard
        // port on loopback refuses the connection immediately.
        std::env::set_var("TEMPS_TELEMETRY_ENDPOINT", "http://127.0.0.1:9/v1/events");
        let svc = TelemetryService::new(&dir, "0.0.0-test").unwrap();
        std::env::remove_var("TEMPS_TELEMETRY_ENDPOINT");
        assert!(svc.is_enabled());

        // Not claimed yet.
        assert!(!svc
            .inner
            .claimed
            .lock()
            .unwrap()
            .contains("analytics_first_event_received"));

        // First call records the milestone in the in-process guard so subsequent
        // calls short-circuit (no DB wired here, so this is the only guard).
        svc.report_once(
            "analytics_first_event_received",
            TelemetryEvent::new(TelemetryEventKind::AnalyticsFirstEventReceived),
        );
        assert!(
            svc.inner
                .claimed
                .lock()
                .unwrap()
                .contains("analytics_first_event_received"),
            "first report_once should record the milestone"
        );

        // A second call is a cheap no-op (still exactly one entry).
        svc.report_once(
            "analytics_first_event_received",
            TelemetryEvent::new(TelemetryEventKind::AnalyticsFirstEventReceived),
        );
        assert_eq!(
            svc.inner
                .claimed
                .lock()
                .unwrap()
                .iter()
                .filter(|m| **m == "analytics_first_event_received")
                .count(),
            1,
            "milestone recorded exactly once"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn report_once_is_noop_when_disabled() {
        let _env = lock_telemetry_env();
        let dir = temp_dir();
        std::env::set_var("TEMPS_TELEMETRY", "0");
        let svc = TelemetryService::new(&dir, "0.0.0-test").unwrap();
        std::env::remove_var("TEMPS_TELEMETRY");

        assert!(!svc.is_enabled());
        // Disabled: must not record anything (returns before the guard).
        svc.report_once(
            "analytics_first_event_received",
            TelemetryEvent::new(TelemetryEventKind::AnalyticsFirstEventReceived),
        );
        assert!(
            svc.inner.claimed.lock().unwrap().is_empty(),
            "disabled reporter must not claim milestones"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Minimal local HTTP sink standing in for the ingest endpoint, so tests
    /// can count what would have been sent without touching the network.
    async fn spawn_counting_sink() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind sink");
        let addr = listener.local_addr().expect("sink addr");
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = socket.read(&mut buf).await;
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        (format!("http://{addr}/v1/events"), hits)
    }

    fn service_with_endpoint(dir: &Path, endpoint: &str, env_opt_out: bool) -> TelemetryService {
        let _env = lock_telemetry_env();
        std::env::set_var("TEMPS_TELEMETRY_ENDPOINT", endpoint);
        if env_opt_out {
            std::env::set_var("TEMPS_TELEMETRY", "0");
        } else {
            std::env::remove_var("TEMPS_TELEMETRY");
        }
        let svc = TelemetryService::new(dir, "0.0.0-test").unwrap();
        std::env::remove_var("TEMPS_TELEMETRY_ENDPOINT");
        std::env::remove_var("TEMPS_TELEMETRY");
        svc
    }

    #[test]
    fn effective_enabled_precedence_env_then_admin_then_default() {
        assert_eq!(effective_enabled(false, None), DEFAULT_TELEMETRY_ENABLED);
        assert!(effective_enabled(false, Some(true)));
        assert!(!effective_enabled(false, Some(false)));
        // The environment kill switch wins over an admin opt-in.
        assert!(!effective_enabled(true, Some(true)));
        assert!(!effective_enabled(true, None));
    }

    #[tokio::test]
    async fn admin_preference_switches_sending_at_runtime() {
        let dir = temp_dir();
        let (endpoint, hits) = spawn_counting_sink().await;
        let svc = service_with_endpoint(&dir, &endpoint, false);
        assert_eq!(svc.endpoint(), endpoint);

        // Admin turns it off: no event of any kind leaves the process.
        svc.apply_admin_preference(Some(false));
        assert!(!svc.is_enabled());
        svc.report(TelemetryEvent::new(TelemetryEventKind::InstanceHeartbeat));
        svc.report_once(
            "first_deploy_succeeded",
            TelemetryEvent::new(TelemetryEventKind::FirstDeploySucceeded),
        );
        svc.send_now(TelemetryEvent::new(TelemetryEventKind::UpgradeFailed))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0, "disabled must send nothing");
        assert!(
            svc.inner.claimed.lock().unwrap().is_empty(),
            "a disabled reporter must not burn milestones"
        );

        // Admin turns it back on: the very next event is sent, no restart.
        svc.apply_admin_preference(Some(true));
        assert!(svc.is_enabled());
        svc.send_now(TelemetryEvent::new(TelemetryEventKind::InstanceHeartbeat))
            .await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn env_opt_out_overrides_admin_opt_in() {
        let dir = temp_dir();
        let (endpoint, hits) = spawn_counting_sink().await;
        let svc = service_with_endpoint(&dir, &endpoint, true);
        assert!(svc.env_opted_out());

        svc.apply_admin_preference(Some(true));
        assert!(!svc.is_enabled(), "TEMPS_TELEMETRY=0 must win");
        svc.send_now(TelemetryEvent::new(TelemetryEventKind::InstanceHeartbeat))
            .await;
        svc.report(TelemetryEvent::new(TelemetryEventKind::InstanceHeartbeat));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enabled_from_env_defaults_on_and_honors_opt_out() {
        let _env = lock_telemetry_env();
        std::env::remove_var("TEMPS_TELEMETRY");
        assert!(TelemetryService::enabled_from_env());

        for off in ["0", "false", "OFF", "No", "disabled"] {
            std::env::set_var("TEMPS_TELEMETRY", off);
            assert!(
                !TelemetryService::enabled_from_env(),
                "{off} should disable"
            );
        }
        for on in ["1", "true", "yes", "anything-else"] {
            std::env::set_var("TEMPS_TELEMETRY", on);
            assert!(TelemetryService::enabled_from_env(), "{on} should enable");
        }
        std::env::remove_var("TEMPS_TELEMETRY");
    }
}
