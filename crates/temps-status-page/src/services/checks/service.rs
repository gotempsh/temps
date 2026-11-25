// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Creating and changing monitors for targets outside Temps (ADR 108 D4).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QuerySelect, Set, TransactionTrait,
};
use temps_core::url_validation::{
    resolve_and_validate_domain, validate_outbound_ip, UrlValidationError,
};
use temps_entities::{projects, status_monitors};
use tracing::{info, warn};

/// How many times a monitor save is recomputed when another save lands
/// between its read and its write.
const UPDATE_ATTEMPTS: usize = 3;

use super::error::{CheckConfigError, TargetMonitorError};
use super::locations::CONTROL_PLANE_LOCATION;
use super::runner::TargetChecks;
use super::spec::{
    parse_target, validate_spec, CheckRequest, CheckSpec, MonitorCheckKind, StoredCheck, Target,
};
use super::types::{
    CreateTargetMonitorRequest, ProbeLocationView, ProbeLocationsResponse,
    UpdateTargetMonitorRequest,
};
use crate::services::interval_history::record_interval_change;
use crate::services::types::{normalize_check_interval_seconds, DEFAULT_CHECK_INTERVAL_SECS};

/// Target monitors one project may have. Each probes at most every 30 s.
pub const MAX_TARGET_MONITORS_PER_PROJECT: u64 = 50;
pub const MAX_NAME_CHARS: usize = 100;

pub struct TargetMonitorService {
    db: Arc<DatabaseConnection>,
    checks: Arc<TargetChecks>,
    /// Whether to resolve a target when it is saved, so a private address
    /// is refused at once instead of at the first probe. Off in tests that
    /// must not touch the network.
    check_addresses: bool,
}

fn config(project_id: i32) -> impl Fn(CheckConfigError) -> TargetMonitorError {
    move |source| TargetMonitorError::Config { project_id, source }
}

fn validate_name(project_id: i32, name: &str) -> Result<String, TargetMonitorError> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(TargetMonitorError::Config {
            project_id,
            source: CheckConfigError::Invalid {
                field: "name",
                reason: "a name is 1 to 100 printable characters".to_string(),
            },
        });
    }
    Ok(name.to_string())
}

/// How long a save waits for a target's address. A lookup that takes
/// longer is treated like a name that does not resolve now: accepted, since
/// every check looks the address up again and contacts only public ones.
const SAVE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Why `target` cannot be checked, if its address is not public. A name
/// that does not resolve now is accepted: the monitor reports it.
async fn address_problem(target: &Target) -> Option<String> {
    address_problem_within(target, SAVE_LOOKUP_TIMEOUT, |host, port| async move {
        resolve_and_validate_domain(&host, port).await
    })
    .await
}

/// [`address_problem`] with its deadline and lookup passed in, for tests.
pub(super) async fn address_problem_within<F, Fut>(
    target: &Target,
    limit: Duration,
    lookup: F,
) -> Option<String>
where
    F: FnOnce(String, u16) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<std::net::SocketAddr>, UrlValidationError>>,
{
    let (host, port) = match target {
        Target::Url(url) => (
            url.host_str()?.trim_matches(['[', ']']).to_string(),
            url.port_or_known_default()?,
        ),
        Target::HostPort { host, port } => (host.clone(), *port),
        Target::Name(_) => return None,
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return validate_outbound_ip(ip).err().map(|e| e.to_string());
    }
    match tokio::time::timeout(limit, lookup(host.clone(), port)).await {
        Ok(Ok(_)) | Ok(Err(UrlValidationError::DnsResolutionFailed(_))) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => {
            warn!(
                host = %host,
                "looking up {host} took over {} s while saving a monitor; saved, and each check looks it up again",
                limit.as_secs()
            );
            None
        }
    }
}

impl TargetMonitorService {
    pub fn new(db: Arc<DatabaseConnection>, checks: Arc<TargetChecks>) -> Self {
        Self {
            db,
            checks,
            check_addresses: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn without_address_checks(mut self) -> Self {
        self.check_addresses = false;
        self
    }

    async fn ensure_public(
        &self,
        project_id: i32,
        target: &Target,
    ) -> Result<(), TargetMonitorError> {
        if !self.check_addresses {
            return Ok(());
        }
        match address_problem(target).await {
            Some(reason) => Err(TargetMonitorError::NonPublicTarget {
                project_id,
                target: target.stored(),
                reason,
            }),
            None => Ok(()),
        }
    }

    fn encrypt(
        &self,
        project_id: i32,
        request: &CheckRequest,
    ) -> Result<Option<String>, TargetMonitorError> {
        if request.is_empty() {
            return Ok(None);
        }
        let json = serde_json::to_string(request).map_err(|e| TargetMonitorError::Encryption {
            project_id,
            reason: e.to_string(),
        })?;
        self.checks
            .encryption()
            .encrypt_string(&json)
            .map(Some)
            .map_err(|e| TargetMonitorError::Encryption {
                project_id,
                reason: e.to_string(),
            })
    }

    fn stored(spec: CheckSpec, request: &CheckRequest) -> serde_json::Value {
        let stored = StoredCheck {
            spec,
            header_names: request
                .headers
                .iter()
                .map(|h| h.name.trim().to_string())
                .collect(),
            has_body: request.body.as_deref().is_some_and(|b| !b.is_empty()),
        };
        // A struct of strings, numbers and enums always serialises.
        serde_json::to_value(stored).unwrap_or(serde_json::Value::Null)
    }

    pub async fn create(
        &self,
        project_id: i32,
        request: CreateTargetMonitorRequest,
    ) -> Result<status_monitors::Model, TargetMonitorError> {
        let name = validate_name(project_id, &request.name)?;
        let kind = request.check.kind();
        let target = parse_target(kind, &request.target).map_err(config(project_id))?;
        let check_request = request.request.unwrap_or_default();
        let spec = validate_spec(request.check, &check_request).map_err(config(project_id))?;
        self.ensure_public(project_id, &target).await?;
        let request_encrypted = self.encrypt(project_id, &check_request)?;

        let txn = self.db.begin().await?;
        // Locking the project row serializes creates against the limit.
        projects::Entity::find_by_id(project_id)
            .select_only()
            .column(projects::Column::Id)
            .lock_exclusive()
            .into_tuple::<i32>()
            .one(&txn)
            .await?
            .ok_or(TargetMonitorError::ProjectNotFound { project_id })?;
        let existing = status_monitors::Entity::find()
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .filter(status_monitors::Column::CheckKind.ne(MonitorCheckKind::Environment.as_str()))
            .count(&txn)
            .await?;
        if existing >= MAX_TARGET_MONITORS_PER_PROJECT {
            return Err(TargetMonitorError::TooMany {
                project_id,
                limit: MAX_TARGET_MONITORS_PER_PROJECT,
            });
        }
        let model = status_monitors::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(None),
            name: Set(name),
            monitor_type: Set(kind.as_str().to_string()),
            check_interval_seconds: Set(normalize_check_interval_seconds(
                request
                    .check_interval_seconds
                    .unwrap_or(DEFAULT_CHECK_INTERVAL_SECS),
            )),
            is_active: Set(true),
            is_managed: Set(false),
            // Due on the next scheduler sweep.
            next_check_at: Set(None),
            check_kind: Set(kind.as_str().to_string()),
            target: Set(Some(target.stored())),
            check_spec: Set(Some(Self::stored(spec, &check_request))),
            request_encrypted: Set(request_encrypted),
            probe_from_nodes: Set(request.probe_from_nodes),
            location_results: Set(None),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
        txn.commit().await?;
        info!(
            project_id,
            monitor_id = model.id,
            kind = kind.as_str(),
            "created a target monitor"
        );
        Ok(model)
    }

    /// Where a monitor checked from worker nodes would be checked from now.
    pub async fn probe_locations(&self) -> Result<ProbeLocationsResponse, TargetMonitorError> {
        let nodes = self.checks.locations().nodes().await?;
        let mut locations = vec![ProbeLocationView {
            name: CONTROL_PLANE_LOCATION.to_string(),
            node_id: None,
            region: None,
        }];
        locations.extend(nodes.into_iter().map(|node| ProbeLocationView {
            name: node.name,
            node_id: Some(node.node_id),
            region: node.region,
        }));
        let configured = locations.len() > 1;
        Ok(ProbeLocationsResponse {
            locations,
            configured,
            reason: (!configured).then(|| {
                "No worker node is online, so checks run from this server only. Add a worker \
                 node to check from a second place."
                    .to_string()
            }),
            setup_path: (!configured).then(|| "/settings/nodes".to_string()),
        })
    }

    /// Change a target monitor. Every field left out keeps its saved value,
    /// read in the same transaction that writes: a save computed from a row
    /// that changed meanwhile is recomputed, so a rename or pause never
    /// restores settings another request just saved.
    pub async fn update(
        &self,
        project_id: i32,
        monitor_id: i32,
        request: UpdateTargetMonitorRequest,
    ) -> Result<status_monitors::Model, TargetMonitorError> {
        for _ in 0..UPDATE_ATTEMPTS {
            let read = status_monitors::Entity::find_by_id(monitor_id)
                .filter(status_monitors::Column::ProjectId.eq(project_id))
                .one(self.db.as_ref())
                .await?
                .ok_or(TargetMonitorError::NotFound { monitor_id })?;
            if let Some(model) = self
                .update_from(project_id, monitor_id, &request, read)
                .await?
            {
                return Ok(model);
            }
        }
        Err(TargetMonitorError::Concurrent { monitor_id })
    }

    /// Apply `request` on top of `model`, as read earlier. `None` when the
    /// row changed since that read, and nothing was written.
    pub(super) async fn update_from(
        &self,
        project_id: i32,
        monitor_id: i32,
        request: &UpdateTargetMonitorRequest,
        model: status_monitors::Model,
    ) -> Result<Option<status_monitors::Model>, TargetMonitorError> {
        let request = request.clone();
        if MonitorCheckKind::from_stored(&model.check_kind) == MonitorCheckKind::Environment {
            return Err(TargetMonitorError::NotTargetMonitor { monitor_id });
        }
        let current = self.checks.decode(&model)?;

        let spec = request.check.unwrap_or_else(|| current.stored.spec.clone());
        let kind = spec.kind();
        let target = match request.target.as_deref() {
            Some(raw) => parse_target(kind, raw).map_err(config(project_id))?,
            None => parse_target(kind, &current.target.stored()).map_err(config(project_id))?,
        };
        let check_request = match (request.request, kind) {
            (Some(new), _) => new,
            (None, MonitorCheckKind::Http) => current.request.clone(),
            (None, _) => CheckRequest::default(),
        };
        let spec = validate_spec(spec, &check_request).map_err(config(project_id))?;
        if target != current.target {
            self.ensure_public(project_id, &target).await?;
        }
        let request_encrypted = if check_request == current.request {
            model.request_encrypted.clone()
        } else {
            self.encrypt(project_id, &check_request)?
        };

        let read_at = model.updated_at;
        let mut active: status_monitors::ActiveModel = model.into();
        if let Some(name) = request.name.as_deref() {
            active.name = Set(validate_name(project_id, name)?);
        }
        if let Some(seconds) = request.check_interval_seconds {
            active.check_interval_seconds = Set(normalize_check_interval_seconds(seconds));
        }
        if let Some(is_active) = request.is_active {
            active.is_active = Set(is_active);
        }
        active.monitor_type = Set(kind.as_str().to_string());
        active.check_kind = Set(kind.as_str().to_string());
        active.target = Set(Some(target.stored()));
        active.check_spec = Set(Some(Self::stored(spec, &check_request)));
        active.request_encrypted = Set(request_encrypted);
        if let Some(probe_from_nodes) = request.probe_from_nodes {
            active.probe_from_nodes = Set(probe_from_nodes);
        }
        // The last answers were for the old settings.
        active.location_results = Set(None);
        // Probe the new settings on the next sweep. Saving also moves
        // `updated_at`, which discards a probe of the old settings still in
        // flight.
        active.next_check_at = Set(None);
        // The settings above were computed without a lock (checking a new
        // target resolves it); write them only if the row is still the one
        // they were computed from.
        let txn = self.db.begin().await?;
        let locked = status_monitors::Entity::find_by_id(monitor_id)
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(TargetMonitorError::NotFound { monitor_id })?;
        if locked.updated_at != read_at {
            return Ok(None);
        }
        // Past checks keep the interval they ran under, so coverage of a
        // window that has ended does not move with this change.
        if let Some(seconds) = request.check_interval_seconds {
            record_interval_change(&txn, monitor_id, locked.check_interval_seconds, seconds)
                .await?;
        }
        let model = active.update(&txn).await?;
        txn.commit().await?;
        info!(
            project_id,
            monitor_id,
            kind = kind.as_str(),
            "updated a target monitor"
        );
        Ok(Some(model))
    }
}
