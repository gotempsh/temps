//! Suppression list — recipients who must not receive further email due to
//! a hard bounce, a spam complaint, or a manual admin action. Checked by
//! `EmailService::send` before every send: without this, a permanently-bad
//! or complained address kept getting mailed on every subsequent send,
//! which is the exact pattern that gets a sending domain downgraded by
//! receiving mail providers.

use sea_orm::{
    sea_query::OnConflict, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
};
use std::sync::Arc;
use temps_entities::suppressed_recipients;

use crate::errors::EmailError;

/// Why an address was suppressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    /// A hard/permanent bounce (mailbox doesn't exist, domain rejects mail, …).
    Bounced,
    /// The recipient marked a message as spam.
    Complained,
    /// An admin suppressed (or un-suppressed) the address by hand.
    Manual,
}

impl SuppressionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            SuppressionReason::Bounced => "bounced",
            SuppressionReason::Complained => "complained",
            SuppressionReason::Manual => "manual",
        }
    }
}

/// Service for managing the email suppression list.
pub struct SuppressionService {
    db: Arc<DatabaseConnection>,
}

impl SuppressionService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Trim + lowercase for storage/lookup comparison. `pub(crate)` so
    /// callers filtering a recipient list against `suppressed_among`'s
    /// results (which come back normalized, not in their original casing)
    /// can match them correctly.
    pub(crate) fn normalize(email: &str) -> String {
        email.trim().to_lowercase()
    }

    /// Add an address to the suppression list, or update its reason if it's
    /// already there (e.g. a bounce followed later by a complaint).
    pub async fn suppress(
        &self,
        email: &str,
        reason: SuppressionReason,
        domain_id: i32,
        detail: Option<String>,
    ) -> Result<(), EmailError> {
        self.suppress_with(self.db.as_ref(), email, reason, domain_id, detail)
            .await
    }

    /// Transaction-aware variant used by SNS processing so the event and the
    /// resulting suppression become durable atomically before SNS is ACKed.
    pub async fn suppress_with<C: ConnectionTrait>(
        &self,
        connection: &C,
        email: &str,
        reason: SuppressionReason,
        domain_id: i32,
        detail: Option<String>,
    ) -> Result<(), EmailError> {
        let normalized = Self::normalize(email);

        suppressed_recipients::Entity::insert(suppressed_recipients::ActiveModel {
            email: Set(normalized),
            reason: Set(reason.as_str().to_string()),
            domain_id: Set(domain_id),
            detail: Set(detail),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::columns([
                suppressed_recipients::Column::DomainId,
                suppressed_recipients::Column::Email,
            ])
            .update_columns([
                suppressed_recipients::Column::Reason,
                suppressed_recipients::Column::Detail,
            ])
            .to_owned(),
        )
        .exec(connection)
        .await?;

        Ok(())
    }

    /// Remove an address from the suppression list (manual admin override —
    /// e.g. the mailbox was fixed, or the bounce/complaint was a mistake).
    pub async fn unsuppress(&self, domain_id: i32, email: &str) -> Result<(), EmailError> {
        let normalized = Self::normalize(email);
        suppressed_recipients::Entity::delete_many()
            .filter(suppressed_recipients::Column::Email.eq(normalized))
            .filter(suppressed_recipients::Column::DomainId.eq(domain_id))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Is this single address currently suppressed?
    pub async fn is_suppressed(&self, domain_id: i32, email: &str) -> Result<bool, EmailError> {
        let normalized = Self::normalize(email);
        let count = suppressed_recipients::Entity::find()
            .filter(suppressed_recipients::Column::Email.eq(normalized))
            .filter(suppressed_recipients::Column::DomainId.eq(domain_id))
            .count(self.db.as_ref())
            .await?;
        Ok(count > 0)
    }

    /// Which of these addresses are currently suppressed — one query instead
    /// of N, for `EmailService::send` checking every `to` recipient at once.
    pub async fn suppressed_among(
        &self,
        domain_id: i32,
        emails: &[String],
    ) -> Result<Vec<String>, EmailError> {
        if emails.is_empty() {
            return Ok(Vec::new());
        }
        let normalized: Vec<String> = emails.iter().map(|e| Self::normalize(e)).collect();
        let rows = suppressed_recipients::Entity::find()
            .filter(suppressed_recipients::Column::Email.is_in(normalized))
            .filter(suppressed_recipients::Column::DomainId.eq(domain_id))
            .all(self.db.as_ref())
            .await?;
        Ok(rows.into_iter().map(|r| r.email).collect())
    }

    /// Paginated list of the whole suppression list, most recent first.
    pub async fn list(
        &self,
        domain_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<suppressed_recipients::Model>, u64), EmailError> {
        let page = page.max(1);
        let page_size = std::cmp::min(page_size, 100).max(1);

        let paginator = suppressed_recipients::Entity::find()
            .filter(suppressed_recipients::Column::DomainId.eq(domain_id))
            .order_by_desc(suppressed_recipients::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;
        Ok((items, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{email_domains, email_providers};

    fn docker_is_unavailable(error: &impl std::fmt::Display) -> bool {
        let message = error.to_string().to_lowercase();
        message.contains("docker")
            || message.contains("testcontainers")
            || message.contains("container runtime")
            || message.contains("/var/run/docker.sock")
            || message.contains("failed to create a container")
            || message.contains("hyper legacy client")
    }

    async fn setup() -> Option<(TestDatabase, SuppressionService, i32, i32)> {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if docker_is_unavailable(&error) => {
                eprintln!("Docker unavailable, skipping suppression integration test: {error:#}");
                return None;
            }
            Err(error) => panic!("suppression test database setup failed: {error:#}"),
        };
        let provider = email_providers::ActiveModel {
            name: Set("Suppression test provider".to_string()),
            provider_type: Set("ses".to_string()),
            region: Set("us-east-1".to_string()),
            credentials: Set("test-credentials".to_string()),
            is_active: Set(true),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        let first_domain = email_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set("first.example.com".to_string()),
            status: Set("verified".to_string()),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        let second_domain = email_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set("second.example.com".to_string()),
            status: Set("verified".to_string()),
            ..Default::default()
        }
        .insert(db.db.as_ref())
        .await
        .unwrap();
        let service = SuppressionService::new(db.db.clone());
        Some((db, service, first_domain.id, second_domain.id))
    }

    macro_rules! require_setup {
        () => {
            match setup().await {
                Some(environment) => environment,
                None => return,
            }
        };
    }

    #[test]
    fn suppression_reason_as_str() {
        assert_eq!(SuppressionReason::Bounced.as_str(), "bounced");
        assert_eq!(SuppressionReason::Complained.as_str(), "complained");
        assert_eq!(SuppressionReason::Manual.as_str(), "manual");
    }

    #[test]
    fn normalize_trims_and_lowercases() {
        assert_eq!(
            SuppressionService::normalize("  Person@Example.COM  "),
            "person@example.com"
        );
    }

    #[tokio::test]
    async fn not_suppressed_by_default() {
        let (_db, service, domain_id, _) = require_setup!();
        assert!(!service
            .is_suppressed(domain_id, "nobody@example.com")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn suppress_then_is_suppressed() {
        let (_db, service, domain_id, _) = require_setup!();
        service
            .suppress(
                "Bounced@Example.com",
                SuppressionReason::Bounced,
                domain_id,
                Some("mailbox does not exist".to_string()),
            )
            .await
            .unwrap();

        // Case/whitespace-insensitive lookup.
        assert!(service
            .is_suppressed(domain_id, "  bounced@example.com  ")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn suppress_is_idempotent_and_updates_reason() {
        let (_db, service, domain_id, _) = require_setup!();
        let email = "person@example.com";

        service
            .suppress(email, SuppressionReason::Bounced, domain_id, None)
            .await
            .unwrap();
        service
            .suppress(email, SuppressionReason::Complained, domain_id, None)
            .await
            .unwrap();

        let (rows, total) = service.list(domain_id, 1, 10).await.unwrap();
        assert_eq!(total, 1, "re-suppressing must not create a duplicate row");
        assert_eq!(rows[0].reason, "complained");
    }

    #[tokio::test]
    async fn unsuppress_removes_the_address() {
        let (_db, service, domain_id, _) = require_setup!();
        let email = "person@example.com";
        service
            .suppress(email, SuppressionReason::Manual, domain_id, None)
            .await
            .unwrap();
        assert!(service.is_suppressed(domain_id, email).await.unwrap());

        service.unsuppress(domain_id, email).await.unwrap();
        assert!(!service.is_suppressed(domain_id, email).await.unwrap());
    }

    #[tokio::test]
    async fn unsuppress_nonexistent_is_a_noop() {
        let (_db, service, domain_id, _) = require_setup!();
        assert!(service
            .unsuppress(domain_id, "nobody@example.com")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn suppressed_among_returns_only_matches() {
        let (_db, service, domain_id, _) = require_setup!();
        service
            .suppress(
                "bad@example.com",
                SuppressionReason::Bounced,
                domain_id,
                None,
            )
            .await
            .unwrap();

        let result = service
            .suppressed_among(
                domain_id,
                &[
                    "bad@example.com".to_string(),
                    "good@example.com".to_string(),
                ],
            )
            .await
            .unwrap();

        assert_eq!(result, vec!["bad@example.com".to_string()]);
    }

    #[tokio::test]
    async fn suppressed_among_empty_input_short_circuits() {
        let (_db, service, domain_id, _) = require_setup!();
        assert_eq!(
            service.suppressed_among(domain_id, &[]).await.unwrap(),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn list_is_paginated_most_recent_first() {
        let (_db, service, domain_id, _) = require_setup!();
        for i in 0..3 {
            service
                .suppress(
                    &format!("person{i}@example.com"),
                    SuppressionReason::Manual,
                    domain_id,
                    None,
                )
                .await
                .unwrap();
        }

        let (page1, total) = service.list(domain_id, 1, 2).await.unwrap();
        assert_eq!(total, 3);
        assert_eq!(page1.len(), 2);

        let (page2, _) = service.list(domain_id, 2, 2).await.unwrap();
        assert_eq!(page2.len(), 1);
    }

    #[tokio::test]
    async fn concurrent_suppressions_are_atomic_and_leave_one_row() {
        let (_db, service, domain_id, _) = require_setup!();
        let service = Arc::new(service);
        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        let mut tasks = Vec::new();

        for task_index in 0..16 {
            let service = service.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                service
                    .suppress(
                        " Concurrent@Example.COM ",
                        SuppressionReason::Bounced,
                        domain_id,
                        Some(format!("notification-{task_index}")),
                    )
                    .await
            }));
        }

        for task in tasks {
            task.await
                .expect("suppression task must not panic")
                .expect("concurrent suppression must behave as an atomic upsert");
        }

        let (rows, total) = service.list(domain_id, 1, 100).await.unwrap();
        assert_eq!(
            total, 1,
            "concurrent notifications must create one suppression"
        );
        assert_eq!(rows[0].email, "concurrent@example.com");
    }

    #[tokio::test]
    async fn suppression_and_unsuppression_are_isolated_by_domain() {
        let (_db, service, first_domain_id, second_domain_id) = require_setup!();
        let recipient = "shared-recipient@example.com";

        service
            .suppress(
                recipient,
                SuppressionReason::Complained,
                first_domain_id,
                None,
            )
            .await
            .unwrap();

        assert!(service
            .is_suppressed(first_domain_id, recipient)
            .await
            .unwrap());
        assert!(!service
            .is_suppressed(second_domain_id, recipient)
            .await
            .unwrap());
        assert!(service
            .suppressed_among(second_domain_id, &[recipient.to_string()])
            .await
            .unwrap()
            .is_empty());

        service
            .suppress(
                recipient,
                SuppressionReason::Bounced,
                second_domain_id,
                None,
            )
            .await
            .unwrap();
        service
            .unsuppress(first_domain_id, recipient)
            .await
            .unwrap();

        assert!(!service
            .is_suppressed(first_domain_id, recipient)
            .await
            .unwrap());
        assert!(service
            .is_suppressed(second_domain_id, recipient)
            .await
            .unwrap());
    }
}
