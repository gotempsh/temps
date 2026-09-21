// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use regex::bytes::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Detection output deliberately contains neither the secret nor matching substrings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Candidate {
    pub id: String,
    pub description: String,
    pub evidence: DetectionEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DetectionEvidence {
    ValuePattern,
    VariableName,
}

/// Identification is local and must never send candidates to external services.
pub trait CredentialDetector: Send + Sync {
    fn detect(&self, name: &str, value: &str) -> Vec<Candidate>;
}

#[derive(Debug, thiserror::Error)]
pub enum DetectionError {
    #[error("Credential catalog could not be parsed")]
    Catalog(#[source] toml::de::Error),
    #[error("Credential detection rule '{id}' has an unsupported regular expression: {source}")]
    Pattern {
        id: String,
        #[source]
        source: regex::Error,
    },
}

#[derive(Deserialize)]
struct Catalog {
    rules: Vec<Rule>,
}
#[derive(Deserialize)]
struct Rule {
    id: String,
    description: String,
    regex: Option<String>,
    #[serde(default)]
    entropy: f64,
    #[serde(default, rename = "secretGroup")]
    secret_group: Option<usize>,
    #[serde(default)]
    keywords: Vec<String>,
}
struct CompiledRule {
    rule: Rule,
    pattern: Regex,
}

/// Adapts the pinned MIT-licensed Gitleaks expressions as *candidate hints*.
/// Repository-specific allowlists/path rules are not a claim of full Gitleaks parity.
/// The host separately selects a reviewed automatic policy or an explicit verifier.
pub struct CatalogDetector {
    rules: Vec<CompiledRule>,
}
impl CatalogDetector {
    pub fn bundled() -> Result<Self, DetectionError> {
        let catalog: Catalog = toml::from_str(include_str!("../catalog/gitleaks.toml"))
            .map_err(DetectionError::Catalog)?;
        let mut rules = Vec::with_capacity(catalog.rules.len());
        for rule in catalog.rules {
            let Some(expression) = &rule.regex else {
                continue;
            };
            let pattern = RegexBuilder::new(expression)
                .unicode(false)
                .build()
                .map_err(|source| DetectionError::Pattern {
                    id: rule.id.clone(),
                    source,
                })?;
            rules.push(CompiledRule { rule, pattern });
        }
        Ok(Self { rules })
    }
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}
impl CredentialDetector for CatalogDetector {
    fn detect(&self, name: &str, value: &str) -> Vec<Candidate> {
        // Bound matching and allocations for pasted certificates or oversized values.
        if name.len() > 256 || value.len() > 16_384 || value.is_empty() {
            return Vec::new();
        }
        let input = format!("{name}=\"{value}\"\n");
        let lower = input.to_ascii_lowercase();
        let mut matches = Vec::new();
        for entry in &self.rules {
            if !entry.rule.keywords.is_empty()
                && !entry
                    .rule
                    .keywords
                    .iter()
                    .any(|word| lower.contains(&word.to_ascii_lowercase()))
            {
                continue;
            }
            let Some(captures) = entry.pattern.captures(input.as_bytes()) else {
                continue;
            };
            let group = entry
                .rule
                .secret_group
                .unwrap_or(if captures.len() > 1 { 1 } else { 0 });
            let Some(secret) = captures.get(group) else {
                continue;
            };
            // Never let a name or a token embedded in another secret authorize
            // automatic transmission of the whole environment-variable value.
            if matches!(
                entry.rule.id.as_str(),
                "github-pat"
                    | "github-fine-grained-pat"
                    | "github-oauth"
                    | "openai-api-key"
                    | "anthropic-api-key"
            ) && (secret.as_bytes() != value.as_bytes() || secret.start() != name.len() + 2)
            {
                continue;
            }
            if entropy(std::str::from_utf8(secret.as_bytes()).unwrap_or("")) < entry.rule.entropy {
                continue;
            }
            matches.push(Candidate {
                id: entry.rule.id.clone(),
                description: entry.rule.description.clone(),
                evidence: DetectionEvidence::ValuePattern,
            });
        }
        // Explicit naming hints also work for legacy tokens without a recognizable prefix.
        let name = name.to_ascii_uppercase();
        for (id, aliases) in [
            (
                "github",
                &["GITHUB_TOKEN", "GH_TOKEN", "GITHUB_API_KEY", "GITHUB_PAT"][..],
            ),
            (
                "gitlab",
                &["GITLAB_TOKEN", "GITLAB_API_TOKEN", "GITLAB_PAT"][..],
            ),
            ("openai", &["OPENAI_API_KEY", "OPENAI_KEY"][..]),
            ("anthropic", &["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"][..]),
            ("temps", &["TEMPS_API_KEY", "TEMPS_API_TOKEN"][..]),
        ] {
            if aliases
                .iter()
                .any(|alias| name == *alias || name.ends_with(&format!("_{alias}")))
            {
                matches.push(Candidate {
                    id: id.into(),
                    description: format!("{id} credential suggested by variable name"),
                    evidence: DetectionEvidence::VariableName,
                });
            }
        }
        matches
    }
}
fn entropy(input: &str) -> f64 {
    if input.is_empty() {
        return 0.0;
    }
    let mut frequencies = [0usize; 256];
    for byte in input.bytes() {
        frequencies[byte as usize] += 1;
    }
    frequencies
        .iter()
        .filter(|n| **n > 0)
        .map(|n| {
            let p = *n as f64 / input.len() as f64;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_candidates_must_match_the_entire_value_not_the_name() {
        let detector = CatalogDetector::bundled().unwrap();
        let token = format!("ghp_{}", "abcdefghijklmnopqrstuvwxyz0123456789");
        for (name, value) in [
            (token.as_str(), "unrelated-secret".to_string()),
            ("GITHUB_TOKEN", format!("other-secret;{token}")),
            ("GITHUB_TOKEN", format!("{token};other-secret")),
        ] {
            assert!(crate::automatic_preset(&detector.detect(name, &value), &value).is_none());
        }
        assert!(
            crate::automatic_preset(&detector.detect("GITHUB_TOKEN", &token), &token).is_some()
        );
    }
    #[test]
    fn compiles_entire_catalog_and_finds_named_credentials() {
        let detector = CatalogDetector::bundled().unwrap_or_else(|e| panic!("{e}"));
        assert!(detector.rule_count() >= 200);
        let candidates = detector.detect("PROD_OPENAI_API_KEY", "synthetic-placeholder");
        assert!(candidates.iter().any(|c| c.id == "openai"));
        assert!(!serde_json::to_string(&candidates)
            .unwrap()
            .contains("synthetic-placeholder"));
    }
    #[test]
    fn unrelated_names_are_not_guessed_as_providers() {
        let detector = CatalogDetector::bundled().unwrap_or_else(|e| panic!("{e}"));
        assert!(detector.detect("LOG_LEVEL", "debug").is_empty());
        assert!(detector.detect("OPENAI_API_KEY", "").is_empty());
        assert!(detector
            .detect("OPENAI_API_KEY", &"x".repeat(16_385))
            .is_empty());
    }
    #[test]
    fn entropy_excludes_constant_placeholders() {
        assert_eq!(entropy("aaaaaaaa"), 0.0);
        assert!(entropy("abcd1234") > 2.0);
    }
}
