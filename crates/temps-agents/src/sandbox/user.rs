//! Single source of truth for the sandbox container's user, home, and
//! work directory. Keep every `/home/...`, `temps:temps`, and `/workspace`
//! literal funneled through these constants — a future image with a
//! different user only needs changes here.
//!
//! Constants (not config) on purpose: the values are baked into the
//! Dockerfile this crate generates, so they have to be known at compile
//! time anyway. Promoting to runtime config would require regenerating
//! the image per-deployment, which we don't want.

/// Non-root user inside the sandbox container.
pub const SANDBOX_USER: &str = "temps";

/// Group inside the sandbox container. Matches `SANDBOX_USER` because the
/// Dockerfile's `useradd -m` creates a same-named primary group.
pub const SANDBOX_GROUP: &str = "temps";

/// `chown` argument string — `user:group`.
pub const SANDBOX_CHOWN: &str = "temps:temps";

/// Home directory of [`SANDBOX_USER`].
pub const SANDBOX_HOME: &str = "/home/temps";

/// Path inside the container where the project's repository is mounted.
/// Lives under `SANDBOX_HOME` so per-user images route automatically when
/// the user changes.
pub const SANDBOX_WORK_DIR: &str = "/home/temps/workspace";

#[cfg(test)]
mod tests {
    use super::*;

    /// The composite paths must stay in sync if anyone edits one without
    /// the others. Cheap compile-time-ish guard against drift.
    #[test]
    fn paths_are_consistent() {
        assert_eq!(SANDBOX_HOME, format!("/home/{}", SANDBOX_USER));
        assert_eq!(SANDBOX_CHOWN, format!("{}:{}", SANDBOX_USER, SANDBOX_GROUP));
        assert!(
            SANDBOX_WORK_DIR.starts_with(SANDBOX_HOME),
            "work dir should live under sandbox home so images with a different user route automatically"
        );
    }
}
