// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where an operator creates the credential for a catalog service. The Tokenless
//! templates carry no such links, so Temps keeps this table itself. It lists only
//! pages whose purpose is known; a service that is not listed gets no link rather
//! than a guessed one.

const LINKS: &[(&str, &str)] = &[
    ("notion", "https://www.notion.so/my-integrations"),
    ("anthropic", "https://console.anthropic.com/settings/keys"),
    ("openai", "https://platform.openai.com/api-keys"),
    ("slack", "https://api.slack.com/apps"),
    ("stripe", "https://dashboard.stripe.com/apikeys"),
    (
        "github",
        "https://github.com/settings/personal-access-tokens",
    ),
    (
        "gitlab",
        "https://gitlab.com/-/user_settings/personal_access_tokens",
    ),
    ("linear", "https://linear.app/settings/account/security"),
    ("sendgrid", "https://app.sendgrid.com/settings/api_keys"),
    ("mistral", "https://console.mistral.ai/api-keys"),
    ("groq", "https://console.groq.com/keys"),
    ("gemini", "https://aistudio.google.com/apikey"),
    ("discord", "https://discord.com/developers/applications"),
    (
        "datadog",
        "https://docs.datadoghq.com/account_management/api-app-keys/",
    ),
    (
        "pagerduty",
        "https://support.pagerduty.com/main/docs/api-access-keys",
    ),
    ("twilio", "https://console.twilio.com/"),
    (
        "jira",
        "https://id.atlassian.com/manage-profile/security/api-tokens",
    ),
    (
        "heroku",
        "https://dashboard.heroku.com/account/applications",
    ),
    (
        "figma",
        "https://www.figma.com/developers/api#access-tokens",
    ),
    ("postmark", "https://account.postmarkapp.com/servers"),
    ("deepseek", "https://platform.deepseek.com/api_keys"),
    ("supabase", "https://supabase.com/dashboard/account/tokens"),
    ("mailgun", "https://app.mailgun.com/settings/api_security"),
    (
        "cloudflare",
        "https://dash.cloudflare.com/profile/api-tokens",
    ),
    ("openrouter", "https://openrouter.ai/keys"),
    ("perplexity", "https://www.perplexity.ai/settings/api"),
    ("xai", "https://console.x.ai"),
    ("cohere", "https://dashboard.cohere.com/api-keys"),
    ("huggingface", "https://huggingface.co/settings/tokens"),
    (
        "sentry",
        "https://sentry.io/settings/account/api/auth-tokens/",
    ),
    (
        "digitalocean",
        "https://cloud.digitalocean.com/account/api/tokens",
    ),
    ("airtable", "https://airtable.com/create/tokens"),
    ("asana", "https://app.asana.com/0/my-apps"),
    (
        "hubspot",
        "https://developers.hubspot.com/docs/api/private-apps",
    ),
    ("posthog", "https://app.posthog.com/settings/user-api-keys"),
    ("mailchimp", "https://mailchimp.com/help/about-api-keys/"),
    ("npm", "https://www.npmjs.com/settings/~/tokens"),
    ("circleci", "https://app.circleci.com/settings/user/tokens"),
    ("replicate", "https://replicate.com/account/api-tokens"),
    ("elevenlabs", "https://elevenlabs.io/app/settings/api-keys"),
    (
        "brave-search",
        "https://api-dashboard.search.brave.com/app/keys",
    ),
    ("tavily", "https://app.tavily.com/home"),
    ("together", "https://api.together.ai/settings/api-keys"),
    (
        "confluence",
        "https://id.atlassian.com/manage-profile/security/api-tokens",
    ),
];

/// HTTPS page where the credential for the catalog service `id` is created.
pub(crate) fn credential_url(id: &str) -> Option<&'static str> {
    LINKS
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, url)| *url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_https_unique_and_name_real_catalog_entries() {
        let templates = tokenless_core::template::builtin_templates().unwrap();
        let ids: Vec<_> = templates
            .iter()
            .map(|(template, _)| template.id.as_str())
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for (id, url) in LINKS {
            assert!(ids.contains(id), "{id} is not a catalog service");
            assert!(seen.insert(*id), "{id} is listed twice");
            let parsed = url::Url::parse(url).unwrap();
            assert_eq!(parsed.scheme(), "https", "{id}");
            assert!(
                parsed.username().is_empty() && parsed.password().is_none(),
                "{id}"
            );
        }
        assert!(credential_url("notion").is_some());
        assert_eq!(credential_url("generic-bearer"), None);
    }
}
