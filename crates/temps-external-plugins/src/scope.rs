// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a plugin that may write is allowed to change: which areas of the
//! platform, and in which projects.
//!
//! `api_write` alone changes nothing. An administrator names the areas, and
//! may limit them to some projects. This module decides, for one call,
//! whether it falls inside that scope and says why when it does not.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Most projects one grant may be limited to.
pub const MAX_WRITE_PROJECTS: usize = 100;

/// An area of the platform a plugin may be allowed to change.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PluginWriteArea {
    /// Create, change or delete projects.
    Projects,
    /// Create, change or delete environments.
    Environments,
    /// Start, change or cancel deployments.
    Deployments,
}

impl PluginWriteArea {
    pub const ALL: [Self; 3] = [Self::Projects, Self::Environments, Self::Deployments];

    /// The value stored on the grant, which is also the first segment of the
    /// API paths the area covers.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::Environments => "environments",
            Self::Deployments => "deployments",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|area| area.as_str() == value)
    }
}

/// What a changing call is aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteTarget {
    /// The collection of an area: creating something new.
    Collection(PluginWriteArea),
    /// One existing record of an area.
    Record(PluginWriteArea, i32),
}

impl WriteTarget {
    /// The target of a call to `path` (below `/api`), when the path is one a
    /// plugin may delegate at all.
    pub fn of_path(path: &str) -> Option<Self> {
        let mut segments = path.split('/');
        if segments.next() != Some("") {
            return None;
        }
        let area = PluginWriteArea::from_stored(segments.next()?)?;
        let target = match segments.next() {
            None => Self::Collection(area),
            Some(id) => Self::Record(area, id.parse().ok().filter(|id| *id > 0)?),
        };
        segments.next().is_none().then_some(target)
    }

    pub fn area(self) -> PluginWriteArea {
        match self {
            Self::Collection(area) | Self::Record(area, _) => area,
        }
    }
}

/// Why a changing call falls outside what a plugin may change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteDenied {
    #[error("Plugin '{plugin}' may not change {area}: an administrator has not allowed it to. Allowed: {allowed}")]
    Area {
        plugin: String,
        area: &'static str,
        allowed: String,
    },
    #[error("Plugin '{plugin}' may only change the projects it was limited to ({allowed}), so it cannot create a project")]
    NewProject { plugin: String, allowed: String },
    #[error("Plugin '{plugin}' may only change the projects it was limited to ({allowed}), and this call does not say which project it changes")]
    ProjectUnknown { plugin: String, allowed: String },
    #[error("Plugin '{plugin}' may only change the projects it was limited to ({allowed}), not project {project_id}")]
    ProjectOutside {
        plugin: String,
        project_id: i32,
        allowed: String,
    },
}

/// The project a changing call belongs to, as far as it can be known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetProject {
    /// The call changes this project or something in it.
    Known(i32),
    /// The call creates a project, so it belongs to none yet.
    New,
    /// The call does not say, or names something that does not exist.
    Unknown,
}

/// What an administrator allowed a plugin to change.
#[derive(Debug, Clone, Copy)]
pub struct WriteScope<'a> {
    pub areas: &'a [PluginWriteArea],
    /// `None`: every project.
    pub projects: Option<&'a [i32]>,
}

impl WriteScope<'_> {
    fn areas_text(&self) -> String {
        if self.areas.is_empty() {
            return "nothing".to_string();
        }
        self.areas
            .iter()
            .map(|area| area.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Whether the area of `target` may be changed at all.
    pub fn allows_area(&self, plugin: &str, target: WriteTarget) -> Result<(), WriteDenied> {
        if self.areas.contains(&target.area()) {
            return Ok(());
        }
        Err(WriteDenied::Area {
            plugin: plugin.to_string(),
            area: target.area().as_str(),
            allowed: self.areas_text(),
        })
    }

    /// Whether the scope is limited to some projects, so the project a call
    /// belongs to has to be found before it runs.
    pub fn limits_projects(&self) -> bool {
        self.projects.is_some()
    }

    /// Whether a call that belongs to `project` may run.
    pub fn allows_project(&self, plugin: &str, project: TargetProject) -> Result<(), WriteDenied> {
        let Some(projects) = self.projects else {
            return Ok(());
        };
        let allowed = || {
            projects
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        match project {
            TargetProject::Known(project_id) if projects.contains(&project_id) => Ok(()),
            TargetProject::Known(project_id) => Err(WriteDenied::ProjectOutside {
                plugin: plugin.to_string(),
                project_id,
                allowed: allowed(),
            }),
            TargetProject::New => Err(WriteDenied::NewProject {
                plugin: plugin.to_string(),
                allowed: allowed(),
            }),
            TargetProject::Unknown => Err(WriteDenied::ProjectUnknown {
                plugin: plugin.to_string(),
                allowed: allowed(),
            }),
        }
    }
}

/// A request body, read only for the project it names.
#[derive(Debug, Deserialize)]
pub struct NamedProject {
    project_id: Option<i64>,
}

impl NamedProject {
    /// The project the body names, when it names one with a positive id.
    pub fn id(&self) -> Option<i32> {
        self.project_id
            .and_then(|id| i32::try_from(id).ok())
            .filter(|id| *id > 0)
    }
}

/// The `project_id` a JSON request body names, when it names one.
pub fn project_id_in_body(json: &str) -> Option<i32> {
    serde_json::from_str::<NamedProject>(json).ok()?.id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_an_area_and_a_record_or_its_collection() {
        assert_eq!(
            WriteTarget::of_path("/projects"),
            Some(WriteTarget::Collection(PluginWriteArea::Projects))
        );
        assert_eq!(
            WriteTarget::of_path("/deployments/42"),
            Some(WriteTarget::Record(PluginWriteArea::Deployments, 42))
        );
        for path in [
            "",
            "/",
            "projects",
            "/settings",
            "/projects/",
            "/projects/abc",
            "/projects/0",
            "/projects/-3",
            "/projects/7/domains",
            "/projects/7/",
            "//projects",
        ] {
            assert_eq!(WriteTarget::of_path(path), None, "{path}");
        }
    }

    #[test]
    fn write_permission_without_an_area_changes_nothing() {
        let scope = WriteScope {
            areas: &[],
            projects: None,
        };
        let denied = scope
            .allows_area(
                "reports",
                WriteTarget::Collection(PluginWriteArea::Projects),
            )
            .expect_err("no area was allowed");
        assert_eq!(
            denied.to_string(),
            "Plugin 'reports' may not change projects: an administrator has not allowed it to. Allowed: nothing"
        );
    }

    #[test]
    fn only_the_allowed_areas_may_be_changed() {
        let scope = WriteScope {
            areas: &[PluginWriteArea::Deployments],
            projects: None,
        };
        assert!(scope
            .allows_area(
                "reports",
                WriteTarget::Record(PluginWriteArea::Deployments, 3)
            )
            .is_ok());
        let denied = scope
            .allows_area(
                "reports",
                WriteTarget::Record(PluginWriteArea::Environments, 3),
            )
            .expect_err("environments were not allowed");
        assert!(denied.to_string().contains("may not change environments"));
        assert!(denied.to_string().contains("Allowed: deployments"));
        // Without a project limit, any project may be changed.
        assert!(!scope.limits_projects());
        for project in [
            TargetProject::Known(1),
            TargetProject::New,
            TargetProject::Unknown,
        ] {
            assert!(scope.allows_project("reports", project).is_ok());
        }
    }

    #[test]
    fn a_project_limit_refuses_other_projects_new_projects_and_unnamed_ones() {
        let scope = WriteScope {
            areas: &PluginWriteArea::ALL,
            projects: Some(&[7, 9]),
        };
        assert!(scope.limits_projects());
        assert!(scope
            .allows_project("reports", TargetProject::Known(9))
            .is_ok());
        assert_eq!(
            scope.allows_project("reports", TargetProject::Known(8)),
            Err(WriteDenied::ProjectOutside {
                plugin: "reports".into(),
                project_id: 8,
                allowed: "7, 9".into(),
            })
        );
        assert!(matches!(
            scope.allows_project("reports", TargetProject::New),
            Err(WriteDenied::NewProject { .. })
        ));
        assert!(matches!(
            scope.allows_project("reports", TargetProject::Unknown),
            Err(WriteDenied::ProjectUnknown { .. })
        ));
        // An empty limit allows no project at all.
        let none = WriteScope {
            areas: &PluginWriteArea::ALL,
            projects: Some(&[]),
        };
        assert!(none
            .allows_project("reports", TargetProject::Known(7))
            .is_err());
    }

    #[test]
    fn a_body_names_its_project_only_with_a_positive_integer() {
        assert_eq!(
            project_id_in_body(r#"{"project_id": 7, "name": "x"}"#),
            Some(7)
        );
        for body in [
            "",
            "[]",
            "{}",
            r#"{"project_id": null}"#,
            r#"{"project_id": "7"}"#,
            r#"{"project_id": 0}"#,
            r#"{"project_id": -1}"#,
            r#"{"project_id": 99999999999}"#,
            r#"{"project_id": 7.5}"#,
        ] {
            assert_eq!(project_id_in_body(body), None, "{body}");
        }
    }

    #[test]
    fn an_area_round_trips_through_its_stored_value() {
        for area in PluginWriteArea::ALL {
            assert_eq!(PluginWriteArea::from_stored(area.as_str()), Some(area));
        }
        assert_eq!(PluginWriteArea::from_stored("settings"), None);
    }
}
