use thiserror::Error;

#[derive(Error, Debug)]
pub enum AiGatewayError {
    #[error("Provider '{provider}' not found or not configured")]
    ProviderNotConfigured { provider: String },

    #[error("Provider key {key_id} not found")]
    ProviderKeyNotFound { key_id: i32 },

    #[error("Model '{model}' not found: no provider configured that serves this model")]
    ModelNotFound { model: String },

    #[error("Model '{model}' is not allowed in scope '{scope}'")]
    ModelNotAllowed { model: String, scope: String },

    #[error(
        "AI gateway rate limit exceeded for scope '{scope}': maximum {limit_per_minute} requests per minute; retry in {retry_after_seconds}s"
    )]
    RateLimitExceeded {
        scope: String,
        limit_per_minute: i64,
        retry_after_seconds: u64,
    },

    /// Exact amounts are intentionally omitted from the Display (and therefore
    /// from HTTP error responses) to prevent leaking the operator's configured
    /// budget and current spend to untrusted deployment-token callers.
    /// Use structured tracing fields for server-side diagnostics.
    #[error("Monthly budget exceeded for scope '{scope}'")]
    MonthlyBudgetExceeded {
        scope: String,
        spent_microcents: i64,
        limit_microcents: i64,
    },

    #[error("No pricing is configured for model '{model}' required by budget scope '{scope}'")]
    PricingUnavailable { model: String, scope: String },

    #[error(
        "AI gateway budget scope '{scope}' requires max_tokens so spend can be reserved safely"
    )]
    BudgetRequiresMaxTokens { scope: String },

    #[error("AI gateway budget scope '{scope}' cannot safely project remote image token cost")]
    BudgetProjectionUnavailable { scope: String },

    #[error("Invalid AI gateway governance config for scope '{scope}': {field}={value} must be non-negative")]
    InvalidGovernanceConfig {
        scope: String,
        field: &'static str,
        value: i64,
    },

    #[error("Invalid AI gateway governance scope '{scope}'; expected instance, project:<id>, environment:<id>, or token:<id>")]
    InvalidGovernanceScope { scope: String },

    #[error("AI gateway governance config for scope '{scope}' was not found")]
    GovernanceConfigNotFound { scope: String },

    #[error("Upstream provider error for model '{model}': {status} {message}")]
    UpstreamError {
        model: String,
        status: u16,
        message: String,
    },

    #[error("Request translation failed for provider '{provider}': {reason}")]
    TranslationError { provider: String, reason: String },

    #[error("Streaming error for model '{model}': {reason}")]
    StreamError { model: String, reason: String },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("HTTP client error: {0}")]
    HttpClient(String),

    #[error("Internal error: {message}")]
    Internal { message: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Invalid provider base URL: {reason}")]
    InvalidProviderUrl { reason: String },
}

impl From<reqwest::Error> for AiGatewayError {
    fn from(error: reqwest::Error) -> Self {
        // Strip the request URL before converting to string — the URL may contain
        // embedded credentials (BYOK base_url) that must not appear in logs.
        let scrubbed = error.without_url();
        AiGatewayError::HttpClient(scrubbed.to_string())
    }
}
