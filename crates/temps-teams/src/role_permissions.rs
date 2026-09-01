// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-scoped permission mapping for the four fixed team roles
//! (`owner`/`admin`/`deployer`/`viewer`).
//!
//! This is the single place those labels acquire permission semantics.
//! A plugin can supply a different answer for an individual membership via
//! [`temps_core::MembershipPermissionResolver`], but the result is still
//! capped by the team's grant role resolved through this same mapping.
//!
//! Lives here rather than as a method on [`TeamRole`] because
//! `temps-entities` is a pure Sea-ORM data crate with no dependency on
//! `temps-auth`, where `Permission` is defined.

use temps_auth::permissions::Permission;
use temps_entities::TeamRole;

/// Read-only permissions across project-scoped resources. No instance-admin
/// surfaces (`Users*`, `System*`, `ApiKeys*`, `Settings*`, `Audit*`) — those
/// are never project-scoped, so they have no place in a project-role
/// permission set regardless of which role.
const VIEWER: &[Permission] = &[
    Permission::ProjectsRead,
    Permission::DeploymentsRead,
    Permission::DomainsRead,
    Permission::EnvironmentsRead,
    Permission::LogsRead,
    Permission::MetricsRead,
    Permission::AnalyticsRead,
    Permission::BackupsRead,
    Permission::CronsRead,
    Permission::ExternalServicesRead,
    Permission::FilesRead,
    Permission::PipelinesRead,
    Permission::ErrorTrackingRead,
    Permission::StatusPageRead,
    Permission::OtelRead,
    Permission::VulnerabilityScansRead,
    Permission::BlobRead,
    Permission::KvRead,
    Permission::StacksRead,
    Permission::SandboxesRead,
    Permission::SpeedInsightsRead,
    Permission::SessionMetricsRead,
    Permission::FunnelRead,
    Permission::DeploymentTokensRead,
];

/// `viewer` plus the ability to ship code and manage what shipping it
/// needs — deploy, and the env vars/pipelines that go with it. Cannot
/// delete resources, touch domains, or change project settings.
const DEPLOYER_ADDITIONS: &[Permission] = &[
    Permission::DeploymentsCreate,
    Permission::DeploymentsWrite,
    Permission::EnvironmentsCreate,
    Permission::EnvironmentsWrite,
    Permission::PipelinesCreate,
    Permission::PipelinesWrite,
    Permission::PipelinesExecute,
    Permission::StacksWrite,
];

/// `deployer` plus full operational control of a project — every
/// resource's write/create/delete variants and project settings — but
/// **not** the ability to destroy the project itself. Excludes
/// `ProjectsDelete`: the standard "project admin, not owner" split.
const ADMIN_ADDITIONS: &[Permission] = &[
    Permission::ProjectsWrite,
    Permission::DeploymentsDelete,
    Permission::DomainsCreate,
    Permission::DomainsWrite,
    Permission::DomainsDelete,
    Permission::EnvironmentsDelete,
    Permission::BackupsCreate,
    Permission::BackupsWrite,
    Permission::BackupsDelete,
    Permission::CronsCreate,
    Permission::CronsWrite,
    Permission::CronsDelete,
    Permission::ExternalServicesCreate,
    Permission::ExternalServicesWrite,
    Permission::ExternalServicesDelete,
    Permission::FilesWrite,
    Permission::FilesUpload,
    Permission::FilesDelete,
    Permission::ErrorTrackingWrite,
    Permission::ErrorTrackingCreate,
    Permission::StatusPageWrite,
    Permission::StatusPageCreate,
    Permission::StatusPageDelete,
    Permission::VulnerabilityScansWrite,
    Permission::VulnerabilityScansCreate,
    Permission::VulnerabilityScansDelete,
    Permission::BlobWrite,
    Permission::BlobDelete,
    Permission::KvWrite,
    Permission::KvDelete,
    Permission::StacksCreate,
    Permission::StacksDelete,
    Permission::SandboxesWrite,
    Permission::SandboxesExec,
    Permission::DeploymentTokensWrite,
    Permission::DeploymentTokensCreate,
    Permission::DeploymentTokensDelete,
    Permission::OtelWrite,
];

/// `admin` plus the one thing only an owner can do: delete the project.
const OWNER_ADDITIONS: &[Permission] = &[Permission::ProjectsDelete];

/// Project-scoped permission set for a fixed team role.
///
/// Sets are strictly monotonic — `viewer ⊆ deployer ⊆ admin ⊆ owner` — by
/// construction (each tier is the prior tier's `Vec` extended with that
/// tier's additions), not by separately-maintained lists that could drift
/// out of the subset relationship.
pub fn fixed_role_permissions(role: TeamRole) -> Vec<Permission> {
    let mut perms: Vec<Permission> = VIEWER.to_vec();
    if matches!(role, TeamRole::Deployer | TeamRole::Admin | TeamRole::Owner) {
        perms.extend_from_slice(DEPLOYER_ADDITIONS);
    }
    if matches!(role, TeamRole::Admin | TeamRole::Owner) {
        perms.extend_from_slice(ADMIN_ADDITIONS);
    }
    if role == TeamRole::Owner {
        perms.extend_from_slice(OWNER_ADDITIONS);
    }
    perms
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn as_set(role: TeamRole) -> HashSet<Permission> {
        fixed_role_permissions(role).into_iter().collect()
    }

    #[test]
    fn viewer_is_read_only() {
        let perms = as_set(TeamRole::Viewer);
        for p in &perms {
            let s = p.to_string();
            assert!(
                s.ends_with(":read"),
                "viewer permission set must be read-only, found {s}"
            );
        }
        assert!(!perms.contains(&Permission::DeploymentsCreate));
        assert!(!perms.contains(&Permission::DeploymentsWrite));
        assert!(!perms.contains(&Permission::ProjectsDelete));
        assert!(!perms.contains(&Permission::EnvironmentsWrite));
    }

    #[test]
    fn deployer_adds_deploy_not_delete() {
        let perms = as_set(TeamRole::Deployer);
        assert!(perms.contains(&Permission::DeploymentsCreate));
        assert!(perms.contains(&Permission::DeploymentsWrite));
        assert!(perms.contains(&Permission::EnvironmentsWrite));
        for p in &perms {
            let s = p.to_string();
            assert!(!s.ends_with(":delete"), "deployer must not hold {s}");
        }
        assert!(!perms.contains(&Permission::ProjectsWrite));
        assert!(!perms.contains(&Permission::ProjectsDelete));
    }

    #[test]
    fn admin_excludes_project_delete() {
        let perms = as_set(TeamRole::Admin);
        assert!(perms.contains(&Permission::DeploymentsDelete));
        assert!(perms.contains(&Permission::DomainsDelete));
        assert!(perms.contains(&Permission::ProjectsWrite));
        assert!(
            !perms.contains(&Permission::ProjectsDelete),
            "admin must not hold ProjectsDelete — that's owner-only"
        );
    }

    #[test]
    fn owner_includes_project_delete() {
        assert!(as_set(TeamRole::Owner).contains(&Permission::ProjectsDelete));
    }

    #[test]
    fn permission_sets_are_monotonic() {
        let viewer = as_set(TeamRole::Viewer);
        let deployer = as_set(TeamRole::Deployer);
        let admin = as_set(TeamRole::Admin);
        let owner = as_set(TeamRole::Owner);

        assert!(viewer.is_subset(&deployer), "viewer ⊆ deployer must hold");
        assert!(deployer.is_subset(&admin), "deployer ⊆ admin must hold");
        assert!(admin.is_subset(&owner), "admin ⊆ owner must hold");

        // And strictly so — each tier actually adds something.
        assert!(viewer.len() < deployer.len());
        assert!(deployer.len() < admin.len());
        assert!(admin.len() < owner.len());
    }

    #[test]
    fn no_instance_admin_surfaces_leak_into_any_tier() {
        for role in [
            TeamRole::Viewer,
            TeamRole::Deployer,
            TeamRole::Admin,
            TeamRole::Owner,
        ] {
            for p in fixed_role_permissions(role) {
                let s = p.to_string();
                assert!(
                    !s.starts_with("users:")
                        && !s.starts_with("system:")
                        && !s.starts_with("api_keys:")
                        && !s.starts_with("settings:")
                        && !s.starts_with("audit:"),
                    "project role {role:?} must never hold instance-admin permission {s}"
                );
            }
        }
    }
}
