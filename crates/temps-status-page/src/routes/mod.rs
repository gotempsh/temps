// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Defined before the submodules so each of them can use it.
/// Authorize `$permission` on every project in `$project_ids` (the projects a
/// status page draws from), with ADR-028 semantics on each. A page without
/// components has no project to check against, so only instance
/// administrators may act on it.
macro_rules! page_projects_guard {
    ($auth:expr, $permission:ident, $project_ids:expr, $checker:expr) => {{
        let __project_ids: &[i32] = &$project_ids;
        if __project_ids.is_empty() && ($auth.is_deployment_token() || !$auth.is_instance_admin()) {
            return Err(temps_core::error_builder::forbidden()
                .title("Insufficient Permissions")
                .detail(
                    "This status page has no components left, so it belongs to no project; \
                     only an instance administrator can manage it",
                )
                .build());
        }
        for &__project_id in __project_ids {
            project_permission_guard!($auth, $permission, __project_id, $checker);
            project_scope_guard!($auth, __project_id);
        }
    }};
}

/// Instance permission, then the permission on every project of the page.
/// `$state` has `pages: Arc<PublicStatusPageService>` and
/// `project_access_checker`. Expands to an early return; evaluates to the
/// page's project ids.
macro_rules! page_access_guard {
    ($auth:expr, $state:expr, $permission:ident, $status_page_id:expr) => {{
        permission_guard!($auth, $permission);
        let project_ids = $state
            .pages
            .page_project_ids($status_page_id)
            .await
            .map_err($crate::routes::status_page::map_error)?;
        page_projects_guard!(
            $auth,
            $permission,
            project_ids,
            $state.project_access_checker.clone()
        );
        project_ids
    }};
}

mod audit;
pub mod maintenance;
pub mod page_password;
pub mod public;
pub mod public_subscriptions;
pub mod status_page;
pub mod status_pages;
pub mod subscribers;
pub mod webhooks;

pub use status_page::*;
