// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::ServiceAccessService;
use crate::{error::AccessError, types::AccessServiceProvider};
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
}
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceCatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub origin: Option<String>,
    pub supported: bool,
    pub reason: Option<String>,
    /// Existing presentation identity, when known. New catalog selections use
    /// the ordinary custom-HTTP policy shape for older-reader compatibility.
    pub provider: Option<AccessServiceProvider>,
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
        } else if !matches!(template.injection, InjectionSpec::Bearer { .. }) {
            Some("This service needs a credential format other than a bearer token; Temps vault sessions do not support that format yet.")
        } else if !template.default_headers.is_empty() {
            Some("This service requires additional request headers; Temps vault sessions do not support those headers yet.")
        } else {
            None
        };
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
            id: template.id,
            name: template.name,
            description: template.description,
            category: category.into(),
            origin,
            supported: reason.is_none(),
            reason: reason.map(str::to_owned),
            provider,
        });
    }
    items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
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
#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(all
            .items
            .iter()
            .any(|item| item.id == "anthropic" && !item.supported && item.reason.is_some()));
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
}
