//! IP-based rate limiting middleware for authentication endpoints.
//!
//! Provides a simple sliding-window rate limiter to prevent brute force attacks
//! on login, password reset, and MFA verification endpoints.

use crate::resolve_client_ip;
use axum::{
    extract::ConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::warn;

/// Configuration for the auth rate limiter.
#[derive(Debug, Clone)]
pub struct AuthRateLimitConfig {
    /// Maximum requests allowed within the window.
    pub max_requests: u32,
    /// Time window for counting requests.
    pub window: Duration,
    /// Maximum number of tracked IPs before forced eviction of stale entries.
    pub max_tracked_ips: usize,
}

impl Default for AuthRateLimitConfig {
    fn default() -> Self {
        Self {
            // 10 auth attempts per minute per IP
            max_requests: 10,
            window: Duration::from_secs(60),
            max_tracked_ips: 10_000,
        }
    }
}

/// Entry tracking requests from a single IP.
#[derive(Debug)]
struct RateLimitEntry {
    /// Timestamps of recent requests within the window.
    timestamps: Vec<Instant>,
}

/// Shared state for the rate limiter.
#[derive(Debug, Clone)]
pub struct AuthRateLimiter {
    entries: Arc<Mutex<HashMap<String, RateLimitEntry>>>,
    config: AuthRateLimitConfig,
}

impl AuthRateLimiter {
    pub fn new(config: AuthRateLimitConfig) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// Check if a request from the given IP should be allowed.
    /// Returns Ok(()) if allowed, Err(()) if rate limited.
    async fn check(&self, ip: &str) -> Result<(), ()> {
        let now = Instant::now();
        let window_start = now - self.config.window;

        let mut entries = self.entries.lock().await;

        // Evict stale entries when approaching the cap to bound memory usage.
        // This runs at 50% capacity to avoid doing it on every request near the limit.
        if entries.len() >= self.config.max_tracked_ips / 2 {
            entries.retain(|_, v| v.timestamps.last().is_some_and(|t| *t > window_start));
        }

        // If at cap after eviction, only allow already-tracked IPs
        if entries.len() >= self.config.max_tracked_ips && !entries.contains_key(ip) {
            warn!(
                "Rate limiter at capacity ({} IPs tracked), rejecting new IP",
                entries.len()
            );
            return Err(());
        }

        let entry = entries.entry(ip.to_string()).or_insert(RateLimitEntry {
            timestamps: Vec::new(),
        });

        // Remove timestamps outside the window
        entry.timestamps.retain(|t| *t > window_start);

        if entry.timestamps.len() >= self.config.max_requests as usize {
            return Err(());
        }

        entry.timestamps.push(now);

        Ok(())
    }
}

/// Axum middleware function for rate limiting auth endpoints.
///
/// Extracts the client IP from the immediate peer address. Only trusts
/// `X-Forwarded-For` / `X-Real-IP` headers when the peer is loopback (i.e.
/// requests arrived via a trusted same-host reverse proxy like Pingora).
pub async fn auth_rate_limit_middleware(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // Extract rate limiter from request extensions
    let limiter = request.extensions().get::<AuthRateLimiter>().cloned();

    let limiter = match limiter {
        Some(l) => l,
        None => {
            // No rate limiter configured, pass through
            return next.run(request).await;
        }
    };

    let ip = resolve_client_ip(request.headers(), Some(peer));

    match limiter.check(&ip).await {
        Ok(()) => next.run(request).await,
        Err(()) => {
            warn!("Rate limit exceeded for IP {} on auth endpoint", ip);
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", "60")],
                "Too many requests. Please try again later.",
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_allows_within_limit() {
        let limiter = AuthRateLimiter::new(AuthRateLimitConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
            ..Default::default()
        });

        for _ in 0..5 {
            assert!(limiter.check("1.2.3.4").await.is_ok());
        }
    }

    #[tokio::test]
    async fn test_rate_limiter_blocks_over_limit() {
        let limiter = AuthRateLimiter::new(AuthRateLimitConfig {
            max_requests: 3,
            window: Duration::from_secs(60),
            ..Default::default()
        });

        // First 3 should succeed
        assert!(limiter.check("1.2.3.4").await.is_ok());
        assert!(limiter.check("1.2.3.4").await.is_ok());
        assert!(limiter.check("1.2.3.4").await.is_ok());

        // 4th should be blocked
        assert!(limiter.check("1.2.3.4").await.is_err());
    }

    #[tokio::test]
    async fn test_rate_limiter_different_ips_independent() {
        let limiter = AuthRateLimiter::new(AuthRateLimitConfig {
            max_requests: 2,
            window: Duration::from_secs(60),
            ..Default::default()
        });

        // IP A fills its quota
        assert!(limiter.check("1.1.1.1").await.is_ok());
        assert!(limiter.check("1.1.1.1").await.is_ok());
        assert!(limiter.check("1.1.1.1").await.is_err());

        // IP B should still have its own quota
        assert!(limiter.check("2.2.2.2").await.is_ok());
        assert!(limiter.check("2.2.2.2").await.is_ok());
        assert!(limiter.check("2.2.2.2").await.is_err());
    }

    #[tokio::test]
    async fn test_rate_limiter_window_expiry() {
        let limiter = AuthRateLimiter::new(AuthRateLimitConfig {
            max_requests: 2,
            window: Duration::from_millis(50), // Very short window for testing
            ..Default::default()
        });

        assert!(limiter.check("1.2.3.4").await.is_ok());
        assert!(limiter.check("1.2.3.4").await.is_ok());
        assert!(limiter.check("1.2.3.4").await.is_err());

        // Wait for window to expire
        tokio::time::sleep(Duration::from_millis(60)).await;

        // Should be allowed again
        assert!(limiter.check("1.2.3.4").await.is_ok());
    }

    #[tokio::test]
    async fn test_rate_limiter_brute_force_simulation() {
        let limiter = AuthRateLimiter::new(AuthRateLimitConfig {
            max_requests: 10,
            window: Duration::from_secs(60),
            ..Default::default()
        });

        let attacker_ip = "10.0.0.1";

        // Simulate 10 rapid login attempts (allowed)
        for i in 0..10 {
            assert!(
                limiter.check(attacker_ip).await.is_ok(),
                "Request {} should be allowed",
                i + 1
            );
        }

        // 11th attempt should be blocked
        assert!(
            limiter.check(attacker_ip).await.is_err(),
            "11th request must be blocked to prevent brute force"
        );

        // But a legitimate user from a different IP should not be affected
        assert!(
            limiter.check("8.8.8.8").await.is_ok(),
            "Different IP should not be rate limited"
        );
    }

    fn make_headers(pairs: &[(&str, &str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn parse_addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn rate_limit_ip_uses_xff_only_when_peer_is_loopback() {
        let headers = make_headers(&[("x-forwarded-for", "1.2.3.4, 5.6.7.8")]);

        // Loopback peer (trusted reverse proxy): use rightmost XFF (5.6.7.8)
        let ip = resolve_client_ip(&headers, Some(parse_addr("127.0.0.1:1234")));
        assert_eq!(ip, "5.6.7.8");

        // Non-loopback peer (attacker reaching console API directly):
        // ignore the spoofed XFF and use the actual peer address.
        let peer = parse_addr("198.51.100.7:9999");
        let ip = resolve_client_ip(&headers, Some(peer));
        assert_eq!(ip, "198.51.100.7");
    }

    #[test]
    fn rate_limit_ip_attacker_cannot_rotate_xff_to_bypass() {
        // Attacker connects directly (non-loopback) and tries different XFF
        // values per request. Resolved IP must stay pinned to peer.
        let peer = parse_addr("203.0.113.5:42424");
        let h1 = make_headers(&[("x-forwarded-for", "1.1.1.1")]);
        let h2 = make_headers(&[("x-forwarded-for", "2.2.2.2")]);
        let h3 = make_headers(&[("x-real-ip", "9.9.9.9")]);

        assert_eq!(resolve_client_ip(&h1, Some(peer)), "203.0.113.5");
        assert_eq!(resolve_client_ip(&h2, Some(peer)), "203.0.113.5");
        assert_eq!(resolve_client_ip(&h3, Some(peer)), "203.0.113.5");
    }

    #[test]
    fn rate_limit_ip_falls_back_to_peer_when_no_headers() {
        let headers = make_headers(&[]);
        let peer = parse_addr("127.0.0.1:5555");
        assert_eq!(resolve_client_ip(&headers, Some(peer)), "127.0.0.1");
    }

    #[test]
    fn rate_limit_ip_xff_ipv6_peer_is_loopback() {
        let headers = make_headers(&[("x-forwarded-for", "10.0.0.1")]);
        let peer = parse_addr("[::1]:1234");
        assert_eq!(resolve_client_ip(&headers, Some(peer)), "10.0.0.1");
    }

    #[test]
    fn rate_limit_ip_unknown_when_no_peer_no_headers() {
        let headers = make_headers(&[]);
        assert_eq!(resolve_client_ip(&headers, None), "unknown");
    }

    #[test]
    fn rate_limit_ip_no_peer_ignores_headers() {
        // Without a peer we can't verify the proxy is trusted — must NOT
        // honor headers (defense in depth).
        let headers = make_headers(&[("x-forwarded-for", "1.1.1.1")]);
        assert_eq!(resolve_client_ip(&headers, None), "unknown");
    }

    #[tokio::test]
    async fn test_rate_limiter_memory_cap() {
        let limiter = AuthRateLimiter::new(AuthRateLimitConfig {
            max_requests: 100,
            window: Duration::from_secs(60),
            max_tracked_ips: 5,
        });

        // Fill up to the cap
        for i in 0..5 {
            assert!(limiter.check(&format!("10.0.0.{}", i)).await.is_ok());
        }

        // New IP should be rejected when at capacity
        assert!(
            limiter.check("10.0.0.99").await.is_err(),
            "New IP must be rejected when at capacity"
        );

        // Existing tracked IP should still work
        assert!(
            limiter.check("10.0.0.0").await.is_ok(),
            "Already-tracked IP should still be allowed"
        );
    }
}
