// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The pieces a chat has made: the projects, databases and services, and
//! repositories its confirmed write actions created, plus the sandbox linked to
//! it. The chat's right-hand canvas lists them so the user can see everything
//! the conversation put in place.
//!
//! Nothing new is recorded when a piece is created. Every write the assistant
//! performs already lands in `ai_pending_actions` with the operation that ran
//! and the response it returned, so the list is read back from the executed
//! rows and checked against the live tables. A piece that has since been
//! deleted drops out of the list, and one whose state changed shows its current
//! state rather than the state at creation time.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use temps_presets::stored_preset::StoredPresetExt;

use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temps_entities::{
    ai_conversations, ai_pending_actions, external_services, project_services, projects, sandboxes,
};
use thiserror::Error;
use utoipa::ToSchema;

use crate::sandbox_link::link_from_metadata;

/// Most creating actions read for one chat. A chat that made more than this
/// reports `truncated` and lists the newest ones.
pub const MAX_RESOURCES: u64 = 100;

const OP_CREATE_PROJECT: &str = "create_project";
const OP_CREATE_SERVICE: &str = "create_service";
const OP_CREATE_REPOSITORY: &str = "create_repository";
/// Attaching an existing service to a project. The service was not made by the
/// chat, but the chat put it to use, and the user wants to see the database
/// their app now runs on next to the app.
const OP_LINK_SERVICE: &str = "link_service_to_project";

/// Write operations whose executed result names a piece the chat made.
#[cfg(test)]
const CREATING_OPERATIONS: [&str; 3] = [OP_CREATE_PROJECT, OP_CREATE_SERVICE, OP_CREATE_REPOSITORY];

/// Every operation read back: the creating ones plus [`OP_LINK_SERVICE`].
const LISTED_OPERATIONS: [&str; 4] = [
    OP_CREATE_PROJECT,
    OP_CREATE_SERVICE,
    OP_CREATE_REPOSITORY,
    OP_LINK_SERVICE,
];

#[derive(Debug, Error)]
pub enum ChatResourceError {
    #[error("Failed to list the resources created by conversation {conversation_id}: {reason}")]
    Database {
        conversation_id: String,
        reason: String,
    },
}

/// What kind of piece a chat made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatResourceKind {
    /// A Temps project.
    Project,
    /// An external service: a database, cache, object store or similar.
    Service,
    /// A Git repository created through a connected provider.
    Repository,
    /// The sandbox linked to the chat.
    Sandbox,
}

/// One piece the chat made, with its current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChatResource {
    pub kind: ChatResourceKind,
    /// Numeric id for projects and services, `owner/name` for repositories,
    /// public id (`sbx_…`) for sandboxes.
    pub id: String,
    pub name: String,
    /// Project slug, for opening the project.
    pub slug: Option<String>,
    /// What it is: the framework preset, the service type, the repository's
    /// visibility, the sandbox backend.
    pub detail: Option<String>,
    /// Current status, for services and sandboxes.
    pub status: Option<String>,
    /// Where to see it outside Temps: a repository's web page.
    pub url: Option<String>,
    /// ISO 8601 time the piece was made (or, for a sandbox, linked).
    pub created_at: DateTime<Utc>,
}

/// Everything the chat made, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChatResourcesResponse {
    pub resources: Vec<ChatResource>,
    /// True when the chat made more pieces than this response covers.
    pub truncated: bool,
}

/// Which projects decide whether the caller may still see a listed piece.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Visibility {
    /// A project: shown only while the caller can read it.
    Project(i32),
    /// A service: shown while the caller can read at least one project it is
    /// linked to. A service linked to no project is an instance-level
    /// resource, governed by instance permissions rather than project
    /// membership, so it stays listed for the chat's owner as before.
    Service { linked_projects: Vec<i32> },
    /// Repositories (owned by the git provider) and the sandbox (owned by the
    /// user, checked when it is read): not scoped to a project.
    Unscoped,
}

/// The pieces a chat made, before they are narrowed to the projects the
/// caller can still read: the chat is theirs, but project membership can
/// change after the chat made something.
#[derive(Debug, Clone)]
pub struct ChatResourceListing {
    entries: Vec<(ChatResource, Visibility)>,
    truncated: bool,
}

impl ChatResourceListing {
    /// Every project whose access decides what is shown, sorted and unique,
    /// so the caller checks them in one batch.
    pub fn project_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self
            .entries
            .iter()
            .flat_map(|(_, visibility)| match visibility {
                Visibility::Project(id) => vec![*id],
                Visibility::Service { linked_projects } => linked_projects.clone(),
                Visibility::Unscoped => Vec::new(),
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// The response for a caller with `access` (project id to whether they
    /// may read it). `None` means no project restriction applies, as for an
    /// admin. A project missing from the map is treated as not readable.
    pub fn visible_to(self, access: Option<&BTreeMap<i32, bool>>) -> ChatResourcesResponse {
        let can_read = |project_id: &i32| {
            access.is_none_or(|access| access.get(project_id).copied().unwrap_or(false))
        };
        let resources = self
            .entries
            .into_iter()
            .filter(|(_, visibility)| match visibility {
                Visibility::Project(id) => can_read(id),
                Visibility::Service { linked_projects } => {
                    linked_projects.is_empty() || linked_projects.iter().any(can_read)
                }
                Visibility::Unscoped => true,
            })
            .map(|(resource, _)| resource)
            .collect();
        ChatResourcesResponse {
            resources,
            truncated: self.truncated,
        }
    }
}

/// What an executed action created, as far as its own record says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Created {
    Project {
        id: i32,
    },
    Service {
        id: i32,
        service_type: Option<String>,
    },
    Repository {
        full_name: String,
        name: String,
        private: Option<bool>,
        web_url: Option<String>,
    },
}

fn id_from(value: &Value) -> Option<i32> {
    i32::try_from(value.get("id")?.as_i64()?).ok()
}

fn text_from(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Read what an executed creating action made out of its recorded result.
/// `None` for any other operation and for a result with no usable identity.
fn created_by(operation_id: &str, params: &Value, result: Option<&Value>) -> Option<Created> {
    if operation_id == OP_LINK_SERVICE {
        // The approved parameters carry the service id; the result is only the
        // link itself.
        let service_id = id_from(params.get("__temps_display")?)?;
        return Some(Created::Service {
            id: service_id,
            service_type: None,
        });
    }
    let result = result?;
    match operation_id {
        OP_CREATE_PROJECT => Some(Created::Project {
            id: id_from(result)?,
        }),
        OP_CREATE_SERVICE => Some(Created::Service {
            id: id_from(result)?,
            service_type: params
                .get("__temps_display")
                .and_then(|display| text_from(display, "service_type")),
        }),
        OP_CREATE_REPOSITORY => {
            let name = text_from(result, "name")?;
            let full_name = text_from(result, "full_name")
                .or_else(|| Some(format!("{}/{}", text_from(result, "owner")?, name)))?;
            Some(Created::Repository {
                full_name,
                name,
                private: result.get("private").and_then(Value::as_bool),
                web_url: text_from(result, "web_url"),
            })
        }
        _ => None,
    }
}

/// Reads the pieces a conversation made.
pub struct ChatResourceService {
    db: Arc<DatabaseConnection>,
}

impl ChatResourceService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// The pieces `conversation` made that still exist, newest first. The
    /// caller has already shown the conversation is theirs; `user_id` limits the
    /// linked sandbox to one they own. Narrow the listing to the projects the
    /// caller may read with [`ChatResourceListing::visible_to`].
    pub async fn list(
        &self,
        conversation: &ai_conversations::Model,
        user_id: i32,
    ) -> Result<ChatResourceListing, ChatResourceError> {
        let database = |reason: sea_orm::DbErr| ChatResourceError::Database {
            conversation_id: conversation.public_id.clone(),
            reason: reason.to_string(),
        };

        let mut actions = ai_pending_actions::Entity::find()
            .filter(ai_pending_actions::Column::ConversationId.eq(conversation.id))
            .filter(ai_pending_actions::Column::Status.eq("executed"))
            .filter(ai_pending_actions::Column::OperationId.is_in(LISTED_OPERATIONS))
            .order_by(ai_pending_actions::Column::CreatedAt, Order::Desc)
            .order_by(ai_pending_actions::Column::Id, Order::Desc)
            .limit(MAX_RESOURCES + 1)
            .all(self.db.as_ref())
            .await
            .map_err(database)?;
        let truncated = actions.len() as u64 > MAX_RESOURCES;
        actions.truncate(MAX_RESOURCES as usize);

        let created: Vec<(Created, DateTime<Utc>)> = actions
            .iter()
            .filter_map(|action| {
                let made =
                    created_by(&action.operation_id, &action.params, action.result.as_ref())?;
                Some((made, action.executed_at.unwrap_or(action.created_at)))
            })
            .collect();

        let project_ids: Vec<i32> = created
            .iter()
            .filter_map(|(made, _)| match made {
                Created::Project { id } => Some(*id),
                _ => None,
            })
            .collect();
        let service_ids: Vec<i32> = created
            .iter()
            .filter_map(|(made, _)| match made {
                Created::Service { id, .. } => Some(*id),
                _ => None,
            })
            .collect();

        let live_projects: HashMap<i32, projects::Model> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            projects::Entity::find()
                .filter(projects::Column::Id.is_in(project_ids))
                .filter(projects::Column::IsDeleted.eq(false))
                .all(self.db.as_ref())
                .await
                .map_err(database)?
                .into_iter()
                .map(|project| (project.id, project))
                .collect()
        };
        let live_services: HashMap<i32, external_services::Model> = if service_ids.is_empty() {
            HashMap::new()
        } else {
            external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids))
                .all(self.db.as_ref())
                .await
                .map_err(database)?
                .into_iter()
                .map(|service| (service.id, service))
                .collect()
        };
        // One query for the projects every live service is attached to.
        let mut service_projects: HashMap<i32, Vec<i32>> = HashMap::new();
        if !live_services.is_empty() {
            let links = project_services::Entity::find()
                .filter(
                    project_services::Column::ServiceId
                        .is_in(live_services.keys().copied().collect::<Vec<_>>()),
                )
                .all(self.db.as_ref())
                .await
                .map_err(database)?;
            for link in links {
                service_projects
                    .entry(link.service_id)
                    .or_default()
                    .push(link.project_id);
            }
        }

        let mut entries: Vec<(ChatResource, Visibility)> = Vec::with_capacity(created.len() + 1);
        for (made, at) in created {
            let entry = match made {
                Created::Project { id } => live_projects.get(&id).map(|project| {
                    let resource = ChatResource {
                        kind: ChatResourceKind::Project,
                        id: project.id.to_string(),
                        name: project.name.clone(),
                        slug: Some(project.slug.clone()),
                        detail: Some(project.preset.display_name().to_string()),
                        status: None,
                        url: None,
                        created_at: at,
                    };
                    (resource, Visibility::Project(project.id))
                }),
                Created::Service { id, service_type } => live_services.get(&id).map(|service| {
                    let resource = ChatResource {
                        kind: ChatResourceKind::Service,
                        id: service.id.to_string(),
                        name: service.name.clone(),
                        slug: None,
                        detail: Some(service_type.unwrap_or_else(|| service.service_type.clone())),
                        status: Some(service.status.clone()),
                        url: None,
                        created_at: at,
                    };
                    let linked_projects = service_projects.get(&id).cloned().unwrap_or_default();
                    (resource, Visibility::Service { linked_projects })
                }),
                Created::Repository {
                    full_name,
                    name,
                    private,
                    web_url,
                } => Some((
                    ChatResource {
                        kind: ChatResourceKind::Repository,
                        id: full_name,
                        name,
                        slug: None,
                        detail: private
                            .map(|private| if private { "Private" } else { "Public" }.to_string()),
                        status: None,
                        url: web_url,
                        created_at: at,
                    },
                    Visibility::Unscoped,
                )),
            };
            if let Some((resource, visibility)) = entry {
                if !entries
                    .iter()
                    .any(|(seen, _)| seen.kind == resource.kind && seen.id == resource.id)
                {
                    entries.push((resource, visibility));
                }
            }
        }

        if let Some(link) = link_from_metadata(conversation.metadata.as_ref()) {
            let sandbox = sandboxes::Entity::find()
                .filter(sandboxes::Column::PublicId.eq(link.sandbox_id.as_str()))
                .filter(sandboxes::Column::UserId.eq(user_id))
                .filter(sandboxes::Column::Status.ne("destroyed"))
                .one(self.db.as_ref())
                .await
                .map_err(database)?;
            if let Some(sandbox) = sandbox {
                entries.push((
                    ChatResource {
                        kind: ChatResourceKind::Sandbox,
                        id: sandbox.public_id,
                        name: sandbox.name,
                        slug: None,
                        detail: sandbox.backend,
                        status: Some(sandbox.status),
                        url: None,
                        created_at: link.linked_at,
                    },
                    Visibility::Unscoped,
                ));
            }
        }
        entries.sort_by_key(|(resource, _)| std::cmp::Reverse(resource.created_at));

        Ok(ChatResourceListing { entries, truncated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_created_project_is_read_from_its_result() {
        let result = json!({"id": 36, "name": "demo-app", "slug": "demo-app"});
        assert_eq!(
            created_by(OP_CREATE_PROJECT, &json!({}), Some(&result)),
            Some(Created::Project { id: 36 })
        );
    }

    #[test]
    fn a_created_service_keeps_the_type_the_user_asked_for() {
        let params = json!({"__temps_display": {"name": "cache", "service_type": "redis"}});
        let result = json!({"id": 7, "name": "cache", "status": "starting"});
        assert_eq!(
            created_by(OP_CREATE_SERVICE, &params, Some(&result)),
            Some(Created::Service {
                id: 7,
                service_type: Some("redis".to_string())
            })
        );
    }

    #[test]
    fn a_service_without_a_display_type_is_still_found() {
        let result = json!({"id": 7});
        assert_eq!(
            created_by(OP_CREATE_SERVICE, &json!({"encrypted": "x"}), Some(&result)),
            Some(Created::Service {
                id: 7,
                service_type: None
            })
        );
    }

    #[test]
    fn a_created_repository_is_read_from_its_result() {
        let result = json!({
            "name": "demo",
            "owner": "acme",
            "private": true,
            "web_url": "https://git.example.com/acme/demo",
            "full_name": "acme/demo",
        });
        assert_eq!(
            created_by(OP_CREATE_REPOSITORY, &json!({}), Some(&result)),
            Some(Created::Repository {
                full_name: "acme/demo".to_string(),
                name: "demo".to_string(),
                private: Some(true),
                web_url: Some("https://git.example.com/acme/demo".to_string()),
            })
        );
    }

    #[test]
    fn a_repository_without_a_full_name_is_named_from_owner_and_name() {
        let result = json!({"name": "demo", "owner": "acme"});
        assert_eq!(
            created_by(OP_CREATE_REPOSITORY, &json!({}), Some(&result)),
            Some(Created::Repository {
                full_name: "acme/demo".to_string(),
                name: "demo".to_string(),
                private: None,
                web_url: None,
            })
        );
    }

    #[test]
    fn results_without_an_identity_name_nothing() {
        assert_eq!(created_by(OP_CREATE_PROJECT, &json!({}), None), None);
        assert_eq!(
            created_by(OP_CREATE_PROJECT, &json!({}), Some(&json!({"name": "x"}))),
            None
        );
        assert_eq!(
            created_by(
                OP_CREATE_PROJECT,
                &json!({}),
                Some(&json!({"id": "not-a-number"}))
            ),
            None
        );
        assert_eq!(
            created_by(
                OP_CREATE_PROJECT,
                &json!({}),
                Some(&json!({"id": 99999999999_i64}))
            ),
            None
        );
        assert_eq!(
            created_by(
                OP_CREATE_REPOSITORY,
                &json!({}),
                Some(&json!({"owner": "acme"}))
            ),
            None
        );
    }

    #[test]
    fn other_operations_create_nothing() {
        let result = json!({"id": 1, "name": "x"});
        assert_eq!(
            created_by("trigger_project_pipeline", &json!({}), Some(&result)),
            None
        );
        // A link with no recorded service names nothing.
        assert_eq!(created_by(OP_LINK_SERVICE, &json!({}), Some(&result)), None);
    }

    #[test]
    fn a_service_the_chat_linked_to_a_project_is_listed_by_its_id() {
        let params = json!({"__temps_display": {"id": 1, "project_id": 36}});
        assert_eq!(
            created_by(OP_LINK_SERVICE, &params, None),
            Some(Created::Service {
                id: 1,
                service_type: None
            })
        );
    }

    #[test]
    fn every_creating_operation_is_read_back() {
        for operation in CREATING_OPERATIONS {
            let result = json!({"id": 1, "name": "x", "owner": "o"});
            assert!(
                created_by(operation, &json!({}), Some(&result)).is_some(),
                "{operation} is queried but never parsed"
            );
        }
    }

    fn resource(kind: ChatResourceKind, id: &str) -> ChatResource {
        ChatResource {
            kind,
            id: id.to_string(),
            name: id.to_string(),
            slug: None,
            detail: None,
            status: None,
            url: None,
            created_at: Utc::now(),
        }
    }

    fn listing() -> ChatResourceListing {
        ChatResourceListing {
            entries: vec![
                (
                    resource(ChatResourceKind::Project, "1"),
                    Visibility::Project(1),
                ),
                (
                    resource(ChatResourceKind::Project, "2"),
                    Visibility::Project(2),
                ),
                // Attached only to the project the caller lost.
                (
                    resource(ChatResourceKind::Service, "10"),
                    Visibility::Service {
                        linked_projects: vec![2],
                    },
                ),
                // Shared with a project the caller still reads.
                (
                    resource(ChatResourceKind::Service, "11"),
                    Visibility::Service {
                        linked_projects: vec![2, 1],
                    },
                ),
                // Attached to no project at all.
                (
                    resource(ChatResourceKind::Service, "12"),
                    Visibility::Service {
                        linked_projects: vec![],
                    },
                ),
                (
                    resource(ChatResourceKind::Repository, "acme/demo"),
                    Visibility::Unscoped,
                ),
                (
                    resource(ChatResourceKind::Sandbox, "sbx_1"),
                    Visibility::Unscoped,
                ),
            ],
            truncated: true,
        }
    }

    fn ids(response: &ChatResourcesResponse) -> Vec<&str> {
        response
            .resources
            .iter()
            .map(|resource| resource.id.as_str())
            .collect()
    }

    #[test]
    fn every_project_a_listed_piece_depends_on_is_checked_once() {
        assert_eq!(listing().project_ids(), vec![1, 2]);
    }

    #[test]
    fn a_project_the_caller_lost_and_its_only_services_are_hidden() {
        let access = BTreeMap::from([(1, true), (2, false)]);
        let response = listing().visible_to(Some(&access));
        assert_eq!(ids(&response), vec!["1", "11", "12", "acme/demo", "sbx_1"]);
        assert!(response.truncated);
    }

    #[test]
    fn a_project_missing_from_the_access_map_is_not_readable() {
        let access = BTreeMap::from([(1, true)]);
        let response = listing().visible_to(Some(&access));
        assert!(!ids(&response).contains(&"2"));
        assert!(!ids(&response).contains(&"10"));
    }

    #[test]
    fn without_a_project_restriction_everything_is_listed() {
        let response = listing().visible_to(None);
        assert_eq!(response.resources.len(), 7);
    }

    #[test]
    fn a_caller_with_no_project_access_keeps_only_unscoped_pieces() {
        let access = BTreeMap::from([(1, false), (2, false)]);
        let response = listing().visible_to(Some(&access));
        assert_eq!(ids(&response), vec!["12", "acme/demo", "sbx_1"]);
    }

    #[test]
    fn kinds_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_value(ChatResourceKind::Repository).ok(),
            Some(json!("repository"))
        );
    }
}
