// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email subscribers of public status pages (ADR 108 D2).
//!
//! The public side (subscribe, confirm, unsubscribe) is reached only through a
//! published snapshot: an unknown or unpublished page is refused before the
//! database is touched. Its answers never reveal whether an address was
//! already subscribed. The admin side manages the per-page settings and the
//! subscriber list; addresses are personal data and leave the database only
//! through the authorized list and export endpoints.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
    TransactionTrait,
};
use temps_config::ConfigService;
use temps_core::{EncryptionService, UtcDateTime};
use temps_entities::{status_page_subscribers, status_pages};
use tracing::info;

use super::error::SubscriptionError;
use super::mailer::SubscriberMailer;
use super::tokens::{random_hex, token_hash, unsubscribe_token};
use super::types::{
    PublicOutcome, SubscriberListResponse, SubscriberResponse, SubscriptionCapability,
    SubscriptionSettingsResponse, UpdateSubscriptionSettingsRequest,
};
use crate::services::public_pages::{PublishedSnapshot, StatusSnapshotCache};

/// Most subscribers per page, confirmed and pending together.
pub const MAX_SUBSCRIBERS_PER_PAGE: u64 = 10_000;
/// Most unconfirmed requests per page. Bounds what a flood of subscribe
/// posts with made-up addresses can store and send.
pub const MAX_PENDING_PER_PAGE: u64 = 1_000;
/// A confirmation link is valid this long; an unconfirmed row is deleted
/// once its latest request is older.
pub const CONFIRMATION_VALID_HOURS: i64 = 48;
/// A repeated subscribe request sends a new confirmation only after this
/// long, so the form cannot be used to mail-bomb an address.
pub const RESEND_AFTER_MINUTES: i64 = 10;
/// Longest address accepted (RFC 5321 path limit).
pub const MAX_EMAIL_LEN: usize = 254;

/// Addresses of one page's links: the custom hostname when one is verified,
/// otherwise the slug URL on the configured public base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLinks {
    root: String,
    custom_host: bool,
}

impl PageLinks {
    pub fn for_snapshot(snapshot: &PublishedSnapshot, public_base: Option<&str>) -> Option<Self> {
        let mut hosts: Vec<&String> = snapshot.custom_hosts.iter().collect();
        hosts.sort();
        if let Some(host) = hosts.first() {
            return Some(Self {
                root: format!("https://{host}"),
                custom_host: true,
            });
        }
        public_base.map(|base| Self {
            root: format!(
                "{}/api/status/{}",
                base.trim_end_matches('/'),
                snapshot.slug
            ),
            custom_host: false,
        })
    }

    pub fn page(&self) -> String {
        if self.custom_host {
            format!("{}/", self.root)
        } else {
            self.root.clone()
        }
    }

    pub fn confirm(&self, token: &str) -> String {
        format!("{}/confirm/{token}", self.root)
    }

    pub fn unsubscribe(&self, token: &str) -> String {
        format!("{}/unsubscribe/{token}", self.root)
    }
}

/// Normalize and check a visitor-supplied address.
pub fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    (email.len() <= MAX_EMAIL_LEN
        && !email.chars().any(char::is_control)
        && temps_email::services::is_valid_email_syntax(&email))
    .then_some(email)
}

/// One CSV field, quoted, with formula prefixes neutralized so a spreadsheet
/// never evaluates an address.
fn csv_field(value: &str) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

#[derive(Debug, FromQueryResult)]
struct Counts {
    total: i64,
    pending: i64,
}

pub struct StatusPageSubscriberService {
    pub(crate) db: Arc<DatabaseConnection>,
    pub(crate) cache: Arc<StatusSnapshotCache>,
    pub(crate) encryption: Arc<EncryptionService>,
    pub(crate) mailer: Arc<dyn SubscriberMailer>,
    config: Arc<ConfigService>,
}

impl StatusPageSubscriberService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        cache: Arc<StatusSnapshotCache>,
        encryption: Arc<EncryptionService>,
        mailer: Arc<dyn SubscriberMailer>,
        config: Arc<ConfigService>,
    ) -> Self {
        Self {
            db,
            cache,
            encryption,
            mailer,
            config,
        }
    }

    /// The configured public base URL, `None` when none is set.
    pub(crate) async fn public_base(&self) -> Result<Option<String>, SubscriptionError> {
        let settings =
            self.config
                .get_settings()
                .await
                .map_err(|e| SubscriptionError::Settings {
                    reason: e.to_string(),
                })?;
        Ok(settings.configured_public_ingress_url())
    }

    // ---- public side -------------------------------------------------------

    /// A visitor asked to subscribe `raw_email` to the page of `snapshot`.
    /// Answers the same whether the address is new, pending or confirmed.
    pub async fn subscribe(
        &self,
        snapshot: &PublishedSnapshot,
        raw_email: &str,
    ) -> Result<PublicOutcome, SubscriptionError> {
        if !snapshot.accepts_subscriptions() {
            return Ok(PublicOutcome::Full);
        }
        let Some(email) = normalize_email(raw_email) else {
            return Ok(PublicOutcome::InvalidEmail);
        };
        let page_id = snapshot.status_page_id;
        let now = Utc::now();
        let txn = self.db.begin().await?;
        // Serializes subscribe requests per page, so the caps hold exactly.
        let locked: Option<i32> = status_pages::Entity::find_by_id(page_id)
            .select_only()
            .column(status_pages::Column::Id)
            .filter(status_pages::Column::SubscriptionsEnabled.eq(true))
            .lock_exclusive()
            .into_tuple()
            .one(&txn)
            .await?;
        if locked.is_none() {
            return Ok(PublicOutcome::Full);
        }
        // At either cap every address gets the same answer, known or not, so
        // the difference cannot tell anyone which addresses are subscribed.
        let counts = Counts::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT count(*) AS total, count(*) FILTER (WHERE state = 'pending') AS pending \
             FROM status_page_subscribers WHERE status_page_id = $1",
            [page_id.into()],
        ))
        .one(&txn)
        .await?
        .unwrap_or(Counts {
            total: 0,
            pending: 0,
        });
        if counts.total.max(0) as u64 >= MAX_SUBSCRIBERS_PER_PAGE
            || counts.pending.max(0) as u64 >= MAX_PENDING_PER_PAGE
        {
            tracing::warn!(
                status_page_id = page_id,
                total = counts.total,
                pending = counts.pending,
                "status page subscription refused: the page is at its subscriber limit"
            );
            return Ok(PublicOutcome::Full);
        }
        let existing = status_page_subscribers::Entity::find()
            .filter(status_page_subscribers::Column::StatusPageId.eq(page_id))
            .filter(status_page_subscribers::Column::Email.eq(email.as_str()))
            .one(&txn)
            .await?;
        match existing {
            Some(row) if row.state == status_page_subscribers::STATE_CONFIRMED => {}
            Some(row) => {
                let resend_due = row.confirmation_sent_at.is_none_or(|sent| {
                    sent < now - chrono::Duration::minutes(RESEND_AFTER_MINUTES)
                });
                if resend_due {
                    let mut active: status_page_subscribers::ActiveModel = row.into();
                    active.confirmation_sent_at = Set(None);
                    active.requested_at = Set(now);
                    active.update(&txn).await?;
                }
            }
            None => {
                let nonce = random_hex()?;
                let unsubscribe_hash = token_hash(&unsubscribe_token(&self.encryption, &nonce));
                status_page_subscribers::ActiveModel {
                    status_page_id: Set(page_id),
                    email: Set(email),
                    state: Set(status_page_subscribers::STATE_PENDING.to_string()),
                    requested_at: Set(now),
                    unsubscribe_nonce: Set(nonce),
                    unsubscribe_token_hash: Set(unsubscribe_hash),
                    ..Default::default()
                }
                .insert(&txn)
                .await?;
            }
        }
        txn.commit().await?;
        Ok(PublicOutcome::Accepted)
    }

    /// Follow a confirmation link. Valid once, before it expires.
    pub async fn confirm(
        &self,
        snapshot: &PublishedSnapshot,
        token: &str,
    ) -> Result<PublicOutcome, SubscriptionError> {
        let now = Utc::now();
        let result = status_page_subscribers::Entity::update_many()
            .col_expr(
                status_page_subscribers::Column::State,
                Expr::value(status_page_subscribers::STATE_CONFIRMED),
            )
            .col_expr(
                status_page_subscribers::Column::ConfirmedAt,
                Expr::value(now),
            )
            .col_expr(
                status_page_subscribers::Column::ConfirmTokenHash,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                status_page_subscribers::Column::ConfirmExpiresAt,
                Expr::value(Option::<UtcDateTime>::None),
            )
            .col_expr(status_page_subscribers::Column::UpdatedAt, Expr::value(now))
            .filter(status_page_subscribers::Column::StatusPageId.eq(snapshot.status_page_id))
            .filter(status_page_subscribers::Column::ConfirmTokenHash.eq(token_hash(token)))
            .filter(
                status_page_subscribers::Column::State.eq(status_page_subscribers::STATE_PENDING),
            )
            .filter(status_page_subscribers::Column::ConfirmExpiresAt.gt(now))
            .exec(self.db.as_ref())
            .await?;
        Ok(if result.rows_affected == 1 {
            info!(
                status_page_id = snapshot.status_page_id,
                "status page subscriber confirmed"
            );
            PublicOutcome::Confirmed
        } else {
            PublicOutcome::LinkInvalid
        })
    }

    /// Follow an unsubscribe link: the row, and with it the address, is
    /// deleted. Answers the same for a link that was already used.
    pub async fn unsubscribe(
        &self,
        snapshot: &PublishedSnapshot,
        token: &str,
    ) -> Result<PublicOutcome, SubscriptionError> {
        let result = status_page_subscribers::Entity::delete_many()
            .filter(status_page_subscribers::Column::StatusPageId.eq(snapshot.status_page_id))
            .filter(status_page_subscribers::Column::UnsubscribeTokenHash.eq(token_hash(token)))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected > 0 {
            info!(
                status_page_id = snapshot.status_page_id,
                "status page subscriber unsubscribed"
            );
        }
        Ok(PublicOutcome::Unsubscribed)
    }

    // ---- admin side --------------------------------------------------------

    async fn find_page(
        &self,
        status_page_id: i32,
    ) -> Result<status_pages::Model, SubscriptionError> {
        status_pages::Entity::find_by_id(status_page_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(SubscriptionError::StatusPageNotFound { status_page_id })
    }

    /// Whether links can be built for `page`: a verified custom hostname or a
    /// configured public URL.
    async fn links_capability(
        &self,
        page: &status_pages::Model,
    ) -> Result<SubscriptionCapability, SubscriptionError> {
        let has_custom_host = self
            .cache
            .get(&page.slug)
            .is_some_and(|snapshot| !snapshot.custom_hosts.is_empty());
        if has_custom_host || self.public_base().await?.is_some() {
            return Ok(SubscriptionCapability::ready());
        }
        Ok(SubscriptionCapability::missing(
            "Temps has no public URL, so confirmation and unsubscribe links would have no \
             address people can open. Set the external URL or the public ingress URL, or add \
             a custom domain to this page.",
            Some(temps_config::public_ingress::PUBLIC_INGRESS_SETUP_PATH.to_string()),
        ))
    }

    async fn settings_for(
        &self,
        page: status_pages::Model,
    ) -> Result<SubscriptionSettingsResponse, SubscriptionError> {
        let readiness = self
            .mailer
            .readiness(page.subscription_sender.as_deref())
            .await?;
        let links = self.links_capability(&page).await?;
        let counts = Counts::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT count(*) AS total, count(*) FILTER (WHERE state = 'pending') AS pending \
             FROM status_page_subscribers WHERE status_page_id = $1",
            [page.id.into()],
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(Counts {
            total: 0,
            pending: 0,
        });
        let pending = counts.pending.max(0) as u64;
        Ok(SubscriptionSettingsResponse {
            status_page_id: page.id,
            enabled: page.subscriptions_enabled,
            enabled_at: page.subscriptions_enabled_at,
            sender: page.subscription_sender,
            email: match readiness.problem {
                None => SubscriptionCapability::ready(),
                Some(problem) => {
                    SubscriptionCapability::missing(problem.reason, problem.setup_path)
                }
            },
            links,
            verified_domains: readiness.verified_domains,
            confirmed_count: (counts.total.max(0) as u64).saturating_sub(pending),
            pending_count: pending,
            max_subscribers: MAX_SUBSCRIBERS_PER_PAGE,
        })
    }

    pub async fn settings(
        &self,
        status_page_id: i32,
    ) -> Result<SubscriptionSettingsResponse, SubscriptionError> {
        let page = self.find_page(status_page_id).await?;
        self.settings_for(page).await
    }

    /// Change the settings. Switching subscriptions on needs a sender mail can
    /// go out from and an address for the links, so a visitor is never asked
    /// for an address that could not be confirmed.
    ///
    /// The page row is locked and read again inside the transaction that
    /// writes it, and only the fields in `request` are written, so two saves
    /// never undo each other: a sender change that finishes after a switch-off
    /// leaves subscriptions off.
    ///
    /// `scoped_projects` is `Some` for a credential confined to projects (a
    /// project-scoped API key): it may only choose a sender whose domain is
    /// authorized for every one of them, and the page remembers that, so
    /// delivery checks the grant again.
    pub async fn update_settings(
        &self,
        status_page_id: i32,
        request: UpdateSubscriptionSettingsRequest,
        scoped_projects: Option<&[i32]>,
    ) -> Result<SubscriptionSettingsResponse, SubscriptionError> {
        let txn = self.db.begin().await?;
        let page = status_pages::Entity::find_by_id(status_page_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(SubscriptionError::StatusPageNotFound { status_page_id })?;
        let sender = match request.sender.as_deref().map(str::trim) {
            None => page.subscription_sender.clone(),
            Some("") => None,
            Some(raw) => {
                Some(
                    normalize_email(raw).ok_or_else(|| SubscriptionError::InvalidSender {
                        sender: raw.to_string(),
                        reason: "it is not a valid email address".to_string(),
                    })?,
                )
            }
        };
        let enabled = request.enabled.unwrap_or(page.subscriptions_enabled);
        if let (Some(_), Some(sender)) = (request.sender.as_ref(), sender.as_ref()) {
            // With no verified domain yet, the address is kept so the form
            // remembers it; switching on is refused below until it works.
            let readiness = self.mailer.readiness(Some(sender)).await?;
            let domain = super::mailer::sender_domain(sender).unwrap_or_default();
            if !readiness.verified_domains.is_empty()
                && !readiness.verified_domains.contains(&domain)
            {
                return Err(SubscriptionError::InvalidSender {
                    sender: sender.clone(),
                    reason: format!(
                        "{domain} is not a verified email domain with an active provider; use \
                         one of: {}",
                        readiness.verified_domains.join(", ")
                    ),
                });
            }
            if let Some(project_ids) = scoped_projects {
                if !self.mailer.sender_granted(sender, project_ids).await? {
                    let projects = project_ids
                        .iter()
                        .map(i32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(SubscriptionError::InvalidSender {
                        sender: sender.clone(),
                        reason: format!(
                            "{domain} is not authorized for project {projects}; this credential \
                             can only send from a domain granted to the page's projects"
                        ),
                    });
                }
            }
        }
        if enabled {
            let readiness = self.mailer.readiness(sender.as_deref()).await?;
            if let Some(problem) = readiness.problem {
                return Err(SubscriptionError::NotReady {
                    status_page_id,
                    reason: problem.reason,
                });
            }
            let links = self.links_capability(&page).await?;
            if let Some(reason) = links.reason.filter(|_| !links.configured) {
                return Err(SubscriptionError::NotReady {
                    status_page_id,
                    reason,
                });
            }
        }
        let switching_on = enabled && !page.subscriptions_enabled;
        let mut active: status_pages::ActiveModel = page.clone().into();
        if request.enabled.is_some() {
            active.subscriptions_enabled = Set(enabled);
            if switching_on {
                active.subscriptions_enabled_at = Set(Some(Utc::now()));
            }
        }
        if request.sender.is_some() {
            active.subscription_sender_scoped = Set(sender.is_some() && scoped_projects.is_some());
            active.subscription_sender = Set(sender);
        }
        let page = if active.is_changed() {
            active.update(&txn).await?
        } else {
            page
        };
        txn.commit().await?;
        // The form appears or disappears with the snapshot. A failed rebuild
        // leaves the previous snapshot; the background refresh retries.
        if let Err(error) = self.cache.refresh_page(status_page_id).await {
            tracing::warn!(
                status_page_id,
                error = %error,
                "status page subscription settings saved but the public snapshot could not be rebuilt; the next refresh applies them"
            );
        }
        self.settings_for(page).await
    }

    pub async fn list(
        &self,
        status_page_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<SubscriberListResponse, SubscriptionError> {
        self.find_page(status_page_id).await?;
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let paginator = status_page_subscribers::Entity::find()
            .filter(status_page_subscribers::Column::StatusPageId.eq(status_page_id))
            .order_by_desc(status_page_subscribers::Column::CreatedAt)
            .order_by_desc(status_page_subscribers::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator
            .fetch_page(page - 1)
            .await?
            .into_iter()
            .map(SubscriberResponse::from)
            .collect();
        Ok(SubscriberListResponse {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn delete(
        &self,
        status_page_id: i32,
        subscriber_id: i32,
    ) -> Result<(), SubscriptionError> {
        let result = status_page_subscribers::Entity::delete_many()
            .filter(status_page_subscribers::Column::Id.eq(subscriber_id))
            .filter(status_page_subscribers::Column::StatusPageId.eq(status_page_id))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(SubscriptionError::SubscriberNotFound {
                status_page_id,
                subscriber_id,
            });
        }
        Ok(())
    }

    /// Every subscriber of the page as CSV (`email,state,subscribed_at,
    /// confirmed_at`). Bounded by [`MAX_SUBSCRIBERS_PER_PAGE`] rows (well
    /// under a megabyte), so it is built in memory. Returns the row count too.
    pub async fn export_csv(
        &self,
        status_page_id: i32,
    ) -> Result<(String, usize), SubscriptionError> {
        self.find_page(status_page_id).await?;
        let rows = status_page_subscribers::Entity::find()
            .filter(status_page_subscribers::Column::StatusPageId.eq(status_page_id))
            .order_by_asc(status_page_subscribers::Column::Id)
            .limit(MAX_SUBSCRIBERS_PER_PAGE)
            .all(self.db.as_ref())
            .await?;
        let mut csv = String::from("email,state,subscribed_at,confirmed_at\n");
        for row in &rows {
            csv.push_str(&format!(
                "{},{},{},{}\n",
                csv_field(&row.email),
                csv_field(&row.state),
                row.created_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                row.confirmed_at
                    .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_default(),
            ));
        }
        Ok((csv, rows.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_lowercased_trimmed_and_checked() {
        assert_eq!(
            normalize_email("  Someone@Example.TEST "),
            Some("someone@example.test".to_string())
        );
        for bad in [
            "",
            "no-at-sign",
            "a@",
            "@example.test",
            "a\n@example.test",
            &format!("{}@example.test", "a".repeat(250)),
        ] {
            assert_eq!(normalize_email(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn csv_fields_are_quoted_and_formulas_neutralized() {
        assert_eq!(csv_field("a@example.test"), "\"a@example.test\"");
        assert_eq!(csv_field("=cmd@example.test"), "\"'=cmd@example.test\"");
        assert_eq!(csv_field("a\"b@example.test"), "\"a\"\"b@example.test\"");
    }
}
