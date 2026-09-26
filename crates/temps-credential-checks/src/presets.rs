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
    /// Whether a value-pattern match can safely identify the public issuer.
    pub automatic: bool,
    pub documentation_url: String,
}
/// Reviewed read-only verification endpoints. No inference, email, or payment requests.
/// Sources document authentication, response codes, scope, and regional limitations.
struct Definition {
    id: &'static str,
    name: &'static str,
    url: &'static str,
    header: &'static str,
    prefix: &'static str,
    rules: &'static [&'static str],
    documentation: &'static str,
    description: &'static str,
}
const PROVIDERS: &[Definition] = &[
    Definition {
        id: "github",
        name: "GitHub personal token",
        url: "https://api.github.com/user",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["github-pat","github-fine-grained-pat","github-oauth"],
        documentation: "https://docs.github.com/en/rest/users/users#get-the-authenticated-user",
        description: "Checks authenticated user access. Fine-grained tokens may require user permissions.",
    },
    Definition {
        id: "gitlab",
        name: "GitLab",
        url: "https://gitlab.com/api/v4/personal_access_tokens/self",
        header: "PRIVATE-TOKEN",
        prefix: "",
        rules: &[],
        documentation: "https://docs.gitlab.com/api/personal_access_tokens/",
        description: "Checks personal-token access and expiration. Confirm the GitLab host before configuring.",
    },
    Definition {
        id: "openai",
        name: "OpenAI",
        url: "https://api.openai.com/v1/models",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["openai-api-key"],
        documentation: "https://platform.openai.com/docs/api-reference/models/list",
        description: "Checks model-list access; does not establish credits or inference access.",
    },
    Definition {
        id: "anthropic",
        name: "Anthropic",
        url: "https://api.anthropic.com/v1/models",
        header: "x-api-key",
        prefix: "",
        rules: &["anthropic-api-key"],
        documentation: "https://platform.claude.com/docs/en/api/models/list",
        description: "Checks model-list access; does not establish credits or inference access.",
    },
    Definition {
        id: "digitalocean",
        name: "DigitalOcean",
        url: "https://api.digitalocean.com/v2/account",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["digitalocean-pat","digitalocean-access-token"],
        documentation: "https://docs.digitalocean.com/reference/api/reference/account/",
        description: "Checks account access. Requires account:read; does not check balance.",
    },
    Definition {
        id: "doppler",
        name: "Doppler",
        url: "https://api.doppler.com/v3/me",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["doppler-api-token"],
        documentation: "https://docs.doppler.com/reference/auth-me",
        description: "Checks personal-token identity without reading stored secrets.",
    },
    Definition {
        id: "huggingface",
        name: "Hugging Face",
        url: "https://huggingface.co/api/whoami-v2",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["huggingface-access-token"],
        documentation: "https://huggingface.co/docs/hub/api",
        description: "Checks user-token identity; does not establish inference credits.",
    },
    Definition {
        id: "npm",
        name: "npm",
        url: "https://registry.npmjs.org/-/whoami",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["npm-access-token"],
        documentation: "https://docs.npmjs.com/cli/commands/npm-whoami/",
        description: "Checks identity at the public npm registry, not package publishing permissions.",
    },
    Definition {
        id: "airtable",
        name: "Airtable",
        url: "https://api.airtable.com/v0/meta/whoami",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["airtable-personnal-access-token"],
        documentation: "https://airtable.com/developers/web/api/get-user-id-scopes",
        description: "Checks personal-token identity and access to token metadata.",
    },
    Definition {
        id: "stripe",
        name: "Stripe",
        url: "https://api.stripe.com/v1/account",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["stripe-access-token"],
        documentation: "https://docs.stripe.com/api/accounts/retrieve",
        description: "Checks account access for secret and restricted keys. Restricted keys may lack permission.",
    },
    Definition {
        id: "typeform",
        name: "Typeform",
        url: "https://api.typeform.com/me",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["typeform-api-token"],
        documentation: "https://www.typeform.com/developers/create/reference/retrieve-your-own-user/",
        description: "Checks user-profile access. Requires the accounts:read scope.",
    },
    Definition {
        id: "readme",
        name: "ReadMe",
        url: "https://api.readme.com/v2/projects/me",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["readme-api-token"],
        documentation: "https://docs.readme.com/main/reference/api-upgrade-guide",
        description: "Checks project access with the v2 API; older API keys may require migration.",
    },
    Definition {
        id: "groq",
        name: "Groq",
        url: "https://api.groq.com/openai/v1/models",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["groq-api-key"],
        documentation: "https://console.groq.com/docs/api-reference",
        description: "Checks model-list access; does not establish credits or inference access.",
    },
    Definition {
        id: "replicate",
        name: "Replicate",
        url: "https://api.replicate.com/v1/account",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &["replicate-api-token"],
        documentation: "https://replicate.com/docs/reference/http#get-account",
        description: "Checks account identity without creating a prediction or spending credits.",
    },
    Definition {
        id: "resend",
        name: "Resend",
        url: "https://api.resend.com/domains?limit=1",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &[],
        documentation: "https://resend.com/docs/api-reference/domains/list-domains",
        description: "Checks domain-list access. Sending-only keys lack permission; configure explicitly.",
    },
    Definition {
        id: "postman",
        name: "Postman",
        url: "https://api.postman.com/me",
        header: "X-Api-Key",
        prefix: "",
        rules: &[],
        documentation: "https://learning.postman.com/api-docs/api-reference/users/get-authenticated-user",
        description: "Checks user identity. Confirm US or EU API region before configuring.",
    },
    Definition {
        id: "sendgrid",
        name: "SendGrid",
        url: "https://api.sendgrid.com/v3/scopes",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &[],
        documentation: "https://www.twilio.com/docs/sendgrid/api-reference/api-key-permissions/retrieve-a-list-of-scopes-for-which-this-user-has-access",
        description: "Checks granted API scopes. Confirm global or EU API region before configuring.",
    },
    Definition {
        id: "vercel",
        name: "Vercel",
        url: "https://api.vercel.com/v2/user",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &[],
        documentation: "https://vercel.com/docs/rest-api/reference/endpoints/user/get-the-user",
        description: "Checks user-profile access. Unrecognized token formats require explicit configuration.",
    },
    Definition {
        id: "netlify",
        name: "Netlify",
        url: "https://api.netlify.com/api/v1/user",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &[],
        documentation: "https://open-api.netlify.com/",
        description: "Checks user-profile access. Ambiguous token formats require explicit configuration.",
    },
    Definition {
        id: "mistral",
        name: "Mistral AI",
        url: "https://api.mistral.ai/v1/models",
        header: "Authorization",
        prefix: "Bearer ",
        rules: &[],
        documentation: "https://docs.mistral.ai/api/endpoint/models",
        description: "Checks model-list access. Ambiguous token formats require explicit configuration; no credit check.",
    },
 ];
/// Single policy registry shared by detection and automatic destination selection.
pub(crate) fn automatic_provider_for_rule(rule: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find(|provider| provider.rules.contains(&rule))
        .map(|provider| provider.id)
}
pub fn provider_presets() -> Vec<ProviderPreset> {
    PROVIDERS
        .iter()
        .map(|provider| {
            let mut spec = HttpCheckSpec {
                url: provider.url.into(),
                method: HttpCheckMethod::Get,
                headers: BTreeMap::new(),
                credential_header: Some(provider.header.into()),
                credential_prefix: provider.prefix.into(),
                accepted_statuses: vec![200],
                expiration: None,
                numeric_rules: vec![],
            };
            if provider.id == "anthropic" {
                spec.headers
                    .insert("anthropic-version".into(), "2023-06-01".into());
            }
            if provider.id == "gitlab" {
                spec.expiration = Some(ExpirationRule {
                    field: ResponseField::JsonPointer("/expires_at".into()),
                    warning_days: vec![30, 7, 1],
                });
            }
            ProviderPreset {
                id: provider.id.into(),
                name: provider.name.into(),
                description: provider.description.into(),
                spec,
                automatic: !provider.rules.is_empty(),
                documentation_url: provider.documentation.into(),
            }
        })
        .collect()
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
        let Some(provider) = automatic_provider_for_rule(&candidate.id) else {
            continue;
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
                id: "github-pat".into(),
                description: String::new(),
                evidence: crate::detection::DetectionEvidence::VariableName
            }],
            "unrelated-secret"
        )
        .is_none());
        assert_eq!(
            automatic_preset(&[candidate("github-pat")], "synthetic")
                .unwrap()
                .id,
            "github"
        );
        assert!(automatic_preset(
            &[candidate("github-pat"), candidate("openai-api-key")],
            "synthetic"
        )
        .is_none());
        assert!(automatic_preset(&[candidate("gitlab-pat")], "glpat-synthetic").is_none());
        assert!(automatic_preset(&[candidate("github-pat")], "ghs_synthetic").is_none());
        assert!(automatic_preset(&[], "synthetic").is_none());
    }
    #[test]
    fn catalog_metadata_and_automatic_policy_stay_in_sync() {
        let presets = provider_presets();
        let ids: std::collections::BTreeSet<_> = presets.iter().map(|p| &p.id).collect();
        assert_eq!(ids.len(), presets.len());
        assert_eq!(presets.len(), 20);
        assert_eq!(presets.iter().filter(|p| p.automatic).count(), 13);
        for provider in PROVIDERS {
            let preset = presets.iter().find(|p| p.id == provider.id).unwrap();
            assert!(preset.documentation_url.starts_with("https://"));
            assert_eq!(preset.automatic, !provider.rules.is_empty());
            for rule in provider.rules {
                assert_eq!(automatic_provider_for_rule(rule), Some(provider.id));
            }
        }
        for rule in [
            "gitlab-pat",
            "postman-api-token",
            "sendgrid-api-token",
            "netlify-access-token",
            "digitalocean-refresh-token",
            "huggingface-organization-api-token",
        ] {
            assert!(
                automatic_provider_for_rule(rule).is_none(),
                "{rule} must not guess a destination"
            );
        }
    }
    #[test]
    fn every_recipe_distinguishes_success_invalid_scope_and_outage() {
        for preset in provider_presets() {
            let verifier = crate::HttpCredentialVerifier::new(preset.spec.clone()).unwrap();
            for (code, expected) in [
                (200, crate::CheckStatus::Healthy),
                (401, crate::CheckStatus::Error),
                (403, crate::CheckStatus::Warning),
                (429, crate::CheckStatus::Unknown),
                (503, crate::CheckStatus::Unknown),
            ] {
                let response = crate::HttpCheckResponse {
                    status: code,
                    headers: BTreeMap::new(),
                    body: br#"{"expires_at":"2099-01-01"}"#.to_vec(),
                };
                assert_eq!(
                    verifier.evaluate(&response, chrono::Utc::now()).status,
                    expected,
                    "{} HTTP {code}",
                    preset.id
                );
            }
        }
    }
    #[test]
    fn all_presets_validate() {
        for preset in provider_presets() {
            crate::HttpCredentialVerifier::new(preset.spec).expect("preset is a valid HTTP recipe");
        }
    }
}
