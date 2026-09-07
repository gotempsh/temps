// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // This migration originally reset every `is_managed = TRUE` row
        // unconditionally, on the theory that the only way a row could be
        // TRUE at this point was the name-guessing bug in the first shipped
        // version of m20260831_000002 (see that file's history). That
        // premise doesn't hold in practice: m20260831_000002's guessing
        // logic was corrected before it ever shipped in a standalone
        // release, while this migration shipped one release later. Any
        // install that upgraded through that intervening release had
        // already run the corrected m20260831_000002 (which never guesses
        // ownership) and, in the normal course of reconciliation, already
        // had `ensure_monitor_for_environment` create a legitimate managed
        // monitor. This migration's blanket UPDATE then demoted that
        // legitimate monitor too — indistinguishable from a name-guessed
        // one, since both use the identical "{environment} Monitor" naming
        // convention — causing reconciliation to create a second, duplicate
        // managed monitor on the very next boot.
        //
        // There is no data-driven way to tell a name-guessed row from a
        // legitimately created one after the fact, so correcting one case
        // by construction reintroduces the other. Given the guessing bug
        // never reached a standalone release (no evidence any production
        // database ever ran it), this migration is kept registered for
        // migration-history compatibility but no longer touches data.
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
