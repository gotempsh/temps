// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal access grants on the wire (ADR 101).
//!
//! The control plane attaches to every internal route the callers from other
//! projects that the route's project granted access to; the worker's internal
//! proxy admits a caller of another project only when it matches one. The
//! list is additive: an agent or a control plane that does not know the field
//! sees an empty list, which admits nobody from another project.

use serde::{Deserialize, Serialize};

/// One caller scope a route admits: every environment of `project_id`, or only
/// `environment_id` of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InternalAccessCaller {
    pub project_id: i32,
    /// `None`: every environment of the project, present and future.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<i32>,
}

impl InternalAccessCaller {
    /// Whether a workload of `project_id` / `environment_id` is this caller.
    /// A workload whose environment is unknown matches only a project-wide
    /// entry.
    pub fn admits(&self, project_id: i32, environment_id: Option<i32>) -> bool {
        self.project_id == project_id
            && match self.environment_id {
                None => true,
                Some(granted) => environment_id == Some(granted),
            }
    }
}

/// One stored grant, as the control plane reads it to build caller lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InternalAccessGrantScope {
    pub target_project_id: i32,
    /// `None`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    pub caller: InternalAccessCaller,
}

impl InternalAccessGrantScope {
    /// Whether this grant covers a route of `project_id` / `environment_id`.
    /// A route whose environment is unknown is covered only by a
    /// project-wide grant.
    pub fn covers(&self, project_id: i32, environment_id: Option<i32>) -> bool {
        self.target_project_id == project_id
            && match self.target_environment_id {
                None => true,
                Some(target) => environment_id == Some(target),
            }
    }
}

/// The callers a route of `project_id` / `environment_id` admits from other
/// projects: deduplicated and sorted, so equal grant sets produce equal
/// snapshots. A grant naming the route's own project is dropped (the database
/// refuses one; same-project calls never need it).
pub fn callers_for_route(
    grants: &[InternalAccessGrantScope],
    project_id: i32,
    environment_id: Option<i32>,
) -> Vec<InternalAccessCaller> {
    let mut callers: Vec<InternalAccessCaller> = grants
        .iter()
        .filter(|grant| grant.covers(project_id, environment_id))
        .map(|grant| grant.caller)
        .filter(|caller| caller.project_id != project_id)
        .collect();
    callers.sort_unstable();
    callers.dedup();
    callers
}

/// Whether any entry of `callers` admits a workload of `project_id` /
/// `environment_id`.
pub fn permits(
    callers: &[InternalAccessCaller],
    project_id: i32,
    environment_id: Option<i32>,
) -> bool {
    callers
        .iter()
        .any(|caller| caller.admits(project_id, environment_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(target: (i32, Option<i32>), caller: (i32, Option<i32>)) -> InternalAccessGrantScope {
        InternalAccessGrantScope {
            target_project_id: target.0,
            target_environment_id: target.1,
            caller: InternalAccessCaller {
                project_id: caller.0,
                environment_id: caller.1,
            },
        }
    }

    #[test]
    fn a_project_wide_entry_admits_every_environment_of_the_caller() {
        let caller = InternalAccessCaller {
            project_id: 7,
            environment_id: None,
        };
        assert!(caller.admits(7, Some(70)));
        assert!(caller.admits(7, None));
        assert!(!caller.admits(8, Some(70)));
    }

    #[test]
    fn an_environment_entry_admits_only_that_environment() {
        let caller = InternalAccessCaller {
            project_id: 7,
            environment_id: Some(70),
        };
        assert!(caller.admits(7, Some(70)));
        assert!(!caller.admits(7, Some(71)));
        assert!(!caller.admits(7, None), "an unknown environment is not 70");
        assert!(!caller.admits(8, Some(70)));
    }

    #[test]
    fn route_callers_follow_the_target_scope() {
        let grants = [
            grant((1, None), (2, None)),
            grant((1, Some(10)), (3, Some(30))),
            grant((1, Some(11)), (4, None)),
            grant((5, None), (6, None)),
            // Duplicate scope through two grants.
            grant((1, Some(10)), (2, None)),
        ];
        assert_eq!(
            callers_for_route(&grants, 1, Some(10)),
            vec![
                InternalAccessCaller {
                    project_id: 2,
                    environment_id: None
                },
                InternalAccessCaller {
                    project_id: 3,
                    environment_id: Some(30)
                },
            ]
        );
        assert_eq!(
            callers_for_route(&grants, 1, Some(11)),
            vec![
                InternalAccessCaller {
                    project_id: 2,
                    environment_id: None
                },
                InternalAccessCaller {
                    project_id: 4,
                    environment_id: None
                },
            ]
        );
        // A route without an environment only gets project-wide grants.
        assert_eq!(
            callers_for_route(&grants, 1, None),
            vec![InternalAccessCaller {
                project_id: 2,
                environment_id: None
            }]
        );
        assert!(callers_for_route(&grants, 9, Some(90)).is_empty());
    }

    #[test]
    fn a_self_grant_never_reaches_a_route() {
        let grants = [grant((1, None), (1, None))];
        assert!(callers_for_route(&grants, 1, Some(10)).is_empty());
    }

    #[test]
    fn permits_requires_a_matching_entry() {
        let callers = [
            InternalAccessCaller {
                project_id: 2,
                environment_id: None,
            },
            InternalAccessCaller {
                project_id: 3,
                environment_id: Some(30),
            },
        ];
        assert!(permits(&callers, 2, Some(20)));
        assert!(permits(&callers, 3, Some(30)));
        assert!(!permits(&callers, 3, Some(31)));
        assert!(!permits(&callers, 4, Some(40)));
        assert!(!permits(&[], 2, Some(20)));
    }

    #[test]
    fn the_wire_form_omits_an_unset_environment() {
        let caller = InternalAccessCaller {
            project_id: 2,
            environment_id: None,
        };
        assert_eq!(
            serde_json::to_string(&caller).unwrap(),
            r#"{"project_id":2}"#
        );
        let parsed: InternalAccessCaller =
            serde_json::from_str(r#"{"project_id":3,"environment_id":30}"#).unwrap();
        assert_eq!(parsed.environment_id, Some(30));
    }
}
