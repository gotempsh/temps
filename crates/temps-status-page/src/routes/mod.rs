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

mod audit;
pub mod public;
pub mod public_subscriptions;
pub mod status_page;
pub mod status_pages;
pub mod subscribers;

pub use status_page::*;
