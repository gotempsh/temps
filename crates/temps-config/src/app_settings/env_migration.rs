// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The one-release shim migrating deprecated environment variables into settings.

use super::*;

// ── Deprecated environment variables -> settings (one-release shim) ─

/// What to do at boot with a deprecated environment variable, given the
/// setting that replaces it. Produced by [`decide_env_setting_migration`] and
/// [`decide_secret_env_migration`], which are pure so the decision is
/// unit-testable without touching process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvSettingMigration<T> {
    /// The variable is not set, or is blank: nothing to do.
    EnvUnset,
    /// The setting is still at its "never configured" value: persist this
    /// value once. From then on the setting is the only source.
    Persist(T),
    /// The setting already holds the value the variable carries.
    AlreadyPersisted,
    /// The setting holds a different, explicit value: the variable is ignored.
    SettingAlreadySet,
    /// The variable is set but unusable. Nothing is persisted.
    Invalid { reason: String },
    /// An earlier start already acted on this variable (see
    /// [`EnvMigrationLedger`]), so it is ignored even when the setting is
    /// back at its default: an admin may have chosen that value.
    AlreadyHandled,
}

impl<T> EnvSettingMigration<T> {
    /// Emit the deprecation `warn!` for this outcome. Never logs the value
    /// (it may be a credential). `Persist` is reported here too, before the
    /// write; [`record_env_setting_persisted`] reports the write itself.
    pub fn log(&self, env_var: &'static str, setting: &'static str) {
        match self {
            Self::EnvUnset => {}
            Self::Persist(_) => tracing::warn!(
                env_var,
                setting,
                "{env_var} is deprecated: persisting its value into the `{setting}` setting \
                 once. Remove {env_var} from the environment and change the value in Settings \
                 from now on"
            ),
            Self::AlreadyPersisted => tracing::warn!(
                env_var,
                setting,
                "{env_var} is deprecated and its value is already stored in the `{setting}` \
                 setting; remove {env_var} from the environment"
            ),
            Self::SettingAlreadySet => tracing::warn!(
                env_var,
                setting,
                "{env_var} is set but ignored: the `{setting}` setting already has a value, \
                 which wins. Remove {env_var} from the environment and change the value in \
                 Settings instead"
            ),
            Self::Invalid { reason } => tracing::warn!(
                env_var,
                setting,
                %reason,
                "{env_var} is deprecated and its value is invalid ({reason}); ignored and not \
                 persisted into `{setting}`. Remove {env_var} and configure `{setting}` in \
                 Settings"
            ),
            Self::AlreadyHandled => tracing::warn!(
                env_var,
                setting,
                "{env_var} is deprecated and was already migrated into the `{setting}` setting \
                 on an earlier start; ignored. Remove {env_var} from the environment"
            ),
        }
    }

    /// The value to persist, when there is one.
    pub fn into_persist(self) -> Option<T> {
        match self {
            Self::Persist(value) => Some(value),
            _ => None,
        }
    }

    /// Apply the ledger: a variable an earlier start already acted on is
    /// ignored. `EnvUnset` and `Invalid` pass through, so their logging is
    /// unchanged.
    pub fn unless_handled(self, ledger: &EnvMigrationLedger, env_var: &str) -> Self {
        match self {
            Self::Persist(_) | Self::AlreadyPersisted | Self::SettingAlreadySet
                if ledger.is_handled(env_var) =>
            {
                Self::AlreadyHandled
            }
            other => other,
        }
    }

    /// Whether the shim is done with this variable without writing it: the
    /// setting already holds its value, or an explicit value supersedes it.
    /// Such a variable is recorded in the ledger right away; a `Persist` is
    /// recorded only after its write succeeds.
    pub fn is_settled_without_write(&self) -> bool {
        matches!(self, Self::AlreadyPersisted | Self::SettingAlreadySet)
    }
}

/// Settings-row key of the [`EnvMigrationLedger`]. It lives beside the
/// [`AppSettings`] fields in the same JSON document but is not one of them,
/// so it is neither returned by nor writable through the settings API;
/// generic saves keep it because they merge into the stored document.
pub const ENV_MIGRATION_LEDGER_KEY: &str = "env_migrations";

/// The deprecated environment variables the one-release shim has already
/// acted on. Without it, "the setting is still at its default" cannot tell
/// "never configured" from "an admin chose the default", and every restart
/// would re-apply a variable still left in the environment over the admin's
/// choice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvMigrationLedger {
    #[serde(default)]
    pub handled: std::collections::BTreeSet<String>,
}

impl EnvMigrationLedger {
    pub fn is_handled(&self, env_var: &str) -> bool {
        self.handled.contains(env_var)
    }
}

/// Decide what a deprecated environment variable means for its replacement
/// setting.
///
/// - `env_value`: the raw variable, `None` when unset. Blank counts as unset.
/// - `stored`: the setting's current value.
/// - `unset`: the setting's "never configured" value (its default). A stored
///   value equal to it is treated as not set, so the variable is persisted.
/// - `parse`: turns the raw variable into the setting's type.
pub fn decide_env_setting_migration<T: PartialEq>(
    env_value: Option<&str>,
    stored: &T,
    unset: &T,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> EnvSettingMigration<T> {
    let Some(raw) = env_value.map(str::trim).filter(|value| !value.is_empty()) else {
        return EnvSettingMigration::EnvUnset;
    };
    let value = match parse(raw) {
        Ok(value) => value,
        Err(reason) => return EnvSettingMigration::Invalid { reason },
    };
    if *stored == value {
        EnvSettingMigration::AlreadyPersisted
    } else if stored != unset {
        EnvSettingMigration::SettingAlreadySet
    } else {
        EnvSettingMigration::Persist(value)
    }
}

/// A plaintext credential read from a deprecated environment variable, on its
/// way to being encrypted. `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvSecret(String);

impl EnvSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for EnvSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnvSecret([REDACTED])")
    }
}

/// [`decide_env_setting_migration`] for a credential stored as ciphertext.
/// Ciphertext is not comparable with the plaintext, so any stored value counts
/// as "already set" and the variable is ignored.
pub fn decide_secret_env_migration(
    env_value: Option<&str>,
    stored_configured: bool,
) -> EnvSettingMigration<EnvSecret> {
    let Some(raw) = env_value.map(str::trim).filter(|value| !value.is_empty()) else {
        return EnvSettingMigration::EnvUnset;
    };
    if stored_configured {
        EnvSettingMigration::SettingAlreadySet
    } else {
        EnvSettingMigration::Persist(EnvSecret(raw.to_string()))
    }
}

/// Audit record for a setting persisted from a deprecated environment
/// variable at boot. The actor is the server itself (no user). Carries the
/// variable and setting names only, never the value.
#[derive(Debug, Clone, Serialize)]
pub struct EnvSettingMigratedAudit {
    pub env_var: &'static str,
    pub setting: &'static str,
}

impl temps_core::AuditOperation for EnvSettingMigratedAudit {
    fn operation_type(&self) -> String {
        "SETTING_MIGRATED_FROM_ENV".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        None
    }

    fn user_agent(&self) -> &str {
        "temps-server (startup)"
    }

    fn serialize(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// Report that `setting` was persisted from `env_var`: a structured `info!`
/// always, plus an audit record when an audit logger is reachable. An audit
/// failure is logged and does not fail startup.
pub async fn record_env_setting_persisted(
    audit: Option<&dyn temps_core::AuditLogger>,
    env_var: &'static str,
    setting: &'static str,
) {
    tracing::info!(
        event = "setting_migrated_from_env",
        env_var,
        setting,
        "Persisted deprecated environment variable {env_var} into the `{setting}` setting"
    );
    if let Some(audit) = audit {
        let record = EnvSettingMigratedAudit { env_var, setting };
        if let Err(error) = audit.create_audit_log(&record).await {
            tracing::error!(
                env_var,
                setting,
                %error,
                "Failed to write the audit record for migrating {env_var} into `{setting}`"
            );
        }
    }
}

#[cfg(test)]
mod env_setting_migration_tests {
    use super::*;

    fn parse_u32(raw: &str) -> Result<u32, String> {
        raw.parse::<u32>().map_err(|e| e.to_string())
    }

    #[test]
    fn env_set_and_setting_unset_is_persisted() {
        let outcome = decide_env_setting_migration(Some("8"), &4u32, &4u32, parse_u32);
        assert_eq!(outcome, EnvSettingMigration::Persist(8));
    }

    #[test]
    fn setting_already_set_wins_and_env_is_ignored() {
        let outcome = decide_env_setting_migration(Some("8"), &16u32, &4u32, parse_u32);
        assert_eq!(outcome, EnvSettingMigration::SettingAlreadySet);
    }

    #[test]
    fn matching_value_is_not_written_again() {
        let outcome = decide_env_setting_migration(Some("8"), &8u32, &4u32, parse_u32);
        assert_eq!(outcome, EnvSettingMigration::AlreadyPersisted);
    }

    fn ledger_with(env_vars: &[&str]) -> EnvMigrationLedger {
        EnvMigrationLedger {
            handled: env_vars.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    #[test]
    fn a_handled_variable_is_not_reapplied_when_the_admin_chose_the_default() {
        // The setting is back at its default because an admin chose it, not
        // because it was never configured: the variable must stay ignored.
        let outcome = decide_env_setting_migration(Some("8"), &4u32, &4u32, parse_u32)
            .unless_handled(&ledger_with(&["TEMPS_X"]), "TEMPS_X");
        assert_eq!(outcome, EnvSettingMigration::AlreadyHandled);
        assert_eq!(outcome.into_persist(), None);
    }

    #[test]
    fn the_ledger_only_affects_its_own_variable() {
        let outcome = decide_env_setting_migration(Some("8"), &4u32, &4u32, parse_u32)
            .unless_handled(&ledger_with(&["TEMPS_OTHER"]), "TEMPS_X");
        assert_eq!(outcome, EnvSettingMigration::Persist(8));
    }

    #[test]
    fn the_ledger_keeps_unset_and_invalid_outcomes() {
        let ledger = ledger_with(&["TEMPS_X"]);
        assert_eq!(
            decide_env_setting_migration(None, &4u32, &4u32, parse_u32)
                .unless_handled(&ledger, "TEMPS_X"),
            EnvSettingMigration::EnvUnset
        );
        assert!(matches!(
            decide_env_setting_migration(Some("x"), &4u32, &4u32, parse_u32)
                .unless_handled(&ledger, "TEMPS_X"),
            EnvSettingMigration::Invalid { .. }
        ));
    }

    #[test]
    fn only_outcomes_without_a_write_are_settled_immediately() {
        let settled = |outcome: EnvSettingMigration<u32>| outcome.is_settled_without_write();
        assert!(settled(EnvSettingMigration::AlreadyPersisted));
        assert!(settled(EnvSettingMigration::SettingAlreadySet));
        // A Persist is recorded only once its write succeeds.
        assert!(!settled(EnvSettingMigration::Persist(8)));
        assert!(!settled(EnvSettingMigration::EnvUnset));
        assert!(!settled(EnvSettingMigration::AlreadyHandled));
        assert!(!settled(EnvSettingMigration::Invalid {
            reason: "bad".to_string()
        }));
    }

    #[test]
    fn unset_or_blank_env_does_nothing() {
        assert_eq!(
            decide_env_setting_migration(None, &4u32, &4u32, parse_u32),
            EnvSettingMigration::EnvUnset
        );
        assert_eq!(
            decide_env_setting_migration(Some("   "), &4u32, &4u32, parse_u32),
            EnvSettingMigration::EnvUnset
        );
    }

    #[test]
    fn invalid_env_is_reported_and_not_persisted() {
        let outcome = decide_env_setting_migration(Some("lots"), &4u32, &4u32, parse_u32);
        assert!(matches!(outcome, EnvSettingMigration::Invalid { .. }));
        assert_eq!(outcome.into_persist(), None);
    }

    #[test]
    fn env_value_is_trimmed_before_parsing() {
        let outcome = decide_env_setting_migration(Some(" 8 \n"), &4u32, &4u32, parse_u32);
        assert_eq!(outcome, EnvSettingMigration::Persist(8));
    }

    #[test]
    fn secret_env_is_persisted_only_when_nothing_is_stored() {
        assert_eq!(
            decide_secret_env_migration(Some("s3cr3t"), false),
            EnvSettingMigration::Persist(EnvSecret("s3cr3t".into()))
        );
        assert_eq!(
            decide_secret_env_migration(Some("s3cr3t"), true),
            EnvSettingMigration::SettingAlreadySet
        );
        assert_eq!(
            decide_secret_env_migration(Some(""), false),
            EnvSettingMigration::EnvUnset
        );
    }

    #[test]
    fn secret_debug_never_prints_the_value() {
        let outcome = decide_secret_env_migration(Some("s3cr3t"), false);
        assert!(!format!("{outcome:?}").contains("s3cr3t"));
    }

    #[test]
    fn migration_audit_records_names_only() {
        use temps_core::AuditOperation;
        let audit = EnvSettingMigratedAudit {
            env_var: "TEMPS_OTEL_S3_SECRET_KEY",
            setting: "otel_ingest.s3_archive.secret_key_encrypted",
        };
        assert_eq!(audit.operation_type(), "SETTING_MIGRATED_FROM_ENV");
        assert_eq!(audit.user_id(), None);
        let json = AuditOperation::serialize(&audit).expect("serializes");
        assert!(json.contains("TEMPS_OTEL_S3_SECRET_KEY"));
    }
}
