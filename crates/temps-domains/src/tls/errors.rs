use thiserror::Error;

#[derive(Error, Debug)]
pub enum TlsError {
    #[error("Repository error: {0}")]
    Repository(#[from] RepositoryError),

    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("DNS error: {0}")]
    Dns(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Certificate not found: {0}")]
    NotFound(String),

    #[error("Certificate expired for domain: {0}")]
    Expired(String),

    #[error("Manual action required: {0}")]
    ManualActionRequired(String),

    #[error("Operation error: {0}")]
    Operation(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[derive(Error, Debug)]
pub enum RepositoryError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Duplicate entry: {0}")]
    DuplicateEntry(String),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<sea_orm::DbErr> for RepositoryError {
    fn from(err: sea_orm::DbErr) -> Self {
        match err {
            sea_orm::DbErr::RecordNotFound(msg) => RepositoryError::NotFound(msg),
            sea_orm::DbErr::RecordNotInserted => {
                RepositoryError::DuplicateEntry("Record not inserted".to_string())
            }
            sea_orm::DbErr::ConnectionAcquire(err) => {
                RepositoryError::Connection(err.to_string())
            }
            _ => RepositoryError::Database(err.to_string()),
        }
    }
}

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("ACME error: {0}")]
    Acme(String),

    #[error("Certificate generation error: {0}")]
    CertificateGeneration(String),

    #[error("Challenge failed: {0}")]
    ChallengeFailed(String),

    #[error("Validation failed: {0}")]
    ValidationFailed(String),

    #[error("Unsupported challenge type: {0}")]
    UnsupportedChallenge(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<instant_acme::Error> for ProviderError {
    fn from(err: instant_acme::Error) -> Self {
        ProviderError::Acme(err.to_string())
    }
}

impl From<rcgen::Error> for ProviderError {
    fn from(err: rcgen::Error) -> Self {
        ProviderError::CertificateGeneration(err.to_string())
    }
}

impl From<anyhow::Error> for ProviderError {
    fn from(err: anyhow::Error) -> Self {
        ProviderError::Internal(err.to_string())
    }
}

impl From<anyhow::Error> for TlsError {
    fn from(err: anyhow::Error) -> Self {
        TlsError::Internal(err.to_string())
    }
}

#[derive(Error, Debug)]
pub enum BuilderError {
    #[error("Missing repository")]
    MissingRepository,

    #[error("Missing certificate provider")]
    MissingProvider,

    #[error("Missing DNS provider")]
    MissingDns,

    #[error("Missing queue service")]
    MissingQueue,
}