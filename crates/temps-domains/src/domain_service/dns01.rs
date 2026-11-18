// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Issuing a certificate over DNS-01 through the DNS provider in Temps that
//! manages the domain's zone, in one call: order, publish the challenge
//! record, wait until public resolvers see it, validate, then remove the
//! record. The `setup-dns` and `order/finalize` endpoints do the same in
//! separate requests; *Make it reachable* needs it inside a run.
//!
//! Let's Encrypt validates an order once. Asking before the record is
//! visible spends the order, so validation is only requested after the
//! propagation check passes. Whatever happens after publishing, the records
//! this order published are removed again.

use std::collections::BTreeMap;

use async_trait::async_trait;
use tracing::{info, warn};

use super::{DomainLookup, DomainService, DomainServiceError};
use crate::dns_provider::DnsPropagationChecker;
use crate::handlers::domain_handler::{
    clean_up_dns01_challenge, publish_dns01_challenge, Dns01Cleanup,
};
use crate::tls::ChallengeType;

/// How long public resolvers get to see a published challenge record.
const PROPAGATION_WAIT_SECS: u32 = 180;
/// How often they are asked meanwhile.
const PROPAGATION_POLL_SECS: u32 = 10;
/// Share of the public resolvers that must see every value, as `temps
/// setup` requires.
const PROPAGATION_MIN_PERCENT: u8 = 50;

impl DomainService {
    /// Issue `domain_name`'s certificate over DNS-01 through the DNS provider
    /// in Temps that manages its zone (verified, automatic DNS on, active).
    /// The domain must already exist (`create_domain`).
    pub async fn issue_dns01_through_provider(
        &self,
        domain_name: &str,
        user_email: &str,
        dns_providers: &temps_dns::services::DnsProviderService,
    ) -> Result<(), DomainServiceError> {
        let (provider, zone) = dns_providers
            .find_provider_for_domain(domain_name)
            .await
            .map_err(|e| DomainServiceError::Dns01Provider {
                domain: domain_name.to_string(),
                provider: "for this zone".to_string(),
                detail: e.to_string(),
            })?
            .ok_or_else(|| DomainServiceError::Dns01NoProvider {
                domain: domain_name.to_string(),
            })?;
        let instance = dns_providers
            .create_provider_instance(&provider)
            .map_err(|e| DomainServiceError::Dns01Provider {
                domain: domain_name.to_string(),
                provider: provider.name.clone(),
                detail: e.to_string(),
            })?;
        info!(
            "Issuing a DNS-01 certificate for {} through DNS provider {} (zone {})",
            domain_name, provider.name, zone.domain
        );
        let steps = LiveSteps {
            service: self,
            domain: domain_name,
            email: user_email,
            provider_id: provider.id,
            provider_name: &provider.name,
            provider: instance.as_ref(),
            zone: &zone.domain,
        };
        issue(&steps).await
    }
}

/// What ordering the certificate returned.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Requested {
    /// A still-valid authorization: issued without a challenge.
    Issued,
    /// The `(name, value)` TXT records to publish for the order at
    /// `order_url`, the one this call created.
    Publish {
        order_url: String,
        records: Vec<(String, String)>,
    },
}

/// One DNS-01 issuance's steps, so their order and the cleanup are tested
/// without a CA or a DNS provider.
#[async_trait]
trait Dns01Steps: Send + Sync {
    async fn request(&self) -> Result<Requested, DomainServiceError>;
    /// Every later step acts on `order_url` only: an order created for the
    /// domain meanwhile (a retry from the console or the API) is someone
    /// else's, and is never published to, validated or cleaned up here.
    async fn publish(
        &self,
        order_url: &str,
        records: &[(String, String)],
    ) -> Result<(), DomainServiceError>;
    async fn wait_visible(&self, records: &[(String, String)]) -> Result<(), DomainServiceError>;
    async fn complete(&self, order_url: &str) -> Result<(), DomainServiceError>;
    /// Remove what `order_url` published; failures are logged, never
    /// raised: the outcome of issuing is what the caller needs.
    async fn clean_up(&self, order_url: &str);
}

async fn issue(steps: &dyn Dns01Steps) -> Result<(), DomainServiceError> {
    let (order_url, records) = match steps.request().await? {
        Requested::Issued => return Ok(()),
        Requested::Publish { order_url, records } => (order_url, records),
    };
    // `publish` records what it is about to create before creating it, so a
    // failure part way still leaves something to remove.
    let validated = async {
        steps.publish(&order_url, &records).await?;
        steps.wait_visible(&records).await?;
        steps.complete(&order_url).await
    }
    .await;
    steps.clean_up(&order_url).await;
    validated
}

struct LiveSteps<'a> {
    service: &'a DomainService,
    domain: &'a str,
    email: &'a str,
    provider_id: i32,
    provider_name: &'a str,
    provider: &'a dyn temps_dns::providers::DnsProvider,
    zone: &'a str,
}

impl LiveSteps<'_> {
    async fn order(&self) -> Result<Option<crate::tls::models::AcmeOrder>, DomainServiceError> {
        let domain = self
            .service
            .find_domain(DomainLookup::Name(self.domain))
            .await?;
        Ok(self
            .service
            .repository
            .find_acme_order_by_domain(domain.id)
            .await?)
    }

    /// The domain's order, while it is still the one at `order_url`.
    async fn this_order(
        &self,
        order_url: &str,
    ) -> Result<Option<crate::tls::models::AcmeOrder>, DomainServiceError> {
        Ok(still_this_order(self.order().await?, order_url))
    }

    fn replaced(&self, order_url: &str, before: &str) -> DomainServiceError {
        DomainServiceError::Challenge(format!(
            "The ACME order {order_url} for {} was replaced by a newer one before {before}, so \
             Temps left the newer order alone. Try again once it has finished.",
            self.domain
        ))
    }
}

/// `order` when it is still the one at `order_url`: a replacement is a
/// different order with its own challenge and cleanup receipt.
fn still_this_order(
    order: Option<crate::tls::models::AcmeOrder>,
    order_url: &str,
) -> Option<crate::tls::models::AcmeOrder> {
    order.filter(|order| order.order_url == order_url)
}

#[async_trait]
impl Dns01Steps for LiveSteps<'_> {
    async fn request(&self) -> Result<Requested, DomainServiceError> {
        let challenge = self
            .service
            .request_challenge_with(
                DomainLookup::Name(self.domain),
                self.email,
                true,
                Some(ChallengeType::Dns01),
            )
            .await?;
        if challenge.status == "completed" {
            return Ok(Requested::Issued);
        }
        if challenge.txt_records.is_empty() {
            return Err(DomainServiceError::Challenge(format!(
                "Let's Encrypt returned no DNS-01 TXT record to publish for {}",
                self.domain
            )));
        }
        // The order this request stored: the one carrying its challenge.
        let order_url = self
            .order()
            .await?
            .filter(|order| order.token.as_deref() == Some(challenge.token.as_str()))
            .map(|order| order.order_url)
            .ok_or_else(|| {
                DomainServiceError::Challenge(format!(
                    "The ACME order for {} was replaced while Temps was creating it, so Temps \
                     published nothing. Try again once the other request has finished.",
                    self.domain
                ))
            })?;
        Ok(Requested::Publish {
            order_url,
            records: challenge
                .txt_records
                .into_iter()
                .map(|r| (r.name, r.value))
                .collect(),
        })
    }

    async fn publish(
        &self,
        order_url: &str,
        records: &[(String, String)],
    ) -> Result<(), DomainServiceError> {
        let order = self
            .this_order(order_url)
            .await?
            .ok_or_else(|| self.replaced(order_url, "its DNS-01 record was published"))?;
        let published = publish_dns01_challenge(
            self.domain,
            self.provider,
            self.service.repository.as_ref(),
            order,
            self.provider_id,
            self.zone,
            records,
        )
        .await?;
        if published.created < published.total {
            return Err(DomainServiceError::Dns01Provider {
                domain: self.domain.to_string(),
                provider: self.provider_name.to_string(),
                detail: format!(
                    "created {} of {} record(s): {}",
                    published.created,
                    published.total,
                    published.failures.join("; ")
                ),
            });
        }
        Ok(())
    }

    async fn wait_visible(&self, records: &[(String, String)]) -> Result<(), DomainServiceError> {
        let checker = DnsPropagationChecker::new();
        for (name, values) in by_name(records) {
            let result = checker
                .wait_for_propagation(
                    &name,
                    &values,
                    PROPAGATION_MIN_PERCENT,
                    PROPAGATION_WAIT_SECS,
                    PROPAGATION_POLL_SECS,
                )
                .await;
            if !result.is_some_and(|r| r.is_propagated) {
                return Err(DomainServiceError::Dns01NotVisible {
                    domain: self.domain.to_string(),
                    record: name,
                    waited_secs: u64::from(PROPAGATION_WAIT_SECS),
                });
            }
        }
        Ok(())
    }

    async fn complete(&self, order_url: &str) -> Result<(), DomainServiceError> {
        if self.this_order(order_url).await?.is_none() {
            return Err(self.replaced(order_url, "Let's Encrypt was asked to validate it"));
        }
        self.service
            .complete_challenge_with(DomainLookup::Name(self.domain), self.email, true)
            .await
            .map(|_| ())
    }

    async fn clean_up(&self, order_url: &str) {
        let order = match self.this_order(order_url).await {
            Ok(Some(order)) => order,
            // Replaced: the newer order took over this one's cleanup receipt
            // (`request_challenge` moves it), so finalizing that order removes
            // these records. Touching them here would break its challenge.
            Ok(None) => {
                info!(
                    "The ACME order {} for {} was replaced; its DNS-01 records are left to the \
                     newer order's cleanup",
                    order_url, self.domain
                );
                return;
            }
            Err(e) => {
                warn!(
                    "Could not read the ACME order for {} to remove its DNS-01 records: {}",
                    self.domain, e
                );
                return;
            }
        };
        match clean_up_dns01_challenge(
            self.domain,
            self.provider,
            self.provider_id,
            self.service.repository.as_ref(),
            order,
        )
        .await
        {
            Ok(Dns01Cleanup::Nothing) => {}
            Ok(Dns01Cleanup::Removed(count)) => info!(
                "Removed {} DNS-01 challenge record(s) for {} from {}",
                count, self.domain, self.provider_name
            ),
            Ok(Dns01Cleanup::Incomplete(errors)) => warn!(
                "Some DNS-01 challenge records for {} are still at {}; the order keeps them for \
                 a retry of finalize: {}",
                self.domain,
                self.provider_name,
                errors.join("; ")
            ),
            Ok(Dns01Cleanup::Manual(reason)) => warn!(
                "The DNS-01 challenge records for {} at {} need removing by hand: {}",
                self.domain, self.provider_name, reason
            ),
            Err(e) => warn!(
                "Could not remove the DNS-01 challenge records for {}: {}",
                self.domain, e
            ),
        }
    }
}

/// Every value expected at each record name: a wildcard and its apex share
/// one `_acme-challenge` name with two values.
fn by_name(records: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in records {
        grouped.entry(name.clone()).or_default().push(value.clone());
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<&'static str>>,
        /// The order each step acted on.
        orders: Mutex<Vec<String>>,
        issued_without_challenge: bool,
        fail: Option<&'static str>,
    }

    const ORDER: &str = "https://acme.example/order/1";

    impl Fake {
        fn step(&self, name: &'static str) -> Result<(), DomainServiceError> {
            self.calls.lock().unwrap().push(name);
            if self.fail == Some(name) {
                return Err(DomainServiceError::Challenge(format!("{name} failed")));
            }
            Ok(())
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn on(&self, order_url: &str) {
            self.orders.lock().unwrap().push(order_url.to_string());
        }
    }

    #[async_trait]
    impl Dns01Steps for Fake {
        async fn request(&self) -> Result<Requested, DomainServiceError> {
            self.step("request")?;
            Ok(if self.issued_without_challenge {
                Requested::Issued
            } else {
                Requested::Publish {
                    order_url: ORDER.into(),
                    records: vec![("_acme-challenge.app.example.com".into(), "token".into())],
                }
            })
        }
        async fn publish(
            &self,
            order_url: &str,
            _: &[(String, String)],
        ) -> Result<(), DomainServiceError> {
            self.on(order_url);
            self.step("publish")
        }
        async fn wait_visible(&self, _: &[(String, String)]) -> Result<(), DomainServiceError> {
            self.step("wait_visible")
        }
        async fn complete(&self, order_url: &str) -> Result<(), DomainServiceError> {
            self.on(order_url);
            self.step("complete")
        }
        async fn clean_up(&self, order_url: &str) {
            self.on(order_url);
            self.calls.lock().unwrap().push("clean_up");
        }
    }

    #[tokio::test]
    async fn the_record_is_published_and_seen_before_validation_then_removed() {
        let steps = Fake::default();
        issue(&steps).await.unwrap();
        assert_eq!(
            steps.calls(),
            ["request", "publish", "wait_visible", "complete", "clean_up"]
        );
        // Every step acts on the order this call created, never on whichever
        // order the domain has by then.
        assert_eq!(*steps.orders.lock().unwrap(), [ORDER, ORDER, ORDER]);
    }

    fn order(order_url: &str) -> crate::tls::models::AcmeOrder {
        crate::tls::models::AcmeOrder {
            id: 1,
            order_url: order_url.into(),
            domain_id: 7,
            email: "ops@example.com".into(),
            status: "pending".into(),
            identifiers: serde_json::json!([]),
            authorizations: None,
            finalize_url: None,
            certificate_url: None,
            error: None,
            error_type: None,
            token: Some("token".into()),
            key_authorization: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    /// A newer order for the domain (a retry while this call waited for
    /// DNS) is someone else's: its records and receipt are never touched.
    #[test]
    fn a_replacement_order_is_never_taken_for_this_one() {
        assert!(still_this_order(Some(order(ORDER)), ORDER).is_some());
        assert!(still_this_order(Some(order("https://acme.example/order/2")), ORDER).is_none());
        assert!(still_this_order(None, ORDER).is_none());
    }

    #[tokio::test]
    async fn validation_is_never_asked_for_before_the_record_is_visible() {
        let steps = Fake {
            fail: Some("wait_visible"),
            ..Fake::default()
        };
        assert!(issue(&steps).await.is_err());
        assert_eq!(
            steps.calls(),
            ["request", "publish", "wait_visible", "clean_up"]
        );
    }

    #[tokio::test]
    async fn what_was_published_is_removed_whichever_step_fails() {
        for failing in ["publish", "complete"] {
            let steps = Fake {
                fail: Some(failing),
                ..Fake::default()
            };
            let error = issue(&steps).await.unwrap_err();
            assert!(error.to_string().contains(failing), "{error}");
            assert_eq!(steps.calls().last(), Some(&"clean_up"), "{failing}");
        }
    }

    #[tokio::test]
    async fn nothing_is_published_when_the_order_is_already_valid_or_fails() {
        let steps = Fake {
            issued_without_challenge: true,
            ..Fake::default()
        };
        issue(&steps).await.unwrap();
        assert_eq!(steps.calls(), ["request"]);

        let steps = Fake {
            fail: Some("request"),
            ..Fake::default()
        };
        assert!(issue(&steps).await.is_err());
        assert_eq!(steps.calls(), ["request"]);
    }

    #[test]
    fn a_wildcard_and_its_apex_are_awaited_under_one_name() {
        let grouped = by_name(&[
            ("_acme-challenge.example.com".into(), "a".into()),
            ("_acme-challenge.example.com".into(), "b".into()),
            ("_acme-challenge.other.example.com".into(), "c".into()),
        ]);
        assert_eq!(
            grouped.get("_acme-challenge.example.com"),
            Some(&vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(grouped.len(), 2);
    }
}
