use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Severity levels for notifications - more granular than priority
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotificationSeverity {
    /// Debug-level information, typically not sent in production
    Debug,
    /// Informational messages about normal operations
    Info,
    /// Warning conditions that might require attention
    Warning,
    /// Error conditions that require immediate attention
    Error,
    /// Critical conditions that require urgent action
    Critical,
    /// Emergency conditions that affect system stability
    Emergency,
}

impl NotificationSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
            Self::Emergency => "emergency",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warning" | "warn" => Some(Self::Warning),
            "error" => Some(Self::Error),
            "critical" => Some(Self::Critical),
            "emergency" => Some(Self::Emergency),
            _ => None,
        }
    }

    /// Returns an emoji representation for visual distinction
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Debug => "🔍",
            Self::Info => "ℹ️",
            Self::Warning => "⚠️",
            Self::Error => "❌",
            Self::Critical => "🚨",
            Self::Emergency => "🆘",
        }
    }

    /// Returns a color code for visual representation (e.g., in Slack)
    pub fn color(&self) -> &'static str {
        match self {
            Self::Debug => "#808080",     // Gray
            Self::Info => "#0099FF",      // Blue
            Self::Warning => "#FFCC00",   // Yellow
            Self::Error => "#FF3333",     // Red
            Self::Critical => "#CC0000",  // Dark Red
            Self::Emergency => "#990000", // Darker Red
        }
    }
}

impl fmt::Display for NotificationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// Keep the old NotificationPriority for backward compatibility
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationPriority {
    Low,
    Normal,
    High,
    Critical,
}

impl fmt::Display for NotificationPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotificationPriority::Low => write!(f, "Low"),
            NotificationPriority::Normal => write!(f, "Normal"),
            NotificationPriority::High => write!(f, "High"),
            NotificationPriority::Critical => write!(f, "Critical"),
        }
    }
}

impl From<NotificationSeverity> for NotificationPriority {
    fn from(severity: NotificationSeverity) -> Self {
        match severity {
            NotificationSeverity::Debug => NotificationPriority::Low,
            NotificationSeverity::Info => NotificationPriority::Normal,
            NotificationSeverity::Warning => NotificationPriority::Normal,
            NotificationSeverity::Error => NotificationPriority::High,
            NotificationSeverity::Critical | NotificationSeverity::Emergency => {
                NotificationPriority::Critical
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationType {
    Info,
    Warning,
    Error,
    Alert,
}

impl fmt::Display for NotificationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotificationType::Info => write!(f, "Info"),
            NotificationType::Warning => write!(f, "Warning"),
            NotificationType::Error => write!(f, "Error"),
            NotificationType::Alert => write!(f, "Alert"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub title: String,
    pub message: String,
    pub notification_type: NotificationType,
    pub priority: NotificationPriority,
    pub severity: Option<NotificationSeverity>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, String>,
    /// Bypass all throttling and rate limiting - use with extreme caution
    pub bypass_throttling: bool,
}

impl Default for Notification {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            message: String::new(),
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            bypass_throttling: false,
        }
    }
}

impl Notification {
    /// Create a new notification with default values
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            ..Default::default()
        }
    }

    /// Get the effective severity (from severity field or derived from priority)
    pub fn effective_severity(&self) -> NotificationSeverity {
        self.severity.unwrap_or_else(|| match self.priority {
            NotificationPriority::Low => NotificationSeverity::Info,
            NotificationPriority::Normal => NotificationSeverity::Warning,
            NotificationPriority::High => NotificationSeverity::Error,
            NotificationPriority::Critical => NotificationSeverity::Critical,
        })
    }

    /// Set the severity
    pub fn with_severity(mut self, severity: NotificationSeverity) -> Self {
        self.severity = Some(severity);
        self
    }

    /// Set the priority
    pub fn with_priority(mut self, priority: NotificationPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Bypass throttling for this notification
    pub fn bypass_throttling(mut self) -> Self {
        self.bypass_throttling = true;
        self
    }

    /// Add metadata
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}
