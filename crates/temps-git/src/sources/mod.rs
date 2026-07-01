//! ProjectSource implementations for GitHub, GitLab, Gitea, and Bitbucket
//!
//! These implementations allow framework detection and preset configuration
//! to work directly with git provider APIs without cloning repositories

pub mod bitbucket_source;
pub mod gitea_source;
pub mod github_source;
pub mod gitlab_source;

pub use bitbucket_source::BitbucketSource;
pub use gitea_source::GiteaSource;
pub use github_source::GitHubSource;
pub use gitlab_source::GitLabSource;
