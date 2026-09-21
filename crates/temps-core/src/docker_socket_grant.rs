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

    /// Whether the control plane **declares** that `slug` requires the socket.
    ///
    /// The same variable answers two questions on the control plane: which
    /// projects this host would mount the socket for, and — because it is the
    /// only process an operator with a shell on the control plane configures —
    /// which projects the cluster as a whole treats as socket-requiring. A
    /// worker's heartbeat can narrow *where* a declared project runs; it can
    /// never make this answer true, or one compromised worker would decide
    /// which projects are root-equivalent (see [`slug_is_reserved`]).
    pub fn declares(&self, slug: &str) -> bool {
        self.allows(slug)
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

/// Whether `slug` is reserved by a host's grant, and may therefore only be
/// **claimed** — created, or renamed to — by an instance admin.
///
/// Pure, with the grant injected, so the rule is testable against an explicit
/// grant instead of the process-wide `OnceLock` (mirrors
/// `temps_deployer::docker::docker_socket_bind_for`). Callers in the control
/// plane pass [`process_grant`].
///
/// The rule exists because `projects.slug` is writable by any project writer:
/// without it, renaming a project onto a granted slug would hand its next
/// deployment host root on every machine that grants that slug. Only the claim
/// is reserved — an existing project whose slug already matches keeps working,
/// including through updates that do not change the slug.
pub fn slug_is_reserved(grant: &DockerSocketGrant, slug: &str) -> bool {
    grant.allows(slug)
}

/// The sentence shown to whoever tried to claim a reserved slug.
///
/// Names the ADR and the variable rather than only refusing: the person who
/// hits this is usually an operator who *did* set the variable and is now
/// surprised their own project create is rejected.
pub fn reserved_slug_reason(slug: &str) -> String {
    format!(
        "Project slug '{slug}' is reserved by this host's {DOCKER_SOCKET_PROJECTS_ENV} policy \
         (ADR 045): a project with that slug is granted `/var/run/docker.sock` and is therefore \
         root-equivalent on every host that grants it. Only an instance admin may create or \
         rename a project onto a granted slug. Choose another slug, or ask an admin."
    )
}

/// Who is asking for a deployment, for the ADR-045 rule that a project holding
/// host Docker access may only be deployed by an instance admin.
///
/// Restricting the *slug* is not enough on its own: a granted project created
/// by an admin has no restrictive access grants of its own, so any principal
/// with `DeploymentsCreate` on it could otherwise deploy an image and command
/// of their choosing into a container that gets `/var/run/docker.sock` — host
/// root by a route that never touches the slug.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeployCaller {
    /// An ordinary principal holding `DeploymentsCreate`. The fail-closed
    /// default, so a deploy path that never considered this cannot be the one
    /// that hands out the socket.
    #[default]
    ProjectWriter,
    /// An instance admin (`AuthContext::is_instance_admin`) — the same bar
    /// that may claim the slug in the first place.
    InstanceAdmin,
    /// Temps itself, with no user behind the request: failover and node-drain
    /// rescheduling, cron, the deployment processor.
    ///
    /// Allowed, deliberately. These paths redeploy *the workload that is
    /// already there* — they carry no attacker-chosen image or command — and
    /// refusing them would mean a granted infrastructure service silently
    /// stays down after its node dies, which is the failure mode this whole
    /// feature exists to avoid.
    Platform,
}

impl DeployCaller {
    /// Derive the caller from an instance-admin check.
    pub fn from_instance_admin(is_instance_admin: bool) -> Self {
        if is_instance_admin {
            Self::InstanceAdmin
        } else {
            Self::ProjectWriter
        }
    }

    /// Whether this caller may deploy a project that holds host Docker access.
    pub fn may_deploy_granted_project(self) -> bool {
        matches!(self, Self::InstanceAdmin | Self::Platform)
    }
}

/// Whether deploying `project_slug` requires instance-admin authority, and the
/// caller does not have it (ADR 045).
///
/// Pure, with the grant injected, for the same reason as [`slug_is_reserved`].
/// A project no host declares is unaffected — which is every project on every
/// install that never set the variable.
pub fn deploy_requires_instance_admin(
    grant: &DockerSocketGrant,
    project_slug: &str,
    caller: DeployCaller,
) -> bool {
    grant.declares(project_slug) && !caller.may_deploy_granted_project()
}

/// The sentence shown to whoever tried to deploy a granted project without
/// instance-admin authority.
pub fn granted_project_deploy_reason(slug: &str) -> String {
    format!(
        "Project '{slug}' is declared in {DOCKER_SOCKET_PROJECTS_ENV} on this control plane \
         (ADR 045), so its containers receive `/var/run/docker.sock` and are root-equivalent on \
         the host that runs them. Deploying it is therefore restricted to instance admins — \
         holding deploy permission on the project is not enough, because the deployed image and \
         command would run as host root. Ask an admin to deploy it, or remove the slug from \
         {DOCKER_SOCKET_PROJECTS_ENV} and restart the control plane if it should no longer hold \
         host Docker access."
    )
}

/// The sentence shown to whoever tried to rename a project *off* a reserved
/// slug.
///
/// The symmetric half of [`reserved_slug_reason`], and needed for the same
/// reason: the grant is keyed by slug, so a rename away from one both revokes
/// the project's host Docker access on every host that grants it — silently
/// breaking an operator-owned infrastructure service — and frees the slug for
/// whoever creates a project next, who would inherit that access. Both ends of
/// the move are therefore admin-only.
pub fn released_slug_reason(slug: &str) -> String {
    format!(
        "Project slug '{slug}' is granted `/var/run/docker.sock` by this host's \
         {DOCKER_SOCKET_PROJECTS_ENV} policy (ADR 045). Renaming this project away from it \
         would revoke its host Docker access on every host that grants it, and free the slug \
         for the next project created. Only an instance admin may do that."
    )
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
/// because the person reading it is debugging alone on their own host. Both
/// halves of the rule are spelled out: the control plane's variable *declares*
/// that a project requires the socket, and each host's own variable decides
/// whether that host provides it. Setting only one of the two is the failure
/// an operator cannot otherwise see.
pub fn not_granted_reason(slug: &str) -> String {
    format!(
        "No host grants project '{slug}' access to the Docker socket. Set \
         {DOCKER_SOCKET_PROJECTS_ENV}={slug} on the control plane and restart `temps serve` — \
         that declares the project requires the socket — and set the same variable on each host \
         that should run it (`temps agent` on a worker node; the control plane itself already \
         counts) and restart it. A granted project is root-equivalent on every host that \
         grants it."
    )
}

/// The sentence shown when worker nodes advertise the grant but this control
/// plane never declared the project.
///
/// A heartbeat narrows *where* a declared project may run; it never creates
/// the requirement, or one compromised or misconfigured worker could make
/// itself the only eligible host for any project it names. The operator has
/// already done half the work here, so say which half is missing and which
/// hosts are already configured.
pub fn not_declared_reason(slug: &str, advertising_nodes: &[String]) -> String {
    format!(
        "Node(s) {} grant project '{slug}' host Docker access, but this control plane does not \
         declare it: a node's advertisement alone never grants the socket. Set \
         {DOCKER_SOCKET_PROJECTS_ENV}={slug} on the control plane and restart `temps serve`, \
         then deployments of this project are placed only on hosts that grant it.",
        advertising_nodes.join(", ")
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
    /// `control_plane_declares` is this control plane's own process grant. It
    /// is the *gate*: a project is granted host Docker access only when the
    /// operator declared it there, because that is the one variable no worker
    /// heartbeat can write. Nodes advertising a slug the control plane never
    /// declared are reported as the misconfiguration they are, not as a grant
    /// — otherwise a single worker could decide, from its own heartbeat, which
    /// projects are root-equivalent.
    ///
    /// `granting_node_names` is every worker node advertising the grant, of
    /// any status — a node that is currently offline still *grants* it, and
    /// telling the operator otherwise would send them to change a variable
    /// that is already correct.
    pub fn evaluate(
        slug: &str,
        control_plane_declares: bool,
        granting_node_names: Vec<String>,
    ) -> Self {
        if !control_plane_declares {
            return Self {
                granted: false,
                // Deliberately empty: `nodes` lists hosts this project is
                // actually granted on, and without the declaration it is
                // granted nowhere Temps will place it. The advertising nodes
                // are named in `reason` instead, where they are the fix.
                nodes: Vec::new(),
                reason: Some(if granting_node_names.is_empty() {
                    not_granted_reason(slug)
                } else {
                    not_declared_reason(slug, &granting_node_names)
                }),
                setup_path: Some(DOCKER_SOCKET_SETUP_PATH.to_string()),
            };
        }

        let mut nodes = vec![CONTROL_PLANE_NODE_NAME.to_string()];
        nodes.extend(granting_node_names);

        Self {
            granted: true,
            nodes,
            reason: None,
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
    fn a_node_advertisement_alone_never_grants_the_capability() {
        // The finding this rule exists for: a worker that advertises a slug
        // the control plane never declared must not read back as "granted",
        // or the console would promise host Docker access that no operator
        // asked for.
        let capability =
            DockerSocketCapability::evaluate("node-daemon", false, vec!["worker-1".to_string()]);
        assert!(!capability.granted);
        assert!(capability.nodes.is_empty());
        let reason = capability
            .reason
            .expect("an ungranted project explains why");
        assert!(reason.contains("worker-1"), "{reason}");
        assert!(
            reason.contains("does not declare it"),
            "the missing half must be named: {reason}"
        );
        assert!(
            reason.contains("TEMPS_DOCKER_SOCKET_PROJECTS=node-daemon"),
            "{reason}"
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
        // Both halves of the rule, or an operator sets one and waits.
        assert!(reason.contains("temps serve"));
        assert!(reason.contains("control plane"));
    }

    #[test]
    fn a_reserved_slug_is_exactly_a_granted_slug() {
        let grant = DockerSocketGrant::parse(Some("node-daemon"));
        assert!(slug_is_reserved(&grant, "node-daemon"));
        // Same exact-match rule as the bind: a near miss is not reserved, and
        // is also not granted, so the two can never disagree.
        assert!(!slug_is_reserved(&grant, "node-daemon-2"));
        assert!(!slug_is_reserved(&grant, "Node-Daemon"));
        assert!(!slug_is_reserved(
            &DockerSocketGrant::default(),
            "node-daemon"
        ));
    }

    #[test]
    fn reserved_slug_reason_names_the_adr_the_variable_and_the_remedy() {
        let reason = reserved_slug_reason("node-daemon");
        assert!(reason.contains("ADR 045"), "{reason}");
        assert!(reason.contains("TEMPS_DOCKER_SOCKET_PROJECTS"), "{reason}");
        assert!(reason.contains("instance admin"), "{reason}");
    }

    #[test]
    fn declares_is_the_same_exact_match_as_allows() {
        let grant = DockerSocketGrant::parse(Some("node-daemon"));
        assert!(grant.declares("node-daemon"));
        assert!(!grant.declares("infra-agent"));
    }

    #[test]
    fn deploying_a_granted_project_is_admin_only() {
        let grant = DockerSocketGrant::parse(Some("node-daemon"));
        // Holding deploy permission on the project is not enough: the image
        // and command the caller chooses would run as host root.
        assert!(deploy_requires_instance_admin(
            &grant,
            "node-daemon",
            DeployCaller::ProjectWriter
        ));
        assert!(!deploy_requires_instance_admin(
            &grant,
            "node-daemon",
            DeployCaller::InstanceAdmin
        ));
    }

    #[test]
    fn temps_itself_may_redeploy_a_granted_project() {
        // Failover, node drain and the deployment processor redeploy the
        // workload that is already there. Refusing them would leave a granted
        // infrastructure service down after its node dies — the exact failure
        // this feature exists to avoid.
        let grant = DockerSocketGrant::parse(Some("node-daemon"));
        assert!(!deploy_requires_instance_admin(
            &grant,
            "node-daemon",
            DeployCaller::Platform
        ));
    }

    #[test]
    fn an_ungranted_project_is_deployable_by_anyone_who_may_deploy_it() {
        // The state of every project on every install, including the other
        // projects on a host that grants one. Nothing about ordinary
        // deployment permissions changes.
        let grant = DockerSocketGrant::parse(Some("node-daemon"));
        assert!(!deploy_requires_instance_admin(
            &grant,
            "my-app",
            DeployCaller::ProjectWriter
        ));
        // Exact match, same as the bind — a near miss is not declared.
        assert!(!deploy_requires_instance_admin(
            &grant,
            "node-daemon-2",
            DeployCaller::ProjectWriter
        ));
        assert!(!deploy_requires_instance_admin(
            &DockerSocketGrant::default(),
            "node-daemon",
            DeployCaller::ProjectWriter
        ));
    }

    #[test]
    fn the_deploy_caller_defaults_to_the_fail_closed_answer() {
        // A deploy path that never considered ADR 045 must not be the one
        // that hands out the socket.
        assert_eq!(DeployCaller::default(), DeployCaller::ProjectWriter);
        assert!(!DeployCaller::default().may_deploy_granted_project());
        assert!(DeployCaller::from_instance_admin(true).may_deploy_granted_project());
        assert!(!DeployCaller::from_instance_admin(false).may_deploy_granted_project());
    }

    #[test]
    fn granted_project_deploy_reason_says_why_permission_was_not_enough() {
        let reason = granted_project_deploy_reason("node-daemon");
        assert!(reason.contains("ADR 045"), "{reason}");
        assert!(reason.contains("TEMPS_DOCKER_SOCKET_PROJECTS"), "{reason}");
        assert!(reason.contains("instance admins"), "{reason}");
        // The operator reading this has nobody to ask: it must name both ways
        // out, not just the refusal.
        assert!(reason.contains("Ask an admin"), "{reason}");
    }
}
