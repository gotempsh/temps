// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Management of per-project / per-environment IP allow/deny lists.
//!
//! This service owns validation and persistence only. Enforcement is done by
//! [`crate::ip_access::ProjectIpAccessGate`], which picks up writes from the
//! database on its own poll, so the service never talks to the proxy.
//!
//! Scope model: `environment_id = None` is the project-wide policy; a row with
//! an environment id overrides it for that environment alone (replace, not
//! merge). Deleting an environment row returns it to inheriting the project.

use crate::ip_access::{
    parse_cidr, CidrError, CidrSet, IpAccessMode, MAX_CIDRS_PER_POLICY, MAX_DESCRIPTION_LEN,
};
use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use temps_entities::project_ip_access_rules::{self, IpAccessCidrEntry, IpAccessCidrs};
use temps_entities::{environments, projects};
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum ProjectIpAccessError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },

    #[error("No IP access policy is configured for {scope}")]
    PolicyNotFound { scope: String },

    #[error("Invalid CIDR in IP access policy for {scope}: {source}")]
    InvalidCidr {
        scope: String,
        #[source]
        source: CidrError,
    },

    #[error("Duplicate CIDR '{cidr}' in IP access policy for {scope}")]
    DuplicateCidr { scope: String, cidr: String },

    #[error("IP access policy for {scope} has {count} entries; the limit is {max}")]
    TooManyCidrs {
        scope: String,
        count: usize,
        max: usize,
    },

    #[error(
        "Description for CIDR '{cidr}' in IP access policy for {scope} is {len} characters; \
         the limit is {max}"
    )]
    DescriptionTooLong {
        scope: String,
        cidr: String,
        len: usize,
        max: usize,
    },

    #[error(
        "An allow list for {scope} with no entries would block every client; add at least one \
         CIDR or set the mode to 'disabled'"
    )]
    EmptyAllowList { scope: String },

    #[error(
        "Saving this policy for {scope} would {effect} your current address {caller_ip}; \
         resubmit with acknowledge_lockout=true to apply it anyway"
    )]
    WouldLockOutCaller {
        scope: String,
        caller_ip: String,
        effect: &'static str,
    },

    #[error("'{input}' is not a valid IPv4 or IPv6 address")]
    InvalidIpAddress { input: String },

    #[error(
        "A draft to test against {scope} needs both 'mode' and 'cidrs'; '{missing}' was not \
         supplied. Send both to preview a draft, or neither to test the saved policy"
    )]
    IncompleteDraft {
        scope: String,
        missing: &'static str,
    },

    #[error(
        "The IP access policy for {scope} was changed by another request at the same time; \
         reload it and try again"
    )]
    ConcurrentUpdate { scope: String },

    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),
}

/// Whether `error` is a violation of a unique index. Postgres reports it as
/// SQLSTATE 23505, which sea-orm exposes through `sql_err()`; the text match
/// covers drivers and mocks that surface only the message.
fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) || {
        let text = error.to_string();
        text.contains("duplicate key value violates unique constraint")
            || text.contains("UNIQUE constraint failed")
    }
}

impl From<sea_orm::DbErr> for ProjectIpAccessError {
    fn from(error: sea_orm::DbErr) -> Self {
        // The only unique index on the table is one policy per scope, so a
        // violation means two writers raced to create the same scope. The
        // scope is not known here; `set` re-labels it before returning.
        if is_unique_violation(&error) {
            return ProjectIpAccessError::ConcurrentUpdate {
                scope: "this scope".to_string(),
            };
        }
        ProjectIpAccessError::Database(error)
    }
}

/// Human-readable scope used in error messages.
fn scope_label(project_id: i32, environment_id: Option<i32>) -> String {
    match environment_id {
        Some(environment_id) => format!("environment {environment_id} of project {project_id}"),
        None => format!("project {project_id}"),
    }
}

/// Request body for `PUT`: the complete policy for one scope.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetProjectIpAccessRequest {
    /// `disabled` keeps the list but enforces nothing; `allow` admits only
    /// listed clients; `deny` blocks listed clients.
    pub mode: IpAccessMode,
    /// CIDRs (IPv4 or IPv6). A bare address is a single host. At most 256.
    pub cidrs: Vec<IpAccessCidrEntry>,
    /// Apply an allow list that excludes (or a deny list that includes) the
    /// address this request came from. Without it such a save is refused
    /// with 409 so an operator cannot lock themselves out by accident.
    #[serde(default)]
    pub acknowledge_lockout: bool,
}

/// One scope's stored policy.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectIpAccessResponse {
    pub project_id: i32,
    /// `null` for the project-wide policy.
    pub environment_id: Option<i32>,
    /// `false` when nothing is stored for this scope. For an environment that
    /// means it inherits the project-wide policy.
    pub configured: bool,
    pub mode: IpAccessMode,
    pub cidrs: Vec<IpAccessCidrEntry>,
    /// Maximum number of CIDRs a policy may hold.
    pub max_cidrs: usize,
    #[schema(value_type = Option<String>, format = "date-time", example = "2026-10-13T09:30:00Z")]
    pub updated_at: Option<DateTime<Utc>>,
}

impl ProjectIpAccessResponse {
    fn unconfigured(project_id: i32, environment_id: Option<i32>) -> Self {
        Self {
            project_id,
            environment_id,
            configured: false,
            mode: IpAccessMode::Disabled,
            cidrs: Vec::new(),
            max_cidrs: MAX_CIDRS_PER_POLICY,
            updated_at: None,
        }
    }
}

impl From<project_ip_access_rules::Model> for ProjectIpAccessResponse {
    fn from(model: project_ip_access_rules::Model) -> Self {
        Self {
            project_id: model.project_id,
            environment_id: model.environment_id,
            configured: true,
            // The CHECK constraint guarantees a known mode; an unknown value
            // is reported as the most restrictive reading, never as open.
            mode: IpAccessMode::parse(&model.mode).unwrap_or(IpAccessMode::Allow),
            cidrs: model.cidrs.0,
            max_cidrs: MAX_CIDRS_PER_POLICY,
            updated_at: Some(model.updated_at),
        }
    }
}

/// Request body for the "try an address against the rules" endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestProjectIpAccessRequest {
    /// Address to evaluate.
    #[schema(example = "100.101.102.103")]
    pub ip: String,
    /// Evaluate this unsaved draft instead of the stored policy. Both `mode`
    /// and `cidrs` must be given together.
    pub mode: Option<IpAccessMode>,
    pub cidrs: Option<Vec<IpAccessCidrEntry>>,
}

/// Which policy decided a test.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolicySource {
    /// The unsaved draft supplied in the request.
    Draft,
    /// The environment's own policy.
    Environment,
    /// The project-wide policy (inherited by the environment).
    Project,
    /// No policy applies: everything is allowed.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestProjectIpAccessResponse {
    /// The address as evaluated (IPv4-mapped IPv6 is shown as IPv4).
    pub ip: String,
    pub allowed: bool,
    pub source: PolicySource,
    pub mode: IpAccessMode,
    /// The listed CIDR the address fell inside, when one did.
    pub matched_cidr: Option<String>,
    /// One-sentence explanation suitable for display.
    pub reason: String,
}

/// A validated, canonical policy ready to persist.
#[derive(Debug)]
struct ValidatedPolicy {
    mode: IpAccessMode,
    entries: Vec<IpAccessCidrEntry>,
    networks: Vec<IpNetwork>,
}

/// Fold an IPv4-mapped IPv6 address into IPv4, as the gate does when matching.
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        ip => ip,
    }
}

/// Parse and canonicalise a request's entries. Pure: no database access.
fn validate_policy(
    scope: &str,
    mode: IpAccessMode,
    cidrs: &[IpAccessCidrEntry],
) -> Result<ValidatedPolicy, ProjectIpAccessError> {
    if cidrs.len() > MAX_CIDRS_PER_POLICY {
        return Err(ProjectIpAccessError::TooManyCidrs {
            scope: scope.to_string(),
            count: cidrs.len(),
            max: MAX_CIDRS_PER_POLICY,
        });
    }

    let mut seen: HashSet<String> = HashSet::with_capacity(cidrs.len());
    let mut entries = Vec::with_capacity(cidrs.len());
    let mut networks = Vec::with_capacity(cidrs.len());
    for entry in cidrs {
        let network =
            parse_cidr(&entry.cidr).map_err(|source| ProjectIpAccessError::InvalidCidr {
                scope: scope.to_string(),
                source,
            })?;
        let canonical = format!("{}/{}", network.network(), network.prefix());
        if !seen.insert(canonical.clone()) {
            return Err(ProjectIpAccessError::DuplicateCidr {
                scope: scope.to_string(),
                cidr: canonical,
            });
        }

        let description = entry
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        if let Some(description) = &description {
            let len = description.chars().count();
            if len > MAX_DESCRIPTION_LEN {
                return Err(ProjectIpAccessError::DescriptionTooLong {
                    scope: scope.to_string(),
                    cidr: canonical,
                    len,
                    max: MAX_DESCRIPTION_LEN,
                });
            }
        }

        entries.push(IpAccessCidrEntry {
            cidr: canonical,
            description,
        });
        networks.push(network);
    }

    if mode == IpAccessMode::Allow && entries.is_empty() {
        return Err(ProjectIpAccessError::EmptyAllowList {
            scope: scope.to_string(),
        });
    }

    Ok(ValidatedPolicy {
        mode,
        entries,
        networks,
    })
}

/// Evaluate one address against a validated policy: `(allowed, matched_cidr)`.
///
/// `entries[i]` must be the entry `networks[i]` was parsed from (see
/// [`aligned_policy`]); the verdict itself comes from the production matcher,
/// the entry only labels which CIDR matched.
fn evaluate(
    mode: IpAccessMode,
    entries: &[IpAccessCidrEntry],
    networks: &[IpNetwork],
    ip: IpAddr,
) -> (bool, Option<String>) {
    let ip = normalize_ip(ip);
    let matched = networks
        .iter()
        .position(|network| network.contains(ip))
        .and_then(|index| entries.get(index))
        .map(|entry| entry.cidr.clone());
    (evaluate_with_set(mode, networks, ip), matched)
}

/// Pair each stored entry with its parsed network, dropping any entry that no
/// longer parses (the gate skips those too), so indexes stay aligned.
fn aligned_policy(stored: Vec<IpAccessCidrEntry>) -> (Vec<IpAccessCidrEntry>, Vec<IpNetwork>) {
    stored
        .into_iter()
        .filter_map(|entry| parse_cidr(&entry.cidr).ok().map(|network| (entry, network)))
        .unzip()
}

/// The verdict through the production matcher, so the test endpoint cannot
/// drift from what the proxy does.
fn evaluate_with_set(mode: IpAccessMode, networks: &[IpNetwork], ip: IpAddr) -> bool {
    let set = CidrSet::from_networks(networks);
    match mode {
        IpAccessMode::Disabled => true,
        IpAccessMode::Allow => set.contains(ip),
        IpAccessMode::Deny => !set.contains(ip),
    }
}

pub struct ProjectIpAccessService {
    db: Arc<DatabaseConnection>,
}

impl ProjectIpAccessService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn ensure_scope_exists(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<(), ProjectIpAccessError> {
        // Only the id is read: this is an existence check, and decoding a
        // whole project row would pull its large JSON config for nothing.
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .select_only()
            .column(projects::Column::Id)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?;
        if project.is_none() {
            return Err(ProjectIpAccessError::ProjectNotFound { project_id });
        }
        if let Some(environment_id) = environment_id {
            let environment = environments::Entity::find_by_id(environment_id)
                .filter(environments::Column::ProjectId.eq(project_id))
                .filter(environments::Column::DeletedAt.is_null())
                .select_only()
                .column(environments::Column::Id)
                .into_tuple::<i32>()
                .one(self.db.as_ref())
                .await?;
            if environment.is_none() {
                return Err(ProjectIpAccessError::EnvironmentNotFound {
                    project_id,
                    environment_id,
                });
            }
        }
        Ok(())
    }

    async fn find_row(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Option<project_ip_access_rules::Model>, sea_orm::DbErr> {
        let mut query = project_ip_access_rules::Entity::find()
            .filter(project_ip_access_rules::Column::ProjectId.eq(project_id));
        query = match environment_id {
            Some(environment_id) => {
                query.filter(project_ip_access_rules::Column::EnvironmentId.eq(environment_id))
            }
            None => query.filter(project_ip_access_rules::Column::EnvironmentId.is_null()),
        };
        query.one(self.db.as_ref()).await
    }

    /// The stored policy for one scope, or an unconfigured placeholder.
    pub async fn get(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<ProjectIpAccessResponse, ProjectIpAccessError> {
        self.ensure_scope_exists(project_id, environment_id).await?;
        Ok(match self.find_row(project_id, environment_id).await? {
            Some(row) => ProjectIpAccessResponse::from(row),
            None => ProjectIpAccessResponse::unconfigured(project_id, environment_id),
        })
    }

    /// Validate and store the complete policy for one scope.
    ///
    /// `caller_ip` is the address the management request came from; when it
    /// would be locked out by the new policy and `acknowledge_lockout` is
    /// false the write is refused.
    pub async fn set(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        request: SetProjectIpAccessRequest,
        caller_ip: Option<IpAddr>,
        updated_by: Option<i32>,
    ) -> Result<ProjectIpAccessResponse, ProjectIpAccessError> {
        let scope = scope_label(project_id, environment_id);
        let policy = validate_policy(&scope, request.mode, &request.cidrs)?;

        if !request.acknowledge_lockout {
            if let Some(caller_ip) = caller_ip {
                let effect = match policy.mode {
                    IpAccessMode::Allow
                        if !evaluate_with_set(IpAccessMode::Allow, &policy.networks, caller_ip) =>
                    {
                        Some("exclude")
                    }
                    IpAccessMode::Deny
                        if !evaluate_with_set(IpAccessMode::Deny, &policy.networks, caller_ip) =>
                    {
                        Some("block")
                    }
                    _ => None,
                };
                if let Some(effect) = effect {
                    return Err(ProjectIpAccessError::WouldLockOutCaller {
                        scope,
                        caller_ip: normalize_ip(caller_ip).to_string(),
                        effect,
                    });
                }
            }
        }

        self.ensure_scope_exists(project_id, environment_id).await?;

        let cidrs = IpAccessCidrs(policy.entries);
        // Two writers creating the same scope at once both see no row and both
        // insert; the unique index rejects the second. That writer retries
        // once, finds the row the first one made, and updates it.
        let saved = match self
            .upsert(project_id, environment_id, policy.mode, &cidrs, updated_by)
            .await
        {
            Err(ProjectIpAccessError::ConcurrentUpdate { .. }) => {
                self.upsert(project_id, environment_id, policy.mode, &cidrs, updated_by)
                    .await
            }
            other => other,
        }
        .map_err(|error| match error {
            ProjectIpAccessError::ConcurrentUpdate { .. } => {
                ProjectIpAccessError::ConcurrentUpdate {
                    scope: scope.clone(),
                }
            }
            other => other,
        })?;

        Ok(ProjectIpAccessResponse::from(saved))
    }

    /// Insert or update the single row for a scope inside one transaction.
    async fn upsert(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        mode: IpAccessMode,
        cidrs: &IpAccessCidrs,
        updated_by: Option<i32>,
    ) -> Result<project_ip_access_rules::Model, ProjectIpAccessError> {
        let txn = self.db.begin().await?;
        let mut query = project_ip_access_rules::Entity::find()
            .filter(project_ip_access_rules::Column::ProjectId.eq(project_id));
        query = match environment_id {
            Some(environment_id) => {
                query.filter(project_ip_access_rules::Column::EnvironmentId.eq(environment_id))
            }
            None => query.filter(project_ip_access_rules::Column::EnvironmentId.is_null()),
        };
        let existing = query.one(&txn).await?;

        let saved = match existing {
            Some(row) => {
                let mut active: project_ip_access_rules::ActiveModel = row.into();
                active.mode = Set(mode.as_str().to_string());
                active.cidrs = Set(cidrs.clone());
                active.updated_by = Set(updated_by);
                active.update(&txn).await?
            }
            None => {
                project_ip_access_rules::ActiveModel {
                    project_id: Set(project_id),
                    environment_id: Set(environment_id),
                    mode: Set(mode.as_str().to_string()),
                    cidrs: Set(cidrs.clone()),
                    updated_by: Set(updated_by),
                    ..Default::default()
                }
                .insert(&txn)
                .await?
            }
        };
        txn.commit().await?;
        Ok(saved)
    }

    /// Remove one scope's policy. For an environment this returns it to
    /// inheriting the project-wide policy.
    pub async fn delete(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<(), ProjectIpAccessError> {
        self.ensure_scope_exists(project_id, environment_id).await?;
        let row = self.find_row(project_id, environment_id).await?;
        let Some(row) = row else {
            return Err(ProjectIpAccessError::PolicyNotFound {
                scope: scope_label(project_id, environment_id),
            });
        };
        project_ip_access_rules::Entity::delete_by_id(row.id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Evaluate `request.ip` against an unsaved draft, or against the policy
    /// that governs the scope (its own, else the project's).
    pub async fn test(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        request: TestProjectIpAccessRequest,
    ) -> Result<TestProjectIpAccessResponse, ProjectIpAccessError> {
        let scope = scope_label(project_id, environment_id);
        let ip = request.ip.trim().parse::<IpAddr>().map_err(|_| {
            ProjectIpAccessError::InvalidIpAddress {
                input: request.ip.chars().take(64).collect(),
            }
        })?;
        let ip = normalize_ip(ip);

        self.ensure_scope_exists(project_id, environment_id).await?;

        // `fail_closed` explains a saved policy the proxy cannot read: the
        // proxy then refuses every address, so the preview must too.
        let mut fail_closed: Option<String> = None;
        let (source, mode, entries, networks) = match (request.mode, request.cidrs) {
            (Some(mode), Some(cidrs)) => {
                // A draft may be an empty allow list while being edited; the
                // preview then correctly shows everything blocked.
                let policy = if mode == IpAccessMode::Allow && cidrs.is_empty() {
                    ValidatedPolicy {
                        mode,
                        entries: Vec::new(),
                        networks: Vec::new(),
                    }
                } else {
                    validate_policy(&scope, mode, &cidrs)?
                };
                (PolicySource::Draft, mode, policy.entries, policy.networks)
            }
            (Some(_), None) => {
                return Err(ProjectIpAccessError::IncompleteDraft {
                    scope,
                    missing: "cidrs",
                });
            }
            (None, Some(_)) => {
                return Err(ProjectIpAccessError::IncompleteDraft {
                    scope,
                    missing: "mode",
                });
            }
            (None, None) => {
                let own = match environment_id {
                    Some(_) => self.find_row(project_id, environment_id).await?,
                    None => None,
                };
                let (source, row) = match own {
                    Some(row) => (PolicySource::Environment, Some(row)),
                    None => (
                        PolicySource::Project,
                        self.find_row(project_id, None).await?,
                    ),
                };
                match row {
                    Some(row) => {
                        let stored_mode = IpAccessMode::parse(&row.mode);
                        let total = row.cidrs.0.len();
                        let (entries, networks) = aligned_policy(row.cidrs.0);
                        let unreadable = total - entries.len();
                        match stored_mode {
                            // Same rules as the proxy's compile step: an
                            // unknown mode, or a deny list with an entry that
                            // no longer parses, refuses everyone.
                            None => {
                                fail_closed = Some(format!(
                                    "Blocked: the stored mode '{}' is not recognised, so every \
                                     address is refused until the policy is saved again.",
                                    row.mode
                                ));
                                (source, IpAccessMode::Allow, Vec::new(), Vec::new())
                            }
                            Some(IpAccessMode::Deny) if unreadable > 0 => {
                                fail_closed = Some(format!(
                                    "Blocked: the stored deny list has {unreadable} unreadable \
                                     entr{}, so every address is refused until the policy is \
                                     saved again.",
                                    if unreadable == 1 { "y" } else { "ies" }
                                ));
                                (source, IpAccessMode::Allow, Vec::new(), Vec::new())
                            }
                            Some(mode) => (source, mode, entries, networks),
                        }
                    }
                    None => (
                        PolicySource::None,
                        IpAccessMode::Disabled,
                        Vec::new(),
                        Vec::new(),
                    ),
                }
            }
        };

        // A deny list that names nothing, and a disabled policy, enforce nothing.
        let source = if mode == IpAccessMode::Disabled {
            if source == PolicySource::Draft {
                PolicySource::Draft
            } else {
                PolicySource::None
            }
        } else {
            source
        };

        let (allowed, matched_cidr) = evaluate(mode, &entries, &networks, ip);
        let reason = match (mode, &matched_cidr) {
            (IpAccessMode::Allow, _) if fail_closed.is_some() => fail_closed.unwrap_or_default(),
            (IpAccessMode::Disabled, _) => {
                "No IP restriction applies, so every address is allowed.".to_string()
            }
            (IpAccessMode::Allow, Some(cidr)) => {
                format!("Allowed: {ip} is inside {cidr} on the allow list.")
            }
            (IpAccessMode::Allow, None) => {
                format!("Blocked: {ip} is not inside any CIDR on the allow list.")
            }
            (IpAccessMode::Deny, Some(cidr)) => {
                format!("Blocked: {ip} is inside {cidr} on the deny list.")
            }
            (IpAccessMode::Deny, None) => {
                format!("Allowed: {ip} is not inside any CIDR on the deny list.")
            }
        };

        Ok(TestProjectIpAccessResponse {
            ip: ip.to_string(),
            allowed,
            source,
            mode,
            matched_cidr,
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn entry(cidr: &str) -> IpAccessCidrEntry {
        IpAccessCidrEntry {
            cidr: cidr.to_string(),
            description: None,
        }
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test IP is valid")
    }

    /// What the existence check selects: a single `id` column.
    fn id_row(id: i32) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        std::collections::BTreeMap::from([("id", sea_orm::Value::Int(Some(id)))])
    }

    fn no_rows() -> Vec<std::collections::BTreeMap<&'static str, sea_orm::Value>> {
        Vec::new()
    }

    // ---- pure validation ----------------------------------------------

    #[test]
    fn validate_canonicalises_and_keeps_descriptions() {
        let policy = validate_policy(
            "project 1",
            IpAccessMode::Allow,
            &[
                IpAccessCidrEntry {
                    cidr: " 203.0.113.7 ".into(),
                    description: Some("  Office  ".into()),
                },
                entry("::ffff:10.0.0.0/104"),
            ],
        )
        .expect("valid policy");
        assert_eq!(policy.entries[0].cidr, "203.0.113.7/32");
        assert_eq!(policy.entries[0].description.as_deref(), Some("Office"));
        assert_eq!(policy.entries[1].cidr, "10.0.0.0/8");
    }

    #[test]
    fn validate_rejects_empty_allow_list_with_scope_in_message() {
        let err = validate_policy("project 42", IpAccessMode::Allow, &[]).unwrap_err();
        assert!(matches!(err, ProjectIpAccessError::EmptyAllowList { .. }));
        assert!(err.to_string().contains("project 42"));
    }

    #[test]
    fn validate_allows_empty_deny_and_disabled_lists() {
        assert!(validate_policy("project 1", IpAccessMode::Deny, &[]).is_ok());
        assert!(validate_policy("project 1", IpAccessMode::Disabled, &[]).is_ok());
    }

    #[test]
    fn validate_rejects_host_bits_and_garbage() {
        let err =
            validate_policy("project 1", IpAccessMode::Deny, &[entry("10.0.0.5/8")]).unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::InvalidCidr {
                source: CidrError::HostBitsSet { .. },
                ..
            }
        ));
        assert!(err.to_string().contains("10.0.0.0/8"));

        let err = validate_policy("project 1", IpAccessMode::Deny, &[entry("nope")]).unwrap_err();
        assert!(matches!(err, ProjectIpAccessError::InvalidCidr { .. }));
    }

    #[test]
    fn validate_rejects_duplicates_after_canonicalisation() {
        let err = validate_policy(
            "project 1",
            IpAccessMode::Deny,
            &[entry("203.0.113.7"), entry("203.0.113.7/32")],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::DuplicateCidr { ref cidr, .. } if cidr == "203.0.113.7/32"
        ));
    }

    #[test]
    fn validate_enforces_the_entry_limit() {
        let at_limit: Vec<_> = (0..MAX_CIDRS_PER_POLICY)
            .map(|i| entry(&format!("10.{}.{}.0/24", i / 256, i % 256)))
            .collect();
        assert!(validate_policy("project 1", IpAccessMode::Deny, &at_limit).is_ok());

        let mut over = at_limit;
        over.push(entry("11.0.0.0/8"));
        let err = validate_policy("project 1", IpAccessMode::Deny, &over).unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::TooManyCidrs {
                count: 257,
                max: 256,
                ..
            }
        ));
    }

    #[test]
    fn validate_enforces_description_length() {
        let err = validate_policy(
            "project 1",
            IpAccessMode::Deny,
            &[IpAccessCidrEntry {
                cidr: "10.0.0.0/8".into(),
                description: Some("x".repeat(MAX_DESCRIPTION_LEN + 1)),
            }],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::DescriptionTooLong { .. }
        ));
    }

    #[test]
    fn evaluate_reports_the_matching_cidr() {
        let policy = validate_policy(
            "project 1",
            IpAccessMode::Allow,
            &[entry("100.64.0.0/10"), entry("fd7a:115c:a1e0::/48")],
        )
        .expect("valid");
        let (allowed, matched) = evaluate(
            policy.mode,
            &policy.entries,
            &policy.networks,
            ip("100.100.1.1"),
        );
        assert!(allowed);
        assert_eq!(matched.as_deref(), Some("100.64.0.0/10"));

        let (allowed, matched) = evaluate(
            policy.mode,
            &policy.entries,
            &policy.networks,
            ip("::ffff:8.8.8.8"),
        );
        assert!(!allowed);
        assert_eq!(matched, None);
    }

    #[test]
    fn service_and_gate_matchers_agree() {
        let policy = validate_policy(
            "project 1",
            IpAccessMode::Allow,
            &[
                entry("10.0.0.0/8"),
                entry("2001:db8::/32"),
                entry("0.0.0.0/0"),
            ],
        )
        .expect("valid");
        for candidate in [
            "10.1.1.1",
            "8.8.8.8",
            "2001:db8::1",
            "2001:db9::1",
            "::ffff:10.1.1.1",
            "::1",
        ] {
            let (service, _) = evaluate(
                policy.mode,
                &policy.entries,
                &policy.networks,
                ip(candidate),
            );
            assert_eq!(
                service,
                evaluate_with_set(policy.mode, &policy.networks, ip(candidate)),
                "matchers disagree for {candidate}"
            );
        }
    }

    // ---- service ------------------------------------------------------

    fn rule_row(
        project_id: i32,
        environment_id: Option<i32>,
        mode: &str,
        cidrs: &[&str],
    ) -> project_ip_access_rules::Model {
        let now = chrono::Utc::now();
        project_ip_access_rules::Model {
            id: 1,
            project_id,
            environment_id,
            mode: mode.into(),
            cidrs: IpAccessCidrs(cidrs.iter().map(|c| entry(c)).collect()),
            updated_by: None,
            revision: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn get_unknown_project_is_not_found_with_the_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![no_rows()])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service.get(999, None).await.unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::ProjectNotFound { project_id: 999 }
        ));
        assert!(err.to_string().contains("999"));
    }

    #[tokio::test]
    async fn get_returns_unconfigured_placeholder_when_no_row_exists() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service.get(5, None).await.expect("get succeeds");
        assert!(!response.configured);
        assert_eq!(response.mode, IpAccessMode::Disabled);
        assert!(response.cidrs.is_empty());
        assert_eq!(response.max_cidrs, MAX_CIDRS_PER_POLICY);
    }

    #[tokio::test]
    async fn get_returns_the_stored_policy() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![rule_row(5, None, "allow", &["100.64.0.0/10"])]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service.get(5, None).await.expect("get succeeds");
        assert!(response.configured);
        assert_eq!(response.mode, IpAccessMode::Allow);
        assert_eq!(response.cidrs[0].cidr, "100.64.0.0/10");
    }

    #[tokio::test]
    async fn set_refuses_a_lockout_before_touching_the_database() {
        // No query results are queued: any DB access would error out.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .set(
                5,
                None,
                SetProjectIpAccessRequest {
                    mode: IpAccessMode::Allow,
                    cidrs: vec![entry("100.64.0.0/10")],
                    acknowledge_lockout: false,
                },
                Some(ip("203.0.113.9")),
                Some(1),
            )
            .await
            .unwrap_err();
        match err {
            ProjectIpAccessError::WouldLockOutCaller {
                ref caller_ip,
                effect,
                ..
            } => {
                assert_eq!(caller_ip, "203.0.113.9");
                assert_eq!(effect, "exclude");
            }
            other => panic!("expected WouldLockOutCaller, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_refuses_a_deny_list_that_blocks_the_caller() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .set(
                5,
                Some(9),
                SetProjectIpAccessRequest {
                    mode: IpAccessMode::Deny,
                    cidrs: vec![entry("203.0.113.0/24")],
                    acknowledge_lockout: false,
                },
                Some(ip("::ffff:203.0.113.9")),
                Some(1),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::WouldLockOutCaller {
                effect: "block",
                ..
            }
        ));
        assert!(err.to_string().contains("environment 9 of project 5"));
    }

    #[tokio::test]
    async fn set_reports_validation_errors_before_lockout_or_database() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .set(
                5,
                None,
                SetProjectIpAccessRequest {
                    mode: IpAccessMode::Allow,
                    cidrs: vec![],
                    acknowledge_lockout: false,
                },
                Some(ip("203.0.113.9")),
                Some(1),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectIpAccessError::EmptyAllowList { .. }));
    }

    #[tokio::test]
    async fn set_inserts_a_new_policy_when_the_caller_stays_in() {
        let inserted = rule_row(5, None, "allow", &["100.64.0.0/10"]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // ensure_scope_exists: project
            .append_query_results(vec![vec![id_row(5)]])
            // lookup of an existing row inside the transaction
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            // INSERT ... RETURNING
            .append_query_results(vec![vec![inserted]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .set(
                5,
                None,
                SetProjectIpAccessRequest {
                    mode: IpAccessMode::Allow,
                    cidrs: vec![entry("100.64.0.0/10")],
                    acknowledge_lockout: false,
                },
                Some(ip("100.100.1.1")),
                Some(1),
            )
            .await
            .expect("set succeeds");
        assert!(response.configured);
        assert_eq!(response.mode, IpAccessMode::Allow);
    }

    #[test]
    fn aligned_policy_drops_unparsable_entries_without_shifting_labels() {
        let (entries, networks) = aligned_policy(vec![
            entry("bogus"),
            entry("10.0.0.0/8"),
            entry("2001:db8::/32"),
        ]);
        assert_eq!(entries.len(), 2);
        assert_eq!(networks.len(), 2);

        let (allowed, matched) = evaluate(IpAccessMode::Allow, &entries, &networks, ip("10.1.2.3"));
        assert!(allowed);
        assert_eq!(matched.as_deref(), Some("10.0.0.0/8"));

        let (allowed, matched) =
            evaluate(IpAccessMode::Deny, &entries, &networks, ip("2001:db8::7"));
        assert!(!allowed);
        assert_eq!(matched.as_deref(), Some("2001:db8::/32"));
    }

    #[test]
    fn unique_violations_are_recognised_from_the_driver_message() {
        let duplicate = sea_orm::DbErr::Exec(sea_orm::RuntimeErr::Internal(
            "duplicate key value violates unique constraint \"project_ip_access_rules_scope_unique\""
                .to_string(),
        ));
        assert!(is_unique_violation(&duplicate));
        assert!(!is_unique_violation(&sea_orm::DbErr::Custom(
            "connection reset".to_string()
        )));
        assert!(matches!(
            ProjectIpAccessError::from(duplicate),
            ProjectIpAccessError::ConcurrentUpdate { .. }
        ));
    }

    #[tokio::test]
    async fn set_retries_once_when_another_writer_created_the_scope_first() {
        let duplicate = sea_orm::DbErr::Exec(sea_orm::RuntimeErr::Internal(
            "duplicate key value violates unique constraint \"project_ip_access_rules_scope_unique\""
                .to_string(),
        ));
        let stored = rule_row(5, None, "deny", &["198.51.100.0/24"]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // ensure_scope_exists: project
            .append_query_results(vec![vec![id_row(5)]])
            // first attempt: no row yet ...
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            // ... and the INSERT loses the race
            .append_query_errors(vec![duplicate])
            // retry: the winner's row is now visible
            .append_query_results(vec![vec![stored.clone()]])
            // UPDATE ... RETURNING
            .append_query_results(vec![vec![stored]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .set(
                5,
                None,
                SetProjectIpAccessRequest {
                    mode: IpAccessMode::Deny,
                    cidrs: vec![entry("198.51.100.0/24")],
                    acknowledge_lockout: true,
                },
                None,
                Some(1),
            )
            .await
            .expect("the retry updates the row the other writer created");
        assert!(response.configured);
        assert_eq!(response.mode, IpAccessMode::Deny);
    }

    #[tokio::test]
    async fn set_reports_a_repeated_unique_violation_as_a_conflict_with_the_scope() {
        let duplicate = || {
            sea_orm::DbErr::Exec(sea_orm::RuntimeErr::Internal(
                "duplicate key value violates unique constraint".to_string(),
            ))
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            // ensure_scope_exists: environment
            .append_query_results(vec![vec![id_row(9)]])
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            .append_query_errors(vec![duplicate()])
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            .append_query_errors(vec![duplicate()])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .set(
                5,
                Some(9),
                SetProjectIpAccessRequest {
                    mode: IpAccessMode::Deny,
                    cidrs: vec![entry("198.51.100.0/24")],
                    acknowledge_lockout: true,
                },
                None,
                None,
            )
            .await
            .unwrap_err();
        match err {
            ProjectIpAccessError::ConcurrentUpdate { scope } => {
                assert!(scope.contains("environment 9"), "{scope}");
            }
            other => panic!("expected ConcurrentUpdate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_surfaces_database_errors() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .set(
                5,
                None,
                SetProjectIpAccessRequest {
                    mode: IpAccessMode::Deny,
                    cidrs: vec![entry("198.51.100.0/24")],
                    acknowledge_lockout: true,
                },
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectIpAccessError::Database(_)));
        assert!(err.to_string().contains("connection reset"));
    }

    #[tokio::test]
    async fn delete_without_a_stored_policy_is_not_found_with_the_scope() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service.delete(5, None).await.unwrap_err();
        assert!(matches!(err, ProjectIpAccessError::PolicyNotFound { .. }));
        assert!(err.to_string().contains("project 5"));
    }

    #[tokio::test]
    async fn test_rejects_an_unparsable_address() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .test(
                5,
                None,
                TestProjectIpAccessRequest {
                    ip: "999.1.1.1".into(),
                    mode: None,
                    cidrs: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectIpAccessError::InvalidIpAddress { .. }));
    }

    #[tokio::test]
    async fn test_evaluates_an_unsaved_draft() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .test(
                5,
                None,
                TestProjectIpAccessRequest {
                    ip: "100.100.1.1".into(),
                    mode: Some(IpAccessMode::Allow),
                    cidrs: Some(vec![entry("100.64.0.0/10")]),
                },
            )
            .await
            .expect("test succeeds");
        assert!(response.allowed);
        assert_eq!(response.source, PolicySource::Draft);
        assert_eq!(response.matched_cidr.as_deref(), Some("100.64.0.0/10"));
    }

    #[tokio::test]
    async fn test_with_an_empty_allow_draft_blocks_everything_instead_of_erroring() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .test(
                5,
                None,
                TestProjectIpAccessRequest {
                    ip: "100.100.1.1".into(),
                    mode: Some(IpAccessMode::Allow),
                    cidrs: Some(vec![]),
                },
            )
            .await
            .expect("test succeeds");
        assert!(!response.allowed);
    }

    #[tokio::test]
    async fn test_without_any_policy_allows_and_says_so() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .test(
                5,
                None,
                TestProjectIpAccessRequest {
                    ip: "8.8.8.8".into(),
                    mode: None,
                    cidrs: None,
                },
            )
            .await
            .expect("test succeeds");
        assert!(response.allowed);
        assert_eq!(response.source, PolicySource::None);
    }

    fn probe(ip: &str) -> TestProjectIpAccessRequest {
        TestProjectIpAccessRequest {
            ip: ip.into(),
            mode: None,
            cidrs: None,
        }
    }

    #[tokio::test]
    async fn test_with_only_a_mode_is_rejected_not_answered_from_the_saved_list() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .test(
                5,
                None,
                TestProjectIpAccessRequest {
                    mode: Some(IpAccessMode::Deny),
                    ..probe("8.8.8.8")
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::IncompleteDraft {
                missing: "cidrs",
                ..
            }
        ));
        assert!(err.to_string().contains("project 5"));
    }

    #[tokio::test]
    async fn test_with_only_cidrs_is_rejected_not_answered_from_the_saved_list() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let err = service
            .test(
                5,
                None,
                TestProjectIpAccessRequest {
                    cidrs: Some(vec![entry("10.0.0.0/8")]),
                    ..probe("8.8.8.8")
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProjectIpAccessError::IncompleteDraft {
                missing: "mode",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_a_saved_deny_list_with_an_unreadable_entry_refuses_everyone_like_the_proxy() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![rule_row(
                5,
                None,
                "deny",
                &["10.0.0.0/8", "not-a-cidr"],
            )]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .test(5, None, probe("8.8.8.8"))
            .await
            .expect("test succeeds");
        assert!(!response.allowed);
        assert_eq!(response.source, PolicySource::Project);
        assert!(
            response.reason.contains("unreadable"),
            "{}",
            response.reason
        );
    }

    #[tokio::test]
    async fn test_a_saved_deny_list_of_only_unreadable_entries_refuses_everyone() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![rule_row(5, None, "deny", &["not-a-cidr"])]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .test(5, None, probe("8.8.8.8"))
            .await
            .expect("test succeeds");
        assert!(!response.allowed);
    }

    #[tokio::test]
    async fn test_a_saved_allow_list_still_skips_an_unreadable_entry() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![id_row(5)]])
            .append_query_results(vec![vec![rule_row(
                5,
                None,
                "allow",
                &["not-a-cidr", "10.0.0.0/8"],
            )]])
            .into_connection();
        let service = ProjectIpAccessService::new(Arc::new(db));

        let response = service
            .test(5, None, probe("10.1.2.3"))
            .await
            .expect("test succeeds");
        assert!(response.allowed);
        assert_eq!(response.matched_cidr.as_deref(), Some("10.0.0.0/8"));
    }
}
