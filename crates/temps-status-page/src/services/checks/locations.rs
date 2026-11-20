// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Checking a target from more than one place (ADR 108 D5).
//!
//! A monitor with `probe_from_nodes` is checked from the control plane and
//! from up to [`MAX_NODE_LOCATIONS`] online worker nodes at once, each
//! running the same probe ([`temps_uptime_probe`]) through its own network.
//! The verdict is the majority's ([`combine`]): one location that cannot
//! reach the target makes a check neither down nor degraded, which removes
//! most false alarms. A location that does not answer is listed and not
//! counted.
//!
//! Each node gets the check's decrypted headers and body only inside a body
//! sealed to that node's key; a check with headers or a body is not sent to a
//! node that cannot receive it sealed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use temps_config::node_client::{build_node_http_client, node_agent_endpoint, ClusterCaError};
use temps_config::ConfigService;
use temps_core::sealed_request::{NodeSealing, SealRequestError, SEALED_REQUEST_CONTENT_TYPE};
use temps_core::{EncryptionError, EncryptionService, UtcDateTime};
use temps_entities::nodes;
use temps_uptime_probe::{ProbeOutcome, ProbeStatus, RemoteProbeRequest, REMOTE_PROBE_PATH};
use thiserror::Error;
use tracing::debug;
use utoipa::ToSchema;

/// Worker nodes a check runs from, besides the control plane.
pub const MAX_NODE_LOCATIONS: usize = 4;
/// The location name of the control plane itself.
pub const CONTROL_PLANE_LOCATION: &str = "control plane";
/// A node whose last heartbeat is older is not asked.
const NODE_FRESH_FOR_SECS: i64 = 180;
/// How much longer than the check's own timeout a node may take to answer.
const NODE_GRACE: Duration = Duration::from_secs(5);
/// A node client is rebuilt after this long, or on its first failure.
const CLIENT_MAX_AGE: Duration = Duration::from_secs(600);
/// Largest agent answer read; an outcome is a few hundred bytes.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// Longest message kept from one location.
const MAX_MESSAGE_CHARS: usize = 300;
/// Dissenting locations named in a check's message.
const MAX_NAMED_LOCATIONS: usize = 3;

/// What one location reported on a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MonitorLocationResult {
    /// `control plane`, or the worker node's name.
    pub location: String,
    /// The worker node; `null` for the control plane.
    pub node_id: Option<i32>,
    /// The node's `region` label, when it has one.
    pub region: Option<String>,
    /// `null` when the location did not answer: it is then not counted.
    pub status: Option<ProbeStatus>,
    pub response_time_ms: Option<i32>,
    /// Why it is not operational, or why the location did not answer.
    pub message: Option<String>,
}

/// Every location's answer to the latest check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MonitorLocationResults {
    #[schema(value_type = String, format = "date-time")]
    pub checked_at: UtcDateTime,
    pub results: Vec<MonitorLocationResult>,
}

/// A worker node a check can run from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLocation {
    pub node_id: i32,
    pub name: String,
    pub region: Option<String>,
}

/// Why a worker node gave no answer to a check. The messages read after the
/// node's name in the location list ("worker-a: no answer: could not reach
/// its agent"); the fields say which node and what failed.
#[derive(Debug, Error)]
pub enum NodeProbeError {
    #[error("node {node} is not a probe location")]
    NotALocation { node: String },
    #[error("could not read node {node_id}: {source}")]
    ReadNode {
        node_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("node {node_id} no longer exists")]
    NodeGone { node_id: i32 },
    #[error("node {node} has no stored token")]
    NoToken { node: String },
    #[error("the token of node {node} is unreadable: {source}")]
    UnreadableToken {
        node: String,
        #[source]
        source: EncryptionError,
    },
    #[error("no client for node {node}: {source}")]
    Client {
        node: String,
        #[source]
        source: ClusterCaError,
    },
    #[error("its headers and body were not sent to node {node}: {source}")]
    NotSealed {
        node: String,
        #[source]
        source: SealRequestError,
    },
    #[error("could not reach its agent: {source}")]
    Unreachable {
        node: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("its worker agent is too old to run uptime checks; update it")]
    AgentTooOld { node: String },
    #[error("{}", temps_core::sealed_request::older_agent_message(node))]
    SealingUnsupported { node: String },
    #[error("its answer could not be read: {source}")]
    AnswerUnreadable {
        node: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("its answer was larger than {limit} bytes")]
    AnswerTooLarge { node: String, limit: usize },
    #[error("its agent answered HTTP {status} without a result")]
    NoResult { node: String, status: u16 },
    #[error("its agent refused the check (HTTP {status}): {reason}")]
    Refused {
        node: String,
        status: u16,
        reason: String,
    },
}

/// Where checks can run besides the control plane.
#[async_trait]
pub trait ProbeLocations: Send + Sync {
    /// The worker nodes a check can run from now, at most
    /// [`MAX_NODE_LOCATIONS`].
    async fn nodes(&self) -> Result<Vec<NodeLocation>, sea_orm::DbErr>;

    /// Run `request` from `node`. `Err` says why the node gave no answer.
    async fn probe(
        &self,
        node: &NodeLocation,
        request: &RemoteProbeRequest,
    ) -> Result<ProbeOutcome, NodeProbeError>;
}

/// No worker nodes: every check runs from the control plane only.
#[derive(Debug, Default)]
pub struct NoNodeLocations;

#[async_trait]
impl ProbeLocations for NoNodeLocations {
    async fn nodes(&self) -> Result<Vec<NodeLocation>, sea_orm::DbErr> {
        Ok(Vec::new())
    }

    async fn probe(
        &self,
        node: &NodeLocation,
        _: &RemoteProbeRequest,
    ) -> Result<ProbeOutcome, NodeProbeError> {
        Err(NodeProbeError::NotALocation {
            node: node.name.clone(),
        })
    }
}

/// What picking a node looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCandidate {
    pub id: i32,
    pub name: String,
    pub role: String,
    pub status: String,
    pub has_token: bool,
    pub last_heartbeat: Option<UtcDateTime>,
    pub region: Option<String>,
}

impl From<&nodes::Model> for NodeCandidate {
    fn from(node: &nodes::Model) -> Self {
        Self {
            id: node.id,
            name: node.name.clone(),
            role: node.role.clone(),
            status: node.status.clone(),
            has_token: node.token_encrypted.is_some(),
            last_heartbeat: node.last_heartbeat,
            region: node
                .labels
                .get("region")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|region| !region.is_empty())
                .map(str::to_string),
        }
    }
}

/// The nodes to ask, from `candidates`: online workers with a token, in id
/// order, one per region first so the locations are as far apart as the
/// cluster allows, then the rest, at most [`MAX_NODE_LOCATIONS`].
pub fn pick_nodes(candidates: &[NodeCandidate], now: UtcDateTime) -> Vec<NodeLocation> {
    let fresh_after = now - chrono::Duration::seconds(NODE_FRESH_FOR_SECS);
    let mut eligible: Vec<&NodeCandidate> = candidates
        .iter()
        .filter(|node| {
            node.role == "worker"
                && node.status == "active"
                && node.has_token
                && node.last_heartbeat.is_some_and(|seen| seen >= fresh_after)
        })
        .collect();
    eligible.sort_by_key(|node| node.id);
    let mut picked: Vec<&NodeCandidate> = Vec::new();
    for node in &eligible {
        let new_region = node.region.as_ref().is_some_and(|region| {
            !picked
                .iter()
                .any(|chosen| chosen.region.as_ref() == Some(region))
        });
        if new_region && picked.len() < MAX_NODE_LOCATIONS {
            picked.push(node);
        }
    }
    for node in &eligible {
        if picked.len() >= MAX_NODE_LOCATIONS {
            break;
        }
        if !picked.iter().any(|chosen| chosen.id == node.id) {
            picked.push(node);
        }
    }
    picked.sort_by_key(|node| node.id);
    picked
        .into_iter()
        .map(|node| NodeLocation {
            node_id: node.id,
            name: node.name.clone(),
            region: node.region.clone(),
        })
        .collect()
}

fn shorten(message: String) -> String {
    if message.chars().count() <= MAX_MESSAGE_CHARS {
        return message;
    }
    let mut short: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
    short.push('…');
    short
}

impl MonitorLocationResult {
    pub fn control_plane(outcome: ProbeOutcome) -> Self {
        Self {
            location: CONTROL_PLANE_LOCATION.to_string(),
            node_id: None,
            region: None,
            status: Some(outcome.status),
            response_time_ms: outcome.response_time_ms,
            message: outcome.message.map(shorten),
        }
    }

    pub fn node(node: &NodeLocation, answer: Result<ProbeOutcome, NodeProbeError>) -> Self {
        let (status, response_time_ms, message) = match answer {
            Ok(outcome) => (
                Some(outcome.status),
                outcome.response_time_ms.filter(|ms| *ms >= 0),
                outcome.message,
            ),
            Err(error) => (None, None, Some(format!("no answer: {error}"))),
        };
        Self {
            location: node.name.clone(),
            node_id: Some(node.node_id),
            region: node.region.clone(),
            status,
            response_time_ms,
            message: message.map(shorten),
        }
    }
}

fn median(mut values: Vec<i32>) -> Option<i32> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

fn named(results: &[&MonitorLocationResult]) -> String {
    let mut named: Vec<String> = results
        .iter()
        .take(MAX_NAMED_LOCATIONS)
        .map(|result| match &result.message {
            Some(message) => format!("{}: {message}", result.location),
            None => result.location.clone(),
        })
        .collect();
    if results.len() > MAX_NAMED_LOCATIONS {
        named.push(format!("and {} more", results.len() - MAX_NAMED_LOCATIONS));
    }
    named.join("; ")
}

/// The verdict of a check run from several locations: down when more than
/// half of the locations that answered say down, degraded when more than
/// half say down or degraded, otherwise operational. A tie is not a
/// majority, so with two locations both must agree.
pub fn combine(results: &[MonitorLocationResult]) -> ProbeOutcome {
    let answered: Vec<&MonitorLocationResult> = results
        .iter()
        .filter(|result| result.status.is_some())
        .collect();
    if answered.is_empty() {
        return ProbeOutcome {
            status: ProbeStatus::Down,
            response_time_ms: None,
            message: Some("no location answered".to_string()),
        };
    }
    let total = answered.len();
    let down: Vec<&MonitorLocationResult> = answered
        .iter()
        .copied()
        .filter(|result| result.status == Some(ProbeStatus::Down))
        .collect();
    let failing: Vec<&MonitorLocationResult> = answered
        .iter()
        .copied()
        .filter(|result| result.status != Some(ProbeStatus::Operational))
        .collect();
    let response_time_ms = median(
        answered
            .iter()
            .filter_map(|result| result.response_time_ms)
            .collect(),
    );
    let (status, message) = if down.len() * 2 > total {
        (
            ProbeStatus::Down,
            Some(format!(
                "down from {} of {total} locations: {}",
                down.len(),
                named(&down)
            )),
        )
    } else if failing.len() * 2 > total {
        (
            ProbeStatus::Degraded,
            Some(format!(
                "failing from {} of {total} locations: {}",
                failing.len(),
                named(&failing)
            )),
        )
    } else {
        (ProbeStatus::Operational, None)
    };
    ProbeOutcome {
        status,
        response_time_ms,
        message,
    }
}

/// The agent's answer envelope (`AgentResponse` in `temps-node-agent`).
#[derive(Deserialize)]
struct AgentAnswer {
    success: bool,
    data: Option<ProbeOutcome>,
    error: Option<String>,
}

struct NodeClient {
    built: Instant,
    client: reqwest::Client,
    url: String,
    token: String,
    sealing: NodeSealing,
}

/// Worker nodes as probe locations, reached over the same authenticated
/// (mutual TLS when the node is `https://`) agent API as deployments.
pub struct NodeProbeLocations {
    db: Arc<DatabaseConnection>,
    config: Arc<ConfigService>,
    encryption: Arc<EncryptionService>,
    /// A std mutex on purpose: its guard cannot be held across an `.await` in
    /// these `Send` futures, so a slow client build for one node never blocks
    /// checks that go to the others.
    clients: Mutex<HashMap<i32, Arc<NodeClient>>>,
}

impl NodeProbeLocations {
    pub fn new(
        db: Arc<DatabaseConnection>,
        config: Arc<ConfigService>,
        encryption: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            config,
            encryption,
            clients: Mutex::new(HashMap::new()),
        }
    }

    fn cached(&self, node_id: i32) -> Option<Arc<NodeClient>> {
        let clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        clients
            .get(&node_id)
            .filter(|client| client.built.elapsed() < CLIENT_MAX_AGE)
            .cloned()
    }

    /// The node's client: the cached one while it is fresh, otherwise a new
    /// one. The cache lock is released while the node is read and the client
    /// is built.
    async fn client(&self, node_id: i32) -> Result<Arc<NodeClient>, NodeProbeError> {
        if let Some(client) = self.cached(node_id) {
            return Ok(client);
        }
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| NodeProbeError::ReadNode { node_id, source })?
            .ok_or(NodeProbeError::NodeGone { node_id })?;
        let encrypted = node
            .token_encrypted
            .as_deref()
            .ok_or_else(|| NodeProbeError::NoToken {
                node: node.name.clone(),
            })?;
        let token = self
            .encryption
            .decrypt_string(encrypted)
            .map_err(|source| NodeProbeError::UnreadableToken {
                node: node.name.clone(),
                source,
            })?;
        let endpoint = node_agent_endpoint(&node);
        let client = build_node_http_client(&endpoint, &self.config, &self.encryption, None)
            .await
            .map_err(|source| NodeProbeError::Client {
                node: node.name.clone(),
                source,
            })?;
        let client = Arc::new(NodeClient {
            built: Instant::now(),
            client,
            url: format!(
                "{}{REMOTE_PROBE_PATH}",
                endpoint.dial_url.trim_end_matches('/')
            ),
            token,
            sealing: node.request_sealing(),
        });
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node_id, client.clone());
        Ok(client)
    }

    fn forget(&self, node_id: i32) {
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&node_id);
    }

    async fn ask(
        &self,
        node: &NodeLocation,
        client: &NodeClient,
        request: &RemoteProbeRequest,
    ) -> Result<ProbeOutcome, NodeProbeError> {
        let name = || node.name.clone();
        let sealed = client
            .sealing
            .seal_json(&node.name, REMOTE_PROBE_PATH, request);
        let builder = client
            .client
            .post(&client.url)
            .bearer_auth(&client.token)
            .timeout(request.check.timeout() + NODE_GRACE);
        let builder = match sealed {
            Ok(body) => builder
                .header(reqwest::header::CONTENT_TYPE, SEALED_REQUEST_CONTENT_TYPE)
                .body(body),
            // Nothing secret to protect: the target and check are shown to
            // anyone who can read the monitor.
            Err(_) if request.request.is_empty() => builder.json(request),
            Err(source) => return Err(NodeProbeError::NotSealed { node: name(), source }),
        };
        let mut response = builder
            .send()
            .await
            .map_err(|source| NodeProbeError::Unreachable { node: name(), source })?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(NodeProbeError::AgentTooOld { node: name() });
        }
        if status == reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE {
            return Err(NodeProbeError::SealingUnsupported { node: name() });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| NodeProbeError::AnswerUnreadable { node: name(), source })?
        {
            if body.len() + chunk.len() > MAX_ANSWER_BYTES {
                return Err(NodeProbeError::AnswerTooLarge {
                    node: name(),
                    limit: MAX_ANSWER_BYTES,
                });
            }
            body.extend_from_slice(&chunk);
        }
        let answer: AgentAnswer =
            serde_json::from_slice(&body).map_err(|_| NodeProbeError::NoResult {
                node: name(),
                status: status.as_u16(),
            })?;
        match (answer.success, answer.data, answer.error) {
            (true, Some(outcome), _) => Ok(outcome),
            (_, _, Some(reason)) => Err(NodeProbeError::Refused {
                node: name(),
                status: status.as_u16(),
                reason,
            }),
            _ => Err(NodeProbeError::NoResult {
                node: name(),
                status: status.as_u16(),
            }),
        }
    }
}

#[async_trait]
impl ProbeLocations for NodeProbeLocations {
    async fn nodes(&self) -> Result<Vec<NodeLocation>, sea_orm::DbErr> {
        let candidates = nodes::Entity::find()
            .filter(nodes::Column::Role.eq("worker"))
            .filter(nodes::Column::Status.eq("active"))
            .order_by_asc(nodes::Column::Id)
            .all(self.db.as_ref())
            .await?;
        let candidates: Vec<NodeCandidate> = candidates.iter().map(NodeCandidate::from).collect();
        Ok(pick_nodes(&candidates, chrono::Utc::now()))
    }

    async fn probe(
        &self,
        node: &NodeLocation,
        request: &RemoteProbeRequest,
    ) -> Result<ProbeOutcome, NodeProbeError> {
        let client = self.client(node.node_id).await?;
        let answer = self.ask(node, &client, request).await;
        if let Err(error) = &answer {
            debug!(node_id = node.node_id, node = %node.name, %error, "A node gave no uptime answer");
            self.forget(node.node_id);
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i32, region: Option<&str>) -> NodeCandidate {
        NodeCandidate {
            id,
            name: format!("worker-{id}"),
            role: "worker".into(),
            status: "active".into(),
            has_token: true,
            last_heartbeat: Some(chrono::Utc::now()),
            region: region.map(str::to_string),
        }
    }

    fn result(
        location: &str,
        status: Option<ProbeStatus>,
        ms: Option<i32>,
    ) -> MonitorLocationResult {
        MonitorLocationResult {
            location: location.to_string(),
            node_id: None,
            region: None,
            status,
            response_time_ms: ms,
            message: status
                .filter(|status| *status != ProbeStatus::Operational)
                .map(|_| format!("{location} failed")),
        }
    }

    #[test]
    fn picks_online_workers_one_per_region_first() {
        let now = chrono::Utc::now();
        let mut stale = node(2, Some("eu"));
        stale.last_heartbeat = Some(now - chrono::Duration::minutes(10));
        let mut offline = node(3, Some("us"));
        offline.status = "offline".into();
        let mut control = node(4, Some("ap"));
        control.role = "control".into();
        let mut tokenless = node(5, None);
        tokenless.has_token = false;
        let candidates = vec![
            node(9, None),
            node(8, Some("eu")),
            node(7, Some("eu")),
            stale,
            offline,
            control,
            tokenless,
            node(6, Some("us")),
            node(10, None),
            node(11, None),
            node(12, Some("ap")),
        ];
        let picked: Vec<i32> = pick_nodes(&candidates, now)
            .into_iter()
            .map(|location| location.node_id)
            .collect();
        // us, eu and ap first (12 beats 9, 10 and 11), then the next by id.
        assert_eq!(picked, vec![6, 7, 8, 12]);
        let picked = pick_nodes(&candidates[..1], now);
        assert_eq!(picked[0].region, None);
        assert!(pick_nodes(&[], now).is_empty());
    }

    #[test]
    fn the_majority_decides() {
        use ProbeStatus::{Degraded, Down, Operational};
        // One location out of three cannot reach it: not an outage.
        let one_down = combine(&[
            result("control plane", Some(Operational), Some(30)),
            result("worker-a", Some(Down), None),
            result("worker-b", Some(Operational), Some(50)),
        ]);
        assert_eq!(one_down.status, Operational);
        assert_eq!(one_down.message, None);
        assert_eq!(one_down.response_time_ms, Some(50));

        let two_down = combine(&[
            result("control plane", Some(Down), Some(10)),
            result("worker-a", Some(Down), None),
            result("worker-b", Some(Operational), Some(50)),
        ]);
        assert_eq!(two_down.status, Down);
        let message = two_down.message.unwrap();
        assert!(
            message.starts_with("down from 2 of 3 locations"),
            "{message}"
        );
        assert!(message.contains("worker-a: worker-a failed"), "{message}");

        // A tie is no majority.
        let tie = combine(&[
            result("control plane", Some(Down), None),
            result("worker-a", Some(Operational), Some(20)),
        ]);
        assert_eq!(tie.status, Operational);

        let mixed = combine(&[
            result("control plane", Some(Degraded), Some(900)),
            result("worker-a", Some(Down), None),
            result("worker-b", Some(Operational), Some(20)),
        ]);
        assert_eq!(mixed.status, Degraded);
        assert!(mixed.message.unwrap().starts_with("failing from 2 of 3"));
    }

    #[test]
    fn locations_that_did_not_answer_are_not_counted() {
        use ProbeStatus::{Down, Operational};
        let outcome = combine(&[
            result("control plane", Some(Down), None),
            result("worker-a", None, None),
            result("worker-b", None, None),
        ]);
        assert_eq!(outcome.status, Down);
        assert!(outcome.message.unwrap().starts_with("down from 1 of 1"));
        assert_eq!(combine(&[result("worker-a", None, None)]).status, Down);
        let up = combine(&[result("control plane", Some(Operational), Some(5))]);
        assert_eq!(up.status, Operational);
    }

    #[test]
    fn node_answers_are_bounded() {
        let location = NodeLocation {
            node_id: 3,
            name: "worker-a".into(),
            region: Some("eu".into()),
        };
        let long = MonitorLocationResult::node(
            &location,
            Ok(ProbeOutcome {
                status: ProbeStatus::Down,
                response_time_ms: Some(-4),
                message: Some("x".repeat(5000)),
            }),
        );
        assert_eq!(long.response_time_ms, None);
        assert_eq!(long.message.unwrap().chars().count(), MAX_MESSAGE_CHARS + 1);
        let silent = MonitorLocationResult::node(
            &location,
            Err(NodeProbeError::AgentTooOld {
                node: "worker-a".into(),
            }),
        );
        assert_eq!(silent.status, None);
        assert_eq!(silent.region.as_deref(), Some("eu"));
        assert_eq!(
            silent.message.as_deref(),
            Some("no answer: its worker agent is too old to run uptime checks; update it")
        );
    }

    #[test]
    fn node_probe_errors_keep_their_cause_and_node() {
        use std::error::Error as _;
        let error = NodeProbeError::ReadNode {
            node_id: 9,
            source: sea_orm::DbErr::Custom("connection reset".into()),
        };
        assert!(error.to_string().contains("node 9"), "{error}");
        assert!(
            error.source().is_some_and(|source| source.to_string().contains("connection reset")),
            "the database error stays the source"
        );
        let refused = NodeProbeError::Refused {
            node: "worker-a".into(),
            status: 429,
            reason: "this node is already running 16 uptime checks".into(),
        };
        assert!(matches!(refused, NodeProbeError::Refused { status: 429, .. }));
        assert!(refused.to_string().contains("HTTP 429"), "{refused}");
    }

    #[test]
    fn many_dissenting_locations_are_summarised() {
        let results: Vec<MonitorLocationResult> = (0..5)
            .map(|i| result(&format!("worker-{i}"), Some(ProbeStatus::Down), None))
            .collect();
        let message = combine(&results).message.unwrap();
        assert!(message.ends_with("and 2 more"), "{message}");
    }
}
