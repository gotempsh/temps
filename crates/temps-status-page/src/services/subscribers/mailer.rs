// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The mail side of subscriptions, behind a trait so the dispatcher and the
//! settings can be tested without an email provider.
//!
//! [`TempsEmailMailer`] sends through the transactional email stack
//! (`temps-email`), which applies the domain's suppression list on every
//! send: an address that bounced or complained is dropped there. Every mail
//! carries a subscriber's own confirm or unsubscribe link, so it is sent
//! without history: the email log keeps neither the address nor the content.

use std::sync::Arc;

use async_trait::async_trait;
use temps_email::services::{
    DomainService, EmailService, ProviderService, SendEmailRequest, SUPPRESSED_RECIPIENTS_ERROR,
};

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

/// What became of one mail the provider took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// Handed to the provider (or possibly so: a send whose outcome is
    /// unknown is not repeated, so nobody gets the same mail twice).
    Delivered,
    /// Not sent: the address is on the domain's suppression list after a
    /// hard bounce or complaint. Never retried.
    Suppressed,
}

/// The outcome of a send from the status the email stack recorded for it.
/// A captured email that was not suppressed was not sent (no verified domain,
/// no active provider, a provider error), so it is an error to retry.
pub fn outcome_of(status: &str, error_message: Option<&str>) -> Result<Sent, String> {
    match status {
        "sent" | "delivery_unknown" => Ok(Sent::Delivered),
        "captured"
            if error_message
                .is_some_and(|message| message.starts_with(SUPPRESSED_RECIPIENTS_ERROR)) =>
        {
            Ok(Sent::Suppressed)
        }
        "captured" => Err(format!(
            "the email was captured instead of sent: {}",
            error_message.unwrap_or("no verified domain or active provider")
        )),
        other => Err(format!("the email ended in status {other}, not sent")),
    }
}

#[async_trait]
pub trait SubscriberMailer: Send + Sync {
    /// Whether mail can be sent from `sender` (or, with `None`, whether any
    /// sender could work and which ones).
    async fn readiness(&self, sender: Option<&str>) -> Result<MailReadiness, SubscriptionError>;
    /// Whether `sender`'s domain is authorized for every one of
    /// `project_ids`: the grant a project-scoped deployment token needs to
    /// send from it. `false` for an unknown domain or no project.
    async fn sender_granted(
        &self,
        sender: &str,
        project_ids: &[i32],
    ) -> Result<bool, SubscriptionError>;
    /// Send one mail. `Err` when it was not sent and should be retried.
    async fn send(&self, mail: OutgoingMail) -> Result<Sent, SubscriptionError>;
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

    async fn sender_granted(
        &self,
        sender: &str,
        project_ids: &[i32],
    ) -> Result<bool, SubscriptionError> {
        let setup = |e: temps_email::EmailError| SubscriptionError::EmailSetup {
            reason: e.to_string(),
        };
        let Some(domain) = sender_domain(sender) else {
            return Ok(false);
        };
        let Some(domain) = self
            .domains
            .find_by_domain_name(&domain)
            .await
            .map_err(setup)?
        else {
            return Ok(false);
        };
        if project_ids.is_empty() {
            return Ok(false);
        }
        for &project_id in project_ids {
            if !self
                .domains
                .is_authorized_for_project(domain.id, project_id)
                .await
                .map_err(setup)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn send(&self, mail: OutgoingMail) -> Result<Sent, SubscriptionError> {
        let subscriber_id = mail.subscriber_id;
        let failed = |reason: String| SubscriptionError::Send {
            subscriber_id,
            reason,
        };
        let response = self
            .email
            .send_without_history(SendEmailRequest {
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
            .map_err(|e| failed(e.to_string()))?;
        let error_message = if response.status == "captured" {
            self.email
                .get(response.id)
                .await
                .map_err(|e| failed(e.to_string()))?
                .error_message
        } else {
            None
        };
        outcome_of(&response.status, error_message.as_deref()).map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_sent_or_suppressed_email_counts_as_done() {
        assert_eq!(outcome_of("sent", None), Ok(Sent::Delivered));
        assert_eq!(outcome_of("delivery_unknown", None), Ok(Sent::Delivered));
        assert_eq!(
            outcome_of(
                "captured",
                Some(&format!(
                    "{SUPPRESSED_RECIPIENTS_ERROR} (previous hard bounce)"
                ))
            ),
            Ok(Sent::Suppressed)
        );
        let unsent = outcome_of("captured", Some("Domain example.test is not verified"))
            .expect_err("a capture is not a delivery");
        assert!(unsent.contains("not verified"), "{unsent}");
        assert!(outcome_of("captured", None).is_err());
        assert!(outcome_of("failed", None).is_err());
    }

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
