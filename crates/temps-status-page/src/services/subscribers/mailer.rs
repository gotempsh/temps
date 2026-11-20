// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The mail side of subscriptions, behind a trait so the dispatcher and the
//! settings can be tested without an email provider.
//!
//! [`TempsEmailMailer`] sends through the transactional email stack
//! (`temps-email`), which applies the domain's suppression list on every
//! send: an address that bounced or complained is dropped there.

use std::sync::Arc;

use async_trait::async_trait;
use temps_email::services::{DomainService, EmailService, ProviderService, SendEmailRequest};

use super::error::SubscriptionError;

/// Where the console sends an operator to set up email.
pub const EMAIL_SETUP_PATH: &str = "/email";

/// Whether subscriber mail can go out from a sender, and why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailReadiness {
    /// Verified domains whose provider is active: the senders that work.
    pub verified_domains: Vec<String>,
    /// `None` when mail can be sent from the sender asked about.
    pub problem: Option<MailProblem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailProblem {
    pub reason: String,
    /// Console path that fixes it; `None` when the fix is on the same form.
    pub setup_path: Option<String>,
}

/// One message to one recipient.
#[derive(Debug, Clone)]
pub struct OutgoingMail {
    pub subscriber_id: i32,
    pub from: String,
    pub from_name: String,
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

#[async_trait]
pub trait SubscriberMailer: Send + Sync {
    /// Whether mail can be sent from `sender` (or, with `None`, whether any
    /// sender could work and which ones).
    async fn readiness(&self, sender: Option<&str>) -> Result<MailReadiness, SubscriptionError>;
    async fn send(&self, mail: OutgoingMail) -> Result<(), SubscriptionError>;
}

/// The domain part of an address, lowercased.
pub fn sender_domain(sender: &str) -> Option<String> {
    sender
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
}

/// Judge `sender` against the verified domains. Pure, for tests.
pub fn judge_sender(
    has_active_provider: bool,
    verified_domains: Vec<String>,
    sender: Option<&str>,
) -> MailReadiness {
    let problem = if !has_active_provider {
        Some(MailProblem {
            reason: "No email provider is configured".to_string(),
            setup_path: Some(EMAIL_SETUP_PATH.to_string()),
        })
    } else if verified_domains.is_empty() {
        Some(MailProblem {
            reason: "No email domain is verified; add and verify the domain to send from"
                .to_string(),
            setup_path: Some(EMAIL_SETUP_PATH.to_string()),
        })
    } else {
        match sender {
            None => Some(MailProblem {
                reason: "Choose a sender address at one of your verified email domains".to_string(),
                setup_path: None,
            }),
            Some(sender) => {
                let domain = sender_domain(sender).unwrap_or_default();
                (!verified_domains.contains(&domain)).then(|| MailProblem {
                    reason: format!(
                        "The sender address {sender} is not at a verified email domain with an \
                         active provider"
                    ),
                    setup_path: Some(EMAIL_SETUP_PATH.to_string()),
                })
            }
        }
    };
    MailReadiness {
        verified_domains,
        problem,
    }
}

pub struct TempsEmailMailer {
    email: Arc<EmailService>,
    providers: Arc<ProviderService>,
    domains: Arc<DomainService>,
}

impl TempsEmailMailer {
    pub fn new(
        email: Arc<EmailService>,
        providers: Arc<ProviderService>,
        domains: Arc<DomainService>,
    ) -> Self {
        Self {
            email,
            providers,
            domains,
        }
    }
}

#[async_trait]
impl SubscriberMailer for TempsEmailMailer {
    async fn readiness(&self, sender: Option<&str>) -> Result<MailReadiness, SubscriptionError> {
        let setup = |e: temps_email::EmailError| SubscriptionError::EmailSetup {
            reason: e.to_string(),
        };
        let active: Vec<i32> = self
            .providers
            .list_active()
            .await
            .map_err(setup)?
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        let mut verified: Vec<String> = self
            .domains
            .list_verified()
            .await
            .map_err(setup)?
            .into_iter()
            .filter(|domain| active.contains(&domain.provider_id))
            .map(|domain| domain.domain.to_ascii_lowercase())
            .collect();
        verified.sort();
        verified.dedup();
        Ok(judge_sender(!active.is_empty(), verified, sender))
    }

    async fn send(&self, mail: OutgoingMail) -> Result<(), SubscriptionError> {
        let subscriber_id = mail.subscriber_id;
        self.email
            .send(SendEmailRequest {
                from: mail.from,
                from_name: Some(mail.from_name),
                to: vec![mail.to],
                cc: None,
                bcc: None,
                reply_to: None,
                subject: mail.subject,
                html: Some(mail.html),
                text: Some(mail.text),
                headers: None,
                tags: Some(vec!["status-page".to_string()]),
                // The mail carries capability links (confirm, unsubscribe);
                // they are never rewritten through a tracker.
                track_opens: false,
                track_clicks: false,
            })
            .await
            .map(|_| ())
            .map_err(|e| SubscriptionError::Send {
                subscriber_id,
                reason: e.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_names_the_first_missing_piece() {
        let none = judge_sender(false, vec![], Some("status@example.test"));
        let problem = none.problem.expect("not ready");
        assert_eq!(problem.reason, "No email provider is configured");
        assert_eq!(problem.setup_path.as_deref(), Some(EMAIL_SETUP_PATH));

        let no_domain = judge_sender(true, vec![], None);
        assert!(no_domain
            .problem
            .expect("not ready")
            .reason
            .contains("No email domain is verified"));

        let no_sender = judge_sender(true, vec!["example.test".into()], None);
        let problem = no_sender.problem.expect("not ready");
        assert!(problem.reason.contains("Choose a sender"));
        assert_eq!(problem.setup_path, None);

        let wrong = judge_sender(true, vec!["example.test".into()], Some("a@other.test"));
        assert!(wrong
            .problem
            .expect("not ready")
            .reason
            .contains("a@other.test"));

        let ready = judge_sender(
            true,
            vec!["example.test".into()],
            Some("Status@Example.TEST"),
        );
        assert_eq!(ready.problem, None);
    }
}
