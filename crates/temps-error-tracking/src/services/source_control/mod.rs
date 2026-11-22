// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error groups and the project's repository (ADR 111, D5): issues opened
//! from a group or linked to it, the commits that last touched the code an
//! error points at, and ownership rules that assign a new group by path.
//!
//! The repository is the one the project deploys from, reached through
//! [`ProjectSourceControl`], which `temps-git` implements over the project's
//! git provider connection. Nothing here runs on ingest except the
//! ownership match, which reads rules the service keeps in memory.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, TransactionTrait,
};
use temps_core::source_control::{
    ProjectRepository, ProjectSourceControl, RepositoryCommit, SourceControlError,
};

use super::group_workflow::{activity, ActivityDetails, ActivityKind};
use super::types::ErrorTrackingError;

pub mod frames;
pub mod ownership;

#[cfg(test)]
mod tests;

/// Most issues linked to one group.
pub const MAX_LINKS_PER_GROUP: usize = 10;
/// Longest linked URL.
pub const MAX_URL_CHARS: usize = 2048;
/// Longest issue title.
pub const MAX_ISSUE_TITLE_CHARS: usize = 255;
/// Most suspect commits shown.
pub const MAX_SUSPECT_COMMITS: usize = 5;
/// Most frames listed in an issue body.
const ISSUE_BODY_FRAMES: usize = 10;
/// Where a server without the git integration is told to look.
pub const GIT_PROVIDERS_PATH: &str = "/git-providers";

/// Whether the project's repository can be reached, and if not why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceControlStatus {
    pub configured: bool,
    pub provider: Option<String>,
    pub repository: Option<String>,
    pub web_url: Option<String>,
    pub reason: Option<String>,
    pub setup_path: Option<String>,
}

/// An issue linked to an error group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLink {
    pub id: i64,
    pub group_id: i32,
    /// `github`, `gitlab`, or `link` for any other URL.
    pub provider: String,
    pub repository: Option<String>,
    pub number: Option<i64>,
    pub url: String,
    pub title: String,
    /// `open` or `closed` when last read; `None` for plain links.
    pub state: Option<String>,
    pub state_checked_at: Option<DateTime<Utc>>,
    pub created_by: Option<i32>,
    pub created_at: DateTime<Utc>,
}

/// The commits that last touched the file of an error's innermost
/// application frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspectCommits {
    /// The repository path looked up, when the frame has one.
    pub path: Option<String>,
    pub function: Option<String>,
    pub lineno: Option<i64>,
    /// The commit or branch the history was read at.
    pub at: Option<String>,
    pub commits: Vec<RepositoryCommit>,
    /// Why there are none, when there are none.
    pub reason: Option<String>,
}

/// Service for issues, suspect commits and ownership of error groups.
pub struct SourceControlService {
    db: Arc<DatabaseConnection>,
    source_control: OnceLock<Arc<dyn ProjectSourceControl>>,
    pub ownership: Arc<ownership::OwnershipService>,
    /// Groups an issue is being opened for in this process.
    issues_in_flight: Arc<Mutex<HashSet<i32>>>,
}

/// A group an issue is being opened for, released when dropped (on every
/// return path, including a cancelled request). The set is touched only
/// to claim and release, never across an await.
struct IssueInFlight {
    groups: Arc<Mutex<HashSet<i32>>>,
    group_id: i32,
}

impl IssueInFlight {
    fn claim(groups: &Arc<Mutex<HashSet<i32>>>, group_id: i32) -> Option<Self> {
        let mut set = groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set.insert(group_id).then(|| Self {
            groups: groups.clone(),
            group_id,
        })
    }
}

impl Drop for IssueInFlight {
    fn drop(&mut self) {
        self.groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.group_id);
    }
}

const LINK_COLUMNS: &str = "id, error_group_id, provider, repository, number, url, title, \
                            state, state_checked_at, created_by, created_at";

fn link_from_row(row: &QueryResult) -> Result<IssueLink, ErrorTrackingError> {
    Ok(IssueLink {
        id: row.try_get("", "id")?,
        group_id: row.try_get("", "error_group_id")?,
        provider: row.try_get("", "provider")?,
        repository: row.try_get("", "repository")?,
        number: row.try_get("", "number")?,
        url: row.try_get("", "url")?,
        title: row.try_get("", "title")?,
        state: row.try_get("", "state")?,
        state_checked_at: row.try_get("", "state_checked_at")?,
        created_by: row.try_get("", "created_by")?,
        created_at: row.try_get("", "created_at")?,
    })
}

fn truncate_chars(value: &str, max: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Escape text placed in a Markdown issue body so it renders as text.
fn markdown_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Check a pasted issue URL: absolute http(s), without credentials.
pub fn validate_issue_url(url: &str) -> Result<String, ErrorTrackingError> {
    let url = url.trim();
    if url.is_empty() || url.chars().count() > MAX_URL_CHARS {
        return Err(ErrorTrackingError::Validation(format!(
            "an issue URL is 1 to {MAX_URL_CHARS} characters"
        )));
    }
    let parsed = url::Url::parse(url).map_err(|error| {
        ErrorTrackingError::Validation(format!("'{url}' is not a URL: {error}"))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ErrorTrackingError::Validation(format!(
            "'{url}' is not an http or https URL"
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ErrorTrackingError::Validation(
            "an issue URL cannot carry a user name or password".into(),
        ));
    }
    Ok(parsed.to_string())
}

/// The issue number when `url` is an issue of `repository`:
/// `{web_url}/issues/{n}` on GitHub, `{web_url}/-/issues/{n}` on GitLab.
pub fn issue_number_in(repository: &ProjectRepository, url: &str) -> Option<i64> {
    let base = repository.web_url.trim_end_matches('/');
    let rest = url.strip_prefix(base)?;
    let rest = match repository.provider.as_str() {
        "github" => rest.strip_prefix("/issues/")?,
        "gitlab" => rest.strip_prefix("/-/issues/")?,
        _ => return None,
    };
    let digits: &str = rest.split(['/', '?', '#']).next()?;
    digits.parse::<i64>().ok().filter(|n| *n > 0)
}

/// The facts of a group an issue is opened from.
#[derive(Debug, Clone)]
pub struct IssueSource {
    pub group_id: i32,
    pub title: String,
    pub error_type: String,
    pub message: Option<String>,
    pub total_count: i32,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub frames: Vec<frames::AppFrame>,
    /// The project's `error_source_root`, which frame paths are relative to.
    pub source_root: Option<String>,
    /// The group's page in the console, when the server knows its URL.
    pub console_url: Option<String>,
}

/// The body of an issue opened from a group: what failed, how often, where,
/// and a link back. Only the exception and the application frames in files
/// of the repository; never request data, user data, tags or breadcrumbs,
/// nor a frame's URL, whose query string can carry a token or a user's id.
pub fn issue_body(source: &IssueSource) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "**{}**",
        markdown_text(&truncate_chars(&source.error_type, 200))
    ));
    if let Some(message) = source.message.as_deref().filter(|m| !m.trim().is_empty()) {
        body.push_str(&format!(
            ": {}",
            markdown_text(&truncate_chars(message, 1000))
        ));
    }
    body.push_str(&format!(
        "\n\n- Events: {}\n- First seen: {}\n- Last seen: {}\n",
        source.total_count,
        source.first_seen.format("%Y-%m-%d %H:%M UTC"),
        source.last_seen.format("%Y-%m-%d %H:%M UTC"),
    ));
    // The repository path of each frame in a file of the repository: what
    // a reader opens, without the bundler's `webpack:///` and the like.
    let located: Vec<(&frames::AppFrame, String)> = source
        .frames
        .iter()
        .filter_map(|frame| {
            frames::repository_path(&frame.filename, source.source_root.as_deref())
                .map(|file| (frame, file))
        })
        .take(ISSUE_BODY_FRAMES)
        .collect();
    if !located.is_empty() {
        body.push_str("\n**Stack (innermost first)**\n\n");
        for (frame, file) in located {
            let location = match frame.lineno {
                Some(line) => format!("{file}:{line}"),
                None => file,
            };
            body.push_str(&format!(
                "- `{}`{}\n",
                truncate_chars(&location, 300).replace('`', "'"),
                frame
                    .function
                    .as_deref()
                    .map(|f| format!(" in `{}`", truncate_chars(f, 200).replace('`', "'")))
                    .unwrap_or_default()
            ));
        }
    }
    match source.console_url.as_deref() {
        Some(url) => body.push_str(&format!(
            "\n[Error group {} in Temps]({url})\n",
            source.group_id
        )),
        None => body.push_str(&format!("\nError group {} in Temps.\n", source.group_id)),
    }
    body
}

fn source_control_reason(error: &SourceControlError) -> String {
    error.to_string()
}

impl SourceControlService {
    pub fn new(db: Arc<DatabaseConnection>, ownership: Arc<ownership::OwnershipService>) -> Self {
        Self {
            ownership,
            db,
            source_control: OnceLock::new(),
            issues_in_flight: Arc::default(),
        }
    }

    /// Use `source_control` to reach repositories. Set once at startup.
    pub fn set_source_control(&self, source_control: Arc<dyn ProjectSourceControl>) {
        if self.source_control.set(source_control).is_err() {
            tracing::debug!("source control was already set for error tracking");
        }
    }

    fn source_control(&self) -> Option<&Arc<dyn ProjectSourceControl>> {
        self.source_control.get()
    }

    async fn project_slug(&self, project_id: i32) -> Result<String, ErrorTrackingError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT slug FROM projects WHERE id = $1",
                [project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::ProjectNotFound)?;
        Ok(row.try_get("", "slug")?)
    }

    /// Whether the project's repository can be reached.
    pub async fn status(&self, project_id: i32) -> Result<SourceControlStatus, ErrorTrackingError> {
        let settings_path = || async {
            self.project_slug(project_id)
                .await
                .map(|slug| format!("/projects/{slug}/settings/git"))
        };
        let Some(source_control) = self.source_control() else {
            return Ok(SourceControlStatus {
                configured: false,
                provider: None,
                repository: None,
                web_url: None,
                reason: Some(
                    "This server has no git provider integration, so issues cannot be opened in a repository".into(),
                ),
                setup_path: Some(GIT_PROVIDERS_PATH.into()),
            });
        };
        match source_control.repository(project_id).await {
            Ok(Some(repository)) if repository.supported() => Ok(SourceControlStatus {
                configured: true,
                provider: Some(repository.provider),
                repository: Some(repository.repository),
                web_url: Some(repository.web_url),
                reason: None,
                setup_path: None,
            }),
            Ok(Some(repository)) => Ok(SourceControlStatus {
                configured: false,
                reason: Some(source_control_reason(&SourceControlError::Unsupported {
                    project_id,
                    provider: repository.provider.clone(),
                })),
                provider: Some(repository.provider),
                repository: Some(repository.repository),
                web_url: Some(repository.web_url),
                setup_path: Some(settings_path().await?),
            }),
            Ok(None) => Ok(SourceControlStatus {
                configured: false,
                provider: None,
                repository: None,
                web_url: None,
                reason: Some(source_control_reason(&SourceControlError::NotConnected {
                    project_id,
                })),
                setup_path: Some(settings_path().await?),
            }),
            Err(error) => Ok(SourceControlStatus {
                configured: false,
                provider: None,
                repository: None,
                web_url: None,
                reason: Some(source_control_reason(&error)),
                setup_path: Some(settings_path().await?),
            }),
        }
    }

    async fn require_group<C: ConnectionTrait>(
        db: &C,
        project_id: i32,
        group_id: i32,
    ) -> Result<(), ErrorTrackingError> {
        db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT 1 AS found FROM error_groups WHERE id = $1 AND project_id = $2",
            [group_id.into(), project_id.into()],
        ))
        .await?
        .map(|_| ())
        .ok_or(ErrorTrackingError::GroupNotInProject {
            project_id,
            group_id,
        })
    }

    /// The issues linked to a group, oldest first.
    pub async fn list_links(
        &self,
        project_id: i32,
        group_id: i32,
    ) -> Result<Vec<IssueLink>, ErrorTrackingError> {
        Self::require_group(self.db.as_ref(), project_id, group_id).await?;
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT {LINK_COLUMNS} FROM error_group_issue_links \
                     WHERE project_id = $1 AND error_group_id = $2 ORDER BY id LIMIT $3"
                ),
                [
                    project_id.into(),
                    group_id.into(),
                    (MAX_LINKS_PER_GROUP as i64).into(),
                ],
            ))
            .await?;
        rows.iter().map(link_from_row).collect()
    }

    /// Take the group's link lock for the rest of `txn` and refuse when the
    /// group has [`MAX_LINKS_PER_GROUP`] links. Two links added at once are
    /// counted one after the other, so neither goes past the limit. The
    /// lock is not the group row, which ingest updates.
    async fn reserve_link<C: ConnectionTrait>(
        txn: &C,
        group_id: i32,
    ) -> Result<(), ErrorTrackingError> {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext('error_group_issue_links'), $1)",
            [group_id.into()],
        ))
        .await?;
        let count: i64 = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS n FROM error_group_issue_links WHERE error_group_id = $1",
                [group_id.into()],
            ))
            .await?
            .map(|row| row.try_get("", "n"))
            .transpose()?
            .unwrap_or(0);
        if count >= MAX_LINKS_PER_GROUP as i64 {
            return Err(ErrorTrackingError::IssueLinkLimit {
                group_id,
                limit: MAX_LINKS_PER_GROUP,
            });
        }
        Ok(())
    }

    /// Store a link inside `txn`, refusing a duplicate URL or one past the
    /// limit, and record it in the group's activity.
    #[allow(
        clippy::too_many_arguments,
        reason = "one row of a link and who made it"
    )]
    async fn insert_link<C: ConnectionTrait>(
        txn: &C,
        project_id: i32,
        group_id: i32,
        provider: &str,
        repository: Option<&str>,
        number: Option<i64>,
        url: &str,
        title: &str,
        state: Option<&str>,
        actor_user_id: Option<i32>,
    ) -> Result<IssueLink, ErrorTrackingError> {
        Self::reserve_link(txn, group_id).await?;
        let row = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "INSERT INTO error_group_issue_links \
                     (project_id, error_group_id, provider, repository, number, url, title, state, \
                      state_checked_at, created_by) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
                             CASE WHEN $8::text IS NULL THEN NULL ELSE NOW() END, $9) \
                     ON CONFLICT (error_group_id, url) DO NOTHING \
                     RETURNING {LINK_COLUMNS}"
                ),
                [
                    project_id.into(),
                    group_id.into(),
                    provider.into(),
                    repository.map(str::to_string).into(),
                    number.into(),
                    url.into(),
                    title.into(),
                    state.map(str::to_string).into(),
                    actor_user_id.into(),
                ],
            ))
            .await?
            .ok_or_else(|| ErrorTrackingError::IssueAlreadyLinked {
                group_id,
                url: url.to_string(),
            })?;
        let link = link_from_row(&row)?;
        activity::record_many(
            txn,
            project_id,
            &[group_id],
            ActivityKind::IssueLinked,
            actor_user_id,
            &ActivityDetails {
                issue_url: Some(link.url.clone()),
                issue_title: Some(link.title.clone()),
                ..Default::default()
            },
        )
        .await?;
        Ok(link)
    }

    /// Link an issue by URL. An issue of the project's own GitHub or GitLab
    /// repository is recognised, so its state can be read; any other URL
    /// (Jira, Linear, another tracker) is stored as given.
    pub async fn link_issue(
        &self,
        project_id: i32,
        group_id: i32,
        url: &str,
        title: Option<&str>,
        actor_user_id: Option<i32>,
    ) -> Result<IssueLink, ErrorTrackingError> {
        let url = validate_issue_url(url)?;
        Self::require_group(self.db.as_ref(), project_id, group_id).await?;
        let title = title.map(str::trim).filter(|t| !t.is_empty());
        if let Some(title) = title {
            if title.chars().count() > MAX_ISSUE_TITLE_CHARS {
                return Err(ErrorTrackingError::Validation(format!(
                    "an issue title is at most {MAX_ISSUE_TITLE_CHARS} characters"
                )));
            }
        }
        // Recognise an issue of the project's repository and read it, so
        // the link shows its title and state. A failed read keeps the link.
        let mut tracked = None;
        if let Some(source_control) = self.source_control() {
            if let Ok(Some(repository)) = source_control.repository(project_id).await {
                if let Some(number) = issue_number_in(&repository, &url) {
                    let issue = source_control.issue(project_id, number).await;
                    if let Err(error) = &issue {
                        tracing::warn!(project_id, group_id, number, %error, "Linked issue could not be read; it is linked without its state");
                    }
                    tracked = Some((repository, number, issue.ok()));
                }
            }
        }
        let txn = self.db.begin().await?;
        let link = match &tracked {
            Some((repository, number, issue)) => {
                let title = title
                    .map(str::to_string)
                    .or_else(|| {
                        issue
                            .as_ref()
                            .map(|i| truncate_chars(&i.title, MAX_ISSUE_TITLE_CHARS))
                    })
                    .unwrap_or_else(|| format!("{} #{number}", repository.repository));
                Self::insert_link(
                    &txn,
                    project_id,
                    group_id,
                    &repository.provider,
                    Some(&repository.repository),
                    Some(*number),
                    &url,
                    &title,
                    issue.as_ref().map(|i| i.state.as_str()),
                    actor_user_id,
                )
                .await?
            }
            None => {
                let title = title
                    .map(str::to_string)
                    .unwrap_or_else(|| truncate_chars(&url, MAX_ISSUE_TITLE_CHARS));
                Self::insert_link(
                    &txn,
                    project_id,
                    group_id,
                    "link",
                    None,
                    None,
                    &url,
                    &title,
                    None,
                    actor_user_id,
                )
                .await?
            }
        };
        txn.commit().await?;
        Ok(link)
    }

    /// Open an issue for a group in the project's repository and link it.
    ///
    /// One issue per group and repository is opened while it is open: a
    /// second request answers that it exists. Two requests at once do not
    /// both open one; the second is refused while the first runs. No
    /// transaction is open while the provider answers, so a link added
    /// meanwhile can fill the group: the issue is then reported as opened
    /// but not linked ([`ErrorTrackingError::IssueOpenedNotLinked`]).
    pub async fn create_issue(
        &self,
        project_id: i32,
        group_id: i32,
        title: Option<&str>,
        actor_user_id: Option<i32>,
        console_base_url: Option<&str>,
    ) -> Result<IssueLink, ErrorTrackingError> {
        let source_control = self
            .source_control()
            .ok_or_else(|| ErrorTrackingError::SourceControlUnavailable {
                project_id,
                reason: "this server has no git provider integration".into(),
            })?
            .clone();
        let title = match title.map(str::trim).filter(|t| !t.is_empty()) {
            Some(title) if title.chars().count() > MAX_ISSUE_TITLE_CHARS => {
                return Err(ErrorTrackingError::Validation(format!(
                    "an issue title is at most {MAX_ISSUE_TITLE_CHARS} characters"
                )))
            }
            other => other.map(str::to_string),
        };
        let mut source = self.issue_source(project_id, group_id).await?;
        if let Some(base) = console_base_url {
            let slug = self.project_slug(project_id).await?;
            source.console_url = Some(format!(
                "{}/projects/{slug}/errors/{group_id}",
                base.trim_end_matches('/')
            ));
        }
        let repository = source_control
            .repository(project_id)
            .await
            .map_err(|error| ErrorTrackingError::SourceControl {
                project_id,
                source: error,
            })?
            .ok_or(ErrorTrackingError::SourceControl {
                project_id,
                source: SourceControlError::NotConnected { project_id },
            })?;
        if !repository.supported() {
            return Err(ErrorTrackingError::SourceControl {
                project_id,
                source: SourceControlError::Unsupported {
                    project_id,
                    provider: repository.provider,
                },
            });
        }

        // One request at a time opens an issue for a group; a concurrent one
        // is refused, not queued. The guard is in memory, so nothing in the
        // database is held while the provider answers.
        let Some(_in_flight) = IssueInFlight::claim(&self.issues_in_flight, group_id) else {
            return Err(ErrorTrackingError::IssueCreationInProgress { group_id });
        };
        // Refuse a group that already has an open issue in the repository,
        // or is full, before the issue is opened. This transaction is only
        // read and ends before the provider is called.
        {
            let txn = self.db.begin().await?;
            Self::reserve_link(&txn, group_id).await?;
            if let Some(row) = txn
                .query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT url FROM error_group_issue_links \
                     WHERE error_group_id = $1 AND provider = $2 AND repository = $3 \
                       AND COALESCE(state, 'open') = 'open' ORDER BY id LIMIT 1",
                    [
                        group_id.into(),
                        repository.provider.clone().into(),
                        repository.repository.clone().into(),
                    ],
                ))
                .await?
            {
                return Err(ErrorTrackingError::IssueAlreadyLinked {
                    group_id,
                    url: row.try_get("", "url")?,
                });
            }
            txn.rollback().await?;
        }
        let title = title.unwrap_or_else(|| truncate_chars(&source.title, MAX_ISSUE_TITLE_CHARS));
        let issue = source_control
            .create_issue(project_id, &title, &issue_body(&source))
            .await
            .map_err(|error| ErrorTrackingError::SourceControl {
                project_id,
                source: error,
            })?;
        tracing::info!(project_id, group_id, number = issue.number, repository = %repository.repository, "Opened an issue for an error group");
        // The issue exists now whatever happens here: a failure to store
        // its link says which issue it was, so it can be linked by URL
        // instead of opened again.
        let stored = async {
            let txn = self.db.begin().await?;
            let link = Self::insert_link(
                &txn,
                project_id,
                group_id,
                &repository.provider,
                Some(&repository.repository),
                Some(issue.number),
                &issue.url,
                &truncate_chars(&issue.title, MAX_ISSUE_TITLE_CHARS),
                Some(issue.state.as_str()),
                actor_user_id,
            )
            .await?;
            txn.commit().await?;
            Ok::<_, ErrorTrackingError>(link)
        }
        .await;
        stored.map_err(|error| {
            tracing::error!(project_id, group_id, number = issue.number, url = %issue.url, %error, "Opened an issue for an error group but could not link it");
            ErrorTrackingError::IssueOpenedNotLinked {
                group_id,
                number: issue.number,
                url: issue.url.clone(),
                source: Box::new(error),
            }
        })
    }

    /// Remove a link. The issue itself is left as it is.
    pub async fn unlink_issue(
        &self,
        project_id: i32,
        group_id: i32,
        link_id: i64,
        actor_user_id: Option<i32>,
    ) -> Result<IssueLink, ErrorTrackingError> {
        let txn = self.db.begin().await?;
        let row = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "DELETE FROM error_group_issue_links \
                     WHERE id = $1 AND project_id = $2 AND error_group_id = $3 \
                     RETURNING {LINK_COLUMNS}"
                ),
                [link_id.into(), project_id.into(), group_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::IssueLinkNotFound { group_id, link_id })?;
        let link = link_from_row(&row)?;
        activity::record_many(
            &txn,
            project_id,
            &[group_id],
            ActivityKind::IssueUnlinked,
            actor_user_id,
            &ActivityDetails {
                issue_url: Some(link.url.clone()),
                issue_title: Some(link.title.clone()),
                ..Default::default()
            },
        )
        .await?;
        txn.commit().await?;
        Ok(link)
    }

    /// Read again the state of the group's issues in the project's
    /// repository. A failed read keeps the last state; the links are
    /// returned either way, with the number not refreshed. An issue of
    /// another repository (the project moved, or the link was made before)
    /// cannot be read through this one and keeps its last state, counted
    /// as not refreshed.
    pub async fn refresh_links(
        &self,
        project_id: i32,
        group_id: i32,
    ) -> Result<(Vec<IssueLink>, usize), ErrorTrackingError> {
        let links = self.list_links(project_id, group_id).await?;
        let tracked: Vec<&IssueLink> = links
            .iter()
            .filter(|l| l.provider != "link" && l.number.is_some())
            .collect();
        if tracked.is_empty() {
            return Ok((links, 0));
        }
        let Some(source_control) = self.source_control() else {
            return Ok((links.clone(), tracked.len()));
        };
        let repository = match source_control.repository(project_id).await {
            Ok(repository) => repository,
            Err(error) => {
                tracing::warn!(project_id, group_id, %error, "Could not read the project's repository to refresh linked issues");
                None
            }
        };
        let mut failed = 0;
        for link in tracked {
            let Some(number) = link.number else { continue };
            let current = repository.as_ref().is_some_and(|repository| {
                repository.provider == link.provider
                    && link.repository.as_deref() == Some(repository.repository.as_str())
            });
            if !current {
                failed += 1;
                tracing::info!(project_id, group_id, link_id = link.id, provider = %link.provider, repository = ?link.repository, "A linked issue is not in the project's repository; its state is not refreshed");
                continue;
            }
            match source_control.issue(project_id, number).await {
                Ok(issue) => {
                    self.db
                        .execute(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            "UPDATE error_group_issue_links \
                             SET state = $2, title = $3, state_checked_at = NOW() WHERE id = $1",
                            [
                                link.id.into(),
                                issue.state.as_str().into(),
                                truncate_chars(&issue.title, MAX_ISSUE_TITLE_CHARS).into(),
                            ],
                        ))
                        .await?;
                }
                Err(error) => {
                    failed += 1;
                    tracing::warn!(project_id, group_id, link_id = link.id, %error, "Could not read the state of a linked issue");
                }
            }
        }
        Ok((self.list_links(project_id, group_id).await?, failed))
    }

    /// The group's title, counts and the frames of its latest event.
    async fn issue_source(
        &self,
        project_id: i32,
        group_id: i32,
    ) -> Result<IssueSource, ErrorTrackingError> {
        let group = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT g.title, g.error_type, g.message_template, g.total_count, g.first_seen, \
                        g.last_seen, e.exception_value, e.data, p.error_source_root \
                 FROM error_groups g \
                 JOIN projects p ON p.id = g.project_id \
                 LEFT JOIN LATERAL (SELECT exception_value, data FROM error_events \
                                    WHERE error_group_id = g.id AND project_id = g.project_id \
                                    ORDER BY timestamp DESC LIMIT 1) e ON TRUE \
                 WHERE g.id = $1 AND g.project_id = $2",
                [group_id.into(), project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::GroupNotInProject {
                project_id,
                group_id,
            })?;
        let data: Option<serde_json::Value> = group.try_get("", "data")?;
        let message: Option<String> = group.try_get("", "exception_value")?;
        let template: Option<String> = group.try_get("", "message_template")?;
        Ok(IssueSource {
            group_id,
            title: group.try_get("", "title")?,
            error_type: group.try_get("", "error_type")?,
            message: message.or(template),
            total_count: group.try_get("", "total_count")?,
            first_seen: group.try_get("", "first_seen")?,
            last_seen: group.try_get("", "last_seen")?,
            frames: data.as_ref().map(frames::app_frames).unwrap_or_default(),
            source_root: group.try_get("", "error_source_root")?,
            console_url: None,
        })
    }

    /// The commits that last touched the file of the latest event's
    /// innermost application frame, read at the event's release commit
    /// when the release is tied to a deployment, else on the default branch.
    pub async fn suspect_commits(
        &self,
        project_id: i32,
        group_id: i32,
    ) -> Result<SuspectCommits, ErrorTrackingError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT e.data, d.commit_sha, p.error_source_root \
                 FROM error_groups g \
                 JOIN projects p ON p.id = g.project_id \
                 LEFT JOIN LATERAL (SELECT data, deployment_id FROM error_events \
                                    WHERE error_group_id = g.id AND project_id = g.project_id \
                                    ORDER BY timestamp DESC LIMIT 1) e ON TRUE \
                 LEFT JOIN deployments d ON d.id = e.deployment_id \
                 WHERE g.id = $1 AND g.project_id = $2",
                [group_id.into(), project_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::GroupNotInProject {
                project_id,
                group_id,
            })?;
        let data: Option<serde_json::Value> = row.try_get("", "data")?;
        let commit: Option<String> = row.try_get("", "commit_sha")?;
        let source_root: Option<String> = row.try_get("", "error_source_root")?;
        let none = |reason: String| SuspectCommits {
            path: None,
            function: None,
            lineno: None,
            at: None,
            commits: Vec::new(),
            reason: Some(reason),
        };
        let frames = data.as_ref().map(frames::app_frames).unwrap_or_default();
        let Some((frame, path)) = frames.iter().find_map(|frame| {
            frames::repository_path(&frame.filename, source_root.as_deref())
                .map(|path| (frame, path))
        }) else {
            return Ok(none(
                "The latest event has no application frame in a file of the repository".into(),
            ));
        };
        let Some(source_control) = self.source_control() else {
            return Ok(none(
                "This server has no git provider integration, so commits cannot be read".into(),
            ));
        };
        let at = commit.filter(|sha| !sha.is_empty());
        let mut result = SuspectCommits {
            path: Some(path.clone()),
            function: frame.function.clone(),
            lineno: frame.lineno,
            at: at.clone(),
            commits: Vec::new(),
            reason: None,
        };
        match source_control
            .commits_touching(project_id, &path, at.as_deref(), MAX_SUSPECT_COMMITS)
            .await
        {
            Ok(commits) if commits.is_empty() => {
                result.reason = Some(format!("No commit in the repository touched {path}"));
            }
            Ok(commits) => result.commits = commits,
            Err(error) => result.reason = Some(error.to_string()),
        }
        Ok(result)
    }
}
