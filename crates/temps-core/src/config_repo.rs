// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which repositories an agent's `config_repo` may name.
//!
//! An agent's config repository is cloned with the project's git connection
//! and its `.claude/` directory is overlaid into the run's sandbox. The name
//! comes from the project's own `.temps.yaml` (or an API write by a project
//! member), so it is not trusted to point anywhere: it may name the project's
//! own repository or one the administrator has listed, and nothing else.

use thiserror::Error;

/// A repository as `owner/repo`, lowercased (hosts compare names without case).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// Why a config repository is not used.
#[derive(Error, Debug, PartialEq, Eq)]
pub enum ConfigRepoRefusal {
    #[error("'{value}' is not an 'owner/repo' repository name")]
    Malformed { value: String },

    #[error(
        "config repository '{repo}' is neither this project's repository ('{project_repo}') nor \
         on the administrator's list of allowed config repositories (Workflow sandbox settings); \
         it was not used"
    )]
    NotAllowed { repo: String, project_repo: String },
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Parse `owner/repo`, also accepting a `https://github.com/owner/repo(.git)`
/// style URL. Returns `None` for anything else, including extra path
/// segments, credentials and parent-directory components.
pub fn parse_repo_ref(value: &str) -> Option<RepoRef> {
    let mut rest = value.trim();
    for prefix in ["https://github.com/", "http://github.com/", "github.com/"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            break;
        }
    }
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    if !valid_segment(owner) || !valid_segment(repo) {
        return None;
    }
    Some(RepoRef {
        owner: owner.to_ascii_lowercase(),
        repo: repo.to_ascii_lowercase(),
    })
}

/// Decide whether `candidate` may be used as a config repository.
///
/// Allowed: the project's own repository, or an entry of the administrator's
/// `allowlist`. An allowlist entry is `owner/repo`, or `owner/*` for every
/// repository of one owner.
pub fn check_config_repo(
    candidate: &str,
    project_owner: &str,
    project_repo: &str,
    allowlist: &[String],
) -> Result<RepoRef, ConfigRepoRefusal> {
    let repo = parse_repo_ref(candidate).ok_or_else(|| ConfigRepoRefusal::Malformed {
        value: candidate.trim().escape_debug().to_string(),
    })?;

    let own = RepoRef {
        owner: project_owner.to_ascii_lowercase(),
        repo: project_repo.to_ascii_lowercase(),
    };
    if repo == own {
        return Ok(repo);
    }
    let listed = allowlist.iter().any(|entry| {
        let entry = entry.trim();
        match entry.strip_suffix("/*") {
            Some(owner) => valid_segment(owner) && owner.eq_ignore_ascii_case(&repo.owner),
            None => parse_repo_ref(entry).is_some_and(|allowed| allowed == repo),
        }
    });
    if listed {
        return Ok(repo);
    }
    Err(ConfigRepoRefusal::NotAllowed {
        repo: repo.to_string(),
        project_repo: own.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(candidate: &str, allowlist: &[&str]) -> bool {
        let allowlist: Vec<String> = allowlist.iter().map(|s| s.to_string()).collect();
        check_config_repo(candidate, "Acme", "Shop", &allowlist).is_ok()
    }

    #[test]
    fn repository_names_are_parsed_strictly() {
        assert_eq!(
            parse_repo_ref(" Acme/Claude-Config "),
            Some(RepoRef {
                owner: "acme".into(),
                repo: "claude-config".into()
            })
        );
        assert_eq!(
            parse_repo_ref("https://github.com/acme/cfg.git"),
            parse_repo_ref("acme/cfg")
        );
        for bad in [
            "",
            "acme",
            "acme/",
            "/cfg",
            "acme/cfg/extra",
            "../cfg",
            "acme/..",
            "acme/cf g",
            "https://user:token@github.com/acme/cfg",
            "https://evil.example/acme/cfg",
            "acme/cfg?x=1",
        ] {
            assert_eq!(parse_repo_ref(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_projects_own_repository_is_always_allowed() {
        assert!(allowed("acme/shop", &[]));
        assert!(allowed("ACME/Shop", &[]));
        assert!(allowed("https://github.com/acme/shop.git", &[]));
    }

    #[test]
    fn another_repository_needs_the_administrators_list() {
        assert!(!allowed("acme/private-notes", &[]));
        assert!(!allowed("other/cfg", &["acme/private-notes"]));
        assert!(allowed("acme/private-notes", &["Acme/Private-Notes"]));
        assert!(allowed("platform/claude-config", &["platform/*"]));
        assert!(allowed("Platform/Other", &["platform/*"]));
        // A wildcard names one owner, not every owner.
        assert!(!allowed("platformx/cfg", &["platform/*"]));
        assert!(!allowed("other/cfg", &["*/*", "*"]));
    }

    #[test]
    fn a_refusal_names_the_repository_and_how_to_allow_it() {
        let refusal = check_config_repo("other/cfg", "acme", "shop", &[]).unwrap_err();
        let message = refusal.to_string();
        assert!(message.contains("other/cfg"), "{message}");
        assert!(message.contains("acme/shop"), "{message}");
        assert!(message.contains("administrator"), "{message}");
        assert!(matches!(
            check_config_repo("not a repo", "acme", "shop", &[]),
            Err(ConfigRepoRefusal::Malformed { .. })
        ));
    }
}
