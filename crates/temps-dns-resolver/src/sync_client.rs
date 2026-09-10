// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Long-poll sync loop against the control plane's
//! `GET /api/internal/nodes/{id}/dns/changes` and `POST .../dns/ack`
//! endpoints.
//!
//! Runs as a Tokio task. Owns the control-plane HTTP client and writes to
//! a shared [`ZoneStore`].
//!
//! ## Flow per tick
//!
//! 1. `GET /api/internal/nodes/{id}/dns/changes?since={zone.generation()}`.
//! 2. If `full_snapshot: true`, call `ZoneStore::replace`. Else
//!    `ZoneStore::apply_diff` with `records` (upserts) and `removed_ids`.
//! 3. `POST /api/internal/nodes/{id}/dns/ack { applied_generation: response.generation }`.
//! 4. Sleep `poll_interval`. On any HTTP error, exponential backoff up to
//!    `max_backoff`. The zone keeps serving the last successful state.

use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::config::ResolverConfig;
use crate::error::ResolverError;
use crate::record::ZoneRecord;
use crate::zone_store::ZoneStore;

#[derive(Debug, Clone, Deserialize)]
struct ChangesResponse {
    generation: i64,
    full_snapshot: bool,
    records: Vec<ZoneRecord>,
    #[serde(default)]
    removed_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
struct AckBody {
    applied_generation: i64,
}

/// Cap for `SyncBadResponse::reason`. Enough to see a Problem+JSON detail or
/// a short plaintext error; not enough for a proxy's full HTML error page to
/// bloat the `nodes.dns_resolver_last_error` column it eventually lands in.
const SYNC_ERROR_REASON_MAX_BYTES: usize = 512;

/// Truncate to a char boundary at or before `SYNC_ERROR_REASON_MAX_BYTES`,
/// tagging the result so a reader knows text was cut rather than complete.
fn truncate_reason(reason: &str) -> String {
    if reason.len() <= SYNC_ERROR_REASON_MAX_BYTES {
        return reason.to_string();
    }
    let mut end = SYNC_ERROR_REASON_MAX_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... (truncated)", &reason[..end])
}

/// Point-in-time health of the sync loop, read by `ResolverHandle::status()`
/// for the agent heartbeat and any local troubleshooting surface. Cloned out
/// of a shared lock rather than computed on demand, so a stuck loop (e.g.
/// blocked on a slow control-plane response) still reports its last known
/// good state instead of blocking the reader.
#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    /// When the last sync tick completed without error. `None` before the
    /// first successful tick — this is the "never synced" state, distinct
    /// from "synced a while ago", which callers should treat as unhealthy.
    pub last_success_at: Option<SystemTime>,
    /// Resets to 0 on every successful tick. A node reporting a large,
    /// growing count here has lost contact with the control plane's DNS
    /// change feed and is serving an increasingly stale zone.
    pub consecutive_failures: u32,
    /// The most recent tick error, kept even after a subsequent success so
    /// "it failed once and recovered" stays visible for one heartbeat cycle
    /// rather than disappearing the instant it clears.
    pub last_error: Option<String>,
}

pub struct SyncClient {
    config: ResolverConfig,
    zone: Arc<ZoneStore>,
    http: reqwest::Client,
    /// Notified when shutdown is requested. The loop checks this between
    /// poll cycles and during backoff sleeps.
    shutdown: Arc<Notify>,
    status: Arc<RwLock<SyncStatus>>,
}

impl SyncClient {
    pub fn new(
        config: ResolverConfig,
        zone: Arc<ZoneStore>,
        shutdown: Arc<Notify>,
        status: Arc<RwLock<SyncStatus>>,
    ) -> Result<Self, ResolverError> {
        let http = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .build()
            .map_err(|e| ResolverError::Internal(format!("build reqwest client: {e}")))?;
        Ok(Self {
            config,
            zone,
            http,
            shutdown,
            status,
        })
    }

    /// Run the sync loop forever (until `shutdown` is notified). Errors are
    /// logged and trigger backoff — they never bubble out, because the only
    /// thing the agent can do with them is "retry", and we already do.
    pub async fn run(self) {
        let mut backoff = self.config.initial_backoff;
        loop {
            match self.tick_once().await {
                Ok(()) => {
                    backoff = self.config.initial_backoff;
                    if let Ok(mut s) = self.status.write() {
                        s.last_success_at = Some(SystemTime::now());
                        s.consecutive_failures = 0;
                    }
                    if select_sleep(self.config.poll_interval, &self.shutdown).await {
                        info!(node_id = self.config.node_id, "DNS sync loop shutting down");
                        return;
                    }
                }
                Err(e) => {
                    warn!(
                        node_id = self.config.node_id,
                        error = %e,
                        backoff_ms = backoff.as_millis(),
                        "DNS sync tick failed; backing off"
                    );
                    if let Ok(mut s) = self.status.write() {
                        s.consecutive_failures += 1;
                        s.last_error = Some(e.to_string());
                    }
                    if select_sleep(backoff, &self.shutdown).await {
                        info!(
                            node_id = self.config.node_id,
                            "DNS sync loop shutting down (during backoff)"
                        );
                        return;
                    }
                    backoff = std::cmp::min(backoff * 2, self.config.max_backoff);
                }
            }
        }
    }

    /// One full sync round. Public for unit tests; production code only
    /// calls [`Self::run`].
    pub async fn tick_once(&self) -> Result<(), ResolverError> {
        let since = self.zone.generation();
        // Mounted under /api by the plugin runtime — same as every other
        // agent → control-plane RPC (heartbeat, peers, etc.).
        let url = format!(
            "{}/api/internal/nodes/{}/dns/changes?since={}",
            self.config.control_plane_url.trim_end_matches('/'),
            self.config.node_id,
            since
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.config.node_token)
            .send()
            .await
            .map_err(|e| ResolverError::SyncHttp {
                node_id: self.config.node_id,
                reason: format!("changes GET: {e}"),
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ResolverError::SyncBadResponse {
                node_id: self.config.node_id,
                status: status.as_u16(),
                // Truncated: a non-2xx body can be an arbitrarily large
                // intervening proxy/WAF error page rather than a small
                // Problem+JSON detail, and this reason round-trips through
                // the agent heartbeat into the `nodes.dns_resolver_last_error`
                // column and back out through `GET /cluster/dns/status` — an
                // unbounded body here would bloat that row and every reader
                // of it, not just this log line.
                reason: truncate_reason(&resp.text().await.unwrap_or_default()),
            });
        }
        let body: ChangesResponse = resp.json().await.map_err(|e| ResolverError::SyncHttp {
            node_id: self.config.node_id,
            reason: format!("changes JSON parse: {e}"),
        })?;

        let server_generation = body.generation;
        if body.full_snapshot {
            debug!(
                node_id = self.config.node_id,
                generation = server_generation,
                count = body.records.len(),
                "Applying full DNS snapshot"
            );
            self.zone.replace(server_generation, body.records);
        } else if server_generation > since {
            debug!(
                node_id = self.config.node_id,
                from = since,
                to = server_generation,
                upserts = body.records.len(),
                removes = body.removed_ids.len(),
                "Applying DNS diff"
            );
            self.zone
                .apply_diff(server_generation, body.records, &body.removed_ids);
        } else {
            // Up to date.
            return Ok(());
        }

        // ACK back the new generation.
        let ack_url = format!(
            "{}/api/internal/nodes/{}/dns/ack",
            self.config.control_plane_url.trim_end_matches('/'),
            self.config.node_id,
        );
        let ack_resp = self
            .http
            .post(&ack_url)
            .bearer_auth(&self.config.node_token)
            .json(&AckBody {
                applied_generation: server_generation,
            })
            .send()
            .await
            .map_err(|e| ResolverError::SyncHttp {
                node_id: self.config.node_id,
                reason: format!("ack POST: {e}"),
            })?;
        if !ack_resp.status().is_success() {
            // ACK failure is non-fatal — the next changes call will re-ACK.
            warn!(
                node_id = self.config.node_id,
                status = %ack_resp.status(),
                "ACK rejected; will retry on next tick"
            );
        }
        Ok(())
    }
}

/// Sleep for `dur` or until `shutdown` fires. Returns `true` if shutdown
/// was triggered (caller should exit), `false` if the timer expired
/// normally.
async fn select_sleep(dur: Duration, shutdown: &Notify) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(dur) => false,
        _ = shutdown.notified() => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::ZoneRecord;
    use std::path::PathBuf;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_record(id: i64, fqdn: &str, ip: &str, generation: i64) -> ZoneRecord {
        ZoneRecord {
            id,
            fqdn: fqdn.into(),
            record_type: "A".into(),
            target_ip: Some(ip.into()),
            target_port: Some(5432),
            ttl: 30,
            owner_kind: "service_member".into(),
            owner_id: id,
            node_id: None,
            generation,
        }
    }

    fn config_for(server_url: &str) -> ResolverConfig {
        ResolverConfig {
            node_id: 1,
            node_token: "tok".into(),
            control_plane_url: server_url.to_string(),
            listen_addrs: vec![],
            snapshot_dir: std::env::temp_dir().join("temps-dns-resolver-test"),
            poll_interval: Duration::from_millis(50),
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(100),
            http_timeout: Duration::from_secs(2),
            upstream_resolvers: vec![],
            disable_sync: false,
        }
    }

    #[tokio::test]
    async fn tick_applies_full_snapshot_then_acks() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/internal/nodes/1/dns/changes"))
            .and(query_param("since", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "generation": 5,
                "full_snapshot": true,
                "records": [make_record(1, "a.temps.local", "172.20.5.10", 5)],
                "removed_ids": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/internal/nodes/1/dns/ack"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": 1,
                "applied_generation": 5,
                "server_generation": 5
            })))
            .expect(1)
            .mount(&server)
            .await;

        let zone = Arc::new(ZoneStore::new(PathBuf::from("/dev/null")));
        let shutdown = Arc::new(Notify::new());
        let client = SyncClient::new(
            config_for(&server.uri()),
            zone.clone(),
            shutdown,
            Arc::new(RwLock::new(SyncStatus::default())),
        )
        .unwrap();

        client.tick_once().await.expect("tick succeeds");

        assert_eq!(zone.generation(), 5);
        assert_eq!(zone.snapshot().records().len(), 1);
    }

    #[tokio::test]
    async fn tick_applies_diff() {
        let server = MockServer::start().await;
        let zone = Arc::new(ZoneStore::new(
            std::env::temp_dir().join("zone-diff-test.json"),
        ));
        zone.replace(3, vec![make_record(1, "a.temps.local", "1.1.1.1", 3)]);

        Mock::given(method("GET"))
            .and(path("/api/internal/nodes/1/dns/changes"))
            .and(query_param("since", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "generation": 7,
                "full_snapshot": false,
                "records": [
                    make_record(1, "a.temps.local", "1.1.1.99", 7),
                    make_record(2, "b.temps.local", "2.2.2.2", 7)
                ],
                "removed_ids": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/internal/nodes/1/dns/ack"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": 1, "applied_generation": 7, "server_generation": 7
            })))
            .mount(&server)
            .await;

        let shutdown = Arc::new(Notify::new());
        let client = SyncClient::new(
            config_for(&server.uri()),
            zone.clone(),
            shutdown,
            Arc::new(RwLock::new(SyncStatus::default())),
        )
        .unwrap();
        client.tick_once().await.expect("tick succeeds");

        assert_eq!(zone.generation(), 7);
        let snap = zone.snapshot();
        let by_id: std::collections::HashMap<i64, &ZoneRecord> =
            snap.records().iter().map(|r| (r.id, r)).collect();
        assert_eq!(by_id[&1].target_ip.as_deref(), Some("1.1.1.99"));
        assert!(by_id.contains_key(&2));
    }

    #[tokio::test]
    async fn tick_no_change_is_noop() {
        let server = MockServer::start().await;
        let zone = Arc::new(ZoneStore::new(
            std::env::temp_dir().join("zone-noop-test.json"),
        ));
        zone.replace(9, vec![make_record(1, "a.temps.local", "1.1.1.1", 9)]);

        Mock::given(method("GET"))
            .and(path("/api/internal/nodes/1/dns/changes"))
            .and(query_param("since", "9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "generation": 9,
                "full_snapshot": false,
                "records": [],
                "removed_ids": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Ack endpoint should NOT be called when there's nothing new.
        Mock::given(method("POST"))
            .and(path("/api/internal/nodes/1/dns/ack"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let shutdown = Arc::new(Notify::new());
        let client = SyncClient::new(
            config_for(&server.uri()),
            zone.clone(),
            shutdown,
            Arc::new(RwLock::new(SyncStatus::default())),
        )
        .unwrap();
        client.tick_once().await.expect("tick succeeds");

        assert_eq!(zone.generation(), 9);
    }

    #[tokio::test]
    async fn run_tracks_failure_then_recovers_in_status() {
        let server = MockServer::start().await;

        // First tick fails (503); second and later ticks succeed. Wiremock
        // matches registration order and `up_to_n_times` exhausts a mock
        // before falling through to the next one for the same route.
        Mock::given(method("GET"))
            .and(path("/api/internal/nodes/1/dns/changes"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/internal/nodes/1/dns/changes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "generation": 1,
                "full_snapshot": true,
                "records": [],
                "removed_ids": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/internal/nodes/1/dns/ack"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": 1, "applied_generation": 1, "server_generation": 1
            })))
            .mount(&server)
            .await;

        let zone = Arc::new(ZoneStore::new(
            std::env::temp_dir().join("zone-status-test.json"),
        ));
        let shutdown = Arc::new(Notify::new());
        let status = Arc::new(RwLock::new(SyncStatus::default()));
        let client = SyncClient::new(
            config_for(&server.uri()),
            zone,
            shutdown.clone(),
            status.clone(),
        )
        .unwrap();

        let handle = tokio::spawn(client.run());

        // Poll until the second (successful) tick has landed, rather than a
        // fixed sleep — avoids flaking under slow CI schedulers.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if status.read().unwrap().last_success_at.is_some() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "sync loop never recorded a successful tick"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        shutdown.notify_waiters();
        handle.await.unwrap();

        let final_status = status.read().unwrap().clone();
        assert!(final_status.last_success_at.is_some());
        assert_eq!(
            final_status.consecutive_failures, 0,
            "a later success must reset the failure counter"
        );
    }

    #[tokio::test]
    async fn tick_returns_error_on_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/internal/nodes/1/dns/changes"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let zone = Arc::new(ZoneStore::new(
            std::env::temp_dir().join("zone-503-test.json"),
        ));
        let shutdown = Arc::new(Notify::new());
        let client = SyncClient::new(
            config_for(&server.uri()),
            zone.clone(),
            shutdown,
            Arc::new(RwLock::new(SyncStatus::default())),
        )
        .unwrap();
        let err = client.tick_once().await.unwrap_err();
        assert!(matches!(
            err,
            ResolverError::SyncBadResponse { status: 503, .. }
        ));
        // Zone unchanged.
        assert_eq!(zone.generation(), 0);
    }
}
