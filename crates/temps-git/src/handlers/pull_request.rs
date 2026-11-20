// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pull request deliveries shared by the GitHub and Gitea receivers, which
//! send the same `pull_request` payload shape.

use serde::Deserialize;

/// The branch of a pull request a GitHub or Gitea `pull_request` delivery
/// reports as closed or merged, and which of the two. `None` for any other
/// action, and for a pull request from a fork: its branch lives in another
/// repository, so a same-named preview here belongs to a branch of this
/// repository, not to it.
pub(crate) fn closed_pull_request(
    body: &[u8],
) -> Option<(String, temps_core::PreviewBranchCloseReason)> {
    #[derive(Deserialize)]
    struct Payload {
        action: String,
        number: i64,
        pull_request: PullRequest,
    }

    #[derive(Deserialize)]
    struct PullRequest {
        #[serde(default)]
        merged: bool,
        head: Side,
        base: Side,
    }

    #[derive(Deserialize)]
    struct Side {
        #[serde(rename = "ref")]
        git_ref: String,
        repo: Option<SideRepo>,
    }

    #[derive(Deserialize)]
    struct SideRepo {
        full_name: String,
    }

    let payload: Payload = serde_json::from_slice(body).ok()?;
    if payload.action != "closed" {
        return None;
    }

    let pull_request = payload.pull_request;
    let head = pull_request.head.repo?.full_name;
    let base = pull_request.base.repo?.full_name;
    if !head.eq_ignore_ascii_case(&base) {
        return None;
    }

    let reason = if pull_request.merged {
        temps_core::PreviewBranchCloseReason::PullRequestMerged {
            number: payload.number,
        }
    } else {
        temps_core::PreviewBranchCloseReason::PullRequestClosed {
            number: payload.number,
        }
    };
    Some((pull_request.head.git_ref, reason))
}

#[cfg(test)]
mod tests {
    use super::closed_pull_request;
    use temps_core::PreviewBranchCloseReason;

    fn delivery(action: &str, merged: bool, head: serde_json::Value) -> Vec<u8> {
        serde_json::json!({
            "action": action,
            "number": 12,
            "pull_request": {
                "merged": merged,
                "head": { "ref": "feature/login", "repo": head },
                "base": { "ref": "main", "repo": { "full_name": "example-owner/example-repository" } }
            }
        })
        .to_string()
        .into_bytes()
    }

    fn same_repository() -> serde_json::Value {
        serde_json::json!({ "full_name": "Example-Owner/example-repository" })
    }

    #[test]
    fn a_merged_pull_request_retires_its_branch() {
        assert_eq!(
            closed_pull_request(&delivery("closed", true, same_repository())),
            Some((
                "feature/login".to_string(),
                PreviewBranchCloseReason::PullRequestMerged { number: 12 }
            ))
        );
    }

    #[test]
    fn a_pull_request_closed_unmerged_retires_its_branch() {
        assert_eq!(
            closed_pull_request(&delivery("closed", false, same_repository())),
            Some((
                "feature/login".to_string(),
                PreviewBranchCloseReason::PullRequestClosed { number: 12 }
            ))
        );
    }

    #[test]
    fn other_pull_request_actions_retire_nothing() {
        for action in ["opened", "synchronize", "reopened", "edited"] {
            assert_eq!(
                closed_pull_request(&delivery(action, false, same_repository())),
                None
            );
        }
    }

    /// A fork's branch is not this repository's branch, even with the same
    /// name, and a deleted fork leaves no head repository at all.
    #[test]
    fn a_pull_request_from_a_fork_retires_nothing() {
        let fork = serde_json::json!({ "full_name": "someone-else/example-repository" });
        assert_eq!(closed_pull_request(&delivery("closed", true, fork)), None);
        assert_eq!(
            closed_pull_request(&delivery("closed", true, serde_json::Value::Null)),
            None
        );
    }

    #[test]
    fn a_malformed_delivery_retires_nothing() {
        assert_eq!(closed_pull_request(b"not json"), None);
        assert_eq!(closed_pull_request(br#"{"action":"closed"}"#), None);
    }
}
