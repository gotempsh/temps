//! Google service account authentication via JWT.
//!
//! Implements the OAuth 2.0 JWT Bearer flow for service accounts:
//! 1. Construct a JWT with the indexing scope
//! 2. Sign it with the service account's RSA private key
//! 3. Exchange it for an access token at Google's token endpoint
//! 4. Cache the token until it expires

use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::types::ServiceAccountKey;

const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
/// Space-separated scopes: indexing API + read-only cloud platform (for quota queries).
const SCOPES: &str = "https://www.googleapis.com/auth/indexing https://www.googleapis.com/auth/cloud-platform.read-only";

/// Cached access token with expiry tracking.
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    /// When the token expires (with a 60-second safety margin)
    expires_at: chrono::DateTime<Utc>,
}

/// JWT claims for Google service account token exchange.
#[derive(Debug, Serialize)]
struct JwtClaims {
    iss: String,
    scope: String,
    aud: String,
    iat: i64,
    exp: i64,
}

/// Token exchange response from Google's OAuth2 endpoint.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    token_type: String,
    expires_in: i64,
}

/// Google API error response.
#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    error_description: Option<String>,
}

/// Service account authenticator that manages JWT-based access tokens.
#[derive(Clone)]
pub struct GoogleAuth {
    key: Arc<ServiceAccountKey>,
    http_client: reqwest::Client,
    cached_token: Arc<Mutex<Option<CachedToken>>>,
}

impl GoogleAuth {
    /// Create a new authenticator from a service account key.
    pub fn new(key: ServiceAccountKey, http_client: reqwest::Client) -> Self {
        Self {
            key: Arc::new(key),
            http_client,
            cached_token: Arc::new(Mutex::new(None)),
        }
    }

    /// Get a valid access token, refreshing if necessary.
    pub async fn get_access_token(&self) -> Result<String, AuthError> {
        let mut cached = self.cached_token.lock().await;

        // Return cached token if it's still valid
        if let Some(ref token) = *cached {
            if Utc::now() < token.expires_at {
                return Ok(token.access_token.clone());
            }
        }

        // Generate a new JWT and exchange it for an access token
        let jwt = self.create_signed_jwt()?;
        let token_response = self.exchange_jwt(&jwt).await?;

        let new_token = CachedToken {
            access_token: token_response.access_token.clone(),
            // Expire 60 seconds early to avoid edge-case rejections
            expires_at: Utc::now() + chrono::Duration::seconds(token_response.expires_in - 60),
        };

        *cached = Some(new_token);
        Ok(token_response.access_token)
    }

    /// Get the GCP project ID from the service account key.
    pub fn project_id(&self) -> &str {
        &self.key.project_id
    }

    /// Create a signed JWT for the token exchange.
    fn create_signed_jwt(&self) -> Result<String, AuthError> {
        let now = Utc::now().timestamp();

        let claims = JwtClaims {
            iss: self.key.client_email.clone(),
            scope: SCOPES.to_string(),
            aud: GOOGLE_TOKEN_URI.to_string(),
            iat: now,
            exp: now + 3600, // 1 hour max
        };

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.key.private_key_id.clone());

        let encoding_key =
            EncodingKey::from_rsa_pem(self.key.private_key.as_bytes()).map_err(|e| {
                AuthError::KeyParsing {
                    reason: format!("Failed to parse RSA private key: {}", e),
                }
            })?;

        encode(&header, &claims, &encoding_key).map_err(|e| AuthError::JwtCreation {
            reason: format!("Failed to create JWT: {}", e),
        })
    }

    /// Exchange a signed JWT for an access token.
    async fn exchange_jwt(&self, jwt: &str) -> Result<TokenResponse, AuthError> {
        let response = self
            .http_client
            .post(GOOGLE_TOKEN_URI)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", jwt),
            ])
            .send()
            .await
            .map_err(|e| AuthError::TokenExchange {
                reason: format!("HTTP request failed: {}", e),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            // Try to parse as Google error response
            if let Ok(err) = serde_json::from_str::<TokenErrorResponse>(&body) {
                return Err(AuthError::TokenExchange {
                    reason: format!(
                        "Token exchange failed ({}): {} - {}",
                        status,
                        err.error,
                        err.error_description.unwrap_or_default()
                    ),
                });
            }
            return Err(AuthError::TokenExchange {
                reason: format!("Token exchange failed ({}): {}", status, body),
            });
        }

        response
            .json::<TokenResponse>()
            .await
            .map_err(|e| AuthError::TokenExchange {
                reason: format!("Failed to parse token response: {}", e),
            })
    }

    /// Invalidate the cached token, forcing a refresh on next call.
    pub async fn invalidate_token(&self) {
        let mut cached = self.cached_token.lock().await;
        *cached = None;
    }
}

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Failed to parse service account key: {reason}")]
    KeyParsing { reason: String },

    #[error("Failed to create JWT: {reason}")]
    JwtCreation { reason: String },

    #[error("Token exchange failed: {reason}")]
    TokenExchange { reason: String },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::RsaPrivateKey;

    /// Generate a test service account key with an in-memory RSA private key.
    fn test_service_account_key() -> ServiceAccountKey {
        let mut rng = rsa::rand_core::OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).expect("generate RSA key");
        let pem = private_key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("encode PEM");

        ServiceAccountKey {
            key_type: "service_account".into(),
            project_id: "test-project".into(),
            private_key_id: "key123".into(),
            private_key: pem.to_string(),
            client_email: "test@test-project.iam.gserviceaccount.com".into(),
            client_id: "123456789".into(),
            auth_uri: "https://accounts.google.com/o/oauth2/auth".into(),
            token_uri: GOOGLE_TOKEN_URI.into(),
        }
    }

    #[test]
    fn test_jwt_creation() {
        let key = test_service_account_key();
        let http_client = reqwest::Client::new();
        let auth = GoogleAuth::new(key, http_client);

        let jwt = auth.create_signed_jwt();
        assert!(jwt.is_ok(), "JWT creation should succeed: {:?}", jwt.err());

        let jwt = jwt.unwrap();
        // JWT should have 3 parts separated by dots
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT should have header.payload.signature");

        // Decode the header
        let header_bytes = URL_SAFE_NO_PAD.decode(parts[0]).unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["kid"], "key123");

        // Decode the payload
        let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert_eq!(payload["iss"], "test@test-project.iam.gserviceaccount.com");
        assert_eq!(payload["scope"], SCOPES);
        assert_eq!(payload["aud"], GOOGLE_TOKEN_URI);
    }

    #[test]
    fn test_invalid_key_returns_error() {
        let mut key = test_service_account_key();
        key.private_key = "not-a-valid-pem-key".into();
        let http_client = reqwest::Client::new();
        let auth = GoogleAuth::new(key, http_client);

        let result = auth.create_signed_jwt();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::KeyParsing { .. }));
    }

    #[tokio::test]
    async fn test_token_caching() {
        let key = test_service_account_key();
        let http_client = reqwest::Client::new();
        let auth = GoogleAuth::new(key, http_client);

        // Manually insert a cached token
        {
            let mut cached = auth.cached_token.lock().await;
            *cached = Some(CachedToken {
                access_token: "cached-token-abc".into(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
            });
        }

        // Should return cached token without making HTTP request
        let token = auth.get_access_token().await.unwrap();
        assert_eq!(token, "cached-token-abc");
    }

    #[tokio::test]
    async fn test_expired_token_is_not_returned() {
        let key = test_service_account_key();
        let http_client = reqwest::Client::new();
        let auth = GoogleAuth::new(key, http_client);

        // Insert an expired token
        {
            let mut cached = auth.cached_token.lock().await;
            *cached = Some(CachedToken {
                access_token: "expired-token".into(),
                expires_at: Utc::now() - chrono::Duration::hours(1),
            });
        }

        // This will try to exchange a JWT which will fail (no network),
        // but it proves the expired token is not returned
        let result = auth.get_access_token().await;
        assert!(result.is_err());
        // The error should be from the token exchange, not from returning the expired token
        assert!(matches!(
            result.unwrap_err(),
            AuthError::TokenExchange { .. }
        ));
    }

    #[tokio::test]
    async fn test_invalidate_token() {
        let key = test_service_account_key();
        let http_client = reqwest::Client::new();
        let auth = GoogleAuth::new(key, http_client);

        // Insert a cached token
        {
            let mut cached = auth.cached_token.lock().await;
            *cached = Some(CachedToken {
                access_token: "to-be-invalidated".into(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
            });
        }

        auth.invalidate_token().await;

        let cached = auth.cached_token.lock().await;
        assert!(cached.is_none());
    }
}
