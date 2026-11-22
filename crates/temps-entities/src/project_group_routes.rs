// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A path route inside a [`super::project_groups`] row (ADR 050): requests to
/// the source project's hostnames whose path starts with `path_prefix` are
/// served by the target project's matching environment. The proxy resolves
/// these into its in-memory route table; it never reads this table per
/// request.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "project_group_routes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub group_id: i32,
    /// The project whose hostnames the rule applies to (e.g. the frontend).
    pub source_project_id: i32,
    /// The project that serves matching requests (e.g. the API).
    pub target_project_id: i32,
    /// Segment-aligned prefix such as `/api`; never `/` and never with a
    /// trailing slash.
    pub path_prefix: String,
    /// Drop the prefix from the path sent to the target (`/api/x` → `/x`).
    pub strip_prefix: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project_groups::Entity",
        from = "Column::GroupId",
        to = "super::project_groups::Column::Id",
        on_delete = "Cascade"
    )]
    Group,
}

impl Related<super::project_groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Group.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Paths the platform answers itself on every app hostname (analytics,
/// OTLP ingest and captcha endpoints, the password gate, ACME challenges). A path route can
/// never claim them, and the proxy never path-routes a request under them.
pub const RESERVED_PATH_PREFIXES: [&str; 5] = [
    "/_temps",
    "/api/_temps",
    "/api/__temps",
    "/api/otel",
    "/.well-known",
];

pub const MAX_PATH_PREFIX_LEN: usize = 255;

/// Whether `path` is `prefix` itself or below it: `/api` matches `/api` and
/// `/api/users`, never `/apiary`.
pub fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Whether `path` belongs to the platform rather than to any app.
pub fn is_reserved_path(path: &str) -> bool {
    RESERVED_PATH_PREFIXES
        .iter()
        .any(|reserved| path_matches_prefix(path, reserved))
}

/// Canonical form of a user-entered prefix (`api/` → `/api`), or why it
/// cannot be one. Segments are plain URL path characters, so a prefix means
/// the same thing to every client and cannot smuggle `..` or encoded bytes.
pub fn normalize_path_prefix(input: &str) -> Result<String, String> {
    let trimmed = input.trim().trim_end_matches('/');
    let prefix = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    if prefix == "/" {
        return Err(
            "The prefix cannot be '/': that would send every request to the other service. \
                    Use a path such as /api"
                .to_string(),
        );
    }
    if prefix.len() > MAX_PATH_PREFIX_LEN {
        return Err(format!(
            "The prefix must be at most {MAX_PATH_PREFIX_LEN} characters"
        ));
    }
    for segment in prefix[1..].split('/') {
        let valid = !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'));
        if !valid {
            return Err(format!(
                "'{prefix}' is not a valid path prefix: use segments of letters, digits, \
                 '-', '.', '_' or '~', such as /api or /api/v2"
            ));
        }
    }
    if is_reserved_path(&prefix) {
        return Err(format!(
            "'{prefix}' is reserved: the platform answers requests under it itself"
        ));
    }
    Ok(prefix)
}

/// The path the target receives. With `strip_prefix`, `/api/users?x=1`
/// under `/api` becomes `/users?x=1` and `/api` itself becomes `/`.
///
/// The result always starts with exactly one `/`. Stripping `/api` from
/// `/api//evil.com/x` would otherwise leave `//evil.com/x`, which targets
/// read as a scheme-relative URL (another host) in redirects they build from
/// the request path.
pub fn rewrite_path(path_and_query: &str, prefix: &str, strip_prefix: bool) -> String {
    if !strip_prefix {
        return path_and_query.to_string();
    }
    let rest = path_and_query
        .strip_prefix(prefix)
        .unwrap_or(path_and_query);
    format!("/{}", rest.trim_start_matches('/'))
}

/// The environment of the target project that serves a source environment:
/// the one with the same slug (`production` → `production`), or, for a
/// preview, the target's preview of the same branch. Never falls back to
/// another environment: a preview frontend must not reach a production API
/// by accident. `None` means the rule does not apply to that environment.
pub fn target_environment<'a>(
    source: &super::environments::Model,
    target_environments: &'a [super::environments::Model],
) -> Option<&'a super::environments::Model> {
    target_environments
        .iter()
        .filter(|t| t.deleted_at.is_none() && t.is_preview == source.is_preview)
        .find(|t| {
            if source.is_preview {
                source.branch.is_some() && t.branch == source.branch
            } else {
                t.slug == source.slug
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environments;

    fn env(id: i32, slug: &str, preview: bool, branch: Option<&str>) -> environments::Model {
        let mut e: environments::Model = serde_json::from_value(serde_json::json!({
            "id": id, "name": slug, "slug": slug, "subdomain": slug,
            "last_deployment": null, "host": "", "upstreams": [],
            "created_at": "2026-09-28T00:00:00Z", "updated_at": "2026-09-28T00:00:00Z",
            "project_id": 1, "current_deployment_id": null, "branch": null,
            "deleted_at": null, "deployment_config": null, "is_preview": false,
            "protected": false, "sleeping": false, "attack_mode": null,
            "force_https": null, "last_activity_at": null
        }))
        .expect("environment fixture");
        e.is_preview = preview;
        e.branch = branch.map(str::to_string);
        e
    }

    #[test]
    fn environments_match_by_slug() {
        let targets = [
            env(10, "production", false, Some("main")),
            env(11, "staging", false, None),
        ];
        let source = env(1, "staging", false, Some("develop"));
        assert_eq!(
            target_environment(&source, &targets).map(|e| e.id),
            Some(11)
        );
        let source = env(2, "qa", false, None);
        assert!(target_environment(&source, &targets).is_none());
    }

    #[test]
    fn previews_match_by_branch_and_never_fall_back_to_production() {
        let targets = [
            env(10, "production", false, Some("main")),
            env(12, "preview-feat", true, Some("feat/login")),
        ];
        let source = env(3, "preview-feat", true, Some("feat/login"));
        assert_eq!(
            target_environment(&source, &targets).map(|e| e.id),
            Some(12)
        );
        let other = env(4, "preview-x", true, Some("feat/other"));
        assert!(target_environment(&other, &targets).is_none());
    }

    #[test]
    fn deleted_environments_are_ignored() {
        let mut gone = env(10, "production", false, None);
        gone.deleted_at = Some(chrono::Utc::now());
        let source = env(1, "production", false, None);
        assert!(target_environment(&source, &[gone]).is_none());
    }

    #[test]
    fn prefixes_are_normalized() {
        assert_eq!(normalize_path_prefix("api/").unwrap(), "/api");
        assert_eq!(normalize_path_prefix(" /api/v2 ").unwrap(), "/api/v2");
        assert_eq!(normalize_path_prefix("/.api~x_1-y").unwrap(), "/.api~x_1-y");
    }

    #[test]
    fn bad_prefixes_are_rejected() {
        for bad in [
            "/", "", "//", "/api//v2", "/a/../b", "/./a", "/a b", "/a%2F", "/a?x", "/é",
        ] {
            assert!(
                normalize_path_prefix(bad).is_err(),
                "{bad} should be rejected"
            );
        }
        assert!(normalize_path_prefix(&format!("/{}", "a".repeat(255))).is_err());
    }

    #[test]
    fn platform_paths_are_reserved() {
        for reserved in [
            "/_temps",
            "/api/_temps",
            "/api/_temps/events",
            "/api/__temps",
            "/.well-known/x",
        ] {
            assert!(
                normalize_path_prefix(reserved).is_err(),
                "{reserved} should be reserved"
            );
        }
        // A parent of a reserved path is fine: the platform answers first.
        assert!(normalize_path_prefix("/api").is_ok());
        assert!(is_reserved_path("/api/_temps/event"));
        assert!(!is_reserved_path("/api/_tempsx"));
    }

    #[test]
    fn prefixes_match_whole_segments() {
        assert!(path_matches_prefix("/api", "/api"));
        assert!(path_matches_prefix("/api/users", "/api"));
        assert!(!path_matches_prefix("/apiary", "/api"));
        assert!(!path_matches_prefix("/", "/api"));
    }

    #[test]
    fn stripping_keeps_the_rest_of_the_path_and_query() {
        assert_eq!(rewrite_path("/api/users?x=1", "/api", true), "/users?x=1");
        assert_eq!(rewrite_path("/api", "/api", true), "/");
        assert_eq!(rewrite_path("/api?x=1", "/api", true), "/?x=1");
        assert_eq!(
            rewrite_path("/api/users?x=1", "/api", false),
            "/api/users?x=1"
        );
    }

    #[test]
    fn stripping_never_leaves_a_scheme_relative_path() {
        assert_eq!(
            rewrite_path("/api//evil.com/x", "/api", true),
            "/evil.com/x"
        );
        assert_eq!(rewrite_path("/api///x?y=1", "/api", true), "/x?y=1");
        assert_eq!(rewrite_path("/api/", "/api", true), "/");
    }
}
