// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Counting one ingested event in its group, and deciding whether the event
//! reopens the group (ADR 074, D1.3).
//!
//! This runs inside the ingest transaction, after the group row is locked,
//! and replaces the single counter update ingest always made. The common
//! path (an unresolved group) is still that one statement. An ignored group
//! with a user condition costs one more insert while the condition is active;
//! reopening a group costs one more cleanup statement, once.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

use super::super::types::{CreateErrorEventData, ErrorTrackingError};

/// Longest user key stored for the distinct-user ignore condition. Keys are
/// SDK-controlled; longer ones are cut, which can only merge two users that
/// share a 256-character prefix.
const MAX_USER_KEY_CHARS: usize = 256;

/// The identity an event's user is counted under: the first of user id,
/// email, username, IP address and visitor, as Sentry counts users.
pub(crate) fn user_key(event: &CreateErrorEventData) -> Option<String> {
    let text = [
        event.user_id.as_deref().map(|v| ("id", v)),
        event.user_email.as_deref().map(|v| ("email", v)),
        event.user_username.as_deref().map(|v| ("username", v)),
        event.user_ip_address.as_deref().map(|v| ("ip", v)),
    ]
    .into_iter()
    .flatten()
    .find(|(_, v)| !v.trim().is_empty())
    .map(|(kind, v)| format!("{kind}:{}", v.trim()));
    text.or_else(|| event.visitor_id.map(|id| format!("visitor:{id}")))
        .map(|key| key.chars().take(MAX_USER_KEY_CHARS).collect())
}

/// Count the event in `group_id` and return the group's status afterwards.
///
/// - `resolved` reopens to `unresolved`, as before, unless it was resolved in
///   the next release (D1.5): then only an event from a release newer than
///   the one it was resolved after reopens it.
/// - `ignored` with no condition reopens to `unresolved`, as before.
/// - `ignored` with conditions stays `ignored` until any one is met: the time
///   has passed, `total_count` reached the target, or enough new distinct
///   users were affected. `ignore_forever` never reopens.
///
/// When the group reopens, its ignore and resolution conditions and the users
/// counted for them are cleared, so a later plain ignore starts from nothing.
pub(crate) async fn count_event_in_group<C: ConnectionTrait>(
    tx: &C,
    group_id: i32,
    previous_status: &str,
    event_time: DateTime<Utc>,
    event: &CreateErrorEventData,
) -> Result<String, ErrorTrackingError> {
    // A new distinct user only matters to an ignored group waiting on a user
    // condition; the insert selects nothing for any other group.
    let mut new_user: i32 = 0;
    if previous_status == "ignored" {
        if let Some(key) = user_key(event) {
            let inserted = tx
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO error_group_ignore_users (error_group_id, user_key) \
                     SELECT id, $2 FROM error_groups \
                     WHERE id = $1 AND status = 'ignored' AND ignore_until_user_count IS NOT NULL \
                     ON CONFLICT DO NOTHING",
                    [group_id.into(), key.into()],
                ))
                .await?
                .rows_affected();
            new_user = i32::from(inserted > 0);
        }
    }

    let status: String = tx
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_groups SET \
               total_count = total_count + 1, \
               last_seen = GREATEST(last_seen, $2), \
               updated_at = NOW(), \
               ignore_user_count = ignore_user_count + $3, \
               status = CASE \
                 WHEN status = 'resolved' AND resolved_in_next_release AND NOT EXISTS ( \
                   SELECT 1 FROM error_releases r \
                   LEFT JOIN error_releases b ON b.id = error_groups.resolved_in_release_after_id \
                   WHERE r.project_id = error_groups.project_id AND r.version = $4 \
                     AND (b.id IS NULL OR (r.released_at, r.id) > (b.released_at, b.id))) \
                   THEN 'resolved' \
                 WHEN status = 'resolved' THEN 'unresolved' \
                 WHEN status = 'ignored' AND ( \
                   ignore_forever OR ( \
                     (ignore_until IS NOT NULL OR ignore_until_event_count IS NOT NULL \
                      OR ignore_until_user_count IS NOT NULL) \
                     AND (ignore_until IS NULL OR $2 < ignore_until) \
                     AND (ignore_until_event_count IS NULL \
                          OR total_count + 1 < ignore_until_event_count) \
                     AND (ignore_until_user_count IS NULL \
                          OR ignore_user_count + $3 < ignore_until_user_count))) \
                   THEN 'ignored' \
                 WHEN status = 'ignored' THEN 'unresolved' \
                 ELSE status END \
             WHERE id = $1 RETURNING status",
            [
                group_id.into(),
                event_time.into(),
                new_user.into(),
                event.release_version.as_deref().map(str::trim).into(),
            ],
        ))
        .await?
        .ok_or(ErrorTrackingError::GroupNotFound)?
        .try_get("", "status")?;

    if previous_status != status && status == "unresolved" {
        clear_ignore_conditions(tx, group_id).await?;
    }
    Ok(status)
}

/// Forget a group's ignore and resolution conditions and the users counted
/// for them.
pub(crate) async fn clear_ignore_conditions<C: ConnectionTrait>(
    tx: &C,
    group_id: i32,
) -> Result<(), ErrorTrackingError> {
    clear_ignore_conditions_many(tx, &[group_id]).await
}

/// [`clear_ignore_conditions`] for many groups in two statements.
pub(crate) async fn clear_ignore_conditions_many<C: ConnectionTrait>(
    tx: &C,
    group_ids: &[i32],
) -> Result<(), ErrorTrackingError> {
    if group_ids.is_empty() {
        return Ok(());
    }
    tx.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE error_groups SET ignore_until = NULL, ignore_until_event_count = NULL, \
         ignore_until_user_count = NULL, ignore_user_count = 0, ignore_forever = FALSE, \
         resolved_in_next_release = FALSE, resolved_in_release_after_id = NULL \
         WHERE id = ANY($1)",
        [group_ids.to_vec().into()],
    ))
    .await?;
    clear_ignore_users_many(tx, group_ids).await
}

/// Forget the distinct users counted for the groups' ignore conditions.
pub(crate) async fn clear_ignore_users_many<C: ConnectionTrait>(
    tx: &C,
    group_ids: &[i32],
) -> Result<(), ErrorTrackingError> {
    if group_ids.is_empty() {
        return Ok(());
    }
    tx.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "DELETE FROM error_group_ignore_users WHERE error_group_id = ANY($1)",
        [group_ids.to_vec().into()],
    ))
    .await?;
    Ok(())
}
