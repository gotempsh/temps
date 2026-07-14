//! Suppression list — recipients who must not receive further email due to
//! a hard bounce, a spam complaint, or a manual admin action. Checked by
//! `EmailService::send` before every send: without this, a permanently-bad
//! or complained address kept getting mailed on every subsequent send,
//! which is the exact pattern that gets a sending domain downgraded by
//! receiving mail providers.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
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
    fn as_str(&self) -> &'static str {
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
        domain_id: Option<i32>,
        detail: Option<String>,
    ) -> Result<(), EmailError> {
        let normalized = Self::normalize(email);

        let existing = suppressed_recipients::Entity::find()
            .filter(suppressed_recipients::Column::Email.eq(normalized.clone()))
            .one(self.db.as_ref())
            .await?;

        match existing {
            Some(model) => {
                let mut active: suppressed_recipients::ActiveModel = model.into();
                active.reason = Set(reason.as_str().to_string());
                active.domain_id = Set(domain_id);
                active.detail = Set(detail);
                active.update(self.db.as_ref()).await?;
            }
            None => {
                let active = suppressed_recipients::ActiveModel {
                    email: Set(normalized),
                    reason: Set(reason.as_str().to_string()),
                    domain_id: Set(domain_id),
                    detail: Set(detail),
                    ..Default::default()
                };
                active.insert(self.db.as_ref()).await?;
            }
        }

        Ok(())
    }

    /// Remove an address from the suppression list (manual admin override —
    /// e.g. the mailbox was fixed, or the bounce/complaint was a mistake).
    pub async fn unsuppress(&self, email: &str) -> Result<(), EmailError> {
        let normalized = Self::normalize(email);
        suppressed_recipients::Entity::delete_many()
            .filter(suppressed_recipients::Column::Email.eq(normalized))
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Is this single address currently suppressed?
    pub async fn is_suppressed(&self, email: &str) -> Result<bool, EmailError> {
        let normalized = Self::normalize(email);
        let count = suppressed_recipients::Entity::find()
            .filter(suppressed_recipients::Column::Email.eq(normalized))
            .count(self.db.as_ref())
            .await?;
        Ok(count > 0)
    }

    /// Which of these addresses are currently suppressed — one query instead
    /// of N, for `EmailService::send` checking every `to` recipient at once.
    pub async fn suppressed_among(&self, emails: &[String]) -> Result<Vec<String>, EmailError> {
        if emails.is_empty() {
            return Ok(Vec::new());
        }
        let normalized: Vec<String> = emails.iter().map(|e| Self::normalize(e)).collect();
        let rows = suppressed_recipients::Entity::find()
            .filter(suppressed_recipients::Column::Email.is_in(normalized))
            .all(self.db.as_ref())
            .await?;
        Ok(rows.into_iter().map(|r| r.email).collect())
    }

    /// Paginated list of the whole suppression list, most recent first.
    pub async fn list(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<suppressed_recipients::Model>, u64), EmailError> {
        let page = page.max(1);
        let page_size = std::cmp::min(page_size, 100).max(1);

        let paginator = suppressed_recipients::Entity::find()
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
    use temps_database::test_utils::TestDatabase;

    async fn setup() -> (TestDatabase, SuppressionService) {
        let db = TestDatabase::with_migrations().await.unwrap();
        let service = SuppressionService::new(db.db.clone());
        (db, service)
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
        let (_db, service) = setup().await;
        assert!(!service.is_suppressed("nobody@example.com").await.unwrap());
    }

    #[tokio::test]
    async fn suppress_then_is_suppressed() {
        let (_db, service) = setup().await;
        service
            .suppress(
                "Bounced@Example.com",
                SuppressionReason::Bounced,
                None,
                Some("mailbox does not exist".to_string()),
            )
            .await
            .unwrap();

        // Case/whitespace-insensitive lookup.
        assert!(service
            .is_suppressed("  bounced@example.com  ")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn suppress_is_idempotent_and_updates_reason() {
        let (_db, service) = setup().await;
        let email = "person@example.com";

        service
            .suppress(email, SuppressionReason::Bounced, None, None)
            .await
            .unwrap();
        service
            .suppress(email, SuppressionReason::Complained, None, None)
            .await
            .unwrap();

        let (rows, total) = service.list(1, 10).await.unwrap();
        assert_eq!(total, 1, "re-suppressing must not create a duplicate row");
        assert_eq!(rows[0].reason, "complained");
    }

    #[tokio::test]
    async fn unsuppress_removes_the_address() {
        let (_db, service) = setup().await;
        let email = "person@example.com";
        service
            .suppress(email, SuppressionReason::Manual, None, None)
            .await
            .unwrap();
        assert!(service.is_suppressed(email).await.unwrap());

        service.unsuppress(email).await.unwrap();
        assert!(!service.is_suppressed(email).await.unwrap());
    }

    #[tokio::test]
    async fn unsuppress_nonexistent_is_a_noop() {
        let (_db, service) = setup().await;
        assert!(service.unsuppress("nobody@example.com").await.is_ok());
    }

    #[tokio::test]
    async fn suppressed_among_returns_only_matches() {
        let (_db, service) = setup().await;
        service
            .suppress("bad@example.com", SuppressionReason::Bounced, None, None)
            .await
            .unwrap();

        let result = service
            .suppressed_among(&[
                "bad@example.com".to_string(),
                "good@example.com".to_string(),
            ])
            .await
            .unwrap();

        assert_eq!(result, vec!["bad@example.com".to_string()]);
    }

    #[tokio::test]
    async fn suppressed_among_empty_input_short_circuits() {
        let (_db, service) = setup().await;
        assert_eq!(
            service.suppressed_among(&[]).await.unwrap(),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn list_is_paginated_most_recent_first() {
        let (_db, service) = setup().await;
        for i in 0..3 {
            service
                .suppress(
                    &format!("person{i}@example.com"),
                    SuppressionReason::Manual,
                    None,
                    None,
                )
                .await
                .unwrap();
        }

        let (page1, total) = service.list(1, 2).await.unwrap();
        assert_eq!(total, 3);
        assert_eq!(page1.len(), 2);

        let (page2, _) = service.list(2, 2).await.unwrap();
        assert_eq!(page2.len(), 1);
    }
}
