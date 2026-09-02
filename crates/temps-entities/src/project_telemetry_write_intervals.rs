// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Append-only ledger of where a project's spans actually went (ADR-041 §1).
//!
//! One row per contiguous period during which a project's spans went to one
//! place. Never updated except to close an open interval by setting
//! `effective_to`; never deleted.
//!
//! # Why a ledger and not a single `cutover_at` timestamp
//!
//! The mode genuinely flips more than once. An operator changes their mind;
//! Cloud is disconnected; the plan allowance is exhausted and span writes fall
//! back to local until it resets (ADR-041 §7b); the allowance resets and they
//! move back. A single timestamp models the common case and silently mis-routes
//! every other one — and mis-routing here means answering a query from the
//! wrong store, which ADR-040 §3's "never serve local rows under a Cloud label"
//! contract forbids outright.
//!
//! The common case is a one-row ledger, so this costs nothing where it does not
//! matter and is correct where it does.
//!
//! # Why the reason is an enum
//!
//! Most flips are not the operator's. A project that was Cloud-primary this
//! morning and is writing locally this afternoon needs to say *why* — "your
//! Cloud ingest allowance is exhausted" and "you disconnected Cloud" have
//! completely different fixes, and a self-hosted operator has nobody to ask.

use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

use super::cloud_telemetry_write_mode::CloudTelemetryWriteMode;

/// Why an interval opened.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum TelemetryWriteIntervalReason {
    /// An operator changed the write mode deliberately.
    #[default]
    #[sea_orm(string_value = "operator")]
    Operator,
    /// `DELETE /cloud`, or the Cloud telemetry feature switch being turned off.
    /// The declared `write_mode` on the project row is left alone: intent is
    /// preserved even though the destination changed.
    #[sea_orm(string_value = "cloud_disconnected")]
    CloudDisconnected,
    /// Cloud answered `QuotaExhausted`. Retrying until the queue overflowed
    /// would convert a billing state into data loss, so span writes resume
    /// locally until the allowance resets.
    #[sea_orm(string_value = "quota_exhausted")]
    QuotaExhausted,
    /// Cloud refused this instance's credential. Only the operator can fix it;
    /// waiting cannot.
    #[sea_orm(string_value = "credential_rejected")]
    CredentialRejected,
    /// The durable outbox hit its byte cap and spans were written to the local
    /// store rather than dropped.
    #[sea_orm(string_value = "queue_overflow_spill")]
    QueueOverflowSpill,
    /// Cloud started accepting again after a `local` fallback interval, so the
    /// operator's declared Cloud-primary intent takes effect again.
    #[sea_orm(string_value = "cloud_recovered")]
    CloudRecovered,
}

impl TelemetryWriteIntervalReason {
    /// One line the operator can act on, naming what happened and what to do.
    ///
    /// Never a bare enum name: the Console renders this verbatim and there is
    /// no support channel behind it.
    pub fn message(&self) -> &'static str {
        match self {
            TelemetryWriteIntervalReason::Operator => "Changed by an operator in project settings.",
            TelemetryWriteIntervalReason::CloudDisconnected => {
                "Temps Cloud was disconnected, or the Cloud telemetry switch was turned off. \
                 Spans are being stored on this instance again. Reconnect to resume \
                 Cloud-primary writes."
            }
            TelemetryWriteIntervalReason::QuotaExhausted => {
                "The Temps Cloud ingest allowance is exhausted. Spans are being stored on this \
                 instance until it resets. Raise the allowance, or accept local storage."
            }
            TelemetryWriteIntervalReason::CredentialRejected => {
                "Temps Cloud refused this instance's credential. Spans are being stored on this \
                 instance. Re-enroll to resume Cloud-primary writes."
            }
            TelemetryWriteIntervalReason::QueueOverflowSpill => {
                "The telemetry outbox reached its size cap while Cloud was unreachable. Spans \
                 are being stored on this instance so none are lost. Raise the cap in Cloud \
                 settings, or restore connectivity."
            }
            TelemetryWriteIntervalReason::CloudRecovered => {
                "Temps Cloud is accepting telemetry again, so this project's spans are \
                 Cloud-primary once more."
            }
        }
    }

    /// Whether this reason represents something the instance decided on the
    /// operator's behalf. Drives whether the Console nags.
    pub fn is_involuntary(&self) -> bool {
        !matches!(self, TelemetryWriteIntervalReason::Operator)
    }
}

impl std::fmt::Display for TelemetryWriteIntervalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelemetryWriteIntervalReason::Operator => write!(f, "operator"),
            TelemetryWriteIntervalReason::CloudDisconnected => write!(f, "cloud_disconnected"),
            TelemetryWriteIntervalReason::QuotaExhausted => write!(f, "quota_exhausted"),
            TelemetryWriteIntervalReason::CredentialRejected => write!(f, "credential_rejected"),
            TelemetryWriteIntervalReason::QueueOverflowSpill => write!(f, "queue_overflow_spill"),
            TelemetryWriteIntervalReason::CloudRecovered => write!(f, "cloud_recovered"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "project_telemetry_write_intervals")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: i32,
    /// Where spans went during this interval.
    pub mode: CloudTelemetryWriteMode,
    pub effective_from: DBDateTime,
    /// `NULL` while this is the open interval. Exactly one row per project may
    /// have `effective_to IS NULL`, enforced by a partial unique index.
    pub effective_to: Option<DBDateTime>,
    pub reason: TelemetryWriteIntervalReason,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_an_actionable_message() {
        // No state may render as a bare enum name in the Console.
        for reason in [
            TelemetryWriteIntervalReason::Operator,
            TelemetryWriteIntervalReason::CloudDisconnected,
            TelemetryWriteIntervalReason::QuotaExhausted,
            TelemetryWriteIntervalReason::CredentialRejected,
            TelemetryWriteIntervalReason::QueueOverflowSpill,
            TelemetryWriteIntervalReason::CloudRecovered,
        ] {
            let message = reason.message();
            assert!(!message.trim().is_empty(), "{reason} has no message");
            assert!(
                message.ends_with('.'),
                "{reason} message must be a sentence: {message}"
            );
        }
    }

    #[test]
    fn an_involuntary_flip_is_distinguishable_from_an_operator_one() {
        // The Console must be able to explain a flip the operator did not make
        // without treating their own deliberate change as an incident.
        assert!(!TelemetryWriteIntervalReason::Operator.is_involuntary());
        assert!(TelemetryWriteIntervalReason::QuotaExhausted.is_involuntary());
        assert!(TelemetryWriteIntervalReason::CloudDisconnected.is_involuntary());
        assert!(TelemetryWriteIntervalReason::CredentialRejected.is_involuntary());
        assert!(TelemetryWriteIntervalReason::QueueOverflowSpill.is_involuntary());
        assert!(TelemetryWriteIntervalReason::CloudRecovered.is_involuntary());
    }

    #[test]
    fn the_fallback_reasons_name_where_the_spans_are_now() {
        // A fallback message that does not say "on this instance" leaves the
        // operator unable to tell degraded from lost.
        for reason in [
            TelemetryWriteIntervalReason::CloudDisconnected,
            TelemetryWriteIntervalReason::QuotaExhausted,
            TelemetryWriteIntervalReason::CredentialRejected,
            TelemetryWriteIntervalReason::QueueOverflowSpill,
        ] {
            assert!(
                reason.message().contains("this instance"),
                "{reason} must say where spans are going now: {}",
                reason.message()
            );
        }
    }

    #[test]
    fn display_matches_the_stored_column_values() {
        assert_eq!(
            TelemetryWriteIntervalReason::Operator.to_string(),
            "operator"
        );
        assert_eq!(
            TelemetryWriteIntervalReason::CloudDisconnected.to_string(),
            "cloud_disconnected"
        );
        assert_eq!(
            TelemetryWriteIntervalReason::QuotaExhausted.to_string(),
            "quota_exhausted"
        );
        assert_eq!(
            TelemetryWriteIntervalReason::CredentialRejected.to_string(),
            "credential_rejected"
        );
        assert_eq!(
            TelemetryWriteIntervalReason::QueueOverflowSpill.to_string(),
            "queue_overflow_spill"
        );
        assert_eq!(
            TelemetryWriteIntervalReason::CloudRecovered.to_string(),
            "cloud_recovered"
        );
    }
}
