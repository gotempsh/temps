//! Screenshot Error Types

use thiserror::Error;

pub type ScreenshotResult<T> = Result<T, ScreenshotError>;

#[derive(Error, Debug)]
pub enum ScreenshotError {
    #[error("Screenshot capture failed: {0}")]
    CaptureFailed(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP request failed: {0}")]
    HttpRequest(String),

    #[error("Chrome browser error: {0}")]
    ChromeError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Screenshot provider not configured")]
    ProviderNotConfigured,

    #[error("Provider error: {0}")]
    ProviderError(String),
}
