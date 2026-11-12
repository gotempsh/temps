// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::ServiceAccessService;
use crate::{
    error::AccessError,
    types::{
        AccessServiceProvider, CatalogReference, CredentialDelivery, CredentialFormat,
        CredentialHelp, RequestHeader,
    },
};
use serde::{Deserialize, Serialize};
use tokenless_core::{
    injection::InjectionSpec,
    template::{builtin_templates, ServiceKind, TemplateCategory},
};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ServiceCatalogQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    pub search: Option<String>,
    pub id: Option<String>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceCatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub logo: String,
    pub popular: bool,
    pub origin: Option<String>,
    pub supported: bool,
    pub reason: Option<String>,
    /// Existing presentation identity, when known. New catalog selections use
    /// the ordinary custom-HTTP policy shape for older-reader compatibility.
    pub provider: Option<AccessServiceProvider>,
    /// How this service takes its credential and which fixed headers it needs.
    /// Present when it differs from a plain bearer token; the console sends it
    /// back as the policy's `delivery`.
    pub delivery: Option<CredentialDelivery>,
    /// One label for a single credential, or two (username, password) when the
    /// service signs in with a pair that is stored as `username:password`.
    pub credential_labels: Vec<String>,
    /// Where to create the credential: a page when Temps knows one, and the
    /// template's own notes about each field. Both may be absent.
    pub credential_help: CredentialHelp,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceCatalog {
    pub items: Vec<ServiceCatalogEntry>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
impl ServiceAccessService {
    pub fn service_catalog(
        &self,
        query: ServiceCatalogQuery,
    ) -> Result<ServiceCatalog, AccessError> {
        catalog(query)
    }
}
fn catalog(query: ServiceCatalogQuery) -> Result<ServiceCatalog, AccessError> {
    let templates = builtin_templates().map_err(|source| AccessError::Catalog {
        revision: crate::tokenless::SDK_REVISION,
        source,
    })?;
    let search = query.search.unwrap_or_default().trim().to_lowercase();
    let mut items = Vec::new();
    for (template, _) in templates {
        if query.id.as_ref().is_some_and(|id| id != &template.id) {
            continue;
        }
        if ["vercel", "netlify", "resend"]
            .iter()
            .any(|id| template.id == *id || template.id.starts_with(&format!("{id}-")))
        {
            continue;
        }
        let category = match template.category {
            TemplateCategory::LlmProviders => "AI models",
            TemplateCategory::DeveloperTools => "Developer tools",
            TemplateCategory::Infrastructure => "Infrastructure",
            TemplateCategory::Communication => "Communication",
            TemplateCategory::Monitoring => "Monitoring",
            TemplateCategory::Productivity => "Productivity",
            TemplateCategory::Payments => "Payments",
            TemplateCategory::Commerce => "Commerce",
            TemplateCategory::Search => "Search",
            TemplateCategory::Custom => "Custom",
        };
        if !search.is_empty()
            && ![
                template.id.as_str(),
                template.name.as_str(),
                template.description.as_str(),
                category,
            ]
            .iter()
            .any(|value| value.to_lowercase().contains(&search))
        {
            continue;
        }
        let reason = if template.kind == ServiceKind::Mcp {
            Some("MCP tool-scoped access is not supported by Temps vault sessions yet.")
        } else {
            None
        };
        let description = description_for(&template.id, template.description.clone());
        let (delivery, credential_labels) = delivery_for(&template);
        let credential_help = help_for(&template);
        let origin = template
            .hosts
            .iter()
            .map(ToString::to_string)
            .find(|host| !host.contains('*'))
            .map(|host| format!("https://{host}"));
        let provider = match template.id.as_str() {
            "github" => Some(AccessServiceProvider::Github),
            "gitlab" => Some(AccessServiceProvider::Gitlab),
            "openai" => Some(AccessServiceProvider::Openai),
            "anthropic" => Some(AccessServiceProvider::Anthropic),
            "stripe" => Some(AccessServiceProvider::Stripe),
            "sendgrid" => Some(AccessServiceProvider::Sendgrid),
            "mistral" => Some(AccessServiceProvider::Mistral),
            "groq" => Some(AccessServiceProvider::Groq),
            _ => None,
        };
        items.push(ServiceCatalogEntry {
            logo: template.logo.unwrap_or_else(|| template.id.clone()),
            popular: template.popular,
            id: template.id,
            name: template.name,
            description,
            category: category.into(),
            origin,
            supported: reason.is_none(),
            reason: reason.map(str::to_owned),
            provider,
            delivery,
            credential_labels,
            credential_help,
        });
    }
    items.sort_by(|a, b| {
        b.popular
            .cmp(&a.popular)
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    let total = items.len() as u64;
    let page = query.page.unwrap_or(1).clamp(1, 1_000_000);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    Ok(ServiceCatalog {
        items: items
            .into_iter()
            .skip(((page - 1) * page_size) as usize)
            .take(page_size as usize)
            .collect(),
        total,
        page,
        page_size,
    })
}
/// A template's description, except where it advises a setup Temps refuses.
/// Datadog's says to add `DD-APPLICATION-KEY` as a custom header: a second
/// secret in a fixed header would be stored and shown in plain text, so
/// [`crate::tokenless::delivery::validate_delivery`] rejects it, and the
/// catalog says what the service does reach instead.
fn description_for(id: &str, description: String) -> String {
    match id {
        "datadog" => "Metrics, logs and monitors. Temps sends the API key only, so endpoints \
                      that also need an application key are not available."
            .into(),
        _ => description,
    }
}

/// The delivery a template needs, or `None` for a plain bearer token, with the
/// labels for its credential inputs. Header values that are `{{VARIABLE}}`
/// placeholders ship empty: the operator supplies them for their own account.
fn delivery_for(
    template: &tokenless_core::template::ServiceTemplate,
) -> (Option<CredentialDelivery>, Vec<String>) {
    let label = |name: &str| {
        template
            .credential_fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.label.clone())
    };
    let (format, labels) = match &template.injection {
        InjectionSpec::Bearer { field } => (CredentialFormat::Bearer, vec![label(field)]),
        InjectionSpec::Header {
            name,
            field,
            prefix,
        } => (
            CredentialFormat::Header {
                name: name.to_ascii_lowercase(),
                prefix: prefix.clone(),
            },
            vec![label(field)],
        ),
        InjectionSpec::BasicToken {
            username,
            password_field,
        } => (
            CredentialFormat::BasicToken {
                username: username.clone(),
            },
            vec![label(password_field)],
        ),
        InjectionSpec::Basic {
            username_field,
            password_field,
        } => (
            CredentialFormat::Basic,
            vec![label(username_field), label(password_field)],
        ),
    };
    let headers: Vec<RequestHeader> = template
        .default_headers
        .iter()
        .map(|header| RequestHeader {
            name: header.name.to_ascii_lowercase(),
            value: if header.value.contains("{{") {
                String::new()
            } else {
                header.value.clone()
            },
        })
        .collect();
    let labels = labels
        .into_iter()
        .map(|label| label.unwrap_or_else(|| "Credential".into()))
        .collect();
    let delivery = (format != CredentialFormat::Bearer || !headers.is_empty())
        .then_some(CredentialDelivery { format, headers });
    (delivery, labels)
}

/// Where the credential for `template` is created, plus the template's own
/// notes about its credential fields.
fn help_for(template: &tokenless_core::template::ServiceTemplate) -> CredentialHelp {
    CredentialHelp {
        url: super::credential_links::credential_url(&template.id).map(str::to_owned),
        notes: template
            .credential_fields
            .iter()
            .filter(|field| !field.description.is_empty())
            .map(|field| format!("{}: {}", field.label, field.description))
            .collect(),
    }
}

/// The catalog entry whose own host is `origin`, so a service saved from the
/// catalog keeps its logo, credential labels and credential help without storing
/// anything beyond the ordinary custom-HTTP policy. Exact hosts win over
/// wildcards; ties go to the popular, then shortest, template id so the answer
/// is stable across releases.
pub(crate) fn reference_for_origin(origin: &str) -> Option<CatalogReference> {
    static INDEX: std::sync::OnceLock<Vec<(String, CatalogReference)>> = std::sync::OnceLock::new();
    let index = INDEX.get_or_init(|| {
        let Ok(templates) = builtin_templates() else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        for (template, _) in templates {
            if template.kind == ServiceKind::Mcp {
                continue;
            }
            let reference = CatalogReference {
                id: template.id.clone(),
                name: template.name.clone(),
                logo: template.logo.clone().unwrap_or_else(|| template.id.clone()),
                credential_labels: delivery_for(&template).1,
                credential_help: help_for(&template),
            };
            for host in &template.hosts {
                entries.push((host.to_string(), reference.clone(), !template.popular));
            }
        }
        entries.sort_by(|a, b| (&a.2, a.1.id.len(), &a.1.id).cmp(&(&b.2, b.1.id.len(), &b.1.id)));
        entries
            .into_iter()
            .map(|(host, reference, _)| (host, reference))
            .collect()
    });
    let host = origin.strip_prefix("https://")?.to_ascii_lowercase();
    let exact = index.iter().find(|(pattern, _)| *pattern == host);
    let wildcard = || {
        index.iter().find(|(pattern, _)| {
            pattern
                .strip_prefix("*.")
                .is_some_and(|suffix| host.ends_with(&format!(".{suffix}")))
        })
    };
    exact
        .or_else(wildcard)
        .map(|(_, reference)| reference.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn popular_entries_precede_alphabetical_catalog_and_exact_selection_is_stable() {
        let all = catalog(ServiceCatalogQuery {
            page_size: Some(100),
            ..Default::default()
        })
        .unwrap();
        assert!(all.items[0].popular);
        assert!(all
            .items
            .windows(2)
            .all(|pair| pair[0].popular || !pair[1].popular));
        for entry in &all.items {
            let selected = catalog(ServiceCatalogQuery {
                id: Some(entry.id.clone()),
                ..Default::default()
            })
            .unwrap();
            assert_eq!(selected.total, 1);
            assert_eq!(selected.items[0].id, entry.id);
            assert!(!selected.items[0].logo.is_empty());
        }
        assert_eq!(
            catalog(ServiceCatalogQuery {
                id: Some("vercel".into()),
                ..Default::default()
            })
            .unwrap()
            .total,
            0
        );
    }
    #[test]
    fn catalog_is_sdk_backed_paginated_and_explains_unsupported_entries() {
        let all = catalog(ServiceCatalogQuery {
            page_size: Some(100),
            ..Default::default()
        })
        .unwrap();
        assert!(all.total > 30);
        assert!(all
            .items
            .iter()
            .any(|item| item.id == "deepseek" && item.supported));
        let mcp = catalog(ServiceCatalogQuery {
            id: Some("supabase-mcp".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(!mcp.items[0].supported && mcp.items[0].reason.is_some());
        assert!(!all
            .items
            .iter()
            .any(|item| ["vercel", "netlify", "resend"].contains(&item.id.as_str())));
        assert_eq!(catalog(Default::default()).unwrap().items.len(), 20);
        assert!(catalog(ServiceCatalogQuery {
            search: Some("missing-catalog-fixture".into()),
            ..Default::default()
        })
        .unwrap()
        .items
        .is_empty());
        let last = catalog(ServiceCatalogQuery {
            page: Some(1_000_000),
            ..Default::default()
        })
        .unwrap();
        assert!(last.items.is_empty());
    }
    #[test]
    fn origin_resolves_to_its_catalog_reference_without_inventing_one() {
        let slack = reference_for_origin("https://slack.com").expect("slack is in the catalog");
        assert!(!slack.logo.is_empty() && !slack.credential_labels.is_empty());
        assert_eq!(
            slack.credential_help.url.as_deref(),
            Some("https://api.slack.com/apps")
        );
        assert_eq!(
            reference_for_origin("https://SLACK.com").map(|r| r.id),
            Some(slack.id)
        );
        assert!(reference_for_origin("https://internal.example.invalid").is_none());
        assert!(reference_for_origin("http://slack.com").is_none());
    }
    /// No entry tells the operator to put a credential in a request header:
    /// fixed headers are plain text that linked sandbox owners can read, so
    /// validation refuses one, and the catalog must not advise it.
    #[test]
    fn no_entry_advises_a_second_credential_in_a_request_header() {
        let mut checked = 0;
        for page in 1..=3 {
            let all = catalog(ServiceCatalogQuery {
                page: Some(page),
                page_size: Some(100),
                ..Default::default()
            })
            .unwrap();
            for entry in all.items {
                let description = entry.description.to_lowercase();
                assert!(
                    !description.contains("custom header"),
                    "{}: {}",
                    entry.id,
                    entry.description
                );
                checked += 1;
            }
        }
        assert!(checked > 60, "only {checked} entries were checked");
        let datadog = catalog(ServiceCatalogQuery {
            id: Some("datadog".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(datadog.items[0].supported);
        assert!(datadog.items[0].description.contains("API key only"));
    }

    #[test]
    fn every_listed_http_service_is_supported_with_a_deliverable_credential_format() {
        let mut checked = 0;
        for page in 1..=3 {
            let all = catalog(ServiceCatalogQuery {
                page: Some(page),
                page_size: Some(100),
                ..Default::default()
            })
            .unwrap();
            for entry in all.items {
                if entry.id.ends_with("-mcp") || entry.id == "generic-mcp" {
                    assert!(!entry.supported, "{} needs tool-scoped access", entry.id);
                    continue;
                }
                assert!(entry.supported, "{} must be supported", entry.id);
                assert!(!entry.credential_labels.is_empty(), "{}", entry.id);
                let mut delivery = entry.delivery.clone();
                if let Some(value) = &mut delivery {
                    // Account-specific header values ship empty for the operator to fill.
                    for header in &mut value.headers {
                        if header.value.is_empty() {
                            header.value = "account-value".into();
                        }
                    }
                }
                crate::tokenless::delivery::validate_delivery(&mut delivery)
                    .unwrap_or_else(|error| panic!("{}: {error}", entry.id));
                let secret = if crate::tokenless::delivery::credential_is_login(delivery.as_ref()) {
                    assert_eq!(entry.credential_labels.len(), 2, "{}", entry.id);
                    "user:secret"
                } else {
                    assert_eq!(entry.credential_labels.len(), 1, "{}", entry.id);
                    "secret"
                };
                crate::tokenless::delivery::render(delivery.as_ref(), secret)
                    .unwrap_or_else(|error| panic!("{}: {error}", entry.id));
                checked += 1;
            }
        }
        assert!(checked > 60, "only {checked} HTTP services were checked");
    }
}
