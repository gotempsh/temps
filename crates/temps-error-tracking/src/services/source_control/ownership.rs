// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ownership rules (ADR 111, D5): a path pattern and the member who owns
//! the code under it. When an event creates a group, the group's frames are
//! matched innermost first against the project's rules in order; the first
//! rule that matches a frame assigns the group to its owner, unless someone
//! already assigned it.
//!
//! Rules are few (at most [`MAX_RULES_PER_PROJECT`] per project) and kept in
//! memory, loaded once and reloaded after every change, so ingest reads no
//! rule from the database and a project without rules costs one map lookup.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, TransactionTrait,
};

use super::super::group_workflow::{activity, ActivityDetails, ActivityKind, Assignee};
use super::super::types::ErrorTrackingError;

/// Most rules a project has.
pub const MAX_RULES_PER_PROJECT: usize = 50;
/// Longest pattern.
pub const MAX_PATTERN_CHARS: usize = 300;
/// Most frames of an event matched against the rules.
pub const MAX_MATCHED_FRAMES: usize = 50;

/// One ownership rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipRule {
    pub id: i32,
    pub project_id: i32,
    /// Lower positions are tried first.
    pub position: i32,
    pub pattern: String,
    pub owner_user_id: i32,
    pub owner_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A rule as written by a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipRuleInput {
    pub pattern: String,
    pub owner_user_id: i32,
}

/// A rule that assigned a new group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipMatch {
    pub rule_id: i32,
    pub owner_user_id: i32,
    pub owner_name: Option<String>,
    pub pattern: String,
    pub path: String,
}

/// Check a pattern: a repository path where `*` matches within one
/// directory and `**` across directories.
pub fn validate_pattern(pattern: &str) -> Result<String, ErrorTrackingError> {
    let pattern = pattern
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/');
    if pattern.is_empty() || pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err(ErrorTrackingError::Validation(format!(
            "an ownership pattern is 1 to {MAX_PATTERN_CHARS} characters"
        )));
    }
    if pattern.contains('\\') || pattern.split('/').any(|s| s == "..") {
        return Err(ErrorTrackingError::Validation(format!(
            "ownership pattern '{pattern}' must be a repository path with '/' separators and no '..'"
        )));
    }
    if pattern.chars().any(char::is_control) {
        return Err(ErrorTrackingError::Validation(
            "an ownership pattern cannot contain control characters".into(),
        ));
    }
    Ok(pattern.to_string())
}

/// Whether `path` matches `pattern`. A pattern without `/` matches the file
/// name in any directory (`*.py`); a pattern ending in `/` matches
/// everything under that directory.
pub fn pattern_matches(pattern: &str, path: &str) -> bool {
    if let Some(dir) = pattern.strip_suffix('/') {
        return segments_match(&format!("{dir}/**"), path);
    }
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return glob(pattern.as_bytes(), name.as_bytes());
    }
    segments_match(pattern, path)
}

fn segments_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((segment, path_rest)) => {
                glob(first.as_bytes(), segment.as_bytes()) && match_segments(rest, path_rest)
            }
            None => false,
        },
    }
}

/// `*` and `?` within one segment, iteratively (no backtracking blow-up).
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// The first rule, in order, that matches a path, trying paths in order.
pub fn first_match<'a>(
    rules: &'a [OwnershipRule],
    paths: &[String],
) -> Option<(&'a OwnershipRule, &'a str, String)> {
    for path in paths.iter().take(MAX_MATCHED_FRAMES) {
        if let Some(rule) = rules.iter().find(|r| pattern_matches(&r.pattern, path)) {
            return Some((rule, rule.pattern.as_str(), path.clone()));
        }
    }
    None
}

fn rule_from_row(row: &QueryResult) -> Result<OwnershipRule, ErrorTrackingError> {
    Ok(OwnershipRule {
        id: row.try_get("", "id")?,
        project_id: row.try_get("", "project_id")?,
        position: row.try_get("", "position")?,
        pattern: row.try_get("", "pattern")?,
        owner_user_id: row.try_get("", "owner_user_id")?,
        owner_name: row.try_get("", "owner_name")?,
        created_at: row.try_get("", "created_at")?,
        updated_at: row.try_get("", "updated_at")?,
    })
}

const RULE_SELECT: &str = "SELECT r.id, r.project_id, r.position, r.pattern, r.owner_user_id, \
                           u.name AS owner_name, r.created_at, r.updated_at \
                           FROM error_ownership_rules r JOIN users u ON u.id = r.owner_user_id";

type RuleMap = HashMap<i32, Arc<[OwnershipRule]>>;

/// Ownership rules of every project, and their matching at ingest.
pub struct OwnershipService {
    db: Arc<DatabaseConnection>,
    /// Every project's rules; `None` until first loaded.
    rules: ArcSwapOption<RuleMap>,
}

impl OwnershipService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            rules: ArcSwapOption::empty(),
        }
    }

    /// Read every rule into memory.
    pub async fn reload(&self) -> Result<(), ErrorTrackingError> {
        let rows = self
            .db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                format!("{RULE_SELECT} ORDER BY r.project_id, r.position, r.id"),
            ))
            .await?;
        let mut map: HashMap<i32, Vec<OwnershipRule>> = HashMap::new();
        for row in &rows {
            let rule = rule_from_row(row)?;
            map.entry(rule.project_id).or_default().push(rule);
        }
        let map: RuleMap = map.into_iter().map(|(k, v)| (k, Arc::from(v))).collect();
        self.rules.store(Some(Arc::new(map)));
        Ok(())
    }

    /// Whether the project may have rules: true until rules are loaded, so
    /// nothing is missed before the first load.
    pub fn may_have_rules(&self, project_id: i32) -> bool {
        match self.rules.load().as_ref() {
            Some(map) => map.contains_key(&project_id),
            None => true,
        }
    }

    async fn project_rules(
        &self,
        project_id: i32,
    ) -> Result<Arc<[OwnershipRule]>, ErrorTrackingError> {
        if self.rules.load().is_none() {
            self.reload().await?;
        }
        Ok(self
            .rules
            .load()
            .as_ref()
            .and_then(|map| map.get(&project_id).cloned())
            .unwrap_or_else(|| Arc::from(Vec::new())))
    }

    /// The project's rules in the order they are tried.
    pub async fn list(&self, project_id: i32) -> Result<Vec<OwnershipRule>, ErrorTrackingError> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("{RULE_SELECT} WHERE r.project_id = $1 ORDER BY r.position, r.id LIMIT $2"),
                [project_id.into(), (MAX_RULES_PER_PROJECT as i64).into()],
            ))
            .await?;
        rows.iter().map(rule_from_row).collect()
    }

    /// Replace the project's rules with `rules`, in order. Returns the new
    /// rules and the previous ones, for the audit log.
    pub async fn replace(
        &self,
        project_id: i32,
        rules: &[OwnershipRuleInput],
    ) -> Result<(Vec<OwnershipRule>, Vec<OwnershipRule>), ErrorTrackingError> {
        if rules.len() > MAX_RULES_PER_PROJECT {
            return Err(ErrorTrackingError::Validation(format!(
                "a project has at most {MAX_RULES_PER_PROJECT} ownership rules, got {}",
                rules.len()
            )));
        }
        let mut patterns = Vec::with_capacity(rules.len());
        for rule in rules {
            patterns.push(validate_pattern(&rule.pattern)?);
        }
        let previous = self.list(project_id).await?;
        let txn = self.db.begin().await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM error_ownership_rules WHERE project_id = $1",
            [project_id.into()],
        ))
        .await?;
        if !rules.is_empty() {
            let positions: Vec<i32> = (0..rules.len() as i32).collect();
            let owners: Vec<i32> = rules.iter().map(|r| r.owner_user_id).collect();
            let inserted = txn
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO error_ownership_rules (project_id, position, pattern, owner_user_id) \
                     SELECT $1, r.position, r.pattern, r.owner \
                     FROM UNNEST($2::int[], $3::text[], $4::int[]) AS r(position, pattern, owner) \
                     JOIN users u ON u.id = r.owner AND u.deleted_at IS NULL",
                    [
                        project_id.into(),
                        positions.into(),
                        patterns.clone().into(),
                        owners.clone().into(),
                    ],
                ))
                .await?
                .rows_affected();
            if inserted != rules.len() as u64 {
                let known: Vec<i32> = txn
                    .query_all(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT id FROM users WHERE id = ANY($1) AND deleted_at IS NULL",
                        [owners.clone().into()],
                    ))
                    .await?
                    .iter()
                    .filter_map(|row| row.try_get("", "id").ok())
                    .collect();
                let missing = owners
                    .iter()
                    .find(|id| !known.contains(id))
                    .copied()
                    .unwrap_or_default();
                return Err(ErrorTrackingError::Validation(format!(
                    "ownership rule owner {missing} is not a user"
                )));
            }
        }
        txn.commit().await?;
        self.reload().await?;
        Ok((self.list(project_id).await?, previous))
    }

    /// The rule that owns the event creating a group, if any: its frames,
    /// innermost first, matched against the project's rules in order. Runs
    /// once per new group, inside its transaction; a project without rules
    /// costs one map lookup.
    pub async fn match_new_group<C: ConnectionTrait>(
        &self,
        db: &C,
        error_data: &super::super::types::CreateErrorEventData,
    ) -> Option<OwnershipMatch> {
        let project_id = error_data.project_id;
        let rules = match self.project_rules(project_id).await {
            Ok(rules) if !rules.is_empty() => rules,
            Ok(_) => return None,
            Err(error) => {
                tracing::warn!(project_id, %error, "Ownership rules could not be loaded; the new group is not assigned");
                return None;
            }
        };
        let source_root: Option<String> = match db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT error_source_root FROM projects WHERE id = $1",
                [project_id.into()],
            ))
            .await
        {
            Ok(row) => row.and_then(|r| r.try_get("", "error_source_root").ok().flatten()),
            Err(error) => {
                tracing::warn!(project_id, %error, "Could not read the project's source root for ownership rules");
                None
            }
        };
        let native = error_data
            .exceptions
            .first()
            .and_then(|e| e.stack_trace.as_ref())
            .or(error_data.stack_trace.as_ref());
        let paths: Vec<String> =
            super::frames::app_frames_from(error_data.raw_sentry_event.as_ref(), native)
                .iter()
                .take(MAX_MATCHED_FRAMES)
                .filter_map(|frame| {
                    super::frames::repository_path(&frame.filename, source_root.as_deref())
                })
                .collect();
        let (rule, pattern, path) = first_match(&rules, &paths)?;
        Some(OwnershipMatch {
            rule_id: rule.id,
            owner_user_id: rule.owner_user_id,
            owner_name: rule.owner_name.clone(),
            pattern: pattern.to_string(),
            path,
        })
    }

    /// Assign a group an event has just created to the owner a rule
    /// matched. A group someone already assigned is left alone.
    pub async fn assign_matched(
        &self,
        project_id: i32,
        group_id: i32,
        found: &OwnershipMatch,
    ) -> Result<bool, ErrorTrackingError> {
        let txn = self.db.begin().await?;
        let assigned = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_groups SET assignee_user_id = $3, assignee_team_id = NULL, \
                        assigned_to = NULL, updated_at = NOW() \
                 WHERE id = $1 AND project_id = $2 \
                   AND assignee_user_id IS NULL AND assignee_team_id IS NULL \
                   AND EXISTS (SELECT 1 FROM users u WHERE u.id = $3 AND u.deleted_at IS NULL)",
                [
                    group_id.into(),
                    project_id.into(),
                    found.owner_user_id.into(),
                ],
            ))
            .await?
            .rows_affected();
        if assigned == 0 {
            return Ok(false);
        }
        activity::record_many(
            &txn,
            project_id,
            &[group_id],
            ActivityKind::Assigned,
            None,
            &ActivityDetails {
                assignee: Some(Assignee::User(found.owner_user_id)),
                assignee_name: found.owner_name.clone(),
                ownership_rule: Some(found.pattern.clone()),
                ..Default::default()
            },
        )
        .await?;
        txn.commit().await?;
        tracing::info!(project_id, group_id, rule_id = found.rule_id, owner_user_id = found.owner_user_id, path = %found.path, "Assigned a new error group by ownership rule");
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i32, pattern: &str, owner: i32) -> OwnershipRule {
        OwnershipRule {
            id,
            project_id: 1,
            position: id,
            pattern: pattern.into(),
            owner_user_id: owner,
            owner_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn patterns_match_by_directory_by_file_name_and_across_directories() {
        assert!(pattern_matches(
            "src/billing/",
            "src/billing/invoice/pdf.ts"
        ));
        assert!(!pattern_matches("src/billing/", "src/billingx/a.ts"));
        assert!(pattern_matches("*.py", "app/models/cart.py"));
        assert!(!pattern_matches("*.py", "app/models/cart.pyc"));
        assert!(pattern_matches("src/**/cart.ts", "src/cart.ts"));
        assert!(pattern_matches("src/**/cart.ts", "src/shop/v2/cart.ts"));
        assert!(pattern_matches("src/*/cart.ts", "src/shop/cart.ts"));
        assert!(!pattern_matches("src/*/cart.ts", "src/shop/v2/cart.ts"));
        assert!(pattern_matches("lib/pay?.rb", "lib/pay1.rb"));
        // A long pattern of stars does not backtrack exponentially.
        let stars = "*a".repeat(40);
        assert!(!pattern_matches(&stars, &"a".repeat(39)));
    }

    #[test]
    fn the_innermost_frame_wins_then_the_rule_order() {
        let rules = vec![
            rule(1, "src/shared/", 10),
            rule(2, "src/cart/", 20),
            rule(3, "*.ts", 30),
        ];
        let paths = vec![
            "src/cart/total.ts".to_string(),
            "src/shared/http.ts".to_string(),
        ];
        let (found, _, path) = first_match(&rules, &paths).expect("match");
        assert_eq!(
            found.owner_user_id, 20,
            "the innermost frame is matched first"
        );
        assert_eq!(path, "src/cart/total.ts");
        assert!(first_match(&rules, &["docs/readme.md".to_string()]).is_none());
    }

    #[test]
    fn patterns_are_repository_paths() {
        assert_eq!(validate_pattern("./src/a/").expect("ok"), "src/a/");
        for bad in ["", "src\\a", "src/../etc", "a\u{1}"] {
            assert!(validate_pattern(bad).is_err(), "{bad:?}");
        }
        assert!(validate_pattern(&"a".repeat(MAX_PATTERN_CHARS + 1)).is_err());
    }
}
