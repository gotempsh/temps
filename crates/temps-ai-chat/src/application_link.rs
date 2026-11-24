// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Linking a project into an application workspace.
//!
//! A link is more than the `ai_application_projects` row: the caller needs
//! write access to every project the workspace will hold, the workspace tree
//! and the sandbox's per-project permissions must be prepared, and a running
//! workspace's data network must reach the new project's services. The HTTP
//! handler and the assistant's `temps_bind_blueprint` tool both link through
//! [`ApplicationProjectLinker`], so neither can skip a step.

use std::sync::Arc;

use sea_orm::{ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter};
use temps_auth::context::AuthContext;
use temps_auth::permissions::{Permission, Role};
use temps_entities::ai_conversations;
use temps_sandbox::error::SandboxError;
use temps_sandbox::SandboxService;
use tracing::error;

use crate::applications::{
    ApplicationError, ApplicationService, ApplicationWithProjects, ApplicationWorkspaceService,
};

/// Why a caller may not act on the projects of an application.
#[derive(Debug, thiserror::Error)]
pub enum ProjectPermissionError {
    #[error("this operation requires the {permission} permission")]
    MissingInstancePermission { permission: String },
    #[error("a user identity is required to act on the projects of an AI application")]
    NoUserIdentity,
    #[error("could not check project permissions of user {user_id}: {reason}")]
    PermissionCheck { user_id: i32, reason: String },
    #[error("could not check project membership of user {user_id}: {reason}")]
    MembershipCheck { user_id: i32, reason: String },
    #[error("the {permission} permission is required on project {project_id}")]
    Denied { project_id: i32, permission: String },
}

/// Whether a project grants `required`, given the team layer's answers.
/// `Some(Some(granted))` is a scoped project role; `Some(None)` means the
/// team layer has no per-project opinion, so membership decides; a project
/// missing from the answer is denied.
fn project_grants(
    permissions: Option<&Option<Vec<String>>>,
    member: Option<bool>,
    required: &str,
) -> bool {
    match permissions {
        Some(Some(granted)) => granted.iter().any(|permission| permission == required),
        Some(None) => member.unwrap_or(false),
        None => false,
    }
}

/// Enforce one permission across every listed project. Instance permissions
/// are the ceiling; a project role may narrow them but can never grant a
/// permission that the instance role does not hold.
pub(crate) async fn check_project_permission(
    auth: &AuthContext,
    checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
    project_ids: &[i32],
    required: &Permission,
) -> Result<(), ProjectPermissionError> {
    let required_name = required.to_string();
    if !auth.has_permission(required) {
        return Err(ProjectPermissionError::MissingInstancePermission {
            permission: required_name,
        });
    }
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(());
    }
    let Some(checker) = checker else {
        return Ok(());
    };
    let user_id = auth
        .user_id_opt()
        .ok_or(ProjectPermissionError::NoUserIdentity)?;
    let permissions = checker
        .effective_project_permissions_batch(user_id, project_ids)
        .await
        .map_err(|error| ProjectPermissionError::PermissionCheck {
            user_id,
            reason: error.to_string(),
        })?;
    let membership = checker
        .user_can_access_projects(user_id, project_ids)
        .await
        .map_err(|error| ProjectPermissionError::MembershipCheck {
            user_id,
            reason: error.to_string(),
        })?;
    for project_id in project_ids {
        if !project_grants(
            permissions.get(project_id),
            membership.get(project_id).copied(),
            &required_name,
        ) {
            return Err(ProjectPermissionError::Denied {
                project_id: *project_id,
                permission: required_name,
            });
        }
    }
    Ok(())
}

/// Why the runtime-update gate of a workspace could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeLockError {
    #[error(
        "workspace '{application_id}' is busy: a turn is running in it or another update holds it"
    )]
    Busy { application_id: String },
    #[error("could not check the running turns of workspace '{application_id}': {source}")]
    Database {
        application_id: String,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl From<RuntimeLockError> for crate::ChatError {
    fn from(error: RuntimeLockError) -> Self {
        match error {
            RuntimeLockError::Busy { application_id } => {
                crate::ChatError::WorkspaceRuntimeBusy { application_id }
            }
            RuntimeLockError::Database { source, .. } => crate::ChatError::from(source),
        }
    }
}

/// Take a workspace's runtime-update gate. Hold it across preflight,
/// replacement and settings persistence: a turn claim either finishes before
/// the gate (and is found here), or fails while an update owns it.
pub(crate) async fn lock_application_runtime(
    db: &DatabaseConnection,
    application_id: &str,
) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>, RuntimeLockError> {
    let busy = || RuntimeLockError::Busy {
        application_id: application_id.to_string(),
    };
    let guard = crate::service::application_runtime_gate(application_id)
        .try_write_owned()
        .map_err(|_| busy())?;
    let active = ai_conversations::Entity::find()
        .filter(ai_conversations::Column::ContextType.eq("application"))
        .filter(
            Condition::any()
                .add(ai_conversations::Column::ContextId.eq(application_id))
                .add(ai_conversations::Column::ContextId.starts_with(format!("{application_id}:"))),
        )
        .filter(ai_conversations::Column::TurnStatus.eq("running"))
        .one(db)
        .await
        .map_err(|source| RuntimeLockError::Database {
            application_id: application_id.to_string(),
            source,
        })?;
    if active.is_some() {
        return Err(busy());
    }
    Ok(guard)
}

/// A sandbox call made while synchronizing a workspace's data network.
#[derive(Debug, thiserror::Error)]
#[error("could not {operation} for workspace '{application_id}': {source}")]
pub struct NetworkSyncError {
    pub application_id: String,
    pub operation: &'static str,
    /// Boxed: a sandbox error is large, and this travels in `Result`s.
    #[source]
    pub source: Box<SandboxError>,
}

/// Whether a workspace in this state may keep its data-plane attachments.
pub(crate) fn workspace_may_retain_data_plane(status: &str) -> bool {
    matches!(status, "running" | "recovering")
}

/// Attach a running workspace to exactly the services of its linked
/// projects. A stopped workspace, or a server without sandboxes, has nothing
/// to synchronize.
pub(crate) async fn synchronize_application_network(
    sandboxes: Option<&Arc<SandboxService>>,
    user_id: i32,
    application: &ApplicationWithProjects,
) -> Result<(), NetworkSyncError> {
    let Some(sandboxes) = sandboxes else {
        return Ok(());
    };
    let application_id = &application.application.public_id;
    let failure = |operation: &'static str| {
        move |source: SandboxError| NetworkSyncError {
            application_id: application_id.clone(),
            operation,
            source: Box::new(source),
        }
    };
    let Some(summary) = sandboxes
        .application_workspace_summary(user_id, application_id)
        .await
        .map_err(failure("inspect before data-network synchronization"))?
        .filter(|summary| workspace_may_retain_data_plane(&summary.status))
    else {
        return Ok(());
    };
    let project_ids = application
        .projects
        .iter()
        .map(|project| project.id)
        .collect::<Vec<_>>();
    sandboxes
        .synchronize_application_data_network(
            user_id,
            application_id,
            &summary.public_id,
            &project_ids,
        )
        .await
        .map_err(failure("synchronize data network"))?;
    Ok(())
}

/// Why a project could not be linked into a workspace.
#[derive(Debug, thiserror::Error)]
pub enum LinkProjectError {
    #[error(transparent)]
    Permission(#[from] ProjectPermissionError),
    #[error(transparent)]
    RuntimeLock(#[from] RuntimeLockError),
    #[error(transparent)]
    Application(#[from] ApplicationError),
    #[error("could not set project {project_id}'s permissions in workspace '{application_id}': {source}")]
    ProjectPermissions {
        application_id: String,
        project_id: i32,
        /// Boxed for the same reason as [`NetworkSyncError::source`].
        #[source]
        source: Box<SandboxError>,
    },
    #[error(transparent)]
    Network(#[from] NetworkSyncError),
}

/// Links projects into application workspaces with every check and side
/// effect the link needs, and undoes the link when a later step fails.
#[derive(Clone)]
pub struct ApplicationProjectLinker {
    db: Arc<DatabaseConnection>,
    applications: Arc<ApplicationService>,
    workspaces: Arc<ApplicationWorkspaceService>,
    sandboxes: Option<Arc<SandboxService>>,
}

impl ApplicationProjectLinker {
    pub fn new(
        db: Arc<DatabaseConnection>,
        applications: Arc<ApplicationService>,
        workspaces: Arc<ApplicationWorkspaceService>,
        sandboxes: Option<Arc<SandboxService>>,
    ) -> Self {
        Self {
            db,
            applications,
            workspaces,
            sandboxes,
        }
    }

    /// The write permissions a caller needs on `project_id` and on every
    /// project `previous` already links before the link is made.
    pub async fn authorize(
        &self,
        auth: &AuthContext,
        checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
        previous: &ApplicationWithProjects,
        project_id: i32,
    ) -> Result<(), ProjectPermissionError> {
        let previous_project_ids = previous
            .projects
            .iter()
            .map(|project| project.id)
            .collect::<Vec<_>>();
        for project_ids in [&previous_project_ids[..], &[project_id][..]] {
            for required in [Permission::ProjectsWrite, Permission::SandboxesWrite] {
                check_project_permission(auth, checker, project_ids, &required).await?;
            }
        }
        Ok(())
    }

    /// Link `project_id` into the workspace `previous` describes: authorize,
    /// take the runtime-update gate, insert the link, prepare the workspace
    /// tree and the project's sandbox permissions, and attach a running
    /// workspace to the project's services. Any failure after the insert
    /// unlinks the project again and restores the previous data network.
    pub async fn link(
        &self,
        auth: &AuthContext,
        checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
        previous: &ApplicationWithProjects,
        project_id: i32,
    ) -> Result<ApplicationWithProjects, LinkProjectError> {
        self.authorize(auth, checker, previous, project_id).await?;
        let application_id = previous.application.public_id.clone();
        let user_id = auth.user_id();
        let _runtime_gate = lock_application_runtime(self.db.as_ref(), &application_id).await?;
        let application = self
            .applications
            .link_project(user_id, &application_id, project_id)
            .await?;
        if let Err(workspace_error) = self
            .workspaces
            .ensure(&application.application.public_id, &application.projects)
            .await
        {
            self.roll_back(
                user_id,
                &application_id,
                project_id,
                "workspace preparation",
            )
            .await;
            return Err(workspace_error.into());
        }
        if let (Some(project), Some(sandboxes)) = (
            application
                .projects
                .iter()
                .find(|project| project.id == project_id),
            self.sandboxes.as_ref(),
        ) {
            if let Err(source) = sandboxes
                .normalize_application_project_permissions(user_id, &application_id, &project.slug)
                .await
            {
                self.roll_back(
                    user_id,
                    &application_id,
                    project_id,
                    "workspace permission update",
                )
                .await;
                return Err(LinkProjectError::ProjectPermissions {
                    application_id,
                    project_id,
                    source: Box::new(source),
                });
            }
        }
        if let Err(network_error) =
            synchronize_application_network(self.sandboxes.as_ref(), user_id, &application).await
        {
            self.roll_back(
                user_id,
                &application_id,
                project_id,
                "database network update",
            )
            .await;
            // The database rollback alone is insufficient: network attachment
            // is a separate side effect. Restore the last authorized topology
            // before returning the failure.
            if let Err(reconcile_error) =
                synchronize_application_network(self.sandboxes.as_ref(), user_id, previous).await
            {
                error!(
                    application_id,
                    error = %reconcile_error,
                    "Failed to restore application data network after link rollback"
                );
            }
            return Err(network_error.into());
        }
        Ok(application)
    }

    async fn roll_back(&self, user_id: i32, application_id: &str, project_id: i32, step: &str) {
        if let Err(rollback_error) = self
            .applications
            .unlink_project(user_id, application_id, project_id)
            .await
        {
            error!(
                application_id,
                project_id,
                step,
                error = %rollback_error,
                "Failed to roll back project link after a later step failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::users;

    fn auth_with(permissions: Vec<Permission>) -> AuthContext {
        let now = chrono::Utc::now();
        let user = users::Model {
            id: 42,
            name: "Test User".to_string(),
            email: "user@example.test".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };
        AuthContext::new_api_key(user, None, Some(permissions), "test-key".to_string(), 1)
    }

    /// A team layer where the caller is a member of every project with the
    /// given role permissions (`None`: no per-project opinion).
    struct ProjectRole(Option<Vec<String>>);

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for ProjectRole {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(true)
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.0.clone())
        }
    }

    fn checker(role: Option<Vec<Permission>>) -> Arc<dyn temps_core::ProjectAccessChecker> {
        Arc::new(ProjectRole(role.map(|permissions| {
            permissions.iter().map(ToString::to_string).collect()
        })))
    }

    fn writer() -> AuthContext {
        auth_with(vec![
            Permission::ProjectsRead,
            Permission::ProjectsWrite,
            Permission::SandboxesWrite,
        ])
    }

    #[tokio::test]
    async fn a_project_viewer_may_not_link_the_project() {
        let viewer = checker(Some(vec![Permission::ProjectsRead]));
        let error =
            check_project_permission(&writer(), Some(&viewer), &[7], &Permission::ProjectsWrite)
                .await
                .expect_err("membership with a read-only project role must not allow a link");
        assert!(matches!(
            error,
            ProjectPermissionError::Denied { project_id: 7, ref permission }
                if permission == &Permission::ProjectsWrite.to_string()
        ));
    }

    #[tokio::test]
    async fn a_project_role_without_sandbox_write_may_not_link_the_project() {
        let editor = checker(Some(vec![
            Permission::ProjectsRead,
            Permission::ProjectsWrite,
        ]));
        let error =
            check_project_permission(&writer(), Some(&editor), &[7], &Permission::SandboxesWrite)
                .await
                .expect_err("project write alone must not put a project in a workspace sandbox");
        assert!(matches!(
            error,
            ProjectPermissionError::Denied { project_id: 7, .. }
        ));
    }

    #[tokio::test]
    async fn a_project_role_with_both_writes_may_link_the_project() {
        let maintainer = checker(Some(vec![
            Permission::ProjectsWrite,
            Permission::SandboxesWrite,
        ]));
        for required in [Permission::ProjectsWrite, Permission::SandboxesWrite] {
            check_project_permission(&writer(), Some(&maintainer), &[7], &required)
                .await
                .expect("the project role grants both writes");
        }
    }

    #[tokio::test]
    async fn the_instance_role_caps_what_a_project_role_grants() {
        let maintainer = checker(Some(vec![
            Permission::ProjectsWrite,
            Permission::SandboxesWrite,
        ]));
        let reader = auth_with(vec![Permission::ProjectsRead, Permission::ProjectsWrite]);
        let error = check_project_permission(
            &reader,
            Some(&maintainer),
            &[7],
            &Permission::SandboxesWrite,
        )
        .await
        .expect_err("a project role cannot grant what the instance role lacks");
        assert!(matches!(
            error,
            ProjectPermissionError::MissingInstancePermission { .. }
        ));
    }

    #[tokio::test]
    async fn membership_decides_without_a_scoped_project_role() {
        let member = checker(None);
        check_project_permission(&writer(), Some(&member), &[7], &Permission::ProjectsWrite)
            .await
            .expect("a member with no scoped role keeps coarse membership semantics");
    }

    #[test]
    fn a_project_missing_from_the_answer_is_denied() {
        assert!(!project_grants(None, Some(true), "projects:write"));
        assert!(!project_grants(Some(&None), Some(false), "projects:write"));
        assert!(!project_grants(Some(&None), None, "projects:write"));
        assert!(project_grants(Some(&None), Some(true), "projects:write"));
    }

    #[test]
    fn only_a_live_workspace_keeps_its_data_plane() {
        assert!(workspace_may_retain_data_plane("running"));
        assert!(workspace_may_retain_data_plane("recovering"));
        assert!(!workspace_may_retain_data_plane("stopped"));
        assert!(!workspace_may_retain_data_plane("failed"));
    }
}
