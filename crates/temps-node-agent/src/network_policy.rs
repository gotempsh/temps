// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The worker's network policy enforcer (network access plan, §4.15).
//!
//! The same [`NodeEnforcer`] node 0 runs, fed from the control plane
//! instead of the database: every pass posts a [`NodePolicyReport`] to
//! `/api/internal/nodes/{id}/network-policy` with the node token and gets
//! the node's mode and snapshot back. The report tells the control plane
//! which datapath this node enforces with, which is what allows an
//! operator to move the node out of `legacy`, and which workloads it
//! observed on its previous pass (in every mode), which is how every other
//! node learns their addresses. In `legacy` the pass only observes the
//! local workloads and publishes the egress-proxy table: no rule applied,
//! no program loaded. The digest of the snapshot it holds rides along, so
//! a pass in which nothing changed brings back no snapshot.
//!
//! A failed fetch fails the pass, and the kernel keeps what the last pass
//! applied: a control plane outage never opens or closes anything.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bollard::Docker;
use temps_workload_netpol::{
    available_datapath, choose_executor, choose_port_resolver, spawn_container_wake,
    DatapathConfig, DockerInstanceSource, EgressRecordingCapability, EgressTable, EnforcerError,
    IdentityMap, NodeEnforcer, NodeMode, NodePolicy, NodePolicyReport, PassOutcome, PolicySnapshot,
    PolicySource, TableName, POLICY_PROTOCOL,
};
use tracing::{info, warn};

use crate::netpol_guard::BootGuard;

/// Whether this host has a boot guard: on Linux a systemd unit loads it
/// before Docker. A Mac worker (ADR 090) has no such unit and no `nft` on the
/// Mac itself, so its agent neither writes nor lifts one.
fn boot_guard_supported() -> bool {
    cfg!(target_os = "linux")
}

/// File name, in the agent data directory, of the last policy the control
/// plane sent.
pub const POLICY_CACHE_FILE: &str = "netpol-last-policy.json";

/// How often the worker re-reads its mode and policy.
pub const PASS_INTERVAL: Duration = Duration::from_secs(15);

/// Reads this node's mode and snapshot from the control plane. `mode`
/// fetches both, `snapshot` returns what that fetch brought, so a pass is
/// one request. The request carries what `observed` recorded on the
/// previous pass, so the control plane's view of this node trails it by
/// one pass.
pub struct AgentPolicySource {
    http: reqwest::Client,
    url: String,
    node: String,
    token: String,
    backend: &'static str,
    last: Mutex<Option<PolicySnapshot>>,
    /// The control plane's digest of `last`, sent back so an unchanged
    /// pass costs no snapshot.
    last_digest: Mutex<Option<String>>,
    observed: Mutex<IdentityMap>,
    /// Where the last policy received is kept, so an agent that boots while
    /// the control plane is unreachable enforces it instead of nothing.
    cache: Option<PathBuf>,
    /// The last policy written to `cache`, to skip identical writes.
    cached: Mutex<Option<String>>,
    guard: Option<BootGuard>,
    /// What this node says about recording outbound connections, on every
    /// report. See [`egress_recording_capability`].
    egress_recording: EgressRecordingCapability,
}

/// Whether this agent can record the outbound connections of sandboxes that
/// opt in. Records are taken by the per-node egress proxy, which this agent
/// does not serve yet: sandboxes still egress through their own sidecar
/// (ADR-055 phase 7 moves them onto the per-node proxy). So a Linux agent
/// reports [`RecordingUnavailable::ProxyNotServing`] and any other platform
/// [`RecordingUnavailable::UnsupportedPlatform`], and the control plane
/// refuses to turn recording on for its sandboxes, saying why. When the
/// agent serves the proxy with a recorder, this becomes `detect(true)`.
///
/// [`RecordingUnavailable::ProxyNotServing`]: temps_workload_netpol::RecordingUnavailable::ProxyNotServing
/// [`RecordingUnavailable::UnsupportedPlatform`]: temps_workload_netpol::RecordingUnavailable::UnsupportedPlatform
pub fn egress_recording_capability() -> EgressRecordingCapability {
    EgressRecordingCapability::detect(false)
}

impl AgentPolicySource {
    pub fn new(
        http: reqwest::Client,
        control_plane_url: &str,
        node_id: i32,
        token: String,
        backend: &'static str,
    ) -> Self {
        Self {
            http,
            url: policy_url(control_plane_url, node_id),
            node: format!("node-{node_id}"),
            token,
            backend,
            last: Mutex::new(None),
            last_digest: Mutex::new(None),
            observed: Mutex::new(IdentityMap::new()),
            cache: None,
            cached: Mutex::new(None),
            guard: None,
            egress_recording: egress_recording_capability(),
        }
    }

    /// Keep the last policy in `path`, and use it when the control plane
    /// cannot be reached before this process got its first answer.
    pub fn with_cache(mut self, path: PathBuf) -> Self {
        self.cache = Some(path);
        self
    }

    pub fn with_boot_guard(mut self, guard: BootGuard) -> Self {
        self.guard = Some(guard);
        self
    }

    fn store_cache(&self, policy: &NodePolicy) {
        let Some(path) = &self.cache else { return };
        let encoded = match serde_json::to_string(policy) {
            Ok(encoded) => encoded,
            Err(error) => {
                warn!(node = %self.node, %error, "could not encode the network policy for its cache");
                return;
            }
        };
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if cached.as_deref() == Some(encoded.as_str()) {
            return;
        }
        match write_private(path, &encoded) {
            Ok(()) => *cached = Some(encoded),
            Err(error) => warn!(
                node = %self.node,
                path = %path.display(),
                %error,
                "could not cache the network policy; a reboot during a control plane outage \
                 would leave this node without it"
            ),
        }
    }

    /// The cached policy, only before this process received one: after
    /// that, a failed fetch keeps what the kernel holds, as before.
    fn cached_policy(&self) -> Option<NodePolicy> {
        if self
            .last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return None;
        }
        let path = self.cache.as_ref()?;
        let data = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str(&data) {
            Ok(policy) => Some(policy),
            Err(error) => {
                warn!(node = %self.node, path = %path.display(), %error, "ignoring an unreadable network policy cache");
                None
            }
        }
    }

    fn source_error(&self, reason: String) -> EnforcerError {
        EnforcerError::Source {
            node: self.node.clone(),
            reason,
        }
    }
}

/// The policy endpoint of `node_id` on the control plane.
pub fn policy_url(control_plane_url: &str, node_id: i32) -> String {
    format!(
        "{}/api/internal/nodes/{node_id}/network-policy",
        control_plane_url.trim_end_matches('/')
    )
}

#[async_trait]
impl PolicySource for AgentPolicySource {
    async fn mode(&self) -> Result<NodeMode, EnforcerError> {
        let held_snapshot = {
            let last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            let digest = self.last_digest.lock().unwrap_or_else(|e| e.into_inner());
            last.as_ref().and(digest.clone())
        };
        let report = NodePolicyReport {
            backend: self.backend.to_string(),
            identities: self
                .observed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            egress_recording: Some(self.egress_recording.clone()),
            protocol: POLICY_PROTOCOL,
            held_snapshot,
        };
        let policy = match self.fetch(&report).await {
            Ok(policy) => {
                let policy = self.resolve_unchanged(policy);
                self.store_cache(&policy);
                policy
            }
            Err(error) => match self.cached_policy() {
                Some(cached) => {
                    warn!(
                        node = %self.node,
                        %error,
                        mode = ?cached.mode,
                        "control plane unreachable before this agent's first policy; enforcing \
                         the last policy it sent until it answers"
                    );
                    cached
                }
                None => return Err(error),
            },
        };
        // Observations are reported in every mode: knowing its workloads
        // does not depend on whether the node filters.
        *self.last_digest.lock().unwrap_or_else(|e| e.into_inner()) = policy.snapshot_digest;
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        *last = policy.snapshot;
        Ok(policy.mode)
    }

    async fn legacy_snapshot(&self) -> Result<Option<PolicySnapshot>, EnforcerError> {
        Ok(self.last.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    async fn snapshot(&self) -> Result<PolicySnapshot, EnforcerError> {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        last.clone().ok_or_else(|| {
            self.source_error(
                "the control plane sent a mode outside legacy without a snapshot".to_string(),
            )
        })
    }

    fn observed(&self, local: &IdentityMap) {
        *self.observed.lock().unwrap_or_else(|e| e.into_inner()) = local.clone();
    }

    async fn applied(&self, outcome: &PassOutcome) {
        if let Some(guard) = &self.guard {
            guard.after_pass(outcome).await;
        }
    }
}

impl AgentPolicySource {
    /// An `unchanged` answer means "keep the snapshot you hold": it becomes
    /// that snapshot, so the cache and the pass see a complete policy.
    fn resolve_unchanged(&self, mut policy: NodePolicy) -> NodePolicy {
        if policy.unchanged {
            policy.snapshot = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if policy.snapshot.is_none() {
                // Cannot happen (the digest is only sent while a snapshot
                // is held); the next pass asks again with no digest.
                policy.snapshot_digest = None;
            }
            policy.unchanged = false;
        }
        policy
    }

    async fn fetch(&self, report: &NodePolicyReport) -> Result<NodePolicy, EnforcerError> {
        let response = self
            .http
            .post(&self.url)
            .json(report)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| self.source_error(format!("POST {}: {e}", self.url)))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(self.source_error(format!(
                "POST {} returned {status}: {}",
                self.url,
                body.chars().take(300).collect::<String>()
            )));
        }
        response
            .json()
            .await
            .map_err(|e| self.source_error(format!("decoding the policy from {}: {e}", self.url)))
    }
}

fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let temporary = path.with_extension("json.tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents.as_bytes())?;
    }
    std::fs::rename(&temporary, path)
}

/// Starts this worker's enforcer on the local Docker daemon.
pub fn spawn_worker_enforcer(
    http: reqwest::Client,
    control_plane_url: &str,
    node_id: i32,
    token: String,
    docker: Docker,
    dns: temps_node_dns::DnsPolicySlot,
    data_dir: &std::path::Path,
) -> Result<tokio::task::JoinHandle<()>, EnforcerError> {
    let node = format!("node-{node_id}");
    let table = TableName::new(format!("temps_netpol_node{node_id}")).map_err(|source| {
        EnforcerError::Compile {
            node: node.clone(),
            source,
        }
    })?;
    // The agent always runs next to its daemon.
    let daemon_is_local = true;
    let config = DatapathConfig::default();
    let datapath = available_datapath(config.clone());
    let backend = if datapath.is_ok() { "ebpf" } else { "nftables" };
    let enforcer = NodeEnforcer::new(
        node.clone(),
        table,
        Arc::from(choose_executor(docker.clone(), daemon_is_local)),
        Arc::from(choose_port_resolver(docker.clone(), daemon_is_local)),
        Arc::new(DockerInstanceSource::new(node.clone(), docker.clone())),
        Arc::new({
            let source = AgentPolicySource::new(http, control_plane_url, node_id, token, backend)
                .with_cache(data_dir.join(POLICY_CACHE_FILE));
            if boot_guard_supported() {
                source.with_boot_guard(BootGuard::new(data_dir, docker.clone()))
            } else {
                source
            }
        }),
        dns,
        Arc::new(ArcSwap::from_pointee(EgressTable::default())),
    );
    let enforcer = match datapath {
        Ok(opener) => enforcer
            .with_datapath(opener, &config)
            .with_wake(spawn_container_wake(node.clone(), docker)),
        Err(reason) => {
            info!(node = %node, %reason, "network policy backend: nftables");
            enforcer
        }
    };
    info!(
        node = %node,
        backend,
        interval_secs = PASS_INTERVAL.as_secs(),
        "worker network policy enforcer started (mode is read from the control plane every pass)"
    );
    Ok(tokio::spawn(enforcer.run(PASS_INTERVAL)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_linux_keeps_a_boot_guard() {
        assert_eq!(boot_guard_supported(), cfg!(target_os = "linux"));
        #[cfg(target_os = "macos")]
        assert!(
            !boot_guard_supported(),
            "a Mac worker has no boot-time loader"
        );
    }

    #[test]
    fn the_policy_url_never_doubles_slashes() {
        assert_eq!(
            policy_url("https://cp.example:8081/", 4),
            "https://cp.example:8081/api/internal/nodes/4/network-policy"
        );
        assert_eq!(
            policy_url("https://cp.example:8081", 4),
            "https://cp.example:8081/api/internal/nodes/4/network-policy"
        );
    }

    #[test]
    fn this_agent_reports_that_it_cannot_record_yet_and_why() {
        let capability = egress_recording_capability();
        assert!(!capability.is_available());
        #[cfg(target_os = "linux")]
        assert_eq!(
            capability,
            EgressRecordingCapability::Unavailable {
                reason: temps_workload_netpol::RecordingUnavailable::ProxyNotServing
            }
        );
        #[cfg(not(target_os = "linux"))]
        assert!(matches!(
            capability,
            EgressRecordingCapability::Unavailable {
                reason: temps_workload_netpol::RecordingUnavailable::UnsupportedPlatform { .. }
            }
        ));
    }

    /// A scripted control plane: answers each POST with the next of
    /// `answers` and returns the request bodies it received.
    async fn scripted_control_plane(
        answers: Vec<String>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for answer in answers {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 8192];
                let body = loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break body.to_string();
                        }
                    }
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                            answer.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                bodies.push(body);
            }
            bodies
        });
        (addr, server)
    }

    fn source(addr: std::net::SocketAddr) -> AgentPolicySource {
        AgentPolicySource::new(
            reqwest::Client::new(),
            &format!("http://{addr}"),
            4,
            "token".into(),
            "nftables",
        )
    }

    /// A new agent asks with its protocol, holds the snapshot it gets in
    /// `legacy`, sends its digest back, and keeps it on `unchanged`.
    #[tokio::test]
    async fn a_legacy_snapshot_is_held_and_an_unchanged_answer_keeps_it() {
        let mut snapshot = PolicySnapshot::default();
        snapshot
            .system_ingress
            .push("10.201.0.0/16".parse().unwrap());
        let full = serde_json::to_string(&NodePolicy {
            mode: NodeMode::Legacy,
            snapshot: Some(snapshot.clone()),
            snapshot_digest: Some("d1".into()),
            unchanged: false,
        })
        .unwrap();
        let unchanged = r#"{"mode":"legacy","snapshot_digest":"d1","unchanged":true}"#.to_string();
        let (addr, server) = scripted_control_plane(vec![full, unchanged]).await;
        let source = source(addr);
        assert_eq!(source.mode().await.unwrap(), NodeMode::Legacy);
        assert_eq!(
            source
                .legacy_snapshot()
                .await
                .unwrap()
                .unwrap()
                .system_ingress,
            snapshot.system_ingress
        );
        source.observed(&IdentityMap::new().with(
            temps_workload::WorkloadRef::new(temps_workload::WorkloadKind::Sandbox, 1),
            "10.237.1.2".parse().unwrap(),
        ));
        assert_eq!(source.mode().await.unwrap(), NodeMode::Legacy);
        assert_eq!(
            source
                .legacy_snapshot()
                .await
                .unwrap()
                .unwrap()
                .system_ingress,
            snapshot.system_ingress,
            "unchanged keeps the held snapshot"
        );
        let bodies = server.await.unwrap();
        let first: NodePolicyReport = serde_json::from_str(&bodies[0]).unwrap();
        let second: NodePolicyReport = serde_json::from_str(&bodies[1]).unwrap();
        assert_eq!(first.protocol, POLICY_PROTOCOL);
        assert_eq!(first.held_snapshot, None, "nothing held yet");
        assert_eq!(second.held_snapshot.as_deref(), Some("d1"));
        assert_eq!(
            second.identities.workloads().count(),
            1,
            "observations are reported in legacy"
        );
    }

    /// An older control plane answers `legacy` with the mode alone: the
    /// agent then has no snapshot, so its pass observes nothing, as today.
    #[tokio::test]
    async fn an_old_control_planes_legacy_answer_leaves_nothing_to_observe() {
        let (addr, server) = scripted_control_plane(vec![r#"{"mode":"legacy"}"#.into()]).await;
        let source = source(addr);
        assert_eq!(source.mode().await.unwrap(), NodeMode::Legacy);
        assert!(source.legacy_snapshot().await.unwrap().is_none());
        server.await.unwrap();
    }

    /// Every pass's report carries the capability, so the control plane can
    /// answer for this node.
    #[tokio::test]
    async fn the_policy_report_carries_the_recording_capability() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            let answer = r#"{"mode":"legacy"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let source = AgentPolicySource::new(
            reqwest::Client::new(),
            &format!("http://{addr}"),
            4,
            "token".into(),
            "nftables",
        );
        assert_eq!(source.mode().await.unwrap(), NodeMode::Legacy);
        let request = server.await.unwrap();
        let body = request.split_once("\r\n\r\n").unwrap().1;
        let report: NodePolicyReport = serde_json::from_str(body).unwrap();
        assert_eq!(report.egress_recording, Some(egress_recording_capability()));
    }
}
