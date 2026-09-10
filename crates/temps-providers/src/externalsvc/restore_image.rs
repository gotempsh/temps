// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Validation for the restore-time `docker_image` override.
//!
//! Restoring into a *new* service clones the source service's root credentials
//! into the new container's environment. Whatever image receives them is
//! therefore as trusted as the database itself, and it is named by the caller
//! in `parameter_overrides`. Without a check, "restore this backup into a new
//! service, using image `attacker/exfil`" starts an attacker-chosen container
//! on the node with the source database's root password in its env.
//!
//! Shared by every engine that supports restore-to-new-service so the rule
//! cannot be fixed for one and silently missed for the others.

use anyhow::Result;
use tracing::info;

/// Operator-supplied additions to an engine's restore-image allowlist,
/// mirroring `TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES`.
///
/// Same contract as the PostgreSQL variable, for the same reason — which
/// repository this machine may pull and execute is operator policy, not
/// per-tenant config, so it stays host-level rather than an API setting:
///
/// - **Additive.** The built-in list is the floor; a typo here can never
///   strand an existing service or block a restore that worked yesterday.
/// - **Comma-separated**, trimmed, empties dropped.
/// - **Read once per process**, so it is changed by restarting Temps.
///
/// The one difference from the PostgreSQL variable is the unit. PostgreSQL
/// matches whole `image:tag` strings; the engines here match on the
/// *repository*, because a restore must be able to retag (restoring a 10.11
/// backup onto 11.4 is the normal case). An entry may therefore be written
/// either way — `ghcr.io/acme/mariadb` or `ghcr.io/acme/mariadb:11.4` — and the
/// tag is ignored, since allowing the repository is what the entry means.
pub fn extra_allowed_repositories(env_var: &str) -> Vec<String> {
    let Ok(raw) = std::env::var(env_var) else {
        return Vec::new();
    };
    let repositories = parse_extra_repositories(&raw);
    if !repositories.is_empty() {
        // Word not split across a format placeholder: the repo's typos hook
        // reads the fragment as a misspelling and "fixes" it.
        let noun = if repositories.len() == 1 {
            "repository"
        } else {
            "repositories"
        };
        info!(
            "{} allows {} additional restore image {}: {}",
            env_var,
            repositories.len(),
            noun,
            repositories.join(", ")
        );
    }
    repositories
}

/// Parse the operator-provided list. Split out from
/// [`extra_allowed_repositories`] so it is testable without mutating process
/// environment or racing the caller's `OnceLock`.
fn parse_extra_repositories(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        // Normalise to the repository, so an operator who writes a tag (the
        // natural thing to copy out of a compose file) is not silently ignored.
        .map(|entry| image_repository(entry).to_string())
        .collect()
}

/// Repository part of an image reference — everything before the tag/digest.
///
/// A `:` only introduces a tag when it appears after the last `/`, otherwise it
/// is a registry port (`registry.internal:5000/team/postgres`).
pub fn image_repository(image: &str) -> &str {
    let image = match image.split_once('@') {
        Some((repo, _digest)) => repo,
        None => image,
    };
    match image.rsplit_once(':') {
        Some((repo, tag)) if !repo.is_empty() && !tag.contains('/') => repo,
        _ => image,
    }
}

/// Validate a restore-time `docker_image` override.
///
/// The override is accepted when it names either the same repository the source
/// service already runs, or one of `allowed_repositories` — the engine's own
/// known-good images. Only the tag is ever the caller's free choice.
///
/// Matching is exact repository equality, never a prefix test: `postgres-evil`
/// must not be accepted because it starts with `postgres`, and `evil/postgres`
/// must not be accepted because it ends with one.
pub fn restore_image_override<'a>(
    source_image: &str,
    requested: &'a str,
    allowed_repositories: &[&str],
) -> Result<&'a str> {
    restore_image_override_with_extra(source_image, requested, allowed_repositories, &[], None)
}

/// [`restore_image_override`] with the operator's additions supplied
/// explicitly, so the composition is testable without mutating the process
/// environment or racing a `OnceLock`.
///
/// `env_var` is named only to make the rejection message actionable — a
/// self-hosted operator has no support channel, so the error has to say which
/// variable widens the list rather than reading as "Temps cannot run your
/// image".
pub fn restore_image_override_with_extra<'a>(
    source_image: &str,
    requested: &'a str,
    allowed_repositories: &[&str],
    extra_repositories: &[String],
    env_var: Option<&str>,
) -> Result<&'a str> {
    let requested_repo = image_repository(requested);
    if requested_repo.is_empty() {
        return Err(anyhow::anyhow!(
            "Invalid docker_image override '{}': no repository",
            requested
        ));
    }
    if requested_repo == image_repository(source_image)
        || allowed_repositories.contains(&requested_repo)
        || extra_repositories.iter().any(|r| r == requested_repo)
    {
        return Ok(requested);
    }

    let mut known: Vec<String> = allowed_repositories.iter().map(|r| r.to_string()).collect();
    known.extend(extra_repositories.iter().cloned());
    Err(anyhow::anyhow!(
        "docker_image override '{}' is not permitted for a restore: the new service inherits the \
         source service's credentials, so the image must stay on the source's repository ('{}') \
         or one of: {}.{}",
        requested,
        image_repository(source_image),
        known.join(", "),
        match env_var {
            Some(var) => format!(
                " To allow additional repositories on this instance, set {var} to a \
                 comma-separated list (e.g. {var}={requested_repo}) and restart temps."
            ),
            None => String::new(),
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWED: &[&str] = &["postgres", "gotempsh/postgres-walg"];

    #[test]
    fn repository_strips_tags_and_digests_but_not_registry_ports() {
        assert_eq!(image_repository("postgres:17"), "postgres");
        assert_eq!(image_repository("postgres"), "postgres");
        assert_eq!(image_repository("postgres@sha256:abc123"), "postgres");
        // The `:5000` is a registry port, not a tag — the repository is the
        // whole path after it.
        assert_eq!(
            image_repository("registry.internal:5000/team/postgres"),
            "registry.internal:5000/team/postgres"
        );
        assert_eq!(
            image_repository("registry.internal:5000/team/postgres:17"),
            "registry.internal:5000/team/postgres"
        );
    }

    #[test]
    fn allowed_repositories_and_the_source_repository_are_accepted() {
        assert!(restore_image_override("postgres:16", "postgres:17", ALLOWED).is_ok());
        assert!(
            restore_image_override("postgres:16", "gotempsh/postgres-walg:17", ALLOWED).is_ok()
        );
        // Same repository as the source, even though it is not on the list.
        assert!(restore_image_override(
            "registry.internal:5000/team/pg:16",
            "registry.internal:5000/team/pg:17",
            ALLOWED
        )
        .is_ok());
    }

    /// The operator escape hatch: a private-registry image an instance
    /// legitimately runs is allowed once the operator says so, and only then.
    #[test]
    fn operator_additions_widen_the_allowlist_and_nothing_else() {
        let extra = vec!["ghcr.io/acme/postgres".to_string()];

        // Rejected without the addition...
        assert!(
            restore_image_override("postgres:16", "ghcr.io/acme/postgres:17", ALLOWED).is_err()
        );
        // ...accepted with it, and the tag stays the caller's choice.
        assert!(restore_image_override_with_extra(
            "postgres:16",
            "ghcr.io/acme/postgres:17",
            ALLOWED,
            &extra,
            None
        )
        .is_ok());

        // An addition widens exactly one repository — not its lookalikes.
        for hostile in ["ghcr.io/acme/postgres-evil:17", "ghcr.io/evil/postgres:17"] {
            assert!(
                restore_image_override_with_extra("postgres:16", hostile, ALLOWED, &extra, None)
                    .is_err(),
                "{hostile} must still be rejected"
            );
        }
    }

    /// A rejection has to name the variable that widens the list — a
    /// self-hosted operator has no one to ask.
    #[test]
    fn rejection_names_the_operator_escape_hatch() {
        let err = restore_image_override_with_extra(
            "postgres:16",
            "ghcr.io/acme/postgres:17",
            ALLOWED,
            &[],
            Some("TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES"),
            "{err}"
        );
        assert!(err.contains("restart temps"), "{err}");
    }

    /// Operators copy `repo:tag` out of a compose file; that must not silently
    /// do nothing, since these engines allow the tag to vary anyway.
    #[test]
    fn operator_entries_are_normalised_to_repositories() {
        assert_eq!(
            parse_extra_repositories("ghcr.io/acme/mariadb:11.4, ghcr.io/acme/mongo ,, "),
            vec![
                "ghcr.io/acme/mariadb".to_string(),
                "ghcr.io/acme/mongo".to_string()
            ]
        );
        // A registry port is not a tag, so it survives normalisation.
        assert_eq!(
            parse_extra_repositories("registry.internal:5000/team/mariadb"),
            vec!["registry.internal:5000/team/mariadb".to_string()]
        );
        assert!(parse_extra_repositories("  ,, ").is_empty());
    }

    /// The whole point: an attacker-named image would receive the source
    /// database's root credentials.
    #[test]
    fn lookalike_repositories_are_rejected() {
        for hostile in [
            "postgres-evil:17",              // prefix of an allowed name
            "evil/postgres:17",              // suffix of an allowed name
            "docker.io/library/postgres:17", // fully-qualified is a different repository
            "attacker/exfil:latest",
            ":17",
        ] {
            assert!(
                restore_image_override("postgres:16", hostile, ALLOWED).is_err(),
                "{hostile} must be rejected"
            );
        }
    }
}
