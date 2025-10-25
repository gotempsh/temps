//! ProjectSource implementations for GitHub and GitLab
//!
//! These implementations allow framework detection and preset configuration
//! to work directly with GitHub and GitLab APIs without cloning repositories

pub mod github_source;
pub mod gitlab_source;

pub use github_source::GitHubSource;
pub use gitlab_source::GitLabSource;
