// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{ExpirationRule, HttpCheckMethod, HttpCheckSpec, ResponseField};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub spec: HttpCheckSpec,
}
/// Reviewed verification destinations for explicit configuration or automatic policy.
pub fn provider_presets() -> Vec<ProviderPreset> {
    let mut presets = Vec::new();
    for (id, name, url, header, prefix) in [
        (
            "github",
            "GitHub personal token",
            "https://api.github.com/user",
            "Authorization",
            "Bearer ",
        ),
        (
            "gitlab",
            "GitLab",
            "https://gitlab.com/api/v4/personal_access_tokens/self",
            "PRIVATE-TOKEN",
            "",
        ),
        (
            "openai",
            "OpenAI",
            "https://api.openai.com/v1/models",
            "Authorization",
            "Bearer ",
        ),
        (
            "anthropic",
            "Anthropic",
            "https://api.anthropic.com/v1/models",
            "x-api-key",
            "",
        ),
    ] {
        let mut spec = HttpCheckSpec {
            url: url.into(),
            method: HttpCheckMethod::Get,
            headers: BTreeMap::new(),
            credential_header: Some(header.into()),
            credential_prefix: prefix.into(),
            accepted_statuses: vec![200],
            expiration: None,
            numeric_rules: vec![],
        };
        if id == "anthropic" {
            spec.headers
                .insert("anthropic-version".into(), "2023-06-01".into());
        }
        if id == "gitlab" {
            spec.expiration = Some(ExpirationRule {
                field: ResponseField::JsonPointer("/expires_at".into()),
                warning_days: vec![30, 7, 1],
            });
        }
        presets.push(ProviderPreset { id:id.into(), name:name.into(), description: if id=="gitlab" { "Checks personal-token access and expiration; requires access to the self-inspection endpoint.".into() } else { "Checks API access. Does not establish remaining credits or expiration.".into() }, spec });
    }
    presets
}
/// Select exactly one reviewed public issuer. Host-dependent tokens (GitLab and
/// Temps) need an explicit endpoint; generic names never authorize guessing a host.
pub fn automatic_preset(candidates: &[crate::Candidate], value: &str) -> Option<ProviderPreset> {
    if value.starts_with("ghs_") || value.starts_with("ghr_") || value.starts_with("sk-ant-admin") {
        return None;
    }
    let mut issuers = std::collections::BTreeSet::new();
    for candidate in candidates {
        if !matches!(
            candidate.evidence,
            crate::detection::DetectionEvidence::ValuePattern
        ) {
            continue;
        }
        let provider = match candidate.id.as_str() {
            "github" | "github-pat" | "github-fine-grained-pat" | "github-oauth" => "github",
            "openai" | "openai-api-key" => "openai",
            "anthropic" | "anthropic-api-key" => "anthropic",
            _ => continue,
        };
        issuers.insert(provider);
    }
    if issuers.len() != 1 {
        return None;
    }
    provider_presets()
        .into_iter()
        .find(|preset| issuers.contains(preset.id.as_str()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_policy_requires_unambiguous_supported_issuer() {
        let candidate = |id: &str| crate::Candidate {
            id: id.into(),
            description: String::new(),
            evidence: crate::detection::DetectionEvidence::ValuePattern,
        };
        assert!(automatic_preset(
            &[crate::Candidate {
                id: "github".into(),
                description: String::new(),
                evidence: crate::detection::DetectionEvidence::VariableName
            }],
            "unrelated-secret"
        )
        .is_none());
        assert_eq!(
            automatic_preset(&[candidate("github")], "synthetic")
                .unwrap()
                .id,
            "github"
        );
        assert!(
            automatic_preset(&[candidate("github"), candidate("openai")], "synthetic").is_none()
        );
        assert!(automatic_preset(&[candidate("gitlab-pat")], "glpat-synthetic").is_none());
        assert!(automatic_preset(&[candidate("github")], "ghs_synthetic").is_none());
        assert!(automatic_preset(&[], "synthetic").is_none());
    }
    #[test]
    fn all_presets_validate() {
        for preset in provider_presets() {
            crate::HttpCredentialVerifier::new(preset.spec).expect("preset is a valid HTTP recipe");
        }
    }
}
