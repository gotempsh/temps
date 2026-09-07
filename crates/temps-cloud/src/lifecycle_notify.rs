// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Push backup lifecycle events (started/completed/failed) to Cloud as they
//! happen, instead of waiting for [`crate::backup_mirror`]'s next poll to
//! notice. That sweep remains the source of truth for what actually
//! happened -- a dropped or failed push here costs Cloud a stale
//! "processing" indicator until the next sweep tick, never an incorrect or
//! lost backup record.

use std::sync::{Arc, LazyLock};

use regex::Regex;
use temps_cloud_protocol::{BackupLifecycleEventRequest, BackupLifecycleStage};
use temps_core::{Job, JobQueue};
use tracing::{debug, error, info, warn};

use crate::service::CloudService;

/// Subscribe to the job queue and forward backup lifecycle events to Cloud.
/// Runs for the lifetime of the process; there is no shutdown signal because
/// the underlying `JobQueue` receiver simply stops yielding jobs on shutdown.
pub async fn run(service: Arc<CloudService>, queue: Arc<dyn JobQueue>) {
    info!("Cloud backup lifecycle notifier started");
    let mut receiver = queue.subscribe();
    loop {
        match receiver.recv().await {
            Ok(job) => {
                let Some(stage_job) = to_lifecycle_job(&job) else {
                    continue;
                };
                if !service.link().is_linked() {
                    debug!("Cloud is not linked; skipping backup lifecycle push");
                    continue;
                }
                let Some(instance_id) = service.link().instance_id() else {
                    debug!("Cloud link has no instance_id yet; skipping backup lifecycle push");
                    continue;
                };

                let event = BackupLifecycleEventRequest {
                    instance_id,
                    backup_id: stage_job.backup_id,
                    engine: stage_job.engine,
                    stage: stage_job.stage,
                    occurred_at: chrono::Utc::now(),
                    s3_location: stage_job.s3_location,
                    size_bytes: stage_job.size_bytes,
                    error_message: stage_job.error_message,
                };

                match service.link().notify_backup_lifecycle(&event).await {
                    Ok(_) => debug!(
                        backup_id = event.backup_id,
                        stage = ?event.stage,
                        "reported backup lifecycle event to Cloud",
                    ),
                    Err(e) => warn!(
                        backup_id = event.backup_id,
                        stage = ?event.stage,
                        error = %e,
                        "failed to report backup lifecycle event to Cloud; the mirror sweep will still catch the outcome",
                    ),
                }
            }
            Err(e) => {
                error!(
                    "backup lifecycle notifier failed to receive job from queue: {}",
                    e
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Intermediate, borrow-free extraction of the fields needed to build a
/// [`BackupLifecycleEventRequest`], decoupled from the enum match so the
/// match arms below stay a single line each.
struct LifecycleJob {
    backup_id: i32,
    engine: String,
    stage: BackupLifecycleStage,
    s3_location: Option<String>,
    size_bytes: Option<i64>,
    error_message: Option<String>,
}

fn to_lifecycle_job(job: &Job) -> Option<LifecycleJob> {
    match job {
        Job::BackupStarted(j) => Some(LifecycleJob {
            backup_id: j.backup_id,
            engine: j.engine.clone(),
            stage: BackupLifecycleStage::Started,
            s3_location: None,
            size_bytes: None,
            error_message: None,
        }),
        Job::BackupCompleted(j) => Some(LifecycleJob {
            backup_id: j.backup_id,
            engine: j.engine.clone(),
            stage: BackupLifecycleStage::Completed,
            s3_location: Some(j.s3_location.clone()),
            size_bytes: j.size_bytes,
            error_message: None,
        }),
        Job::BackupFailed(j) => Some(LifecycleJob {
            backup_id: j.backup_id,
            engine: j.engine.clone(),
            stage: BackupLifecycleStage::Failed,
            s3_location: None,
            size_bytes: None,
            error_message: Some(bound_error_message(&redact_credentials(&j.error_message))),
        }),
        _ => None,
    }
}

/// Failure reasons on this path come from raw engine stderr, which is not
/// scrubbed of credentials at every call site (`s3_mirror`'s own reason
/// string is the one place that already had to special-case this). This is
/// the first path that ships that text off-box: `redact_credentials` must run
/// before this truncates, since a secret straddling the 500-char boundary
/// would otherwise ship its still-live prefix.
const MAX_ERROR_MESSAGE_LEN: usize = 500;

fn bound_error_message(message: &str) -> String {
    if message.chars().count() <= MAX_ERROR_MESSAGE_LEN {
        return message.to_string();
    }
    let mut truncated: String = message.chars().take(MAX_ERROR_MESSAGE_LEN).collect();
    truncated.push_str(" [truncated]");
    truncated
}

/// `scheme://user:PASSWORD@host` -- keeps the scheme/user/host, drops the
/// password. Covers postgres/mysql/s3-style connection strings.
static CONN_STR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<scheme>[a-zA-Z][a-zA-Z0-9+.-]*://[^:/@\s]+:)[^@\s]+(?P<host>@)").unwrap()
});

/// `user:PASSWORD@tcp(host:port)/db` -- the DSN form WAL-G's Go MySQL driver
/// uses for MariaDB (`WALG_MYSQL_DATASOURCE_NAME`). It has no URL scheme, so
/// `CONN_STR_RE` does not match it.
static MYSQL_DSN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?P<user>[^:/@\s]+):[^@\s]+(?P<at>@tcp\()").unwrap());

/// `KEY=value` / `KEY: value` for known credential env-var names, in either
/// case. Value runs until whitespace or a shell/URL delimiter.
static ENV_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?P<key>AWS_SECRET_ACCESS_KEY|AWS_ACCESS_KEY_ID|AWS_SESSION_TOKEN|PGPASSWORD|S3_SECRET_KEY|S3_SECRET_ACCESS_KEY|WALG_MYSQL_DATASOURCE_NAME)(?P<sep>\s*[:=]\s*)(?P<value>[^\s&]+)",
    )
    .unwrap()
});

/// Catch-all: a `secret`/`password`/`passwd`/`token`/`key` keyword directly
/// followed by `=`/`:` and a long token-like value. Short, human-readable
/// values (e.g. "password: invalid") are left alone -- they are not secrets,
/// they are error text describing a rejected credential. The value charset
/// includes common generated-password punctuation (`!#$%^&*~`) in addition to
/// base64/hex characters -- narrower classes miss passwords like `Zx9!Qw8#Mn7$Rt4^`.
static GENERIC_CRED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:secret|password|passwd|token|key)\b(?P<sep>\s*[:=]\s*)(?P<value>[A-Za-z0-9+/_=!#$%^&*~-]{16,})").unwrap()
});

/// Scrub known credential shapes out of raw engine error text before it is
/// bounded and shipped to Cloud. Conservative by design: it is better to
/// over-redact a long token-like string than to leak a real secret.
fn redact_credentials(message: &str) -> String {
    let redacted = CONN_STR_RE.replace_all(message, "${scheme}***${host}");
    let redacted = MYSQL_DSN_RE.replace_all(&redacted, "${user}:***${at}");
    let redacted = ENV_KEY_RE.replace_all(&redacted, "${key}${sep}***");
    let redacted = GENERIC_CRED_RE.replace_all(&redacted, "${sep}***");
    redacted.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_postgres_connection_string_password() {
        let input =
            "connection failed: postgres://backup_user:S3cr3tPassw0rd!@db.internal:5432/app";
        let redacted = redact_credentials(input);
        assert!(!redacted.contains("S3cr3tPassw0rd!"));
        assert!(redacted.contains("postgres://backup_user:***@db.internal:5432/app"));
    }

    #[test]
    fn redact_mysql_connection_string_password() {
        let input = "mysql://root:hunter2hunter2@10.0.0.5:3306/mydb: connection refused";
        let redacted = redact_credentials(input);
        assert!(!redacted.contains("hunter2hunter2"));
        assert!(redacted.contains("mysql://root:***@10.0.0.5:3306/mydb"));
    }

    #[test]
    fn redact_aws_secret_access_key_equals() {
        let input = "wal-g upload failed: AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY exit 1";
        let redacted = redact_credentials(input);
        assert!(!redacted.contains("wJalrXUtnFEMI"));
        assert!(redacted.contains("AWS_SECRET_ACCESS_KEY=***"));
    }

    #[test]
    fn redact_pgpassword_assignment() {
        let input =
            "pg_dump: PGPASSWORD=correcthorsebatterystaple123 pg_dump failed: connection refused";
        let redacted = redact_credentials(input);
        assert!(!redacted.contains("correcthorsebatterystaple123"));
        assert!(redacted.contains("PGPASSWORD=***"));
    }

    #[test]
    fn plain_error_message_passes_through_unchanged() {
        let input = "mariadb-backup: could not connect to host db-01: timed out after 30s";
        assert_eq!(redact_credentials(input), input);
    }

    #[test]
    fn redact_mysql_dsn_without_scheme() {
        let input = "wal-g: failed to connect: root:hunter2hunter2@tcp(127.0.0.1:3306)/mysql: connection refused";
        let redacted = redact_credentials(input);
        assert!(!redacted.contains("hunter2hunter2"));
        assert!(redacted.contains("root:***@tcp(127.0.0.1:3306)/mysql"));
    }

    #[test]
    fn redact_walg_mysql_datasource_name_env_dump() {
        let input = "env: WALG_MYSQL_DATASOURCE_NAME=root:hunter2hunter2@tcp(127.0.0.1:3306)/mysql";
        let redacted = redact_credentials(input);
        assert!(!redacted.contains("hunter2hunter2"));
    }

    #[test]
    fn redact_generic_credential_with_special_characters() {
        let input = "config error: secret: Zx9!Qw8#Mn7$Rt4^Yh3&2024 rejected";
        let redacted = redact_credentials(input);
        assert!(!redacted.contains("Zx9!Qw8#Mn7$Rt4^Yh3&2024"));
    }

    #[test]
    fn short_password_keyword_value_not_redacted() {
        // Short, human-readable values describing a rejected credential are
        // not secrets and must not be mangled into noise.
        let input = "authentication failed: password: invalid";
        assert_eq!(redact_credentials(input), input);
    }

    #[test]
    fn redact_then_bound_removes_secret_before_truncation() {
        let secret = "a".repeat(20);
        let filler = "x".repeat(MAX_ERROR_MESSAGE_LEN);
        let input = format!("AWS_SECRET_ACCESS_KEY={secret} {filler}");
        let bounded = bound_error_message(&redact_credentials(&input));
        assert!(!bounded.contains(&secret));
    }

    #[test]
    fn bound_error_message_counts_chars_not_bytes() {
        // A 300-char string of a 2-byte UTF-8 character is 600 bytes but only
        // 300 chars -- it must NOT be reported as truncated.
        let input = "é".repeat(300);
        let bounded = bound_error_message(&input);
        assert_eq!(bounded, input);
        assert!(!bounded.contains("[truncated]"));
    }
}
