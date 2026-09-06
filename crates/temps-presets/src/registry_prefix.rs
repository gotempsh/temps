// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rewrites `FROM` lines in a generated Dockerfile through an
//! operator-configured registry prefix.
//!
//! Every preset (including autopack) writes multi-stage Dockerfiles where a
//! later stage's `FROM` can name either a real image (`FROM node:22-slim`) or
//! an earlier stage (`FROM autopack-build`). Only the former should ever be
//! rewritten — renaming a stage reference would break the build outright.
//! This tracks stage names declared via `AS <name>` as it walks the file and
//! only rewrites `FROM` references that are neither a known stage nor already
//! qualified to some other registry (see [`temps_core::registry_prefix`]).

use temps_core::registry_prefix::qualify_with_registry_prefix;

/// Rewrite every `FROM <image>` line in `dockerfile` whose `<image>` is an
/// implicit Docker Hub reference, prepending `prefix`. `FROM <stage>` lines
/// referencing an earlier build stage are left untouched, as is any image
/// already qualified to another registry.
pub fn apply_registry_prefix(dockerfile: &str, prefix: &str) -> String {
    let mut stage_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = String::with_capacity(dockerfile.len());
    let ends_with_newline = dockerfile.ends_with('\n');

    for line in dockerfile.lines() {
        out.push_str(&rewrite_line(line, prefix, &mut stage_names));
        out.push('\n');
    }

    if !ends_with_newline {
        out.pop();
    }
    out
}

/// Rewrite a single line if it is a `FROM` directive, recording any stage
/// name it declares along the way.
fn rewrite_line(
    line: &str,
    prefix: &str,
    stage_names: &mut std::collections::HashSet<String>,
) -> String {
    let trimmed = line.trim_start();
    let Some(rest) = strip_from_prefix(trimmed) else {
        return line.to_string();
    };
    let leading_ws = &line[..line.len() - trimmed.len()];

    let mut parts = rest.split_whitespace();
    let Some(image) = parts.next() else {
        return line.to_string();
    };

    let stage_name = match parts.next() {
        Some(word) if word.eq_ignore_ascii_case("as") => parts.next(),
        _ => None,
    };

    // A build-arg reference (`FROM ${BASE_IMAGE}`) is not a literal image
    // name -- rewriting it would bake the prefix onto whatever the ARG
    // expands to at build time, not onto docker.io. No built-in preset emits
    // this today, but a project's own `.temps.yaml`-influenced Dockerfile
    // reasonably could.
    let new_image = if stage_names.contains(image) || image.starts_with('$') {
        image.to_string()
    } else {
        qualify_with_registry_prefix(image, Some(prefix))
    };

    if let Some(stage) = stage_name {
        stage_names.insert(stage.to_string());
        format!("{leading_ws}FROM {new_image} AS {stage}")
    } else {
        format!("{leading_ws}FROM {new_image}")
    }
}

/// Case-insensitively strip a leading `FROM ` directive keyword, the way
/// Docker itself parses it (Dockerfile directives are case-insensitive by
/// spec even though every generator in this codebase emits them uppercase).
fn strip_from_prefix(trimmed: &str) -> Option<&str> {
    let bytes = trimmed.as_bytes();
    if bytes.len() < 5 || !bytes[..4].eq_ignore_ascii_case(b"FROM") {
        return None;
    }
    if !bytes[4].is_ascii_whitespace() {
        return None;
    }
    Some(trimmed[5..].trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_a_single_stage_dockerfile() {
        let dockerfile = "FROM node:22-slim AS app\nRUN npm install\n";
        let rewritten = apply_registry_prefix(dockerfile, "registry.example.com/docker");
        assert_eq!(
            rewritten,
            "FROM registry.example.com/docker/node:22-slim AS app\nRUN npm install\n"
        );
    }

    #[test]
    fn leaves_stage_references_untouched_across_a_multi_stage_build() {
        let dockerfile = "\
FROM debian:bookworm-slim AS autopack-packages
RUN apt-get update
FROM autopack-packages AS autopack-install
RUN npm ci
FROM autopack-install AS autopack-build
RUN npm run build
FROM debian:bookworm-slim AS final
COPY --from=autopack-build /app /app
";
        let rewritten = apply_registry_prefix(dockerfile, "registry.example.com/docker");

        assert!(rewritten.contains(
            "FROM registry.example.com/docker/debian:bookworm-slim AS autopack-packages"
        ));
        // Stage references must survive verbatim -- rewriting these would
        // point the build at an image that was never built.
        assert!(rewritten.contains("FROM autopack-packages AS autopack-install"));
        assert!(rewritten.contains("FROM autopack-install AS autopack-build"));
        // A later stage reusing the same base image is rewritten independently.
        assert!(rewritten.contains("FROM registry.example.com/docker/debian:bookworm-slim AS final"));
    }

    #[test]
    fn does_not_rewrite_images_already_qualified_to_another_registry() {
        let dockerfile = "FROM ghcr.io/gotempsh/temps-base:latest AS app\n";
        let rewritten = apply_registry_prefix(dockerfile, "registry.example.com/docker");
        assert_eq!(rewritten, dockerfile);
    }

    #[test]
    fn preserves_leading_indentation_and_bare_from_without_a_stage_name() {
        let dockerfile = "  FROM node:22-slim\n";
        let rewritten = apply_registry_prefix(dockerfile, "registry.example.com/docker");
        assert_eq!(
            rewritten,
            "  FROM registry.example.com/docker/node:22-slim\n"
        );
    }

    #[test]
    fn preserves_a_missing_trailing_newline() {
        let dockerfile = "FROM node:22-slim AS app";
        let rewritten = apply_registry_prefix(dockerfile, "registry.example.com/docker");
        assert_eq!(
            rewritten,
            "FROM registry.example.com/docker/node:22-slim AS app"
        );
    }

    #[test]
    fn does_not_rewrite_a_build_arg_base_image() {
        let dockerfile = "ARG BASE_IMAGE=node:22-slim\nFROM ${BASE_IMAGE} AS app\n";
        let rewritten = apply_registry_prefix(dockerfile, "registry.example.com/docker");
        assert_eq!(rewritten, dockerfile);
    }

    #[test]
    fn ignores_non_from_lines_entirely() {
        let dockerfile = "# a comment mentioning FROM inside text\nRUN echo hi\n";
        let rewritten = apply_registry_prefix(dockerfile, "registry.example.com/docker");
        assert_eq!(rewritten, dockerfile);
    }
}
