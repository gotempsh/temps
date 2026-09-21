// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host-level grant of the Docker socket to named projects (ADR 045).
//!
//! Temps never hands a deployed container the host's Docker engine. The one
//! exception this module implements is an **operator-set, host-level** grant:
//! `TEMPS_DOCKER_SOCKET_PROJECTS=<slug>[,<slug>...]`, read once at startup by
//! every process that can create a container (`temps serve` for local
//! placement, `temps agent` on each worker). A project named there — and only
//! there — gets `/var/run/docker.sock` bind-mounted into its containers *on
//! that host*.
//!
//! It is deliberately an environment variable rather than a column, following
//! `TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES`: which projects may hold
//! root-equivalent access to *this machine* is a decision for whoever has a
//! shell on this machine. It must not be reachable through the API, because
//! any account or token that could flip it would be one step from the host.
//!
//! This type lives in `temps-core` rather than `temps-deployer` because five
//! crates need it: the deployer (builds the bind), the agent (advertises it),
//! `temps-deployments` (schedules against it), `temps-projects` (surfaces the
//! capability) and the CLI (constructs it at startup). It is pure, dependency
//! free, and pairs with [`crate::docker_handle`], which already owns the
//! placement-capability vocabulary.

use std::collections::BTreeSet;

/// Environment variable naming the projects this host grants the socket to.
pub const DOCKER_SOCKET_PROJECTS_ENV: &str = "TEMPS_DOCKER_SOCKET_PROJECTS";

/// The single bind a granted container receives. Nothing else about the
/// container's `HostConfig` changes: `cap_drop: ALL`, `no-new-privileges`, the
/// PID limit and the read-only secrets mount all stay.
pub const DOCKER_SOCKET_BIND: &str = "/var/run/docker.sock:/var/run/docker.sock";

/// Key under a node's agent-reported `capacity` JSON carrying the slugs that
/// node grants. Stored there rather than in a dedicated column because the
/// value is agent-derived, refreshed wholesale on every heartbeat, and never
/// written by an operator through the API — exactly like the rest of
/// `capacity`.
pub const NODE_CAPACITY_KEY: &str = "docker_socket_projects";

/// Name used for the control plane in capability responses. The control plane
/// is not a row in `nodes` (see `CONTROL_PLANE_NODE_ID`), so it needs a stable
/// label clients can render next to real node names.
pub const CONTROL_PLANE_NODE_NAME: &str = "control-plane";

/// Console path an operator visits to see which hosts grant the socket.
pub const DOCKER_SOCKET_SETUP_PATH: &str = crate::docker_handle::WORKER_NODE_SETUP_PATH;

/// The set of project slugs this process grants host Docker access to.
///
/// Empty (the default, and the state of every install that never sets the
/// variable) means the historical behaviour: no container ever receives the
/// socket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DockerSocketGrant {
    slugs: BTreeSet<String>,
}

impl DockerSocketGrant {
    /// Read the grant from this process's environment. Call once at startup
    /// and inject the result; re-reading per deploy would let a later
    /// `set_var` (tests, embedders) change container privileges at runtime.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(DOCKER_SOCKET_PROJECTS_ENV).ok().as_deref())
    }

    /// Pure parse of the variable's value, split from [`Self::from_env`] so the
    /// rules are testable without mutating process-global environment state
    /// from parallel tests.
    ///
    /// Comma-separated; each entry is trimmed; empty entries (a trailing comma,
    /// `a,,b`, whitespace-only) are dropped; duplicates collapse. Slugs are
    /// matched **exactly** as stored on the project — no case folding, because
    /// a near-miss must fail closed rather than widen the grant.
    pub fn parse(raw: Option<&str>) -> Self {
        let slugs = raw
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect();
        Self { slugs }
    }

    /// Build a grant from an explicit slug list (heartbeat payloads, tests).
    pub fn from_slugs<I, S>(slugs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            slugs: slugs
                .into_iter()
                .map(Into::into)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }

    /// Whether this host grants `slug` host Docker access.
    pub fn allows(&self, slug: &str) -> bool {
        self.slugs.contains(slug)
    }

    /// The granted slugs, sorted, for logging and capability responses.
    pub fn slugs(&self) -> impl Iterator<Item = &str> {
        self.slugs.iter().map(String::as_str)
    }

    /// Whether this host grants nothing (the default).
    pub fn is_empty(&self) -> bool {
        self.slugs.is_empty()
    }

    /// Owned, sorted copy for serialisation (heartbeat body, capability).
    pub fn to_vec(&self) -> Vec<String> {
        self.slugs.iter().cloned().collect()
    }

    /// Emit the one startup line that tells an operator what this process
    /// decided. Logged unconditionally — "granted to nobody" is the answer an
    /// operator debugging a missing socket needs just as much as the list.
    pub fn log_startup(&self, process: &str) {
        if self.is_empty() {
            tracing::info!(
                process,
                env = DOCKER_SOCKET_PROJECTS_ENV,
                "Host Docker socket grant: no project is granted host Docker access on this host"
            );
        } else {
            tracing::info!(
                process,
                env = DOCKER_SOCKET_PROJECTS_ENV,
                projects = %self.to_vec().join(", "),
                "Host Docker socket grant: these projects receive /var/run/docker.sock on this \
                 host and are therefore root-equivalent on it"
            );
        }
    }
}

/// The process-wide grant, parsed from the environment on first use and then
/// frozen for the life of the process.
///
/// Every surface in a given process — the deployer that builds the container,
/// the scheduler that decides whether `Local` is eligible, the projects API
/// that reports the capability — must answer from the *same* snapshot, or the
/// console can promise a placement the deployer then refuses. A `OnceLock` is
/// what makes that true without threading the value through five plugin
/// registration orders, and it is also what makes the value un-mutable at
/// runtime: a later `set_var` cannot change container privileges.
pub fn process_grant() -> &'static DockerSocketGrant {
    static PROCESS_GRANT: std::sync::OnceLock<DockerSocketGrant> = std::sync::OnceLock::new();
    PROCESS_GRANT.get_or_init(DockerSocketGrant::from_env)
}

/// Slugs a node advertises in its heartbeat `capacity` JSON.
///
/// Tolerant by construction: a node that has never reported (older agent), or
/// reports a non-array, yields an empty list rather than an error — the effect
/// is "this node grants nothing", which is the safe answer.
pub fn capacity_grants(capacity: &serde_json::Value) -> Vec<String> {
    capacity
        .get(NODE_CAPACITY_KEY)
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Merge this process's advertised grant into a `capacity` JSON object.
///
/// Used control-plane side when persisting a heartbeat, so the slugs travel in
/// the column that is already wholly replaced every beat.
pub fn set_capacity_grants(capacity: &mut serde_json::Value, slugs: &[String]) {
    if !capacity.is_object() {
        *capacity = serde_json::json!({});
    }
    if let Some(object) = capacity.as_object_mut() {
        object.insert(
            NODE_CAPACITY_KEY.to_string(),
            serde_json::Value::from(slugs.to_vec()),
        );
    }
}

/// The sentence shown to an operator when a project is granted nowhere.
///
/// Names the exact variable, the exact value, and both processes that read it,
/// because the person reading it is debugging alone on their own host.
pub fn not_granted_reason(slug: &str) -> String {
    format!(
        "No host grants project '{slug}' access to the Docker socket. Set \
         {DOCKER_SOCKET_PROJECTS_ENV}={slug} on the host that should run it and restart \
         `temps serve` (control plane) or `temps agent` (worker node). A granted project is \
         root-equivalent on that host."
    )
}

/// Where a project is granted host Docker access, published on the project
/// response so the console can render a badge (granted) or an onboarding state
/// (not granted) instead of the feature being invisible.
///
/// Deliberately read-only. The grant is host policy; there is no write path,
/// and an API that could set it would be one step from host root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DockerSocketCapability {
    /// Whether any host grants this project `/var/run/docker.sock`.
    pub granted: bool,
    /// Hosts that grant it: worker node names, plus `control-plane` when this
    /// control plane's own environment names the project. Empty when not
    /// granted.
    #[schema(example = json!(["control-plane", "worker-1"]))]
    pub nodes: Vec<String>,
    /// Why it is not granted, when `granted` is false. Names the exact
    /// variable, value and processes, because the operator is debugging alone.
    pub reason: Option<String>,
    /// Console path that shows the hosts this could be set on.
    pub setup_path: Option<String>,
}

impl DockerSocketCapability {
    /// Derive the capability from its two inputs. Pure, so the rule lives in
    /// one place and is testable without a database.
    ///
    /// `granting_node_names` is every worker node advertising the grant, of
    /// any status — a node that is currently offline still *grants* it, and
    /// telling the operator otherwise would send them to change a variable
    /// that is already correct.
    pub fn evaluate(
        slug: &str,
        control_plane_grants: bool,
        granting_node_names: Vec<String>,
    ) -> Self {
        let mut nodes = Vec::new();
        if control_plane_grants {
            nodes.push(CONTROL_PLANE_NODE_NAME.to_string());
        }
        nodes.extend(granting_node_names);

        let granted = !nodes.is_empty();
        Self {
            granted,
            nodes,
            reason: (!granted).then(|| not_granted_reason(slug)),
            setup_path: Some(DOCKER_SOCKET_SETUP_PATH.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_is_granted_when_the_control_plane_grants_it() {
        let capability = DockerSocketCapability::evaluate("node-daemon", true, Vec::new());
        assert!(capability.granted);
        assert_eq!(capability.nodes, vec!["control-plane".to_string()]);
        assert!(capability.reason.is_none());
        assert_eq!(
            capability.setup_path.as_deref(),
            Some(DOCKER_SOCKET_SETUP_PATH)
        );
    }

    #[test]
    fn capability_lists_every_granting_host() {
        let capability = DockerSocketCapability::evaluate(
            "infra-agent",
            true,
            vec!["worker-1".to_string(), "worker-2".to_string()],
        );
        assert_eq!(
            capability.nodes,
            vec![
                "control-plane".to_string(),
                "worker-1".to_string(),
                "worker-2".to_string()
            ]
        );
    }

    #[test]
    fn capability_onboards_when_no_host_grants_it() {
        let capability = DockerSocketCapability::evaluate("node-daemon", false, Vec::new());
        assert!(!capability.granted);
        assert!(capability.nodes.is_empty());
        // Never a bare disabled state: it must say what to set and where.
        let reason = capability
            .reason
            .expect("an ungranted project explains why");
        assert!(reason.contains("TEMPS_DOCKER_SOCKET_PROJECTS=node-daemon"));
        assert_eq!(
            capability.setup_path.as_deref(),
            Some(DOCKER_SOCKET_SETUP_PATH)
        );
    }

    #[test]
    fn parse_unset_grants_nothing() {
        let grant = DockerSocketGrant::parse(None);
        assert!(grant.is_empty());
        assert!(!grant.allows("node-daemon"));
    }

    #[test]
    fn parse_empty_string_grants_nothing() {
        assert!(DockerSocketGrant::parse(Some("")).is_empty());
        assert!(DockerSocketGrant::parse(Some("   ")).is_empty());
        assert!(DockerSocketGrant::parse(Some(",,,")).is_empty());
    }

    #[test]
    fn parse_trims_entries() {
        let grant = DockerSocketGrant::parse(Some("  node-daemon , infra-agent "));
        assert!(grant.allows("node-daemon"));
        assert!(grant.allows("infra-agent"));
        assert_eq!(grant.to_vec(), vec!["infra-agent", "node-daemon"]);
    }

    #[test]
    fn parse_drops_empty_and_trailing_entries() {
        let grant = DockerSocketGrant::parse(Some("node-daemon,,infra-agent,"));
        assert_eq!(grant.to_vec(), vec!["infra-agent", "node-daemon"]);
    }

    #[test]
    fn parse_collapses_duplicates() {
        let grant = DockerSocketGrant::parse(Some("node-daemon,node-daemon, node-daemon"));
        assert_eq!(grant.to_vec(), vec!["node-daemon"]);
    }

    #[test]
    fn parse_matches_slugs_exactly() {
        let grant = DockerSocketGrant::parse(Some("node-daemon"));
        // A near miss must fail closed rather than widen the grant.
        assert!(!grant.allows("Node-Daemon"));
        assert!(!grant.allows("node-daemon-2"));
        assert!(!grant.allows("node"));
    }

    #[test]
    fn from_slugs_normalises_like_parse() {
        let grant = DockerSocketGrant::from_slugs([" node-daemon ", "", "infra-agent"]);
        assert_eq!(grant.to_vec(), vec!["infra-agent", "node-daemon"]);
    }

    #[test]
    fn capacity_grants_reads_the_advertised_list() {
        let capacity = serde_json::json!({
            "cpu_usage": 0.2,
            "docker_socket_projects": ["node-daemon", " infra-agent ", ""],
        });
        assert_eq!(
            capacity_grants(&capacity),
            vec!["node-daemon".to_string(), "infra-agent".to_string()]
        );
    }

    #[test]
    fn capacity_grants_tolerates_missing_and_malformed() {
        assert!(capacity_grants(&serde_json::json!({})).is_empty());
        assert!(capacity_grants(&serde_json::json!(null)).is_empty());
        assert!(capacity_grants(&serde_json::json!({"docker_socket_projects": "nope"})).is_empty());
    }

    #[test]
    fn set_capacity_grants_round_trips() {
        let mut capacity = serde_json::json!({"cpu_usage": 0.5});
        set_capacity_grants(&mut capacity, &["node-daemon".to_string()]);
        assert_eq!(capacity_grants(&capacity), vec!["node-daemon".to_string()]);
        assert_eq!(capacity["cpu_usage"], serde_json::json!(0.5));
    }

    #[test]
    fn set_capacity_grants_replaces_a_non_object() {
        let mut capacity = serde_json::json!("not-an-object");
        set_capacity_grants(&mut capacity, &["infra-agent".to_string()]);
        assert_eq!(capacity_grants(&capacity), vec!["infra-agent".to_string()]);
    }

    #[test]
    fn not_granted_reason_names_the_variable_and_the_slug() {
        let reason = not_granted_reason("node-daemon");
        assert!(reason.contains("TEMPS_DOCKER_SOCKET_PROJECTS=node-daemon"));
        assert!(reason.contains("temps agent"));
    }
}
