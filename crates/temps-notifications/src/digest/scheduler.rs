//! Weekly digest scheduler
//!
//! This module implements the background scheduler for weekly digest emails.
//! The scheduler runs as a background task and sends digests based on user preferences.

use chrono::{Datelike, Timelike, Utc, Weekday};
use std::sync::Arc;
use tokio::time::{sleep, Duration as TokioDuration};
use tracing::{debug, error, info, warn};

use super::DigestService;
use crate::services::NotificationPreferencesService;

/// Background scheduler for weekly digest emails
pub struct DigestScheduler {
    digest_service: Arc<DigestService>,
    preferences_service: Arc<NotificationPreferencesService>,
}

impl DigestScheduler {
    /// Create a new digest scheduler and start the background task
    pub fn new(
        digest_service: Arc<DigestService>,
        preferences_service: Arc<NotificationPreferencesService>,
    ) -> Arc<Self> {
        let scheduler = Arc::new(Self {
            digest_service,
            preferences_service,
        });

        // Spawn background task
        let scheduler_clone = scheduler.clone();
        tokio::spawn(async move {
            scheduler_clone.run_scheduler().await;
        });

        info!("Weekly digest scheduler started");
        scheduler
    }

    /// Main scheduler loop - calculates exact sleep duration until next scheduled time
    async fn run_scheduler(&self) {
        loop {
            // Get current preferences
            let preferences = match self.preferences_service.get_preferences().await {
                Ok(prefs) => prefs,
                Err(e) => {
                    error!("Failed to get preferences: {}", e);
                    // Sleep for 1 hour and retry
                    sleep(TokioDuration::from_secs(3600)).await;
                    continue;
                }
            };

            // Skip if weekly digest is disabled
            if !preferences.weekly_digest_enabled {
                debug!("Weekly digest is disabled, checking again in 1 hour");
                sleep(TokioDuration::from_secs(3600)).await;
                continue;
            }

            // Calculate duration until next scheduled time
            let sleep_duration = self.calculate_next_run_duration(&preferences);

            info!(
                "Next weekly digest scheduled in {} hours ({} minutes)",
                sleep_duration.as_secs() / 3600,
                sleep_duration.as_secs() / 60
            );

            // Sleep until next scheduled time
            sleep(sleep_duration).await;

            // Send the digest
            match self.send_digest(&preferences).await {
                Ok(_) => {
                    info!("Successfully sent weekly digest");
                }
                Err(e) => {
                    error!("Failed to send weekly digest: {}", e);
                }
            }
        }
    }

    /// Calculate duration until next scheduled run
    fn calculate_next_run_duration(
        &self,
        preferences: &crate::services::NotificationPreferences,
    ) -> TokioDuration {
        let now = Utc::now();
        let send_day = Self::parse_weekday(&preferences.digest_send_day);
        let send_hour = Self::parse_hour(&preferences.digest_send_time);
        let send_minute = Self::parse_minute(&preferences.digest_send_time);

        // Calculate days until next occurrence of the target weekday
        let current_weekday = now.weekday();
        let days_until_target = if current_weekday == send_day {
            // Same day - check if time has passed
            let current_hour = now.hour();
            let current_minute = now.minute();

            if current_hour < send_hour
                || (current_hour == send_hour && current_minute < send_minute)
            {
                // Time hasn't passed yet today
                0
            } else {
                // Time has passed, schedule for next week
                7
            }
        } else {
            // Different day - calculate days forward
            let current_day_num = current_weekday.num_days_from_monday();
            let target_day_num = send_day.num_days_from_monday();

            if target_day_num > current_day_num {
                target_day_num - current_day_num
            } else {
                7 - (current_day_num - target_day_num)
            }
        };

        // Calculate the exact target time
        let target_time = if days_until_target == 0 {
            // Today at the specified time
            now.date_naive()
                .and_hms_opt(send_hour, send_minute, 0)
                .unwrap()
                .and_utc()
        } else {
            // Future day at the specified time
            (now + chrono::Duration::days(days_until_target as i64))
                .date_naive()
                .and_hms_opt(send_hour, send_minute, 0)
                .unwrap()
                .and_utc()
        };

        // Calculate duration from now to target time
        let duration = target_time.signed_duration_since(now);
        let seconds = duration.num_seconds().max(1); // Minimum 1 second

        TokioDuration::from_secs(seconds as u64)
    }

    /// Send the weekly digest
    async fn send_digest(
        &self,
        preferences: &crate::services::NotificationPreferences,
    ) -> anyhow::Result<()> {
        info!("Sending weekly digest");

        self.digest_service
            .generate_and_send_weekly_digest(preferences.digest_sections.clone())
            .await?;

        Ok(())
    }

    /// Parse weekday string to Weekday enum
    fn parse_weekday(day: &str) -> Weekday {
        match day.to_lowercase().as_str() {
            "monday" | "mon" => Weekday::Mon,
            "tuesday" | "tue" => Weekday::Tue,
            "wednesday" | "wed" => Weekday::Wed,
            "thursday" | "thu" => Weekday::Thu,
            "friday" | "fri" => Weekday::Fri,
            "saturday" | "sat" => Weekday::Sat,
            "sunday" | "sun" => Weekday::Sun,
            _ => {
                warn!("Invalid weekday '{}', defaulting to Monday", day);
                Weekday::Mon
            }
        }
    }

    /// Parse time string (HH:MM format) to hour (0-23)
    fn parse_hour(time: &str) -> u32 {
        time.split(':')
            .next()
            .and_then(|h| h.parse().ok())
            .unwrap_or_else(|| {
                warn!("Invalid time '{}', defaulting to 09:00", time);
                9
            })
    }

    /// Parse time string (HH:MM format) to minute (0-59)
    fn parse_minute(time: &str) -> u32 {
        time.split(':')
            .nth(1)
            .and_then(|m| m.parse().ok())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_weekday() {
        assert_eq!(DigestScheduler::parse_weekday("Monday"), Weekday::Mon);
        assert_eq!(DigestScheduler::parse_weekday("mon"), Weekday::Mon);
        assert_eq!(DigestScheduler::parse_weekday("Tuesday"), Weekday::Tue);
        assert_eq!(DigestScheduler::parse_weekday("Wednesday"), Weekday::Wed);
        assert_eq!(DigestScheduler::parse_weekday("Thursday"), Weekday::Thu);
        assert_eq!(DigestScheduler::parse_weekday("Friday"), Weekday::Fri);
        assert_eq!(DigestScheduler::parse_weekday("Saturday"), Weekday::Sat);
        assert_eq!(DigestScheduler::parse_weekday("Sunday"), Weekday::Sun);
        assert_eq!(DigestScheduler::parse_weekday("invalid"), Weekday::Mon); // Default
    }

    #[test]
    fn test_parse_hour() {
        assert_eq!(DigestScheduler::parse_hour("09:00"), 9);
        assert_eq!(DigestScheduler::parse_hour("14:30"), 14);
        assert_eq!(DigestScheduler::parse_hour("00:00"), 0);
        assert_eq!(DigestScheduler::parse_hour("23:59"), 23);
        assert_eq!(DigestScheduler::parse_hour("invalid"), 9); // Default
    }

    #[test]
    fn test_parse_minute() {
        assert_eq!(DigestScheduler::parse_minute("09:00"), 0);
        assert_eq!(DigestScheduler::parse_minute("14:30"), 30);
        assert_eq!(DigestScheduler::parse_minute("00:15"), 15);
        assert_eq!(DigestScheduler::parse_minute("23:59"), 59);
        assert_eq!(DigestScheduler::parse_minute("invalid"), 0); // Default
        assert_eq!(DigestScheduler::parse_minute("09"), 0); // No colon, default to 0
    }
}
