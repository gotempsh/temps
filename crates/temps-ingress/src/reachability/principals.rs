// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who a `tailnet_users` address admits (ADR 065 D6): its
//! `address_tailnet_principals` rows, read for the plan and the run, and
//! replaced by the run's `set_audience` in the same transaction as the
//! audience itself, so an address is never `tailnet_users` with someone
//! else's list.
//!
//! A row that does not parse is dropped on read rather than repaired: the
//! list it leaves admits fewer people, never more.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    Set,
};
use temps_core::tailnet::TailnetPrincipal;
use temps_entities::address_tailnet_principals;

/// The list stored for `address_id`, normalized and sorted.
pub async fn load<C: ConnectionTrait>(
    db: &C,
    address_id: &str,
) -> Result<Vec<TailnetPrincipal>, DbErr> {
    let rows = address_tailnet_principals::Entity::find()
        .filter(address_tailnet_principals::Column::AddressId.eq(address_id))
        .order_by_asc(address_tailnet_principals::Column::Id)
        .all(db)
        .await?;
    Ok(address_tailnet_principals::principals(&rows))
}

/// Make `address_id`'s list exactly `list`. Run inside the transaction that
/// writes the audience.
pub async fn replace<C: ConnectionTrait>(
    db: &C,
    address_id: &str,
    list: &[TailnetPrincipal],
) -> Result<(), DbErr> {
    address_tailnet_principals::Entity::delete_many()
        .filter(address_tailnet_principals::Column::AddressId.eq(address_id))
        .exec(db)
        .await?;
    let now = chrono::Utc::now();
    for principal in list {
        address_tailnet_principals::ActiveModel {
            address_id: Set(address_id.to_string()),
            kind: Set(principal.kind.as_str().to_string()),
            value: Set(principal.value.clone()),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i32, kind: &str, value: &str) -> address_tailnet_principals::Model {
        address_tailnet_principals::Model {
            id,
            address_id: "custom:7".into(),
            kind: kind.into(),
            value: value.into(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn rows_that_do_not_parse_are_dropped_never_widened() {
        let rows = [
            row(1, "login", "Alice@Example.com"),
            row(2, "tag", "tag:ci"),
            row(3, "group", "group:eng"),
            row(4, "login", "tag:server"),
            row(5, "tag", "nope"),
            row(6, "login", "alice@example.com"),
        ];
        assert_eq!(
            address_tailnet_principals::principals(&rows),
            vec![
                TailnetPrincipal::parse("alice@example.com").unwrap(),
                TailnetPrincipal::parse("tag:ci").unwrap(),
            ]
        );
    }
}
