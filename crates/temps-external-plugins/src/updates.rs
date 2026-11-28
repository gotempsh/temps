// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A plugin is pinned to the exact build an administrator approved.
//!
//! A plugin's service can be redeployed by anyone who may deploy it, and a
//! new build is new code holding the plugin's permissions. So Temps attaches
//! only to the approved build. When the service runs anything else, or the
//! approved build starts asking for more, the build is recorded here as an
//! update that waits, with what it asks for beyond what was approved, and
//! nothing is attached until an administrator approves exactly that build
//! and manifest.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_plugin_protocol::channel::PluginHostPermission;
use temps_plugin_protocol::{PluginCapability, PluginManifest};
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("Database error while trying to {operation} of plugin '{plugin_name}': {source}")]
    Database {
        plugin_name: String,
        operation: &'static str,
        source: DbErr,
    },
    #[error("The manifest of plugin '{plugin_name}' cannot be stored for review: {source}")]
    Manifest {
        plugin_name: String,
        source: serde_json::Error,
    },
}

/// What a build asks for compared with what an administrator approved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct AccessDifference {
    /// The approved version, when it is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_from: Option<String>,
    pub version_to: String,
    /// Permissions the update asks for that the approved build did not.
    /// Approving the update does not allow them.
    pub added_permissions: Vec<PluginHostPermission>,
    /// Permissions the update no longer asks for.
    pub removed_permissions: Vec<PluginHostPermission>,
    /// Classes of delegated API calls the update newly intends to make.
    pub added_capabilities: Vec<PluginCapability>,
    /// Routes the update newly serves without a Temps session.
    pub added_public_paths: Vec<String>,
    /// Platform events the update newly subscribes to.
    pub added_events: Vec<String>,
    /// Whether the update asks for anything the approved build did not.
    pub asks_for_more: bool,
}

impl AccessDifference {
    /// Compare `update` with the manifest that was approved. With no
    /// approved manifest to compare with, everything the update asks for is
    /// new.
    pub fn between(approved: Option<&PluginManifest>, update: &PluginManifest) -> Self {
        fn added<T: PartialEq + Clone>(before: Option<&[T]>, after: &[T]) -> Vec<T> {
            let mut added: Vec<T> = Vec::new();
            for item in after {
                let known = before.is_some_and(|before| before.contains(item));
                if !known && !added.contains(item) {
                    added.push(item.clone());
                }
            }
            added
        }
        let added_permissions = added(
            approved.map(|manifest| manifest.host_permissions.as_slice()),
            &update.host_permissions,
        );
        let removed_permissions = approved
            .map(|manifest| added(Some(&update.host_permissions), &manifest.host_permissions))
            .unwrap_or_default();
        let added_capabilities = added(
            approved.map(|manifest| manifest.capabilities.as_slice()),
            &update.capabilities,
        );
        let added_public_paths = added(
            approved.map(|manifest| manifest.public_paths.as_slice()),
            &update.public_paths,
        );
        let added_events = added(
            approved.map(|manifest| manifest.events.as_slice()),
            &update.events,
        );
        let asks_for_more = !(added_permissions.is_empty()
            && added_capabilities.is_empty()
            && added_public_paths.is_empty()
            && added_events.is_empty());
        Self {
            version_from: approved.map(|manifest| manifest.version.clone()),
            version_to: update.version.clone(),
            added_permissions,
            removed_permissions,
            added_capabilities,
            added_public_paths,
            added_events,
            asks_for_more,
        }
    }
}

/// What an approval must quote: one build answering one manifest. A build
/// that answers anything else afterwards is a different review.
pub fn review_id(build: &str, manifest: &PluginManifest) -> Result<String, serde_json::Error> {
    fn canonical(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(fields) => {
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort();
                out.push('{');
                for key in keys {
                    out.push_str(&serde_json::Value::String(key.clone()).to_string());
                    out.push(':');
                    if let Some(field) = fields.get(key) {
                        canonical(field, out);
                    }
                    out.push(',');
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for item in items {
                    canonical(item, out);
                    out.push(',');
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }
    let mut text = String::new();
    canonical(&serde_json::to_value(manifest)?, &mut text);
    let mut hasher = Sha256::new();
    hasher.update(b"temps-plugin-update-review-v1\0");
    hasher.update(build.as_bytes());
    hasher.update([0u8]);
    hasher.update(text.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

/// A build a plugin's service runs that no administrator approved.
#[derive(Debug, Clone)]
pub struct PendingUpdate {
    pub build: String,
    pub deployment_id: i32,
    /// The manifest the build answered with.
    pub manifest: PluginManifest,
    /// See [`review_id`].
    pub review: String,
    /// Since when it waits; `None` before it is stored.
    pub since: Option<DateTime<Utc>>,
}

/// The build a plugin is pinned to, and the update that waits, if any.
#[derive(Debug, Clone, Default)]
pub struct BuildPin {
    /// `None` for a plugin Temps has not attached to since pins existed.
    pub build: Option<String>,
    pub deployment_id: Option<i32>,
    /// The manifest that was approved with the build.
    pub approved: Option<PluginManifest>,
    pub pending: Option<PendingUpdate>,
}

impl BuildPin {
    /// The columns a pin is read from, for a query that selects them.
    pub const COLUMNS: &'static str =
        "manifest, pinned_build, pinned_deployment_id, pending_build, \
         pending_deployment_id, pending_manifest, pending_review, pending_since";

    /// Read a pin from a row that selected [`Self::COLUMNS`].
    pub fn from_row(row: &sea_orm::QueryResult) -> Result<Self, DbErr> {
        let manifest: Option<serde_json::Value> = row.try_get("", "manifest")?;
        let pending_build: Option<String> = row.try_get("", "pending_build")?;
        let pending_deployment_id: Option<i32> = row.try_get("", "pending_deployment_id")?;
        let pending_manifest: Option<serde_json::Value> = row.try_get("", "pending_manifest")?;
        let pending_review: Option<String> = row.try_get("", "pending_review")?;
        let pending_since: Option<DateTime<Utc>> = row.try_get("", "pending_since")?;
        // An update whose record is incomplete, or whose manifest this build
        // cannot read, is no update to approve: the next attempt to attach
        // records a current one.
        let pending = match (
            pending_build,
            pending_deployment_id,
            pending_manifest.and_then(|value| serde_json::from_value(value).ok()),
            pending_review,
        ) {
            (Some(build), Some(deployment_id), Some(manifest), Some(review)) => {
                Some(PendingUpdate {
                    build,
                    deployment_id,
                    manifest,
                    review,
                    since: pending_since,
                })
            }
            _ => None,
        };
        Ok(Self {
            build: row.try_get("", "pinned_build")?,
            deployment_id: row.try_get("", "pinned_deployment_id")?,
            // A manifest this build cannot read is the same as none.
            approved: manifest.and_then(|value| serde_json::from_value(value).ok()),
            pending,
        })
    }

    /// What the waiting update asks for beyond what was approved.
    pub fn pending_difference(&self) -> Option<AccessDifference> {
        self.pending
            .as_ref()
            .map(|pending| AccessDifference::between(self.approved.as_ref(), &pending.manifest))
    }

    /// Whether Temps may attach to `build` answering `manifest` without
    /// asking: it is the approved build, asking for nothing more. A plugin
    /// with no pin yet takes the build that runs.
    pub fn allows(&self, build: &str, manifest: &PluginManifest) -> bool {
        match self.build.as_deref() {
            None => true,
            Some(pinned) => {
                pinned == build
                    && !AccessDifference::between(self.approved.as_ref(), manifest).asks_for_more
            }
        }
    }
}

/// Reads and records build pins on the plugin's actor row.
pub struct PluginBuilds {
    db: Arc<DatabaseConnection>,
}

impl PluginBuilds {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// The pin of the active plugin `plugin_name`; `None` when no such
    /// plugin is added.
    pub async fn pin_of(&self, plugin_name: &str) -> Result<Option<BuildPin>, UpdateError> {
        let database = |source| UpdateError::Database {
            plugin_name: plugin_name.to_string(),
            operation: "read the approved build",
            source,
        };
        let Some(row) = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT {} FROM external_plugin_actors WHERE plugin_name=$1 AND active=TRUE",
                    BuildPin::COLUMNS
                ),
                [plugin_name.into()],
            ))
            .await
            .map_err(database)?
        else {
            return Ok(None);
        };
        BuildPin::from_row(&row).map(Some).map_err(database)
    }

    /// Record that `update` waits for an administrator. Returns whether it
    /// is news: `false` when the same review was already waiting.
    pub async fn hold(
        &self,
        plugin_name: &str,
        update: &PendingUpdate,
    ) -> Result<bool, UpdateError> {
        let manifest =
            serde_json::to_value(&update.manifest).map_err(|source| UpdateError::Manifest {
                plugin_name: plugin_name.to_string(),
                source,
            })?;
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE external_plugin_actors SET pending_build=$2, pending_deployment_id=$3, \
                        pending_manifest=$4, pending_review=$5, pending_since=NOW(), updated_at=NOW() \
                 WHERE plugin_name=$1 AND active=TRUE AND pending_review IS DISTINCT FROM $5",
                [
                    plugin_name.into(),
                    update.build.clone().into(),
                    update.deployment_id.into(),
                    manifest.into(),
                    update.review.clone().into(),
                ],
            ))
            .await
            .map_err(|source| UpdateError::Database {
                plugin_name: plugin_name.to_string(),
                operation: "record the update that waits",
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str) -> PluginManifest {
        PluginManifest::builder("reports", version)
            .host_permission(PluginHostPermission::EventsRead)
            .capability(PluginCapability::ApiRead)
            .public_path("/hooks/*")
            .event("deployment.succeeded")
            .build()
    }

    #[test]
    fn a_build_that_asks_for_the_same_asks_for_nothing_more() {
        let difference = AccessDifference::between(Some(&manifest("1.0.0")), &manifest("1.1.0"));
        assert_eq!(difference.version_from.as_deref(), Some("1.0.0"));
        assert_eq!(difference.version_to, "1.1.0");
        assert!(!difference.asks_for_more);
        assert!(difference.added_permissions.is_empty());
        assert!(difference.removed_permissions.is_empty());
    }

    #[test]
    fn each_kind_of_new_access_is_named_and_counts_as_more() {
        let approved = manifest("1.0.0");
        let mut permission = manifest("1.1.0");
        permission
            .host_permissions
            .push(PluginHostPermission::ApiWrite);
        let difference = AccessDifference::between(Some(&approved), &permission);
        assert_eq!(
            difference.added_permissions,
            [PluginHostPermission::ApiWrite]
        );
        assert!(difference.asks_for_more);

        let mut capability = manifest("1.1.0");
        capability.capabilities.push(PluginCapability::ApiWrite);
        let difference = AccessDifference::between(Some(&approved), &capability);
        assert_eq!(difference.added_capabilities, [PluginCapability::ApiWrite]);
        assert!(difference.asks_for_more);

        let mut public = manifest("1.1.0");
        public.public_paths.push("/export/*".into());
        // Listed twice by the plugin, reported once.
        public.public_paths.push("/export/*".into());
        let difference = AccessDifference::between(Some(&approved), &public);
        assert_eq!(difference.added_public_paths, ["/export/*"]);
        assert!(difference.asks_for_more);

        let mut event = manifest("1.1.0");
        event.events.push("project.deleted".into());
        let difference = AccessDifference::between(Some(&approved), &event);
        assert_eq!(difference.added_events, ["project.deleted"]);
        assert!(difference.asks_for_more);
    }

    #[test]
    fn asking_for_less_is_reported_and_is_not_more() {
        let mut less = manifest("1.1.0");
        less.host_permissions.clear();
        less.public_paths.clear();
        let difference = AccessDifference::between(Some(&manifest("1.0.0")), &less);
        assert_eq!(
            difference.removed_permissions,
            [PluginHostPermission::EventsRead]
        );
        assert!(!difference.asks_for_more);
    }

    #[test]
    fn with_nothing_approved_to_compare_everything_asked_for_is_new() {
        let difference = AccessDifference::between(None, &manifest("1.0.0"));
        assert_eq!(difference.version_from, None);
        assert_eq!(
            difference.added_permissions,
            [PluginHostPermission::EventsRead]
        );
        assert!(difference.removed_permissions.is_empty());
        assert!(difference.asks_for_more);

        // A plugin that asks for nothing asks for nothing more.
        let quiet = PluginManifest::builder("reports", "1.0.0").build();
        assert!(!AccessDifference::between(None, &quiet).asks_for_more);
    }

    #[test]
    fn a_review_names_one_build_and_one_manifest() {
        let first = review_id("build-1", &manifest("1.0.0")).expect("review");
        assert_eq!(first.len(), 64);
        assert_eq!(
            first,
            review_id("build-1", &manifest("1.0.0")).expect("review")
        );
        assert_ne!(
            first,
            review_id("build-2", &manifest("1.0.0")).expect("review")
        );
        assert_ne!(
            first,
            review_id("build-1", &manifest("1.0.1")).expect("review")
        );
        let mut more = manifest("1.0.0");
        more.host_permissions.push(PluginHostPermission::ApiWrite);
        assert_ne!(first, review_id("build-1", &more).expect("review"));
    }

    #[test]
    fn only_the_approved_build_asking_for_no_more_is_attached_without_asking() {
        let pin = BuildPin {
            build: Some("build-1".into()),
            deployment_id: Some(7),
            approved: Some(manifest("1.0.0")),
            pending: None,
        };
        assert!(pin.allows("build-1", &manifest("1.0.0")));
        // Another build, even one asking for exactly the same.
        assert!(!pin.allows("build-2", &manifest("1.0.0")));
        // The approved build, now asking for more.
        let mut more = manifest("1.0.0");
        more.host_permissions.push(PluginHostPermission::ApiWrite);
        assert!(!pin.allows("build-1", &more));
        // A plugin with no pin yet takes the build that runs.
        assert!(BuildPin::default().allows("build-9", &more));
        assert!(pin.pending_difference().is_none());

        let waiting = BuildPin {
            pending: Some(PendingUpdate {
                build: "build-2".into(),
                deployment_id: 8,
                manifest: more,
                review: "review".into(),
                since: None,
            }),
            ..pin
        };
        let difference = waiting.pending_difference().expect("a waiting update");
        assert_eq!(
            difference.added_permissions,
            [PluginHostPermission::ApiWrite]
        );
    }
}
