// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use cron::Schedule;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use temps_core::jobs::AutopilotTriggerJob;
use temps_core::{Job, JobQueue};

use crate::services::config_service::AgentConfigService;

/// Background scheduler that polls agent configs for cron schedules
/// and emits `AutopilotTrigger` jobs when they fire.
///
/// Follows the same minute-aligned polling pattern as `DatabaseCronConfigService`
/// in `temps-deployments`.
pub struct AgentCronScheduler {
    config_service: Arc<AgentConfigService>,
    queue: Arc<dyn JobQueue>,
}

impl AgentCronScheduler {
    pub fn new(config_service: Arc<AgentConfigService>, queue: Arc<dyn JobQueue>) -> Self {
        Self {
            config_service,
            queue,
        }
    }

    /// Start the scheduler loop. Runs indefinitely.
    /// Should be spawned as a background task.
    pub async fn run(&self) {
        tracing::info!("Agent cron scheduler started");

        loop {
            // Sleep until the start of the next minute
            let now = Utc::now();
            let secs_into_minute = now.timestamp() % 60;
            let sleep_secs = if secs_into_minute == 0 {
                60 // Already at the start of a minute, wait for the next one
            } else {
                60 - secs_into_minute
            };
            tokio::time::sleep(Duration::from_secs(sleep_secs as u64)).await;

            if let Err(e) = self.tick().await {
                tracing::error!("Agent cron scheduler tick failed: {}", e);
            }
        }
    }

    /// Process one tick: load all agents with cron schedules, check if they should fire now.
    async fn tick(&self) -> Result<(), String> {
        let now = Utc::now();

        // Load all enabled agents across all projects
        let agents = self
            .config_service
            .list_all_enabled_agents()
            .await
            .map_err(|e| format!("Failed to load agents: {}", e))?;

        let mut triggered = 0;

        for agent in &agents {
            // Extract cron schedule from trigger_config: { "schedule": { "cron": "0 0 * * *" } }
            let cron_expr = agent
                .trigger_config
                .get("schedule")
                .and_then(|s| s.get("cron"))
                .and_then(|v| v.as_str());

            let cron_expr = match cron_expr {
                Some(expr) if !expr.is_empty() => expr,
                _ => continue, // No cron schedule configured
            };

            // Parse and check if the cron matches the current minute
            if !should_fire(cron_expr, &now) {
                continue;
            }

            tracing::info!(
                "Agent cron fired: '{}' (project {}, agent '{}', schedule '{}')",
                agent.name,
                agent.project_id,
                agent.slug,
                cron_expr
            );

            // Emit a trigger job — the existing process_jobs loop handles
            // gate evaluation (cooldown, budget, concurrency) and execution
            let trigger = AutopilotTriggerJob {
                project_id: agent.project_id,
                trigger_type: "schedule".to_string(),
                trigger_source_id: Some(agent.id),
                trigger_source_type: Some("agent_schedule".to_string()),
                error_group_id: None,
            };

            if let Err(e) = self.queue.send(Job::AutopilotTrigger(trigger)).await {
                tracing::error!(
                    "Failed to emit cron trigger for agent '{}' in project {}: {:?}",
                    agent.slug,
                    agent.project_id,
                    e
                );
            } else {
                triggered += 1;
            }
        }

        if triggered > 0 {
            tracing::debug!(
                "Agent cron tick: {} agent(s) triggered out of {} checked",
                triggered,
                agents.len()
            );
        }

        Ok(())
    }
}

/// Convert standard cron DOW (0-6, 0=Sunday) to `cron` crate DOW (1-7, 1=Sunday).
/// Only converts the 5th field (day-of-week) if it contains numeric values.
/// Handles ranges (0-5 → 1-6), lists (0,3 → 1,4), and steps (*/2 unchanged).
fn convert_dow(cron_expr: &str) -> String {
    let parts: Vec<&str> = cron_expr.split_whitespace().collect();
    if parts.len() != 5 {
        return cron_expr.to_string();
    }

    let dow = parts[4];

    // If it's *, contains letters (SUN, MON, etc.), or contains /, leave as-is
    if dow == "*" || dow.chars().any(|c| c.is_alphabetic()) {
        return cron_expr.to_string();
    }

    // Convert each numeric value: 0→1, 1→2, ..., 6→7, 7→1 (7 is also Sunday)
    let converted_dow = dow
        .split(',')
        .map(|part| {
            if part.contains('-') {
                // Range: e.g. "0-5" → "1-6"
                let range_parts: Vec<&str> = part.split('-').collect();
                if range_parts.len() == 2 {
                    let start = range_parts[0]
                        .parse::<u8>()
                        .map(|n| if n == 7 { 1 } else { n + 1 });
                    let end = range_parts[1]
                        .parse::<u8>()
                        .map(|n| if n == 7 { 1 } else { n + 1 });
                    match (start, end) {
                        (Ok(s), Ok(e)) => format!("{}-{}", s, e),
                        _ => part.to_string(),
                    }
                } else {
                    part.to_string()
                }
            } else if let Ok(n) = part.parse::<u8>() {
                // Single number: 0→1, 7→1
                let converted = if n == 7 { 1 } else { n + 1 };
                converted.to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "{} {} {} {} {}",
        parts[0], parts[1], parts[2], parts[3], converted_dow
    )
}

/// Check if a 5-field cron expression should fire at the given time.
/// Converts 5-field to 7-field (seconds + year) for the `cron` crate.
fn should_fire(cron_expr: &str, now: &chrono::DateTime<Utc>) -> bool {
    // The `cron` crate requires 7-field expressions: sec min hour dom month dow year
    // User provides standard 5-field: min hour dom month dow
    //
    // DOW mapping:
    //   Standard cron: 0=Sunday, 1=Monday, ..., 6=Saturday (also 7=Sunday)
    //   `cron` crate:  1=Sunday, 2=Monday, ..., 7=Saturday
    //
    // We convert numeric DOW values by adding 1 (0→1, 6→7).
    // Named days (SUN, MON, etc.) are handled natively by the crate.
    let full_expr = format!("0 {} *", convert_dow(cron_expr));

    let schedule = match Schedule::from_str(&full_expr) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Invalid cron expression '{}' (expanded: '{}'): {}",
                cron_expr,
                full_expr,
                e
            );
            return false;
        }
    };

    // Check if any upcoming occurrence falls within this minute
    // (the schedule iterator gives the next occurrence after `now`)
    // We check if there's an occurrence between (now - 30s) and (now + 30s)
    // to handle the minute boundary
    let window_start = *now - chrono::Duration::seconds(30);
    if let Some(next) = schedule.after(&window_start).next() {
        // The next occurrence should be within the current minute
        let diff = (next - *now).num_seconds().abs();
        diff < 60
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_should_fire_every_minute() {
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 10, 0, 0).unwrap();
        assert!(should_fire("* * * * *", &now));
    }

    #[test]
    fn test_should_fire_specific_minute() {
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 10, 30, 0).unwrap();
        assert!(should_fire("30 10 * * *", &now));
    }

    #[test]
    fn test_should_not_fire_wrong_minute() {
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 10, 31, 0).unwrap();
        assert!(!should_fire("30 10 * * *", &now));
    }

    #[test]
    fn test_should_fire_daily_midnight() {
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 0, 0, 0).unwrap();
        assert!(should_fire("0 0 * * *", &now));
    }

    #[test]
    fn test_should_not_fire_daily_wrong_hour() {
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 14, 0, 0).unwrap();
        assert!(!should_fire("0 0 * * *", &now));
    }

    #[test]
    fn test_should_fire_weekly_monday() {
        // April 6, 2026 is a Monday
        // In the cron crate's 7-field format (with year), DOW uses 1=Sunday..7=Saturday
        // So Monday = 2 in 7-field. But we convert from 5-field where 1=Monday.
        // The cron crate actually accepts both 0-6 and 1-7 ranges and interprets them the same.
        // Let's use MON keyword to be safe:
        let now = Utc.with_ymd_and_hms(2026, 4, 6, 9, 0, 0).unwrap();
        assert!(should_fire("0 9 * * MON", &now));
    }

    #[test]
    fn test_should_not_fire_weekly_wrong_day() {
        // April 2, 2026 is a Thursday
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 9, 0, 0).unwrap();
        assert!(!should_fire("0 9 * * MON", &now)); // Monday only
    }

    #[test]
    fn test_should_fire_sunday_numeric_zero() {
        // April 5, 2026 is a Sunday — DOW 0 in standard cron
        let now = Utc.with_ymd_and_hms(2026, 4, 5, 9, 0, 0).unwrap();
        assert!(should_fire("0 9 * * 0", &now));
    }

    #[test]
    fn test_should_not_fire_sunday_on_thursday() {
        // April 2, 2026 is a Thursday
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 9, 0, 0).unwrap();
        assert!(!should_fire("0 9 * * 0", &now));
    }

    #[test]
    fn test_convert_dow_numeric() {
        assert_eq!(convert_dow("0 9 * * 0"), "0 9 * * 1"); // Sun
        assert_eq!(convert_dow("0 9 * * 1"), "0 9 * * 2"); // Mon
        assert_eq!(convert_dow("0 9 * * 5"), "0 9 * * 6"); // Fri
        assert_eq!(convert_dow("0 9 * * 6"), "0 9 * * 7"); // Sat
        assert_eq!(convert_dow("0 9 * * 7"), "0 9 * * 1"); // 7=Sun too
    }

    #[test]
    fn test_convert_dow_named_unchanged() {
        assert_eq!(convert_dow("0 9 * * MON"), "0 9 * * MON");
        assert_eq!(convert_dow("0 9 * * SUN"), "0 9 * * SUN");
    }

    #[test]
    fn test_convert_dow_star_unchanged() {
        assert_eq!(convert_dow("0 9 * * *"), "0 9 * * *");
    }

    #[test]
    fn test_convert_dow_range() {
        assert_eq!(convert_dow("0 9 * * 1-5"), "0 9 * * 2-6"); // Mon-Fri
    }

    #[test]
    fn test_convert_dow_list() {
        assert_eq!(convert_dow("0 9 * * 0,6"), "0 9 * * 1,7"); // Sun,Sat
    }

    #[test]
    fn test_invalid_cron_returns_false() {
        let now = Utc::now();
        assert!(!should_fire("invalid", &now));
        assert!(!should_fire("", &now));
    }

    #[test]
    fn test_should_fire_every_6_hours() {
        let now = Utc.with_ymd_and_hms(2026, 4, 2, 6, 0, 0).unwrap();
        assert!(should_fire("0 */6 * * *", &now));
    }
}
