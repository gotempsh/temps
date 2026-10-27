// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cloudflare Access signing keys (ADR 065 D9).
//!
//! A listener verifies an Access JWT with the team's public keys, which the
//! control plane fetches from `https://<team>.cloudflareaccess.com/cdn-cgi/
//! access/certs` and sends per address in the route snapshot. They are kept
//! on the provider row, so a key rotation is a row update, and the row's
//! trigger announces a route reload like any other route change.
//!
//! - Every hour, the keys of every provider in use are fetched again.
//! - A listener that sees a JWT signed with a `kid` it does not know asks
//!   for a refresh through its node channel. Refreshes of one provider are
//!   at most one per [`REFRESH_INTERVAL`], so a visitor sending made-up
//!   `kid`s cannot make the control plane hammer Cloudflare.
//! - No token is needed: the keys are public. The URL is only ever built
//!   for a `<team>.cloudflareaccess.com` host, so a provider row cannot
//!   point the control plane at any other server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde::Deserialize;
use temps_core::gate::AccessKey;
use temps_entities::{cloudflare_access_providers, environment_domains, project_custom_domains};
use thiserror::Error;
use tracing::{debug, info, warn};

/// How often the keys of a provider in use are fetched again.
pub const HOURLY: Duration = Duration::from_secs(60 * 60);
/// The least time between two fetches of one provider's keys.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub use temps_core::gate_access::{is_team_sign_in, team_domain, TEAM_DOMAIN_SUFFIX};
/// A certs answer larger than this is not Cloudflare's.
const MAX_CERTS_BYTES: usize = 64 * 1024;
/// More keys than this are not Cloudflare's (it publishes two).
const MAX_KEYS: usize = 16;

#[derive(Debug, Error)]
pub enum AccessKeyError {
    #[error("Cloudflare Access provider {provider_id} not found")]
    NotFound { provider_id: i32 },
    #[error(
        "Cloudflare Access provider {provider_id} has team domain '{team_domain}', which is not \
         a <team>.cloudflareaccess.com host"
    )]
    BadTeamDomain {
        provider_id: i32,
        team_domain: String,
    },
    #[error("Failed to fetch the Access signing keys of {team_domain}: {reason}")]
    Fetch { team_domain: String, reason: String },
    #[error("Failed to {operation} for Cloudflare Access provider {provider_id}: {source}")]
    Database {
        operation: &'static str,
        provider_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// The JWT issuer for a team domain.
pub fn issuer(team_domain: &str) -> String {
    format!("https://{team_domain}")
}

#[derive(Debug, Deserialize)]
struct Jwks {
    #[serde(default)]
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

/// The RS256 signing keys in a certs answer. Anything else in it (other
/// key types, other algorithms, encryption keys) is ignored, so a listener
/// can never be handed a key for an algorithm it must refuse.
pub fn parse_keys(body: &[u8]) -> Result<Vec<AccessKey>, String> {
    if body.len() > MAX_CERTS_BYTES {
        return Err(format!(
            "the answer is {} bytes, more than the {MAX_CERTS_BYTES} a key set needs",
            body.len()
        ));
    }
    let jwks: Jwks =
        serde_json::from_slice(body).map_err(|e| format!("the answer is not a key set ({e})"))?;
    let keys: Vec<AccessKey> = jwks
        .keys
        .into_iter()
        .filter(|k| k.kty.as_deref() == Some("RSA"))
        .filter(|k| k.alg.as_deref().is_none_or(|alg| alg == "RS256"))
        .filter(|k| k.usage.as_deref().is_none_or(|usage| usage == "sig"))
        .filter_map(|k| match (k.kid, k.n, k.e) {
            (Some(kid), Some(n), Some(e)) if !kid.is_empty() && !n.is_empty() && !e.is_empty() => {
                Some(AccessKey { kid, n, e })
            }
            _ => None,
        })
        .take(MAX_KEYS)
        .collect();
    if keys.is_empty() {
        return Err("the key set has no RS256 signing key".to_string());
    }
    Ok(keys)
}

/// The keys stored on a provider row, `[]` when none were fetched yet.
pub fn stored_keys(jwks: Option<&serde_json::Value>) -> Vec<AccessKey> {
    jwks.and_then(|value| value.get("keys"))
        .and_then(|keys| serde_json::from_value::<Vec<AccessKey>>(keys.clone()).ok())
        .unwrap_or_default()
}

fn to_stored(keys: &[AccessKey]) -> serde_json::Value {
    serde_json::json!({ "keys": keys })
}

/// What every Access address of one team shares in the route snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTeam {
    pub issuer: String,
    pub keys: Vec<AccessKey>,
}

/// The issuer and stored keys of every saved Access token, by id. Read by
/// the route table only when an address is gated by Access.
pub async fn access_teams<C: sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<HashMap<i32, AccessTeam>, sea_orm::DbErr> {
    Ok(cloudflare_access_providers::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| {
            (
                row.id,
                AccessTeam {
                    issuer: issuer(&row.team_domain),
                    keys: stored_keys(row.jwks.as_ref()),
                },
            )
        })
        .collect())
}

/// Fetches and stores Access signing keys.
pub struct AccessKeyService {
    db: Arc<DatabaseConnection>,
    http: reqwest::Client,
    last_fetch: Mutex<HashMap<i32, Instant>>,
}

/// Read `response`'s body, refusing one over `max` bytes: by its
/// `Content-Length` before reading anything, and while it streams when it
/// has none, so a broken or hostile server never makes Temps hold more
/// than `max` bytes of it.
pub async fn read_capped(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let too_large = || format!("the answer is larger than the {max} bytes allowed");
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > max {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl AccessKeyService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            db,
            http,
            last_fetch: Mutex::new(HashMap::new()),
        }
    }

    /// Takes the right to fetch `provider_id`'s keys now, unless they were
    /// fetched less than [`REFRESH_INTERVAL`] ago.
    fn take_slot(&self, provider_id: i32, now: Instant) -> bool {
        let mut last = self.last_fetch.lock();
        match last.get(&provider_id) {
            Some(at) if now.duration_since(*at) < REFRESH_INTERVAL => false,
            _ => {
                last.insert(provider_id, now);
                true
            }
        }
    }

    /// Fetches the keys of `team_domain`.
    pub async fn fetch(&self, team_domain: &str) -> Result<Vec<AccessKey>, AccessKeyError> {
        let fetch_error = |reason: String| AccessKeyError::Fetch {
            team_domain: team_domain.to_string(),
            reason,
        };
        let url = format!("https://{team_domain}/cdn-cgi/access/certs");
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| fetch_error(format!("could not reach {url} ({e})")))?;
        if !response.status().is_success() {
            return Err(fetch_error(format!(
                "{url} answered HTTP {}",
                response.status().as_u16()
            )));
        }
        let body = read_capped(response, MAX_CERTS_BYTES)
            .await
            .map_err(|e| fetch_error(format!("could not read the answer from {url} ({e})")))?;
        parse_keys(&body).map_err(fetch_error)
    }

    /// Fetches `provider_id`'s keys and stores them when they changed.
    /// `force` skips the rate limit (a provider just saved). Returns whether
    /// a fetch was made.
    pub async fn refresh(&self, provider_id: i32, force: bool) -> Result<bool, AccessKeyError> {
        if !force && !self.take_slot(provider_id, Instant::now()) {
            debug!(provider_id, "Access keys fetched recently; refresh skipped");
            return Ok(false);
        }
        if force {
            self.last_fetch.lock().insert(provider_id, Instant::now());
        }
        let db_error = |operation: &'static str| {
            move |source| AccessKeyError::Database {
                operation,
                provider_id,
                source,
            }
        };
        let provider = cloudflare_access_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(db_error("load the provider"))?
            .ok_or(AccessKeyError::NotFound { provider_id })?;
        let Some(domain) = team_domain(&provider.team_domain) else {
            return Err(AccessKeyError::BadTeamDomain {
                provider_id,
                team_domain: provider.team_domain,
            });
        };
        let keys = self.fetch(&domain).await?;
        let changed = stored_keys(provider.jwks.as_ref()) != keys;
        let mut active: cloudflare_access_providers::ActiveModel = provider.into();
        if changed {
            active.jwks = Set(Some(to_stored(&keys)));
            info!(
                provider_id,
                keys = keys.len(),
                "Cloudflare Access signing keys changed"
            );
        }
        active.jwks_fetched_at = Set(Some(chrono::Utc::now()));
        active
            .update(self.db.as_ref())
            .await
            .map_err(db_error("store the signing keys"))?;
        Ok(true)
    }

    /// The Access token behind the address a node names (`custom:<id>` or
    /// `environment:<id>`), if it is gated by Access.
    pub async fn provider_of_address(
        &self,
        address_id: &str,
    ) -> Result<Option<i32>, sea_orm::DbErr> {
        let parse = |prefix: &str| {
            address_id
                .strip_prefix(prefix)
                .and_then(|id| id.parse::<i32>().ok())
        };
        if let Some(id) = parse("custom:") {
            return Ok(project_custom_domains::Entity::find_by_id(id)
                .one(self.db.as_ref())
                .await?
                .filter(|d| {
                    d.gate.as_deref() == Some(temps_entities::audience::GATE_CLOUDFLARE_ACCESS)
                })
                .and_then(|d| d.gate_access_provider_id));
        }
        if let Some(id) = parse("environment:") {
            return Ok(environment_domains::Entity::find_by_id(id)
                .one(self.db.as_ref())
                .await?
                .filter(|d| {
                    d.gate.as_deref() == Some(temps_entities::audience::GATE_CLOUDFLARE_ACCESS)
                })
                .and_then(|d| d.gate_access_provider_id));
        }
        Ok(None)
    }

    /// The providers an address uses now.
    async fn providers_in_use(&self) -> Result<Vec<i32>, sea_orm::DbErr> {
        let mut ids: Vec<i32> = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::GateAccessProviderId.is_not_null())
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .filter_map(|d| d.gate_access_provider_id)
            .collect();
        ids.extend(
            environment_domains::Entity::find()
                .filter(environment_domains::Column::GateAccessProviderId.is_not_null())
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .filter_map(|d| d.gate_access_provider_id),
        );
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// The hourly refresh, until the process ends. An install with no
    /// Access-gated address makes no request at all (D11).
    pub async fn run_hourly(self: Arc<Self>) {
        let mut tick = tokio::time::interval(HOURLY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let providers = match self.providers_in_use().await {
                Ok(providers) => providers,
                Err(error) => {
                    warn!(%error, "could not list the Cloudflare Access providers in use");
                    continue;
                }
            };
            for provider_id in providers {
                if let Err(error) = self.refresh(provider_id, false).await {
                    warn!(provider_id, %error, "Cloudflare Access key refresh failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_label_under_cloudflareaccess_com_is_a_team_domain() {
        assert_eq!(
            team_domain("Acme.cloudflareaccess.com").as_deref(),
            Some("acme.cloudflareaccess.com")
        );
        assert_eq!(
            team_domain("https://team-1.cloudflareaccess.com/").as_deref(),
            Some("team-1.cloudflareaccess.com")
        );
        for bad in [
            "cloudflareaccess.com",
            ".cloudflareaccess.com",
            "a.b.cloudflareaccess.com",
            "evil.example/x.cloudflareaccess.com",
            "team.cloudflareaccess.com.evil.example",
            "team.cloudflareaccess.com:8443",
            "-team.cloudflareaccess.com",
            "te_am.cloudflareaccess.com",
            "example.com",
        ] {
            assert_eq!(team_domain(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_rs256_signing_keys_are_kept() {
        let body = serde_json::json!({
            "keys": [
                {"kid": "a", "kty": "RSA", "alg": "RS256", "use": "sig", "n": "n1", "e": "AQAB"},
                {"kid": "b", "kty": "RSA", "n": "n2", "e": "AQAB"},
                {"kid": "c", "kty": "RSA", "alg": "RS512", "n": "n3", "e": "AQAB"},
                {"kid": "d", "kty": "EC", "alg": "ES256", "x": "x", "y": "y"},
                {"kid": "e", "kty": "RSA", "use": "enc", "n": "n5", "e": "AQAB"},
                {"kty": "RSA", "n": "n6", "e": "AQAB"}
            ],
            "public_cert": {"kid": "a", "cert": "..."}
        });
        let keys = parse_keys(body.to_string().as_bytes()).expect("keys");
        let kids: Vec<&str> = keys.iter().map(|k| k.kid.as_str()).collect();
        assert_eq!(kids, vec!["a", "b"]);
    }

    /// Serve `head` then `body` once on a loopback port; its URL.
    async fn serve_once(head: String, body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        });
        format!("http://{address}/")
    }

    #[tokio::test]
    async fn an_answer_over_the_cap_is_refused_before_it_is_all_read() {
        let client = reqwest::Client::new();
        // Announced too large: refused on the header alone.
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n".into(),
            Vec::new(),
        )
        .await;
        let response = client.get(&url).send().await.unwrap();
        assert!(read_capped(response, 1024)
            .await
            .unwrap_err()
            .contains("larger"));
        // No length given: cut off once it streams past the cap.
        let mut chunked = Vec::new();
        for _ in 0..8 {
            chunked.extend_from_slice(format!("{:x}\r\n", 512).as_bytes());
            chunked.extend_from_slice(&[b'x'; 512]);
            chunked.extend_from_slice(b"\r\n");
        }
        chunked.extend_from_slice(b"0\r\n\r\n");
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".into(),
            chunked,
        )
        .await;
        let response = client.get(&url).send().await.unwrap();
        assert!(read_capped(response, 1024)
            .await
            .unwrap_err()
            .contains("larger"));
        // Within the cap: read whole.
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n".into(),
            b"hello".to_vec(),
        )
        .await;
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(read_capped(response, 1024).await.unwrap(), b"hello");
    }

    #[test]
    fn a_key_set_with_nothing_usable_or_too_large_is_refused() {
        assert!(parse_keys(br#"{"keys": []}"#).is_err());
        assert!(parse_keys(b"<html>").is_err());
        assert!(parse_keys(&vec![b' '; MAX_CERTS_BYTES + 1]).is_err());
    }

    #[test]
    fn stored_keys_round_trip() {
        let keys = vec![AccessKey {
            kid: "a".into(),
            n: "n".into(),
            e: "AQAB".into(),
        }];
        assert_eq!(stored_keys(Some(&to_stored(&keys))), keys);
        assert!(stored_keys(None).is_empty());
        assert!(stored_keys(Some(&serde_json::json!({"keys": "x"}))).is_empty());
    }

    #[test]
    fn refreshes_of_one_provider_are_rate_limited() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let service = AccessKeyService::new(Arc::new(db));
        let now = Instant::now();
        assert!(service.take_slot(1, now));
        assert!(!service.take_slot(1, now + Duration::from_secs(30)));
        assert!(service.take_slot(2, now + Duration::from_secs(30)));
        assert!(service.take_slot(1, now + REFRESH_INTERVAL));
    }
}
