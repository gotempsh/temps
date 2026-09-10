// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Best-effort Dockerfile `EXPOSE` detection for pre-deployment configuration.

use std::collections::HashMap;

const MAX_DOCKERFILE_BYTES: usize = 1024 * 1024;

/// Return the first TCP port exposed by the Dockerfile's final stage.
///
/// Docker routes image metadata from the final stage, so ports declared only
/// in an earlier build stage must not become the project's default. Named
/// stage inheritance (`FROM base`) is followed because inherited `EXPOSE`
/// metadata is retained by the resulting image.
///
/// Only literal ports are returned. Variable expressions and port ranges are
/// intentionally ignored rather than guessed; the built image remains the
/// source of truth for those Dockerfiles.
pub fn detect_primary_exposed_port(dockerfile: &str) -> Option<u16> {
    if dockerfile.len() > MAX_DOCKERFILE_BYTES {
        return None;
    }

    let escape_character = dockerfile_escape_character(dockerfile);
    let mut named_stages: HashMap<String, Option<u16>> = HashMap::new();
    let mut current_alias: Option<String> = None;
    let mut current_port = None;
    let mut saw_from = false;

    for line in logical_lines(dockerfile, escape_character) {
        let Some((instruction, arguments)) = split_instruction(&line) else {
            continue;
        };

        if instruction.eq_ignore_ascii_case("FROM") {
            if saw_from {
                if let Some(alias) = current_alias.take() {
                    named_stages.insert(alias, current_port);
                }
            }

            let Some((base, alias)) = parse_from(arguments) else {
                continue;
            };
            current_port = named_stages
                .get(&base.to_ascii_lowercase())
                .copied()
                .unwrap_or_default();
            current_alias = alias.map(|value| value.to_ascii_lowercase());
            saw_from = true;
            continue;
        }

        if instruction.eq_ignore_ascii_case("EXPOSE") && current_port.is_none() {
            current_port = arguments
                .split_whitespace()
                .take_while(|token| !token.starts_with('#'))
                .find_map(parse_tcp_port);
        }
    }

    current_port
}

fn dockerfile_escape_character(dockerfile: &str) -> char {
    for raw_line in dockerfile.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            break;
        }
        let Some(comment) = trimmed.strip_prefix('#') else {
            break;
        };
        let Some((name, value)) = comment.trim().split_once('=') else {
            break;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "escape" => {
                return match value.trim() {
                    "`" => '`',
                    "\\" => '\\',
                    _ => '\\',
                };
            }
            // Docker permits multiple parser directives in the initial
            // directive block. Other comments end that block.
            "syntax" | "check" => continue,
            _ => break,
        }
    }
    '\\'
}

fn logical_lines(dockerfile: &str, escape_character: char) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for raw_line in dockerfile.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || (current.is_empty() && trimmed.starts_with('#')) {
            continue;
        }

        let continued = trimmed.ends_with(escape_character);
        let part = if continued {
            trimmed.trim_end_matches(escape_character).trim_end()
        } else {
            trimmed
        };

        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(part);

        if !continued {
            lines.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

fn split_instruction(line: &str) -> Option<(&str, &str)> {
    let instruction_end = line.find(char::is_whitespace).unwrap_or(line.len());
    let instruction = &line[..instruction_end];
    let arguments = line[instruction_end..].trim();
    (!instruction.is_empty() && !arguments.is_empty()).then_some((instruction, arguments))
}

fn parse_from(arguments: &str) -> Option<(&str, Option<&str>)> {
    let mut parts = arguments
        .split_whitespace()
        .skip_while(|part| part.starts_with("--"));
    let base = parts.next()?;
    let remaining: Vec<&str> = parts.collect();
    let alias = remaining.windows(2).find_map(|pair| {
        pair[0]
            .eq_ignore_ascii_case("AS")
            .then_some(pair[1])
    });
    Some((base, alias))
}

fn parse_tcp_port(token: &str) -> Option<u16> {
    let (port, protocol) = token.split_once('/').unwrap_or((token, "tcp"));
    if !protocol.eq_ignore_ascii_case("tcp") {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    (port > 0).then_some(port)
}

#[cfg(test)]
mod tests {
    use super::detect_primary_exposed_port;

    #[test]
    fn detects_literal_tcp_port_case_insensitively() {
        let dockerfile = "FROM alpine\nexpose 8080/tcp\n";

        assert_eq!(detect_primary_exposed_port(dockerfile), Some(8080));
    }

    #[test]
    fn uses_first_tcp_port_from_final_stage() {
        let dockerfile = r#"
            FROM rust:1 AS build
            EXPOSE 9000
            FROM debian:bookworm-slim
            EXPOSE 53/udp \
              8080/tcp 8443
        "#;

        assert_eq!(detect_primary_exposed_port(dockerfile), Some(8080));
    }

    #[test]
    fn ignores_ports_declared_only_in_build_stage() {
        let dockerfile = "FROM node:22 AS build\nEXPOSE 3000\nFROM nginx:alpine\n";

        assert_eq!(detect_primary_exposed_port(dockerfile), None);
    }

    #[test]
    fn preserves_expose_metadata_inherited_from_named_stage() {
        let dockerfile = r#"
            FROM alpine AS app-base
            EXPOSE 4321
            FROM app-base AS production
        "#;

        assert_eq!(detect_primary_exposed_port(dockerfile), Some(4321));
    }

    #[test]
    fn ignores_variable_ranges_and_invalid_ports() {
        let dockerfile = "FROM alpine\nEXPOSE $PORT 8000-8005 0 70000\n";

        assert_eq!(detect_primary_exposed_port(dockerfile), None);
    }

    #[test]
    fn stops_at_inline_comment_token() {
        let dockerfile = "FROM alpine\nEXPOSE 3000 # 4000 is documentation only\n";

        assert_eq!(detect_primary_exposed_port(dockerfile), Some(3000));
    }

    #[test]
    fn honors_backtick_escape_directive() {
        let dockerfile =
            "# syntax=docker/dockerfile:1\n# escape=`\nFROM alpine\nEXPOSE `\n  8080/tcp\n";

        assert_eq!(detect_primary_exposed_port(dockerfile), Some(8080));
    }

    #[test]
    fn ignores_escape_comments_after_the_parser_directive_block() {
        let dockerfile =
            "# ordinary comment\n# escape=`\nFROM alpine\nEXPOSE \\\n  8080/tcp\n";

        assert_eq!(detect_primary_exposed_port(dockerfile), Some(8080));
    }

    #[test]
    fn rejects_oversized_dockerfiles_before_parsing() {
        let dockerfile = format!("FROM alpine\nEXPOSE 8080\n{}", "#".repeat(1024 * 1024));

        assert_eq!(detect_primary_exposed_port(&dockerfile), None);
    }

    #[test]
    fn large_expose_lists_return_without_quadratic_deduplication() {
        let ports = (1..=u16::MAX)
            .map(|port| port.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let dockerfile = format!("FROM alpine\nEXPOSE {ports}\n");

        assert_eq!(detect_primary_exposed_port(&dockerfile), Some(1));
    }
}
