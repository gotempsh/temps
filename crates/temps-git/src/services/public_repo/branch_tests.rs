// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Branch listing truncation and the default single-branch lookup, kept
//! beside `public_repo.rs` so the provider file stays a readable size.

use super::*;

#[tokio::test]
async fn github_branch_listing_cut_short_by_the_cap_reports_itself_truncated() {
    let mut server = mockito::Server::new_async().await;
    // One oversized page: the listing stops at the cap with branches the
    // repository has but this response never covered.
    let body: Vec<serde_json::Value> = (0..=MAX_PUBLIC_BRANCHES)
        .map(|index| {
            serde_json::json!({
                "name": format!("branch-{index}"),
                "commit": { "sha": format!("{index:040x}") },
                "protected": false
            })
        })
        .collect();
    let page = server
        .mock(
            "GET",
            mockito::Matcher::Regex("^/repos/example/monorepo/branches\\?".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::Value::Array(body).to_string())
        .expect(1)
        .create_async()
        .await;
    let provider = GitHubPublicProvider::with_token_and_api_url("token".to_string(), server.url());

    let listed = provider
        .list_branches("example", "monorepo")
        .await
        .expect("a capped listing still succeeds");

    assert_eq!(listed.branches.len(), MAX_PUBLIC_BRANCHES);
    assert!(
        listed.truncated,
        "a listing stopped by the cap must say branches were left unread"
    );
    page.assert_async().await;
}

#[tokio::test]
async fn github_branch_listing_the_provider_finished_reports_itself_complete() {
    let mut server = mockito::Server::new_async().await;
    let page = server
        .mock(
            "GET",
            mockito::Matcher::Regex("^/repos/example/monorepo/branches\\?".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!([
                { "name": "main", "commit": { "sha": "abc123" }, "protected": true }
            ])
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;
    let provider = GitHubPublicProvider::with_token_and_api_url("token".to_string(), server.url());

    let listed = provider
        .list_branches("example", "monorepo")
        .await
        .expect("a short page is the whole listing");

    assert_eq!(listed.branches.len(), 1);
    assert!(!listed.truncated);
    page.assert_async().await;
}

/// A provider with no single-branch endpoint falls back to scanning the
/// list. From a truncated one it must refuse to answer rather than turn
/// "unread" into "absent".
#[tokio::test]
async fn the_default_branch_lookup_refuses_to_answer_from_a_capped_list() {
    struct CappedListingOnly;

    #[async_trait]
    impl PublicRepoProvider for CappedListingOnly {
        fn provider_name(&self) -> &'static str {
            "capped"
        }

        async fn get_repository(
            &self,
            _owner: &str,
            _repo: &str,
        ) -> Result<PublicRepoInfo, PublicRepoError> {
            Err(PublicRepoError::Internal("not used".into()))
        }

        async fn list_branches(
            &self,
            _owner: &str,
            _repo: &str,
        ) -> Result<PublicBranchList, PublicRepoError> {
            Ok(PublicBranchList::truncated(vec![PublicBranch {
                name: "branch-0".to_string(),
                commit_sha: "abc123".to_string(),
                protected: false,
            }]))
        }

        async fn get_file_tree(
            &self,
            _owner: &str,
            _repo: &str,
            _reference: &str,
        ) -> Result<Vec<String>, PublicRepoError> {
            Err(PublicRepoError::Internal("not used".into()))
        }

        async fn get_file_content(
            &self,
            _owner: &str,
            _repo: &str,
            _path: &str,
            _reference: &str,
        ) -> Result<FileContent, PublicRepoError> {
            Err(PublicRepoError::Internal("not used".into()))
        }
    }

    let provider = CappedListingOnly;

    let found = provider
        .get_branch("example", "monorepo", "branch-0")
        .await
        .expect("a name present in the prefix is still an answer");
    assert_eq!(found.map(|branch| branch.name).as_deref(), Some("branch-0"));

    let error = provider
        .get_branch("example", "monorepo", "release/2026")
        .await
        .expect_err("a capped list cannot rule a branch out");
    match error {
        PublicRepoError::BranchLookupUnavailable {
            repository,
            branch,
            listed,
        } => {
            assert_eq!(repository, "example/monorepo");
            assert_eq!(branch, "release/2026");
            assert_eq!(listed, 1);
        }
        other => panic!("expected an unverifiable lookup, got {other}"),
    }
}
