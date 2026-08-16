//! Runtime configuration service for the admin gate.
//!
//! The gate itself (see `admin_gate.rs`) holds an atomic `AdminGateHandle`
//! that the middleware reads per request. This service owns the *source* of
//! that handle:
//!
//! 1. **Env precedence.** When any of `TEMPS_ADMIN_ALLOWED_IPS`,
//!    `TEMPS_ADMIN_ALLOWED_HOSTS`, or `TEMPS_ADMIN_TRUST_FORWARDED_FOR` is
//!    set, the env values win and the DB is ignored. UI writes are rejected
//!    with a 409. This keeps GitOps/Ansible setups predictable.
//!
//! 2. **DB-backed otherwise.** Settings are stored as a JSON sub-document on
//!    the existing `settings` singleton row under the key `admin_gate`. On
//!    boot, the service loads that row and pushes the result into the
//!    handle. Subsequent writes go through `update()` which validates,
//!    persists, then atomic-swaps the handle.
//!
//! The DB is only touched at boot and on explicit writes — never on the
//! request path.

use std::net::IpAddr;
use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseConnection, EntityTrait,
    QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

use super::admin_gate::{AdminGateConfig, AdminGateConfigError, AdminGateHandle, AdminGateSource};

/// Postgres channel the settings row's trigger fires on.
const SETTINGS_CHANGE_CHANNEL: &str = "settings_change";

/// Reconciliation floor for the gate listener. `PgListener` does not replay
/// notifications missed while its connection was down, so without a periodic
/// re-read a single dropped NOTIFY strands the process on a stale allowlist
/// forever, with nothing to tell the operator.
const RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

const RECONNECT_BACKOFF_MIN: std::time::Duration = std::time::Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// JSON shape stored under `settings.data["admin_gate"]`. Versioned so we
/// can evolve the schema without a migration.
///
/// Deliberately NOT `deny_unknown_fields`: a newer binary in a rolling upgrade
/// (or before a rollback) may have written a field this build doesn't know,
/// and rejecting it would propagate out of `AdminGateService::new` and stop
/// BOTH `temps proxy` and `temps serve` from booting at all — turning a
/// forward-compatible field into an outage. Unknown keys are instead reported
/// loudly by [`unknown_admin_gate_keys`], which covers the case that motivated
/// strictness (a renamed/typo'd key silently deserializing to an empty, i.e.
/// OPEN, allowlist) without the availability cost.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AdminGateSettings {
    #[serde(default)]
    pub allowed_ips: Vec<String>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub trust_forwarded_for: bool,
}

#[derive(Debug, Error)]
pub enum AdminGateServiceError {
    #[error("Admin gate config invalid: {0}")]
    Invalid(#[from] AdminGateConfigError),

    #[error("Admin gate config is read-only because TEMPS_ADMIN_* env vars are set; unset them to enable runtime configuration")]
    EnvOverridden,

    #[error(
        "Refusing to save: the new rules would deny the caller's connection \
        (ip={caller_ip}, host={caller_host:?}). Add your address/host to the \
        lists or clear the gate before saving."
    )]
    WouldLockOut {
        caller_ip: IpAddr,
        caller_host: Option<String>,
    },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Failed to (de)serialize admin gate settings: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Runtime service that owns the gate handle and persists user updates.
#[derive(Clone)]
pub struct AdminGateService {
    db: Arc<DatabaseConnection>,
    handle: AdminGateHandle,
    /// True when env vars dictate the active config. Set once at construction
    /// time and never changes — the process must restart to flip modes.
    env_overridden: bool,
}

impl AdminGateService {
    /// Build the service. Resolves the *initial* config according to env
    /// precedence, pushes it into a fresh handle, and returns both halves.
    ///
    /// - If any of the env-derived args is non-default, the active config is
    ///   `AdminGateSource::Env` and the DB row (if any) is left untouched.
    /// - Otherwise, the service reads the `admin_gate` JSON subkey from the
    ///   `settings` row and uses that. Empty row → `AdminGateSource::Default`.
    pub async fn new(
        db: Arc<DatabaseConnection>,
        env_allowed_ips: &[String],
        env_allowed_hosts: &[String],
        env_trust_forwarded_for: bool,
    ) -> Result<(Self, AdminGateHandle), AdminGateServiceError> {
        let env_active =
            !env_allowed_ips.is_empty() || !env_allowed_hosts.is_empty() || env_trust_forwarded_for;

        let initial = if env_active {
            info!(
                allowed_ips = ?env_allowed_ips,
                allowed_hosts = ?env_allowed_hosts,
                trust_forwarded_for = env_trust_forwarded_for,
                "Admin gate: using env-supplied config (DB-backed UI will be read-only)"
            );
            AdminGateConfig::from_env(env_allowed_ips, env_allowed_hosts, env_trust_forwarded_for)?
        } else {
            match load_from_db(db.as_ref()).await {
                Ok(Some(settings)) => {
                    info!(
                        allowed_ips = ?settings.allowed_ips,
                        allowed_hosts = ?settings.allowed_hosts,
                        trust_forwarded_for = settings.trust_forwarded_for,
                        "Admin gate: loaded config from settings row"
                    );
                    AdminGateConfig::from_parts(
                        &settings.allowed_ips,
                        &settings.allowed_hosts,
                        settings.trust_forwarded_for,
                        AdminGateSource::Db,
                    )?
                }
                Ok(None) => AdminGateConfig::from_parts(&[], &[], false, AdminGateSource::Default)?,
                Err(e) => {
                    // SECURITY: fail-CLOSED on load error. Previously
                    // we silently installed a noop config here, which
                    // meant any DB problem (corrupt settings row,
                    // transient DB outage, JSON parse failure) would
                    // open the management surface to the world. That
                    // turns "the gate config is broken" into a
                    // privilege-escalation event for anyone who can
                    // reach the box. Refuse to boot instead — the
                    // operator must explicitly intervene (fix the
                    // row, or set TEMPS_ADMIN_* env vars to bypass
                    // the DB path).
                    tracing::error!(
                        target: "temps_cli::admin_gate",
                        error = %e,
                        "Admin gate: failed to load settings from DB. Refusing to start with an open gate. \
                         Fix the `settings` row, or set TEMPS_ADMIN_ALLOWED_IPS / TEMPS_ADMIN_ALLOWED_HOSTS \
                         to override via env."
                    );
                    return Err(e);
                }
            }
        };

        let handle = AdminGateHandle::new(initial);
        Ok((
            Self {
                db,
                handle: handle.clone(),
                env_overridden: env_active,
            },
            handle,
        ))
    }

    /// True when env vars are the source of truth and the UI must show
    /// read-only.
    pub fn env_overridden(&self) -> bool {
        self.env_overridden
    }

    /// Snapshot the current config.
    pub fn snapshot(&self) -> Arc<AdminGateConfig> {
        self.handle.current()
    }

    /// Persist a new config and swap the live handle.
    ///
    /// `caller_ip` / `caller_host` are used for a lockout pre-flight: if the
    /// new rules would deny the caller, the write is rejected.
    pub async fn update(
        &self,
        new_settings: AdminGateSettings,
        caller_ip: IpAddr,
        caller_host: Option<&str>,
    ) -> Result<Arc<AdminGateConfig>, AdminGateServiceError> {
        if self.env_overridden {
            return Err(AdminGateServiceError::EnvOverridden);
        }

        // Build the candidate config — this also validates CIDRs/hosts.
        let candidate = AdminGateConfig::from_parts(
            &new_settings.allowed_ips,
            &new_settings.allowed_hosts,
            new_settings.trust_forwarded_for,
            AdminGateSource::Db,
        )?;

        // Lockout pre-flight: only meaningful when the new config is *not*
        // a noop. A noop config allows everyone, so it can never lock out.
        if !candidate.is_noop() && !candidate.would_allow(caller_ip, caller_host) {
            return Err(AdminGateServiceError::WouldLockOut {
                caller_ip,
                caller_host: caller_host.map(|s| s.to_string()),
            });
        }

        persist_to_db(self.db.as_ref(), &new_settings).await?;
        let prev = self.handle.store(candidate.clone());
        info!(
            allowed_ips = ?new_settings.allowed_ips,
            allowed_hosts = ?new_settings.allowed_hosts,
            trust_forwarded_for = new_settings.trust_forwarded_for,
            previous_source = ?prev.source,
            "Admin gate: configuration reloaded from DB"
        );
        Ok(self.handle.current())
    }

    /// Re-read the gate config from the DB and swap the live handle.
    ///
    /// Used by the `settings_change` listener so a process that did NOT
    /// perform the write (the standalone `temps proxy`, whose console lives in
    /// a different process) still converges on the operator's current config.
    ///
    /// Fails SAFE in every direction: a load error, a malformed sub-document,
    /// or a *missing* sub-document all retain the currently active config
    /// rather than falling back to an open gate. A transient DB blip, a
    /// restore from a pre-gate backup, or an out-of-band writer must never
    /// widen the management surface. Env-overridden processes are a no-op,
    /// matching `update()`'s precedence rule.
    pub async fn reload_from_db(&self) -> Result<Arc<AdminGateConfig>, AdminGateServiceError> {
        if self.env_overridden {
            return Ok(self.handle.current());
        }

        let Some(settings) = load_from_db(self.db.as_ref()).await? else {
            // The sub-document is GONE, which is NOT the same as "the operator
            // cleared the gate". `update()` always persists an explicit
            // `admin_gate` object — empty lists included — so a deliberate
            // clear comes back through the `Some` branch below and converges
            // to a noop config normally. Reaching here means the key was
            // destroyed out of band: a settings writer that dropped it, a
            // control-plane restore from a snapshot taken before the gate was
            // configured, or a DELETE on the row. Widening the gate on that
            // signal would turn any of those into instant, silent privilege
            // escalation on a LIVE proxy — the boot path already refuses to
            // start with an open gate for exactly this reason, so the reload
            // path must not do by NOTIFY what boot refuses to do at all.
            //
            // Note the deliberate asymmetry with `new()`, which maps a missing
            // key to an open Default config: at boot there is no prior state,
            // so "the key is absent" is indistinguishable from "this operator
            // has never configured a gate" — and a fresh install must be able
            // to start. Only here, with a known-good previous config in hand,
            // can absence be recognized as loss rather than as never-set. The
            // protection is therefore uptime-scoped: it stops a running proxy
            // from being opened by a NOTIFY, but a restart after the key is
            // gone still comes up open. The merge fix in `to_json_merged` is
            // what stops the key from being destroyed in the first place; this
            // is the backstop for writers outside this repo.
            let current = self.handle.current();
            if !current.is_noop() {
                tracing::error!(
                    target: "temps_cli::admin_gate",
                    "Admin gate: the `admin_gate` sub-document disappeared from the settings \
                     row. REFUSING to widen the gate — keeping the currently active allowlist. \
                     Re-save the admin gate under Settings → Security to restore it in the \
                     database, otherwise the next restart of this process WILL come up with an \
                     open gate."
                );
            }
            return Ok(current);
        };

        let config = AdminGateConfig::from_parts(
            &settings.allowed_ips,
            &settings.allowed_hosts,
            settings.trust_forwarded_for,
            AdminGateSource::Db,
        )?;

        // Any reload that turns a restrictive gate into an open one is worth an
        // ERROR even when it is legitimate (the operator cleared the lists):
        // on a self-hosted box this is the only signal that the management
        // surface just became reachable from everywhere.
        let widening = !self.handle.current().is_noop() && config.is_noop();
        let prev = self.handle.store(config);
        if widening {
            tracing::error!(
                target: "temps_cli::admin_gate",
                "Admin gate: reload installed an EMPTY allowlist — the management surface is \
                 now reachable from any IP and any Host header. This is expected only if the \
                 gate was just cleared in the console."
            );
        } else {
            let now = self.handle.current();
            info!(
                previous_source = ?prev.source,
                allowed_ips = ?now.allowed_nets.iter().map(|n| n.to_string()).collect::<Vec<_>>(),
                allowed_hosts = ?now.allowed_hosts,
                trust_forwarded_for = now.trust_forwarded_for,
                is_noop = now.is_noop(),
                "Admin gate: reloaded after settings_change notification"
            );
        }
        Ok(self.handle.current())
    }

    /// Spawn the background task that LISTENs on the Postgres `settings_change`
    /// channel and reloads the gate whenever ANY process writes the settings
    /// row.
    ///
    /// In the single-binary `temps serve` the console and the Pingora proxy
    /// share one `AdminGateHandle`, so a UI save is visible to both instantly.
    /// In the ADR-017 split topology they are separate processes with separate
    /// handles: without this listener the standalone `temps proxy` enforces
    /// whatever allowlist existed when it booted, forever. That divergence is
    /// visible in both directions — a newly saved allowlist isn't enforced at
    /// the proxy (management surface stays reachable from disallowed hosts),
    /// and a cleared allowlist keeps 404ing hosts that should now fall through
    /// to the console.
    ///
    /// The task never gives up: connect, subscribe and recv failures all retry
    /// with capped backoff rather than returning, because a listener that
    /// silently exits leaves the process enforcing a stale allowlist with no
    /// indication anything is wrong. A 60s reconcile tick runs alongside the
    /// subscription so a NOTIFY dropped during a transparent reconnect
    /// self-heals instead of stranding the gate.
    pub fn start_settings_listener(self: &Arc<Self>, database_url: String) {
        let service = Arc::clone(self);

        // Env precedence: the DB is not the source of truth here, so there is
        // nothing to converge on.
        if service.env_overridden {
            return;
        }

        tokio::spawn(async move {
            use sqlx::postgres::{PgListener, PgPoolOptions};

            // One connection is all a listener needs; the process already has a
            // Sea-ORM pool for everything else and this runs on a 4 GB box
            // alongside the proxy.
            let pool = loop {
                match PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&database_url)
                    .await
                {
                    Ok(pool) => break pool,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Admin gate: failed to connect for settings_change LISTEN; \
                             retrying in {}s. Until this succeeds the gate keeps its \
                             boot-time allowlist and will NOT see console edits.",
                            RECONNECT_BACKOFF_MAX.as_secs()
                        );
                        tokio::time::sleep(RECONNECT_BACKOFF_MAX).await;
                    }
                }
            };

            // Outer loop owns listener construction so every reconnect builds a
            // FRESH `PgListener`. Re-calling `listen()` on an existing one
            // appends to its internal channel list and re-issues LISTEN for
            // every entry on reconnect, which leaks on a flapping connection.
            let mut backoff = RECONNECT_BACKOFF_MIN;
            loop {
                let mut listener = match PgListener::connect_with(&pool).await {
                    Ok(mut listener) => match listener.listen(SETTINGS_CHANGE_CHANNEL).await {
                        Ok(()) => {
                            backoff = RECONNECT_BACKOFF_MIN;
                            info!(
                                "Admin gate: listening for settings_change to refresh the allowlist"
                            );
                            listener
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Admin gate: failed to subscribe to settings_change; retrying"
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                            continue;
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Admin gate: failed to create PgListener; retrying"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                        continue;
                    }
                };

                // Reload once on (re)subscribe so anything written while we had
                // no subscription is picked up immediately.
                reload_logging_failure(&service).await;

                // `PgListener::recv` reconnects transparently and documents that
                // notifications received while the connection was down are NOT
                // replayed — so a Postgres restart or connection reaper silently
                // eats the NOTIFY without ever returning an error. The interval
                // is the reconciliation floor that makes that self-healing: one
                // indexed single-row read per minute, which is nothing next to
                // being silently stuck on a stale allowlist.
                let mut reconcile = tokio::time::interval(RECONCILE_INTERVAL);
                reconcile.tick().await; // first tick completes immediately

                loop {
                    tokio::select! {
                        received = listener.recv() => match received {
                            Ok(_) => reload_logging_failure(&service).await,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Admin gate: settings_change listener error; rebuilding"
                                );
                                tokio::time::sleep(backoff).await;
                                backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                                break;
                            }
                        },
                        _ = reconcile.tick() => reload_logging_failure(&service).await,
                    }
                }
            }
        });
    }
}

/// Reload the gate, logging (never propagating) a failure. A failed reload
/// retains the currently active config — see `reload_from_db`.
async fn reload_logging_failure(service: &AdminGateService) {
    if let Err(e) = service.reload_from_db().await {
        tracing::error!(
            target: "temps_cli::admin_gate",
            error = %e,
            "Admin gate: reload failed; keeping the previously active configuration"
        );
    }
}

/// Read the `admin_gate` key off the singleton `settings` row. Returns
/// `Ok(None)` when either the row doesn't exist yet or the key isn't set —
/// both mean "no DB config", and the caller will fall back to defaults.
async fn load_from_db(
    db: &DatabaseConnection,
) -> Result<Option<AdminGateSettings>, AdminGateServiceError> {
    let row = temps_entities::settings::Entity::find_by_id(1)
        .one(db)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    match row.data.get("admin_gate").cloned() {
        Some(val) if !val.is_null() => {
            // Report schema drift before parsing. A renamed or typo'd key
            // (`allowedHosts`, `allow_ips`) deserializes happily into
            // all-defaults — an OPEN gate — and this is the only place that
            // can tell the operator why their allowlist stopped applying.
            let unknown = unknown_admin_gate_keys(&val);
            if !unknown.is_empty() {
                tracing::error!(
                    target: "temps_cli::admin_gate",
                    unknown_keys = ?unknown,
                    "Admin gate: the stored `admin_gate` document contains unrecognized keys. \
                     They are IGNORED — if one is a misspelling of allowed_ips / allowed_hosts / \
                     trust_forwarded_for, the gate is running with a narrower (possibly empty) \
                     allowlist than intended. Re-save it under Settings → Security."
                );
            }
            let settings: AdminGateSettings = serde_json::from_value(val)?;
            Ok(Some(settings))
        }
        _ => Ok(None),
    }
}

/// Keys present in a stored `admin_gate` document that this build does not
/// recognize. Used for operator-visible drift reporting only — unknown keys are
/// never fatal, see [`AdminGateSettings`].
fn unknown_admin_gate_keys(value: &serde_json::Value) -> Vec<String> {
    const KNOWN: [&str; 3] = ["allowed_ips", "allowed_hosts", "trust_forwarded_for"];
    value
        .as_object()
        .map(|map| {
            map.keys()
                .filter(|key| !KNOWN.contains(&key.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Write `admin_gate` into the singleton `settings` row, creating it if
/// necessary. Uses an upsert via either insert (no row) or update (row
/// present + merge sub-key).
///
/// Runs in a transaction with the row locked FOR UPDATE on Postgres: this is a
/// read-modify-write of a document shared with `AppSettings`, so without the
/// lock a concurrent `ConfigService::update_settings` can compute its merge
/// from a snapshot that predates this write and clobber the allowlist the
/// operator just saved.
async fn persist_to_db(
    db: &DatabaseConnection,
    new_settings: &AdminGateSettings,
) -> Result<(), AdminGateServiceError> {
    let now = chrono::Utc::now();
    let new_value = serde_json::to_value(new_settings)?;
    let is_postgres = matches!(
        db.get_database_backend(),
        sea_orm::DatabaseBackend::Postgres
    );

    let txn = db.begin().await?;

    let row_query = temps_entities::settings::Entity::find_by_id(1);
    let row_query = if is_postgres {
        row_query.lock_exclusive()
    } else {
        row_query
    };
    let row = row_query.one(&txn).await?;

    match row {
        Some(existing) => {
            let mut data = existing.data.clone();
            match data.as_object_mut() {
                Some(map) => {
                    map.insert("admin_gate".to_string(), new_value);
                }
                None => {
                    // Settings row had a non-object blob — replace it with a
                    // fresh object that contains just our key. Other keys
                    // would already be lost in this case.
                    data = serde_json::json!({ "admin_gate": new_value });
                }
            }
            let mut am: temps_entities::settings::ActiveModel = existing.into();
            am.data = Set(data);
            am.updated_at = Set(now);
            am.update(&txn).await?;
        }
        None => {
            let am = temps_entities::settings::ActiveModel {
                id: Set(1),
                data: Set(serde_json::json!({ "admin_gate": new_value })),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(&txn).await?;
        }
    }

    txn.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::net::Ipv4Addr;

    fn settings_row(data: serde_json::Value) -> temps_entities::settings::Model {
        temps_entities::settings::Model {
            id: 1,
            data,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn env_active_skips_db() {
        // MockDatabase with zero queued results — if we touched the DB this
        // would panic.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let (svc, handle) =
            AdminGateService::new(Arc::new(db), &["10.0.0.0/8".to_string()], &[], false)
                .await
                .unwrap();
        assert!(svc.env_overridden());
        assert_eq!(handle.current().source, AdminGateSource::Env);
        assert_eq!(handle.current().allowed_nets.len(), 1);
    }

    #[tokio::test]
    async fn env_unset_loads_from_db_when_present() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": {
                    "allowed_ips": ["192.168.0.0/16"],
                    "allowed_hosts": ["admin.example.com"],
                    "trust_forwarded_for": true
                }
            }))]])
            .into_connection();
        let (svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();
        assert!(!svc.env_overridden());
        let cfg = handle.current();
        assert_eq!(cfg.source, AdminGateSource::Db);
        assert_eq!(cfg.allowed_nets.len(), 1);
        assert_eq!(cfg.allowed_hosts.len(), 1);
        assert!(cfg.trust_forwarded_for);
    }

    #[tokio::test]
    async fn env_unset_no_db_row_uses_default() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::settings::Model>::new()])
            .into_connection();
        let (_svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();
        let cfg = handle.current();
        assert_eq!(cfg.source, AdminGateSource::Default);
        assert!(cfg.is_noop());
    }

    #[tokio::test]
    async fn update_refused_when_env_overridden() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let (svc, _handle) =
            AdminGateService::new(Arc::new(db), &["10.0.0.0/8".to_string()], &[], false)
                .await
                .unwrap();
        let result = svc
            .update(
                AdminGateSettings::default(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                None,
            )
            .await;
        assert!(matches!(
            result.unwrap_err(),
            AdminGateServiceError::EnvOverridden
        ));
    }

    #[tokio::test]
    async fn update_refused_when_caller_would_be_locked_out() {
        // Boot with no env, no DB row → default (open) config.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::settings::Model>::new()])
            .into_connection();
        let (svc, _handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();

        // Try to lock the gate to a CIDR that doesn't include the caller.
        let result = svc
            .update(
                AdminGateSettings {
                    allowed_ips: vec!["10.0.0.0/8".to_string()],
                    ..Default::default()
                },
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)),
                Some("anywhere"),
            )
            .await;
        assert!(matches!(
            result.unwrap_err(),
            AdminGateServiceError::WouldLockOut { .. }
        ));
    }

    /// The split-topology convergence path: a write performed by the *console*
    /// process must become effective in the *proxy* process, which only sees
    /// the `settings_change` NOTIFY.
    #[tokio::test]
    async fn reload_from_db_picks_up_another_process_write() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Boot: gate empty.
            .append_query_results(vec![vec![settings_row(serde_json::json!({}))]])
            // After the console saved an allowlist.
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": {
                    "allowed_ips": [],
                    "allowed_hosts": ["app.temps.kfs.es"],
                    "trust_forwarded_for": false
                }
            }))]])
            .into_connection();

        let (svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();
        assert!(handle.current().is_noop(), "boots with an empty gate");

        svc.reload_from_db().await.unwrap();

        let cfg = handle.current();
        assert_eq!(cfg.source, AdminGateSource::Db);
        assert_eq!(cfg.allowed_hosts.len(), 1);
    }

    /// A cleared allowlist must also converge — otherwise the proxy keeps
    /// 404ing hosts the operator has since unblocked.
    ///
    /// Note the shape: clearing the gate through `update()` persists an
    /// EXPLICIT `admin_gate` document with empty lists (see `persist_to_db`),
    /// not a missing key. That distinction is the whole point of the test
    /// below — an absent key is an anomaly, not a clear.
    #[tokio::test]
    async fn reload_from_db_converges_when_gate_is_cleared() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": {
                    "allowed_ips": [],
                    "allowed_hosts": ["app.temps.kfs.es"],
                    "trust_forwarded_for": false
                }
            }))]])
            // Console cleared the lists — the key is still there, now empty.
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": {
                    "allowed_ips": [],
                    "allowed_hosts": [],
                    "trust_forwarded_for": false
                }
            }))]])
            .into_connection();

        let (svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();
        assert!(!handle.current().is_noop());

        svc.reload_from_db().await.unwrap();

        assert!(handle.current().is_noop());
        assert_eq!(handle.current().source, AdminGateSource::Db);
    }

    /// SECURITY: a *missing* `admin_gate` sub-document must never widen the
    /// gate. `update()` always writes an explicit document, so absence means
    /// the key was destroyed out of band (a writer that dropped it, a restore
    /// from a pre-gate snapshot, a DELETE on the row). Widening on that signal
    /// would turn any of those into instant privilege escalation on a live
    /// proxy — the boot path refuses to start with an open gate for the same
    /// reason.
    #[tokio::test]
    async fn reload_from_db_refuses_to_widen_when_subdocument_vanishes() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": {
                    "allowed_ips": ["10.0.0.0/8"],
                    "allowed_hosts": ["app.temps.kfs.es"],
                    "trust_forwarded_for": false
                }
            }))]])
            // The key is GONE — not cleared.
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "preview_domain": "temps.kfs.es"
            }))]])
            .into_connection();

        let (svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();

        svc.reload_from_db().await.unwrap();

        let cfg = handle.current();
        assert!(
            !cfg.is_noop(),
            "a vanished admin_gate key must NOT open the gate"
        );
        assert_eq!(cfg.allowed_hosts.len(), 1);
        assert_eq!(cfg.allowed_nets.len(), 1);
    }

    /// A malformed sub-document must fail closed too — the previous config
    /// stays installed rather than degrading to an open gate.
    #[tokio::test]
    async fn reload_from_db_keeps_previous_config_on_malformed_document() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": {
                    "allowed_ips": [],
                    "allowed_hosts": ["app.temps.kfs.es"],
                    "trust_forwarded_for": false
                }
            }))]])
            // `allowed_hosts` is the wrong type.
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": { "allowed_hosts": "not-a-list" }
            }))]])
            .into_connection();

        let (svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();

        assert!(svc.reload_from_db().await.is_err());
        assert!(
            !handle.current().is_noop(),
            "a malformed admin_gate document must not open the gate"
        );
    }

    /// A typo'd/renamed key deserializes to all-defaults — an open gate — so
    /// it must at least be *reported*. It must NOT be fatal: a newer binary's
    /// forward-compatible field would otherwise stop this one from booting.
    #[test]
    fn unknown_keys_are_reported_but_never_fatal() {
        let doc = serde_json::json!({
            "allowedHosts": ["app.temps.kfs.es"],
            "trust_forwarded_for": false
        });

        assert_eq!(
            unknown_admin_gate_keys(&doc),
            vec!["allowedHosts".to_string()]
        );

        let parsed: Result<AdminGateSettings, _> = serde_json::from_value(doc);
        assert!(
            parsed.is_ok(),
            "an unknown key must not fail the parse — that would turn a newer peer's \
             field into a boot failure"
        );
    }

    #[test]
    fn known_keys_are_not_reported_as_unknown() {
        let doc = serde_json::json!({
            "allowed_ips": ["10.0.0.0/8"],
            "allowed_hosts": ["app.temps.kfs.es"],
            "trust_forwarded_for": true
        });
        assert!(unknown_admin_gate_keys(&doc).is_empty());
    }

    /// The documented fail-safe: a DB error during reload must retain the
    /// currently active config rather than degrading to an open gate.
    #[tokio::test]
    async fn reload_from_db_keeps_previous_config_on_db_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row(serde_json::json!({
                "admin_gate": {
                    "allowed_ips": ["10.0.0.0/8"],
                    "allowed_hosts": ["app.temps.kfs.es"],
                    "trust_forwarded_for": false
                }
            }))]])
            .append_query_errors(vec![sea_orm::DbErr::Custom(
                "connection reset by peer".to_string(),
            )])
            .into_connection();

        let (svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();

        assert!(svc.reload_from_db().await.is_err());

        let cfg = handle.current();
        assert!(!cfg.is_noop(), "a DB error must not open the gate");
        assert_eq!(cfg.allowed_hosts.len(), 1);
        assert_eq!(cfg.allowed_nets.len(), 1);
    }

    /// Env precedence holds on the reload path too: a DB write must not be
    /// able to override an env-pinned gate.
    #[tokio::test]
    async fn reload_from_db_is_noop_when_env_overridden() {
        // Zero queued results — touching the DB would panic.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let (svc, handle) =
            AdminGateService::new(Arc::new(db), &["10.0.0.0/8".to_string()], &[], false)
                .await
                .unwrap();

        svc.reload_from_db().await.unwrap();

        assert_eq!(handle.current().source, AdminGateSource::Env);
        assert_eq!(handle.current().allowed_nets.len(), 1);
    }

    #[tokio::test]
    async fn update_allows_when_caller_in_new_range() {
        // Boot finds an existing (empty) settings row → the persist path
        // takes the UPDATE branch. Sea-ORM's `ActiveModel::update()` issues
        // `UPDATE ... RETURNING *` on PostgreSQL, so the mock needs the
        // returning row queued as a query result, not an exec result.
        let bootstrap_row = settings_row(serde_json::json!({}));
        let returned_row = settings_row(serde_json::json!({
            "admin_gate": {
                "allowed_ips": ["10.0.0.0/8"],
                "allowed_hosts": ["admin.example.com"],
                "trust_forwarded_for": false
            }
        }));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Initial load: empty admin_gate key.
            .append_query_results(vec![vec![bootstrap_row.clone()]])
            // persist_to_db re-reads the row.
            .append_query_results(vec![vec![bootstrap_row.clone()]])
            // UPDATE ... RETURNING * — returns the new row.
            .append_query_results(vec![vec![returned_row]])
            .into_connection();
        let (svc, handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();

        let caller = IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3));
        svc.update(
            AdminGateSettings {
                allowed_ips: vec!["10.0.0.0/8".to_string()],
                allowed_hosts: vec!["admin.example.com".to_string()],
                trust_forwarded_for: false,
            },
            caller,
            Some("admin.example.com"),
        )
        .await
        .unwrap();

        let cfg = handle.current();
        assert_eq!(cfg.source, AdminGateSource::Db);
        assert_eq!(cfg.allowed_nets.len(), 1);
        assert_eq!(cfg.allowed_hosts.len(), 1);
    }
}
