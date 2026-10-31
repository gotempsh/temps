// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDN delivery provider selection for new projects.

use super::*;

/// The delivery provider a new project starts with, and whether the caller
/// asked for it or it was inherited from the instance-wide default.
///
/// The distinction decides what happens when the provider is not usable: an
/// explicit request fails loudly, an inherited default degrades to "no
/// delivery" so that importers, starter projects and API callers that never
/// mention delivery keep working when an admin's default points at a provider
/// that has since been disconnected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeliveryChoice {
    pub(super) provider: Option<&'static str>,
    pub(super) explicit: bool,
}

impl DeliveryChoice {
    fn explicit(provider: Option<&'static str>) -> Self {
        Self {
            provider,
            explicit: true,
        }
    }

    fn inherited(provider: Option<&'static str>) -> Self {
        Self {
            provider,
            explicit: false,
        }
    }
}

pub(super) fn chosen_delivery_provider(
    explicit: Option<&str>,
    legacy_cloudflare: Option<bool>,
    cloudflare_default: bool,
    bunny_default: bool,
) -> Result<DeliveryChoice, ProjectError> {
    match explicit {
        Some("none") => Ok(DeliveryChoice::explicit(None)),
        Some("cloudflare") => Ok(DeliveryChoice::explicit(Some("cloudflare"))),
        Some("bunny") => Ok(DeliveryChoice::explicit(Some("bunny"))),
        Some(value) => Err(ProjectError::InvalidInput(format!(
            "Unknown delivery provider '{value}'; choose none, cloudflare, or bunny"
        ))),
        // The legacy flag only ever spoke about Cloudflare: `true` asks for
        // it, `false` opts out of it. Opting out of Cloudflare says nothing
        // about Bunny, so a Bunny instance default still applies.
        None => match legacy_cloudflare {
            Some(true) => Ok(DeliveryChoice::explicit(Some("cloudflare"))),
            Some(false) if bunny_default => Ok(DeliveryChoice::inherited(Some("bunny"))),
            Some(false) => Ok(DeliveryChoice::explicit(None)),
            None if cloudflare_default => Ok(DeliveryChoice::inherited(Some("cloudflare"))),
            None if bunny_default => Ok(DeliveryChoice::inherited(Some("bunny"))),
            None => Ok(DeliveryChoice::inherited(None)),
        },
    }
}

pub(super) fn delivery_provider_label(provider: &str) -> &'static str {
    match provider {
        "cloudflare" => "Cloudflare",
        "bunny" => "Bunny",
        _ => "CDN",
    }
}

impl ProjectService {
    /// The delivery profile a new project using `provider` would be attached
    /// to, or a sentence naming what is missing and where to configure it.
    ///
    /// The outer `Result` is a database failure; the inner one is "not set
    /// up", which the caller turns into either a 400 or a degraded default.
    pub(super) async fn usable_delivery_profile(
        &self,
        provider: &'static str,
    ) -> Result<Result<i32, String>, ProjectError> {
        if provider == "cloudflare" {
            let dns_provider = dns_providers::Entity::find()
                .filter(dns_providers::Column::ProviderType.eq("cloudflare"))
                .filter(dns_providers::Column::IsActive.eq(true))
                .one(self.db.as_ref())
                .await?;
            if dns_provider.is_none() {
                return Ok(Err(
                    "Connect an active Cloudflare DNS provider in Settings > DNS Providers".into(),
                ));
            }
        }
        let profile = delivery_profiles::Entity::find()
            .filter(delivery_profiles::Column::ProviderKind.eq(provider))
            .order_by_asc(delivery_profiles::Column::Id)
            .one(self.db.as_ref())
            .await?;
        Ok(profile.map(|profile| profile.id).ok_or_else(|| {
            format!(
                "Create a {} delivery profile in Delivery Profiles",
                delivery_provider_label(provider)
            )
        }))
    }

    pub async fn cloudflare_project_capability(
        &self,
    ) -> Result<crate::handlers::CloudflareProjectCapability, ProjectError> {
        let defaults = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?
            .map(|row| temps_core::AppSettings::from_json(row.data))
            .unwrap_or_default();
        let provider = dns_providers::Entity::find()
            .filter(dns_providers::Column::ProviderType.eq("cloudflare"))
            .filter(dns_providers::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?;
        let profile = delivery_profiles::Entity::find()
            .filter(delivery_profiles::Column::ProviderKind.eq("cloudflare"))
            .one(self.db.as_ref())
            .await?;
        let bunny_profile = delivery_profiles::Entity::find()
            .filter(delivery_profiles::Column::ProviderKind.eq("bunny"))
            .one(self.db.as_ref())
            .await?;
        let (reason, setup_path) = if provider.is_none() {
            (
                Some("Connect an active Cloudflare DNS provider".into()),
                Some("/dns-providers".into()),
            )
        } else if profile.is_none() {
            (
                Some("Create a Cloudflare delivery profile".into()),
                Some("/delivery-profiles".into()),
            )
        } else {
            (None, None)
        };
        Ok(crate::handlers::CloudflareProjectCapability {
            configured: provider.is_some() && profile.is_some(),
            default_enabled: defaults.cloudflare_new_projects,
            reason,
            setup_path,
            bunny_configured: bunny_profile.is_some(),
            bunny_default_enabled: defaults.bunny_new_projects,
            bunny_reason: bunny_profile
                .is_none()
                .then(|| "Create a Bunny delivery profile with an active Pull Zone".into()),
        })
    }
}

#[cfg(test)]
mod cloudflare_creation_tests {
    use super::{chosen_delivery_provider, DeliveryChoice};

    fn choose(
        explicit: Option<&str>,
        legacy: Option<bool>,
        cloudflare_default: bool,
        bunny_default: bool,
    ) -> DeliveryChoice {
        chosen_delivery_provider(explicit, legacy, cloudflare_default, bunny_default)
            .expect("valid choice")
    }

    #[test]
    fn explicit_choice_overrides_future_project_default() {
        assert_eq!(
            choose(Some("cloudflare"), None, false, true),
            DeliveryChoice::explicit(Some("cloudflare"))
        );
        assert_eq!(
            choose(Some("none"), None, true, false),
            DeliveryChoice::explicit(None)
        );
        assert_eq!(
            choose(None, Some(true), false, false),
            DeliveryChoice::explicit(Some("cloudflare"))
        );
        assert_eq!(
            choose(None, Some(false), true, false),
            DeliveryChoice::explicit(None)
        );
        assert!(chosen_delivery_provider(Some("invalid"), None, false, false).is_err());
    }

    #[test]
    fn absent_choice_inherits_future_project_default() {
        assert_eq!(
            choose(None, None, true, false),
            DeliveryChoice::inherited(Some("cloudflare"))
        );
        assert_eq!(
            choose(None, None, false, true),
            DeliveryChoice::inherited(Some("bunny"))
        );
        assert_eq!(
            choose(None, None, false, false),
            DeliveryChoice::inherited(None)
        );
    }

    /// The legacy flag only ever meant "Cloudflare on/off"; opting out of
    /// Cloudflare must not also switch off an instance-wide Bunny default.
    #[test]
    fn legacy_cloudflare_opt_out_keeps_bunny_default() {
        assert_eq!(
            choose(None, Some(false), false, true),
            DeliveryChoice::inherited(Some("bunny"))
        );
        assert_eq!(
            choose(None, Some(false), true, true),
            DeliveryChoice::inherited(Some("bunny"))
        );
    }
}
