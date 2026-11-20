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
        target_covers(
            self.target_project_id,
            self.target_environment_id,
            project_id,
            environment_id,
        )
    }
}

/// Whether a grant's target scope covers a route of `project_id` /
/// `environment_id`. A route whose environment is unknown is covered only by
/// a project-wide grant.
fn target_covers(
    target_project_id: i32,
    target_environment_id: Option<i32>,
    project_id: i32,
    environment_id: Option<i32>,
) -> bool {
    target_project_id == project_id
        && match target_environment_id {
            None => true,
            Some(target) => environment_id == Some(target),
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

/// A sandbox a route admits through a sandbox grant (ADR 105).
///
/// A snapshot lists only sandboxes that run on the node it is for, so a
/// container elsewhere that copied a sandbox's name matches nothing. The
/// internal proxy recognises the caller by its egress proxy, which is named
/// after the sandbox's container (`name`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InternalAccessSandboxCaller {
    /// The sandbox's container name, [`sandbox_container_name`] of its
    /// public id. Never `sandboxes.name`: that is the label the user gave it.
    pub name: String,
    /// The sandbox's public id, for log lines.
    pub public_id: String,
}

impl InternalAccessSandboxCaller {
    /// The caller for the sandbox with public id `public_id`.
    pub fn from_public_id(public_id: &str) -> Self {
        Self {
            name: sandbox_container_name(public_id),
            public_id: public_id.to_string(),
        }
    }
}

/// The prefix of every sandbox container's name.
pub const SANDBOX_CONTAINER_PREFIX: &str = "temps-sandbox-";
/// The prefix of a sandbox's public id.
pub const SANDBOX_PUBLIC_ID_PREFIX: &str = "sbx_";

/// The container a sandbox with public id `public_id` runs as. A sandbox
/// created through the sandboxes API (the only kind a grant can name) gets
/// its public id without the `sbx_` prefix as its container label, so
/// `sbx_<hex>` runs as `temps-sandbox-<hex>`.
pub fn sandbox_container_name(public_id: &str) -> String {
    let label = public_id
        .strip_prefix(SANDBOX_PUBLIC_ID_PREFIX)
        .unwrap_or(public_id);
    format!("{SANDBOX_CONTAINER_PREFIX}{label}")
}

/// The public id of the sandbox running as `container`, when the name has the
/// shape [`sandbox_container_name`] gives one (`temps-sandbox-` and 16
/// lowercase hex digits). An agent run's container (`temps-sandbox-<run id>`)
/// has none.
pub fn sandbox_public_id_for_container(container: &str) -> Option<String> {
    let label = container.strip_prefix(SANDBOX_CONTAINER_PREFIX)?;
    (label.len() == 16
        && label
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| format!("{SANDBOX_PUBLIC_ID_PREFIX}{label}"))
}

/// The internal DNS zone every internal address lives under.
pub const INTERNAL_ZONE: &str = "temps.local";

/// The registry owner kind of an environment's subdomain alias. It is the
/// weakest claim in the zone: any other owner that publishes the same name
/// (a managed service, a node, a static record) takes it over, and the alias
/// stops answering until the name is free again.
pub const ENVIRONMENT_ALIAS_OWNER_KIND: &str = "environment_subdomain";

/// `<environment subdomain>.temps.local`, the second internal address of an
/// environment besides `<environment>.<project>.temps.local`: its existing
/// unique subdomain (`shop-production`) under the internal zone. `None` when
/// the subdomain is not one DNS label (lower case letters, digits and inner
/// hyphens, at most 63), so it can never be a two-label address of another
/// environment or a name below one.
pub fn environment_alias_name(subdomain: &str) -> Option<String> {
    let label = subdomain.trim().to_ascii_lowercase();
    let valid = (1..=63).contains(&label.len())
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-');
    valid.then(|| format!("{label}.{INTERNAL_ZONE}"))
}

/// The label of a one-label internal name (`shop-production.temps.local`),
/// the form [`environment_alias_name`] gives.
pub fn environment_alias_label(name: &str) -> Option<String> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let label = name.strip_suffix(&format!(".{INTERNAL_ZONE}"))?;
    environment_alias_name(label).map(|_| label.to_string())
}

/// One stored sandbox grant, as the control plane reads it for a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalAccessSandboxGrantScope {
    pub target_project_id: i32,
    /// `None`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    pub sandbox: InternalAccessSandboxCaller,
}

impl InternalAccessSandboxGrantScope {
    /// Whether this grant covers a route of `project_id` / `environment_id`,
    /// with the same rule as a project grant.
    pub fn covers(&self, project_id: i32, environment_id: Option<i32>) -> bool {
        target_covers(
            self.target_project_id,
            self.target_environment_id,
            project_id,
            environment_id,
        )
    }
}

/// The sandboxes a route of `project_id` / `environment_id` admits:
/// deduplicated and sorted, so equal grant sets produce equal snapshots.
pub fn sandboxes_for_route(
    grants: &[InternalAccessSandboxGrantScope],
    project_id: i32,
    environment_id: Option<i32>,
) -> Vec<InternalAccessSandboxCaller> {
    let mut sandboxes: Vec<InternalAccessSandboxCaller> = grants
        .iter()
        .filter(|grant| grant.covers(project_id, environment_id))
        .map(|grant| grant.sandbox.clone())
        .collect();
    sandboxes.sort_unstable();
    sandboxes.dedup();
    sandboxes
}

/// The sandbox in `sandboxes` whose container is `name`, if any.
pub fn admitted_sandbox<'a>(
    sandboxes: &'a [InternalAccessSandboxCaller],
    name: &str,
) -> Option<&'a InternalAccessSandboxCaller> {
    sandboxes.iter().find(|sandbox| sandbox.name == name)
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

    fn sandbox_grant(target: (i32, Option<i32>), name: &str) -> InternalAccessSandboxGrantScope {
        InternalAccessSandboxGrantScope {
            target_project_id: target.0,
            target_environment_id: target.1,
            sandbox: InternalAccessSandboxCaller {
                name: name.to_string(),
                public_id: format!("sbx_{name}"),
            },
        }
    }

    #[test]
    fn route_sandboxes_follow_the_target_scope() {
        let grants = [
            sandbox_grant((1, None), "temps-sandbox-b"),
            sandbox_grant((1, Some(10)), "temps-sandbox-a"),
            sandbox_grant((1, Some(11)), "temps-sandbox-c"),
            sandbox_grant((2, None), "temps-sandbox-d"),
            // The same sandbox through two grants.
            sandbox_grant((1, Some(10)), "temps-sandbox-b"),
        ];
        let names = |project, environment| {
            sandboxes_for_route(&grants, project, environment)
                .into_iter()
                .map(|sandbox| sandbox.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(1, Some(10)), ["temps-sandbox-a", "temps-sandbox-b"]);
        assert_eq!(names(1, Some(11)), ["temps-sandbox-b", "temps-sandbox-c"]);
        assert_eq!(names(1, None), ["temps-sandbox-b"], "unknown environment");
        assert_eq!(names(3, Some(30)), Vec::<String>::new());
    }

    /// The container name comes from the public id, never from the label
    /// the user named the sandbox with, and maps back to the same id.
    #[test]
    fn a_sandbox_caller_is_named_after_its_public_ids_container() {
        let caller = InternalAccessSandboxCaller::from_public_id("sbx_0a1b2c3d4e5f6789");
        assert_eq!(caller.name, "temps-sandbox-0a1b2c3d4e5f6789");
        assert_eq!(caller.public_id, "sbx_0a1b2c3d4e5f6789");
        assert_eq!(
            sandbox_public_id_for_container(&caller.name).as_deref(),
            Some("sbx_0a1b2c3d4e5f6789")
        );
        for container in [
            "temps-sandbox-1000000",
            "temps-sandbox-0A1B2C3D4E5F6789",
            "temps-sandbox-0a1b2c3d4e5f67890",
            "temps-sandbox-",
            "other-0a1b2c3d4e5f6789",
        ] {
            assert_eq!(
                sandbox_public_id_for_container(container),
                None,
                "{container}"
            );
        }
    }

    /// The alias is the environment's subdomain as one label; anything that
    /// is not one label has no alias, so it cannot shadow another address.
    #[test]
    fn an_environment_alias_is_its_subdomain_as_one_label() {
        assert_eq!(
            environment_alias_name("orders-production").as_deref(),
            Some("orders-production.temps.local")
        );
        assert_eq!(
            environment_alias_name(" Shop-Production ").as_deref(),
            Some("shop-production.temps.local")
        );
        assert_eq!(
            environment_alias_name(&"a".repeat(63)).map(|n| n.len()),
            Some(63 + ".temps.local".len())
        );
        for subdomain in [
            "",
            "-shop",
            "shop-",
            "shop.production",
            "shop_production",
            "shop production",
            "señal",
            &"a".repeat(64),
        ] {
            assert_eq!(environment_alias_name(subdomain), None, "{subdomain:?}");
        }
        assert_eq!(
            environment_alias_label("Shop-Production.temps.local.").as_deref(),
            Some("shop-production")
        );
        for name in [
            "production.shop.temps.local",
            "temps.local",
            ".temps.local",
            "shop-production.temps.localhost",
            "shop-production.example.com",
        ] {
            assert_eq!(environment_alias_label(name), None, "{name}");
        }
    }

    #[test]
    fn a_sandbox_is_admitted_by_its_exact_container_name() {
        let listed = sandboxes_for_route(&[sandbox_grant((1, None), "temps-sandbox-a")], 1, None);
        assert!(admitted_sandbox(&listed, "temps-sandbox-a").is_some());
        assert!(admitted_sandbox(&listed, "temps-sandbox-a2").is_none());
        assert!(admitted_sandbox(&listed, "temps-sandbox-").is_none());
        assert!(admitted_sandbox(&[], "temps-sandbox-a").is_none());
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
