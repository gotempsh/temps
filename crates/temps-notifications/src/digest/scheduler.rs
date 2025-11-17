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

    /// Main scheduler loop
    async fn run_scheduler(&self) {
        loop {
            // Check every hour if it's time to send digests
            match self.check_and_send_digests().await {
                Ok(sent_count) => {
                    if sent_count > 0 {
                        info!("Sent {} weekly digest(s)", sent_count);
                    }
                }
                Err(e) => {
                    error!("Error checking/sending digests: {}", e);
                }
            }

            // Sleep for 1 hour before next check
            sleep(TokioDuration::from_secs(3600)).await;
        }
    }

    /// Check if it's time to send digests and send them if needed
    async fn check_and_send_digests(&self) -> anyhow::Result<usize> {
        let now = Utc::now();
        let current_weekday = now.weekday();
        let current_hour = now.hour();

        debug!(
            "Checking digest schedule: weekday={:?}, hour={}",
            current_weekday, current_hour
        );

        // Get global preferences
        let preferences = self.preferences_service.get_preferences().await?;

        // Skip if weekly digest is disabled
        if !preferences.weekly_digest_enabled {
            debug!("Weekly digest is disabled");
            return Ok(0);
        }

        // Check if today matches the configured send day
        let send_day = Self::parse_weekday(&preferences.digest_send_day);
        if current_weekday != send_day {
            debug!(
                "Not the right day: current={:?}, configured={:?}",
                current_weekday, send_day
            );
            return Ok(0);
        }

        // Check if current hour matches the configured send time
        let send_hour = Self::parse_hour(&preferences.digest_send_time);
        if current_hour != send_hour {
            debug!(
                "Not the right hour: current={}, configured={}",
                current_hour, send_hour
            );
            return Ok(0);
        }

        // It's time to send the digest
        info!(
            "Sending weekly digest at {:?} {}:00",
            current_weekday, current_hour
        );

        // Generate and send digest to all enabled email providers
        match self
            .digest_service
            .generate_and_send_weekly_digest(preferences.digest_sections.clone())
            .await
        {
            Ok(_) => {
                info!("Successfully sent weekly digest");
                Ok(1)
            }
            Err(e) => {
                warn!("Failed to send weekly digest: {}", e);
                Err(e)
            }
        }
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
}
